//! Stats + outlier queries.

use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;
use serde_json::Value;

/// Row shape of the scalar-count query in `stats`.
type CountsRow = (i64, i64, i64, i64, i64, i64);

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

/// Dataset bbox of current stops as [west, south, east, north] — the boot
/// seed for the map's initial fitBounds. `None` when the db has no stops.
pub fn bounds(conn: &Connection) -> Result<Option<[f64; 4]>> {
    let (w, s, e, n): (Option<f64>, Option<f64>, Option<f64>, Option<f64>) = conn.query_row(
        "SELECT MIN(longitude), MIN(latitude), MAX(longitude), MAX(latitude)
         FROM stop_version WHERE valid_to IS NULL",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    Ok(match (w, s, e, n) {
        (Some(w), Some(s), Some(e), Some(n)) => Some([w, s, e, n]),
        _ => None,
    })
}

/// Wire shape of `GET /api/stats`.
#[derive(Serialize)]
struct StatsResponse {
    counts: StatsCounts,
    #[serde(rename = "match")]
    match_summary: StatsMatch,
    outlier_stops: i64,
    batch: Option<StatsBatch>,
}

#[derive(Serialize)]
struct StatsCounts {
    routes: i64,
    stops: i64,
    stations: i64,
    groups: i64,
    osm_ways: i64,
    geometry_drafts: i64,
}

#[derive(Serialize)]
struct StatsMatch {
    directions_with_stats: i64,
    directions_total: i64,
    matched_length_m: f64,
    path_length_m: f64,
    snap_dist_p50_avg: Option<f64>,
    outliers: i64,
    gap_repairs: i64,
    directions_p95_over_100m: i64,
    directions_matched_under_half: i64,
    directions_dev_p95_over_50m: i64,
}

#[derive(Serialize)]
struct StatsBatch {
    batch_id: i64,
    captured_at: String,
    valid_from: String,
    status: String,
}

/// Overview: counts, match summary, outlier count, latest batch.
pub fn stats(conn: &Connection) -> Result<Value> {
    let (routes, stops, stations, groups, osm_ways, drafts) = conn.query_row(
        "SELECT (SELECT COUNT(*) FROM route),
                (SELECT COUNT(*) FROM stop_version WHERE valid_to IS NULL),
                (SELECT COUNT(*) FROM station_version WHERE valid_to IS NULL),
                (SELECT COUNT(*) FROM station_group_version WHERE valid_to IS NULL),
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

    Ok(serde_json::to_value(StatsResponse {
        counts: StatsCounts {
            routes,
            stops,
            stations,
            groups,
            osm_ways,
            geometry_drafts: drafts,
        },
        match_summary: StatsMatch {
            directions_with_stats: with_stats,
            directions_total,
            matched_length_m: matched_len.unwrap_or(0.0),
            path_length_m: path_len.unwrap_or(0.0),
            snap_dist_p50_avg: p50_avg,
            outliers: outliers.unwrap_or(0),
            gap_repairs: gap_repairs.unwrap_or(0),
            directions_p95_over_100m: p95_over_100.unwrap_or(0),
            directions_matched_under_half: matched_under_half.unwrap_or(0),
            directions_dev_p95_over_50m: dev_over_50.unwrap_or(0),
        },
        outlier_stops,
        batch: batch.map(|(id, captured, valid, status)| StatsBatch {
            batch_id: id,
            captured_at: captured,
            valid_from: valid,
            status,
        }),
    })?)
}

/// Wire row of `GET /api/qa/outliers` (`outliers[]` elements).
#[derive(Serialize)]
struct OutlierRow {
    route_id: i64,
    route_code: String,
    direction: i64,
    stop_version_id: i64,
    stop_name: String,
    stop_sequence: i64,
    lat: f64,
    lon: f64,
    chainage_m: Option<f64>,
    snap_dist_m: f64,
    method: String,
    /// [lon, lat], null when not projected
    projected: Option<[f64; 2]>,
}

#[derive(Serialize)]
struct OutliersResponse {
    outliers: Vec<OutlierRow>,
    count: usize,
}

/// `GET /api/qa/outliers?min_snap=&limit=` — worst projections first.
/// (The SELECT also carries p.route_version_id for the WHERE — not a wire
/// field; fill-by-name simply never reads it.)
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
    let rows = stmt
        .query_map(rusqlite::params![min_snap, limit], |r| {
            Ok(OutlierRow {
                route_id: r.get("route_id")?,
                route_code: r.get("route_code")?,
                direction: r.get("direction")?,
                stop_version_id: r.get("stop_version_id")?,
                stop_name: r.get("stop_name")?,
                stop_sequence: r.get("stop_sequence")?,
                lat: r.get("latitude")?,
                lon: r.get("longitude")?,
                chainage_m: r.get("chainage_m")?,
                snap_dist_m: r.get("snap_dist_m")?,
                method: r.get("method")?,
                projected: match (
                    r.get::<_, Option<f64>>("projected_lat")?,
                    r.get::<_, Option<f64>>("projected_lon")?,
                ) {
                    (Some(lat), Some(lon)) => Some([lon, lat]),
                    _ => None,
                },
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let count = rows.len();
    Ok(serde_json::to_value(OutliersResponse {
        outliers: rows,
        count,
    })?)
}

/// Wire row of `GET /api/qa/matchdev` (`directions[]` elements).
#[derive(Serialize)]
struct MatchDevRow {
    route_id: i64,
    route_code: String,
    direction: i64,
    stops: i64,
    matched_ratio: Option<f64>,
    dev_p50_m: f64,
    dev_p95_m: f64,
    dev_max_m: f64,
    snap_dist_p95: Option<f64>,
    gap_repairs: i64,
    outliers: i64,
}

#[derive(Serialize)]
struct MatchDevResponse {
    directions: Vec<MatchDevRow>,
    count: usize,
}

/// `GET /api/qa/matchdev?min_dev=&limit=` — directions whose matched OSM
/// path strays from the src polyline (dev p95), worst first. Mirrors the
/// rm-verify match_dev_report.json rows.
pub fn matchdev(conn: &Connection, min_dev: f64, limit: i64) -> Result<Value> {
    let mut stmt = conn.prepare(
        "SELECT r.route_id, r.route_code, s.direction, s.stops,
                CASE WHEN s.path_length_m > 0
                     THEN s.matched_length_m / s.path_length_m END AS matched_ratio,
                s.dev_p50_m, s.dev_p95_m, s.dev_max_m,
                s.snap_dist_p95, s.gap_repairs, s.outliers
         FROM seg_match_stat s
         JOIN route_version rv ON rv.route_version_id = s.route_version_id
         JOIN route r ON r.route_id = rv.route_id
         WHERE s.dev_p95_m > ?1
         ORDER BY s.dev_p95_m DESC
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![min_dev, limit], |r| {
            Ok(MatchDevRow {
                route_id: r.get("route_id")?,
                route_code: r.get("route_code")?,
                direction: r.get("direction")?,
                stops: r.get("stops")?,
                matched_ratio: r.get("matched_ratio")?,
                dev_p50_m: r.get("dev_p50_m")?,
                dev_p95_m: r.get("dev_p95_m")?,
                dev_max_m: r.get("dev_max_m")?,
                snap_dist_p95: r.get("snap_dist_p95")?,
                gap_repairs: r.get("gap_repairs")?,
                outliers: r.get("outliers")?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let count = rows.len();
    Ok(serde_json::to_value(MatchDevResponse {
        directions: rows,
        count,
    })?)
}
