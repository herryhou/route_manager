//! Apply-updates refresh: state machine, chain runner, endpoints.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt; // oneshot

// --- Task 1: state machine ---

#[test]
fn begin_refresh_then_conflict_then_recover() {
    let dir = tempfile::tempdir().unwrap();
    let state = rm_web::state::AppState::with_edit(
        dir.path().join("t.db"),
        None,
        [("herry".to_string(), "sekrit".to_string())].into(),
    );
    assert_eq!(state.refresh_state(), rm_web::state::RefreshState::Idle);

    state.try_begin_refresh("t0".into()).unwrap();
    match state.refresh_state() {
        rm_web::state::RefreshState::Running { step, started_at } => {
            assert_eq!(step, rm_web::state::RefreshStep::Match);
            assert_eq!(started_at, "t0");
        }
        other => panic!("expected Running, got {other:?}"),
    }

    // single-flight: second begin while running is a 409-shaped conflict
    let err = state.try_begin_refresh("t1".into()).unwrap_err();
    assert!(err.to_string().contains("already running"), "{err}");

    // step rewrite keeps started_at
    state.set_refresh_step(rm_web::state::RefreshStep::Segments);
    match state.refresh_state() {
        rm_web::state::RefreshState::Running { step, started_at } => {
            assert_eq!(step, rm_web::state::RefreshStep::Segments);
            assert_eq!(started_at, "t0");
        }
        other => panic!("expected Running, got {other:?}"),
    }

    // terminal states open the gate again
    state.set_refresh_state(rm_web::state::RefreshState::Done {
        started_at: "t1s".into(),
        finished_at: "t2".into(),
    });
    state.try_begin_refresh("t3".into()).unwrap();
    state.set_refresh_state(rm_web::state::RefreshState::Failed {
        error: "x".into(),
        started_at: "t4s".into(),
        finished_at: "t4".into(),
    });
    state.try_begin_refresh("t5".into()).unwrap();
}

// --- Task 2: chain runner ---

