//! In-memory road graph over the osm_* tables: nodes, geometric way
//! segments, uniform grid for candidate retrieval, directed adjacency
//! (oneway honored) for network traversal.

use std::collections::HashMap;

use rm_core::LatLng;
use rusqlite::Connection;

pub const CELL_M: f64 = 100.0;
const LAT_M_PER_DEG: f64 = 111_320.0;
const LON_M_PER_DEG: f64 = 101_600.0; // at Taichung latitude ~24.1

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NodePos {
    pub osm_id: i64,
    pub lat: f64,
    pub lon: f64,
}

/// One geometric segment between consecutive way nodes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Seg {
    pub a: u32, // node idx of lower ordinal
    pub b: u32, // node idx of higher ordinal
    pub way_id: i64,
    pub length_m: f64,
    pub oneway: i32, // parent way: 1 fwd, -1 reverse, 0 both
    pub mid: LatLng,
}

#[derive(Default)]
pub struct RoadGraph {
    pub nodes: Vec<NodePos>,
    node_idx: HashMap<i64, u32>,
    pub segs: Vec<Seg>,
    cell_w: f64, // cells per degree, latitude axis
    cell_h: f64, // cells per degree, longitude axis
    /// flat cell grid (CSR): seg ids per cell over the seg bounding box
    grid_origin: (i64, i64),
    grid_rows: i64,
    grid_cols: i64,
    cell_starts: Vec<u32>,
    cell_ids: Vec<u32>,
    /// planar metre coords per node (same frame as [`point_xy_m`]) for
    /// cheap distance math in hot loops
    pub xy: Vec<(f64, f64)>,
    /// directed adjacency: node idx -> (to node idx, seg idx), oneway honored
    pub out_edges: Vec<Vec<(u32, u32)>>,
}

/// Planar metre coords of an arbitrary point, same frame as
/// [`RoadGraph::xy`]. Consistent with the graph's fixed lon scale
/// (LON_M_PER_DEG at ~24.1 deg latitude).
pub fn point_xy_m(p: LatLng) -> (f64, f64) {
    (p.lon * LON_M_PER_DEG, p.lat * LAT_M_PER_DEG)
}

/// Ways passed to [`RoadGraph::build`]: ordered node ids (travel order).
pub struct WayInput {
    pub way_id: i64,
    pub oneway: i32,
    pub node_ids: Vec<i64>,
}

impl RoadGraph {
    pub fn build(nodes: Vec<NodePos>, ways: &[WayInput]) -> RoadGraph {
        let mut g = RoadGraph {
            nodes,
            ..Default::default()
        };
        g.xy = g
            .nodes
            .iter()
            .map(|n| (n.lon * LON_M_PER_DEG, n.lat * LAT_M_PER_DEG))
            .collect();
        for (i, n) in g.nodes.iter().enumerate() {
            g.node_idx.insert(n.osm_id, i as u32);
        }
        for w in ways {
            for pair in w.node_ids.windows(2) {
                let ia = g.node_idx[&pair[0]];
                let ib = g.node_idx[&pair[1]];
                let a = &g.nodes[ia as usize];
                let b = &g.nodes[ib as usize];
                let seg = Seg {
                    a: ia,
                    b: ib,
                    way_id: w.way_id,
                    length_m: rm_core::geo::haversine_m(
                        LatLng {
                            lat: a.lat,
                            lon: a.lon,
                        },
                        LatLng {
                            lat: b.lat,
                            lon: b.lon,
                        },
                    ),
                    oneway: w.oneway,
                    mid: LatLng {
                        lat: (a.lat + b.lat) / 2.0,
                        lon: (a.lon + b.lon) / 2.0,
                    },
                };
                g.segs.push(seg);
            }
        }
        g.out_edges = vec![Vec::new(); g.nodes.len()];
        for (idx, seg) in g.segs.iter().enumerate() {
            if seg.oneway != -1 {
                g.out_edges[seg.a as usize].push((seg.b, idx as u32));
            }
            if seg.oneway != 1 {
                g.out_edges[seg.b as usize].push((seg.a, idx as u32));
            }
        }
        g.index_grid();
        g
    }

