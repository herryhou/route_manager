//! Debug: print tags + refs of specific ways in a PBF extract.
//!
//! Usage: cargo run -p rm-osm-prepare --example dump_ways -- <file.pbf> <way_id>...

use osmpbf::{Element, ElementReader};

fn main() {
    let mut args = std::env::args().skip(1);
    let pbf = args.next().expect("pbf path");
    let ids: Vec<i64> = args.map(|a| a.parse().expect("way id")).collect();
    let reader = ElementReader::from_path(&pbf).expect("open pbf");
    reader
        .for_each(|el| {
            if let Element::Way(w) = el {
                if ids.contains(&w.id()) {
                    println!("way {} tags={:?}", w.id(), w.tags().collect::<Vec<_>>());
                    let refs: Vec<i64> = w.refs().collect();
                    println!("  refs: {refs:?}");
                }
            }
        })
        .expect("read");
}
