//! Stop/station/group drill-down queries.

use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;
use serde_json::Value;

/// Wire row of the serving-routes arrays (stop/station detail). `bearing`
/// is the member pole's heading — lets the client disambiguate which of a
/// route's two directions serves the clicked side of the road.
#[derive(Serialize)]
struct ServingRoute {
    route_id: i64,
    route_code: String,
    subroute_name: Option<String>,
    direction: i64,
    bearing: Option<f64>,
}

fn serving_routes_where(
    conn: &Connection,
    where_sql: &str,
    params: &[&dyn rusqlite::ToSql],
) -> Result<Vec<ServingRoute>> {
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
        Ok(ServingRoute {
            route_id: r.get("route_id")?,
            route_code: r.get("route_code")?,
            subroute_name: r.get("subroute_name")?,
            direction: r.get("direction")?,
            bearing: r.get("bearing_deg")?,
        })
    })?;
    Ok(it.collect::<Result<_, _>>()?)
}

/// Member stations of a group (current versions), self included — callers
/// filter the station they're showing to get the "other stations" chips.
#[derive(Serialize)]
struct GroupMember {
    station_id: i64,
    station_name: String,
    lat: f64,
    lon: f64,
}

fn group_members(conn: &Connection, group_id: i64) -> Result<Vec<GroupMember>> {
    let mut stmt = conn.prepare(
        "SELECT st.station_id, stv.station_name, stv.latitude, stv.longitude
         FROM station_version stv
         JOIN station st ON st.station_id = stv.station_id
         WHERE stv.station_group_id = ?1 AND stv.valid_to IS NULL",
    )?;
    let it = stmt.query_map([group_id], |r| {
        Ok(GroupMember {
            station_id: r.get("station_id")?,
            station_name: r.get("station_name")?,
            lat: r.get("latitude")?,
            lon: r.get("longitude")?,
        })
    });
    let rows: Vec<GroupMember> = it?.collect::<Result<_, _>>()?;
    Ok(rows)
}

/// One stop-detail row — the aliased join of stop → station → group with
/// both bearings. Field names are the SQL aliases (several source columns
/// collide across joins: two latitude pairs, two bearing_deg).
struct DetailRow {
    stop_version_id: i64,
    stop_code: String,
    stop_name: String,
    lat: f64,
    lon: f64,
    station_id: i64,
    station_name: String,
    station_lat: f64,
    station_lon: f64,
    group_id: i64,
    group_name: String,
    group_source: String,
    bearing_deg: Option<f64>,
    bearing_source: Option<String>,
    station_bearing_deg: Option<f64>,
    station_max_spread_deg: Option<f64>,
    station_member_count: Option<i64>,
    stop_id: i64,
}

/// Wire shape of the `bearing` object in stop detail (null when absent).
#[derive(Serialize)]
struct StopBearing {
    bearing_deg: f64,
    source: Option<String>,
}

/// Wire shape of the `station_bearing` object (null when absent).
#[derive(Serialize)]
struct StationBearing {
    bearing_deg: f64,
    max_spread_deg: Option<f64>,
    member_count: Option<i64>,
}

/// Wire shape of the `stale` object in stop detail.
#[derive(Serialize)]
struct StaleInfo {
    projections: Vec<i64>,
    bearings: bool,
}

#[derive(Serialize)]
struct StopHead {
    stop_version_id: i64,
    stop_code: String,
    stop_name: String,
    lat: f64,
    lon: f64,
    stop_id: i64,
}

#[derive(Serialize)]
struct StationHead {
    station_id: i64,
    station_name: String,
    lat: f64,
    lon: f64,
}

#[derive(Serialize)]
struct GroupHead {
    station_group_id: i64,
    station_group_name: String,
    source: String,
}

/// Wire shape of `GET /api/stops/{stop_version_id}`.
#[derive(Serialize)]
struct StopDetailResponse {
    stop: StopHead,
    station: StationHead,
    group: GroupHead,
    bearing: Option<StopBearing>,
    station_bearing: Option<StationBearing>,
    group_members: Vec<GroupMember>,
    routes_by_stop: Vec<ServingRoute>,
    routes_by_station: Vec<ServingRoute>,
    routes_by_group: Vec<ServingRoute>,
    stale: StaleInfo,
}

