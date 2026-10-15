use clap::Parser;

/// Match published direction polylines onto the OSM road graph (Viterbi).
#[derive(Parser)]
struct Args {
    /// Path to the SQLite database
    #[arg(long, default_value = "data/taichung.db")]
    db: String,

    /// Import batch to process (default: latest)
    #[arg(long)]
    batch: Option<i64>,

    /// Refresh only directions marked stale, then clear their marks
    #[arg(long)]
    stale: bool,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();
    let conn = rm_db::open(std::path::Path::new(&args.db))?;
    let batch = match args.batch {
        Some(b) => b,
        None => rm_db::latest_batch(&conn)?.expect("no import batch; run rm-ingest first"),
    };

    if args.stale {
        match rm_osm_match::run_stale(&conn, batch)? {
            Some(out) => println!(
                "stale: {} directions, {} matched, {} failed, {} marks cleared, {} kept",
                out.summary.directions,
                out.summary.matched,
                out.summary.failed,
                out.cleared,
                out.kept
            ),
            None => println!("nothing stale"),
        }
        return Ok(());
    }

    let s = rm_osm_match::run(&conn, batch)?;

    tracing::info!(
        directions = s.directions,
        matched = s.matched,
        unmatched = s.unmatched,
        failed = s.failed,
        runs = s.total_runs,
        gap_repairs = s.gap_repairs,
        "osm-match complete"
    );
    println!(
        "batch {} directions {} matched {} unmatched {} failed {} runs {} gap_repairs {}",
        s.batch_id, s.directions, s.matched, s.unmatched, s.failed, s.total_runs, s.gap_repairs
    );
    Ok(())
}
