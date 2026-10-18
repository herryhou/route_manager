//! `extend-path`'s planning half: extend a direction polyline outward
//! along the OSM road graph. Per chosen end — snap the endpoint to the
//! nearest road segment (within [`MAX_SNAP_M`]), then walk outward by
//! Dijkstra until `meters` of graph distance, preferring the
//! straightest continuation at every fan-out. All read-only; the write
//! (a whole-line polyline override) is `rm_db::edit_polyline_line`,
//! driven from the dispatch in `lib.rs`.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use anyhow::Result;
use rm_core::{geo, polyline, LatLng};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

use crate::graph::RoadGraph;

/// Maximum endpoint-to-road snap distance (m): beyond it an end is "not
/// near the graph" and the job is rejected naming this limit.
pub const MAX_SNAP_M: f64 = 50.0;

/// Junction-coincidence epsilon (m): the walked chain's first node counts
/// as the original endpoint's own vertex when within this distance.
const SAME_POINT_M: f64 = 1e-9;

/// One end's planned outward extension.
#[derive(Debug, Serialize)]
pub struct Extension {
    /// `"first"` or `"last"` — which end was extended
    pub end: String,
    /// OSM ways the walked chain traverses, in walk order, repeats collapsed
    pub way_ids: Vec<i64>,
    /// `[lat, lon]` vertices to prepend/append, in walk order
    pub new_vertices: Vec<[f64; 2]>,
    /// Endpoint-to-road distance of the snap (m)
    pub snap_dist_m: f64,
}

/// Plan the extension(s) for one route direction: resolve the effective
/// path (override → src → matched path; `line_snapshot` keeps the
/// pre-0013 override degrade), load the road graph, and walk each chosen
/// end outward. Returns the candidate points (original with the extension
/// vertices prepended/appended), the per-end extensions, and the source
/// the path came from (`"override"` / `"src"` / `"matched"`).
pub fn plan_extension(
    conn: &Connection,
    xno: &str,
    direction: i64,
    meters: f64,
    ends: &str,
    logs: &mut Vec<String>,
) -> Result<(Vec<LatLng>, Vec<Extension>, String)> {
    if meters <= 0.0 {
        anyhow::bail!("meters must be > 0 (got {meters})");
    }
    let ends_list: &[&str] = match ends {
        "both" => &["first", "last"],
        "first" => &["first"],
        "last" => &["last"],
        other => anyhow::bail!("invalid end '{other}' — expected both, first or last"),
    };
    let Some(snap) = line_snapshot(conn, xno, direction, logs)? else {
        anyhow::bail!("route '{xno}' does not exist");
    };
    let (pts, source): (Vec<LatLng>, &str) = if let Some(o) = &snap.override_row {
        (polyline::decode(&o.encoded)?, "override")
    } else if let Some(s) = &snap.src_encoded {
        (polyline::decode(s)?, "src")
    } else {
        match rm_db::matched_path(conn, snap.route_id, direction)? {
            Some(p) => (p, "matched"),
            None => (Vec::new(), "none"),
        }
    };
    if pts.len() < 2 {
        anyhow::bail!(
            "no path for route '{xno}' direction {direction} — no override, no src \
             polyline with ≥2 points, no matched path"
        );
    }
    // empty osm_* tables error here with "run rm-osm-prepare" verbatim
    let graph = RoadGraph::from_conn(conn)?;

    let mut extensions = Vec::with_capacity(ends_list.len());
    for end in ends_list {
        extensions.push(walk_one(&graph, &pts, end, meters)?);
    }
    // candidate = original, first-extension prepended, last appended
    let mut candidate = pts.clone();
    for ext in &extensions {
        let vs: Vec<LatLng> = ext
            .new_vertices
            .iter()
            .map(|v| LatLng::new(v[0], v[1]))
            .collect();
        if ext.end == "first" {
            candidate.splice(0..0, vs);
        } else {
            candidate.extend(vs);
        }
    }
    Ok((candidate, extensions, source.to_string()))
}