    /// Load from the osm_* tables written by rm-osm-prepare.
    pub fn from_conn(conn: &Connection) -> anyhow::Result<RoadGraph> {
        let mut nodes: Vec<NodePos> = Vec::new();
        {
            let mut stmt = conn.prepare("SELECT osm_node_id, lat, lon FROM osm_node")?;
            let rows = stmt.query_map([], |r| {
                Ok(NodePos {
                    osm_id: r.get(0)?,
                    lat: r.get(1)?,
                    lon: r.get(2)?,
                })
            })?;
            for row in rows {
                nodes.push(row?);
            }
        }
        // group way_node refs by (way, ordinal)
        let mut way_rows: Vec<(i64, i32, i64)> = Vec::new(); // (way_id, ordinal, node_id)
        let mut oneway_by_way: HashMap<i64, i32> = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT w.osm_way_id, w.oneway, wn.ordinal, wn.osm_node_id
                 FROM osm_way w JOIN osm_way_node wn ON wn.osm_way_id = w.osm_way_id
                 ORDER BY w.osm_way_id, wn.ordinal",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i32>(1)?,
                    r.get::<_, i32>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?;
            for row in rows {
                let (w, ow, ord, nid) = row?;
                oneway_by_way.insert(w, ow);
                way_rows.push((w, ord, nid));
            }
        }
        let mut ways: Vec<WayInput> = Vec::new();
        let mut cur: Option<WayInput> = None;
        for (w, ord, nid) in way_rows {
            match &mut cur {
                Some(c) if c.way_id == w => {
                    debug_assert_eq!(c.node_ids.len() as i32, ord - 1);
                    c.node_ids.push(nid);
                }
                _ => {
                    if let Some(c) = cur.take() {
                        ways.push(c);
                    }
                    cur = Some(WayInput {
                        way_id: w,
                        oneway: *oneway_by_way.get(&w).unwrap_or(&0),
                        node_ids: vec![nid],
                    });
                }
            }
        }
        if let Some(c) = cur {
            ways.push(c);
        }
        anyhow::ensure!(!nodes.is_empty(), "osm_node empty; run rm-osm-prepare");
        Ok(RoadGraph::build(nodes, &ways))
    }

    fn cell_of(&self, p: LatLng) -> (i64, i64) {
        (
            (p.lat * self.cell_w).floor() as i64,
            (p.lon * self.cell_h).floor() as i64,
        )
    }

    fn index_grid(&mut self) {
        self.cell_w = LAT_M_PER_DEG / CELL_M;
        self.cell_h = LON_M_PER_DEG / CELL_M;
        if self.segs.is_empty() {
            self.grid_origin = (0, 0);
            self.grid_rows = 0;
            self.grid_cols = 0;
            self.cell_starts = vec![0];
            self.cell_ids = Vec::new();
            return;
        }
        // cell range (min_y..=max_y, min_x..=max_x) touched by each seg
        let mut ranges: Vec<(i64, i64, i64, i64)> = Vec::with_capacity(self.segs.len());
        let (mut y0, mut x0) = (i64::MAX, i64::MAX);
        let (mut y1, mut x1) = (i64::MIN, i64::MIN);
        for seg in &self.segs {
            let a = self.nodes[seg.a as usize];
            let b = self.nodes[seg.b as usize];
            let (ca0, cb0) = (
                self.cell_of(LatLng {
                    lat: a.lat,
                    lon: a.lon,
                }),
                self.cell_of(LatLng {
                    lat: b.lat,
                    lon: b.lon,
                }),
            );
            let r = (
                ca0.0.min(cb0.0),
                ca0.1.min(cb0.1),
                ca0.0.max(cb0.0),
                ca0.1.max(cb0.1),
            );
            y0 = y0.min(r.0);
            x0 = x0.min(r.1);
            y1 = y1.max(r.2);
            x1 = x1.max(r.3);
            ranges.push(r);
        }
        self.grid_origin = (y0, x0);
        self.grid_rows = y1 - y0 + 1;
        self.grid_cols = x1 - x0 + 1;
        let n_cells = (self.grid_rows * self.grid_cols) as usize;
        // counting sort into CSR layout: exclusive prefix gives cell starts
        let mut counts = vec![0u32; n_cells];
        for r in &ranges {
            for cy in r.0..=r.2 {
                for cx in r.1..=r.3 {
                    counts[self.cell_slot(cy, cx).unwrap() as usize] += 1;
                }
            }
        }
        let mut starts = vec![0u32; n_cells + 1];
        for i in 0..n_cells {
            starts[i + 1] = starts[i] + counts[i];
        }
        let mut fill = starts[..n_cells].to_vec();
        let mut ids = vec![0u32; starts[n_cells] as usize];
        for (idx, r) in ranges.iter().enumerate() {
            for cy in r.0..=r.2 {
                for cx in r.1..=r.3 {
                    let slot = self.cell_slot(cy, cx).unwrap() as usize;
                    ids[fill[slot] as usize] = idx as u32;
                    fill[slot] += 1;
                }
            }
        }
        self.cell_starts = starts;
        self.cell_ids = ids;
    }

    #[inline]
    fn cell_slot(&self, cy: i64, cx: i64) -> Option<usize> {
        let dy = cy - self.grid_origin.0;
        let dx = cx - self.grid_origin.1;
        if dy < 0 || dx < 0 || dy >= self.grid_rows || dx >= self.grid_cols {
            return None;
        }
        Some((dy * self.grid_cols + dx) as usize)
    }

    /// Segment indices whose cells lie within `radius_m` of `p`'s cell.
    pub fn segments_near(&self, p: LatLng, radius_m: f64) -> Vec<u32> {
        let ry = (radius_m / CELL_M).ceil() as i64;
        let (cy, cx) = self.cell_of(p);
        let mut out = Vec::new();
        for dy in -ry..=ry {
            for dx in -ry..=ry {
                if let Some(slot) = self.cell_slot(cy + dy, cx + dx) {
                    let (s, e) = (
                        self.cell_starts[slot] as usize,
                        self.cell_starts[slot + 1] as usize,
                    );
                    out.extend_from_slice(&self.cell_ids[s..e]);
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    pub fn node_pos(&self, idx: u32) -> LatLng {
        let n = &self.nodes[idx as usize];
        LatLng {
            lat: n.lat,
            lon: n.lon,
        }
    }

    pub fn node_osm_id(&self, idx: u32) -> i64 {
        self.nodes[idx as usize].osm_id
    }

    pub fn seg(&self, idx: u32) -> &Seg {
        &self.segs[idx as usize]
    }

    /// Outgoing neighbour node idxs from `from` along `seg` (oneway honored).
    /// Returns the node the segment leads to, or None when barred.
    pub fn seg_exit(&self, idx: u32, from: u32) -> Option<u32> {
        let s = &self.segs[idx as usize];
        match (from == s.a, from == s.b) {
            (true, _) if s.oneway == -1 => None,
            (true, _) => Some(s.b),
            (_, true) if s.oneway == 1 => None,
            (_, true) => Some(s.a),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // two short crossing ways near lat 24.14, lon 120.68 (1e-4 deg ~ 11m)
    fn synthetic() -> RoadGraph {
        let nodes = vec![
            NodePos {
                osm_id: 1,
                lat: 24.1400,
                lon: 120.6800,
            },
            NodePos {
                osm_id: 2,
                lat: 24.1410,
                lon: 120.6800,
            },
            NodePos {
                osm_id: 3,
                lat: 24.1420,
                lon: 120.6800,
            },
            NodePos {
                osm_id: 4,
                lat: 24.1410,
                lon: 120.6790,
            },
            NodePos {
                osm_id: 5,
                lat: 24.1410,
                lon: 120.6810,
            },
        ];
        let ways = vec![
            WayInput {
                way_id: 100,
                oneway: 0,
                node_ids: vec![1, 2, 3],
            },
            WayInput {
                way_id: 200,
                oneway: 1,
                node_ids: vec![4, 2, 5],
            },
        ];
        RoadGraph::build(nodes, &ways)
    }

    #[test]
    fn build_creates_segments_with_lengths() {
        let g = synthetic();
        assert_eq!(g.segs.len(), 4);
        // node1->node2 ~0.001 deg lat ~ 111m
        let s0 = &g.segs[0];
        assert_eq!((s0.a, s0.b, s0.way_id), (0, 1, 100));
        assert!((s0.length_m - 111.3).abs() < 2.0, "{}", s0.length_m);
    }

    #[test]
    fn segments_near_returns_local_segments_only() {
        let g = synthetic();
        let near_node2 = LatLng {
            lat: 24.1410,
            lon: 120.6800,
        };
        let found = g.segments_near(near_node2, 100.0);
        assert!(found.contains(&0) && found.contains(&1));
        assert!(
            found.contains(&2) || found.contains(&3),
            "crossing way visible"
        );

        let far = LatLng {
            lat: 24.1600,
            lon: 120.7000,
        };
        assert!(g.segments_near(far, 100.0).is_empty());
    }

    #[test]
    fn oneway_governs_exit() {
        let g = synthetic();
        // way 200 (idx 2: node4->node2, idx 3: node2->node5) is oneway fwd
        let (n4, n2, n5) = (3u32, 1u32, 4u32);
        assert_eq!(g.seg_exit(2, n4), Some(n2));
        assert_eq!(g.seg_exit(2, n2), None, "reverse barred on oneway");
        assert_eq!(g.seg_exit(3, n2), Some(n5));
        // way 100 bidirectional
        assert_eq!(g.seg_exit(0, 0), Some(1));
        assert_eq!(g.seg_exit(0, 1), Some(0));
        let _ = n5;
    }
}
