//! Route list/detail queries.

use anyhow::{Context, Result};
use rm_core::polyline;
use rm_core::LatLng;
use rusqlite::Connection;
use serde::Serialize;
use serde_json::{json, Value};

use crate::geojson;

fn split_list(s: Option<String>) -> Vec<String> {
    s.map(|v| v.split(',').map(str::to_string).collect())
        .unwrap_or_default()
}

/// Wire row of the `directions` arrays in routes list/detail.
#[derive(Serialize, Clone)]
struct DirectionRow {
    route_id: i64,
    direction: i64,
    stops: i64,
    matched_ratio: Option<f64>,
    snap_dist_p50_m: Option<f64>,
    snap_dist_p95_m: Option<f64>,
    outliers: Option<i64>,
}

fn direction_rows(conn: &Connection, route_ids: &[i64]) -> Result<Vec<DirectionRow>> {
    let mut out = Vec::new();
    if route_ids.is_empty() {
        return Ok(out);
    }
    let placeholders = vec!["?"; route_ids.len()].join(",");
    let sql = format!(
        "SELECT r.route_id, rsv.direction, COUNT(*) AS stops,
                ms.matched_length_m, ms.path_length_m, ms.snap_dist_p50, ms.snap_dist_p95,
                ms.outliers
         FROM route_stop_version rsv
         JOIN subroute_version sv ON sv.subroute_version_id = rsv.subroute_version_id
         JOIN route_version rv ON rv.route_version_id = sv.route_version_id
         JOIN route r ON r.route_id = rv.route_id
         LEFT JOIN seg_match_stat ms
           ON ms.route_version_id = rv.route_version_id AND ms.direction = rsv.direction
         WHERE r.route_id IN ({placeholders})
           AND r.current_route_version_id = rv.route_version_id
         GROUP BY r.route_id, rsv.direction
         ORDER BY r.route_id, rsv.direction"
    );
    let mut stmt = conn.prepare(&sql)?;
    let ids: Vec<String> = route_ids.iter().map(|i| i.to_string()).collect();
    let it = stmt.query_map(rusqlite::params_from_iter(ids), |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, Option<f64>>(3)?,
            r.get::<_, Option<f64>>(4)?,
            r.get::<_, Option<f64>>(5)?,
            r.get::<_, Option<f64>>(6)?,
            r.get::<_, Option<i64>>(7)?,
        ))
    })?;
    for row in it {
        let (route_id, direction, stops, matched, path, p50, p95, outliers) = row?;
        let ratio = match (matched, path) {
            (Some(m), Some(p)) if p > 0.0 => Some(m / p),
            _ => None,
        };
        out.push(DirectionRow {
            route_id,
            direction,
            stops,
            matched_ratio: ratio,
            snap_dist_p50_m: p50,
            snap_dist_p95_m: p95,
            outliers,
        });
    }
    Ok(out)
}

/// Fold the flat direction rows into per-route `directions` arrays.
fn directions_by_route(
    rows: Vec<DirectionRow>,
) -> std::collections::HashMap<i64, Vec<DirectionRow>> {
    let mut map = std::collections::HashMap::new();
    for d in rows {
        map.entry(d.route_id).or_insert_with(Vec::new).push(d);
    }
    map
}

/// Wire row of `GET /api/routes`.
#[derive(Serialize)]
struct RouteListRow {
    route_id: i64,
    route_code: String,
    ext_uid: String,
    subroute_name: Option<String>,
    providers: Vec<String>,
    side_routes: Vec<String>,
    main_routes: Vec<String>,
    directions: Vec<DirectionRow>,
}

#[derive(Serialize)]
struct RoutesListResponse {
    routes: Vec<RouteListRow>,
}

