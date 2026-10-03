//! Segment registry queries: bbox listing + detail.

use anyhow::Result;
use rm_core::{polyline, LatLng};
use rusqlite::Connection;
use serde_json::{json, Value};

use crate::geojson;

/// `GET /api/segments?bbox=minLon,minLat,maxLon,maxLat&min_routes=&limit=`
/// SQL prefilters on endpoint-station coords, Rust confirms any vertex in bbox.
/// Sorted by route_count DESC.
pub fn list(
    conn: &Connection,
    bbox: (f64, f64, f64, f64),
    min_routes: i64,
    limit: i64,
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
    let it = stmt.query_map(
        rusqlite::params![min_lon, min_lat, max_lon, max_lat],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(6)?,
            ))
        },
    )?;
    let rows: Vec<(String, String, f64, i64, i64)> = it.collect::<Result<_, _>>()?;

    let mut features = Vec::new();
    for (id, enc, length_m, way_count, route_count) in rows {
        if route_count < min_routes {
            continue;
        }
        let pts: Vec<LatLng> = match polyline::decode(&enc) {
            Ok(p) if p.len() >= 2 => p,
            _ => continue,
        };
        if !pts
            .iter()
            .any(|p| p.lon >= min_lon && p.lon <= max_lon && p.lat >= min_lat && p.lat <= max_lat)
        {
            continue;
        }
        features.push(geojson::feature(
            geojson::line_string(&pts),
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

/// `GET /api/segments/{segment_id}` — ways composition + traversing routes.
pub fn detail(conn: &Connection, segment_id: &str) -> Result<Option<Value>> {
    let found = match conn.query_row(
        "SELECT segment_id, length_m, way_count, start_station_id, end_station_id
         FROM seg_segment WHERE segment_id = ?1",
        [segment_id],
        |r| -> rusqlite::Result<(String, f64, i64, i64, i64)> {
            Ok((
                r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?,
            ))
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
