//! Integration tests for POST /api/edit/polyline/{xno}/{direction}/extend,
//! driven through the router. Graph fixtures (line_db / graphless_db and
//! their seeds) are copied verbatim from rm-job's extend tests — same
//! walk, same DB shape.

use std::collections::HashMap;
use std::path::PathBuf;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use rm_core::{polyline, LatLng};
use rm_web::router_with;
use rm_web::state::AppState;
use rusqlite::Connection;
use serde_json::{json, Value};
use tower::ServiceExt;

const T0: &str = "2026-01-01T00:00:00Z";

/// read_cmds.rs's chain (station_group → station → stop_version → route →
/// route_version → subroute_version → route_stop_version) plus the
/// id_sequence seeds and batch 1 CLOSED; the src polyline for dir 0 is the
/// caller's geometry (encoded).
fn seed_route(conn: &Connection, dir0_encoded: &str) {
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
             VALUES (1, '1', 0, '{dir0_encoded}');"
    ))
    .unwrap();
}

/// A straight N-S way (100) of four nodes at 0.0001-deg lat steps (~11.1 m
/// per segment), INSERT style mirroring rm-db matched_path's test setup.
/// The main polyline's last vertex sits exactly on node 1.
fn seed_line_graph(conn: &Connection) {
    conn.execute_batch(
        "INSERT INTO osm_way (osm_way_id, highway, oneway, name)
             VALUES (100, 'residential', 0, '直線路');
         INSERT INTO osm_node (osm_node_id, lat, lon) VALUES
            (1, 24.1400, 120.6800),
            (2, 24.1401, 120.6800),
            (3, 24.1402, 120.6800),
            (4, 24.1403, 120.6800);
         INSERT INTO osm_way_node (osm_way_id, ordinal, osm_node_id) VALUES
            (100, 1, 1), (100, 2, 2), (100, 3, 3), (100, 4, 4);",
    )
    .unwrap();
}

/// The main fixture: route + a src polyline (24.1399,120.68) → node 1 of
/// the straight way, so the "last" end extends due north along way 100.
fn line_db() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let conn = rm_db::open(&db).unwrap();
    let encoded = polyline::encode(&[
        LatLng::new(24.1399, 120.6800),
        LatLng::new(24.1400, 120.6800),
    ]);
    seed_route(&conn, &encoded);
    seed_line_graph(&conn);
    (dir, db)
}

/// Route + src polyline but zero osm rows — `RoadGraph::from_conn`'s own
/// "run rm-osm-prepare" error must surface verbatim.
fn graphless_db() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let conn = rm_db::open(&db).unwrap();
    let encoded = polyline::encode(&[
        LatLng::new(24.1399, 120.6800),
        LatLng::new(24.1400, 120.6800),
    ]);
    seed_route(&conn, &encoded);
    (dir, db)
}

// ---- web harness, same shape as tests/api.rs ----

/// Edit-mode router over one fixture DB: editor "herry", token "sekrit".
fn edit_app(db: &std::path::Path) -> axum::Router {
    let mut tokens = HashMap::new();
    tokens.insert("herry".to_string(), "sekrit".to_string());
    router_with(AppState::with_edit(db.to_path_buf(), None, tokens))
}