/// `GET /api/routes?q=&limit=` — one row per route with per-direction QA.
pub fn list(conn: &Connection, q: &str, limit: i64) -> Result<Value> {
    let mut stmt = conn.prepare(
        "SELECT r.route_id, r.route_code, r.ext_uid,
                (SELECT sv.subroute_name FROM subroute_version sv
                  WHERE sv.route_version_id = rv.route_version_id LIMIT 1),
                (SELECT GROUP_CONCAT(DISTINCT rp.provider_name) FROM route_provider rp
                  WHERE rp.route_id = r.route_id),
                (SELECT GROUP_CONCAT(DISTINCT r2.route_code) FROM route_link rl
                  JOIN route r2 ON r2.route_id = rl.side_route_id
                  WHERE rl.main_route_id = r.route_id),
                (SELECT GROUP_CONCAT(DISTINCT r2.route_code) FROM route_link rl
                  JOIN route r2 ON r2.route_id = rl.main_route_id
                  WHERE rl.side_route_id = r.route_id)
         FROM route r
         JOIN route_version rv ON rv.route_version_id = r.current_route_version_id
         WHERE (?1 = '' OR r.route_code LIKE ?1 || '%'
                OR (SELECT sv.subroute_name FROM subroute_version sv
                     WHERE sv.route_version_id = rv.route_version_id LIMIT 1) LIKE '%' || ?1 || '%')
         ORDER BY CASE WHEN r.route_code GLOB '[0-9]*'
                       THEN CAST(r.route_code AS INTEGER) ELSE 99999 END,
                  r.route_code
         LIMIT ?2",
    )?;
    let it = stmt.query_map(rusqlite::params![q, limit], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, Option<String>>(5)?,
            r.get::<_, Option<String>>(6)?,
        ))
    })?;
    let mut routes = Vec::new();
    let mut ids = Vec::new();
    for row in it {
        let (route_id, route_code, ext_uid, name, providers, side, main) = row?;
        ids.push(route_id);
        routes.push((
            route_id,
            route_code,
            ext_uid,
            name,
            split_list(providers),
            split_list(side),
            split_list(main),
        ));
    }

    let dirs = directions_by_route(direction_rows(conn, &ids)?);
    let routes: Vec<RouteListRow> = routes
        .into_iter()
        .map(
            |(route_id, route_code, ext_uid, name, providers, side, main)| RouteListRow {
                route_id,
                route_code,
                ext_uid,
                subroute_name: name,
                providers,
                side_routes: side,
                main_routes: main,
                directions: dirs.get(&route_id).cloned().unwrap_or_default(),
            },
        )
        .collect();
    Ok(serde_json::to_value(RoutesListResponse { routes })?)
}

/// Wire row of the `subroutes` array in route detail.
#[derive(Serialize)]
struct SubrouteRow {
    subroute_version_id: i64,
    subroute_name: String,
}

/// Wire shape of `GET /api/routes/{id}`.
#[derive(Serialize)]
struct RouteDetailResponse {
    route_id: i64,
    route_code: String,
    ext_uid: String,
    /// ext_uid without the provider prefix (e.g. `tcg:1` → `1`)
    xno: String,
    revision: i64,
    version_no: i64,
    valid_from: String,
    status: String,
    subroute_name: Option<String>,
    subroutes: Vec<SubrouteRow>,
    providers: Vec<String>,
    side_routes: Vec<String>,
    main_routes: Vec<String>,
    directions: Vec<DirectionRow>,
}

