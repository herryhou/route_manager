//! `GET /api/stats` — overview counts + match summary + batch info.

use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};

use crate::state::{run_db, AppState};

pub async fn stats(State(state): State<AppState>) -> Result<Json<Value>, crate::error::ApiError> {
    let mut v = run_db(state.clone(), crate::queries::qa::stats).await?;
    v["edit_enabled"] = json!(state.edit_enabled());
    Ok(Json(v))
}
