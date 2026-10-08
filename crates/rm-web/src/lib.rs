//! rm-web: map explorer for the published route/stop/segment data.

pub mod db;
pub mod error;
pub mod geojson;
pub mod handlers;
pub mod queries;
pub mod state;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Result;
use axum::Router;

use state::AppState;

/// Serve the explorer until ctrl-c. Edit tokens enable the POST endpoints.
pub async fn serve(
    db_path: PathBuf,
    addr: SocketAddr,
    tiles: Option<String>,
    edit_tokens: Option<HashMap<String, String>>,
) -> Result<()> {
    let state = match edit_tokens {
        Some(tokens) => AppState::with_edit(db_path, tiles, tokens),
        None => AppState::with_tiles(db_path, tiles),
    };
    state.conn()?; // probe: friendly error before binding if DB missing/stale

    let router = router_with(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("rm-web listening on http://{addr}/");
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

/// Router with an injected state (tests). Edit routes are layered in only
/// when the state carries edit tokens; otherwise the paths stay 404.
pub fn router_with(state: AppState) -> Router {
    let r = handlers::router();
    if state.edit_enabled() {
        r.route(
            "/api/edit/stop/{stop_version_id}",
            axum::routing::post(handlers::edit::edit_stop),
        )
        .route("/api/edit/log", axum::routing::get(handlers::edit::log))
        .route(
            "/api/edit/undo/{edit_id}",
            axum::routing::post(handlers::edit::undo),
        )
        .with_state(state)
    } else {
        r.with_state(state)
    }
}