/// `rm_db::load_polyline_line` with the old-DB degrade: `polyline_override`
/// exists only from migration 0013, so probe `sqlite_master` first — when
/// the table is missing the override is treated as absent (a `logs` entry
/// names the table, never an error — the /raw incident rule) and the src
/// half is loaded with the same queries `load_polyline_line` runs.
fn line_snapshot(
    conn: &Connection,
    xno: &str,
    direction: i64,
    logs: &mut Vec<String>,
) -> Result<Option<rm_db::LineSnapshot>> {
    let have_override: bool = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'polyline_override'",
        [],
        |r| r.get::<_, i64>(0),
    )? > 0;
    if !have_override {
        const DEGRADE: &str =
            "polyline_override table missing — override treated as absent (pre-0013 db)";
        if !logs.iter().any(|l| l == DEGRADE) {
            logs.push(DEGRADE.to_string());
        }
        let (route_id, route_version_id): (i64, Option<i64>) = match conn.query_row(
            "SELECT route_id, current_route_version_id FROM route WHERE ext_uid = ?1",
            [format!("tcg:{xno}")],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ) {
            Ok(v) => v,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let batch_id: i64 = conn.query_row(
            "SELECT COALESCE(MAX(batch_id), 0) FROM import_batch",
            [],
            |r| r.get(0),
        )?;
        let src_encoded: Option<String> = conn
            .query_row(
                "SELECT encoded FROM src_route_point \
                 WHERE batch_id = ?1 AND xno = ?2 AND direction = ?3",
                rusqlite::params![batch_id, xno, direction],
                |r| r.get(0),
            )
            .optional()?;
        return Ok(Some(rm_db::LineSnapshot {
            route_id,
            src_encoded,
            override_row: None,
            route_version_id,
        }));
    }
    rm_db::load_polyline_line(conn, xno, direction)
}