/// Minimal DB for runner tests: schema + one CLOSED batch row. The runner's
/// steps are all no-ops (`None`) when `stale_mark` is empty.
fn runner_db(dir: &tempfile::TempDir) -> rm_web::state::AppState {
    let path = dir.path().join("t.db");
    let conn = rm_db::open(&path).unwrap();
    conn.execute(
        "INSERT INTO import_batch (batch_id, source_dir, file_count, captured_at, valid_from, status, created_at)
         VALUES (1, 'assets/x', 1, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'CLOSED', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    drop(conn);
    rm_web::state::AppState::with_edit(
        path,
        None,
        [("herry".to_string(), "sekrit".to_string())].into(),
    )
}

#[tokio::test]
async fn run_chain_no_marks_ends_done() {
    let dir = tempfile::tempdir().unwrap();
    let state = runner_db(&dir);
    rm_web::refresh::run_chain(state.clone()).await;
    assert!(matches!(
        state.refresh_state(),
        rm_web::state::RefreshState::Done { .. }
    ));
}

#[tokio::test]
async fn run_chain_without_batch_fails_with_message() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    let conn = rm_db::open(&path).unwrap();
    drop(conn); // schema, but NO import_batch row
    let state = rm_web::state::AppState::with_edit(
        path,
        None,
        [("herry".to_string(), "sekrit".to_string())].into(),
    );
    rm_web::refresh::run_chain(state.clone()).await;
    match state.refresh_state() {
        rm_web::state::RefreshState::Failed { error, .. } => {
            assert!(error.contains("no import batch"), "{error}");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[tokio::test]
async fn run_chain_keeps_unconsumable_marks() {
    let dir = tempfile::tempdir().unwrap();
    let state = runner_db(&dir);
    // mark for route 999: unpublished -> the match step warns and KEEPS it
    {
        let conn = rm_db::open(&dir.path().join("t.db")).unwrap();
        // non-empty road graph: rm_osm_match::run_stale builds it before it
        // can classify any mark (same seed as tests/api.rs fixture_db_matched)
        conn.execute_batch(
            "INSERT INTO osm_way (osm_way_id, highway, oneway, name) VALUES (100, 'residential', 0, '測試路');
             INSERT INTO osm_node (osm_node_id, lat, lon) VALUES
                (1, 24.1405, 120.6857), (2, 24.1408, 120.6857), (3, 24.1412, 120.6857);
             INSERT INTO osm_way_node (osm_way_id, ordinal, osm_node_id) VALUES
                (100, 1, 1), (100, 2, 2), (100, 3, 3);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO edit_log (ts, editor, entity, entity_id, action, changes)
             VALUES ('2026-01-01T00:00:00Z', 't', 'polyline', 999, 'create', '{}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO stale_mark VALUES ('seg_match', '999:0', 1, '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
    }
    rm_web::refresh::run_chain(state.clone()).await;
    assert!(matches!(
        state.refresh_state(),
        rm_web::state::RefreshState::Done { .. }
    ));
    let conn = rm_db::open_readonly(&dir.path().join("t.db")).unwrap();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1, "unconsumable mark must survive a Done run");
}

// --- Task 3: endpoints ---

fn authed_req(method: &str, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", "Bearer sekrit")
        .body(Body::empty())
        .unwrap()
}

/// Authed GET /api/edit/refresh (the endpoint sits behind the token
/// middleware, so the request must carry the header).
async fn status_json(app: &axum::Router) -> serde_json::Value {
    let res = app
        .clone()
        .oneshot(authed_req("GET", "/api/edit/refresh"))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let b = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    serde_json::from_slice(&b).unwrap()
}

/// Poll GET /api/edit/refresh until the state leaves `running` (max ~5 s).
async fn settled(app: axum::Router) -> serde_json::Value {
    for _ in 0..100 {
        let v = status_json(&app).await;
        if v["state"] != "running" {
            return v;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("refresh never settled");
}

#[tokio::test]
async fn refresh_requires_token() {
    let dir = tempfile::tempdir().unwrap();
    let state = runner_db(&dir);
    for (method, auth, want) in [
        ("POST", None::<&str>, StatusCode::UNAUTHORIZED),
        ("POST", Some("Bearer nope"), StatusCode::FORBIDDEN),
        ("GET", None::<&str>, StatusCode::UNAUTHORIZED),
        ("GET", Some("Bearer nope"), StatusCode::FORBIDDEN),
    ] {
        let mut req = Request::builder().method(method).uri("/api/edit/refresh");
        if let Some(a) = auth {
            req = req.header("authorization", a);
        }
        let res = rm_web::router_with(state.clone())
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), want, "{method} {auth:?}");
    }
}

#[tokio::test]
async fn refresh_start_runs_to_done() {
    let dir = tempfile::tempdir().unwrap();
    let state = runner_db(&dir);
    let app = rm_web::router_with(state.clone());

    let res = app
        .clone()
        .oneshot(authed_req("POST", "/api/edit/refresh"))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);

    let v = settled(app).await;
    assert_eq!(v["state"], "done");
    assert_eq!(v["pending_marks"], 0);
    assert!(v["started_at"].is_string());
    assert!(v["finished_at"].is_string());
    assert!(v["error"].is_null());
}

#[tokio::test]
async fn refresh_status_shapes_idle_state() {
    let dir = tempfile::tempdir().unwrap();
    let state = runner_db(&dir);
    let app = rm_web::router_with(state.clone());
    let v = status_json(&app).await; // idle snapshot before any POST
    assert_eq!(v["state"], "idle");
    assert_eq!(v["pending_marks"], 0);
    assert!(v["step"].is_null());
}

#[tokio::test]
async fn refresh_routes_absent_without_edit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    let conn = rm_db::open(&path).unwrap();
    drop(conn);
    let state = rm_web::state::AppState::new(path); // no tokens -> edit routes unmounted
    for method in ["POST", "GET"] {
        let res = rm_web::router_with(state.clone())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri("/api/edit/refresh")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{method}");
    }
}
