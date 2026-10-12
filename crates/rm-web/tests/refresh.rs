//! Apply-updates refresh: state machine, chain runner, endpoints.

use axum::http::{Request, StatusCode};
use axum::body::Body;
use rusqlite::Connection;
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
    state.set_refresh_state(rm_web::state::RefreshState::Done { finished_at: "t2".into() });
    state.try_begin_refresh("t3".into()).unwrap();
    state.set_refresh_state(rm_web::state::RefreshState::Failed {
        error: "x".into(),
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
