//! Segment registry queries: bbox listing + detail.

use anyhow::Result;
use rm_core::{polyline, LatLng};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::geojson;

/// `GET /api/segments?bbox=minLon,minLat,maxLon,maxLat&min_routes=&limit=&geom=`
/// SQL prefilters on endpoint-station coords, Rust confirms any vertex in bbox.
/// Sorted by route_count DESC. `geom_osm`: draw stitched OSM way node runs
/// instead of the provider-cut `encoded_polyline`.
pub fn list(
    conn: &Connection,
    bbox: (f64, f64, f64, f64),
    min_routes: i64,
    limit: i64,
    geom_osm: bool,
) -> Result<Value> {
    let (min_lon, min_lat, max_lon, max_lat) = bbox;
    let mut stmt = conn.prepare(
        "SELECT sg.segment_id, sg.encoded_polyline, sg.length_m, sg.way_count,
                sg.start_station_id, sg.end_station_id,
                (SELECT COUNT(DISTINCT rv.route_id)
                 FROM seg_traversal t
                 JOIN subroute_version ssv ON ssv.subroute_version_id = t.subroute_version_id
                 JOIN route_version rv ON rv.route_version_id = ssv.route_version_id
                 WHERE t.segment_id = sg.segment_id) AS route_count
         FROM seg_segment sg
         WHERE EXISTS (
             SELECT 1 FROM station_version sv
             WHERE sv.station_id IN (sg.start_station_id, sg.end_station_id)
               AND sv.valid_to IS NULL
               AND sv.longitude BETWEEN ?1 AND ?3
               AND sv.latitude BETWEEN ?2 AND ?4
         )",
    )?;
    let it = stmt.query_map(rusqlite::params![min_lon, min_lat, max_lon, max_lat], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, f64>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, i64>(6)?,
        ))
    })?;
    let rows: Vec<(String, String, f64, i64, i64)> = it.collect::<Result<_, _>>()?;

    let osm_runs = if geom_osm {
        let ids: Vec<String> = rows
            .iter()
            .filter(|r| r.4 >= min_routes)
            .map(|r| r.0.clone())
            .collect();
        Some(osm_runs_by_segment(conn, &ids)?)
    } else {
        None
    };

    let mut features = Vec::new();
    for (id, enc, length_m, way_count, route_count) in rows {
        if route_count < min_routes {
            continue;
        }
        let (geometry, all_pts) = match &osm_runs {
            Some(runs) => {
                let pieces = stitch_way_pieces(runs.get(&id).map(|v| v.as_slice()).unwrap_or(&[]));
                if pieces.is_empty() {
                    continue;
                }
                let g = if pieces.len() == 1 {
                    geojson::line_string(&pieces[0])
                } else {
                    geojson::multi_line_string(&pieces)
                };
                (g, pieces.concat())
            }
            None => match polyline::decode(&enc) {
                Ok(p) if p.len() >= 2 => (geojson::line_string(&p), p),
                _ => continue,
            },
        };
        if !all_pts
            .iter()
            .any(|p| p.lon >= min_lon && p.lon <= max_lon && p.lat >= min_lat && p.lat <= max_lat)
        {
            continue;
        }
        features.push(geojson::feature(
            geometry,
            json!({
                "segment_id": id,
                "length_m": length_m,
                "way_count": way_count,
                "route_count": route_count,
            }),
        ));
    }
    // sort the full candidate set BEFORE truncating so the limit keeps the
    // most-shared corridors, not an arbitrary scan-order prefix
    features.sort_by_key(|f| -f["properties"]["route_count"].as_i64().unwrap_or(0));
    let truncated = features.len() as i64 > limit;
    if truncated {
        features.truncate(limit as usize);
    }
    Ok(json!({
        "type": "FeatureCollection",
        "features": features,
        "truncated": truncated,
    }))
}

