use std::path::PathBuf;

use clap::Parser;

/// Project stops onto direction polylines and cut per-stop-pair geometry.
#[derive(Parser)]
struct Args {
    /// SQLite DB path
    #[arg(long, default_value = "data/taichung.db")]
    db: PathBuf,
    /// Batch whose polylines to use (default: latest)
    #[arg(long)]
    batch: Option<i64>,
    /// Snap distance beyond which a stop is flagged OUTLIER (metres)
    #[arg(long, default_value_t = 50.0)]
    outlier_m: f64,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();
    let conn = rm_db::open(&args.db)?;
    let batch = match args.batch {
        Some(b) => b,
        None => rm_db::latest_batch(&conn)?.expect("no import batch; run rm-ingest first"),
    };
    let s = rm_segments::run(&conn, batch, args.outlier_m)?;
    println!(
        "batch {}: {} directions, {} projections ({} no_path, {} endpoint, {} outliers, {} clamped), {} draft rows",
        s.batch_id,
        s.directions,
        s.projections,
        s.no_path,
        s.endpoint,
        s.outliers,
        s.clamped,
        s.draft_rows
    );
    Ok(())
}
