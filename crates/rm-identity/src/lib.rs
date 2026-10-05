//! rm-identity: staging -> identity tables + first versions.

mod grouping;

use anyhow::Result;
use rusqlite::Connection;

pub use grouping::{group_stations, ClusterInput};

#[derive(Debug, Default, PartialEq)]
pub struct Summary {
    pub batch_id: i64,
    pub routes: usize,
    pub subroutes: usize,
    pub stations: usize,
    pub station_groups: usize,
    pub stops: usize,
    pub route_links: usize,
    pub route_providers: usize,
    pub warnings: Vec<String>,
}

/// Mint identities + V1 versions for `batch` (idempotent via ext_uid lookup).
pub fn run(conn: &Connection, batch_id: i64) -> Result<Summary> {
    use std::collections::HashMap;

    use rusqlite::OptionalExtension;

    let valid_from: String = conn.query_row(
        "SELECT valid_from FROM import_batch WHERE batch_id = ?1",
        [batch_id],
        |r| r.get(0),
    )?;
    let now = rm_db::now_rfc3339();
    let mut warnings: Vec<String> = Vec::new();

    // ---- load staging (Rust-side numeric ordering) ----

    let mut headers: Vec<(String, String)> = Vec::new(); // (xno, route_code)
    {
        let mut stmt =
            conn.prepare("SELECT xno, route_code FROM src_route_header WHERE batch_id = ?1")?;
        let rows = stmt.query_map([batch_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            headers.push(row?);
        }
    }
    headers.sort_by_key(|h| xno_key(&h.0));

    // provider name directory
    let mut provider_names: HashMap<String, String> = HashMap::new();
    {
        let mut stmt =
            conn.prepare("SELECT provider_code, name FROM src_provider WHERE batch_id = ?1")?;
        let rows = stmt.query_map([batch_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (code, name) = row?;
            provider_names.insert(code, name);
        }
    }

    // route -> provider codes
    let mut route_providers: Vec<(String, String)> = Vec::new();
    {
        let mut stmt =
            conn.prepare("SELECT xno, provider_code FROM src_route_provider WHERE batch_id = ?1")?;
        let rows = stmt.query_map([batch_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            route_providers.push(row?);
        }
    }
    route_providers.sort();

    // main-side pairs
    let mut main_side: Vec<(String, String)> = Vec::new();
    {
        let mut stmt =
            conn.prepare("SELECT main_xno, side_xno FROM src_main_side_route WHERE batch_id = ?1")?;
        let rows = stmt.query_map([batch_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            main_side.push(row?);
        }
    }
    main_side.sort();

    // stop edges, numeric xno then direction then source order
    struct Edge {
        xno: String,
        direction: i64,
        order_no: i64,
        sid: i64,
        node_id: String,
        name: String,
        lat: f64,
        lon: f64,
    }
    let mut edges: Vec<Edge> = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT xno, direction, source_order_no, sid, node_id, station_name, lat, lon
             FROM src_stop_edge WHERE batch_id = ?1",
        )?;
        let rows = stmt.query_map([batch_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, f64>(6)?,
                r.get::<_, f64>(7)?,
            ))
        })?;
        for row in rows {
            let (xno, direction, order_no, sid, node_id, name, lat, lon) = row?;
            edges.push(Edge {
                xno,
                direction,
                order_no,
                sid,
                node_id,
                name,
                lat,
                lon,
            });
        }
    }
    edges.sort_by(|a, b| {
        xno_key(&a.xno)
            .cmp(&xno_key(&b.xno))
            .then(a.direction.cmp(&b.direction))
            .then(a.order_no.cmp(&b.order_no))
            .then(a.sid.cmp(&b.sid))
    });

    // first occurrence wins: station attrs per node, stop per (xno, dir, sid)
    let mut stations: Vec<(String, String, f64, f64)> = Vec::new(); // (node_id, name, lat, lon)
    let mut station_index: HashMap<String, usize> = HashMap::new();
    // per-stop (pole) attrs come from the edge itself, not the station —
    // poles across a dual carriageway share a station but differ in coords
    struct StopRec {
        xno: String,
        direction: i64,
        sid: i64,
        node_id: String,
        name: String,
        lat: f64,
        lon: f64,
    }
    let mut stops: Vec<StopRec> = Vec::new();
    let mut stop_index: HashMap<(String, i64, i64), usize> = HashMap::new();
    for e in &edges {
        if !station_index.contains_key(&e.node_id) {
            station_index.insert(e.node_id.clone(), stations.len());
            stations.push((e.node_id.clone(), e.name.clone(), e.lat, e.lon));
        }
        let key = (e.xno.clone(), e.direction, e.sid);
        match stop_index.get(&key) {
            Some(&i) => {
                if stops[i].node_id != e.node_id {
                    warnings.push(format!(
                        "ambiguous stop: xno={} dir={} sid={} nodes {} and {}; first wins",
                        e.xno, e.direction, e.sid, stops[i].node_id, e.node_id
                    ));
                }
            }
            None => {
                stop_index.insert(key, stops.len());
                stops.push(StopRec {
                    xno: e.xno.clone(),
                    direction: e.direction,
                    sid: e.sid,
                    node_id: e.node_id.clone(),
                    name: e.name.clone(),
                    lat: e.lat,
                    lon: e.lon,
                });
            }
        }
    }
    warnings.sort();
    warnings.dedup();

    // station order for minting: numeric node_id (matches data/stations.json)
    let mut station_order: Vec<usize> = (0..stations.len()).collect();
    station_order.sort_by_key(|&i| stations[i].0.parse::<i64>().unwrap_or(i64::MAX));

    // clustering on the same order
    let cluster_inputs: Vec<grouping::ClusterInput> = station_order
        .iter()
        .map(|&i| grouping::ClusterInput {
            ext_uid: station_ext_uid(&stations[i].0),
            name: stations[i].1.clone(),
            lat: stations[i].2,
            lon: stations[i].3,
        })
        .collect();
    let clusters = grouping::group_stations(&cluster_inputs);

    // ---- write (single tx, ext_uid lookup before every mint) ----

    let tx = conn.unchecked_transaction()?;

    let lookup = |sql: &str, uid: &str| -> Result<Option<i64>> {
        Ok(tx.query_row(sql, [uid], |r| r.get(0)).optional()?)
    };
    let mut route_ids: HashMap<String, i64> = HashMap::new();
    for (xno, code) in &headers {
        let uid = route_ext_uid(xno);
        let id = match lookup("SELECT route_id FROM route WHERE ext_uid = ?1", &uid)? {
            Some(id) => id,
            None => {
                let id = rm_db::mint(&tx, "route")?;
                tx.execute(
                    "INSERT INTO route (route_id, route_code, ext_uid, revision, created_at, updated_at)
                     VALUES (?1, ?2, ?3, 0, ?4, ?4)",
                    rusqlite::params![id, code, uid, now],
                )?;
                let sub_id = rm_db::mint(&tx, "subroute")?;
                tx.execute(
                    "INSERT INTO subroute (subroute_id, route_id, subroute_code, created_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![sub_id, id, code, now],
                )?;
                id
            }
        };
        route_ids.insert(xno.clone(), id);
    }

    // main-side links (only between routes that exist)
    let mut route_links = 0usize;
    for (main, side) in &main_side {
        if let (Some(&m), Some(&s)) = (route_ids.get(main), route_ids.get(side)) {
            tx.execute(
                "INSERT OR IGNORE INTO route_link (main_route_id, side_route_id) VALUES (?1, ?2)",
                rusqlite::params![m, s],
            )?;
            route_links += 1;
        }
    }

    // route -> provider
    let mut route_provider_count = 0usize;
    for (xno, code) in &route_providers {
        if let Some(&rid) = route_ids.get(xno) {
            let name = provider_names
                .get(code)
                .cloned()
                .unwrap_or_else(|| code.clone());
            tx.execute(
                "INSERT OR IGNORE INTO route_provider (route_id, provider_code, provider_name)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![rid, code, name],
            )?;
            route_provider_count += 1;
        }
    }

    // station groups (cluster order is deterministic)
    let mut group_of_station: HashMap<String, i64> = HashMap::new(); // node ext_uid -> group_id
    let mut group_ids: Vec<i64> = Vec::new();
    for cluster in &clusters {
        let members: Vec<String> = cluster
            .iter()
            .map(|&i| cluster_inputs[i].ext_uid.clone())
            .collect();
        let uid = group_ext_uid(&members);
        let id = match lookup(
            "SELECT station_group_id FROM station_group WHERE ext_uid = ?1",
            &uid,
        )? {
            Some(id) => id,
            None => {
                let id = rm_db::mint(&tx, "station_group")?;
                tx.execute(
                    "INSERT INTO station_group (station_group_id, ext_uid, created_at) VALUES (?1, ?2, ?3)",
                    rusqlite::params![id, uid, now],
                )?;
                let name = cluster_inputs[cluster[0]].name.clone();
                let source = if cluster.len() > 1 {
                    "derived:same-name<80m"
                } else {
                    "singleton"
                };
                tx.execute(
                    "INSERT INTO station_group_version
                        (station_group_version_id, station_group_id, station_group_name, valid_from, created_at, source)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    rusqlite::params![
                        rm_db::mint(&tx, "station_group_version")?, id, name, valid_from, now, source
                    ],
                )?;
                id
            }
        };
        for m in &members {
            group_of_station.insert(m.clone(), id);
        }
        group_ids.push(id);
    }

    // stations + station_version V1
    let mut station_ids: HashMap<String, i64> = HashMap::new(); // node_id -> station_id
    for &si in &station_order {
        let (node_id, name, lat, lon) = &stations[si];
        let uid = station_ext_uid(node_id);
        let id = match lookup("SELECT station_id FROM station WHERE ext_uid = ?1", &uid)? {
            Some(id) => id,
            None => {
                let id = rm_db::mint(&tx, "station")?;
                tx.execute(
                    "INSERT INTO station (station_id, ext_uid, created_at) VALUES (?1, ?2, ?3)",
                    rusqlite::params![id, uid, now],
                )?;
                id
            }
        };
        let has_version: bool = tx
            .query_row(
                "SELECT 1 FROM station_version WHERE station_id = ?1 LIMIT 1",
                [id],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if !has_version {
            let group_id = group_of_station[&uid];
            tx.execute(
                "INSERT INTO station_version
                    (station_version_id, station_id, station_group_id, station_name, latitude, longitude, valid_from, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    rm_db::mint(&tx, "station_version")?, id, group_id, name, lat, lon, valid_from, now
                ],
            )?;
        }
        station_ids.insert(node_id.clone(), id);
    }

    // stops + stop_version V1
    for s in &stops {
        let uid = stop_ext_uid(&s.xno, s.direction, s.sid);
        let id = match lookup("SELECT stop_id FROM stop WHERE ext_uid = ?1", &uid)? {
            Some(id) => id,
            None => {
                let id = rm_db::mint(&tx, "stop")?;
                tx.execute(
                    "INSERT INTO stop (stop_id, ext_uid, created_at) VALUES (?1, ?2, ?3)",
                    rusqlite::params![id, uid, now],
                )?;
                id
            }
        };
        let has_version: bool = tx
            .query_row(
                "SELECT 1 FROM stop_version WHERE stop_id = ?1 LIMIT 1",
                [id],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if !has_version {
            let station_id = station_ids[&s.node_id];
            tx.execute(
                "INSERT INTO stop_version
                    (stop_version_id, stop_id, station_id, stop_code, stop_name, direction, latitude, longitude, valid_from, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                rusqlite::params![
                    rm_db::mint(&tx, "stop_version")?,
                    id,
                    station_id,
                    s.sid.to_string(),
                    s.name,
                    s.direction,
                    s.lat,
                    s.lon,
                    valid_from,
                    now
                ],
            )?;
        }
    }

    tx.commit()?;

    Ok(Summary {
        batch_id,
        routes: headers.len(),
        subroutes: headers.len(),
        stations: stations.len(),
        station_groups: clusters.len(),
        stops: stops.len(),
        route_links,
        route_providers: route_provider_count,
        warnings,
    })
}

/// ext_uid for a route: tcg:{xno}
pub fn route_ext_uid(xno: &str) -> String {
    format!("tcg:{xno}")
}

/// ext_uid for a station: tcg:node:{node.id}
pub fn station_ext_uid(node_id: &str) -> String {
    format!("tcg:node:{node_id}")
}

/// ext_uid for a stop occurrence: tcg:{xno}:{direction}:{sid}
pub fn stop_ext_uid(xno: &str, direction: i64, sid: i64) -> String {
    format!("tcg:{xno}:{direction}:{sid}")
}

/// ext_uid for a derived station group: blake3 over sorted member station
/// ext_uids (deterministic: same membership -> same id).
pub fn group_ext_uid(member_station_ext_uids: &[String]) -> String {
    let mut members = member_station_ext_uids.to_vec();
    members.sort();
    format!(
        "tcg:grp:{}",
        rm_core::blake3_128_hex(members.join("|").as_bytes())
    )
}

/// Numeric-aware xno ordering ("2" < "10").
pub fn xno_key(xno: &str) -> (u8, i64, String) {
    match xno.parse::<i64>() {
        Ok(n) => (0, n, xno.to_string()),
        Err(_) => (1, i64::MAX, xno.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ext_uids_are_distinct_per_entity_kind() {
        assert_eq!(route_ext_uid("3"), "tcg:3");
        assert_eq!(station_ext_uid("44"), "tcg:node:44");
        assert_eq!(stop_ext_uid("3", 0, 13708), "tcg:3:0:13708");
        assert_ne!(stop_ext_uid("3", 0, 1), stop_ext_uid("3", 1, 1));
        // group id: order-insensitive, membership-sensitive
        assert_eq!(
            group_ext_uid(&["tcg:node:44".into(), "tcg:node:46".into()]),
            group_ext_uid(&["tcg:node:46".into(), "tcg:node:44".into()])
        );
        assert_ne!(
            group_ext_uid(&["tcg:node:44".into()]),
            group_ext_uid(&["tcg:node:46".into()])
        );
    }

    #[test]
    fn xno_sorts_numerically() {
        let mut v = ["10", "2", "9221", "300"];
        v.sort_by_key(|x| xno_key(x));
        assert_eq!(v, ["2", "10", "300", "9221"]);
    }
}
