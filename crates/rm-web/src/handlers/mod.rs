//! Route table.

pub mod assets;
pub mod edit;
pub mod qa;
pub mod refresh;
pub mod routes;
pub mod stats;
pub mod stops;

use axum::routing::get;
use axum::Router;

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(assets::serve))
        .route("/style.css", get(assets::serve))
        .route("/js/app.js", get(assets::serve))
        .route("/js/api.js", get(assets::serve))
        .route("/js/icons.js", get(assets::serve))
        .route("/js/layers.js", get(assets::serve))
        .route("/js/panels.js", get(assets::serve))
        .route("/js/lineEdit.js", get(assets::serve))
        .route("/js/colors.js", get(assets::serve))
        .route("/vendor/leaflet.js", get(assets::serve))
        .route("/vendor/leaflet.css", get(assets::serve))
        .route("/api/stats", get(stats::stats))
        .route("/api/routes", get(routes::list))
        .route("/api/routes/{id}", get(routes::detail))
        .route("/api/routes/{id}/directions/{dir}/path", get(routes::path))
        .route(
            "/api/routes/{id}/directions/{dir}/stops",
            get(routes::stops),
        )
        .route("/api/routes/{id}/directions/{dir}/ways", get(routes::ways))
        .route("/api/routes/{id}/directions/{dir}/raw", get(routes::raw))
        .route("/api/stops", get(stops::viewport))
        .route("/api/stops/{id}", get(stops::detail))
        .route("/api/stations", get(stops::station_search))
        .route("/api/stations/nearest", get(stops::station_nearest))
        .route("/api/stations/{id}", get(stops::station_detail))
        .route("/api/qa/outliers", get(qa::outliers))
        .route("/api/qa/matchdev", get(qa::matchdev))
}
