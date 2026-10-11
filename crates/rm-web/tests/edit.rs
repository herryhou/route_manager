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
    // name → token, same direction as main.rs parse_tokens ("herry:sekrit")
    let mut tokens = HashMap::new();
    tokens.insert("herry".to_string(), "sekrit".to_string());
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
    conn.execute(
        "INSERT INTO import_batch (batch_id, source_dir, file_count, captured_at, valid_from, status, created_at)
         VALUES (1, 'assets/x', 1, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'CLOSED', '2026-01-01T00:00:00Z')", []).unwrap();
    conn.execute(
        "INSERT INTO src_route_point (batch_id, xno, direction, encoded)
         VALUES (1, '1', 1, '_p~iF~ps|U_ulLnnqC')", []).unwrap();
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
    // the bearer TOKEN resolved to the editor NAME (map is name → token)
    let editor: String = conn
        .query_row("SELECT editor FROM edit_log", [], |r| r.get(0))
        .unwrap();
    assert_eq!(editor, "herry");
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
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/edit/polyline/1/1/piece")
                .header("authorization", "Bearer sekrit")
                .header("content-type", "application/json")
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
async fn stop_detail_reports_staleness_after_position_edit() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    let res = rm_web::router_with(state.clone()).oneshot(
        Request::builder().method("POST").uri("/api/edit/stop/10")
            .header("authorization", "Bearer sekrit")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"lat": 24.001, "note": "move"}"#)).unwrap(),
    ).await.unwrap();
    let new_id = body_json(res).await["new_stop_version_id"].as_i64().unwrap();

    let res = rm_web::router_with(state.clone()).oneshot(
        Request::builder().uri(format!("/api/stops/{new_id}")).body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["stale"]["projections"], serde_json::json!([1]));
    assert_eq!(v["stale"]["bearings"], true);

    // simulated refresh clears the marks → stale flags gone
    let conn = rm_db::open(&dir.path().join("t.db")).unwrap();
    conn.execute("DELETE FROM stale_mark", []).unwrap();
    drop(conn);
    let res = rm_web::router_with(state).oneshot(
        Request::builder().uri(format!("/api/stops/{new_id}")).body(Body::empty()).unwrap(),
    ).await.unwrap();
    let v = body_json(res).await;
    assert_eq!(v["stale"]["projections"], serde_json::json!([]));
    assert_eq!(v["stale"]["bearings"], false);
}

