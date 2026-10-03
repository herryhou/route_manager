//! `GET /api/stats` — overview counts + match summary + batch info.

use axum::extract::State;
use axum::Json;
use serde_json::Value;

use crate::state::{AppState, run_db};

pub async fn stats(State(state): State<AppState>) -> Result<Json<Value>, crate::error::ApiError> {
    let v = run_db(state, crate::queries::qa::stats).await?;
    Ok(Json(v))
}
