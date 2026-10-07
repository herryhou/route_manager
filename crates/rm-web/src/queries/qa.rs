//! Stats + outlier queries.

use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Value};

/// Row shape of the scalar-count query in `stats`.
type CountsRow = (i64, i64, i64, i64, i64, i64, i64, i64);

/// Row shape of the seg_match_stat aggregate in `stats`.
type MatchAgg = (
    i64,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
);

/// Overview: counts, match summary, outlier count, latest batch.
pub fn stats(conn: &Connection) -> Result<Value> {
    let (routes, stops, stations, groups, segments, traversals, osm_ways, drafts) = conn
        .query_row(
            "SELECT (SELECT COUNT(*) FROM route),
                    (SELECT COUNT(*) FROM stop_version WHERE valid_to IS NULL),
                    (SELECT COUNT(*) FROM station_version WHERE valid_to IS NULL),
                    (SELECT COUNT(*) FROM station_group_version WHERE valid_to IS NULL),
                    (SELECT COUNT(*) FROM seg_segment),
                    (SELECT COUNT(*) FROM seg_traversal),
                    (SELECT COUNT(*) FROM osm_way),
                    (SELECT COUNT(*) FROM seg_geometry_draft)",
            [],
            |r| -> rusqlite::Result<CountsRow> {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            },
        )?;

    let directions_total: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT subroute_version_id || '#' || direction)
         FROM route_stop_version",
        [],
        |r| r.get(0),
    )?;

    let (
        with_stats,
        matched_len,
        path_len,
        p50_avg,
        outliers,
        gap_repairs,
        p95_over_100,
        matched_under_half,
        dev_over_50,
    ): MatchAgg = conn.query_row(
        "SELECT COUNT(*), SUM(matched_length_m), SUM(path_length_m), AVG(snap_dist_p50),
                SUM(outliers), SUM(gap_repairs),
                SUM(snap_dist_p95 > 100.0), SUM(matched_length_m < 0.5 * path_length_m),
                SUM(dev_p95_m > 50.0)
         FROM seg_match_stat",
        [],
        |r| -> rusqlite::Result<MatchAgg> {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
            ))
        },
    )?;

    let outlier_stops: i64 = conn.query_row(
        "SELECT COUNT(*) FROM path_stop_projection WHERE method IN ('OUTLIER','NO_PATH')",
        [],
        |r| r.get(0),
    )?;

    let split_pairs: i64 = conn.query_row(
        "SELECT COUNT(*) FROM (SELECT pair_key FROM seg_segment WHERE pair_key != ''
         GROUP BY pair_key HAVING COUNT(*) > 1)",
        [],
        |r| r.get(0),
    )?;

    let batch = match conn.query_row(
        "SELECT batch_id, captured_at, valid_from, status
         FROM import_batch ORDER BY batch_id DESC LIMIT 1",
        [],
        |r| -> rusqlite::Result<(i64, String, String, String)> {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        },
    ) {
        Ok(row) => Some(row),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(e) => return Err(e.into()),
    };

    Ok(json!({
        "counts": {
            "routes": routes,
            "stops": stops,
            "stations": stations,
            "groups": groups,
            "segments": segments,
            "traversals": traversals,
            "osm_ways": osm_ways,
            "geometry_drafts": drafts,
        },
        "match": {
            "directions_with_stats": with_stats,
            "directions_total": directions_total,
            "matched_length_m": matched_len.unwrap_or(0.0),
            "path_length_m": path_len.unwrap_or(0.0),
            "snap_dist_p50_avg": p50_avg,
            "outliers": outliers.unwrap_or(0),
            "gap_repairs": gap_repairs.unwrap_or(0),
            "directions_p95_over_100m": p95_over_100.unwrap_or(0),
            "directions_matched_under_half": matched_under_half.unwrap_or(0),
            "directions_dev_p95_over_50m": dev_over_50.unwrap_or(0),
        },
        "outlier_stops": outlier_stops,
        "variants": {
            "split_pairs": split_pairs,
        },
        "batch": batch.map(|(id, captured, valid, status)| json!({
            "batch_id": id, "captured_at": captured, "valid_from": valid, "status": status,
        })),
    }))
}

