//! Stats + outlier queries.

use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Value};

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
            |r| -> rusqlite::Result<(i64, i64, i64, i64, i64, i64, i64, i64)> {
                Ok((
                    r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?,
                    r.get(7)?,
                ))
            },
        )?;

    let directions_total: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT subroute_version_id || '#' || direction)
         FROM stop_of_route_version",
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
    ): (i64, Option<f64>, Option<f64>, Option<f64>, Option<i64>, Option<i64>, Option<i64>, Option<i64>) =
        conn.query_row(
        "SELECT COUNT(*), SUM(matched_length_m), SUM(path_length_m), AVG(snap_dist_p50),
                SUM(outliers), SUM(gap_repairs),
                SUM(snap_dist_p95 > 100.0), SUM(matched_length_m < 0.5 * path_length_m)
         FROM seg_match_stat",
        [],
        |r| -> rusqlite::Result<(
            i64,
            Option<f64>,
            Option<f64>,
            Option<f64>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
        )> {
            Ok((
                r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?,
                r.get(7)?,
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
        },
        "outlier_stops": outlier_stops,
        "batch": batch.map(|(id, captured, valid, status)| json!({
            "batch_id": id, "captured_at": captured, "valid_from": valid, "status": status,
        })),
    }))
}
