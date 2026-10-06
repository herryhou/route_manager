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
        "batch": batch.map(|(id, captured, valid, status)| json!({
            "batch_id": id, "captured_at": captured, "valid_from": valid, "status": status,
        })),
    }))
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
