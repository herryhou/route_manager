//! Integration tests for the rm-web API, driven through the router.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use rm_web::router_with;
use rm_web::state::AppState;
use tower::ServiceExt;

fn app() -> axum::Router {
    router_with(AppState::new(std::path::PathBuf::from("/nonexistent.db")))
}

/// Fixture pipeline (ingest → identity → publish → segments) on a temp-file DB;
/// returns the dir (keep alive) and the DB path the server can open read-only.
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

async fn get_json(app: &axum::Router, uri: &str) -> serde_json::Value {
    let res = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "GET {uri}");
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn root_serves_html() {
    let res = app()
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/html"));
}

#[tokio::test]
async fn stats_reports_fixture_counts() {
    let (_dir, db) = fixture_db();
    let app = router_with(AppState::new(db));

    let v = get_json(&app, "/api/stats").await;
    let counts = &v["counts"];
    assert_eq!(counts["routes"].as_i64().unwrap(), 2, "fixture has 2 routes");
    for key in ["stops", "stations", "groups", "segments", "traversals"] {
        assert!(counts[key].is_i64(), "counts.{key} missing: {v}");
    }
    assert_eq!(counts["segments"].as_i64().unwrap(), 0, "no registry stage run");
    let m = &v["match"];
    assert_eq!(m["directions_with_stats"].as_i64().unwrap(), 0);
    assert!(m["directions_total"].as_i64().unwrap() >= 2);
    assert!(v["outlier_stops"].is_i64());
    assert_eq!(v["batch"]["batch_id"].as_i64().unwrap(), 1);
    assert!(v["batch"]["captured_at"].is_string());
}