/// The outward walk for one end. Snap → start node → Dijkstra with
/// straightest-first fan-out ordering → chain → vertices (interpolating
/// the final partial segment at the exact `meters` mark, dropping the
/// junction vertex that duplicates the original endpoint).
fn walk_one(g: &RoadGraph, pts: &[LatLng], end: &str, meters: f64) -> Result<Extension> {
    let (endpoint, second) = match end {
        "first" => (pts[0], pts[1]),
        _ => (*pts.last().unwrap(), pts[pts.len() - 2]),
    };
    // travel direction pointing outward (away from the path interior)
    let outward = geo::bearing_deg(second, endpoint);
    let outward_vec = {
        let b = outward.to_radians();
        (b.sin(), b.cos()) // (east, north) unit vector
    };

    // snap: the nearest segment within MAX_SNAP_M (ties → lowest seg idx)
    let mut best: Option<(f64, u32)> = None;
    for idx in g.segments_near(endpoint, MAX_SNAP_M) {
        let s = g.seg(idx);
        let d = geo::point_to_segment_m(endpoint, g.node_pos(s.a), g.node_pos(s.b)).1;
        if d > MAX_SNAP_M {
            continue;
        }
        let better = match best {
            Some((bd, bi)) => d < bd || (d == bd && idx < bi),
            None => true,
        };
        if better {
            best = Some((d, idx));
        }
    }
    let Some((snap_dist_m, snap_idx)) = best else {
        anyhow::bail!(
            "no road within {MAX_SNAP_M} m of the {end} endpoint (snap limit {MAX_SNAP_M} m) \
             — nothing to extend onto"
        );
    };
    let snap = g.seg(snap_idx);
    // Start node. A seg end coincident with the endpoint (within
    // SAME_POINT_M) is the start outright — alignment is degenerate at
    // zero distance. Mid-segment snap: the end best aligned with the
    // outward bearing wins (dot of the endpoint→node unit vector with
    // `outward_vec`), so the walk leaves on the outward side; exact
    // alignment tie → the nearer node, then the lower node index. Proximity
    // alone picks the end behind the travel direction when the endpoint
    // sits nearer it, and the first walked leg (endpoint→start) backtracks
    // along the snap segment before re-crossing the endpoint.
    let dist_a = geo::haversine_m(endpoint, g.node_pos(snap.a));
    let dist_b = geo::haversine_m(endpoint, g.node_pos(snap.b));
    let (start, snap_other) = if dist_a <= SAME_POINT_M {
        (snap.a, snap.b)
    } else if dist_b <= SAME_POINT_M {
        (snap.b, snap.a)
    } else {
        let score = |n: u32, d: f64| (align(endpoint, g.node_pos(n), outward_vec), d, n);
        let (a, b) = (score(snap.a, dist_a), score(snap.b, dist_b));
        let a_wins =
            a.0 > b.0 || (a.0 == b.0 && a.1 < b.1) || (a.0 == b.0 && a.1 == b.1 && a.2 < b.2);
        if a_wins {
            (snap.a, snap.b)
        } else {
            (snap.b, snap.a)
        }
    };

    // Dijkstra outward. Heap key total order: cost, then relaxation rank
    // (assigned straightest-first within each fan-out, so an exact cost tie
    // pops the straighter branch), then node idx. All costs are finite and
    // non-negative, so `to_bits()` is a total order matching `<=` on f64.
    #[derive(PartialEq, Eq, PartialOrd, Ord)]
    struct Key(u64, u32, u32); // (cost bits, rank, node)
    let mut heap: BinaryHeap<Reverse<Key>> = BinaryHeap::new();
    let n = g.nodes.len();
    let mut dist = vec![f64::INFINITY; n];
    let mut prev: Vec<Option<(u32, u32)>> = vec![None; n]; // (prev node, seg idx)
    let mut done = vec![false; n];
    let mut next_rank = 0u32;
    dist[start as usize] = 0.0;
    heap.push(Reverse(Key(0.0f64.to_bits(), next_rank, start)));
    next_rank += 1;
    let mut stop_node: Option<u32> = None;
    while let Some(Reverse(Key(cb, _r, u))) = heap.pop() {
        if done[u as usize] {
            continue;
        }
        done[u as usize] = true;
        let cu = f64::from_bits(cb);
        if cu >= meters {
            stop_node = Some(u);
            break;
        }
        // reference travel direction at u: the hop we arrived on, else the
        // outward bearing at the start node
        let rv = match prev[u as usize] {
            Some((p, _)) => unit_dir(g.xy[p as usize], g.xy[u as usize]),
            None => outward_vec,
        };
        for &(cos, to, seg) in &fan_out_ordered(g, u, rv) {
            // first hop: do not traverse the snap seg back toward the path
            // (doubling back under the endpoint the walk just came from)
            if u == start && to == snap_other && seg == snap_idx && cos < 0.0 {
                continue;
            }
            let nd = cu + g.seg(seg).length_m;
            if nd < dist[to as usize] {
                dist[to as usize] = nd;
                prev[to as usize] = Some((u, seg));
                heap.push(Reverse(Key(nd.to_bits(), next_rank, to)));
                next_rank += 1;
            }
        }
    }
    // graph exhausted before meters: keep the farthest reached node instead
    let stop = match stop_node {
        Some(s) => s,
        None => {
            let mut far = start;
            for (i, &d) in dist.iter().enumerate() {
                if done[i] && d > dist[far as usize] {
                    far = i as u32;
                }
            }
            far
        }
    };

    // chain start → stop (inclusive), then to extension vertices
    let mut chain: Vec<u32> = vec![stop];
    let mut cur = stop;
    while let Some((p, _)) = prev[cur as usize] {
        chain.push(p);
        cur = p;
    }
    chain.reverse();

    let mut way_ids: Vec<i64> = Vec::new();
    let mut new_vertices: Vec<[f64; 2]> = Vec::new();
    if chain.len() >= 2 {
        let start_pos = g.node_pos(chain[0]);
        if geo::haversine_m(start_pos, endpoint) > SAME_POINT_M {
            // mid-segment snap: keep the junction so the extension connects
            new_vertices.push([start_pos.lat, start_pos.lon]);
        }
        let mut cum = 0.0;
        for w in chain.windows(2) {
            let (a, b) = (w[0], w[1]);
            let seg = g.seg(prev[b as usize].expect("chain node has a prev").1);
            if !way_ids.contains(&seg.way_id) {
                way_ids.push(seg.way_id);
            }
            let (a_pos, b_pos) = (g.node_pos(a), g.node_pos(b));
            cum += seg.length_m;
            if cum <= meters {
                new_vertices.push([b_pos.lat, b_pos.lon]);
            } else {
                // final partial segment: interpolate at the exact remaining
                // distance instead of overshooting past `meters` — t is the
                // overshoot fraction from b back toward a
                let t = ((cum - meters) / seg.length_m).clamp(0.0, 1.0);
                let v = LatLng::new(
                    b_pos.lat + (a_pos.lat - b_pos.lat) * t,
                    b_pos.lon + (a_pos.lon - b_pos.lon) * t,
                );
                new_vertices.push([v.lat, v.lon]);
                break;
            }
        }
    }
    Ok(Extension {
        end: end.to_string(),
        way_ids,
        new_vertices,
        snap_dist_m,
    })
}

/// Unit vector `from → to` in the graph's planar metre frame; a zero-length
/// leg bears north by convention (mirrors `bearing_deg`'s degenerate rule).
fn unit_dir(from: (f64, f64), to: (f64, f64)) -> (f64, f64) {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let norm = (dx * dx + dy * dy).sqrt();
    if norm == 0.0 {
        (0.0, 1.0)
    } else {
        (dx / norm, dy / norm)
    }
}

