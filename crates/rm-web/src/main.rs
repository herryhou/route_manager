use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;

/// Read-only map explorer for the published route/stop/segment data.
#[derive(Parser)]
struct Args {
    /// Path to the pipeline SQLite database
    #[arg(long, default_value = "data/taichung.db")]
    db: PathBuf,

    /// Listen address
    #[arg(long, default_value = "127.0.0.1:8787")]
    addr: SocketAddr,

    /// Tile URL template override (default: OpenStreetMap raster tiles; needs internet)
    #[arg(long)]
    tiles: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    rm_web::serve(args.db, args.addr, args.tiles).await
}
