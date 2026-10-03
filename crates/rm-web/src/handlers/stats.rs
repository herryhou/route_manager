//! `GET /api/stats` — overview counts + match summary + batch info.

use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};

use crate::state::AppState;

/// Scaffold stub — real queries land with the stats task.
pub async fn stats(State(_state): State<AppState>) -> Json<Value> {
    Json(json!({}))
}
