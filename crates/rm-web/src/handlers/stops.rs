//! `GET /api/stops/{id}` and `GET /api/stations/{id}` (bbox/search land later).

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::Value;

use crate::error::not_found;
use crate::state::{AppState, run_db};

pub async fn detail(
    Path(stop_version_id): Path<i64>,
    State(state): State<AppState>,
) -> Result<Response, crate::error::ApiError> {
    let v = run_db(state, move |conn| {
        crate::queries::stops::detail(conn, stop_version_id)
    })
    .await?;
    match v {
        Some(v) => Ok(Json(v).into_response()),
        None => Ok(not_found("stop")),
    }
}

pub async fn station_detail(
    Path(station_id): Path<i64>,
    State(state): State<AppState>,
) -> Result<Response, crate::error::ApiError> {
    let v = run_db(state, move |conn| {
        crate::queries::stops::station_detail(conn, station_id)
    })
    .await?;
    match v {
        Some(v) => Ok(Json(v).into_response()),
        None => Ok(not_found("station")),
    }
}
