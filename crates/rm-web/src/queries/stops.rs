//! Stop/station/group drill-down queries.

use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Value};

fn serving_routes_where(
    conn: &Connection,
    where_sql: &str,
    params: &[&dyn rusqlite::ToSql],
) -> Result<Vec<Value>> {
    let sql = format!(
        "SELECT DISTINCT r.route_id, r.route_code, sv.subroute_name, rsv.direction,
                sb.bearing_deg
         FROM route_stop_version rsv
         JOIN subroute_version sv ON sv.subroute_version_id = rsv.subroute_version_id
         JOIN route_version rv ON rv.route_version_id = sv.route_version_id
         JOIN route r ON r.route_id = rv.route_id
         LEFT JOIN stop_bearing sb ON sb.stop_version_id = rsv.stop_version_id
         WHERE {where_sql}
         ORDER BY r.route_id, rsv.direction"
    );
    let mut stmt = conn.prepare(&sql)?;
    let it = stmt.query_map(params, |r| {
        Ok(json!({
            "route_id": r.get::<_, i64>(0)?,
            "route_code": r.get::<_, String>(1)?,
            "subroute_name": r.get::<_, Option<String>>(2)?,
            "direction": r.get::<_, i64>(3)?,
            // member pole's heading — lets the client disambiguate which of a
            // route's two directions serves the clicked side of the road
            "bearing": r.get::<_, Option<f64>>(4)?,
        }))
    })?;
    Ok(it.collect::<Result<_, _>>()?)
}

/// Member stations of a group (current versions), self included — callers
/// filter the station they're showing to get the "other stations" chips.
fn group_members(conn: &Connection, group_id: i64) -> Result<Vec<Value>> {
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
    });
    let rows: Vec<Value> = it?.collect::<Result<_, _>>()?;
    Ok(rows)
}

/// `GET /api/stops/{stop_version_id}` — stop → station → group + serving routes.
pub fn detail(conn: &Connection, stop_version_id: i64) -> Result<Option<Value>> {
    let found = match conn.query_row(
        "SELECT sv.stop_version_id, sv.stop_code, sv.stop_name, sv.latitude, sv.longitude,
                st.station_id, stv.station_name, stv.latitude, stv.longitude,
                sg.station_group_id, sgv.station_group_name, sgv.source,
                sb.bearing_deg, sb.source,
                stb.bearing_deg, stb.max_spread_deg, stb.member_count, sv.stop_id
         FROM stop_version sv
         JOIN station st ON st.station_id = sv.station_id
         JOIN station_version stv
           ON stv.station_id = st.station_id AND stv.valid_to IS NULL
         JOIN station_group sg ON sg.station_group_id = stv.station_group_id
         JOIN station_group_version sgv
           ON sgv.station_group_id = sg.station_group_id AND sgv.valid_to IS NULL
         LEFT JOIN stop_bearing sb ON sb.stop_version_id = sv.stop_version_id
         LEFT JOIN station_bearing stb ON stb.station_id = st.station_id
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
                r.get::<_, Option<f64>>(12)?,
                r.get::<_, Option<String>>(13)?,
                r.get::<_, Option<f64>>(14)?,
                r.get::<_, Option<f64>>(15)?,
                r.get::<_, Option<i64>>(16)?,
                r.get::<_, i64>(17)?,
            ))
        },
    ) {
        Ok(row) => row,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(e) => return Err(e.into()),
    };

    let (_, _, _, _, _, station_id, _, _, _, group_id, _, _, _, _, _, _, _, _) = &found;

    let members = group_members(conn, *group_id)?;

    // staleness badges — position edits mark the projection routes + bearing
    // tables stale until the refresh pipeline re-derives them. route_stop_version
    // already points at the new version id, so the handler's id is the right key.
    let stale_projections = rm_db::stale::projection_routes_for_stop(conn, stop_version_id)?;
    let bearings_stale = rm_db::stale::bearings_stale(conn)?;

    let routes_by_stop =
        serving_routes_where(conn, "rsv.stop_version_id = ?1", &[&stop_version_id])?;
    let routes_by_station = serving_routes_where(
        conn,
        "rsv.stop_version_id IN (SELECT stop_version_id FROM stop_version
                                   WHERE station_id = ?1 AND valid_to IS NULL)",
        &[station_id],
    )?;
    let routes_by_group = serving_routes_where(
        conn,
        "rsv.stop_version_id IN (SELECT stop_version_id FROM stop_version
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
            "stop_id": found.17,
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
        "bearing": match found.12 {
            Some(bearing_deg) => json!({
                "bearing_deg": bearing_deg,
                "source": found.13,
            }),
            None => Value::Null,
        },
        "station_bearing": match found.14 {
            Some(bearing_deg) => json!({
                "bearing_deg": bearing_deg,
                "max_spread_deg": found.15,
                "member_count": found.16,
            }),
            None => Value::Null,
        },
        "group_members": members,
        "routes_by_stop": routes_by_stop,
        "routes_by_station": routes_by_station,
        "routes_by_group": routes_by_group,
        "stale": {
            "projections": stale_projections,
            "bearings": bearings_stale,
        },
    })))
}

