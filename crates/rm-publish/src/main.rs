use std::path::PathBuf;

use clap::Parser;

/// Publish Route/SubRoute/StopOfRoute V1 per route (atomic tx each).
#[derive(Parser)]
struct Args {
    /// SQLite DB path
    #[arg(long, default_value = "data/taichung.db")]
    db: PathBuf,
    /// Batch to publish (default: latest OPEN)
    #[arg(long)]
    batch: Option<i64>,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();
    let mut conn = rm_db::open(&args.db)?;
    let batch = match args.batch {
        Some(b) => b,
        None => rm_db::latest_open_batch(&conn)?.expect("no OPEN batch; run rm-ingest first"),
    };
    let s = rm_publish::run(&mut conn, batch)?;
    println!(
        "batch {}: published {} routes ({} stop_of_route rows), skipped {}, failed {}",
        s.batch_id, s.published, s.stop_of_route_rows, s.skipped, s.failed
    );
    for (xno, reason) in &s.failures {
        println!("FAILED xno={xno}: {reason}");
    }
    if s.failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}