/// Per-segment ordered way node runs for OSM-geometry mode. One chunked
/// IN query over `seg_segment_way`; runs keyed by (segment, ordinal), then
/// assembled in ordinal order.
fn osm_runs_by_segment(
    conn: &Connection,
    ids: &[String],
) -> Result<HashMap<String, Vec<Vec<NodePt>>>> {
    let mut by_key: HashMap<(String, i64), Vec<NodePt>> = HashMap::new();
    for chunk in ids.chunks(500) {
        let placeholders = (1..=chunk.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT sw.segment_id, sw.ordinal, n.osm_node_id, n.lat, n.lon
             FROM seg_segment_way sw
             JOIN osm_way_node wn ON wn.osm_way_id = sw.osm_way_id
             JOIN osm_node n ON n.osm_node_id = wn.osm_node_id
             WHERE sw.segment_id IN ({placeholders})
             ORDER BY sw.segment_id, sw.ordinal, wn.ordinal"
        );
        let mut stmt = conn.prepare(&sql)?;
        let it = stmt.query_map(
            rusqlite::params_from_iter(chunk.iter().map(|s| s.as_str())),
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    NodePt {
                        id: r.get(2)?,
                        lat: r.get(3)?,
                        lon: r.get(4)?,
                    },
                ))
            },
        )?;
        for row in it {
            let (seg, ord, p) = row?;
            by_key.entry((seg, ord)).or_default().push(p);
        }
    }
    let mut grouped: HashMap<String, Vec<(i64, Vec<NodePt>)>> = HashMap::new();
    for ((seg, ord), run) in by_key {
        grouped.entry(seg).or_default().push((ord, run));
    }
    Ok(grouped
        .into_iter()
        .map(|(seg, mut runs)| {
            runs.sort_by_key(|(ord, _)| *ord);
            (seg, runs.into_iter().map(|(_, r)| r).collect())
        })
        .collect())
}

/// Node with identity, for stitching way runs.
#[derive(Debug, Clone, Copy)]
struct NodePt {
    id: i64,
    lat: f64,
    lon: f64,
}

/// Stitch per-way node runs (travel order) into polyline pieces per segment.
/// A run whose last node chains onto the current tail is reversed; shared
/// joint nodes are deduped. A run sharing no end node starts a NEW piece —
/// drawing a connector across the gap would fabricate geometry.
fn stitch_way_pieces(runs: &[Vec<NodePt>]) -> Vec<Vec<LatLng>> {
    let mut pieces: Vec<Vec<LatLng>> = Vec::new();
    let mut last_node: Option<i64> = None;
    for run in runs {
        if run.is_empty() {
            continue;
        }
        let chains = matches!(last_node, Some(t) if run[0].id == t || run[run.len() - 1].id == t);
        if !chains {
            pieces.push(Vec::new()); // disconnected: cleave, no phantom line
        }
        let rev = matches!(last_node, Some(t) if run[run.len() - 1].id == t && run[0].id != t);
        let iter: Box<dyn Iterator<Item = &NodePt>> = if rev {
            Box::new(run.iter().rev())
        } else {
            Box::new(run.iter())
        };
        for p in iter {
            if last_node == Some(p.id) {
                continue; // shared joint node
            }
            pieces.last_mut().expect("piece pushed above").push(LatLng {
                lat: p.lat,
                lon: p.lon,
            });
            last_node = Some(p.id);
        }
    }
    pieces.into_iter().filter(|p| p.len() >= 2).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(id: i64, lat: f64, lon: f64) -> NodePt {
        NodePt { id, lat, lon }
    }

    #[test]
    fn forward_runs_concatenate_with_joint_deduped() {
        let runs = vec![
            vec![n(1, 0.0, 0.0), n(2, 0.0, 0.1)],
            vec![n(2, 0.0, 0.1), n(3, 0.0, 0.2)],
        ];
        let pieces = stitch_way_pieces(&runs);
        assert_eq!(pieces.len(), 1, "chained ways stay one piece");
        assert_eq!(pieces[0].len(), 3, "shared node 2 appears once");
    }

    #[test]
    fn reversed_run_flips_to_chain_onto_tail() {
        let runs = vec![
            vec![n(1, 0.0, 0.0), n(2, 0.0, 0.1)],
            vec![n(3, 0.0, 0.2), n(2, 0.0, 0.1)], // stored reversed
        ];
        let pieces = stitch_way_pieces(&runs);
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0].len(), 3);
        assert_eq!(pieces[0][2].lon, 0.2, "ends at node 3, not back at node 1");
    }

    #[test]
    fn disconnected_run_cleaves_into_separate_piece() {
        let runs = vec![
            vec![n(1, 0.0, 0.0), n(2, 0.0, 0.1)],
            vec![n(9, 5.0, 5.0), n(8, 5.0, 5.1)], // shares no end node
            vec![n(8, 5.0, 5.1), n(7, 5.0, 5.2)], // chains onto the new piece
        ];
        let pieces = stitch_way_pieces(&runs);
        assert_eq!(pieces.len(), 2, "gap starts a new strand, no connector");
        assert_eq!(pieces[1].len(), 3, "third run chains onto second");
    }

    #[test]
    fn empty_runs_skipped() {
        let runs: Vec<Vec<NodePt>> = vec![vec![], vec![n(1, 0.0, 0.0), n(2, 0.0, 0.1)], vec![]];
        let pieces = stitch_way_pieces(&runs);
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0].len(), 2);
    }
}