/// Alignment of the `from → to` direction with `rv`, both (east, north):
/// the dot product of `bearing_deg(from, to)`'s unit vector with `rv` — +1
/// dead ahead, −1 dead behind. Only meaningful at positive distance
/// (`bearing_deg` is degenerate at 0).
fn align(from: LatLng, to: LatLng, rv: (f64, f64)) -> f64 {
    let b = geo::bearing_deg(from, to).to_radians();
    b.sin() * rv.0 + b.cos() * rv.1
}

/// One node's out-edges ordered for relaxation: straightest continuation
/// first — cosine of the edge's unit vector (from `g.xy`) with the
/// reference travel direction `rv` — ties by node idx then seg idx. A
/// deterministic total order: relaxation rank flows into the Dijkstra heap
/// key, so an exact cost tie pops the straighter branch.
fn fan_out_ordered(g: &RoadGraph, u: u32, rv: (f64, f64)) -> Vec<(f64, u32, u32)> {
    let mut edges: Vec<(f64, u32, u32)> = Vec::new(); // (cos, to node, seg idx)
    for &(to, seg) in &g.out_edges[u as usize] {
        if to == u {
            continue;
        }
        let dir = unit_dir(g.xy[u as usize], g.xy[to as usize]);
        edges.push((dir.0 * rv.0 + dir.1 * rv.1, to, seg));
    }
    edges.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    edges
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{NodePos, WayInput};

    fn node(id: i64, lat: f64, lon: f64) -> NodePos {
        NodePos {
            osm_id: id,
            lat: 24.1400 + lat,
            lon: 120.6800 + lon,
        }
    }

    /// Fan-out ordering: straightest continuation first regardless of the
    /// edges' insertion order (west inserted first here).
    #[test]
    fn fan_out_orders_straightest_first() {
        // star from node 0 (osm id 1): north (1), east (2), west (3); the
        // ways are inserted west, east, north so out_edges' natural order
        // is adversarial
        let nodes = vec![
            node(1, 0.0, 0.0),
            node(2, 0.0001, 0.0),
            node(3, 0.0, 0.0001),
            node(4, 0.0, -0.0001),
        ];
        let ways = [
            WayInput {
                way_id: 10,
                oneway: 0,
                node_ids: vec![1, 4],
            },
            WayInput {
                way_id: 11,
                oneway: 0,
                node_ids: vec![1, 3],
            },
            WayInput {
                way_id: 12,
                oneway: 0,
                node_ids: vec![1, 2],
            },
        ];
        let g = RoadGraph::build(nodes, &ways);
        let order = fan_out_ordered(&g, 0, (0.0, 1.0)); // travelling north
        let seq: Vec<i64> = order.iter().map(|&(_, to, _)| g.node_osm_id(to)).collect();
        assert_eq!(
            seq,
            vec![2, 3, 4],
            "north (cos 1) before east (0) before west (-1)"
        );
        // travelling east, the east edge leads
        let order = fan_out_ordered(&g, 0, (1.0, 0.0));
        assert_eq!(g.node_osm_id(order[0].1), 3, "east edge leads");
    }

    /// The snap-seg guard: the path's last leg lies along a through way;
    /// the walk must leave the endpoint along the continuing way, never
    /// double back south under the path — even though the south hop is the
    /// same length as the north hop.
    #[test]
    fn walk_never_doubles_back_along_the_arrival_way() {
        // way 7 (south, through): 1 - 2 - 3; way 9 (north): 3 - 4 - 5
        let nodes = vec![
            node(1, -0.0001, 0.0),
            node(2, 0.0, 0.0),
            node(3, 0.0001, 0.0),
            node(4, 0.0002, 0.0),
            node(5, 0.0003, 0.0),
        ];
        let ways = [
            WayInput {
                way_id: 7,
                oneway: 0,
                node_ids: vec![1, 2, 3],
            },
            WayInput {
                way_id: 9,
                oneway: 0,
                node_ids: vec![3, 4, 5],
            },
        ];
        let g = RoadGraph::build(nodes, &ways);
        // the path arrives at node 3 travelling north along way 7
        let pts = [node(2, 0.0, 0.0), node(3, 0.0001, 0.0)]
            .iter()
            .map(|n| LatLng::new(n.lat, n.lon))
            .collect::<Vec<_>>();
        let ext = walk_one(&g, &pts, "last", 20.0).unwrap();
        assert_eq!(ext.way_ids, vec![9], "extension follows way 9 north");
        assert_eq!(ext.new_vertices.len(), 2);
        let last = ext.new_vertices.last().unwrap();
        let d = geo::haversine_m(pts[1], LatLng::new(last[0], last[1]));
        assert!(
            (19.5..20.5).contains(&d),
            "final vertex interpolates at the 20 m mark ({d:.3} m)"
        );
    }
}
