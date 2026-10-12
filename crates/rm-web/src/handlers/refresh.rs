//! POST /api/edit/refresh — begin the apply-updates chain (single-flight).
//! GET  /api/edit/refresh — lifecycle state + pending mark count.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use crate::error::ApiError;
use crate::handlers::edit::Editor;
use crate::state::{run_db, AppState, RefreshState};

/// Begin one refresh run. 202 immediately; the chain runs on the blocking
/// pool and updates AppState. Second POST while running: 409.
pub async fn start(
    State(state): State<AppState>,
    Editor(_editor): Editor,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    state.try_begin_refresh(rm_db::now_rfc3339())?;
    tokio::spawn(crate::refresh::run_chain(state.clone()));
    Ok((StatusCode::ACCEPTED, Json(json!({}))))
}

/// Status snapshot for the UI button (polled every ~2 s while running).
pub async fn status(State(state): State<AppState>, Editor(_editor): Editor) -> Result<Json<Value>, ApiError> {
    let pending: i64 = run_db(state.clone(), |conn| {
        conn.query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0))
            .map_err(Into::into)
    })
    .await?;
    let mut v = match state.refresh_state() {
        RefreshState::Idle => json!({ "state": "idle" }),
        RefreshState::Running { step, started_at } => json!({
            "state": "running", "step": step.as_str(), "started_at": started_at,
        }),
        RefreshState::Done { started_at, finished_at } => json!({
            "state": "done", "started_at": started_at, "finished_at": finished_at,
        }),
        RefreshState::Failed { error, started_at, finished_at } => json!({
            "state": "failed", "error": error, "started_at": started_at, "finished_at": finished_at,
        }),
    };
    v["pending_marks"] = json!(pending);
    Ok(Json(v))
}
