//! Embedded static assets, served from fixed paths with no-cache.
//! `/` gets the tile-URL template injected (RM_TILES).

use axum::extract::State;
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};

use crate::state::AppState;

const DEFAULT_TILES: &str = "https://tile.openstreetmap.org/{z}/{x}/{y}.png";

fn asset(content_type: &'static str, body: &'static str) -> Response {
    (
        [(header::CACHE_CONTROL, "no-cache")],
        [(header::CONTENT_TYPE, content_type)],
        body,
    )
        .into_response()
}

/// Fixed-path asset handler: one match arm per embedded file.
pub async fn serve(State(state): State<AppState>, uri: Uri) -> Response {
    match uri.path() {
        "/" => {
            let tiles = state.tiles().unwrap_or(DEFAULT_TILES);
            let body = include_str!("../../assets/index.html").replace("__TILES__", tiles);
            (
                [(header::CACHE_CONTROL, "no-cache")],
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                body,
            )
                .into_response()
        }
        "/style.css" => asset("text/css", include_str!("../../assets/style.css")),
        "/js/app.js" => asset(
            "application/javascript",
            include_str!("../../assets/js/app.js"),
        ),
        "/js/api.js" => asset(
            "application/javascript",
            include_str!("../../assets/js/api.js"),
        ),
        "/js/layers.js" => asset(
            "application/javascript",
            include_str!("../../assets/js/layers.js"),
        ),
        "/js/panels.js" => asset(
            "application/javascript",
            include_str!("../../assets/js/panels.js"),
        ),
        "/js/colors.js" => asset(
            "application/javascript",
            include_str!("../../assets/js/colors.js"),
        ),
        "/vendor/leaflet.js" => asset(
            "application/javascript",
            include_str!("../../assets/vendor/leaflet.js"),
        ),
        "/vendor/leaflet.css" => asset("text/css", include_str!("../../assets/vendor/leaflet.css")),
        _ => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}
