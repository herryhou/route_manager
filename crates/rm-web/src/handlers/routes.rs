//! `GET /api/routes` and `GET /api/routes/{id}`.

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::Value;

use crate::error::not_found;
use crate::state::{run_db, AppState};

#[derive(Deserialize)]
pub struct ListParams {
    #[serde(default)]
    q: String,
    limit: Option<i64>,
}

pub async fn list(
    Query(p): Query<ListParams>,
    State(state): State<AppState>,
) -> Result<Json<Value>, crate::error::ApiError> {
    let limit = p.limit.unwrap_or(500).clamp(1, 5000);
    let v = run_db(state, move |conn| {
        crate::queries::routes::list(conn, &p.q, limit)
    })
    .await?;
    Ok(Json(v))
}

pub async fn detail(
    Path(route_id): Path<i64>,
    State(state): State<AppState>,
) -> Result<Response, crate::error::ApiError> {
    let v = run_db(state, move |conn| {
        crate::queries::routes::detail(conn, route_id)
    })
    .await?;
    match v {
        Some(v) => Ok(Json(v).into_response()),
        None => Ok(not_found("route")),
    }
}

pub async fn path(
    Path((route_id, dir)): Path<(i64, i64)>,
    State(state): State<AppState>,
) -> Result<Json<Value>, crate::error::ApiError> {
    let v = run_db(state, move |conn| {
        crate::queries::routes::path(conn, route_id, dir)
    })
    .await?;
    Ok(Json(v))
}

pub async fn stops(
    Path((route_id, dir)): Path<(i64, i64)>,
    State(state): State<AppState>,
) -> Result<Json<Value>, crate::error::ApiError> {
    let v = run_db(state, move |conn| {
        crate::queries::routes::stops(conn, route_id, dir)
    })
    .await?;
    Ok(Json(v))
}

pub async fn ways(
    Path((route_id, dir)): Path<(i64, i64)>,
    State(state): State<AppState>,
) -> Result<Json<Value>, crate::error::ApiError> {
    let v = run_db(state, move |conn| {
        crate::queries::routes::ways(conn, route_id, dir)
    })
    .await?;
    Ok(Json(v))
}

pub async fn raw(
    Path((route_id, dir)): Path<(i64, i64)>,
    State(state): State<AppState>,
) -> Result<Response, crate::error::ApiError> {
    let v = run_db(state, move |conn| {
        crate::queries::routes::raw(conn, route_id, dir)
    })
    .await?;
    match v {
        Some(v) => Ok(Json(v).into_response()),
        None => Ok(crate::error::not_found("route")),
    }
}