/// `GET /api/qa/variants?min_spread=&limit=` — station pairs whose pair_key
/// maps to more than one segment (corridor splits), member length gap
/// descending. Mirrors the rm-verify segment_variant_report.json pairs.
pub fn variants(conn: &Connection, min_spread: f64, limit: i64) -> Result<Value> {
    let mut stmt = conn.prepare(
        "SELECT sg.pair_key, sa.station_name, sb.station_name,
                MAX(sg.length_m) - MIN(sg.length_m) AS spread
         FROM seg_segment sg
         JOIN station_version sa
           ON sa.station_id = sg.start_station_id AND sa.valid_to IS NULL
         JOIN station_version sb
           ON sb.station_id = sg.end_station_id AND sb.valid_to IS NULL
         WHERE sg.pair_key != ''
         GROUP BY sg.pair_key
         HAVING COUNT(*) > 1 AND spread > ?1
         ORDER BY spread DESC, sg.pair_key
         LIMIT ?2",
    )?;
    let it = stmt.query_map(rusqlite::params![min_spread, limit], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, f64>(3)?,
        ))
    })?;
    let mut pairs = Vec::new();
    for row in it {
        let (pair, from, to, spread) = row?;
        let mut segments: Vec<Value> = {
            let mut s = conn.prepare(
                "SELECT segment_id, corridor_key, length_m FROM seg_segment
                 WHERE pair_key = ?1 ORDER BY length_m",
            )?;
            let rows = s.query_map([&pair], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, f64>(2)?,
                ))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .map(|(id, ck, len)| json!({"segment_id": id, "corridor_key": ck, "length_m": len}))
                .collect()
        };
        for seg in &mut segments {
            let seg_id = seg["segment_id"].as_str().unwrap_or_default().to_string();
            // majority way list across this segment's traversals, plus its
            // route users (per traversal, so both directions list)
            let mut by_sig: std::collections::BTreeMap<String, Vec<String>> =
                std::collections::BTreeMap::new();
            {
                let mut t = conn.prepare(
                    "SELECT t.way_sig, r.route_code FROM seg_traversal t
                     JOIN route_version rv ON rv.route_version_id = t.route_version_id
                     JOIN route r ON r.route_id = rv.route_id
                     WHERE t.segment_id = ?1",
                )?;
                let rows = t.query_map([&seg_id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })?;
                for row in rows {
                    let (sig, code) = row?;
                    by_sig.entry(sig).or_default().push(code);
                }
            }
            // majority way list represents the segment (ties -> first);
            // routes + traversals span EVERY variant of the segment
            let mut by_sig: Vec<(String, Vec<String>)> = by_sig.into_iter().collect();
            by_sig.sort_by_key(|(sig, rs)| (std::cmp::Reverse(rs.len()), sig.clone()));
            let majority_sig = by_sig
                .first()
                .map(|(sig, _)| sig.clone())
                .unwrap_or_default();
            let routes: Vec<String> = by_sig
                .iter()
                .flat_map(|(_, rs)| rs.iter().cloned())
                .collect();
            let traversals = routes.len();
            let way_ids: Vec<i64> = majority_sig
                .split('|')
                .filter_map(|w| w.parse().ok())
                .collect();
            let mut names: std::collections::HashMap<i64, String> =
                std::collections::HashMap::new();
            if !way_ids.is_empty() {
                let ids = way_ids
                    .iter()
                    .map(|w| w.to_string())
                    .collect::<Vec<_>>()
                    .join(",");
                let mut w = conn.prepare(&format!(
                    "SELECT osm_way_id, name FROM osm_way WHERE osm_way_id IN ({ids})"
                ))?;
                let rows =
                    w.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
                for row in rows {
                    let (id, name) = row?;
                    names.insert(id, name);
                }
            }
            let ways: Vec<Value> = majority_sig
                .split('|')
                .filter_map(|w| w.parse::<i64>().ok())
                .map(|id| {
                    json!({
                        "osm_way_id": id,
                        "name": names.get(&id).cloned().unwrap_or_default(),
                    })
                })
                .collect();
            *seg = json!({
                "segment_id": seg_id,
                "corridor_key": seg["corridor_key"].clone(),
                "length_m": seg["length_m"].clone(),
                "way_sig": majority_sig,
                "ways": ways,
                "traversals": traversals,
                "routes": routes,
            });
        }
        pairs.push(json!({
            "pair_key": pair,
            "from": from,
            "to": to,
            "length_spread_m": spread,
            "segments": segments,
        }));
    }
    let count = pairs.len();
    Ok(json!({ "pairs": pairs, "count": count }))
}

