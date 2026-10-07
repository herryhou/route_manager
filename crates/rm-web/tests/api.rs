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
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    assert_eq!(
        status,
        StatusCode::OK,
        "GET {uri}: {}",
        String::from_utf8_lossy(&bytes)
    );
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
    assert_eq!(
        counts["routes"].as_i64().unwrap(),
        2,
        "fixture has 2 routes"
    );
    for key in ["stops", "stations", "groups", "segments", "traversals"] {
        assert!(counts[key].is_i64(), "counts.{key} missing: {v}");
    }
    assert_eq!(
        counts["segments"].as_i64().unwrap(),
        0,
        "no registry stage run"
    );
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
        assert!(
            f["properties"]["segment_id"].is_null(),
            "no registry run → no traversal segment: {f}"
        );
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
    assert_eq!(
        d["group"]["station_group_name"],
        serde_json::json!("干城站")
    );
    let members = d["group_members"].as_array().unwrap();
    assert_eq!(members.len(), 2, "nodes 44+46 same name → one group: {d}");
    assert!(
        !d["routes_by_stop"].as_array().unwrap().is_empty(),
        "route 1 serves this stop"
    );
    assert_eq!(
        d["segments"],
        serde_json::json!([]),
        "no registry run → no incident segments"
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
        rows.iter()
            .all(|o| o["method"] != serde_json::json!("PROJECTED")),
        "min_snap keeps method outliers, drops plain far stops: {v}"
    );

    // limit caps rows
    let v = get_json(&app, "/api/qa/outliers?limit=1").await;
    assert_eq!(v["outliers"].as_array().unwrap().len(), 1, "{v}");
}

/// Like `fixture_db`, but with two hand-written match stats: one direction
/// straying 80 m off the src polyline (p95), one sitting tight.
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

#[tokio::test]
async fn qa_matchdev_lists_offenders_only() {
    let (_dir, db) = fixture_db_with_dev();
    let app = router_with(AppState::new(db));

    let v = get_json(&app, "/api/qa/matchdev").await;
    let dirs = v["directions"].as_array().unwrap();
    assert_eq!(dirs.len(), 1, "only the straying direction: {v}");
    assert_eq!(dirs[0]["route_code"], serde_json::json!("2"));
    assert_eq!(dirs[0]["direction"].as_i64().unwrap(), 0);
    assert_eq!(dirs[0]["dev_p95_m"].as_f64().unwrap(), 80.0);
    assert!(
        dirs[0]["route_id"].is_i64(),
        "row carries route_id for click-through: {dirs:?}"
    );
    assert!(
        dirs[0]["matched_ratio"].is_f64(),
        "row carries matched ratio for triage: {dirs:?}"
    );

    // stats feeds the path-dev chip
    let s = get_json(&app, "/api/stats").await;
    assert_eq!(
        s["match"]["directions_dev_p95_over_50m"].as_i64().unwrap(),
        1,
        "{s}"
    );

    // min_dev filters
    let v = get_json(&app, "/api/qa/matchdev?min_dev=999999").await;
    assert_eq!(v["directions"].as_array().unwrap().len(), 0, "{v}");
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
    rm_registry::bearing::run(&conn, s.batch_id).unwrap();
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
        assert!(
            (120.68..120.69).contains(&cs[0][0].as_f64().unwrap()),
            "lon first: {cs:?}"
        );
        assert!(f["properties"]["segment_id"].is_string());
        assert!(f["properties"]["route_count"].as_i64().unwrap() >= 1);
        assert!(f["properties"]["length_m"].as_f64().unwrap() > 0.0);
    }

    // disjunct bbox → empty
    let v = get_json(&app, "/api/segments?bbox=121.0,24.0,121.1,24.1").await;
    assert_eq!(v["features"].as_array().unwrap().len(), 0, "{v}");

    // min_routes=2 → none (each segment carries exactly one route in fixture)
    let v = get_json(
        &app,
        "/api/segments?bbox=120.6850,24.1400,120.6870,24.1420&min_routes=2",
    )
    .await;
    assert_eq!(v["features"].as_array().unwrap().len(), 0, "{v}");

    // detail: ways composition + traversing routes
    let v = get_json(&app, "/api/segments?bbox=120.6850,24.1400,120.6870,24.1420").await;
    let seg_id = v["features"][0]["properties"]["segment_id"]
        .as_str()
        .unwrap();
    let d = get_json(&app, &format!("/api/segments/{seg_id}")).await;
    assert_eq!(d["segment_id"], serde_json::json!(seg_id));
    let cs = d["geometry"]["coordinates"].as_array().expect("geometry");
    assert_eq!(d["geometry"]["type"], serde_json::json!("LineString"));
    assert!(cs.len() >= 2, "highlight polyline needs ≥2 vertices: {d}");
    let ways = d["ways"].as_array().unwrap();
    assert!(!ways.is_empty(), "{d}");
    assert_eq!(ways[0]["osm_way_id"].as_i64().unwrap(), 100);
    assert_eq!(ways[0]["name"], serde_json::json!("測試路"));
    let routes = d["routes"].as_array().unwrap();
    assert_eq!(routes.len(), 1, "one route per fixture segment: {d}");
    let codes: Vec<&str> = routes
        .iter()
        .map(|r| r["route_code"].as_str().unwrap())
        .collect();
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

