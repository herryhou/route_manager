//! Edit-mode integration: auth gating, stop edit endpoint, readback.

use std::collections::HashMap;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use rusqlite::Connection;
use tower::ServiceExt; // oneshot

fn edit_state(dir: &tempfile::TempDir) -> rm_web::state::AppState {
    let conn = rm_db::open(&dir.path().join("t.db")).unwrap();
    fixture(&conn);
    drop(conn);
    let mut tokens = HashMap::new();
    tokens.insert("sekrit".to_string(), "herry".to_string());
    rm_web::state::AppState::with_edit(dir.path().join("t.db"), None, tokens)
}

async fn body_json(res: axum::response::Response) -> serde_json::Value {
    let b = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    serde_json::from_slice(&b).unwrap()
}

async fn get_json(app: &axum::Router, uri: &str) -> serde_json::Value {
    let res = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "GET {uri}");
    body_json(res).await
}

/// Same table chain as rm-db's edit tests: station_group → station → stop →
/// stop_version 10 → route chain → route_stop_version (route 1 serves the
/// stop), plus the id_sequence seed so mint yields 11.
fn fixture(conn: &Connection) -> i64 {
    conn.execute(
        "INSERT INTO station_group (station_group_id, created_at) VALUES (1, '2026-01-01T00:00:00Z')", []).unwrap();
    conn.execute(
        "INSERT INTO station_group_version (station_group_version_id, station_group_id, station_group_name, source, valid_from, created_at)
         VALUES (1, 1, 'g', 'capture', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')", []).unwrap();
    conn.execute(
        "INSERT INTO station (station_id, ext_uid, created_at) VALUES (1, 'tcg:node:1', '2026-01-01T00:00:00Z')", []).unwrap();
    conn.execute(
        "INSERT INTO station_version (station_version_id, station_id, station_group_id, station_name, latitude, longitude, valid_from, created_at)
         VALUES (1, 1, 1, 'S', 24.0, 120.0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')", []).unwrap();
    conn.execute(
        "INSERT INTO stop (stop_id, created_at) VALUES (1, '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO stop_version (stop_version_id, stop_id, station_id, stop_code, stop_name, direction, latitude, longitude, valid_from, created_at)
         VALUES (10, 1, 1, '001', 'Old', 0, 24.0, 120.0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')", []).unwrap();
    conn.execute(
        "INSERT INTO route (route_id, route_code, ext_uid, created_at, updated_at)
         VALUES (1, '1', 'tcg:1', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO route_version (route_version_id, route_id, version_no, valid_from, status, created_at)
         VALUES (1, 1, 1, '2026-01-01T00:00:00Z', 'PUBLISHED', '2026-01-01T00:00:00Z')", []).unwrap();
    conn.execute(
        "INSERT INTO subroute (subroute_id, route_id, subroute_code, created_at)
         VALUES (1, 1, '1-0', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO subroute_version (subroute_version_id, route_version_id, subroute_id, subroute_name, created_at)
         VALUES (1, 1, 1, '1', '2026-01-01T00:00:00Z')", []).unwrap();
    conn.execute(
        "INSERT INTO route_stop_version (route_stop_version_id, subroute_version_id, stop_version_id, direction, stop_sequence, created_at)
         VALUES (1, 1, 10, 0, 1, '2026-01-01T00:00:00Z')", []).unwrap();
    conn.execute(
        "INSERT INTO id_sequence (table_name, next_value) VALUES ('stop_version', 11)",
        [],
    )
    .unwrap();
    10
}

#[tokio::test]
async fn stop_edit_requires_token() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    for (auth, want) in [
        (None, StatusCode::UNAUTHORIZED),
        (Some("Bearer nope"), StatusCode::FORBIDDEN),
    ] {
        let mut req = Request::builder()
            .method("POST")
            .uri("/api/edit/stop/10")
            .header("content-type", "application/json");
        if let Some(a) = auth {
            req = req.header("authorization", a);
        }
        let res = rm_web::router_with(state.clone())
            .oneshot(req.body(Body::from("{}")).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), want);
    }
    // no partial writes
    let conn = rm_db::open_readonly(&dir.path().join("t.db")).unwrap();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM edit_log", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn stop_edit_happy_path_writes_version_and_marks() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    let app = rm_web::router_with(state);
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/edit/stop/10")
                .header("authorization", "Bearer sekrit")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"lat": 24.001, "stop_name": "New", "note": "fix"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert!(v["new_stop_version_id"].as_i64().unwrap() > 10);
    // readback: stats reports edit_enabled; stop detail is served
    let conn = rm_db::open_readonly(&dir.path().join("t.db")).unwrap();
    let src: String = conn
        .query_row(
            "SELECT source FROM stop_version WHERE valid_to IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(src, "manual");
    let stats = get_json(&app, "/api/stats").await;
    assert_eq!(stats["edit_enabled"], serde_json::Value::Bool(true));
}

#[tokio::test]
async fn stop_edit_validates_body() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    for body in ["{}", r#"{"lat": 999}"#, r#"{"lat": "x"}"#] {
        let res = rm_web::router_with(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/edit/stop/10")
                    .header("authorization", "Bearer sekrit")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "body {body}");
    }
}

#[tokio::test]
async fn edit_routes_absent_without_edit_flag() {
    let dir = tempfile::tempdir().unwrap();
    let conn = rm_db::open(&dir.path().join("t.db")).unwrap();
    fixture(&conn);
    drop(conn);
    let state = rm_web::state::AppState::with_tiles(dir.path().join("t.db"), None);
    let app = rm_web::router_with(state);
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/edit/stop/10")
                .header("authorization", "Bearer sekrit")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let v = get_json(&app, "/api/stats").await;
    assert_eq!(v["edit_enabled"], serde_json::Value::Bool(false));
}

#[tokio::test]
async fn log_and_undo_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    // edit stop 10 (name only — no stale marks needed for this test)
    let res = rm_web::router_with(state.clone()).oneshot(
        Request::builder().method("POST").uri("/api/edit/stop/10")
            .header("authorization", "Bearer sekrit")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"stop_name": "New", "note": "fix"}"#)).unwrap(),
    ).await.unwrap();
    let v = body_json(res).await;
    let edit_id = v["edit_id"].as_i64().unwrap();

    // log lists it
    let res = rm_web::router_with(state.clone()).oneshot(
        Request::builder().uri("/api/edit/log?entity=stop&entity_id=1").body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["rows"].as_array().unwrap().len(), 1);

    // undo restores, requires token
    let res = rm_web::router_with(state).oneshot(
        Request::builder().method("POST").uri(format!("/api/edit/undo/{edit_id}"))
            .header("authorization", "Bearer sekrit")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"note": "revert"}"#)).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["new_stop_version_id"].as_i64().unwrap(), 10);
}

#[tokio::test]
async fn edit_state_keeps_tiles() {
    let dir = tempfile::tempdir().unwrap();
    let conn = rm_db::open(&dir.path().join("t.db")).unwrap();
    fixture(&conn);
    drop(conn);
    let mut tokens = HashMap::new();
    tokens.insert("sekrit".to_string(), "herry".to_string());
    let state = rm_web::state::AppState::with_edit(
        dir.path().join("t.db"),
        Some("https://tiles.internal/{z}/{x}/{y}.png".to_string()),
        tokens,
    );
    assert_eq!(
        state.tiles(),
        Some("https://tiles.internal/{z}/{x}/{y}.png")
    );
    assert!(state.edit_enabled());
}
