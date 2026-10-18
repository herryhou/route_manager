//! Integration tests for the rm-web API, driven through the router.

use std::collections::HashMap;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use rm_web::router_with;
use rm_web::state::AppState;
use serde_json::json;
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
    for key in ["stops", "stations", "groups"] {
        assert!(counts[key].is_i64(), "counts.{key} missing: {v}");
    }
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
        // station pair of the piece's boundary stops
        assert!(f["properties"]["from_station_id"].is_i64(), "{f}");
        assert!(f["properties"]["to_station_id"].is_i64(), "{f}");
        assert!(f["properties"]["from_station_name"].is_string(), "{f}");
        assert!(f["properties"]["to_station_name"].is_string(), "{f}");
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
    assert!(
        d["stop"]["stop_id"].is_i64(),
        "stop_id exposed for edit-log queries: {}",
        d["stop"]
    );
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

/// Route 2 gains a second stop reusing route 1's pair-end stop, so both
/// routes traverse the same directed station pair — route 2's cut seeded
/// 500 m longer, which the corridor split must separate into its own cluster.
#[tokio::test]
async fn station_pair_routes_cluster_corridors() {
    let (_dir, db) = fixture_db();
    let app = router_with(AppState::new(db.clone()));

    let routes = get_json(&app, "/api/routes").await;
    let find = |code: &str| {
        routes["routes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["route_code"] == serde_json::json!(code))
            .unwrap()["route_id"]
            .as_i64()
            .unwrap()
    };
    let (id1, id2) = (find("1"), find("2"));

    // piece properties carry the pair this test keys on
    let p = get_json(&app, &format!("/api/routes/{id1}/directions/0/path")).await;
    let prop = &p["features"][0]["properties"];
    let (from_station, to_station) = (
        prop["from_station_id"].as_i64().unwrap(),
        prop["to_station_id"].as_i64().unwrap(),
    );

    let uri = format!("/api/station-pair/{from_station}/{to_station}/routes");
    let v = get_json(&app, &uri).await;
    let cs = v["corridors"].as_array().unwrap();
    assert_eq!(cs.len(), 1, "pre-seed: only route 1 runs the pair: {v}");
    assert_eq!(cs[0]["routes"][0]["route_id"], serde_json::json!(id1));
    let len1 = cs[0]["min_m"].as_f64().unwrap();
    assert!(cs[0]["routes"][0]["length_m"].is_f64());
    assert!(cs[0]["routes"][0]["direction"].is_i64());

    // seed route 2 onto the same directed pair, 500 m longer
    {
        let conn = rm_db::open(&db).unwrap();
        let (to_svid,): (i64,) = conn
            .query_row(
                "SELECT r2.stop_version_id
                 FROM route_stop_current r1
                 JOIN route_stop_current r2
                   ON r2.subroute_version_id = r1.subroute_version_id
                  AND r2.direction = r1.direction
                  AND r2.stop_sequence = r1.stop_sequence + 1
                 WHERE r1.route_id = ?1 AND r1.direction = 0",
                [id1],
                |r| Ok((r.get(0)?,)),
            )
            .unwrap();
        let (sv2, from_rsv): (i64, i64) = conn
            .query_row(
                "SELECT sv.subroute_version_id, rsv.route_stop_version_id
                 FROM subroute_version sv
                 JOIN route_stop_version rsv ON rsv.subroute_version_id = sv.subroute_version_id
                 JOIN route_version rv ON rv.route_version_id = sv.route_version_id
                 WHERE rv.route_id = ?1 AND rsv.direction = 0 AND rsv.stop_sequence = 1",
                [id2],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO route_stop_version
                (subroute_version_id, stop_version_id, direction, stop_sequence, created_at)
             VALUES (?1, ?2, 0, 2, '2026-01-01T00:00:00Z')",
            rusqlite::params![sv2, to_svid],
        )
        .unwrap();
        let new_rsv = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO seg_geometry_draft
                (subroute_version_id, direction, ordinal, from_route_stop_version_id,
                 to_route_stop_version_id, encoded_polyline, length_m, geometry_method)
             VALUES (?1, 0, 1, ?2, ?3, '_p~iF~ps|U', ?4, 'path')",
            rusqlite::params![sv2, from_rsv, new_rsv, len1 + 500.0],
        )
        .unwrap();
    }

    let v = get_json(&app, &uri).await;
    let cs = v["corridors"].as_array().unwrap();
    assert_eq!(cs.len(), 2, "500 m gap must split the pair: {v}");
    assert_eq!(cs[0]["routes"][0]["route_id"], serde_json::json!(id1));
    assert_eq!(cs[1]["routes"][0]["route_id"], serde_json::json!(id2));
    assert!(
        cs[0]["max_m"].as_f64().unwrap() < cs[1]["min_m"].as_f64().unwrap(),
        "corridors ascend: {v}"
    );
    assert!(cs[1]["corridor_m"].is_f64(), "{v}");
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

/// Full fixture chain through matcher + segments: road along lon 120.6857,
/// both routes ride it, matcher records the way runs for the direction paths.
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
    rm_registry::bearing::run(&conn, s.batch_id).unwrap();
    (dir, db)
}
#[tokio::test]
async fn route_ways_overlay_trims_to_entry_exit() {
    let (_dir, db) = fixture_db_matched();
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
    let (_dir, db) = fixture_db_matched();
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
    let (_dir, db) = fixture_db_matched();
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
    let (_dir, db) = fixture_db_matched();
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
async fn raw_endpoint_survives_missing_override_table() {
    let (_dir, db) = fixture_db_matched();
    // a DB migrated only through 0012: no polyline_override table yet — the
    // read endpoint degrades to plain src instead of 500ing
    let conn = rm_db::open(&db).unwrap();
    conn.execute("DROP TABLE polyline_override", []).unwrap();
    drop(conn);

    let app = router_with(AppState::new(db));
    let v = get_json(&app, "/api/routes/1/directions/0/raw").await;
    assert_eq!(
        v["effective"]["source"].as_str(),
        Some("src"),
        "missing override table → source src, not 500: {v}"
    );
    assert!(
        v["effective"]["updated_at"].is_null(),
        "no override → no concurrency token: {v}"
    );
    assert!(
        !v["effective"]["points"].as_array().unwrap().is_empty(),
        "effective line falls back to src points: {v}"
    );
}

// ---------- POST /api/edit/stop (add-stop) ----------

/// Edit-mode router over one fixture DB: editor "herry", token "sekrit"
/// (same direction as main.rs parse_tokens "herry:sekrit").
fn edit_app(db: &std::path::Path) -> axum::Router {
    let mut tokens = HashMap::new();
    tokens.insert("herry".to_string(), "sekrit".to_string());
    router_with(AppState::with_edit(db.to_path_buf(), None, tokens))
}

async fn body_json(res: axum::response::Response) -> serde_json::Value {
    let b = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    serde_json::from_slice(&b).unwrap()
}

/// POST JSON with the editor bearer header (`None` = no header).
async fn post_edit(
    app: &axum::Router,
    uri: &str,
    token: Option<&str>,
    body: serde_json::Value,
) -> axum::response::Response {
    let mut b = Request::builder().method("POST").uri(uri);
    if let Some(t) = token {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    app.clone()
        .oneshot(
            b.header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// Every direction's stop count across the routes list, in list order —
/// the plan-must-not-write probe.
async fn direction_stop_counts(app: &axum::Router) -> Vec<i64> {
    let v = get_json(app, "/api/routes").await;
    v["routes"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|r| r["directions"].as_array().unwrap().iter())
        .map(|d| d["stops"].as_i64().unwrap())
        .collect()
}

#[tokio::test]
async fn add_stop_plan_defaults_to_dry_run_and_writes_nothing() {
    let (_dir, db) = fixture_db();
    let app = edit_app(&db);
    let before = direction_stop_counts(&app).await;

    let res = post_edit(
        &app,
        "/api/edit/stop",
        Some("sekrit"),
        json!({ "name": "測試新增站", "lat": 24.141, "lon": 120.6857, "attach": ["1:0"] }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    let atts = v["attachments"]
        .as_array()
        .unwrap_or_else(|| panic!("attachments array: {v}"));
    assert_eq!(atts.len(), 1);
    assert!(v["row_counts"].is_object(), "row_counts present: {v}");
    assert!(v["warnings"].is_array(), "warnings present: {v}");
    assert!(v.get("edit_id").is_none(), "plan carries no edit_id: {v}");

    let after = direction_stop_counts(&app).await;
    assert_eq!(after, before, "plan leg must not write");
}

#[tokio::test]
async fn add_stop_apply_false_is_also_a_plan() {
    let (_dir, db) = fixture_db();
    let app = edit_app(&db);
    let before = direction_stop_counts(&app).await;

    let res = post_edit(
        &app,
        "/api/edit/stop",
        Some("sekrit"),
        json!({
            "name": "測試新增站", "lat": 24.141, "lon": 120.6857,
            "attach": ["1:0"], "apply": false
        }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["attachments"].as_array().unwrap().len(), 1);
    assert!(v["row_counts"].is_object(), "row_counts present: {v}");
    assert!(v.get("edit_id").is_none(), "apply:false is a plan: {v}");

    let after = direction_stop_counts(&app).await;
    assert_eq!(after, before, "apply:false must not write");
}

#[tokio::test]
async fn add_stop_apply_then_undo_restores_stop_counts() {
    let (_dir, db) = fixture_db();
    let app = edit_app(&db);
    let routes = get_json(&app, "/api/routes").await;
    let id1 = routes["routes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["route_code"] == json!("1"))
        .unwrap()["route_id"]
        .as_i64()
        .unwrap();

    let res = post_edit(
        &app,
        "/api/edit/stop",
        Some("sekrit"),
        json!({
            "name": "測試新增站", "lat": 24.141, "lon": 120.6857,
            "attach": ["1:0"], "apply": true, "note": "web add"
        }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    let edit_id = v["edit_id"]
        .as_i64()
        .unwrap_or_else(|| panic!("edit_id present: {v}"));
    assert_eq!(v["station_created"], json!(true));
    assert!(v["stop_id"].is_i64(), "{v}");
    assert!(v["station_id"].is_i64(), "{v}");

    // applied: the direction now serves three stops
    let stops = get_json(&app, &format!("/api/routes/{id1}/directions/0/stops")).await;
    assert_eq!(stops["stops"].as_array().unwrap().len(), 3, "{stops}");

    // undo deletes the add's own rows — back to two
    let res = post_edit(
        &app,
        &format!("/api/edit/undo/{edit_id}"),
        Some("sekrit"),
        json!({ "note": "revert" }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let stops = get_json(&app, &format!("/api/routes/{id1}/directions/0/stops")).await;
    assert_eq!(
        stops["stops"].as_array().unwrap().len(),
        2,
        "undo restores: {stops}"
    );
}

#[tokio::test]
async fn add_stop_rejects_empty_or_bad_attach_and_bad_coords() {
    let (_dir, db) = fixture_db();
    let app = edit_app(&db);

    // empty attach → 400 with the exact message
    let res = post_edit(
        &app,
        "/api/edit/stop",
        Some("sekrit"),
        json!({ "name": "X", "lat": 24.141, "lon": 120.6857, "attach": [] }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let v = body_json(res).await;
    assert_eq!(
        v["error"],
        json!("nothing to attach — give at least one attach XNO:DIR[@CODE]"),
        "{v}"
    );

    // bad attach string → 400 with parse_attach's message
    let res = post_edit(
        &app,
        "/api/edit/stop",
        Some("sekrit"),
        json!({ "name": "X", "lat": 24.141, "lon": 120.6857, "attach": ["1:x"] }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let v = body_json(res).await;
    assert!(
        v["error"].as_str().unwrap().contains("invalid --attach"),
        "{v}"
    );

    // lat/lon out of range → 400 (same convention as the other edit endpoints)
    for body in [
        json!({ "name": "X", "lat": 91.0, "lon": 120.6857, "attach": ["1:0"] }),
        json!({ "name": "X", "lat": 24.141, "lon": 181.0, "attach": ["1:0"] }),
    ] {
        let res = post_edit(&app, "/api/edit/stop", Some("sekrit"), body.clone()).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "body {body}");
    }
}

// ---------- POST /api/edit/stop/detach (remove-stop) ----------

/// Route id for a fixture route_code.
async fn route_id(app: &axum::Router, code: &str) -> i64 {
    let v = get_json(app, "/api/routes").await;
    v["routes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["route_code"] == json!(code))
        .unwrap()["route_id"]
        .as_i64()
        .unwrap()
}

#[tokio::test]
async fn detach_stop_plan_defaults_to_dry_run_and_writes_nothing() {
    let (_dir, db) = fixture_db();
    let app = edit_app(&db);
    let id2 = route_id(&app, "2").await;
    let before = direction_stop_counts(&app).await;

    let res = post_edit(
        &app,
        "/api/edit/stop/detach",
        Some("sekrit"),
        json!({ "xno": "2", "direction": 0, "code": "21" }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    let occ = v["occurrences"]
        .as_array()
        .unwrap_or_else(|| panic!("occurrences array: {v}"));
    assert_eq!(occ.len(), 1);
    assert!(occ[0]["route_stop_version_id"].is_i64(), "{v}");
    // fixture poles are per-occurrence (tcg:xno:dir:sid): the direction's
    // only stop, so the detach orphans the pole
    assert_eq!(v["pole_orphan"], json!(true), "{v}");
    assert_eq!(v["serving_directions"], json!([0]), "{v}");
    assert!(v["warnings"].is_array(), "{v}");
    assert!(v.get("edit_id").is_none(), "plan carries no edit_id: {v}");

    let after = direction_stop_counts(&app).await;
    assert_eq!(after, before, "plan leg must not write");
    let stops = get_json(&app, &format!("/api/routes/{id2}/directions/0/stops")).await;
    assert_eq!(stops["stops"].as_array().unwrap().len(), 1, "{stops}");
}

#[tokio::test]
async fn detach_stop_delete_pole_plan_warns_on_orphan() {
    let (_dir, db) = fixture_db();
    let app = edit_app(&db);

    let res = post_edit(
        &app,
        "/api/edit/stop/detach",
        Some("sekrit"),
        json!({ "xno": "2", "direction": 0, "code": "21", "delete_pole": true }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["pole_orphan"], json!(true), "{v}");
    let w = v["warnings"]
        .as_array()
        .unwrap_or_else(|| panic!("warnings array: {v}"));
    assert!(
        !w.is_empty(),
        "delete_pole plan on an orphan must warn: {v}"
    );
    assert!(
        w.iter()
            .any(|x| x.as_str().unwrap().contains("delete_pole")),
        "one-way teardown named on the plan: {v}"
    );
}

#[tokio::test]
async fn detach_stop_orphan_apply_refused_without_delete_pole() {
    let (_dir, db) = fixture_db();
    let app = edit_app(&db);
    let id2 = route_id(&app, "2").await;

    let res = post_edit(
        &app,
        "/api/edit/stop/detach",
        Some("sekrit"),
        json!({ "xno": "2", "direction": 0, "code": "21", "apply": true }),
    )
    .await;
    // edit_conflict: OrphanPoleRefused asks for a request change → 400
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let v = body_json(res).await;
    assert!(
        v["error"].as_str().unwrap().contains("--delete-pole"),
        "refusal names the flag: {v}"
    );

    // refused apply writes nothing: the stop is still listed
    let stops = get_json(&app, &format!("/api/routes/{id2}/directions/0/stops")).await;
    let arr = stops["stops"].as_array().unwrap();
    assert_eq!(arr.len(), 1, "{stops}");
    assert_eq!(arr[0]["stop_code"], json!("21"), "{stops}");
}

#[tokio::test]
async fn detach_stop_apply_removes_stop_from_direction() {
    let (_dir, db) = fixture_db();
    // seed pole "12" onto route 1 dir 1 as well, so detaching it from dir 0
    // does not orphan the pole (rm_db's shared-pole fixture shape). Explicit
    // out-of-band id + allocator bump: a plain INSERT would take rowid ==
    // id_sequence.next_value and the apply's first mint would collide.
    {
        let conn = rm_db::open(&db).unwrap();
        conn.execute_batch(
            "INSERT INTO route_stop_version
                (route_stop_version_id, subroute_version_id, stop_version_id, direction, stop_sequence, created_at)
             SELECT 9001, sv.subroute_version_id, rsv.stop_version_id, 1, 2, '2026-01-01T00:00:00Z'
             FROM route_stop_version rsv
             JOIN subroute_version sv ON sv.subroute_version_id = rsv.subroute_version_id
             JOIN route_version rv ON rv.route_version_id = sv.route_version_id
             JOIN route r ON r.route_id = rv.route_id
                  AND r.current_route_version_id = rv.route_version_id
             JOIN stop_version s ON s.stop_version_id = rsv.stop_version_id
             WHERE r.ext_uid = 'tcg:1' AND rsv.direction = 0 AND s.stop_code = '12';
             UPDATE id_sequence SET next_value = 9002 WHERE table_name = 'route_stop_version';",
        )
        .unwrap();
    }
    let app = edit_app(&db);
    let id1 = route_id(&app, "1").await;

    let res = post_edit(
        &app,
        "/api/edit/stop/detach",
        Some("sekrit"),
        json!({ "xno": "1", "direction": 0, "code": "12", "apply": true, "note": "web detach" }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert!(v["edit_id"].is_i64(), "{v}");
    assert_eq!(v["pole_deleted"], json!(false), "{v}");
    assert!(
        v["old_route_version_id"].is_i64() && v["new_route_version_id"].is_i64(),
        "{v}"
    );
    assert_eq!(v["stale_routes"], json!([id1]), "{v}");

    // gone from dir 0's stops; dir 1 still serves the pole
    let stops = get_json(&app, &format!("/api/routes/{id1}/directions/0/stops")).await;
    let arr = stops["stops"].as_array().unwrap();
    assert_eq!(arr.len(), 1, "{stops}");
    assert_eq!(arr[0]["stop_code"], json!("11"), "{stops}");
    let stops1 = get_json(&app, &format!("/api/routes/{id1}/directions/1/stops")).await;
    assert_eq!(stops1["stops"].as_array().unwrap().len(), 2, "{stops1}");
}
