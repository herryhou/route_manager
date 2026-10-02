use std::path::PathBuf;

use clap::Parser;
use rusqlite::OptionalExtension;

/// Mint identities + Stop/Station/StationGroup V1 versions from staging.
#[derive(Parser)]
struct Args {
    /// SQLite DB path
    #[arg(long, default_value = "data/taichung.db")]
    db: PathBuf,
    /// Batch to process (default: latest OPEN)
    #[arg(long)]
    batch: Option<i64>,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();
    let conn = rm_db::open(&args.db)?;
    let batch = match args.batch {
        Some(b) => b,
        None => rm_db::latest_open_batch(&conn)?.expect("no OPEN batch; run rm-ingest first"),
    };
    let s = rm_identity::run(&conn, batch)?;
    println!(
        "batch {}: {} routes, {} subroutes, {} stations, {} groups, {} stops, \
         {} route_links, {} route_providers",
        s.batch_id,
        s.routes,
        s.subroutes,
        s.stations,
        s.station_groups,
        s.stops,
        s.route_links,
        s.route_providers
    );
    for w in &s.warnings {
        println!("warning: {w}");
    }
    // sanity: an OPEN batch that now has identities is ready for publish
    let has_routes: Option<i64> = conn
        .query_row("SELECT 1 FROM route LIMIT 1", [], |r| r.get(0))
        .optional()?;
    if has_routes.is_some() {
        println!("identities present; next: rm-publish");
    }
    Ok(())
}