/// `GET /api/routes/{id}` — None when the id is unknown.
pub fn detail(conn: &Connection, route_id: i64) -> Result<Option<Value>> {
    let found = match conn.query_row(
        "SELECT r.route_id, r.route_code, r.ext_uid, r.revision,
                rv.route_version_id, rv.version_no, rv.valid_from, rv.status
         FROM route r
         JOIN route_version rv ON rv.route_version_id = r.current_route_version_id
         WHERE r.route_id = ?1",
        [route_id],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
            ))
        },
    ) {
        Ok(row) => Some(row),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(e) => return Err(e.into()),
    };

    let Some((id, route_code, ext_uid, revision, rv_id, version_no, valid_from, status)) = found
    else {
        return Ok(None);
    };

    let mut stmt = conn.prepare(
        "SELECT subroute_version_id, subroute_name
         FROM subroute_version WHERE route_version_id = ?1",
    )?;
    let subroutes: Vec<SubrouteRow> = stmt
        .query_map([rv_id], |r| {
            Ok(SubrouteRow {
                subroute_version_id: r.get("subroute_version_id")?,
                subroute_name: r.get("subroute_name")?,
            })
        })?
        .collect::<Result<_, _>>()?;

    let providers = split_list(conn.query_row(
        "SELECT GROUP_CONCAT(DISTINCT provider_name) FROM route_provider WHERE route_id = ?1",
        [route_id],
        |r| r.get(0),
    )?);
    let side = split_list(conn.query_row(
        "SELECT GROUP_CONCAT(DISTINCT r2.route_code) FROM route_link rl
         JOIN route r2 ON r2.route_id = rl.side_route_id WHERE rl.main_route_id = ?1",
        [route_id],
        |r| r.get(0),
    )?);
    let main = split_list(conn.query_row(
        "SELECT GROUP_CONCAT(DISTINCT r2.route_code) FROM route_link rl
         JOIN route r2 ON r2.route_id = rl.main_route_id WHERE rl.side_route_id = ?1",
        [route_id],
        |r| r.get(0),
    )?);

    let dirs = directions_by_route(direction_rows(conn, &[route_id])?)
        .remove(&route_id)
        .unwrap_or_default();

    Ok(Some(serde_json::to_value(RouteDetailResponse {
        route_id: id,
        xno: ext_uid.strip_prefix("tcg:").unwrap_or(&ext_uid).to_string(),
        subroute_name: subroutes.first().map(|s| s.subroute_name.clone()),
        subroutes,
        providers,
        side_routes: side,
        main_routes: main,
        directions: dirs,
        route_code,
        ext_uid,
        revision,
        version_no,
        valid_from,
        status,
    })?))
}

/// Wire properties of one path feature (`seg_geometry_draft` piece).
#[derive(Serialize)]
struct PathPieceProps {
    ordinal: i64,
    length_m: f64,
    geometry_method: String,
    from_seq: i64,
    to_seq: i64,
    from_station_id: i64,
    from_station_name: String,
    to_station_id: i64,
    to_station_name: String,
}

/// `GET /api/routes/{id}/directions/{dir}/path` — canonical geometry
/// as one LineString feature per seg_geometry_draft piece. Each piece's
/// boundary stations (the directed station pair) ride along in properties.
pub fn path(conn: &Connection, route_id: i64, direction: i64) -> Result<Value> {
    let mut stmt = conn.prepare(
        "SELECT gd.ordinal, gd.encoded_polyline, gd.length_m, gd.geometry_method,
                gd.from_route_stop_version_id, gd.to_route_stop_version_id,
                fst.station_id AS from_station_id, fst.station_name AS from_station_name,
                tst.station_id AS to_station_id, tst.station_name AS to_station_name
         FROM seg_geometry_draft gd
         JOIN subroute_version sv ON sv.subroute_version_id = gd.subroute_version_id
         JOIN route_version rv ON rv.route_version_id = sv.route_version_id
         JOIN route r ON r.route_id = rv.route_id
         JOIN route_stop_version frs
           ON frs.route_stop_version_id = gd.from_route_stop_version_id
         JOIN stop_version fsv ON fsv.stop_version_id = frs.stop_version_id
         JOIN station_version fst ON fst.station_id = fsv.station_id AND fst.valid_to IS NULL
         JOIN route_stop_version trs
           ON trs.route_stop_version_id = gd.to_route_stop_version_id
         JOIN stop_version tsv ON tsv.stop_version_id = trs.stop_version_id
         JOIN station_version tst ON tst.station_id = tsv.station_id AND tst.valid_to IS NULL
         WHERE r.route_id = ?1 AND r.current_route_version_id = rv.route_version_id
           AND gd.direction = ?2
         ORDER BY gd.ordinal",
    )?;
    let it = stmt.query_map(rusqlite::params![route_id, direction], |r| {
        Ok((
            r.get::<_, i64>("ordinal")?,
            r.get::<_, String>("encoded_polyline")?,
            r.get::<_, f64>("length_m")?,
            r.get::<_, String>("geometry_method")?,
            r.get::<_, i64>("from_route_stop_version_id")?,
            r.get::<_, i64>("to_route_stop_version_id")?,
            r.get::<_, i64>("from_station_id")?,
            r.get::<_, String>("from_station_name")?,
            r.get::<_, i64>("to_station_id")?,
            r.get::<_, String>("to_station_name")?,
        ))
    })?;

    let mut features = Vec::new();
    for row in it {
        let (
            ordinal,
            encoded,
            length_m,
            method,
            from_rsv,
            to_rsv,
            from_sid,
            from_sname,
            to_sid,
            to_sname,
        ) = row?;
        let pts = polyline::decode(&encoded)?;
        if pts.len() < 2 {
            continue; // degenerate piece — nothing to draw
        }
        let (from_seq, to_seq) = rsv_sequences(conn, from_rsv, to_rsv)?;
        features.push(geojson::feature(
            geojson::line_string(&pts),
            serde_json::to_value(PathPieceProps {
                ordinal,
                length_m,
                geometry_method: method,
                from_seq,
                to_seq,
                from_station_id: from_sid,
                from_station_name: from_sname,
                to_station_id: to_sid,
                to_station_name: to_sname,
            })?,
        ));
    }
    Ok(geojson::feature_collection(features))
}

