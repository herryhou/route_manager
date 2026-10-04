//! Stop handlers: detail, station detail, viewport bbox layer, search.

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::Json;

use crate::error::not_found;
use crate::state::{run_db, AppState};

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

pub async fn station_search(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<axum::response::Response, crate::error::ApiError> {
    // /api/stations?q= — name search feeding the sidebar station list.
    let Some(q) = params.get("q").map(|s| s.trim()).filter(|s| !s.is_empty()) else {
        return Err(crate::error::ApiError::bad_request("q required"));
    };
    let q = q.to_owned();
    let v = run_db(state, move |conn| crate::queries::stops::station_search(conn, &q)).await?;
    Ok(Json(v).into_response())
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

pub async fn viewport(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<axum::response::Response, crate::error::ApiError> {
    // /api/stops?q= searches; /api/stops?bbox= is the viewport layer
    if let Some(q) = params.get("q") {
        let q = q.clone();
        let v = run_db(state, move |conn| crate::queries::stops::search(conn, &q)).await?;
        return Ok(Json(v).into_response());
    }
    let bbox_raw = params.get("bbox").map(|s| s.as_str());
    let Some(bbox_raw) = bbox_raw else {
        return Err(crate::error::ApiError::bad_request(
            "bbox required: minLon,minLat,maxLon,maxLat",
        ));
    };
    let mut it = bbox_raw.split(',');
    let bbox = (
        it.next().and_then(|v| v.parse().ok()),
        it.next().and_then(|v| v.parse().ok()),
        it.next().and_then(|v| v.parse().ok()),
        it.next().and_then(|v| v.parse().ok()),
    );
    let (Some(a), Some(b), Some(c), Some(d)) = bbox else {
        return Err(crate::error::ApiError::bad_request(
            "bbox must be 4 numbers",
        ));
    };
    let kind = params.get("kind").cloned().unwrap_or_else(|| "stop".into());
    if kind != "stop" && kind != "station" {
        return Err(crate::error::ApiError::bad_request(
            "kind must be stop|station",
        ));
    }
    let limit = params
        .get("limit")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(2500)
        .clamp(1, 5000);
    let v = run_db(state, move |conn| {
        crate::queries::stops::viewport(conn, (a, b, c, d), &kind, limit)
    })
    .await?;
    Ok(Json(v).into_response())
}