async fn body_json(res: axum::response::Response) -> Value {
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
    body: Value,
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

/// GET /api/routes/1/directions/0/raw — the fixture route's staging +
/// effective line snapshot.
async fn get_raw(app: &axum::Router) -> Value {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/routes/1/directions/0/raw")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    body_json(res).await
}

const EXTEND_URI: &str = "/api/edit/polyline/1/0/extend";

#[tokio::test]
async fn extend_plan_default_is_dry_run() {
    let (_dir, db) = line_db();
    let app = edit_app(&db);

    let res = post_edit(
        &app,
        EXTEND_URI,
        Some("sekrit"),
        json!({ "meters": 10.0, "end": "last" }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["source"], json!("src"));
    assert!(
        !v["source"].as_str().unwrap().is_empty(),
        "source non-empty: {v}"
    );
    let exts = v["extensions"].as_array().unwrap();
    assert_eq!(exts.len(), 1, "{v}");
    assert!(
        !exts[0]["new_vertices"].as_array().unwrap().is_empty(),
        "walk found vertices: {v}"
    );
    assert!(v["candidate"].as_array().unwrap().len() >= 2, "{v}");
    assert!(v["logs"].is_array(), "{v}");
    assert!(v.get("edit_id").is_none(), "plan carries no edit_id: {v}");

    // dry run wrote nothing: effective line is still plain src
    let raw = get_raw(&app).await;
    assert_eq!(raw["effective"]["source"], json!("src"), "{raw}");
}

#[tokio::test]
async fn extend_apply_then_undo_roundtrips() {
    let (_dir, db) = line_db();
    let app = edit_app(&db);
    let before = get_raw(&app).await;
    let orig = before["effective"]["points"].as_array().unwrap().clone();

    let res = post_edit(
        &app,
        EXTEND_URI,
        Some("sekrit"),
        json!({ "meters": 10.0, "end": "last", "apply": true, "note": "web extend" }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    let edit_id = v["edit_id"]
        .as_i64()
        .unwrap_or_else(|| panic!("edit_id present: {v}"));
    assert_eq!(v["created"], json!(true), "{}", v);
    assert!(v["updated_at"].is_string(), "{v}");
    let last_new = v["extensions"][0]["new_vertices"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();

    // the stored override IS the extended candidate: original prefix kept,
    // last vertex == the walk's last new vertex (1e-5 encode grid)
    let raw = get_raw(&app).await;
    assert_eq!(raw["effective"]["source"], json!("override"), "{raw}");
    assert!(raw["effective"]["updated_at"].is_string(), "{raw}");
    let pts = raw["effective"]["points"].as_array().unwrap();
    assert!(pts.len() > orig.len(), "extension appended: {raw}");
    assert_eq!(&pts[..orig.len()], &orig[..], "original vertices kept");
    let last = pts.last().unwrap();
    assert!(
        (last[0].as_f64().unwrap() - last_new[0].as_f64().unwrap()).abs() < 1e-4
            && (last[1].as_f64().unwrap() - last_new[1].as_f64().unwrap()).abs() < 1e-4,
        "raw end {last} == candidate end {last_new}"
    );

    // undo restores the src polyline
    let res = post_edit(
        &app,
        &format!("/api/edit/undo/{edit_id}"),
        Some("sekrit"),
        json!({ "note": "revert" }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let u = body_json(res).await;
    assert_eq!(u["kind"], json!("polyline"), "{u}");
    let raw = get_raw(&app).await;
    assert_eq!(raw["effective"]["source"], json!("src"), "{raw}");
    assert_eq!(raw["effective"]["points"].as_array().unwrap(), &orig[..]);
}

#[tokio::test]
async fn extend_far_endpoint_is_a_400_naming_the_snap_limit() {
    let (_dir, db) = line_db();
    {
        // dir 1 polyline ends ~2.2 km north of the graph
        let conn = rm_db::open(&db).unwrap();
        conn.execute(
            "INSERT INTO src_route_point (batch_id, xno, direction, encoded)
             VALUES (1, '1', 1, ?1)",
            [polyline::encode(&[
                LatLng::new(24.1399, 120.6800),
                LatLng::new(24.1600, 120.6800),
            ])],
        )
        .unwrap();
    }
    let app = edit_app(&db);
    let res = post_edit(
        &app,
        "/api/edit/polyline/1/1/extend",
        Some("sekrit"),
        json!({ "meters": 10.0, "end": "last" }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let v = body_json(res).await;
    assert!(
        v["error"].as_str().unwrap().contains("50"),
        "names the snap limit: {v}"
    );
}

#[tokio::test]
async fn extend_without_osm_tables_is_a_400_naming_rm_osm_prepare() {
    let (_dir, db) = graphless_db();
    let app = edit_app(&db);
    let res = post_edit(
        &app,
        EXTEND_URI,
        Some("sekrit"),
        json!({ "meters": 10.0, "end": "last" }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let v = body_json(res).await;
    assert!(
        v["error"].as_str().unwrap().contains("rm-osm-prepare"),
        "prepare hint surfaced verbatim: {v}"
    );
}

#[tokio::test]
async fn extend_rejects_invalid_end_and_nonpositive_meters() {
    let (_dir, db) = line_db();
    let app = edit_app(&db);
    let res = post_edit(
        &app,
        EXTEND_URI,
        Some("sekrit"),
        json!({ "meters": 10.0, "end": "middle" }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let res = post_edit(
        &app,
        EXTEND_URI,
        Some("sekrit"),
        json!({ "meters": 0.0, "end": "last" }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}