#[tokio::test]
async fn routes_list_and_detail() {
    let (_dir, db) = fixture_db();
    let app = router_with(AppState::new(db));

    let v = get_json(&app, "/api/routes").await;
    let routes = v["routes"].as_array().unwrap();
    assert_eq!(routes.len(), 2);

    let r1 = routes
        .iter()
        .find(|r| r["route_code"] == serde_json::json!("1"))
        .expect("route 1");
    assert_eq!(r1["subroute_name"], serde_json::json!("A - B"));
    assert_eq!(r1["providers"], serde_json::json!(["台中客運"]));
    assert_eq!(r1["side_routes"], serde_json::json!(["2"]));
    let dirs = r1["directions"].as_array().unwrap();
    assert_eq!(dirs.len(), 2, "route 1 has dir 0 + dir 1: {r1}");
    let d0 = dirs.iter().find(|d| d["direction"] == 0).unwrap();
    assert_eq!(d0["stops"].as_i64().unwrap(), 2);
    assert!(d0["matched_ratio"].is_null(), "no match stats in fixture");

    let r2 = routes
        .iter()
        .find(|r| r["route_code"] == serde_json::json!("2"))
        .expect("route 2");
    assert_eq!(r2["subroute_name"], serde_json::json!("C - D"));
    assert_eq!(r2["providers"], serde_json::json!([]));
    assert_eq!(r2["directions"].as_array().unwrap().len(), 1, "dir 0 only");

    // search narrows by code
    let v = get_json(&app, "/api/routes?q=2").await;
    let routes = v["routes"].as_array().unwrap();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0]["route_code"], serde_json::json!("2"));

    // detail
    let id1 = r1["route_id"].as_i64().unwrap();
    let d = get_json(&app, &format!("/api/routes/{id1}")).await;
    assert_eq!(d["route_code"], serde_json::json!("1"));
    assert_eq!(d["xno"], serde_json::json!("1"));
    assert_eq!(d["version_no"].as_i64().unwrap(), 1);
    assert_eq!(d["status"], serde_json::json!("PUBLISHED"));
    assert!(d["subroutes"].as_array().unwrap().len() >= 1);

    // unknown id → 404
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/routes/999")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn route_path_stops_and_drilldown() {
    let (_dir, db) = fixture_db();
    let app = router_with(AppState::new(db));

    let v = get_json(&app, "/api/routes").await;
    let r1 = v["routes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["route_code"] == serde_json::json!("1"))
        .unwrap();
    let id1 = r1["route_id"].as_i64().unwrap();

    // path: GeoJSON FeatureCollection, [lon, lat] order (Taiwan: lon ~120, lat ~24)
    let p = get_json(&app, &format!("/api/routes/{id1}/directions/0/path")).await;
    assert_eq!(p["type"], serde_json::json!("FeatureCollection"));
    let feats = p["features"].as_array().unwrap();
    assert!(!feats.is_empty(), "route 1 has a routePoint polyline: {p}");
    for f in feats {
        let cs = f["geometry"]["coordinates"].as_array().unwrap();
        assert!(cs.len() >= 2, "LineString needs ≥2 points: {f}");
        let lon = cs[0][0].as_f64().unwrap();
        let lat = cs[0][1].as_f64().unwrap();
        assert!((119.0..122.0).contains(&lon), "lon first, got {cs:?}");
        assert!((23.0..25.0).contains(&lat), "lat second, got {cs:?}");
        assert!(f["properties"]["length_m"].is_f64());
        assert!(f["properties"]["geometry_method"].is_string());
    }

    // stops: ordered, with projection QA fields
    let s = get_json(&app, &format!("/api/routes/{id1}/directions/0/stops")).await;
    let stops = s["stops"].as_array().unwrap();
    assert_eq!(stops.len(), 2);
    assert_eq!(stops[0]["stop_sequence"].as_i64().unwrap(), 1);
    assert_eq!(stops[0]["stop_name"], serde_json::json!("干城站"));
    let s0 = &stops[0];
    assert!(s0["chainage_m"].is_f64(), "projection present: {s0}");
    assert!(s0["method"].is_string());
    assert!(s0["projected"].as_array().is_some_and(|a| a.len() == 2));

    // stop drill-down: station + group + serving routes + group members
    let sv_id = stops[0]["stop_version_id"].as_i64().unwrap();
    let d = get_json(&app, &format!("/api/stops/{sv_id}")).await;
    assert_eq!(d["stop"]["stop_name"], serde_json::json!("干城站"));
    assert!(d["station"]["station_id"].is_i64());
    assert_eq!(d["group"]["station_group_name"], serde_json::json!("干城站"));
    let members = d["group_members"].as_array().unwrap();
    assert_eq!(members.len(), 2, "nodes 44+46 same name → one group: {d}");
    assert!(
        !d["routes_by_stop"].as_array().unwrap().is_empty(),
        "route 1 serves this stop"
    );

    // station detail
    let st_id = d["station"]["station_id"].as_i64().unwrap();
    let st = get_json(&app, &format!("/api/stations/{st_id}")).await;
    assert_eq!(st["station"]["station_id"].as_i64().unwrap(), st_id);
    assert!(!st["stops"].as_array().unwrap().is_empty());
    assert!(!st["routes"].as_array().unwrap().is_empty());

    // unknown stop id → 404
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/stops/99999")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

/// Like `fixture_db`, but route 2 dir 0 gets rm-segments' 3-point reference
/// path plus an extra stop ~Mm off it, so `rm_segments` records OUTLIER
/// projections (fixture routePoint for route 1 decodes to a single point,
/// which yields ENDPOINT, not OUTLIER).
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

#[tokio::test]
async fn qa_outliers_list() {
    let (_dir, db) = fixture_db_with_outlier();
    let app = router_with(AppState::new(db));

    let v = get_json(&app, "/api/qa/outliers").await;
    let outliers = v["outliers"].as_array().unwrap();
    // far stop (OUTLIER, huge snap) + route 1 dir 1's NO_PATH stop
    assert!(outliers.len() >= 2, "OUTLIER + NO_PATH rows: {v}");
    let far = outliers
        .iter()
        .find(|o| o["stop_name"] == serde_json::json!("遠方站"))
        .expect("far stop in outliers");
    assert_eq!(far["method"], serde_json::json!("OUTLIER"));
    assert!(far["snap_dist_m"].as_f64().unwrap() > 1000.0, "{far}");
    assert_eq!(far["route_code"], serde_json::json!("2"));
    assert_eq!(far["direction"].as_i64().unwrap(), 0);
    assert!(far["stop_sequence"].as_i64().unwrap() >= 1);
    assert!(far["lat"].is_f64() && far["lon"].is_f64());
    // sorted by snap DESC: far stop (km away) first, not the NO_PATH one
    assert!(
        outliers[0]["snap_dist_m"].as_f64().unwrap()
            >= outliers[outliers.len() - 1]["snap_dist_m"]
                .as_f64()
                .unwrap(),
        "snap-desc order: {v}"
    );

    // min_snap filters by distance but keeps method outliers
    let v = get_json(&app, "/api/qa/outliers?min_snap=999999").await;
    let rows = v["outliers"].as_array().unwrap();
    assert!(
        rows.iter().all(|o| o["method"] != serde_json::json!("PROJECTED")),
        "min_snap keeps method outliers, drops plain far stops: {v}"
    );

    // limit caps rows
    let v = get_json(&app, "/api/qa/outliers?limit=1").await;
    assert_eq!(v["outliers"].as_array().unwrap().len(), 1, "{v}");
}

#[tokio::test]
async fn unknown_api_path_is_404_json() {
    let res = app()
        .oneshot(
            Request::builder()
                .uri("/api/definitely-not-a-thing")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}


/// Full fixture chain through rm-registry: road along lon 120.6857, both
/// routes ride it, registry mints 3 canonical segments (44→46, 44→50, 50→46).
fn fixture_db_with_segments() -> (tempfile::TempDir, std::path::PathBuf) {
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
        rm_core::LatLng { lat: 24.1412, lon: 120.6857 },
        rm_core::LatLng { lat: 24.1410, lon: 120.6857 },
        rm_core::LatLng { lat: 24.1407, lon: 120.6857 },
        rm_core::LatLng { lat: 24.1405, lon: 120.6857 },
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
         DELETE FROM stop_of_route_version WHERE subroute_version_id IN
            (SELECT subroute_version_id FROM subroute_version WHERE route_version_id =
                (SELECT route_version_id FROM route_version WHERE route_id =
                    (SELECT route_id FROM route WHERE ext_uid = 'tcg:2')));
         DELETE FROM subroute_version WHERE route_version_id IN
            (SELECT route_version_id FROM route_version WHERE route_id =
                (SELECT route_id FROM route WHERE ext_uid = 'tcg:2'));
         DELETE FROM route_version WHERE route_id IN
            (SELECT route_id FROM route WHERE route_id =
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
    rm_registry::run(&conn, s.batch_id).unwrap();
    (dir, db)
}

#[tokio::test]
async fn segments_bbox_and_detail() {
    let (_dir, db) = fixture_db_with_segments();
    let app = router_with(AppState::new(db));

    // bbox around the road (lon 120.6857, lat 24.1405..24.1412)
    let v = get_json(&app, "/api/segments?bbox=120.6850,24.1400,120.6870,24.1420").await;
    assert_eq!(v["type"], serde_json::json!("FeatureCollection"), "{v}");
    let feats = v["features"].as_array().unwrap();
    assert_eq!(feats.len(), 3, "3 canonical segments: {v}");
    for f in feats {
        assert_eq!(f["geometry"]["type"], serde_json::json!("LineString"));
        let cs = f["geometry"]["coordinates"].as_array().unwrap();
        assert!(cs.len() >= 2, "{f}");
        assert!((120.68..120.69).contains(&cs[0][0].as_f64().unwrap()), "lon first: {cs:?}");
        assert!(f["properties"]["segment_id"].is_string());
        assert!(f["properties"]["route_count"].as_i64().unwrap() >= 1);
        assert!(f["properties"]["length_m"].as_f64().unwrap() > 0.0);
    }

    // disjunct bbox → empty
    let v = get_json(&app, "/api/segments?bbox=121.0,24.0,121.1,24.1").await;
    assert_eq!(v["features"].as_array().unwrap().len(), 0, "{v}");

    // min_routes=2 → none (each segment carries exactly one route in fixture)
    let v = get_json(&app, "/api/segments?bbox=120.6850,24.1400,120.6870,24.1420&min_routes=2").await;
    assert_eq!(v["features"].as_array().unwrap().len(), 0, "{v}");

    // detail: ways composition + traversing routes
    let v = get_json(&app, "/api/segments?bbox=120.6850,24.1400,120.6870,24.1420").await;
    let seg_id = v["features"][0]["properties"]["segment_id"].as_str().unwrap();
    let d = get_json(&app, &format!("/api/segments/{seg_id}")).await;
    assert_eq!(d["segment_id"], serde_json::json!(seg_id));
    let ways = d["ways"].as_array().unwrap();
    assert!(!ways.is_empty(), "{d}");
    assert_eq!(ways[0]["osm_way_id"].as_i64().unwrap(), 100);
    assert_eq!(ways[0]["name"], serde_json::json!("測試路"));
    let routes = d["routes"].as_array().unwrap();
    assert_eq!(routes.len(), 1, "one route per fixture segment: {d}");
    let codes: Vec<&str> = routes.iter().map(|r| r["route_code"].as_str().unwrap()).collect();
    assert!(codes.contains(&"1") || codes.contains(&"2"), "{codes:?}");

    // unknown segment id → 404
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/segments/nonexistent")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
