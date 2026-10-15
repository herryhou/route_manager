//! Golden wire-shape contract for every read endpoint: exact key sets +
//! value types, asserted against the current responses. The queries
//! modules' serde structs (candidate 2, shape authority) must keep these
//! green byte-for-byte — a key added, removed, or retyped here is a wire
//! break for the JS consumers in assets/js.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use rm_web::router_with;
use rm_web::state::AppState;
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