/// `GET /api/stops/{stop_version_id}` — stop → station → group + serving routes.
pub fn detail(conn: &Connection, stop_version_id: i64) -> Result<Option<Value>> {
    let found = match conn.query_row(
        "SELECT sv.stop_version_id, sv.stop_code, sv.stop_name,
                sv.latitude AS lat, sv.longitude AS lon,
                st.station_id, stv.station_name,
                stv.latitude AS station_lat, stv.longitude AS station_lon,
                sg.station_group_id, sgv.station_group_name, sgv.source,
                sb.bearing_deg, sb.source AS bearing_source,
                stb.bearing_deg AS station_bearing_deg,
                stb.max_spread_deg AS station_max_spread_deg,
                stb.member_count AS station_member_count,
                sv.stop_id
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
            Ok(DetailRow {
                stop_version_id: r.get("stop_version_id")?,
                stop_code: r.get("stop_code")?,
                stop_name: r.get("stop_name")?,
                lat: r.get("lat")?,
                lon: r.get("lon")?,
                station_id: r.get("station_id")?,
                station_name: r.get("station_name")?,
                station_lat: r.get("station_lat")?,
                station_lon: r.get("station_lon")?,
                group_id: r.get("station_group_id")?,
                group_name: r.get("station_group_name")?,
                group_source: r.get("source")?,
                bearing_deg: r.get("bearing_deg")?,
                bearing_source: r.get("bearing_source")?,
                station_bearing_deg: r.get("station_bearing_deg")?,
                station_max_spread_deg: r.get("station_max_spread_deg")?,
                station_member_count: r.get("station_member_count")?,
                stop_id: r.get("stop_id")?,
            })
        },
    ) {
        Ok(row) => row,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(e) => return Err(e.into()),
    };

    let members = group_members(conn, found.group_id)?;

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
        &[&found.station_id],
    )?;
    let routes_by_group = serving_routes_where(
        conn,
        "rsv.stop_version_id IN (SELECT stop_version_id FROM stop_version
           WHERE station_id IN (SELECT station_id FROM station_version
                                WHERE station_group_id = ?1 AND valid_to IS NULL)
           AND valid_to IS NULL)",
        &[&found.group_id],
    )?;

    Ok(Some(serde_json::to_value(StopDetailResponse {
        stop: StopHead {
            stop_version_id: found.stop_version_id,
            stop_code: found.stop_code,
            stop_name: found.stop_name,
            lat: found.lat,
            lon: found.lon,
            stop_id: found.stop_id,
        },
        station: StationHead {
            station_id: found.station_id,
            station_name: found.station_name,
            lat: found.station_lat,
            lon: found.station_lon,
        },
        group: GroupHead {
            station_group_id: found.group_id,
            station_group_name: found.group_name,
            source: found.group_source,
        },
        bearing: found.bearing_deg.map(|bearing_deg| StopBearing {
            bearing_deg,
            source: found.bearing_source,
        }),
        station_bearing: found.station_bearing_deg.map(|bearing_deg| StationBearing {
            bearing_deg,
            max_spread_deg: found.station_max_spread_deg,
            member_count: found.station_member_count,
        }),
        group_members: members,
        routes_by_stop,
        routes_by_station,
        routes_by_group,
        stale: StaleInfo {
            projections: stale_projections,
            bearings: bearings_stale,
        },
    })?))
}

/// One station-detail row — station → group with the station bearing.
struct StationDetailRow {
    station_id: i64,
    station_name: String,
    lat: f64,
    lon: f64,
    group_id: i64,
    group_name: String,
    group_source: String,
    bearing_deg: Option<f64>,
    max_spread_deg: Option<f64>,
    member_count: Option<i64>,
    station_version_id: i64,
}

/// Wire row of the `stops` array in station detail.
#[derive(Serialize)]
struct StationStopRow {
    stop_version_id: i64,
    stop_code: String,
    stop_name: String,
    direction: i64,
    lat: f64,
    lon: f64,
}