#[tokio::test]
async fn second_edit_of_same_version_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    let post = |body: &'static str| {
        rm_web::router_with(state.clone()).oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/edit/stop/10")
                .header("authorization", "Bearer sekrit")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
    };
    let res = post(r#"{"stop_name": "New"}"#).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    // resubmitting the consumed version id (e.g. a second editor working
    // from a stale panel) is a conflict, not a server error
    let res = post(r#"{"stop_name": "Newer"}"#).await.unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let v = body_json(res).await;
    assert!(
        v["error"].as_str().unwrap().contains("not the current version"),
        "{v}"
    );
}

#[tokio::test]
async fn edit_state_keeps_tiles() {
    let dir = tempfile::tempdir().unwrap();
    let conn = rm_db::open(&dir.path().join("t.db")).unwrap();
    fixture(&conn);
    drop(conn);
    let mut tokens = HashMap::new();
    tokens.insert("herry".to_string(), "sekrit".to_string());
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

/// Second station in group 1 — a regroup target distinct from stop 10's
/// current station 1.
fn second_station(conn: &Connection) {
    conn.execute(
        "INSERT INTO station (station_id, ext_uid, created_at) VALUES (2, 'tcg:node:2', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO station_version (station_version_id, station_id, station_group_id, station_name, latitude, longitude, valid_from, created_at)
         VALUES (2, 2, 1, 'T', 24.0, 120.0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
}

async fn regroup(state: &rm_web::state::AppState, body: &str) -> axum::response::Response {
    rm_web::router_with(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/edit/stop/10/regroup")
                .header("authorization", "Bearer sekrit")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn regroup_happy_path_mints_version_and_move_station_log() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    {
        let conn = rm_db::open(&dir.path().join("t.db")).unwrap();
        second_station(&conn);
    }
    let res = regroup(&state, r#"{"new_station_id": 2, "note": "fix"}"#).await;
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert!(v["new_stop_version_id"].as_i64().unwrap() > 10);
    assert!(v["edit_id"].as_i64().is_some());
    assert!(v.get("stale_routes").is_none(), "{v}");
    // readback: stop now belongs to station 2 as a manual version, and the
    // log row records the membership move
    let conn = rm_db::open_readonly(&dir.path().join("t.db")).unwrap();
    let (station_id, src): (i64, String) = conn
        .query_row(
            "SELECT station_id, source FROM stop_version WHERE valid_to IS NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((station_id, src), (2, "manual".to_string()));
    let action: String = conn
        .query_row("SELECT action FROM edit_log", [], |r| r.get(0))
        .unwrap();
    assert_eq!(action, "move_station");
}

#[tokio::test]
async fn regroup_rejects_unknown_target() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    let res = regroup(&state, r#"{"new_station_id": 999, "note": "x"}"#).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn regroup_rejects_same_station() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    let res = regroup(&state, r#"{"new_station_id": 1, "note": "x"}"#).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn regroup_validates_body() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    for body in ["{}", r#"{"new_station_id": "x"}"#] {
        let res = regroup(&state, body).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "body {body}");
    }
}

#[tokio::test]
async fn regroup_stale_svi_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    {
        let conn = rm_db::open(&dir.path().join("t.db")).unwrap();
        second_station(&conn);
    }
    let res = regroup(&state, r#"{"new_station_id": 2, "note": "first"}"#).await;
    assert_eq!(res.status(), StatusCode::OK);
    // the consumed version id is gone: a second editor's stale panel conflicts
    let res = regroup(&state, r#"{"new_station_id": 2, "note": "again"}"#).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn station_detail_exposes_station_version_id() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    let app = rm_web::router_with(state);
    let v = get_json(&app, "/api/stations/1").await;
    assert_eq!(v["station"]["station_version_id"].as_i64(), Some(1), "{v}");
}

/// The station_version id_sequence seed station edits mint from
/// (edit_state's fixture only seeds the stop_version sequence).
fn station_fixture(conn: &Connection) {
    conn.execute(
        "INSERT INTO id_sequence (table_name, next_value) VALUES ('station_version', 2)",
        [],
    )
    .unwrap();
}

async fn rename_st(state: &rm_web::state::AppState, svi: i64, body: &str) -> axum::response::Response {
    rm_web::router_with(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&format!("/api/edit/station/{svi}"))
                .header("authorization", "Bearer sekrit")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn rename_all_poles_mints_station_and_pole_versions() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    {
        let conn = rm_db::open(&dir.path().join("t.db")).unwrap();
        station_fixture(&conn);
    }
    let res = rename_st(&state, 1, r#"{"new_name": "N", "level": "all", "note": "r"}"#).await;
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["new_station_version_id"].as_i64(), Some(2), "{v}");
    let conn = rm_db::open_readonly(&dir.path().join("t.db")).unwrap();
    // station: new manual version is current, named N
    let (name, src): (String, String) = conn
        .query_row(
            "SELECT station_name, source FROM station_version WHERE valid_to IS NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((name.as_str(), src.as_str()), ("N", "manual"));
    // pole followed the rename (minted version 11 is now current)
    let pole: String = conn
        .query_row(
            "SELECT stop_name FROM stop_version WHERE stop_id = 1 AND valid_to IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pole, "N");
    // log row carries the rename kind + level
    let (kind, level): (String, String) = conn
        .query_row(
            "SELECT json_extract(changes, '$.kind'), json_extract(changes, '$.level') FROM edit_log",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((kind.as_str(), level.as_str()), ("station_rename", "AllPoles"));
}

#[tokio::test]
async fn rename_level_maps_to_pole_outcomes() {
    for (level, want_pole) in [("station", "Old"), ("matching", "Old"), ("all", "N")] {
        let dir = tempfile::tempdir().unwrap();
        let state = edit_state(&dir);
        {
            let conn = rm_db::open(&dir.path().join("t.db")).unwrap();
            station_fixture(&conn);
        }
        let res = rename_st(
            &state,
            1,
            &format!(r#"{{"new_name": "N", "level": "{level}", "note": "x"}}"#),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK, "level {level}");
        drop(res);
        let conn = rm_db::open_readonly(&dir.path().join("t.db")).unwrap();
        let pole: String = conn
            .query_row(
                "SELECT stop_name FROM stop_version WHERE stop_id = 1 AND valid_to IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pole, want_pole, "level {level}");
    }
}

#[tokio::test]
async fn rename_rejects_bad_level_empty_and_noop() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    for (body, want) in [
        (r#"{"new_name": "N", "level": "poles", "note": "x"}"#, StatusCode::BAD_REQUEST),
        (r#"{"new_name": "  ", "level": "all", "note": "x"}"#, StatusCode::BAD_REQUEST),
        (r#"{"new_name": "S", "level": "all", "note": "x"}"#, StatusCode::BAD_REQUEST),
        (r#"{"level": "all"}"#, StatusCode::BAD_REQUEST),
    ] {
        let res = rename_st(&state, 1, body).await;
        assert_eq!(res.status(), want, "body {body}");
    }
    // nothing was written
    let conn = rm_db::open_readonly(&dir.path().join("t.db")).unwrap();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM edit_log", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn rename_stale_svi_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    let res = rename_st(&state, 999, r#"{"new_name": "N", "level": "all", "note": "x"}"#).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
}

async fn move_st(state: &rm_web::state::AppState, svi: i64, body: &str) -> axum::response::Response {
    rm_web::router_with(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&format!("/api/edit/station/{svi}/move"))
                .header("authorization", "Bearer sekrit")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn move_station_mints_version_zero_marks_and_leaves_poles() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    {
        let conn = rm_db::open(&dir.path().join("t.db")).unwrap();
        station_fixture(&conn);
    }
    let res = move_st(&state, 1, r#"{"lat": 24.001, "lon": 120.001, "note": "m"}"#).await;
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["new_station_version_id"].as_i64(), Some(2), "{v}");
    let conn = rm_db::open_readonly(&dir.path().join("t.db")).unwrap();
    let (lat, lon, src): (f64, f64, String) = conn
        .query_row(
            "SELECT latitude, longitude, source FROM station_version WHERE valid_to IS NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!((lat, lon, src), (24.001, 120.001, "manual".to_string()));
    // poles stay put — the pole's current version is untouched
    let (plat, plon): (f64, f64) = conn
        .query_row(
            "SELECT latitude, longitude FROM stop_version WHERE stop_id = 1 AND valid_to IS NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((plat, plon), (24.0, 120.0));
    // station coords derive nothing: ZERO stale marks
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
    // log row: move kind + coord old/new pairs
    let (kind, lat_new): (String, f64) = conn
        .query_row(
            "SELECT json_extract(changes, '$.kind'), json_extract(changes, '$.latitude.new') FROM edit_log",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((kind.as_str(), lat_new), ("station_move", 24.001));
}

#[tokio::test]
async fn move_station_validates_body_and_stale() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    for (body, want) in [
        (r#"{"lat": 91.0, "lon": 120.0, "note": "x"}"#, StatusCode::BAD_REQUEST),
        (r#"{"lat": 24.0, "lon": -181.0, "note": "x"}"#, StatusCode::BAD_REQUEST),
        (r#"{"lat": 24.0, "note": "x"}"#, StatusCode::BAD_REQUEST),
    ] {
        let res = move_st(&state, 1, body).await;
        assert_eq!(res.status(), want, "body {body}");
    }
    let res = move_st(&state, 999, r#"{"lat": 24.0, "lon": 120.0, "note": "x"}"#).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
}

async fn undo_edit(state: &rm_web::state::AppState, edit_id: i64) -> axum::response::Response {
    rm_web::router_with(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&format!("/api/edit/undo/{edit_id}"))
                .header("authorization", "Bearer sekrit")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"note": "rev"}"#.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn undo_route_dispatches_station_rename_rows() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    {
        let conn = rm_db::open(&dir.path().join("t.db")).unwrap();
        station_fixture(&conn);
    }
    let res = rename_st(&state, 1, r#"{"new_name": "N", "level": "all", "note": "r"}"#).await;
    assert_eq!(res.status(), StatusCode::OK);
    let edit_id = body_json(res).await["edit_id"].as_i64().unwrap();

    let res = undo_edit(&state, edit_id).await;
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["restored_station_version_id"].as_i64(), Some(1), "{v}");
    // readback: station 1 current again named S, pole 10 current named Old
    let conn = rm_db::open_readonly(&dir.path().join("t.db")).unwrap();
    let (name, cur): (String, i64) = conn
        .query_row(
            "SELECT station_name, station_version_id FROM station_version WHERE valid_to IS NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((name.as_str(), cur), ("S", 1));
    let pole: String = conn
        .query_row(
            "SELECT stop_name FROM stop_version WHERE valid_to IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pole, "Old");
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM edit_log WHERE action = 'undo'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

#[tokio::test]
async fn undo_unknown_edit_still_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    let res = undo_edit(&state, 999).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
}

fn piece_body() -> serde_json::Value {
    serde_json::json!({
        "points": [[38.5, -120.2], [40.7, -120.95]],
        "anchor_start": [38.5, -120.2],
        "anchor_end": [40.7, -120.95],
        "note": "fix loop"
    })
}

#[tokio::test]
async fn piece_post_happy_path_and_raw_readback() {
    let dir = tempfile::tempdir().unwrap();
    let app = rm_web::router_with(edit_state(&dir));
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/edit/polyline/1/1/piece")
                .header("authorization", "Bearer sekrit")
                .header("content-type", "application/json")
                .body(Body::from(piece_body().to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["created"], true);
    assert_eq!(v["piece_id"], 1);
    // raw endpoint exposes the manual piece decoded
    let raw = get_json(&app, "/api/routes/1/directions/1/raw").await;
    assert_eq!(raw["manual_pieces"][0]["piece_id"], 1);
    assert_eq!(raw["manual_pieces"][0]["points"][0], serde_json::json!([38.5, -120.2]));
}

#[tokio::test]
async fn piece_post_requires_auth_and_valid_payload() {
    let dir = tempfile::tempdir().unwrap();
    let app = rm_web::router_with(edit_state(&dir));
    let post = |app: &axum::Router, uri: &str, body: serde_json::Value, auth: Option<&str>| {
        let mut b = Request::builder().method("POST").uri(uri)
            .header("content-type", "application/json");
        if let Some(a) = auth {
            b = b.header("authorization", a);
        }
        app.clone().oneshot(b.body(Body::from(body.to_string())).unwrap())
    };
    // 401 absent token
    let res = post(&app, "/api/edit/polyline/1/1/piece", piece_body(), None).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    // 403 bad token
    let res = post(&app, "/api/edit/polyline/1/1/piece", piece_body(), Some("Bearer nope")).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    // 400 single point
    let mut bad = piece_body();
    bad["points"] = serde_json::json!([[38.5, -120.2]]);
    let res = post(&app, "/api/edit/polyline/1/1/piece", bad, Some("Bearer sekrit")).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    // 400 unknown route
    let res = post(&app, "/api/edit/polyline/999/1/piece", piece_body(), Some("Bearer sekrit")).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    // 400 piece_id scoped to another direction: create under dir 1, replay
    // the id under dir 2 (no expectation) → PieceScope, never silent reuse
    let res = post(&app, "/api/edit/polyline/1/1/piece", piece_body(), Some("Bearer sekrit")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let created = body_json(res).await;
    let mut foreign = piece_body();
    foreign["piece_id"] = serde_json::json!(created["piece_id"]);
    let res = post(&app, "/api/edit/polyline/1/2/piece", foreign, Some("Bearer sekrit")).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn piece_post_stale_expectation_409() {
    let dir = tempfile::tempdir().unwrap();
    let app = rm_web::router_with(edit_state(&dir));
    let auth = |req: axum::http::request::Builder| req.header("authorization", "Bearer sekrit").header("content-type", "application/json");
    let res = app.clone().oneshot(auth(Request::builder().method("POST").uri("/api/edit/polyline/1/1/piece")).body(Body::from(piece_body().to_string())).unwrap()).await.unwrap();
    let first = body_json(res).await;
    // second POST with the SAME (now-stale) expectation → 409
    let mut body = piece_body();
    body["piece_id"] = serde_json::json!(first["piece_id"]);
    body["expected_updated_at"] = serde_json::json!("2020-01-01T00:00:00Z");
    let res = app.oneshot(auth(Request::builder().method("POST").uri("/api/edit/polyline/1/1/piece")).body(Body::from(body.to_string())).unwrap()).await.unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn piece_post_overlapping_span_409() {
    let dir = tempfile::tempdir().unwrap();
    let app = rm_web::router_with(edit_state(&dir));
    let auth = |req: axum::http::request::Builder| req.header("authorization", "Bearer sekrit").header("content-type", "application/json");
    // A→B succeeds
    let res = app.clone().oneshot(auth(Request::builder().method("POST").uri("/api/edit/polyline/1/1/piece")).body(Body::from(piece_body().to_string())).unwrap()).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    // strictly inside the A→B span, distinct endpoints on the src line
    // (quarter/three-quarter points of (38.5,-120.2)→(40.7,-120.95)) → 409
    let mut inner = piece_body();
    inner["points"] = serde_json::json!([[39.05, -120.3875], [40.15, -120.7625]]);
    inner["anchor_start"] = serde_json::json!([39.05, -120.3875]);
    inner["anchor_end"] = serde_json::json!([40.15, -120.7625]);
    let res = app.oneshot(auth(Request::builder().method("POST").uri("/api/edit/polyline/1/1/piece")).body(Body::from(inner.to_string())).unwrap()).await.unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let v = body_json(res).await;
    assert!(v["error"].as_str().unwrap().contains("overlaps"), "{v}");
}

#[tokio::test]
async fn piece_undo_via_route_deletes_piece() {
    let dir = tempfile::tempdir().unwrap();
    let app = rm_web::router_with(edit_state(&dir));
    let auth = |req: axum::http::request::Builder| req.header("authorization", "Bearer sekrit").header("content-type", "application/json");
    let res = app.clone().oneshot(auth(Request::builder().method("POST").uri("/api/edit/polyline/1/1/piece")).body(Body::from(piece_body().to_string())).unwrap()).await.unwrap();
    let first = body_json(res).await;
    let res = app
        .clone()
        .oneshot(
            Request::builder().method("POST").uri(format!("/api/edit/undo/{}", first["edit_id"]))
                .header("authorization", "Bearer sekrit")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"note":"rev"}"#.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["restored"]["piece_id"], first["piece_id"]);
    assert_eq!(v["restored"]["deleted"], true);
    // readback: no manual pieces left
    let raw = get_json(&app, "/api/routes/1/directions/1/raw").await;
    assert_eq!(raw["manual_pieces"].as_array().map(Vec::len), Some(0));
}
