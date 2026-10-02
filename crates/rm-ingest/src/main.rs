use std::path::PathBuf;

use clap::Parser;

/// Ingest raw Taichung captures into src_* staging tables (one batch).
#[derive(Parser)]
struct Args {
    /// SQLite DB path
    #[arg(long, default_value = "data/taichung.db")]
    db: PathBuf,
    /// Capture root containing routes.json, providers.json, raw/
    #[arg(long, default_value = "assets/Taichung")]
    source: PathBuf,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();
    if let Some(parent) = args.db.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = rm_db::open(&args.db)?;
    let s = rm_ingest::run(&conn, &args.source)?;
    println!(
        "batch {} ingested: {} routes, {} stop_edges, {} route_points, {} providers, \
         {} route_providers, {} main_side_links, {} joint_operations",
        s.batch_id,
        s.routes,
        s.stop_edges,
        s.route_points,
        s.providers,
        s.route_providers,
        s.main_side_links,
        s.joint_operations
    );
    for w in &s.warnings {
        println!("warning: {w}");
    }
    Ok(())
}