/// Wire shape of `GET /api/stations/{station_id}`.
#[derive(Serialize)]
struct StationDetailResponse {
    station: StationDetailHead,
    group: GroupHead,
    bearing: Option<StationBearing>,
    stops: Vec<StationStopRow>,
    routes: Vec<ServingRoute>,
    group_members: Vec<GroupMember>,
    stale: StationStaleInfo,
}

#[derive(Serialize)]
struct StationDetailHead {
    station_id: i64,
    station_name: String,
    lat: f64,
    lon: f64,
    station_version_id: i64,
}

/// Wire shape of the `stale` object in station detail (stations have no
/// projections — only the bearing tables go stale).
#[derive(Serialize)]
struct StationStaleInfo {
    bearings: bool,
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
            Ok(StationDetailRow {
                station_id: r.get("station_id")?,
                station_name: r.get("station_name")?,
                lat: r.get("latitude")?,
                lon: r.get("longitude")?,
                group_id: r.get("station_group_id")?,
                group_name: r.get("station_group_name")?,
                group_source: r.get("source")?,
                bearing_deg: r.get("bearing_deg")?,
                max_spread_deg: r.get("max_spread_deg")?,
                member_count: r.get("member_count")?,
                station_version_id: r.get("station_version_id")?,
            })
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
    let stops: Vec<StationStopRow> = stmt
        .query_map([station_id], |r| {
            Ok(StationStopRow {
                stop_version_id: r.get("stop_version_id")?,
                stop_code: r.get("stop_code")?,
                stop_name: r.get("stop_name")?,
                direction: r.get("direction")?,
                lat: r.get("latitude")?,
                lon: r.get("longitude")?,
            })
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

    Ok(Some(serde_json::to_value(StationDetailResponse {
        station: StationDetailHead {
            station_id: found.station_id,
            station_name: found.station_name,
            lat: found.lat,
            lon: found.lon,
            station_version_id: found.station_version_id,
        },
        group: GroupHead {
            station_group_id: found.group_id,
            station_group_name: found.group_name,
            source: found.group_source,
        },
        bearing: found.bearing_deg.map(|bearing_deg| StationBearing {
            bearing_deg,
            max_spread_deg: found.max_spread_deg,
            member_count: found.member_count,
        }),
        stops,
        routes,
        group_members: group_members(conn, found.group_id)?,
        stale: StationStaleInfo {
            bearings: bearings_stale,
        },
    })?))
}

/// Wire properties of a viewport feature. Stop features carry
/// `bearing_source`, station features `spread` — the other key is absent,
/// not null (hence skip_serializing_if, exactly the old json! behavior).
#[derive(Serialize)]
struct ViewportProps {
    id: i64,
    name: String,
    kind: String,
    bearing: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    spread: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bearing_source: Option<String>,
}

#[derive(Serialize)]
struct ViewportFeature {
    #[serde(rename = "type")]
    typ: &'static str,
    geometry: Value,
    properties: ViewportProps,
}

/// Wire shape of `GET /api/stops?bbox=…` (GeoJSON FeatureCollection).
#[derive(Serialize)]
struct ViewportFc {
    #[serde(rename = "type")]
    typ: &'static str,
    features: Vec<ViewportFeature>,
    truncated: bool,
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
    let features = {
        let it = stmt.query_map(
            rusqlite::params![min_lon, min_lat, max_lon, max_lat, limit],
            |r| {
                let (lat, lon): (f64, f64) = (r.get("latitude")?, r.get("longitude")?);
                Ok(ViewportFeature {
                    typ: "Feature",
                    geometry: crate::geojson::point(lon, lat),
                    properties: ViewportProps {
                        id: if kind == "station" {
                            r.get::<_, i64>("station_id")?
                        } else {
                            r.get::<_, i64>("stop_version_id")?
                        },
                        name: r.get(name_col)?,
                        kind: kind.to_string(),
                        bearing: r.get("bearing_deg")?,
                        spread: if kind == "station" {
                            r.get("max_spread_deg")?
                        } else {
                            None
                        },
                        bearing_source: if kind == "station" {
                            None
                        } else {
                            r.get("source")?
                        },
                    },
                })
            },
        )?;
        it.collect::<Result<Vec<_>, _>>()?
    };
    Ok(serde_json::to_value(ViewportFc {
        typ: "FeatureCollection",
        features,
        truncated: total > limit,
    })?)
}

/// Wire row of `GET /api/stops?q=`.
#[derive(Serialize)]
struct SearchStopRow {
    stop_version_id: i64,
    stop_code: String,
    stop_name: String,
    lat: f64,
    lon: f64,
}

#[derive(Serialize)]
struct StopsSearchResponse {
    stops: Vec<SearchStopRow>,
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
    let stops: Vec<SearchStopRow> = stmt
        .query_map([like], |r| {
            Ok(SearchStopRow {
                stop_version_id: r.get("stop_version_id")?,
                stop_code: r.get("stop_code")?,
                stop_name: r.get("stop_name")?,
                lat: r.get("latitude")?,
                lon: r.get("longitude")?,
            })
        })?
        .collect::<Result<_, _>>()?;
    Ok(serde_json::to_value(StopsSearchResponse { stops })?)
}

/// Wire row of `GET /api/stations?q=`.
#[derive(Serialize)]
struct StationSearchRow {
    station_id: i64,
    station_name: String,
    lat: f64,
    lon: f64,
}

#[derive(Serialize)]
struct StationsSearchResponse {
    stations: Vec<StationSearchRow>,
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
    let stations: Vec<StationSearchRow> = stmt
        .query_map([like], |r| {
            Ok(StationSearchRow {
                station_id: r.get("station_id")?,
                station_name: r.get("station_name")?,
                lat: r.get("latitude")?,
                lon: r.get("longitude")?,
            })
        })?
        .collect::<Result<_, _>>()?;
    Ok(serde_json::to_value(StationsSearchResponse { stations })?)
}

/// `GET /api/stations/nearest?lat=&lon=&max_m=` — nearest current station to
/// a point, for the regroup map-click pick. A degree bbox prefilter (lon side
/// widened by 1/cos(lat)) narrows the current-station scan; the bundled
/// SQLite has no math functions, so the haversine runs in Rust over the
/// (small) candidate set and the radius cut is applied on the computed
/// distance. The box divisor (111,000 < the true 111,195 m/deg at R=6371 km)
/// deliberately over-widens, so the bbox is a strict superset of the circle.
/// Wire shape of `GET /api/stations/nearest` — the one endpoint that
/// spells `latitude`/`longitude` in full (kept for wire compatibility;
/// everywhere else the wire says `lat`/`lon`).
#[derive(Serialize)]
struct StationNearest {
    station_id: i64,
    station_name: String,
    #[serde(rename = "latitude")]
    lat: f64,
    #[serde(rename = "longitude")]
    lon: f64,
    distance_m: f64,
}

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
    let mut best: Option<StationNearest> = None;
    let mut best_d = f64::INFINITY;
    let rows = stmt.query_map(rusqlite::params![lat, lon, lat_delta, lon_delta], |r| {
        Ok((
            r.get::<_, i64>("station_id")?,
            r.get::<_, String>("station_name")?,
            r.get::<_, f64>("latitude")?,
            r.get::<_, f64>("longitude")?,
        ))
    })?;
    for row in rows {
        let (station_id, station_name, slat, slon) = row?;
        let d = haversine_m(lat, lon, slat, slon);
        if d <= max_m && d < best_d {
            best_d = d;
            best = Some(StationNearest {
                station_id,
                station_name,
                lat: slat,
                lon: slon,
                distance_m: d,
            });
        }
    }
    Ok(best.map(serde_json::to_value).transpose()?)
}

/// Great-circle distance in metres (earth radius 6371 km).
fn haversine_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = p2 - p1;
    let dl = (lon2 - lon1).to_radians();
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * 6_371_000.0 * a.sqrt().asin()
}