/// `GET /api/stations/{station_id}` — station + group + stops + serving routes.
pub fn station_detail(conn: &Connection, station_id: i64) -> Result<Option<Value>> {
    let found = match conn.query_row(
        "SELECT st.station_id, stv.station_name, stv.latitude, stv.longitude,
                sg.station_group_id, sgv.station_group_name, sgv.source,
                stb.bearing_deg, stb.max_spread_deg, stb.member_count,
                stv.station_version_id
         FROM station st
         JOIN station_version stv
           ON stv.station_id = st.station_id AND stv.valid_to IS NULL
         JOIN station_group sg ON sg.station_group_id = stv.station_group_id
         JOIN station_group_version sgv
           ON sgv.station_group_id = sg.station_group_id AND sgv.valid_to IS NULL
         LEFT JOIN station_bearing stb ON stb.station_id = st.station_id
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
                r.get::<_, Option<f64>>(7)?,
                r.get::<_, Option<f64>>(8)?,
                r.get::<_, Option<i64>>(9)?,
                r.get::<_, i64>(10)?,
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
        "rsv.stop_version_id IN (SELECT stop_version_id FROM stop_version
                                   WHERE station_id = ?1 AND valid_to IS NULL)",
        &[&station_id],
    )?;

    // stations have no projections — only the bearing tables go stale
    let bearings_stale = rm_db::stale::bearings_stale(conn)?;

    Ok(Some(json!({
        "station": {
            "station_id": found.0,
            "station_name": found.1,
            "lat": found.2,
            "lon": found.3,
            "station_version_id": found.10,
        },
        "group": {
            "station_group_id": found.4,
            "station_group_name": found.5,
            "source": found.6,
        },
        "bearing": match found.7 {
            Some(bearing_deg) => json!({
                "bearing_deg": bearing_deg,
                "max_spread_deg": found.8,
                "member_count": found.9,
            }),
            None => Value::Null,
        },
        "stops": stops,
        "routes": routes,
        "group_members": group_members(conn, found.4)?,
        "stale": {
            "bearings": bearings_stale,
        },
    })))
}

/// `GET /api/stops?bbox=minLon,minLat,maxLon,maxLat&kind=stop|station`
/// Viewport layer — bbox WHERE over current versions, LIMIT with truncation.
pub fn viewport(
    conn: &Connection,
    bbox: (f64, f64, f64, f64),
    kind: &str,
    limit: i64,
) -> Result<Value> {
    let (min_lon, min_lat, max_lon, max_lat) = bbox;
    // kind-specific table + bearing join: stops get their own bearing + source,
    // stations get the circular-mean bearing + member spread.
    let (table, id_col, name_col, join, extra) = if kind == "station" {
        (
            "station_version",
            "station_version.station_id",
            "station_name",
            "LEFT JOIN station_bearing stb ON stb.station_id = station_version.station_id",
            "stb.bearing_deg, stb.max_spread_deg",
        )
    } else {
        (
            "stop_version",
            "stop_version.stop_version_id",
            "stop_name",
            "LEFT JOIN stop_bearing sb ON sb.stop_version_id = stop_version.stop_version_id",
            "sb.bearing_deg, sb.source",
        )
    };
    let total: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM {table}
             WHERE valid_to IS NULL
               AND longitude BETWEEN ?1 AND ?3
               AND latitude BETWEEN ?2 AND ?4"
        ),
        rusqlite::params![min_lon, min_lat, max_lon, max_lat],
        |r| r.get(0),
    )?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {id_col}, {name_col}, longitude, latitude, {extra}
         FROM {table}
         {join}
         WHERE valid_to IS NULL
           AND longitude BETWEEN ?1 AND ?3
           AND latitude BETWEEN ?2 AND ?4
         LIMIT ?5"
    ))?;
    let features: Vec<Value> = {
        let it = stmt.query_map(
            rusqlite::params![min_lon, min_lat, max_lon, max_lat, limit],
            |r| {
                let bearing = r.get::<_, Option<f64>>(4)?;
                let mut props = json!({
                    "id": r.get::<_, i64>(0)?,
                    "name": r.get::<_, String>(1)?,
                    "kind": kind,
                    "bearing": bearing,
                });
                if kind == "station" {
                    props["spread"] = r.get::<_, Option<f64>>(5)?.into();
                } else {
                    props["bearing_source"] = r.get::<_, Option<String>>(5)?.into();
                }
                Ok(json!({
                    "type": "Feature",
                    "geometry": crate::geojson::point(r.get::<_, f64>(2)?, r.get::<_, f64>(3)?),
                    "properties": props,
                }))
            },
        )?;
        it.collect::<Result<_, _>>()?
    };
    Ok(json!({
        "type": "FeatureCollection",
        "features": features,
        "truncated": total > limit,
    }))
}

