//! `GET /api/qa/outliers` — worst stop projections, snap-desc.
//! `GET /api/qa/matchdev` — worst path-vs-polyline deviations, dev-desc.

use axum::extract::{Query, State};
use axum::Json;
use serde_json::Value;

use crate::error::ApiError;
use crate::state::{run_db, AppState};
use std::collections::HashMap;

pub async fn outliers(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let min_snap = params
        .get("min_snap")
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(50.0);
    let limit = params
        .get("limit")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(200)
        .clamp(1, 1000);
    let v = run_db(state, move |conn| {
        crate::queries::qa::outliers(conn, min_snap, limit)
    })
    .await?;
    Ok(Json(v))
}

/// `GET /api/qa/variants` — station pairs sharing one segment id across
/// path variants, spread-desc.
pub async fn variants(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let min_spread = params
        .get("min_spread")
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(200.0);
    let limit = params
        .get("limit")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(200)
        .clamp(1, 1000);
    let v = run_db(state, move |conn| {
        crate::queries::qa::variants(conn, min_spread, limit)
    })
    .await?;
    Ok(Json(v))
}

/// `GET /api/qa/matchdev` — worst path-vs-polyline deviations, dev-desc.
pub async fn matchdev(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let min_dev = params
        .get("min_dev")
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(50.0);
    let limit = params
        .get("limit")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(200)
        .clamp(1, 1000);
    let v = run_db(state, move |conn| {
        crate::queries::qa::matchdev(conn, min_dev, limit)
    })
    .await?;
    Ok(Json(v))
}