/// `GET /api/segments/{segment_id}` — ways composition + traversing routes.
pub fn detail(conn: &Connection, segment_id: &str) -> Result<Option<Value>> {
    let found = match conn.query_row(
        "SELECT segment_id, length_m, way_count, start_station_id, end_station_id
         FROM seg_segment WHERE segment_id = ?1",
        [segment_id],
        |r| -> rusqlite::Result<(String, f64, i64, i64, i64)> {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        },
    ) {
        Ok(row) => row,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(e) => return Err(e.into()),
    };

    let station_name = |id: i64| -> Result<String> {
        Ok(conn.query_row(
            "SELECT station_name FROM station_version
             WHERE station_id = ?1 AND valid_to IS NULL",
            [id],
            |r| r.get(0),
        )?)
    };

    let mut stmt = conn.prepare(
        "SELECT sw.ordinal, sw.osm_way_id, w.name, w.highway, sw.length_m
         FROM seg_segment_way sw
         JOIN osm_way w ON w.osm_way_id = sw.osm_way_id
         WHERE sw.segment_id = ?1
         ORDER BY sw.ordinal",
    )?;
    let ways: Vec<Value> = stmt
        .query_map([segment_id], |r| {
            Ok(json!({
                "ordinal": r.get::<_, i64>(0)?,
                "osm_way_id": r.get::<_, i64>(1)?,
                "name": r.get::<_, Option<String>>(2)?,
                "highway": r.get::<_, Option<String>>(3)?,
                "length_m": r.get::<_, f64>(4)?,
            }))
        })?
        .collect::<Result<_, _>>()?;

    let mut stmt = conn.prepare(
        "SELECT DISTINCT r.route_id, r.route_code, ssv.subroute_name, t.direction
         FROM seg_traversal t
         JOIN subroute_version ssv ON ssv.subroute_version_id = t.subroute_version_id
         JOIN route_version rv ON rv.route_version_id = ssv.route_version_id
         JOIN route r ON r.route_id = rv.route_id
         WHERE t.segment_id = ?1
         ORDER BY r.route_code, t.direction",
    )?;
    let routes: Vec<Value> = stmt
        .query_map([segment_id], |r| {
            Ok(json!({
                "route_id": r.get::<_, i64>(0)?,
                "route_code": r.get::<_, String>(1)?,
                "subroute_name": r.get::<_, Option<String>>(2)?,
                "direction": r.get::<_, i64>(3)?,
            }))
        })?
        .collect::<Result<_, _>>()?;

    Ok(Some(json!({
        "segment_id": found.0,
        "length_m": found.1,
        "way_count": found.2,
        "start_station": {
            "station_id": found.3,
            "station_name": station_name(found.3).ok(),
        },
        "end_station": {
            "station_id": found.4,
            "station_name": station_name(found.4).ok(),
        },
        "ways": ways,
        "routes": routes,
    })))
}
