//! `GET /api/segments` + `GET /api/segments/{id}` — segment registry layer.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::Value;
use std::collections::HashMap;

use crate::error::ApiError;
use crate::state::{AppState, run_db};

pub async fn list(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let bbox = parse_bbox(params.get("bbox").map(|s| s.as_str())).ok_or_else(|| {
        ApiError::bad_request("bbox must be minLon,minLat,maxLon,maxLat")
    })?;
    let min_routes = params
        .get("min_routes")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(1);
    let limit = params
        .get("limit")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(2000)
        .clamp(1, 5000);
    let v = run_db(state, move |conn| {
        crate::queries::segments::list(conn, bbox, min_routes, limit)
    })
    .await?;
    Ok(Json(v))
}

pub async fn detail(
    State(state): State<AppState>,
    Path(segment_id): Path<String>,
) -> Result<Response, ApiError> {
    let v = run_db(state, move |conn| {
        crate::queries::segments::detail(conn, &segment_id)
    })
    .await?;
    match v {
        Some(v) => Ok(Json(v).into_response()),
        None => Ok(StatusCode::NOT_FOUND.into_response()),
    }
}

fn parse_bbox(s: Option<&str>) -> Option<(f64, f64, f64, f64)> {
    let s = s?;
    let mut it = s.split(',');
    let min_lon = it.next()?.parse().ok()?;
    let min_lat = it.next()?.parse().ok()?;
    let max_lon = it.next()?.parse().ok()?;
    let max_lat = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some((min_lon, min_lat, max_lon, max_lat))
}