/// `GET /api/stops?q=` — name/code prefix or substring search, LIMIT 30.
pub fn search(conn: &Connection, q: &str) -> Result<Value> {
    let like = format!("%{q}%");
    let mut stmt = conn.prepare(
        "SELECT stop_version_id, stop_code, stop_name, latitude, longitude
         FROM stop_version
         WHERE valid_to IS NULL AND (stop_name LIKE ?1 OR stop_code LIKE ?1)
         ORDER BY stop_code
         LIMIT 30",
    )?;
    let stops: Vec<Value> = stmt
        .query_map([like], |r| {
            Ok(json!({
                "stop_version_id": r.get::<_, i64>(0)?,
                "stop_code": r.get::<_, String>(1)?,
                "stop_name": r.get::<_, String>(2)?,
                "lat": r.get::<_, f64>(3)?,
                "lon": r.get::<_, f64>(4)?,
            }))
        })?
        .collect::<Result<_, _>>()?;
    Ok(json!({ "stops": stops }))
}

/// `GET /api/stations?q=` — station name substring search, LIMIT 30.
/// 臺/台 normalized so either spelling matches both.
pub fn station_search(conn: &Connection, q: &str) -> Result<Value> {
    let like = format!("%{}%", q.replace('臺', "台"));
    let mut stmt = conn.prepare(
        "SELECT st.station_id, stv.station_name, stv.latitude, stv.longitude
         FROM station st
         JOIN station_version stv
           ON stv.station_id = st.station_id AND stv.valid_to IS NULL
         WHERE stv.station_name LIKE ?1
            OR REPLACE(stv.station_name, '臺', '台') LIKE ?1
         ORDER BY stv.station_name
         LIMIT 30",
    )?;
    let stations: Vec<Value> = stmt
        .query_map([like], |r| {
            Ok(json!({
                "station_id": r.get::<_, i64>(0)?,
                "station_name": r.get::<_, String>(1)?,
                "lat": r.get::<_, f64>(2)?,
                "lon": r.get::<_, f64>(3)?,
            }))
        })?
        .collect::<Result<_, _>>()?;
    Ok(json!({ "stations": stations }))
}

/// `GET /api/stations/nearest?lat=&lon=&max_m=` — nearest current station to
/// a point, for the regroup map-click pick. A degree bbox prefilter (lon side
/// widened by 1/cos(lat)) narrows the current-station scan; the bundled
/// SQLite has no math functions, so the haversine runs in Rust over the
/// (small) candidate set and the radius cut is applied on the computed
/// distance. The box divisor (111,000 < the true 111,195 m/deg at R=6371 km)
/// deliberately over-widens, so the bbox is a strict superset of the circle.
pub fn station_nearest(conn: &Connection, lat: f64, lon: f64, max_m: f64) -> Result<Option<Value>> {
    let lat_delta = max_m / 111_000.0;
    let lon_delta = lat_delta / f64::max(lat.to_radians().cos().abs(), 0.01);
    let mut stmt = conn.prepare(
        "SELECT st.station_id, stv.station_name, stv.latitude, stv.longitude
         FROM station st
         JOIN station_version stv
           ON stv.station_id = st.station_id AND stv.valid_to IS NULL
         WHERE stv.latitude BETWEEN ?1 - ?3 AND ?1 + ?3
           AND stv.longitude BETWEEN ?2 - ?4 AND ?2 + ?4",
    )?;
    let mut best: Option<Value> = None;
    let mut best_d = f64::INFINITY;
    let rows = stmt.query_map(rusqlite::params![lat, lon, lat_delta, lon_delta], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, f64>(2)?,
            r.get::<_, f64>(3)?,
        ))
    })?;
    for row in rows {
        let (station_id, station_name, slat, slon) = row?;
        let d = haversine_m(lat, lon, slat, slon);
        if d <= max_m && d < best_d {
            best_d = d;
            best = Some(json!({
                "station_id": station_id,
                "station_name": station_name,
                "latitude": slat,
                "longitude": slon,
                "distance_m": d,
            }));
        }
    }
    Ok(best)
}

/// Great-circle distance in metres (earth radius 6371 km).
fn haversine_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = p2 - p1;
    let dl = (lon2 - lon1).to_radians();
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * 6_371_000.0 * a.sqrt().asin()
}