/// Stop/station detail carries the registry segments incident to the stop's
/// station: node 44 is the start endpoint of exactly the 44→46 and 44→50 segments.
#[tokio::test]
async fn stop_and_station_detail_list_incident_segments() {
    let (_dir, db) = fixture_db_with_segments();
    let app = router_with(AppState::new(db));

    let v = get_json(&app, "/api/routes").await;
    let r2 = v["routes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["route_code"] == serde_json::json!("2"))
        .unwrap();
    let id2 = r2["route_id"].as_i64().unwrap();

    let s = get_json(&app, &format!("/api/routes/{id2}/directions/0/stops")).await;
    let stops = s["stops"].as_array().unwrap();
    assert_eq!(stops.len(), 3, "route 2 rides the road with 3 stops: {s}");

    let sv_id = stops[0]["stop_version_id"].as_i64().unwrap(); // node 44
    let d = get_json(&app, &format!("/api/stops/{sv_id}")).await;
    let segs = d["segments"].as_array().expect("segments in stop detail");
    assert_eq!(segs.len(), 2, "node 44 ends exactly 2 of 3 segments: {d}");
    for seg in segs {
        assert!(seg["segment_id"].is_string(), "{seg}");
        assert!(seg["length_m"].as_f64().unwrap() > 0.0, "{seg}");
        assert!(seg["way_count"].as_i64().is_some(), "{seg}");
        assert!(seg["other_station_id"].is_i64(), "{seg}");
        assert!(
            seg["other_station_name"]
                .as_str()
                .is_some_and(|n| !n.is_empty()),
            "{seg}"
        );
        assert_eq!(
            seg["is_start"],
            serde_json::json!(true),
            "44 is the start of both its segments: {seg}"
        );
    }

    // station detail mirrors the incident list for its own station
    let st_id = d["station"]["station_id"].as_i64().unwrap();
    let st = get_json(&app, &format!("/api/stations/{st_id}")).await;
    assert_eq!(
        st["segments"]
            .as_array()
            .expect("segments in station detail")
            .len(),
        2,
        "{st}"
    );

    // every listed segment_id resolves through the segment detail endpoint
    for seg in segs {
        let sid = seg["segment_id"].as_str().unwrap();
        let sd = get_json(&app, &format!("/api/segments/{sid}")).await;
        assert_eq!(sd["segment_id"], serde_json::json!(sid));
    }
}

/// Path pieces carry the seg_traversal segment for their stop pair, so the
/// piece panel can drill into the canonical corridor.
#[tokio::test]
async fn route_path_pieces_link_traversal_segments() {
    let (_dir, db) = fixture_db_with_segments();
    let app = router_with(AppState::new(db));

    let v = get_json(&app, "/api/routes").await;
    let id2 = v["routes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["route_code"] == serde_json::json!("2"))
        .unwrap()["route_id"]
        .as_i64()
        .unwrap();

    let p = get_json(&app, &format!("/api/routes/{id2}/directions/0/path")).await;
    let feats = p["features"].as_array().expect("route 2 path pieces");
    let mut linked = 0;
    for f in feats {
        let sid = f["properties"]["segment_id"].as_str();
        if let Some(sid) = sid {
            linked += 1;
            let d = get_json(&app, &format!("/api/segments/{sid}")).await;
            assert_eq!(d["segment_id"], serde_json::json!(sid), "{f}");
            assert_eq!(
                d["geometry"]["type"],
                serde_json::json!("LineString"),
                "linked segment has highlight geometry: {d}"
            );
        }
    }
    assert!(linked > 0, "registry fixture links ≥1 piece: {p}");
}

#[tokio::test]
async fn route_ways_overlay_trims_to_entry_exit() {
    let (_dir, db) = fixture_db_with_segments();
    let app = router_with(AppState::new(db));

    let v = get_json(&app, "/api/routes").await;
    let r1 = v["routes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["route_code"] == serde_json::json!("1"))
        .unwrap();
    let id1 = r1["route_id"].as_i64().unwrap();

    let v = get_json(&app, &format!("/api/routes/{id1}/directions/0/ways")).await;
    assert_eq!(v["type"], serde_json::json!("FeatureCollection"), "{v}");
    let feats = v["features"].as_array().unwrap();
    assert!(!feats.is_empty(), "route 1 matched the road: {v}");
    for f in feats {
        assert_eq!(f["geometry"]["type"], serde_json::json!("LineString"));
        let cs = f["geometry"]["coordinates"].as_array().unwrap();
        assert!(cs.len() >= 2, "{f}");
        // all vertices on the road (lon 120.6857 ± ε, lat 24.1405..24.1412)
        for c in cs {
            let lon = c[0].as_f64().unwrap();
            let lat = c[1].as_f64().unwrap();
            assert!((120.6855..120.6860).contains(&lon), "lon {lon}: {cs:?}");
            assert!((24.1400..24.1415).contains(&lat), "lat {lat}: {cs:?}");
        }
        let p = &f["properties"];
        assert_eq!(p["osm_way_id"].as_i64().unwrap(), 100);
        assert!(
            p["entry_node_id"].is_i64() && p["exit_node_id"].is_i64(),
            "{p}"
        );
        // trimmed: first vertex IS the entry node, last IS the exit node
        assert_eq!(p["entry_coords"].as_array().unwrap().len(), 2, "{p}");
        assert_eq!(cs[0], p["entry_coords"], "{f}");
        assert_eq!(cs[cs.len() - 1], p["exit_coords"], "{f}");
    }

    // dir 1 also overlays (fixture matcher writes runs for every direction)
    let v = get_json(&app, &format!("/api/routes/{id1}/directions/1/ways")).await;
    assert_eq!(v["type"], serde_json::json!("FeatureCollection"), "{v}");
}

#[tokio::test]
async fn raw_staging_compare() {
    let (_dir, db) = fixture_db_with_segments();
    let app = router_with(AppState::new(db));

    let v = get_json(&app, "/api/routes").await;
    let id1 = v["routes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["route_code"] == serde_json::json!("1"))
        .unwrap()["route_id"]
        .as_i64()
        .unwrap();
    let id2 = v["routes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["route_code"] == serde_json::json!("2"))
        .unwrap()["route_id"]
        .as_i64()
        .unwrap();

    // route 1 dir 0: road polyline + the two fixture stops
    let v = get_json(&app, &format!("/api/routes/{id1}/directions/0/raw")).await;
    assert_eq!(v["batch_id"].as_i64().unwrap(), 1);
    let coords = v["path"]["geometry"]["coordinates"].as_array().unwrap();
    assert!(coords.len() >= 2, "{v}");
    assert!(
        (120.68..120.69).contains(&coords[0][0].as_f64().unwrap()),
        "{coords:?}"
    );
    let stops = v["stops"].as_array().unwrap();
    assert_eq!(stops.len(), 2, "{v}");
    assert_eq!(stops[0]["node_id"], serde_json::json!("44"));
    assert_eq!(stops[0]["station_name"], serde_json::json!("干城站"));
    assert!(stops[0]["lat"].is_f64() && stops[0]["lon"].is_f64());
    assert_eq!(stops[0]["source_order_no"].as_i64().unwrap(), 1);

    // route 2 dir 0: three rewritten raw stops in order 44 -> 50 -> 46
    let v = get_json(&app, &format!("/api/routes/{id2}/directions/0/raw")).await;
    let stops = v["stops"].as_array().unwrap();
    let nodes: Vec<&str> = stops
        .iter()
        .map(|s| s["node_id"].as_str().unwrap())
        .collect();
    assert_eq!(nodes, vec!["44", "50", "46"], "{v}");

    // route 1 dir 1: this fixture's UPDATE fills both directions, so the
    // path decodes too; dir 1 staging has the single node-46 stop
    let v = get_json(&app, &format!("/api/routes/{id1}/directions/1/raw")).await;
    assert!(
        v["path"]["geometry"]["coordinates"].as_array().is_some(),
        "{v}"
    );
    let stops = v["stops"].as_array().unwrap();
    assert_eq!(stops.len(), 1, "{v}");
    assert_eq!(stops[0]["node_id"], serde_json::json!("46"));
}

#[tokio::test]
async fn stops_viewport_and_search() {
    let (_dir, db) = fixture_db();
    let app = router_with(AppState::new(db));

    // bbox over the fixture stops (nodes 44/46 near 24.14, 120.68)
    let v = get_json(
        &app,
        "/api/stops?bbox=120.685,24.140,120.687,24.142&kind=stop",
    )
    .await;
    assert_eq!(v["type"], serde_json::json!("FeatureCollection"), "{v}");
    let feats = v["features"].as_array().unwrap();
    assert!(!feats.is_empty(), "{v}");
    for f in feats {
        assert_eq!(f["geometry"]["type"], serde_json::json!("Point"));
        let c = f["geometry"]["coordinates"].as_array().unwrap();
        assert!((120.68..120.69).contains(&c[0].as_f64().unwrap()), "{c:?}");
        assert!(f["properties"]["name"].is_string());
    }
    // kind=station returns station points
    let v = get_json(
        &app,
        "/api/stops?bbox=120.685,24.140,120.687,24.142&kind=station",
    )
    .await;
    assert!(!v["features"].as_array().unwrap().is_empty(), "{v}");

    // disjunct bbox → empty
    let v = get_json(&app, "/api/stops?bbox=121.0,24.0,121.1,24.1&kind=stop").await;
    assert_eq!(v["features"].as_array().unwrap().len(), 0, "{v}");

    // limit → truncated flag
    let v = get_json(
        &app,
        "/api/stops?bbox=120.685,24.140,120.687,24.142&kind=stop&limit=1",
    )
    .await;
    assert_eq!(v["truncated"], serde_json::json!(true), "{v}");
    assert_eq!(v["features"].as_array().unwrap().len(), 1, "{v}");

    // q search by name
    let v = get_json(&app, "/api/stops?q=%E5%B9%B2%E5%9F%8E").await; // 干城
    let results = v["stops"].as_array().unwrap();
    assert!(!results.is_empty(), "{v}");
    assert!(results[0]["stop_name"].as_str().unwrap().contains("干城"));
    assert!(results[0]["stop_version_id"].is_i64());
}

#[tokio::test]
async fn bearings_exposed_across_endpoints() {
    let (_dir, db) = fixture_db_with_segments();
    let app = router_with(AppState::new(db));

    // viewport stops: bearing + source
    let v = get_json(
        &app,
        "/api/stops?bbox=120.685,24.140,120.687,24.142&kind=stop",
    )
    .await;
    let feats = v["features"].as_array().unwrap();
    assert!(!feats.is_empty(), "{v}");
    for f in feats {
        let b = f["properties"]["bearing"]
            .as_f64()
            .expect("stop bearing present");
        assert!((0.0..360.0).contains(&b), "{f}");
        assert!(f["properties"]["bearing_source"].is_string(), "{f}");
    }

    // viewport stations: bearing + spread
    let v = get_json(
        &app,
        "/api/stops?bbox=120.685,24.140,120.687,24.142&kind=station",
    )
    .await;
    let feats = v["features"].as_array().unwrap();
    assert!(!feats.is_empty(), "{v}");
    for f in feats {
        let b = f["properties"]["bearing"]
            .as_f64()
            .expect("station bearing present");
        assert!((0.0..360.0).contains(&b), "{f}");
        assert!(f["properties"]["spread"].is_f64(), "{f}");
    }

    // route stops: per-stop departure heading
    let v = get_json(&app, "/api/routes").await;
    let id1 = v["routes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["route_code"] == serde_json::json!("1"))
        .unwrap()["route_id"]
        .as_i64()
        .unwrap();
    let s = get_json(&app, &format!("/api/routes/{id1}/directions/0/stops")).await;
    let stops = s["stops"].as_array().unwrap();
    assert!(!stops.is_empty(), "{s}");
    for st in stops {
        let b = st["bearing"].as_f64().expect("route-stop bearing present");
        assert!((0.0..360.0).contains(&b), "{st}");
    }

    // stop detail: own bearing + station bearing
    let sv = stops[0]["stop_version_id"].as_i64().unwrap();
    let d = get_json(&app, &format!("/api/stops/{sv}")).await;
    let b = d["bearing"]["bearing_deg"]
        .as_f64()
        .expect("stop detail bearing");
    assert!((0.0..360.0).contains(&b), "{d}");
    assert!(d["bearing"]["source"].is_string(), "{d}");
    let sb = d["station_bearing"]["bearing_deg"]
        .as_f64()
        .expect("station bearing in stop detail");
    assert!((0.0..360.0).contains(&sb), "{d}");
    assert!(d["station_bearing"]["max_spread_deg"].is_f64(), "{d}");
}

#[tokio::test]
async fn raw_endpoint_survives_missing_staging_row() {
    let (_dir, db) = fixture_db_with_segments();
    // second-import shape: published route, but no src_route_point row in the
    // latest batch for this direction
    let conn = rm_db::open(&db).unwrap();
    conn.execute(
        "DELETE FROM src_route_point WHERE xno = '1' AND direction = 0",
        [],
    )
    .unwrap();
    drop(conn);

    let app = router_with(AppState::new(db));
    let v = get_json(&app, "/api/routes/1/directions/0/raw").await;
    assert!(v["path"].is_null(), "missing row → path null, not 500: {v}");
    assert!(
        !v["stops"].as_array().unwrap().is_empty(),
        "staging stops remain: {v}"
    );
}

#[tokio::test]
async fn segments_limit_keeps_top_corridors() {
    let (_dir, db) = fixture_db_with_segments();
    let conn = rm_db::open(&db).unwrap();
    // make the LAST-inserted segment (route 2's, so it trails route 1's in
    // scan order) carry two routes: point route 1's traversal at it
    conn.execute_batch(
        "UPDATE seg_traversal SET segment_id =
            (SELECT segment_id FROM seg_segment ORDER BY rowid DESC LIMIT 1)
         WHERE subroute_version_id IN (
            SELECT ssv.subroute_version_id FROM subroute_version ssv
            JOIN route_version rv ON rv.route_version_id = ssv.route_version_id
            JOIN route r ON r.route_id = rv.route_id
            WHERE r.route_code = '1');",
    )
    .unwrap();
    drop(conn);

    let app = router_with(AppState::new(db));
    // 3 candidates, limit 1: the shared corridor (route_count 2, scanned
    // last) must win over the first-scanned single-route segments
    let v = get_json(
        &app,
        "/api/segments?bbox=120.6850,24.1400,120.6870,24.1420&limit=1",
    )
    .await;
    let feats = v["features"].as_array().unwrap();
    assert_eq!(feats.len(), 1, "{v}");
    assert_eq!(
        feats[0]["properties"]["route_count"].as_i64().unwrap(),
        2,
        "top corridor kept under limit: {v}"
    );
    assert_eq!(v["truncated"], serde_json::json!(true), "{v}");
}

/// Like `fixture_db`, with one pair split into two corridor segments
/// (395 m + 604 m, same pair_key) and one unsplit pair.
fn fixture_db_with_variants() -> (tempfile::TempDir, std::path::PathBuf) {
    let (dir, db) = fixture_db();
    let conn = rm_db::open(&db).unwrap();
    let (st_a, st_b): (i64, i64) = conn
        .query_row(
            "SELECT MIN(station_id), MAX(station_id) FROM station",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let (rv, sv): (i64, i64) = conn
        .query_row(
            "SELECT rv.route_version_id, sv.subroute_version_id
             FROM route_version rv
             JOIN subroute_version sv ON sv.route_version_id = rv.route_version_id LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let mut rsvs: Vec<i64> = conn
        .prepare("SELECT route_stop_version_id FROM route_stop_version ORDER BY 1")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    rsvs.truncate(4);
    assert_eq!(rsvs.len(), 4, "fixture needs two stop pairs: {rsvs:?}");
    let (rsvs0, rsvs1, rsvs2, rsvs3) = (rsvs[0], rsvs[1], rsvs[2], rsvs[3]);
    conn.execute_batch(&format!(
        "INSERT INTO osm_way (osm_way_id, highway, oneway, name)
            VALUES (100, 'primary', 1, '主線'), (300, 'tertiary', 0, '側線');
         INSERT INTO seg_segment
            (segment_id, pair_key, corridor_key, start_station_id, end_station_id,
             encoded_polyline, length_m, way_count, variant_count, length_spread_m)
         VALUES
            ('segx', 'pk1', '390', {st_a}, {st_b}, '', 395.0, 1, 1, 0.0),
            ('segy', 'pk1', '600', {st_a}, {st_b}, '', 604.0, 1, 1, 0.0),
            ('segz', 'pk2', '120', {st_a}, {st_b}, '', 118.0, 1, 1, 0.0);
         INSERT INTO seg_traversal
            (route_version_id, subroute_version_id, direction,
             from_route_stop_version_id, to_route_stop_version_id, segment_id, way_sig)
         VALUES
            ({rv}, {sv}, 0, {rsvs0}, {rsvs1}, 'segx', '100'),
            ({rv}, {sv}, 0, {rsvs2}, {rsvs3}, 'segx', '100|300');",
    ))
    .unwrap();
    (dir, db)
}

#[tokio::test]
async fn qa_variants_lists_split_pairs() {
    let (_dir, db) = fixture_db_with_variants();
    let app = router_with(AppState::new(db));

    // default: every split pair, one entry per pair with member segments
    let v = get_json(&app, "/api/qa/variants").await;
    let pairs = v["pairs"].as_array().unwrap();
    assert_eq!(pairs.len(), 1, "only the split pair, unsplit excluded: {v}");
    assert_eq!(pairs[0]["pair_key"], serde_json::json!("pk1"));
    assert_eq!(pairs[0]["length_spread_m"].as_f64().unwrap(), 209.0);
    assert!(
        pairs[0]["from"].is_string() && pairs[0]["to"].is_string(),
        "station names for triage: {pairs:?}"
    );
    let segments = pairs[0]["segments"].as_array().unwrap();
    assert_eq!(segments.len(), 2, "{pairs:?}");
    // ascending length: the 395 m corridor first
    assert_eq!(segments[0]["segment_id"], serde_json::json!("segx"));
    assert_eq!(segments[0]["corridor_key"], serde_json::json!("390"));
    assert_eq!(segments[0]["way_sig"], serde_json::json!("100"));
    assert_eq!(segments[0]["ways"][0]["name"], serde_json::json!("主線"));
    assert_eq!(segments[1]["segment_id"], serde_json::json!("segy"));
    // segx carries BOTH variants: majority way list shown (tie -> first),
    // routes + traversals span every variant, not just the majority
    assert!(
        segments[0]["routes"].as_array().unwrap().len() == 2
            && segments[0]["traversals"] == serde_json::json!(2),
        "segment carries users across variants: {segments:?}"
    );
    assert!(
        segments[1]["routes"].as_array().unwrap().len() == 0
            && segments[1]["traversals"] == serde_json::json!(0),
        "traversal-less member: {segments:?}"
    );

    // stats feeds the chip with the split-pair count
    let s = get_json(&app, "/api/stats").await;
    assert_eq!(s["variants"]["split_pairs"].as_i64().unwrap(), 1, "{s}");

    // min_spread filters by member length gap (604 - 395 = 209)
    let v = get_json(&app, "/api/qa/variants?min_spread=300").await;
    assert_eq!(v["pairs"].as_array().unwrap().len(), 0, "{v}");
    let v = get_json(&app, "/api/qa/variants?min_spread=100").await;
    assert_eq!(v["pairs"].as_array().unwrap().len(), 1, "{v}");
}
