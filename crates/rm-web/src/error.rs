//! API error envelope: non-2xx with `{"error": "..."}`.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug)]
pub struct ApiError {
    err: anyhow::Error,
    status: Option<StatusCode>,
}

/// Same text the JSON envelope carries (`IntoResponse`).
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.err)
    }
}

impl ApiError {
    /// 400 helper for malformed query params.
    pub fn bad_request(msg: &str) -> Self {
        Self {
            err: anyhow::anyhow!("{msg}"),
            status: Some(StatusCode::BAD_REQUEST),
        }
    }

    /// 401 helper for missing credentials.
    pub fn unauthorized(msg: &str) -> Self {
        Self {
            err: anyhow::anyhow!("{msg}"),
            status: Some(StatusCode::UNAUTHORIZED),
        }
    }

    /// 403 helper for rejected credentials.
    pub fn forbidden(msg: &str) -> Self {
        Self {
            err: anyhow::anyhow!("{msg}"),
            status: Some(StatusCode::FORBIDDEN),
        }
    }

    /// 404 helper (method form; see also the free fn for unknown ids).
    pub fn not_found(msg: &str) -> Self {
        Self {
            err: anyhow::anyhow!("{msg}"),
            status: Some(StatusCode::NOT_FOUND),
        }
    }

    /// 409 helper for edit conflicts (stale version id, un-undoable edit).
    pub fn conflict(msg: &str) -> Self {
        Self {
            err: anyhow::anyhow!("{msg}"),
            status: Some(StatusCode::CONFLICT),
        }
    }

    /// Inspect the wrapped anyhow error, so handlers can special-case domain
    /// errors (e.g. `rm_db::EditError` → 409) that fell through the blanket
    /// `From` map.
    pub fn downcast_ref<E: std::error::Error + Send + Sync + 'static>(&self) -> Option<&E> {
        self.err.downcast_ref::<E>()
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status.unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (status, Json(json!({ "error": self.err.to_string() }))).into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(e: E) -> Self {
        Self {
            err: e.into(),
            status: None,
        }
    }
}

/// 404 helper used by handlers for unknown ids.
pub fn not_found(what: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": format!("{what} not found") })),
    )
        .into_response()
}