fn rsv_sequences(conn: &Connection, from: i64, to: i64) -> Result<(i64, i64)> {
    Ok(conn.query_row(
        "SELECT (SELECT stop_sequence FROM route_stop_version
                  WHERE route_stop_version_id = ?1),
               (SELECT stop_sequence FROM route_stop_version
                  WHERE route_stop_version_id = ?2)",
        [from, to],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
}

/// Wire row of `GET /api/routes/{id}/directions/{dir}/stops`.
#[derive(Serialize)]
struct RouteStopRow {
    stop_sequence: i64,
    source_order_no: Option<i64>,
    stop_version_id: i64,
    stop_code: String,
    stop_name: String,
    lat: f64,
    lon: f64,
    station_id: i64,
    chainage_m: Option<f64>,
    snap_dist_m: Option<f64>,
    method: Option<String>,
    monotonic_clamped: Option<i64>,
    /// [lon, lat], null when not projected
    projected: Option<[f64; 2]>,
    bearing: Option<f64>,
}

#[derive(Serialize)]
struct RouteStopsResponse {
    stops: Vec<RouteStopRow>,
}

/// `GET /api/routes/{id}/directions/{dir}/stops` — ordered stops + projection QA.
pub fn stops(conn: &Connection, route_id: i64, direction: i64) -> Result<Value> {
    let mut stmt = conn.prepare(
        "SELECT rsv.stop_sequence, rsv.source_order_no,
                sv2.stop_version_id, sv2.stop_code, sv2.stop_name,
                sv2.latitude, sv2.longitude, sv2.station_id,
                p.chainage_m, p.snap_dist_m, p.method, p.monotonic_clamped,
                p.projected_lat, p.projected_lon, sb.bearing_deg
         FROM route_stop_version rsv
         JOIN subroute_version sv ON sv.subroute_version_id = rsv.subroute_version_id
         JOIN route_version rv ON rv.route_version_id = sv.route_version_id
         JOIN route r ON r.route_id = rv.route_id
         JOIN stop_version sv2 ON sv2.stop_version_id = rsv.stop_version_id
         LEFT JOIN path_stop_projection p
           ON p.route_stop_version_id = rsv.route_stop_version_id
         LEFT JOIN stop_bearing sb ON sb.stop_version_id = sv2.stop_version_id
         WHERE r.route_id = ?1 AND r.current_route_version_id = rv.route_version_id
           AND rsv.direction = ?2
         ORDER BY rsv.stop_sequence",
    )?;
    let stops = stmt
        .query_map(rusqlite::params![route_id, direction], |r| {
            Ok(RouteStopRow {
                stop_sequence: r.get("stop_sequence")?,
                source_order_no: r.get("source_order_no")?,
                stop_version_id: r.get("stop_version_id")?,
                stop_code: r.get("stop_code")?,
                stop_name: r.get("stop_name")?,
                lat: r.get("latitude")?,
                lon: r.get("longitude")?,
                station_id: r.get("station_id")?,
                chainage_m: r.get("chainage_m")?,
                snap_dist_m: r.get("snap_dist_m")?,
                method: r.get("method")?,
                monotonic_clamped: r.get("monotonic_clamped")?,
                projected: match (
                    r.get::<_, Option<f64>>("projected_lat")?,
                    r.get::<_, Option<f64>>("projected_lon")?,
                ) {
                    (Some(lat), Some(lon)) => Some([lon, lat]),
                    _ => None,
                },
                bearing: r.get("bearing_deg")?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(serde_json::to_value(RouteStopsResponse { stops })?)
}

/// One matched way run row (`seg_match_way` × way name).
struct WayRun {
    ordinal: i64,
    osm_way_id: i64,
    entry_node_id: i64,
    exit_node_id: i64,
    cum_start_m: f64,
    cum_end_m: f64,
    name: Option<String>,
}

/// Wire properties of one ways feature.
#[derive(Serialize)]
struct WayProps {
    ordinal: i64,
    osm_way_id: i64,
    name: Option<String>,
    entry_node_id: i64,
    exit_node_id: i64,
    entry_coords: [f64; 2],
    exit_coords: [f64; 2],
    cum_start_m: f64,
    cum_end_m: f64,
}

/// `GET /api/routes/{id}/directions/{dir}/ways` — matched OSM way runs,
/// each way's node run sliced to its [entry, exit] window.
pub fn ways(conn: &Connection, route_id: i64, dir: i64) -> Result<Value> {
    let mut stmt = conn.prepare(
        "SELECT mw.ordinal, mw.osm_way_id, mw.entry_node_id, mw.exit_node_id,
                mw.cum_start_m, mw.cum_end_m, w.name
         FROM seg_match_way mw
         JOIN subroute_version sv ON sv.subroute_version_id = mw.subroute_version_id
         JOIN route_version rv ON rv.route_version_id = sv.route_version_id
         JOIN route r ON r.route_id = rv.route_id
         JOIN osm_way w ON w.osm_way_id = mw.osm_way_id
         WHERE r.route_id = ?1 AND r.current_route_version_id = rv.route_version_id
           AND mw.direction = ?2
         ORDER BY mw.ordinal",
    )?;
    let runs: Vec<WayRun> = stmt
        .query_map(rusqlite::params![route_id, dir], |r| {
            Ok(WayRun {
                ordinal: r.get("ordinal")?,
                osm_way_id: r.get("osm_way_id")?,
                entry_node_id: r.get("entry_node_id")?,
                exit_node_id: r.get("exit_node_id")?,
                cum_start_m: r.get("cum_start_m")?,
                cum_end_m: r.get("cum_end_m")?,
                name: r.get("name")?,
            })
        })?
        .collect::<Result<_, _>>()?;

    let mut features = Vec::new();
    for run in runs {
        // ordered node run for the way
        let mut nstmt = conn.prepare(
            "SELECT n.osm_node_id, n.lat, n.lon
             FROM osm_way_node wn JOIN osm_node n ON n.osm_node_id = wn.osm_node_id
             WHERE wn.osm_way_id = ?1
             ORDER BY wn.ordinal",
        )?;
        let nodes: Vec<(i64, f64, f64)> = nstmt
            .query_map([run.osm_way_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?;

        let (lo, hi) = match (
            nodes.iter().position(|(id, _, _)| *id == run.entry_node_id),
            nodes.iter().position(|(id, _, _)| *id == run.exit_node_id),
        ) {
            (Some(a), Some(b)) => (a.min(b), a.max(b)),
            _ => (0, nodes.len().saturating_sub(1)), // fall back to full run
        };
        let pts: Vec<LatLng> = nodes[lo..=hi]
            .iter()
            .map(|(_, lat, lon)| LatLng {
                lat: *lat,
                lon: *lon,
            })
            .collect();
        if pts.len() < 2 {
            continue;
        }
        features.push(geojson::feature(
            geojson::line_string(&pts),
            serde_json::to_value(WayProps {
                ordinal: run.ordinal,
                osm_way_id: run.osm_way_id,
                name: run.name,
                entry_node_id: run.entry_node_id,
                exit_node_id: run.exit_node_id,
                entry_coords: [pts.first().unwrap().lon, pts.first().unwrap().lat],
                exit_coords: [pts.last().unwrap().lon, pts.last().unwrap().lat],
                cum_start_m: run.cum_start_m,
                cum_end_m: run.cum_end_m,
            })?,
        ));
    }
    Ok(json!({ "type": "FeatureCollection", "features": features }))
}

/// Wire row of the staging `stops` array in raw.
#[derive(Serialize)]
struct RawStopRow {
    source_order_no: i64,
    ivr_no: Option<i64>,
    sid: Option<i64>,
    node_id: String,
    station_name: String,
    lat: f64,
    lon: f64,
}

/// The effective polyline the downstream pipeline consumes: the override
/// when one exists, else the src staging line.
#[derive(Serialize)]
struct EffectiveLine {
    points: Vec<[f64; 2]>,
    source: String,
    updated_at: Option<String>,
}

/// Wire shape of `GET /api/routes/{id}/directions/{dir}/raw`.
#[derive(Serialize)]
struct RawResponse {
    batch_id: i64,
    path: Option<Value>,
    stops: Vec<RawStopRow>,
    effective: EffectiveLine,
}

/// `GET /api/routes/{id}/directions/{dir}/raw` — latest-batch staging rows
/// for compare: src_route_point polyline + ordered src_stop_edge stops.
pub fn raw(conn: &Connection, route_id: i64, dir: i64) -> Result<Option<Value>> {
    let xno: Option<String> = match conn.query_row(
        "SELECT substr(ext_uid, 5) FROM route WHERE route_id = ?1",
        [route_id],
        |r| r.get(0),
    ) {
        Ok(x) => Some(x),
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let Some(xno) = xno else {
        return Ok(None);
    };

    let batch_id: i64 = conn.query_row(
        "SELECT COALESCE(MAX(batch_id), 0) FROM import_batch",
        [],
        |r| r.get(0),
    )?;

    // missing staging row (e.g. a later batch dropped this route) → no path
    let encoded: Option<String> = match conn.query_row(
        "SELECT encoded FROM src_route_point
         WHERE batch_id = ?1 AND xno = ?2 AND direction = ?3",
        rusqlite::params![batch_id, xno, dir],
        |r| r.get(0),
    ) {
        Ok(enc) => Some(enc),
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(e) => return Err(e.into()),
    };
    let path = match encoded.as_ref() {
        Some(enc) => match polyline::decode(enc) {
            Ok(pts) if pts.len() >= 2 => Some(geojson::feature(
                geojson::line_string(&pts),
                json!({ "source": "src_route_point" }),
            )),
            _ => None,
        },
        None => None,
    };

    let mut stmt = conn.prepare(
        "SELECT source_order_no, ivr_no, sid, node_id, station_name, lat, lon
         FROM src_stop_edge
         WHERE batch_id = ?1 AND xno = ?2 AND direction = ?3
         ORDER BY source_order_no",
    )?;
    let stops: Vec<RawStopRow> = stmt
        .query_map(rusqlite::params![batch_id, xno, dir], |r| {
            Ok(RawStopRow {
                source_order_no: r.get("source_order_no")?,
                ivr_no: r.get("ivr_no")?,
                sid: r.get("sid")?,
                node_id: r.get("node_id")?,
                station_name: r.get("station_name")?,
                lat: r.get("lat")?,
                lon: r.get("lon")?,
            })
        })?
        .collect::<Result<_, _>>()?;

    // effective = override ?? src — the line match/segments/registry
    // consume. `updated_at` is the override's concurrency token (null when
    // the effective line is plain src). A DB migrated only through 0012 has
    // no polyline_override table at all: degrade to plain src (the viewer
    // never migrates; the first edit-write creates the table lazily).
    let has_override_table: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'polyline_override'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .context("probe polyline_override")?
        > 0;
    let (eff_encoded, source, eff_updated_at): (String, &str, Option<String>) =
        if has_override_table {
            let updated_at = conn
                .query_row(
                    "SELECT updated_at FROM polyline_override WHERE xno = ?1 AND direction = ?2",
                    rusqlite::params![xno, dir],
                    |r| r.get(0),
                )
                .ok();
            let source = if updated_at.is_some() {
                "override"
            } else {
                "src"
            };
            (
                rm_db::effective_encoded(conn, batch_id, &xno, dir)?,
                source,
                updated_at,
            )
        } else {
            (encoded.clone().unwrap_or_default(), "src", None)
        };
    let eff_points: Vec<[f64; 2]> = polyline::decode(&eff_encoded)
        .map(|pts| pts.iter().map(|p| [p.lat, p.lon]).collect())
        .unwrap_or_default();

    Ok(Some(serde_json::to_value(RawResponse {
        batch_id,
        path,
        stops,
        effective: EffectiveLine {
            points: eff_points,
            source: source.to_string(),
            updated_at: eff_updated_at,
        },
    })?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TS: &str = "2026-01-01T00:00:00Z";

    /// Route 1 with two route_version chains — edit applies mint a new
    /// chain (rv 2) and keep the old rows (rv 1), so a query filtered on
    /// route_id alone merges superseded chains into the sequence.
    fn two_chain_fixture(conn: &Connection) {
        conn.execute(
            "INSERT INTO station (station_id, ext_uid, created_at) VALUES (1, 'tcg:node:1', ?1)",
            [TS],
        )
        .unwrap();
        for stop_id in [1, 2] {
            conn.execute(
                "INSERT INTO stop (stop_id, ext_uid, created_at) VALUES (?1, ?2, ?3)",
                rusqlite::params![stop_id, format!("tcg:node:{stop_id}"), TS],
            )
            .unwrap();
        }
        // Chain A (superseded): stop_versions 10/11; chain B (current): 20/21.
        for (sv_id, stop_id, code) in [(10, 1, "A1"), (11, 2, "A2"), (20, 1, "B1"), (21, 2, "B2")] {
            conn.execute(
                "INSERT INTO stop_version (stop_version_id, stop_id, station_id, stop_code,
                                           stop_name, direction, latitude, longitude,
                                           valid_from, created_at)
                 VALUES (?1, ?2, 1, ?3, 'S', 0, 24.0, 120.0, ?4, ?4)",
                rusqlite::params![sv_id, stop_id, code, TS],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO route (route_id, route_code, ext_uid, created_at, updated_at)
             VALUES (1, '1', 'tcg:1', ?1, ?1)",
            [TS],
        )
        .unwrap();
        for (rv_id, vno, status) in [(1, 1, "SUPERSEDED"), (2, 2, "PUBLISHED")] {
            conn.execute(
                "INSERT INTO route_version (route_version_id, route_id, version_no,
                                             valid_from, status, created_at)
                 VALUES (?1, 1, ?2, ?3, ?4, ?3)",
                rusqlite::params![rv_id, vno, TS, status],
            )
            .unwrap();
        }
        conn.execute(
            "UPDATE route SET current_route_version_id = 2 WHERE route_id = 1",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO subroute (subroute_id, route_id, subroute_code, created_at)
             VALUES (1, 1, '1-0', ?1)",
            [TS],
        )
        .unwrap();
        for (sv_id, rv_id) in [(1, 1), (2, 2)] {
            conn.execute(
                "INSERT INTO subroute_version (subroute_version_id, route_version_id,
                                                subroute_id, subroute_name, created_at)
                 VALUES (?1, ?2, 1, '1', ?3)",
                rusqlite::params![sv_id, rv_id, TS],
            )
            .unwrap();
        }
        for (rsv_id, sv_id, stop_version_id, seq) in
            [(1, 1, 10, 1), (2, 1, 11, 2), (3, 2, 20, 1), (4, 2, 21, 2)]
        {
            conn.execute(
                "INSERT INTO route_stop_version (route_stop_version_id, subroute_version_id,
                                                  stop_version_id, direction, stop_sequence, created_at)
                 VALUES (?1, ?2, ?3, 0, ?4, ?5)",
                rusqlite::params![rsv_id, sv_id, stop_version_id, seq, TS],
            )
            .unwrap();
        }
    }

    #[test]
    fn stops_returns_only_the_current_route_version_chain() {
        let conn = rm_db::open_in_memory().unwrap();
        two_chain_fixture(&conn);

        let out = stops(&conn, 1, 0).unwrap();
        let codes: Vec<&str> = out["stops"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["stop_code"].as_str().unwrap())
            .collect();
        assert_eq!(
            codes,
            ["B1", "B2"],
            "superseded chain A rows must not merge in"
        );
    }

    #[test]
    fn direction_rows_counts_only_the_current_chain() {
        let conn = rm_db::open_in_memory().unwrap();
        two_chain_fixture(&conn);

        let rows = direction_rows(&conn, &[1]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].stops, 2,
            "superseded chain A rows must not merge in"
        );
    }

    /// Two-chain fixture plus the station_version + seg_geometry_draft rows
    /// path() reads — one piece per chain, distinguished by length_m.
    fn path_fixture(conn: &Connection) {
        two_chain_fixture(conn);
        conn.execute(
            "INSERT INTO station (station_id, ext_uid, created_at) VALUES (2, 'tcg:node:2', ?1)",
            [TS],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO station_group (station_group_id, created_at) VALUES (1, ?1)",
            [TS],
        )
        .unwrap();
        for (stv_id, station_id, name) in [(1, 1, "SA"), (2, 2, "SB")] {
            conn.execute(
                "INSERT INTO station_version (station_version_id, station_id, station_group_id,
                                               station_name, latitude, longitude, valid_from, created_at)
                 VALUES (?1, ?2, 1, ?3, 24.0, 120.0, ?4, ?4)",
                rusqlite::params![stv_id, station_id, name, TS],
            )
            .unwrap();
        }
        for (sv_id, from_rsv, to_rsv, length) in [(1, 1, 2, 100.0), (2, 3, 4, 200.0)] {
            conn.execute(
                "INSERT INTO seg_geometry_draft (subroute_version_id, direction, ordinal,
                                                  from_route_stop_version_id, to_route_stop_version_id,
                                                  encoded_polyline, length_m, geometry_method)
                 VALUES (?1, 0, 1, ?2, ?3, '_p~iF~ps|U_ulLnnqC', ?4, 'path')",
                rusqlite::params![sv_id, from_rsv, to_rsv, length],
            )
            .unwrap();
        }
    }

    #[test]
    fn path_draws_only_the_current_chain() {
        let conn = rm_db::open_in_memory().unwrap();
        path_fixture(&conn);

        let out = path(&conn, 1, 0).unwrap();
        let features = out["features"].as_array().unwrap();
        assert_eq!(features.len(), 1, "superseded chain A piece must not draw");
        assert_eq!(features[0]["properties"]["length_m"], 200.0);
    }

    /// Two-chain fixture plus OSM way 9001 (nodes 100→200→300) and the
    /// seg_match_way rows ways() reads — one run per chain, distinguished
    /// by cum_end_m.
    fn ways_fixture(conn: &Connection) {
        two_chain_fixture(conn);
        conn.execute(
            "INSERT INTO osm_way (osm_way_id, highway, name) VALUES (9001, 'secondary', 'Road')",
            [],
        )
        .unwrap();
        for (ord, node_id) in [(0, 100), (1, 200), (2, 300)] {
            conn.execute(
                "INSERT INTO osm_way_node (osm_way_id, ordinal, osm_node_id) VALUES (9001, ?1, ?2)",
                rusqlite::params![ord, node_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO osm_node (osm_node_id, lat, lon) VALUES (?1, 24.0, 120.0)",
                [node_id],
            )
            .unwrap();
        }
        for (sv_id, cum_end) in [(1, 100.0), (2, 200.0)] {
            conn.execute(
                "INSERT INTO seg_match_way (subroute_version_id, direction, ordinal, osm_way_id,
                                             entry_node_id, exit_node_id, cum_start_m, cum_end_m)
                 VALUES (?1, 0, 1, 9001, 100, 300, 0.0, ?2)",
                rusqlite::params![sv_id, cum_end],
            )
            .unwrap();
        }
    }

    #[test]
    fn ways_lists_only_the_current_chain() {
        let conn = rm_db::open_in_memory().unwrap();
        ways_fixture(&conn);

        let out = ways(&conn, 1, 0).unwrap();
        let features = out["features"].as_array().unwrap();
        assert_eq!(features.len(), 1, "superseded chain A run must not list");
        assert_eq!(features[0]["properties"]["cum_end_m"], 200.0);
    }
}
