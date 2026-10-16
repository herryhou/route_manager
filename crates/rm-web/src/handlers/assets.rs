//! Embedded static assets, served from fixed paths with no-cache.
//! `/` gets the tile-URL template injected (RM_TILES).

use axum::extract::State;
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};

use crate::state::AppState;

const DEFAULT_TILES: &str = "http://localhost:3000/osm-2020-02-10-v3.11_taiwan_taipei/{z}/{x}/{y}";

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
            let bounds = match state.bounds() {
                Some(b) => format!(
                    "[{}]",
                    b.iter()
                        .map(|x| x.to_string())
                        .collect::<Vec<_>>()
                        .join(",")
                ),
                None => "null".to_string(),
            };
            let body = include_str!("../../assets/index.html")
                .replace("__TILES__", tiles)
                .replace("__BOUNDS__", &bounds);
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
        "/js/icons.js" => asset(
            "application/javascript",
            include_str!("../../assets/js/icons.js"),
        ),
        "/js/layers.js" => asset(
            "application/javascript",
            include_str!("../../assets/js/layers.js"),
        ),
        "/js/panels.js" => asset(
            "application/javascript",
            include_str!("../../assets/js/panels.js"),
        ),
        "/js/lineEdit.js" => asset(
            "application/javascript",
            include_str!("../../assets/js/lineEdit.js"),
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
        "/vendor/maplibre-gl.js" => asset(
            "application/javascript",
            include_str!("../../assets/vendor/maplibre-gl.js"),
        ),
        "/vendor/maplibre-gl.css" => asset(
            "text/css",
            include_str!("../../assets/vendor/maplibre-gl.css"),
        ),
        "/vendor/leaflet-maplibre-gl.js" => asset(
            "application/javascript",
            include_str!("../../assets/vendor/leaflet-maplibre-gl.js"),
        ),
        "/vendor/osm-liberty-local.json" => asset(
            "application/json",
            include_str!("../../assets/vendor/osm-liberty-local.json"),
        ),
        _ => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}
