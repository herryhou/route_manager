use std::path::PathBuf;

use clap::Parser;

/// Load whitelisted OSM road ways from a local PBF extract into the DB.
#[derive(Parser)]
struct Args {
    /// SQLite DB path
    #[arg(long, default_value = "data/taichung.db")]
    db: PathBuf,
    /// Local OSM PBF extract (Taichung-scoped)
    #[arg(long, default_value = "assets/Taichung/data/TW-TXG-202610020300.osm.pbf")]
    pbf: PathBuf,
    /// Keep only ways touching this bbox: "south,west,north,east" degrees.
    /// Keeps each surviving way whole, so edge connectivity survives.
    #[arg(long)]
    clip: Option<String>,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();
    let clip = match &args.clip {
        Some(s) => Some(
            rm_osm_prepare::clip::Bbox::parse(s)
                .ok_or_else(|| anyhow::anyhow!("--clip expects south,west,north,east degrees, got {s:?}"))?,
        ),
        None => None,
    };
    let conn = rm_db::open(&args.db)?;
    let s = rm_osm_prepare::run(&conn, &args.pbf, clip)?;
    println!(
        "osm: {} ways, {} way_node refs, {} nodes (from {})",
        s.ways,
        s.way_nodes,
        s.nodes,
        args.pbf.display()
    );
    Ok(())
}
