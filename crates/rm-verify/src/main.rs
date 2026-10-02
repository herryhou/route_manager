use std::path::Path;

use clap::Parser;

/// Invariant checks, count cross-checks, QA exports. Exits non-zero on failure.
#[derive(Parser)]
struct Args {
    /// Path to the SQLite database
    #[arg(long, default_value = "data/taichung.db")]
    db: String,

    /// Import batch to verify (default: latest OPEN batch)
    #[arg(long)]
    batch: Option<i64>,

    /// Directory for QA exports (report.json, segments.geojson, outlier_stops.csv)
    #[arg(long, default_value = "qa")]
    qa_dir: String,
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
    let report = rm_verify::run(
        &conn,
        batch,
        &rm_verify::Expectations::REAL,
        Path::new(&args.qa_dir),
    )?;

    for c in &report.checks {
        println!(
            "{} {:<28} {}",
            if c.pass { "PASS" } else { "FAIL" },
            c.id,
            c.detail
        );
    }
    println!("counts: {:?}\nstats: {:?}", report.counts, report.stats);
    if report.all_pass() {
        println!("verify: all checks pass (batch {batch})");
        Ok(())
    } else {
        let failed = report.checks.iter().filter(|c| !c.pass).count();
        anyhow::bail!("verify: {failed} check(s) failed (batch {batch})");
    }
}
