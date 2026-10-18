//! Read-only queries behind the `find-stop` / `show-route` / `show-path`
//! commands. They open nothing and write nothing: the caller owns the
//! connection (`rm_db::open_readonly`) and the envelope. Unknown routes
//! surface as `Ok(None)` (dispatch rejects) — `find_stop` errors with
//! `EditError::UnknownRoute`, its return type has no room for None.

use anyhow::Result;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, ToSql};
use serde::Serialize;

use rm_db::{EditError, LineSnapshot};

/// One matching current pole: identity ids, its station's current name,
/// and the directions that reference it (first-seen = sequence order).
#[derive(Debug, Serialize)]
pub struct StopCandidate {
    pub stop_version_id: i64,
    pub stop_id: i64,
    pub stop_code: String,
    pub stop_name: String,
    pub station_id: i64,
    pub station_name: String,
    pub lat: f64,
    pub lon: f64,
    pub directions: Vec<i64>,
}

/// One direction's current stop sequence and effective-polyline source.
#[derive(Debug, Serialize)]
pub struct StopRow {
    pub sequence: i64,
    pub stop_code: String,
    pub stop_name: String,
    pub stop_version_id: i64,
    pub station_id: i64,
}

#[derive(Debug, Serialize)]
pub struct DirectionOverview {
    pub direction: i64,
    pub stops: Vec<StopRow>,
    /// `"override"`, `"src"` or `"none"` — the effective polyline's source.
    pub path: String,
}

/// `show-route`'s whole-route overview.
#[derive(Debug, Serialize)]
pub struct RouteOverview {
    pub route_id: i64,
    pub xno: String,
    pub directions: Vec<DirectionOverview>,
}

/// `show-path`'s effective polyline: the override when one exists, else the
/// latest-batch src capture, else nothing.
#[derive(Debug, Serialize)]
pub struct PathOverview {
    pub source: String,
    pub encoded: Option<String>,
    /// First and last point of the decoded polyline, [lat, lon] order.
    pub endpoints: Option<Vec<[f64; 2]>>,
    /// The current override's `updated_at` — the `set-line` concurrency
    /// token. `None` when the effective polyline is the src capture.
    pub updated_at: Option<String>,
}

