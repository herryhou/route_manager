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
