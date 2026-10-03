//! Stop/station/group drill-down queries.

use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Value};

fn serving_routes_where(conn: &Connection, where_sql: &str, params: &[&dyn rusqlite::ToSql]) -> Result<Vec<Value>> {
    let sql = format!(
        "SELECT DISTINCT r.route_id, r.route_code, sv.subroute_name, sorv.direction
         FROM stop_of_route_version sorv
         JOIN subroute_version sv ON sv.subroute_version_id = sorv.subroute_version_id
         JOIN route_version rv ON rv.route_version_id = sv.route_version_id
         JOIN route r ON r.route_id = rv.route_id
         WHERE {where_sql}
         ORDER BY r.route_id, sorv.direction"
    );
    let mut stmt = conn.prepare(&sql)?;
    let it = stmt.query_map(params, |r| {
        Ok(json!({
            "route_id": r.get::<_, i64>(0)?,
            "route_code": r.get::<_, String>(1)?,
            "subroute_name": r.get::<_, Option<String>>(2)?,
            "direction": r.get::<_, i64>(3)?,
        }))
    })?;
    Ok(it.collect::<Result<_, _>>()?)
}

/// `GET /api/stops/{stop_version_id}` — stop → station → group + serving routes.
pub fn detail(conn: &Connection, stop_version_id: i64) -> Result<Option<Value>> {
    let found = match conn.query_row(
        "SELECT sv.stop_version_id, sv.stop_code, sv.stop_name, sv.latitude, sv.longitude,
                st.station_id, stv.station_name, stv.latitude, stv.longitude,
                sg.station_group_id, sgv.station_group_name, sgv.source
         FROM stop_version sv
         JOIN station st ON st.station_id = sv.station_id
         JOIN station_version stv
           ON stv.station_id = st.station_id AND stv.valid_to IS NULL
         JOIN station_group sg ON sg.station_group_id = stv.station_group_id
         JOIN station_group_version sgv
           ON sgv.station_group_id = sg.station_group_id AND sgv.valid_to IS NULL
         WHERE sv.stop_version_id = ?1",
        [stop_version_id],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, f64>(3)?,
                r.get::<_, f64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, f64>(7)?,
                r.get::<_, f64>(8)?,
                r.get::<_, i64>(9)?,
                r.get::<_, String>(10)?,
                r.get::<_, String>(11)?,
            ))
        },
    ) {
        Ok(row) => row,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(e) => return Err(e.into()),
    };

    let (_, _, _, _, _, station_id, _, _, _, group_id, _, _) = &found;

    let members: Vec<Value> = {
        let mut stmt = conn.prepare(
            "SELECT st.station_id, stv.station_name, stv.latitude, stv.longitude
             FROM station_version stv
             JOIN station st ON st.station_id = stv.station_id
             WHERE stv.station_group_id = ?1 AND stv.valid_to IS NULL",
        )?;
        let it = stmt.query_map([group_id], |r| {
            Ok(json!({
                "station_id": r.get::<_, i64>(0)?,
                "station_name": r.get::<_, String>(1)?,
                "lat": r.get::<_, f64>(2)?,
                "lon": r.get::<_, f64>(3)?,
            }))
        })?;
        it.collect::<Result<_, _>>()?
    };

    let routes_by_stop = serving_routes_where(
        conn,
        "sorv.stop_version_id = ?1",
        &[&stop_version_id],
    )?;
    let routes_by_station = serving_routes_where(
        conn,
        "sorv.stop_version_id IN (SELECT stop_version_id FROM stop_version
                                   WHERE station_id = ?1 AND valid_to IS NULL)",
        &[station_id],
    )?;
    let routes_by_group = serving_routes_where(
        conn,
        "sorv.stop_version_id IN (SELECT stop_version_id FROM stop_version
           WHERE station_id IN (SELECT station_id FROM station_version
                                WHERE station_group_id = ?1 AND valid_to IS NULL)
           AND valid_to IS NULL)",
        &[group_id],
    )?;

    Ok(Some(json!({
        "stop": {
            "stop_version_id": found.0,
            "stop_code": found.1,
            "stop_name": found.2,
            "lat": found.3,
            "lon": found.4,
        },
        "station": {
            "station_id": found.5,
            "station_name": found.6,
            "lat": found.7,
            "lon": found.8,
        },
        "group": {
            "station_group_id": found.9,
            "station_group_name": found.10,
            "source": found.11,
        },
        "group_members": members,
        "routes_by_stop": routes_by_stop,
        "routes_by_station": routes_by_station,
        "routes_by_group": routes_by_group,
    })))
}

/// `GET /api/stations/{station_id}` — station + group + stops + serving routes.
pub fn station_detail(conn: &Connection, station_id: i64) -> Result<Option<Value>> {
    let found = match conn.query_row(
        "SELECT st.station_id, stv.station_name, stv.latitude, stv.longitude,
                sg.station_group_id, sgv.station_group_name, sgv.source
         FROM station st
         JOIN station_version stv
           ON stv.station_id = st.station_id AND stv.valid_to IS NULL
         JOIN station_group sg ON sg.station_group_id = stv.station_group_id
         JOIN station_group_version sgv
           ON sgv.station_group_id = sg.station_group_id AND sgv.valid_to IS NULL
         WHERE st.station_id = ?1",
        [station_id],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, f64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
            ))
        },
    ) {
        Ok(row) => row,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(e) => return Err(e.into()),
    };

    let mut stmt = conn.prepare(
        "SELECT stop_version_id, stop_code, stop_name, direction, latitude, longitude
         FROM stop_version WHERE station_id = ?1 AND valid_to IS NULL",
    )?;
    let stops: Vec<Value> = stmt
        .query_map([station_id], |r| {
            Ok(json!({
                "stop_version_id": r.get::<_, i64>(0)?,
                "stop_code": r.get::<_, String>(1)?,
                "stop_name": r.get::<_, String>(2)?,
                "direction": r.get::<_, i64>(3)?,
                "lat": r.get::<_, f64>(4)?,
                "lon": r.get::<_, f64>(5)?,
            }))
        })?
        .collect::<Result<_, _>>()?;

    let routes = serving_routes_where(
        conn,
        "sorv.stop_version_id IN (SELECT stop_version_id FROM stop_version
                                   WHERE station_id = ?1 AND valid_to IS NULL)",
        &[&station_id],
    )?;

    Ok(Some(json!({
        "station": {
            "station_id": found.0,
            "station_name": found.1,
            "lat": found.2,
            "lon": found.3,
        },
        "group": {
            "station_group_id": found.4,
            "station_group_name": found.5,
            "source": found.6,
        },
        "stops": stops,
        "routes": routes,
    })))
}