/// Route identity: `route.ext_uid = 'tcg:' || xno` — the same resolution
/// `plan_add_stop` uses, no fallback. `None` when no route carries that
/// ext_uid or it has no current version.
fn resolve_route(conn: &Connection, xno: &str) -> Result<Option<(i64, i64)>> {
    let (route_id, route_version_id): (i64, Option<i64>) = match conn.query_row(
        "SELECT route_id, current_route_version_id FROM route WHERE ext_uid = ?1",
        [format!("tcg:{xno}")],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ) {
        Ok(v) => v,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    Ok(route_version_id.map(|rv| (route_id, rv)))
}

/// Current poles on one route matching the name substring / exact code —
/// both filters are ANDed; neither given returns every stop. An unknown
/// route is an error (the CLI rejects); a known route with no match is an
/// empty Vec.
pub fn find_stop(
    conn: &Connection,
    xno: &str,
    name: Option<&str>,
    code: Option<&str>,
) -> Result<Vec<StopCandidate>> {
    let Some((_, route_version_id)) = resolve_route(conn, xno)? else {
        return Err(EditError::UnknownRoute(xno.to_string()).into());
    };
    let mut sql = String::from(
        "SELECT rsv.stop_version_id, s.stop_id, s.stop_code, s.stop_name, \
                s.station_id, st.station_name, s.latitude, s.longitude, rsv.direction \
         FROM route_stop_version rsv \
         JOIN subroute_version sub ON sub.subroute_version_id = rsv.subroute_version_id \
         JOIN stop_version s ON s.stop_version_id = rsv.stop_version_id \
         JOIN station_version st ON st.station_id = s.station_id AND st.valid_to IS NULL \
         WHERE sub.route_version_id = ?1",
    );
    // placeholders are numbered ?1..?N in bind order (args.len() + 1), so
    // any filter combination binds correctly
    let mut args: Vec<Box<dyn ToSql>> = vec![Box::new(route_version_id)];
    if let Some(n) = name {
        sql.push_str(&format!(" AND s.stop_name LIKE ?{}", args.len() + 1));
        args.push(Box::new(format!("%{n}%")));
    }
    if let Some(c) = code {
        sql.push_str(&format!(" AND s.stop_code = ?{}", args.len() + 1));
        args.push(Box::new(c.to_string()));
    }
    sql.push_str(" ORDER BY rsv.stop_sequence, rsv.direction, rsv.stop_version_id");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params_from_iter(args.iter().map(|b| b.as_ref())), |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, f64>(6)?,
                r.get::<_, f64>(7)?,
                r.get::<_, i64>(8)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    // group the (sequence-ordered) rows per pole, keeping first-seen order
    let mut out: Vec<StopCandidate> = Vec::new();
    let mut index: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
    for (svi, stop_id, stop_code, stop_name, station_id, station_name, lat, lon, direction) in rows
    {
        match index.get(&svi) {
            Some(&i) => out[i].directions.push(direction),
            None => {
                index.insert(svi, out.len());
                out.push(StopCandidate {
                    stop_version_id: svi,
                    stop_id,
                    stop_code,
                    stop_name,
                    station_id,
                    station_name,
                    lat,
                    lon,
                    directions: vec![direction],
                });
            }
        }
    }
    Ok(out)
}

/// One route's current stops per direction plus each direction's path
/// state. `None` when no route carries that ext_uid.
pub fn show_route(
    conn: &Connection,
    xno: &str,
    logs: &mut Vec<String>,
) -> Result<Option<RouteOverview>> {
    let Some((route_id, route_version_id)) = resolve_route(conn, xno)? else {
        return Ok(None);
    };
    let mut stmt = conn.prepare(
        "SELECT DISTINCT rsv.direction \
         FROM route_stop_version rsv \
         JOIN subroute_version sub ON sub.subroute_version_id = rsv.subroute_version_id \
         WHERE sub.route_version_id = ?1 \
         ORDER BY rsv.direction",
    )?;
    let dirs: Vec<i64> = stmt
        .query_map([route_version_id], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    let mut directions = Vec::with_capacity(dirs.len());
    for direction in dirs {
        let stops = route_stops(conn, route_version_id, direction)?;
        let path = path_source(conn, xno, direction, logs)?;
        directions.push(DirectionOverview {
            direction,
            stops,
            path,
        });
    }
    Ok(Some(RouteOverview {
        route_id,
        xno: xno.to_string(),
        directions,
    }))
}

/// The direction's ordered current stops — rm-web `routes::stops()`'s join
/// minus the projection QA columns. Pinned to the resolved current
/// `route_version_id` (the find_stop pattern): applies mint a new chain
/// and keep the old rows, so filtering on route_id alone would merge
/// superseded chains into the sequence.
fn route_stops(conn: &Connection, route_version_id: i64, direction: i64) -> Result<Vec<StopRow>> {
    let mut stmt = conn.prepare(
        "SELECT rsv.stop_sequence, sv2.stop_code, sv2.stop_name, \
                sv2.stop_version_id, sv2.station_id \
         FROM route_stop_version rsv \
         JOIN subroute_version sv ON sv.subroute_version_id = rsv.subroute_version_id \
         JOIN stop_version sv2 ON sv2.stop_version_id = rsv.stop_version_id \
         WHERE sv.route_version_id = ?1 AND rsv.direction = ?2 \
         ORDER BY rsv.stop_sequence",
    )?;
    let stops = stmt
        .query_map(params![route_version_id, direction], |r| {
            Ok(StopRow {
                sequence: r.get(0)?,
                stop_code: r.get(1)?,
                stop_name: r.get(2)?,
                stop_version_id: r.get(3)?,
                station_id: r.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(stops)
}

/// `show-path`'s effective polyline for one direction. `None` when no route
/// carries that ext_uid.
pub fn show_path(
    conn: &Connection,
    xno: &str,
    direction: i64,
    logs: &mut Vec<String>,
) -> Result<Option<PathOverview>> {
    let Some(snap) = line_snapshot(conn, xno, direction, logs)? else {
        return Ok(None);
    };
    // read the source before the encoded String moves out of the snapshot
    let eff_source = source_name(&snap);
    let updated_at = snap.override_row.as_ref().map(|o| o.updated_at.clone());
    let encoded = snap
        .override_row
        .as_ref()
        .map(|o| o.encoded.clone())
        .or(snap.src_encoded);
    let (source, encoded, endpoints) = match encoded {
        Some(e) => {
            let pts = rm_core::polyline::decode(&e)?;
            match (pts.first(), pts.last()) {
                (Some(first), Some(last)) => (
                    eff_source,
                    Some(e),
                    Some(vec![[first.lat, first.lon], [last.lat, last.lon]]),
                ),
                // decodes to nothing (e.g. "") — no effective polyline
                _ => ("none".to_string(), None, None),
            }
        }
        None => ("none".to_string(), None, None),
    };
    Ok(Some(PathOverview {
        source,
        encoded,
        endpoints,
        updated_at,
    }))
}

fn source_name(snap: &LineSnapshot) -> String {
    if snap.override_row.is_some() {
        "override".to_string()
    } else {
        "src".to_string()
    }
}

/// Which table the effective polyline came from: override wins, then src,
/// else none — the three-way rule the brief pins for `show-route`'s path
/// state.
fn path_source(
    conn: &Connection,
    xno: &str,
    direction: i64,
    logs: &mut Vec<String>,
) -> Result<String> {
    let Some(snap) = line_snapshot(conn, xno, direction, logs)? else {
        return Ok("none".to_string());
    };
    Ok(if snap.override_row.is_some() {
        "override".to_string()
    } else if snap.src_encoded.is_some() {
        "src".to_string()
    } else {
        "none".to_string()
    })
}

/// `rm_db::load_polyline_line` with the old-DB degrade: `polyline_override`
/// exists only from migration 0013, so probe `sqlite_master` first — when
/// the table is missing the override is treated as absent (a `logs` entry
/// names the table, never an error — the /raw incident rule) and the src
/// half is loaded with the same queries `load_polyline_line` runs.
pub(crate) fn line_snapshot(
    conn: &Connection,
    xno: &str,
    direction: i64,
    logs: &mut Vec<String>,
) -> Result<Option<LineSnapshot>> {
    let have_override: bool = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'polyline_override'",
        [],
        |r| r.get::<_, i64>(0),
    )? > 0;
    if !have_override {
        const DEGRADE: &str =
            "polyline_override table missing — override treated as absent (pre-0013 db)";
        if !logs.iter().any(|l| l == DEGRADE) {
            logs.push(DEGRADE.to_string());
        }
        let (route_id, route_version_id): (i64, Option<i64>) = match conn.query_row(
            "SELECT route_id, current_route_version_id FROM route WHERE ext_uid = ?1",
            [format!("tcg:{xno}")],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ) {
            Ok(v) => v,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let batch_id: i64 = conn.query_row(
            "SELECT COALESCE(MAX(batch_id), 0) FROM import_batch",
            [],
            |r| r.get(0),
        )?;
        let src_encoded: Option<String> = conn
            .query_row(
                "SELECT encoded FROM src_route_point \
                 WHERE batch_id = ?1 AND xno = ?2 AND direction = ?3",
                params![batch_id, xno, direction],
                |r| r.get(0),
            )
            .optional()?;
        return Ok(Some(LineSnapshot {
            route_id,
            src_encoded,
            override_row: None,
            route_version_id,
        }));
    }
    rm_db::load_polyline_line(conn, xno, direction)
}
