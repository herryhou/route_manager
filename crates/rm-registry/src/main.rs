use clap::Parser;

/// Directed segment registry: cut way runs at stop chainages, hash segments.
#[derive(Parser)]
struct Args {
    /// Path to the SQLite database
    #[arg(long, default_value = "data/taichung.db")]
    db: String,

    /// Import batch to process (default: latest OPEN batch)
    #[arg(long)]
    batch: Option<i64>,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    let conn = rm_db::open(std::path::Path::new(&args.db))?;
    let batch = match args.batch {
        Some(b) => b,
        None => rm_db::latest_open_batch(&conn)?
            .ok_or_else(|| anyhow::anyhow!("no OPEN batch; pass --batch"))?,
    };
    let s = rm_registry::run(&conn, batch)?;
    println!(
        "batch {} registry: {} directions, {} traversals, {} segments, {} skipped pairs",
        s.batch_id, s.directions, s.traversals, s.segments, s.skipped_pairs
    );
    let b = rm_registry::bearing::run(&conn, batch)?;
    println!(
        "batch {} bearings: {} stops ({} OSM, {} polyline, {} skipped), {} stations",
        batch,
        b.stops_osm + b.stops_polyline + b.stops_skipped,
        b.stops_osm,
        b.stops_polyline,
        b.stops_skipped,
        b.stations
    );
    Ok(())
}
