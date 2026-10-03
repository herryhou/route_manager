//! Route list/detail queries.

use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Value};

fn split_list(s: Option<String>) -> Vec<String> {
    s.map(|v| v.split(',').map(str::to_string).collect())
        .unwrap_or_default()
}

fn direction_rows(conn: &Connection, route_ids: &[i64]) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    if route_ids.is_empty() {
        return Ok(out);
    }
    let placeholders = vec!["?"; route_ids.len()].join(",");
    let sql = format!(
        "SELECT r.route_id, sorv.direction, COUNT(*) AS stops,
                ms.matched_length_m, ms.path_length_m, ms.snap_dist_p50, ms.snap_dist_p95,
                ms.outliers
         FROM stop_of_route_version sorv
         JOIN subroute_version sv ON sv.subroute_version_id = sorv.subroute_version_id
         JOIN route_version rv ON rv.route_version_id = sv.route_version_id
         JOIN route r ON r.route_id = rv.route_id
         LEFT JOIN seg_match_stat ms
           ON ms.route_version_id = rv.route_version_id AND ms.direction = sorv.direction
         WHERE r.route_id IN ({placeholders})
         GROUP BY r.route_id, sorv.direction
         ORDER BY r.route_id, sorv.direction"
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
        out.push(json!({
            "route_id": route_id,
            "direction": direction,
            "stops": stops,
            "matched_ratio": ratio,
            "snap_dist_p50_m": p50,
            "snap_dist_p95_m": p95,
            "outliers": outliers,
        }));
    }
    Ok(out)
}

/// Fold the flat direction rows into per-route `directions` arrays.
fn directions_by_route(rows: Vec<Value>) -> std::collections::HashMap<i64, Vec<Value>> {
    let mut map = std::collections::HashMap::new();
    for d in rows {
        map.entry(d["route_id"].as_i64().unwrap())
            .or_insert_with(Vec::new)
            .push(d);
    }
    map
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
    let routes: Vec<Value> = routes
        .into_iter()
        .map(|(route_id, route_code, ext_uid, name, providers, side, main)| {
            json!({
                "route_id": route_id,
                "route_code": route_code,
                "ext_uid": ext_uid,
                "subroute_name": name,
                "providers": providers,
                "side_routes": side,
                "main_routes": main,
                "directions": dirs.get(&route_id).cloned().unwrap_or_default(),
            })
        })
        .collect();
    Ok(json!({ "routes": routes }))
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
    let subroutes: Vec<Value> = stmt
        .query_map([rv_id], |r| {
            Ok(json!({
                "subroute_version_id": r.get::<_, i64>(0)?,
                "subroute_name": r.get::<_, String>(1)?,
            }))
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

    Ok(Some(json!({
        "route_id": id,
        "route_code": route_code,
        "ext_uid": ext_uid,
        "xno": ext_uid.strip_prefix("tcg:").unwrap_or(&ext_uid),
        "revision": revision,
        "version_no": version_no,
        "valid_from": valid_from,
        "status": status,
        "subroute_name": subroutes.first().and_then(|s| s["subroute_name"].as_str()),
        "subroutes": subroutes,
        "providers": providers,
        "side_routes": side,
        "main_routes": main,
        "directions": dirs,
    })))
}
