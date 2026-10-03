//! rm-web: read-only map explorer for the published route/stop/segment data.

pub mod db;
pub mod error;
pub mod geojson;
pub mod handlers;
pub mod queries;
pub mod state;

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Result;
use axum::Router;

use state::AppState;

/// Serve the explorer until ctrl-c.
pub async fn serve(db_path: PathBuf, addr: SocketAddr, tiles: Option<String>) -> Result<()> {
    let state = AppState::with_tiles(db_path, tiles);
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

/// Router with an injected state (tests).
pub fn router_with(state: AppState) -> Router {
    handlers::router().with_state(state)
}