/// `GET /api/qa/outliers?min_snap=&limit=` — worst projections first.
pub fn outliers(conn: &Connection, min_snap: f64, limit: i64) -> Result<Value> {
    let mut stmt = conn.prepare(
        "SELECT r.route_id, r.route_code, p.route_version_id, p.direction,
                sv.stop_version_id, sv.stop_name, p.stop_sequence,
                sv.latitude, sv.longitude, p.chainage_m, p.snap_dist_m, p.method,
                p.projected_lat, p.projected_lon
         FROM path_stop_projection p
         JOIN route_stop_version rsv
           ON rsv.route_stop_version_id = p.route_stop_version_id
         JOIN stop_version sv ON sv.stop_version_id = rsv.stop_version_id
         JOIN subroute_version ssv ON ssv.subroute_version_id = rsv.subroute_version_id
         JOIN route_version rv ON rv.route_version_id = ssv.route_version_id
         JOIN route r ON r.route_id = rv.route_id
         WHERE p.snap_dist_m > ?1 OR p.method IN ('OUTLIER','NO_PATH')
         ORDER BY p.snap_dist_m DESC
         LIMIT ?2",
    )?;
    let it = stmt.query_map(rusqlite::params![min_snap, limit], |r| {
        Ok(json!({
            "route_id": r.get::<_, i64>(0)?,
            "route_code": r.get::<_, String>(1)?,
            "direction": r.get::<_, i64>(3)?,
            "stop_version_id": r.get::<_, i64>(4)?,
            "stop_name": r.get::<_, String>(5)?,
            "stop_sequence": r.get::<_, i64>(6)?,
            "lat": r.get::<_, f64>(7)?,
            "lon": r.get::<_, f64>(8)?,
            "chainage_m": r.get::<_, Option<f64>>(9)?,
            "snap_dist_m": r.get::<_, f64>(10)?,
            "method": r.get::<_, String>(11)?,
            "projected": match (r.get::<_, Option<f64>>(12)?, r.get::<_, Option<f64>>(13)?) {
                (Some(lat), Some(lon)) => json!([lon, lat]),
                _ => Value::Null,
            },
        }))
    })?;
    let rows: Vec<Value> = it.collect::<Result<_, _>>()?;
    let count = rows.len();
    Ok(json!({ "outliers": rows, "count": count }))
}

/// `GET /api/qa/matchdev?min_dev=&limit=` — directions whose matched OSM
/// path strays from the src polyline (dev p95), worst first. Mirrors the
/// rm-verify match_dev_report.json rows.
pub fn matchdev(conn: &Connection, min_dev: f64, limit: i64) -> Result<Value> {
    let mut stmt = conn.prepare(
        "SELECT r.route_id, r.route_code, s.direction, s.stops,
                CASE WHEN s.path_length_m > 0
                     THEN s.matched_length_m / s.path_length_m END,
                s.dev_p50_m, s.dev_p95_m, s.dev_max_m,
                s.snap_dist_p95, s.gap_repairs, s.outliers
         FROM seg_match_stat s
         JOIN route_version rv ON rv.route_version_id = s.route_version_id
         JOIN route r ON r.route_id = rv.route_id
         WHERE s.dev_p95_m > ?1
         ORDER BY s.dev_p95_m DESC
         LIMIT ?2",
    )?;
    let it = stmt.query_map(rusqlite::params![min_dev, limit], |r| {
        Ok(json!({
            "route_id": r.get::<_, i64>(0)?,
            "route_code": r.get::<_, String>(1)?,
            "direction": r.get::<_, i64>(2)?,
            "stops": r.get::<_, i64>(3)?,
            "matched_ratio": r.get::<_, Option<f64>>(4)?,
            "dev_p50_m": r.get::<_, f64>(5)?,
            "dev_p95_m": r.get::<_, f64>(6)?,
            "dev_max_m": r.get::<_, f64>(7)?,
            "snap_dist_p95": r.get::<_, Option<f64>>(8)?,
            "gap_repairs": r.get::<_, i64>(9)?,
            "outliers": r.get::<_, i64>(10)?,
        }))
    })?;
    let rows: Vec<Value> = it.collect::<Result<_, _>>()?;
    let count = rows.len();
    Ok(json!({ "directions": rows, "count": count }))
}
