use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;

/// Map explorer for the published route/stop data.
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

    /// Enable edit endpoints (POST /api/edit/...)
    #[arg(long, requires = "edit_tokens")]
    edit: bool,

    /// Edit tokens as comma-separated `name:token` entries (or RM_EDIT_TOKENS env)
    #[arg(long, env = "RM_EDIT_TOKENS")]
    edit_tokens: Option<String>,

    /// Apply pending migrations to the DB, then exit (brings pre-edit DBs to 0010+)
    #[arg(long)]
    migrate: bool,
}

/// Parse `name:token` entries, comma-separated. Bails by entry index — the
/// entries carry bare tokens, so they are never echoed.
fn parse_tokens(s: &str) -> anyhow::Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    for (i, entry) in s.split(',').enumerate() {
        let Some((name, token)) = entry.split_once(':') else {
            anyhow::bail!("token entry {}: expected name:token", i + 1);
        };
        if name.is_empty() || token.is_empty() {
            anyhow::bail!("token entry {}: expected non-empty name:token", i + 1);
        }
        if map.insert(name.to_string(), token.to_string()).is_some() {
            anyhow::bail!("token entry {}: duplicate name {name:?}", i + 1);
        }
    }
    Ok(map)
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
    if args.migrate {
        rm_db::open(&args.db)?;
        println!("migrations applied: {}", args.db.display());
        return Ok(());
    }
    let edit_tokens = if args.edit {
        let raw = args
            .edit_tokens
            .expect("clap enforces --edit requires --edit-tokens");
        Some(parse_tokens(&raw)?)
    } else {
        if args.edit_tokens.is_some() {
            anyhow::bail!("--edit-tokens requires --edit");
        }
        None
    };
    rm_web::serve(args.db, args.addr, args.tiles, edit_tokens).await
}
