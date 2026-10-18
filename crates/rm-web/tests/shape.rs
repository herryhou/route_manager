//! Golden wire-shape contract for every endpoint: exact key sets + value
//! types, asserted against the current responses. The queries modules'
//! serde structs (candidate 2, shape authority) and the edit handlers'
//! `<Type>Data` mirrors must keep these green byte-for-byte — a key
//! added, removed, or retyped here is a wire break for the JS consumers
//! in assets/js and the rm-web-jobs agent skill.

use std::collections::HashMap;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use rm_core::{polyline, LatLng};
use rm_web::router_with;
use rm_web::state::AppState;
use serde_json::json;
use tower::ServiceExt;

// ───────────────────────── shape-spec walker ─────────────────────────

enum Spec {
    Str,
    Num,
    Int,
    Bool,
    Any,
    Null,
    /// exact key set — a missing OR unexpected key fails
    Obj(Vec<(&'static str, Spec)>),
    /// every element matches
    Arr(Box<Spec>),
    /// exactly two elements (the [lon, lat] projection pairs)
    Pair(Box<Spec>),
    /// null or the inner shape
    Opt(Box<Spec>),
}

fn obj(fields: Vec<(&'static str, Spec)>) -> Spec {
    Spec::Obj(fields)
}
fn arr(el: Spec) -> Spec {
    Spec::Arr(Box::new(el))
}
fn pair(el: Spec) -> Spec {
    Spec::Pair(Box::new(el))
}
fn opt(el: Spec) -> Spec {
    Spec::Opt(Box::new(el))
}

fn check(v: &serde_json::Value, s: &Spec, path: &str) {
    match s {
        Spec::Str => assert!(v.is_string(), "{path}: want string, got {v}"),
        Spec::Num => assert!(v.is_number(), "{path}: want number, got {v}"),
        Spec::Int => assert!(v.is_i64(), "{path}: want integer, got {v}"),
        Spec::Bool => assert!(v.is_boolean(), "{path}: want bool, got {v}"),
        Spec::Any => {}
        Spec::Null => assert!(v.is_null(), "{path}: want null, got {v}"),
        Spec::Obj(fields) => {
            let o = v
                .as_object()
                .unwrap_or_else(|| panic!("{path}: want object, got {v}"));
            for (k, sub) in fields {
                let sv = o
                    .get(*k)
                    .unwrap_or_else(|| panic!("{path}.{k}: missing key (have {:?})", o.keys()));
                check(sv, sub, &format!("{path}.{k}"));
            }
            for k in o.keys() {
                assert!(
                    fields.iter().any(|(fk, _)| fk == k),
                    "{path}.{k}: unexpected key (have {:?})",
                    o.keys()
                );
            }
        }
        Spec::Arr(el) => {
            let a = v
                .as_array()
                .unwrap_or_else(|| panic!("{path}: want array, got {v}"));
            for (i, item) in a.iter().enumerate() {
                check(item, el, &format!("{path}[{i}]"));
            }
        }
        Spec::Pair(el) => {
            let a = v
                .as_array()
                .unwrap_or_else(|| panic!("{path}: want [a, b], got {v}"));
            assert_eq!(a.len(), 2, "{path}: want 2 elements, got {v}");
            for (i, item) in a.iter().enumerate() {
                check(item, el, &format!("{path}[{i}]"));
            }
        }
        Spec::Opt(el) => {
            if !v.is_null() {
                check(v, el, path);
            }
        }
    }
}

// ───────────────────────── shared row shapes ─────────────────────────

/// `serving_routes_where` rows — routes_by_stop / routes_by_station /
/// routes_by_group in stop detail, `routes` in station detail.
fn serving_route() -> Spec {
    obj(vec![
        ("route_id", Spec::Int),
        ("route_code", Spec::Str),
        ("subroute_name", opt(Spec::Str)),
        ("direction", Spec::Int),
        ("bearing", opt(Spec::Num)),
    ])
}

fn group_member() -> Spec {
    obj(vec![
        ("station_id", Spec::Int),
        ("station_name", Spec::Str),
        ("lat", Spec::Num),
        ("lon", Spec::Num),
    ])
}

/// `direction_rows` rows inside routes list/detail.
fn direction_row() -> Spec {
    obj(vec![
        ("route_id", Spec::Int),
        ("direction", Spec::Int),
        ("stops", Spec::Int),
        ("matched_ratio", opt(Spec::Num)),
        ("snap_dist_p50_m", opt(Spec::Num)),
        ("snap_dist_p95_m", opt(Spec::Num)),
        ("outliers", opt(Spec::Int)),
    ])
}

// ───────────────────────── fixtures (copied from api.rs) ─────────────────────────

fn fixture_db() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    rm_ingest::testfix::make_fixture(&dir.path().join("src"));
    let db = dir.path().join("fixture.db");
    let mut conn = rm_db::open(&db).unwrap();
    let s = rm_ingest::run(&conn, &dir.path().join("src")).unwrap();
    rm_identity::run(&conn, s.batch_id).unwrap();
    rm_publish::run(&mut conn, s.batch_id).unwrap();
    rm_segments::run(&conn, s.batch_id, 50.0).unwrap();
    (dir, db)
}

/// Full chain through matcher + bearing (see api.rs `fixture_db_matched`).
fn fixture_db_matched() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    rm_ingest::testfix::make_fixture(&dir.path().join("src"));
    let db = dir.path().join("fixture.db");
    let mut conn = rm_db::open(&db).unwrap();
    let s = rm_ingest::run(&conn, &dir.path().join("src")).unwrap();
    conn.execute_batch(
        "INSERT INTO osm_way (osm_way_id, highway, oneway, name) VALUES (100, 'residential', 0, '測試路');
         INSERT INTO osm_node (osm_node_id, lat, lon) VALUES
            (1, 24.1405, 120.6857), (2, 24.1408, 120.6857), (3, 24.1412, 120.6857);
         INSERT INTO osm_way_node (osm_way_id, ordinal, osm_node_id) VALUES
            (100, 1, 1), (100, 2, 2), (100, 3, 3);",
    )
    .unwrap();
    let enc = rm_core::polyline::encode(&[
        rm_core::LatLng {
            lat: 24.1412,
            lon: 120.6857,
        },
        rm_core::LatLng {
            lat: 24.1410,
            lon: 120.6857,
        },
        rm_core::LatLng {
            lat: 24.1407,
            lon: 120.6857,
        },
        rm_core::LatLng {
            lat: 24.1405,
            lon: 120.6857,
        },
    ]);
    conn.execute(
        "UPDATE src_route_point SET encoded = ?1 WHERE batch_id = ?2 AND xno IN ('1','2')",
        rusqlite::params![enc, s.batch_id],
    )
    .unwrap();
    let rows: [(i64, i64, &str, f64, f64); 3] = [
        (1, 21, "44", 24.1409, 120.6857),
        (2, 22, "50", 24.14085, 120.6858),
        (3, 23, "46", 24.1408, 120.6859),
    ];
    conn.execute(
        "DELETE FROM src_stop_edge WHERE batch_id = ?1 AND xno = '2'",
        [s.batch_id],
    )
    .unwrap();
    for (order, sid, node, lat, lon) in rows {
        conn.execute(
            "INSERT OR REPLACE INTO src_stop_edge
                (batch_id, xno, direction, source_order_no, ivr_no, sid, node_id, station_name, lat, lon, lat_e6, lon_e6)
             VALUES (?1, '2', 0, ?2, NULL, ?3, ?4, '干城站', ?5, ?6, ?7, ?8)",
            rusqlite::params![
                s.batch_id, order, sid, node, lat, lon,
                (lat * 1e6).round() as i64,
                (lon * 1e6).round() as i64
            ],
        )
        .unwrap();
    }
    conn.execute_batch(
        "UPDATE route SET current_route_version_id = NULL, revision = 0 WHERE ext_uid = 'tcg:2';
         DELETE FROM route_stop_version WHERE subroute_version_id IN
            (SELECT subroute_version_id FROM subroute_version WHERE route_version_id =
                (SELECT route_version_id FROM route_version WHERE route_id =
                    (SELECT route_id FROM route WHERE ext_uid = 'tcg:2')));
         DELETE FROM subroute_version WHERE route_version_id IN
            (SELECT route_version_id FROM route_version WHERE route_id =
                (SELECT route_id FROM route WHERE route_id =
                    (SELECT route_id FROM route WHERE ext_uid = 'tcg:2')));
         DELETE FROM route_version WHERE route_id IN
            (SELECT route_version_id FROM route_version WHERE route_id =
                (SELECT route_id FROM route WHERE ext_uid = 'tcg:2'));
         DELETE FROM stop_version WHERE stop_id IN
            (SELECT stop_id FROM stop WHERE ext_uid LIKE 'tcg:2:%');
         DELETE FROM stop WHERE ext_uid LIKE 'tcg:2:%';",
    )
    .unwrap();
    rm_identity::run(&conn, s.batch_id).unwrap();
    rm_publish::run(&mut conn, s.batch_id).unwrap();
    rm_segments::run(&conn, s.batch_id, 50.0).unwrap();
    rm_osm_match::run(&conn, s.batch_id).unwrap();
    rm_registry::bearing::run(&conn, s.batch_id).unwrap();
    (dir, db)
}

/// Route 2 dir 0 gets an off-path stop so rm_segments records OUTLIER rows.
fn fixture_db_with_outlier() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    rm_ingest::testfix::make_fixture(&dir.path().join("src"));
    let db = dir.path().join("fixture.db");
    let mut conn = rm_db::open(&db).unwrap();
    let s = rm_ingest::run(&conn, &dir.path().join("src")).unwrap();
    conn.execute(
        "UPDATE src_route_point SET encoded = ?1
         WHERE batch_id = ?2 AND xno = '2' AND direction = 0",
        rusqlite::params![rm_segments::geometry::REF_PATH_ENC, s.batch_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO src_stop_edge
            (batch_id, xno, direction, source_order_no, ivr_no, sid, node_id, station_name, lat, lon, lat_e6, lon_e6)
         VALUES (?1, '2', 0, 2, NULL, 99, '99', '遠方站', 24.5, 121.0, 24500000, 121000000)",
        rusqlite::params![s.batch_id],
    )
    .unwrap();
    rm_identity::run(&conn, s.batch_id).unwrap();
    rm_publish::run(&mut conn, s.batch_id).unwrap();
    rm_segments::run(&conn, s.batch_id, 50.0).unwrap();
    (dir, db)
}

/// Two hand-written match stats: one straying direction, one tight.
fn fixture_db_with_dev() -> (tempfile::TempDir, std::path::PathBuf) {
    let (dir, db) = fixture_db();
    let conn = rm_db::open(&db).unwrap();
    let rv: i64 = conn
        .query_row(
            "SELECT rv.route_version_id FROM route r
             JOIN route_version rv ON rv.route_id = r.route_id
                  AND rv.route_version_id = r.current_route_version_id
             WHERE r.ext_uid = 'tcg:2'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    conn.execute_batch(&format!(
        "INSERT INTO seg_match_stat
            (route_version_id, direction, stops, path_length_m, matched_length_m,
             snap_dist_p50, snap_dist_p95, snap_dist_max, outliers, gap_repairs,
             dev_p50_m, dev_p95_m, dev_max_m)
         VALUES
            ({rv}, 0, 1, 1000.0, 1000.0, 5.0, 10.0, 20.0, 0, 0, 40.0, 80.0, 90.0),
            ({rv}, 1, 1, 1000.0, 1000.0, 5.0, 10.0, 20.0, 0, 0, 5.0, 10.0, 12.0);",
    ))
    .unwrap();
    (dir, db)
}

async fn get_json(app: &axum::Router, uri: &str) -> serde_json::Value {
    let res = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    assert_eq!(status, StatusCode::OK, "GET {uri}");
    serde_json::from_slice(&bytes).unwrap()
}

fn app(db: std::path::PathBuf) -> axum::Router {
    router_with(AppState::new(db))
}

/// route 1's id from the list endpoint
async fn route1_id(app: &axum::Router) -> i64 {
    let v = get_json(app, "/api/routes").await;
    v["routes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["route_code"] == serde_json::json!("1"))
        .unwrap()["route_id"]
        .as_i64()
        .unwrap()
}

// ───────────────────────── per-endpoint contracts ─────────────────────────

#[tokio::test]
async fn shape_stats() {
    let (_dir, db) = fixture_db();
    let a = app(db);
    let v = get_json(&a, "/api/stats").await;
    check(
        &v,
        &obj(vec![
            (
                "counts",
                obj(vec![
                    ("routes", Spec::Int),
                    ("stops", Spec::Int),
                    ("stations", Spec::Int),
                    ("groups", Spec::Int),
                    ("osm_ways", Spec::Int),
                    ("geometry_drafts", Spec::Int),
                ]),
            ),
            (
                "match",
                obj(vec![
                    ("directions_with_stats", Spec::Int),
                    ("directions_total", Spec::Int),
                    ("matched_length_m", Spec::Num),
                    ("path_length_m", Spec::Num),
                    ("snap_dist_p50_avg", opt(Spec::Num)),
                    ("outliers", Spec::Int),
                    ("gap_repairs", Spec::Int),
                    ("directions_p95_over_100m", Spec::Int),
                    ("directions_matched_under_half", Spec::Int),
                    ("directions_dev_p95_over_50m", Spec::Int),
                ]),
            ),
            ("outlier_stops", Spec::Int),
            ("edit_enabled", Spec::Bool),
            (
                "batch",
                opt(obj(vec![
                    ("batch_id", Spec::Int),
                    ("captured_at", Spec::Str),
                    ("valid_from", Spec::Str),
                    ("status", Spec::Str),
                ])),
            ),
        ]),
        "stats",
    );
}

#[tokio::test]
async fn shape_routes_list_and_detail() {
    let (_dir, db) = fixture_db();
    let a = app(db);

    let v = get_json(&a, "/api/routes").await;
    check(
        &v,
        &obj(vec![(
            "routes",
            arr(obj(vec![
                ("route_id", Spec::Int),
                ("route_code", Spec::Str),
                ("ext_uid", Spec::Str),
                ("subroute_name", opt(Spec::Str)),
                ("providers", arr(Spec::Str)),
                ("side_routes", arr(Spec::Str)),
                ("main_routes", arr(Spec::Str)),
                ("directions", arr(direction_row())),
            ])),
        )]),
        "routes",
    );

    let id1 = route1_id(&a).await;
    let d = get_json(&a, &format!("/api/routes/{id1}")).await;
    check(
        &d,
        &obj(vec![
            ("route_id", Spec::Int),
            ("route_code", Spec::Str),
            ("ext_uid", Spec::Str),
            ("xno", Spec::Str),
            ("revision", Spec::Int),
            ("version_no", Spec::Int),
            ("valid_from", Spec::Str),
            ("status", Spec::Str),
            ("subroute_name", opt(Spec::Str)),
            (
                "subroutes",
                arr(obj(vec![
                    ("subroute_version_id", Spec::Int),
                    ("subroute_name", Spec::Str),
                ])),
            ),
            ("providers", arr(Spec::Str)),
            ("side_routes", arr(Spec::Str)),
            ("main_routes", arr(Spec::Str)),
            ("directions", arr(direction_row())),
        ]),
        "route detail",
    );
}

#[tokio::test]
async fn shape_route_stops() {
    let (_dir, db) = fixture_db();
    let a = app(db);
    let id1 = route1_id(&a).await;
    let v = get_json(&a, &format!("/api/routes/{id1}/directions/0/stops")).await;
    check(
        &v,
        &obj(vec![(
            "stops",
            arr(obj(vec![
                ("stop_sequence", Spec::Int),
                ("source_order_no", opt(Spec::Int)),
                ("stop_version_id", Spec::Int),
                ("stop_code", Spec::Str),
                ("stop_name", Spec::Str),
                ("lat", Spec::Num),
                ("lon", Spec::Num),
                ("station_id", Spec::Int),
                ("chainage_m", opt(Spec::Num)),
                ("snap_dist_m", opt(Spec::Num)),
                ("method", opt(Spec::Str)),
                ("monotonic_clamped", opt(Spec::Int)),
                ("projected", opt(pair(Spec::Num))),
                ("bearing", opt(Spec::Num)),
            ])),
        )]),
        "route stops",
    );
}

#[tokio::test]
async fn shape_route_path() {
    let (_dir, db) = fixture_db();
    let a = app(db);
    let id1 = route1_id(&a).await;
    let v = get_json(&a, &format!("/api/routes/{id1}/directions/0/path")).await;
    check(
        &v,
        &obj(vec![
            ("type", Spec::Str),
            (
                "features",
                arr(obj(vec![
                    ("type", Spec::Str),
                    (
                        "geometry",
                        obj(vec![("type", Spec::Str), ("coordinates", Spec::Any)]),
                    ),
                    (
                        "properties",
                        obj(vec![
                            ("ordinal", Spec::Int),
                            ("length_m", Spec::Num),
                            ("geometry_method", Spec::Str),
                            ("from_seq", Spec::Int),
                            ("to_seq", Spec::Int),
                            ("from_station_id", Spec::Int),
                            ("from_station_name", Spec::Str),
                            ("to_station_id", Spec::Int),
                            ("to_station_name", Spec::Str),
                        ]),
                    ),
                ])),
            ),
        ]),
        "route path",
    );
}

#[tokio::test]
async fn shape_route_ways() {
    let (_dir, db) = fixture_db_matched();
    let a = app(db);
    let id1 = route1_id(&a).await;
    let v = get_json(&a, &format!("/api/routes/{id1}/directions/0/ways")).await;
    check(
        &v,
        &obj(vec![
            ("type", Spec::Str),
            (
                "features",
                arr(obj(vec![
                    ("type", Spec::Str),
                    (
                        "geometry",
                        obj(vec![("type", Spec::Str), ("coordinates", Spec::Any)]),
                    ),
                    (
                        "properties",
                        obj(vec![
                            ("ordinal", Spec::Int),
                            ("osm_way_id", Spec::Int),
                            ("name", opt(Spec::Str)),
                            ("entry_node_id", Spec::Int),
                            ("exit_node_id", Spec::Int),
                            ("entry_coords", pair(Spec::Num)),
                            ("exit_coords", pair(Spec::Num)),
                            ("cum_start_m", Spec::Num),
                            ("cum_end_m", Spec::Num),
                        ]),
                    ),
                ])),
            ),
        ]),
        "route ways",
    );
}

#[tokio::test]
async fn shape_route_raw() {
    let (_dir, db) = fixture_db_matched();
    let a = app(db);
    let id1 = route1_id(&a).await;
    let v = get_json(&a, &format!("/api/routes/{id1}/directions/0/raw")).await;
    check(
        &v,
        &obj(vec![
            ("batch_id", Spec::Int),
            (
                "path",
                opt(obj(vec![
                    ("type", Spec::Str),
                    (
                        "geometry",
                        obj(vec![("type", Spec::Str), ("coordinates", Spec::Any)]),
                    ),
                    ("properties", obj(vec![("source", Spec::Str)])),
                ])),
            ),
            (
                "stops",
                arr(obj(vec![
                    ("source_order_no", Spec::Int),
                    ("ivr_no", opt(Spec::Int)),
                    ("sid", opt(Spec::Int)),
                    ("node_id", Spec::Str),
                    ("station_name", Spec::Str),
                    ("lat", Spec::Num),
                    ("lon", Spec::Num),
                ])),
            ),
            (
                "effective",
                obj(vec![
                    ("points", arr(pair(Spec::Num))),
                    ("source", Spec::Str),
                    ("updated_at", opt(Spec::Str)),
                ]),
            ),
        ]),
        "route raw",
    );
}

#[tokio::test]
async fn shape_stop_detail() {
    let (_dir, db) = fixture_db_matched();
    let a = app(db);
    let id1 = route1_id(&a).await;
    let s = get_json(&a, &format!("/api/routes/{id1}/directions/0/stops")).await;
    let sv_id = s["stops"][0]["stop_version_id"].as_i64().unwrap();

    let d = get_json(&a, &format!("/api/stops/{sv_id}")).await;
    check(
        &d,
        &obj(vec![
            (
                "stop",
                obj(vec![
                    ("stop_version_id", Spec::Int),
                    ("stop_code", Spec::Str),
                    ("stop_name", Spec::Str),
                    ("lat", Spec::Num),
                    ("lon", Spec::Num),
                    ("stop_id", Spec::Int),
                ]),
            ),
            (
                "station",
                obj(vec![
                    ("station_id", Spec::Int),
                    ("station_name", Spec::Str),
                    ("lat", Spec::Num),
                    ("lon", Spec::Num),
                ]),
            ),
            (
                "group",
                obj(vec![
                    ("station_group_id", Spec::Int),
                    ("station_group_name", Spec::Str),
                    ("source", Spec::Str),
                ]),
            ),
            (
                "bearing",
                opt(obj(vec![("bearing_deg", Spec::Num), ("source", Spec::Str)])),
            ),
            (
                "station_bearing",
                opt(obj(vec![
                    ("bearing_deg", Spec::Num),
                    ("max_spread_deg", Spec::Num),
                    ("member_count", Spec::Int),
                ])),
            ),
            ("group_members", arr(group_member())),
            ("routes_by_stop", arr(serving_route())),
            ("routes_by_station", arr(serving_route())),
            ("routes_by_group", arr(serving_route())),
            (
                "stale",
                obj(vec![
                    ("projections", arr(Spec::Int)),
                    ("bearings", Spec::Bool),
                ]),
            ),
        ]),
        "stop detail",
    );
}

#[tokio::test]
async fn shape_station_detail() {
    let (_dir, db) = fixture_db_matched();
    let a = app(db);
    let id1 = route1_id(&a).await;
    let s = get_json(&a, &format!("/api/routes/{id1}/directions/0/stops")).await;
    let st_id = s["stops"][0]["station_id"].as_i64().unwrap();

    let v = get_json(&a, &format!("/api/stations/{st_id}")).await;
    check(
        &v,
        &obj(vec![
            (
                "station",
                obj(vec![
                    ("station_id", Spec::Int),
                    ("station_name", Spec::Str),
                    ("lat", Spec::Num),
                    ("lon", Spec::Num),
                    ("station_version_id", Spec::Int),
                ]),
            ),
            (
                "group",
                obj(vec![
                    ("station_group_id", Spec::Int),
                    ("station_group_name", Spec::Str),
                    ("source", Spec::Str),
                ]),
            ),
            (
                "bearing",
                opt(obj(vec![
                    ("bearing_deg", Spec::Num),
                    ("max_spread_deg", Spec::Num),
                    ("member_count", Spec::Int),
                ])),
            ),
            (
                "stops",
                arr(obj(vec![
                    ("stop_version_id", Spec::Int),
                    ("stop_code", Spec::Str),
                    ("stop_name", Spec::Str),
                    ("direction", Spec::Int),
                    ("lat", Spec::Num),
                    ("lon", Spec::Num),
                ])),
            ),
            ("routes", arr(serving_route())),
            ("group_members", arr(group_member())),
            ("stale", obj(vec![("bearings", Spec::Bool)])),
        ]),
        "station detail",
    );
}

#[tokio::test]
async fn shape_viewport_stop_and_station() {
    let (_dir, db) = fixture_db_matched();
    let a = app(db);
    let bbox = "bbox=120.685,24.140,120.687,24.142";

    // kind=stop: bearing_source present, spread absent
    let v = get_json(&a, &format!("/api/stops?{bbox}&kind=stop")).await;
    assert_eq!(v["type"], serde_json::json!("FeatureCollection"));
    check(
        &v,
        &obj(vec![
            ("type", Spec::Str),
            ("features", arr(Spec::Any)),
            ("truncated", Spec::Bool),
        ]),
        "viewport envelope",
    );
    for f in v["features"].as_array().unwrap() {
        check(
            f,
            &obj(vec![
                ("type", Spec::Str),
                (
                    "geometry",
                    obj(vec![("type", Spec::Str), ("coordinates", pair(Spec::Num))]),
                ),
                (
                    "properties",
                    obj(vec![
                        ("id", Spec::Int),
                        ("name", Spec::Str),
                        ("kind", Spec::Str),
                        ("bearing", opt(Spec::Num)),
                        ("bearing_source", Spec::Str),
                    ]),
                ),
            ]),
            "stop feature",
        );
    }

    // kind=station: spread present, bearing_source absent
    let v = get_json(&a, &format!("/api/stops?{bbox}&kind=station")).await;
    for f in v["features"].as_array().unwrap() {
        check(
            f,
            &obj(vec![
                ("type", Spec::Str),
                (
                    "geometry",
                    obj(vec![("type", Spec::Str), ("coordinates", pair(Spec::Num))]),
                ),
                (
                    "properties",
                    obj(vec![
                        ("id", Spec::Int),
                        ("name", Spec::Str),
                        ("kind", Spec::Str),
                        ("bearing", opt(Spec::Num)),
                        ("spread", Spec::Num),
                    ]),
                ),
            ]),
            "station feature",
        );
    }
}

#[tokio::test]
async fn shape_stops_search() {
    let (_dir, db) = fixture_db();
    let a = app(db);
    let v = get_json(&a, "/api/stops?q=%E5%B9%B2%E5%9F%8E").await; // 干城
    check(
        &v,
        &obj(vec![(
            "stops",
            arr(obj(vec![
                ("stop_version_id", Spec::Int),
                ("stop_code", Spec::Str),
                ("stop_name", Spec::Str),
                ("lat", Spec::Num),
                ("lon", Spec::Num),
            ])),
        )]),
        "stops search",
    );
}

#[tokio::test]
async fn shape_station_search() {
    let (_dir, db) = fixture_db();
    let a = app(db);
    let v = get_json(&a, "/api/stations?q=%E5%B9%B2%E5%9F%8E").await;
    check(
        &v,
        &obj(vec![(
            "stations",
            arr(obj(vec![
                ("station_id", Spec::Int),
                ("station_name", Spec::Str),
                ("lat", Spec::Num),
                ("lon", Spec::Num),
            ])),
        )]),
        "station search",
    );
}

#[tokio::test]
async fn shape_station_nearest() {
    let (_dir, db) = fixture_db();
    let a = app(db);
    let v = get_json(&a, "/api/stations/nearest?lat=24.14&lon=120.685&max_m=500").await;
    check(
        &v,
        &obj(vec![
            ("station_id", Spec::Int),
            ("station_name", Spec::Str),
            // the wire-key drift site: this endpoint spells them out
            ("latitude", Spec::Num),
            ("longitude", Spec::Num),
            ("distance_m", Spec::Num),
        ]),
        "station nearest",
    );
}

#[tokio::test]
async fn shape_qa_outliers() {
    let (_dir, db) = fixture_db_with_outlier();
    let a = app(db);
    let v = get_json(&a, "/api/qa/outliers").await;
    check(
        &v,
        &obj(vec![
            (
                "outliers",
                arr(obj(vec![
                    ("route_id", Spec::Int),
                    ("route_code", Spec::Str),
                    ("direction", Spec::Int),
                    ("stop_version_id", Spec::Int),
                    ("stop_name", Spec::Str),
                    ("stop_sequence", Spec::Int),
                    ("lat", Spec::Num),
                    ("lon", Spec::Num),
                    ("chainage_m", opt(Spec::Num)),
                    ("snap_dist_m", Spec::Num),
                    ("method", Spec::Str),
                    ("projected", opt(pair(Spec::Num))),
                ])),
            ),
            ("count", Spec::Int),
        ]),
        "qa outliers",
    );
}

#[tokio::test]
async fn shape_qa_matchdev() {
    let (_dir, db) = fixture_db_with_dev();
    let a = app(db);
    let v = get_json(&a, "/api/qa/matchdev").await;
    check(
        &v,
        &obj(vec![
            (
                "directions",
                arr(obj(vec![
                    ("route_id", Spec::Int),
                    ("route_code", Spec::Str),
                    ("direction", Spec::Int),
                    ("stops", Spec::Int),
                    ("matched_ratio", opt(Spec::Num)),
                    ("dev_p50_m", Spec::Num),
                    ("dev_p95_m", Spec::Num),
                    ("dev_max_m", Spec::Num),
                    ("snap_dist_p95", opt(Spec::Num)),
                    ("gap_repairs", Spec::Int),
                    ("outliers", Spec::Int),
                ])),
            ),
            ("count", Spec::Int),
        ]),
        "qa matchdev",
    );
}

#[tokio::test]
async fn shape_station_pair_routes() {
    let (_dir, db) = fixture_db();
    let a = app(db);
    let id1 = route1_id(&a).await;
    let p = get_json(&a, &format!("/api/routes/{id1}/directions/0/path")).await;
    let prop = &p["features"][0]["properties"];
    let uri = format!(
        "/api/station-pair/{}/{}/routes",
        prop["from_station_id"].as_i64().unwrap(),
        prop["to_station_id"].as_i64().unwrap()
    );
    let v = get_json(&a, &uri).await;
    check(
        &v,
        &obj(vec![(
            "corridors",
            arr(obj(vec![
                ("corridor_m", Spec::Num),
                ("min_m", Spec::Num),
                ("max_m", Spec::Num),
                (
                    "routes",
                    arr(obj(vec![
                        ("route_id", Spec::Int),
                        ("route_code", Spec::Str),
                        ("subroute_name", opt(Spec::Str)),
                        ("direction", Spec::Int),
                        ("length_m", Spec::Num),
                    ])),
                ),
            ])),
        )]),
        "station pair routes",
    );
}

// ─────────────────── edit endpoints (bearer POST, wire mirrors) ───────────────────

/// Edit-mode router over one fixture DB (helpers copied from api.rs):
/// editor "herry", token "sekrit".
fn edit_app(db: &std::path::Path) -> axum::Router {
    let mut tokens = HashMap::new();
    tokens.insert("herry".to_string(), "sekrit".to_string());
    router_with(AppState::with_edit(db.to_path_buf(), None, tokens))
}

/// POST JSON with the editor bearer header (copied from api.rs).
async fn post_edit(app: &axum::Router, uri: &str, body: serde_json::Value) -> serde_json::Value {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("authorization", "Bearer sekrit")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    assert_eq!(
        status,
        StatusCode::OK,
        "POST {uri}: {}",
        String::from_utf8_lossy(&bytes)
    );
    serde_json::from_slice(&bytes).unwrap()
}

/// route 1 dir 0's first stop (id + station + version ids).
async fn route1_first_stop(app: &axum::Router) -> serde_json::Value {
    let id1 = route1_id(app).await;
    let v = get_json(app, &format!("/api/routes/{id1}/directions/0/stops")).await;
    v["stops"][0].clone()
}

/// The extend-walk fixture (copied from extend_api.rs): a seeded route
/// whose src polyline ends exactly on node 1 of a straight N-S way, so
/// the "last" end extends due north.
fn line_db() -> (tempfile::TempDir, std::path::PathBuf) {
    const T0: &str = "2026-01-01T00:00:00Z";
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let conn = rm_db::open(&db).unwrap();
    let encoded = polyline::encode(&[
        LatLng::new(24.1399, 120.6800),
        LatLng::new(24.1400, 120.6800),
    ]);
    conn.execute_batch(&format!(
        "INSERT INTO station_group (station_group_id, created_at) VALUES (1, '{T0}');
         INSERT INTO station_group_version (station_group_version_id, station_group_id, station_group_name, source, valid_from, created_at)
             VALUES (1, 1, 'g', 'capture', '{T0}', '{T0}');
         INSERT INTO station (station_id, ext_uid, created_at) VALUES (1, 'tcg:node:1', '{T0}');
         INSERT INTO station_version (station_version_id, station_id, station_group_id, station_name, latitude, longitude, valid_from, created_at)
             VALUES (1, 1, 1, 'S', 24.0, 120.0, '{T0}', '{T0}');
         INSERT INTO stop (stop_id, ext_uid, created_at) VALUES (1, 'tcg:stop:1', '{T0}');
         INSERT INTO stop_version (stop_version_id, stop_id, station_id, stop_code, stop_name, direction, latitude, longitude, valid_from, created_at)
             VALUES (10, 1, 1, '001', 'Old', 0, 24.0, 120.0, '{T0}', '{T0}');
         INSERT INTO route (route_id, route_code, ext_uid, revision, created_at, updated_at)
             VALUES (1, '1', 'tcg:1', 1, '{T0}', '{T0}');
         INSERT INTO route_version (route_version_id, route_id, version_no, valid_from, status, created_at)
             VALUES (1, 1, 1, '{T0}', 'PUBLISHED', '{T0}');
         UPDATE route SET current_route_version_id = 1 WHERE route_id = 1;
         INSERT INTO subroute (subroute_id, route_id, subroute_code, created_at)
             VALUES (1, 1, '1-0', '{T0}');
         INSERT INTO subroute_version (subroute_version_id, route_version_id, subroute_id, subroute_name, created_at)
             VALUES (1, 1, 1, '1', '{T0}');
         INSERT INTO route_stop_version (route_stop_version_id, subroute_version_id, stop_version_id, direction, stop_sequence, created_at)
             VALUES (1, 1, 10, 0, 1, '{T0}');
         INSERT INTO id_sequence (table_name, next_value) VALUES
             ('station_group', 2), ('station_group_version', 2),
             ('station', 2), ('station_version', 2),
             ('stop', 2), ('stop_version', 11),
             ('route_version', 2), ('subroute_version', 2),
             ('route_stop_version', 2);
         INSERT INTO import_batch (batch_id, source_dir, file_count, captured_at, valid_from, status, created_at)
             VALUES (1, 'assets/x', 1, '{T0}', '{T0}', 'CLOSED', '{T0}');
         INSERT INTO src_route_point (batch_id, xno, direction, encoded)
             VALUES (1, '1', 0, '{encoded}');
         INSERT INTO osm_way (osm_way_id, highway, oneway, name)
             VALUES (100, 'residential', 0, '直線路');
         INSERT INTO osm_node (osm_node_id, lat, lon) VALUES
            (1, 24.1400, 120.6800),
            (2, 24.1401, 120.6800),
            (3, 24.1402, 120.6800),
            (4, 24.1403, 120.6800);
         INSERT INTO osm_way_node (osm_way_id, ordinal, osm_node_id) VALUES
            (100, 1, 1), (100, 2, 2), (100, 3, 3), (100, 4, 4);"
    ))
    .unwrap();
    (dir, db)
}

#[tokio::test]
async fn shape_edit_add_plan_and_apply() {
    let (_dir, db) = fixture_db();
    let app = edit_app(&db);
    let stop = route1_first_stop(&app).await;
    let station_id = stop["station_id"].as_i64().unwrap();

    // plan: `apply` absent ⇒ dry run (the new endpoints' default)
    let v = post_edit(
        &app,
        "/api/edit/stop",
        json!({
            "name": "形狀測試桿", "lat": 24.1408, "lon": 120.6858,
            "station": station_id, "attach": ["1:0"]
        }),
    )
    .await;
    check(
        &v,
        &obj(vec![
            ("name", Spec::Str),
            ("lat", Spec::Num),
            ("lon", Spec::Num),
            // station given ⇒ the Pinned verdict, serialized as its tag
            (
                "station",
                obj(vec![(
                    "Pinned",
                    obj(vec![("station_id", Spec::Int), ("name", Spec::Str)]),
                )]),
            ),
            (
                "attachments",
                arr(obj(vec![
                    ("xno", Spec::Str),
                    ("direction", Spec::Int),
                    ("route_id", Spec::Int),
                    ("route_version_id", Spec::Int),
                    ("insert_after_stop_code", opt(Spec::Str)),
                    ("neighbor_before", opt(Spec::Str)),
                    ("neighbor_after", opt(Spec::Str)),
                ])),
            ),
            (
                "row_counts",
                obj(vec![
                    ("stop", Spec::Int),
                    ("stop_version", Spec::Int),
                    ("station", Spec::Int),
                    ("station_version", Spec::Int),
                    ("station_group", Spec::Int),
                    ("station_group_version", Spec::Int),
                    ("route_version", Spec::Int),
                    ("subroute_version", Spec::Int),
                    ("route_stop_version", Spec::Int),
                    ("stale_mark", Spec::Int),
                ]),
            ),
            ("stale_routes", arr(Spec::Int)),
            ("warnings", arr(Spec::Str)),
        ]),
        "add plan",
    );

    let v = post_edit(
        &app,
        "/api/edit/stop",
        json!({
            "name": "形狀測試桿", "lat": 24.1408, "lon": 120.6858,
            "station": station_id, "attach": ["1:0"], "apply": true
        }),
    )
    .await;
    check(
        &v,
        &obj(vec![
            ("edit_id", Spec::Int),
            ("stop_id", Spec::Int),
            ("stop_version_ids", arr(Spec::Int)),
            ("station_id", Spec::Int),
            ("station_created", Spec::Bool),
            ("stale_routes", arr(Spec::Int)),
        ]),
        "add apply",
    );
}

#[tokio::test]
async fn shape_edit_detach_plan_and_apply() {
    let (_dir, db) = fixture_db();
    let app = edit_app(&db);

    // every fixture stop is single-serving, so the detach carries
    // delete_pole: true (plan surfaces the warning, apply tears down) —
    // the orphan refusal itself is pinned in api.rs
    let v = post_edit(
        &app,
        "/api/edit/stop/detach",
        json!({ "xno": "1", "direction": 0, "code": "11", "delete_pole": true }),
    )
    .await;
    check(
        &v,
        &obj(vec![
            ("xno", Spec::Str),
            ("direction", Spec::Int),
            ("route_id", Spec::Int),
            ("route_version_id", Spec::Int),
            ("stop_id", Spec::Int),
            ("stop_version_id", Spec::Int),
            ("stop_code", Spec::Str),
            ("stop_name", Spec::Str),
            (
                "occurrences",
                arr(obj(vec![
                    ("route_stop_version_id", Spec::Int),
                    ("sequence", Spec::Int),
                ])),
            ),
            ("pole_orphan", Spec::Bool),
            ("serving_directions", arr(Spec::Int)),
            ("delete_pole", Spec::Bool),
            ("stale_routes", arr(Spec::Int)),
            ("warnings", arr(Spec::Str)),
        ]),
        "detach plan",
    );

    let v = post_edit(
        &app,
        "/api/edit/stop/detach",
        json!({ "xno": "1", "direction": 0, "code": "11", "delete_pole": true, "apply": true }),
    )
    .await;
    check(
        &v,
        &obj(vec![
            ("edit_id", Spec::Int),
            ("old_route_version_id", Spec::Int),
            ("new_route_version_id", Spec::Int),
            ("pole_deleted", Spec::Bool),
            ("stale_routes", arr(Spec::Int)),
        ]),
        "detach apply",
    );
}

#[tokio::test]
async fn shape_edit_stop_preview() {
    let (_dir, db) = fixture_db();
    let app = edit_app(&db);
    let stop = route1_first_stop(&app).await;
    let sv_id = stop["stop_version_id"].as_i64().unwrap();

    let v = post_edit(
        &app,
        &format!("/api/edit/stop/{sv_id}"),
        json!({ "stop_name": "改名桿", "apply": false }),
    )
    .await;
    check(
        &v,
        &obj(vec![
            ("stop_id", Spec::Int),
            ("stop_version_id", Spec::Int),
            ("current_stop_name", Spec::Str),
            ("current_lat", Spec::Num),
            ("current_lon", Spec::Num),
            (
                "op",
                obj(vec![
                    ("stop_id", Spec::Int),
                    ("stop_version_id", Spec::Int),
                    ("stop_name", opt(Spec::Str)),
                    ("lat", opt(Spec::Num)),
                    ("lon", opt(Spec::Num)),
                ]),
            ),
        ]),
        "stop edit preview",
    );
}

#[tokio::test]
async fn shape_edit_regroup_plan() {
    let (_dir, db) = fixture_db();
    let app = edit_app(&db);
    let stop = route1_first_stop(&app).await;
    let sv_id = stop["stop_version_id"].as_i64().unwrap();
    let current = stop["station_id"].as_i64().unwrap();
    // fixture has two 干城站 stations (node 44 / node 46) — regroup across them
    let v = get_json(&app, "/api/stations?q=%E5%B9%B2%E5%9F%8E").await;
    let target = v["stations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["station_id"].as_i64().unwrap())
        .find(|id| *id != current)
        .unwrap();

    let v = post_edit(
        &app,
        &format!("/api/edit/stop/{sv_id}/regroup"),
        json!({ "new_station_id": target, "apply": false }),
    )
    .await;
    check(
        &v,
        &obj(vec![
            ("stop_id", Spec::Int),
            ("stop_version_id", Spec::Int),
            ("stop_code", Spec::Str),
            ("stop_name", Spec::Str),
            ("old_station_id", Spec::Int),
            ("old_station_name", Spec::Str),
            ("new_station_id", Spec::Int),
            ("new_station_name", Spec::Str),
            (
                "op",
                obj(vec![
                    ("stop_id", Spec::Int),
                    ("stop_version_id", Spec::Int),
                    ("new_station_id", Spec::Int),
                ]),
            ),
        ]),
        "regroup plan",
    );
}

#[tokio::test]
async fn shape_edit_station_rename_and_move_plan() {
    let (_dir, db) = fixture_db();
    let app = edit_app(&db);
    let stop = route1_first_stop(&app).await;
    let st_id = stop["station_id"].as_i64().unwrap();
    let svi = get_json(&app, &format!("/api/stations/{st_id}")).await["station"]
        ["station_version_id"]
        .as_i64()
        .unwrap();

    let v = post_edit(
        &app,
        &format!("/api/edit/station/{svi}"),
        json!({ "new_name": "干城新站", "level": "station", "apply": false }),
    )
    .await;
    check(
        &v,
        &obj(vec![
            ("station_id", Spec::Int),
            ("station_version_id", Spec::Int),
            ("old_name", Spec::Str),
            ("new_name", Spec::Str),
            ("level", Spec::Str),
            (
                "poles",
                arr(obj(vec![
                    ("stop_id", Spec::Int),
                    ("stop_version_id", Spec::Int),
                    ("old_name", Spec::Str),
                    ("new_name", Spec::Str),
                ])),
            ),
            (
                "op",
                obj(vec![
                    ("station_id", Spec::Int),
                    ("old_station_version_id", Spec::Int),
                    ("old_station_name", Spec::Str),
                    ("new_station_name", Spec::Str),
                    ("level", Spec::Str),
                    (
                        "poles",
                        arr(obj(vec![
                            ("stop_id", Spec::Int),
                            ("old_stop_version_id", Spec::Int),
                            ("new_stop_name", Spec::Str),
                        ])),
                    ),
                ]),
            ),
        ]),
        "station rename plan",
    );

    let v = post_edit(
        &app,
        &format!("/api/edit/station/{svi}/move"),
        json!({ "lat": 24.1409, "lon": 120.6857, "apply": false }),
    )
    .await;
    check(
        &v,
        &obj(vec![
            ("station_id", Spec::Int),
            ("station_version_id", Spec::Int),
            ("station_name", Spec::Str),
            ("old_latitude", Spec::Num),
            ("old_longitude", Spec::Num),
            ("new_latitude", Spec::Num),
            ("new_longitude", Spec::Num),
            (
                "op",
                obj(vec![
                    ("station_id", Spec::Int),
                    ("old_station_version_id", Spec::Int),
                    ("new_latitude", Spec::Num),
                    ("new_longitude", Spec::Num),
                ]),
            ),
        ]),
        "station move plan",
    );
}

#[tokio::test]
async fn shape_edit_line_plan() {
    // matched fixture: its src polylines are encoded, so /raw has points
    let (_dir, db) = fixture_db_matched();
    let app = edit_app(&db);
    let id1 = route1_id(&app).await;
    let raw = get_json(&app, &format!("/api/routes/{id1}/directions/0/raw")).await;
    let points = raw["effective"]["points"].clone();

    let v = post_edit(
        &app,
        "/api/edit/polyline/1/0/line",
        json!({ "points": points, "apply": false }),
    )
    .await;
    check(
        &v,
        &obj(vec![
            ("route_id", Spec::Int),
            ("route_version_id", opt(Spec::Int)),
            ("xno", Spec::Str),
            ("direction", Spec::Int),
            ("replaces", Spec::Str),
            ("created", Spec::Bool),
            ("points", Spec::Int),
            ("encoded", Spec::Str),
            ("expected_updated_at", opt(Spec::Str)),
            (
                "op",
                obj(vec![
                    ("xno", Spec::Str),
                    ("direction", Spec::Int),
                    ("encoded", Spec::Str),
                    ("expected_updated_at", opt(Spec::Str)),
                ]),
            ),
        ]),
        "line plan",
    );
}

#[tokio::test]
async fn shape_edit_extend_plan_and_apply() {
    // plan over one copy of the extend fixture
    let (_dir, db) = line_db();
    let app = edit_app(&db);
    let v = post_edit(
        &app,
        "/api/edit/polyline/1/0/extend",
        json!({ "meters": 10.0, "end": "last" }),
    )
    .await;
    let extension = || {
        obj(vec![
            ("end", Spec::Str),
            ("way_ids", arr(Spec::Int)),
            ("new_vertices", arr(pair(Spec::Num))),
            ("snap_dist_m", Spec::Num),
        ])
    };
    check(
        &v,
        &obj(vec![
            ("source", Spec::Str),
            ("candidate", arr(pair(Spec::Num))),
            ("extensions", arr(extension())),
            ("logs", arr(Spec::Str)),
        ]),
        "extend plan",
    );

    // apply over a fresh copy (the write replaces the whole line)
    let (_dir2, db2) = line_db();
    let app = edit_app(&db2);
    let v = post_edit(
        &app,
        "/api/edit/polyline/1/0/extend",
        json!({ "meters": 10.0, "end": "last", "apply": true }),
    )
    .await;
    check(
        &v,
        &obj(vec![
            ("edit_id", Spec::Int),
            ("updated_at", Spec::Str),
            ("created", Spec::Bool),
            ("source", Spec::Str),
            ("extensions", arr(extension())),
        ]),
        "extend apply",
    );
}
