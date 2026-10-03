//! Route table.

pub mod assets;
pub mod routes;
pub mod stats;

use axum::routing::get;
use axum::Router;

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(assets::serve))
        .route("/style.css", get(assets::serve))
        .route("/js/app.js", get(assets::serve))
        .route("/js/api.js", get(assets::serve))
        .route("/js/layers.js", get(assets::serve))
        .route("/js/panels.js", get(assets::serve))
        .route("/js/colors.js", get(assets::serve))
        .route("/vendor/leaflet.js", get(assets::serve))
        .route("/vendor/leaflet.css", get(assets::serve))
        .route("/api/stats", get(stats::stats))
        .route("/api/routes", get(routes::list))
        .route("/api/routes/{id}", get(routes::detail))
}
