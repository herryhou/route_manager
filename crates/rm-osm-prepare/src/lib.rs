//! rm-osm-prepare: load whitelisted road ways from a local OSM PBF extract
//! into osm_way / osm_way_node / osm_node (+ osm_meta). Two-pass streaming:
//! ways first (collecting referenced node ids), then nodes (coords).
//! Re-runnable: the osm_* tables are rewritten atomically in one tx.

pub mod clip;
pub mod filter;
pub mod stitch;

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result};
use rm_core::LatLng;
use rusqlite::Connection;

#[derive(Debug, Default, PartialEq)]
pub struct Summary {
    pub ways: usize,
    pub way_nodes: usize,
    pub nodes: usize,
}

struct WayRow {
    id: i64,
    highway: String,
    oneway: i64,
    name: String,
    refs: Vec<i64>,
}

/// Load `pbf` into the osm_* tables (atomic rewrite). With `clip`, keep
/// only ways touching that bbox (whole, so edge connectivity survives).
pub fn run(conn: &Connection, pbf: &Path, clip_box: Option<clip::Bbox>) -> Result<Summary> {
    use osmpbf::{Element, ElementReader};

    let mut ways: Vec<WayRow> = Vec::new();
    let mut want: HashSet<i64> = HashSet::new();

    let reader = ElementReader::from_path(pbf).context("open pbf (ways pass)")?;
    reader
        .for_each(|el| {
            if let Element::Way(w) = el {
                let tags: Vec<(&str, &str)> = w.tags().collect();
                let get = |key: &str| tags.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
                let highway = get("highway");
                if !filter::highway_allowed(highway) {
                    return;
                }
                let refs: Vec<i64> = w.refs().collect();
                if refs.len() < 2 {
                    return;
                }
                let oneway = get("oneway").map(filter::oneway_value).unwrap_or(0);
                let name = get("name").unwrap_or("").to_string();
                want.extend(refs.iter().copied());
                ways.push(WayRow {
                    id: w.id(),
                    highway: highway.unwrap_or_default().to_string(),
                    oneway,
                    name,
                    refs,
                });
            }
        })
        .context("read ways")?;

    let mut nodes: Vec<(i64, f64, f64)> = Vec::new();
    let reader = ElementReader::from_path(pbf).context("open pbf (nodes pass)")?;
    reader
        .for_each(|el| match el {
            Element::Node(n) => {
                let id = n.id();
                if want.contains(&id) {
                    nodes.push((id, n.lat(), n.lon()));
                }
            }
            Element::DenseNode(n) => {
                let id = n.id();
                if want.contains(&id) {
                    nodes.push((id, n.lat(), n.lon()));
                }
            }
            _ => {}
        })
        .context("read nodes")?;

    // coords of every referenced node, for clip decisions and stitching
    let node_pos: HashMap<i64, LatLng> = nodes
        .iter()
        .map(|&(id, lat, lon)| (id, LatLng::new(lat, lon)))
        .collect();

    // optional bbox clip: drop ways with no node inside, then the nodes
    // only the dropped ways referenced (kept ways stay whole, so edge
    // connectivity survives)
    let (mut ways, keep_ids) = match clip_box {
        Some(b) => clip::clip(ways, |w: &WayRow| &w.refs, &node_pos, &b),
        None => (ways, want),
    };
    let nodes: Vec<(i64, f64, f64)> = match clip_box {
        Some(_) => nodes
            .into_iter()
            .filter(|(id, _, _)| keep_ids.contains(id))
            .collect(),
        None => nodes,
    };
    let connectors = stitch::stitch_busway_ends(
        &ways
            .iter()
            .map(|w| stitch::Way {
                id: w.id,
                highway: w.highway.clone(),
                refs: w.refs.clone(),
            })
            .collect::<Vec<_>>(),
        &node_pos,
    );
    for c in &connectors {
        ways.push(WayRow {
            id: c.id,
            highway: "busway".to_string(),
            oneway: 0,
            name: "busway connector (synthetic)".to_string(),
            refs: vec![c.from_node, c.to_node],
        });
    }

    let summary = Summary {
        ways: ways.len(),
        way_nodes: ways.iter().map(|w| w.refs.len()).sum(),
        nodes: nodes.len(),
    };

    let sha = sha256_hex(pbf)?;
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "DELETE FROM osm_way_node;
         DELETE FROM osm_way;
         DELETE FROM osm_node;
         DELETE FROM osm_meta;",
    )?;
    {
        let mut w = tx.prepare(
            "INSERT INTO osm_way (osm_way_id, highway, oneway, name) VALUES (?1, ?2, ?3, ?4)",
        )?;
        for row in &ways {
            w.execute(rusqlite::params![row.id, row.highway, row.oneway, row.name])?;
        }
    }
    {
        let mut wn = tx.prepare(
            "INSERT INTO osm_way_node (osm_way_id, ordinal, osm_node_id) VALUES (?1, ?2, ?3)",
        )?;
        for row in &ways {
            for (ord, nid) in row.refs.iter().enumerate() {
                wn.execute(rusqlite::params![row.id, (ord + 1) as i64, nid])?;
            }
        }
    }
    {
        let mut n =
            tx.prepare("INSERT INTO osm_node (osm_node_id, lat, lon) VALUES (?1, ?2, ?3)")?;
        for (id, lat, lon) in &nodes {
            n.execute(rusqlite::params![id, lat, lon])?;
        }
    }
    {
        let mut m = tx.prepare("INSERT INTO osm_meta (key, value) VALUES (?1, ?2)")?;
        let mut meta: Vec<(&str, String)> = vec![
            (
                "pbf_file",
                pbf.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string(),
            ),
            ("pbf_sha256", sha),
            ("imported_at", rm_db::now_rfc3339()),
            ("highway_whitelist", filter::HIGHWAY_WHITELIST.join(",")),
            ("way_count", summary.ways.to_string()),
            ("node_count", summary.nodes.to_string()),
        ];
        let mut clip_str: Option<(&str, String)> = None;
        if let Some(b) = clip_box {
            clip_str = Some((
                "clip_bbox",
                format!("{},{},{},{}", b.south, b.west, b.north, b.east),
            ));
        }
        if let Some(entry) = clip_str {
            meta.push(entry);
        }
        for (k, v) in meta {
            m.execute(rusqlite::params![k, v])?;
        }
    }
    tx.commit()?;
    Ok(summary)
}

fn sha256_hex(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    let mut f = std::fs::File::open(path)?;
    std::io::copy(&mut f, &mut h)?;
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}
