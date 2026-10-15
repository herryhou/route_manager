//! Dangling-busway-end stitching (pure, unit-tested).
//!
//! OSM connects median bus lanes to the road network through
//! footway/cycleway crossings (e.g. the 仁愛路 counterflow lane at 東門
//! and 敦南 roundabouts); the highway whitelist drops those, leaving the
//! busway ends dangling and the matcher detouring through back alleys.
//! Each dangling end gets a short synthetic bidirectional way to the
//! nearest node of another way.

use std::collections::{HashMap, HashSet};

use rm_core::geo::haversine_m;
use rm_core::LatLng;

/// Maximum distance from a dangling busway end to a road node of another
/// way for a synthetic connector to be created.
pub const BUSWAY_SNAP_MAX_M: f64 = 25.0;

/// A way as loaded by [`crate::run`](super::run): id, highway, refs.
pub struct Way {
    pub id: i64,
    pub highway: String,
    pub refs: Vec<i64>,
}

/// Synthetic connector produced for one dangling busway end.
pub struct Connector {
    pub id: i64,
    pub from_node: i64,
    pub to_node: i64,
}

/// One connector per dangling `highway=busway` way end to the nearest
/// node of *each* other non-busway way within [`BUSWAY_SNAP_MAX_M`] —
/// several, because the geometrically nearest way can be legally
/// unreachable in the bus's direction of travel (the 東門 roundabout:
/// nearest is the westbound 仁愛路一段 carriageway, the usable entry is
/// the roundabout arc 24m away). `node_pos` must cover every way node.
/// Ids are assigned descending from -1.
pub fn stitch_busway_ends(ways: &[Way], node_pos: &HashMap<i64, LatLng>) -> Vec<Connector> {
    // node -> indices of ways referencing it
    let mut refs_of: HashMap<i64, Vec<usize>> = HashMap::new();
    for (i, w) in ways.iter().enumerate() {
        for n in &w.refs {
            refs_of.entry(*n).or_default().push(i);
        }
    }

    let mut out = Vec::new();
    let mut seen_pairs: HashSet<(i64, i64)> = HashSet::new();
    for (wi, w) in ways.iter().enumerate() {
        if w.highway != "busway" {
            continue;
        }
        for end in [w.refs.first(), w.refs.last()] {
            let Some(end) = end else { continue };
            // dangling: no other way (and no repeated self-reference) here
            let other = |idxs: &[usize]| idxs.iter().any(|i| *i != wi);
            let self_loop = refs_of[end].len() > 1; // this way hits the node twice
            if other(&refs_of[end]) || self_loop {
                continue;
            }
            let Some(p) = node_pos.get(end) else { continue };
            // nearest node per distinct non-busway way within range
            let mut best_per_way: HashMap<usize, (f64, i64)> = HashMap::new();
            for (n, idxs) in refs_of.iter() {
                if n == end || !other(idxs) {
                    continue;
                }
                if idxs.iter().all(|i| ways[*i].highway == "busway") {
                    continue; // no lane-hopping between busways
                }
                let Some(q) = node_pos.get(n) else { continue };
                let d = haversine_m(*p, *q);
                if d > BUSWAY_SNAP_MAX_M {
                    continue;
                }
                for &i in idxs.iter() {
                    if ways[i].highway == "busway" {
                        continue;
                    }
                    let e = best_per_way.entry(i).or_insert((f64::INFINITY, *n));
                    if d < e.0 {
                        *e = (d, *n);
                    }
                }
            }
            let mut targets: Vec<(f64, i64)> = best_per_way.values().copied().collect::<Vec<_>>();
            targets.sort_by(|a, b| a.0.total_cmp(&b.0));
            for (_, target) in targets {
                if seen_pairs.insert((*end, target)) {
                    out.push(Connector {
                        id: -(out.len() as i64 + 1),
                        from_node: *end,
                        to_node: target,
                    });
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn way(id: i64, highway: &str, refs: &[i64]) -> Way {
        Way {
            id,
            highway: highway.to_string(),
            refs: refs.to_vec(),
        }
    }

    fn pos(lat: f64) -> LatLng {
        LatLng::new(lat, 121.5)
    }

    #[test]
    fn dangling_end_near_road_gets_connector() {
        // busway node 1 dangles; road way node 2 is ~5.5m north
        let ways = [way(10, "busway", &[1, 9]), way(20, "secondary", &[2, 3])];
        let mut np = HashMap::new();
        np.insert(1, pos(25.0390));
        np.insert(2, pos(25.0390 + 5.0 / 111_000.0));
        np.insert(3, pos(25.0400));
        np.insert(9, pos(25.0385));
        let cs = stitch_busway_ends(&ways, &np);
        assert_eq!(cs.len(), 1);
        assert_eq!((cs[0].from_node, cs[0].to_node), (1, 2));
        assert_eq!(cs[0].id, -1);
    }

    #[test]
    fn one_connector_per_distinct_way() {
        // two secondary ways both within range -> one connector each;
        // the farther one is kept even though a nearer way exists
        let ways = [
            way(10, "busway", &[1, 9]),
            way(20, "secondary", &[2, 3]),
            way(30, "tertiary", &[4, 5]),
        ];
        let mut np = HashMap::new();
        np.insert(1, pos(25.0390));
        np.insert(2, pos(25.0390 + 4.0 / 111_000.0));
        np.insert(3, pos(25.0400));
        np.insert(4, pos(25.0390 + 20.0 / 111_000.0));
        np.insert(5, pos(25.0380));
        np.insert(9, pos(25.0385));
        let cs = stitch_busway_ends(&ways, &np);
        let mut pairs: Vec<_> = cs.iter().map(|c| (c.from_node, c.to_node)).collect();
        pairs.sort();
        assert_eq!(pairs, vec![(1, 2), (1, 4)]);
    }

    #[test]
    fn other_busway_is_not_a_target() {
        // a parallel busway's end 7m away must not be stitched to
        let ways = [
            way(10, "busway", &[1, 9]),
            way(11, "busway", &[2, 8]),
            way(20, "secondary", &[3, 4]),
        ];
        let mut np = HashMap::new();
        np.insert(1, pos(25.0390));
        np.insert(2, pos(25.0390 + 7.0 / 111_000.0));
        np.insert(3, pos(25.0390 - 20.0 / 111_000.0)); // 27m from node 2
        np.insert(4, pos(25.0400));
        np.insert(8, pos(25.0300));
        np.insert(9, pos(25.0385));
        let cs = stitch_busway_ends(&ways, &np);
        let pairs: Vec<_> = cs.iter().map(|c| (c.from_node, c.to_node)).collect();
        assert_eq!(pairs, vec![(1, 3)]);
    }

    #[test]
    fn shared_end_gets_no_connector() {
        // node 1 shared between busway and road -> already connected
        let ways = [way(10, "busway", &[1, 9]), way(20, "secondary", &[1, 3])];
        let mut np = HashMap::new();
        np.insert(1, pos(25.0390));
        np.insert(3, pos(25.0400));
        np.insert(9, pos(25.0385));
        assert!(stitch_busway_ends(&ways, &np).is_empty());
    }

    #[test]
    fn nearest_beyond_max_gets_no_connector() {
        let ways = [way(10, "busway", &[1, 9]), way(20, "secondary", &[2, 3])];
        let mut np = HashMap::new();
        np.insert(1, pos(25.0390));
        np.insert(2, pos(25.0400)); // ~110m away
        np.insert(3, pos(25.0410));
        np.insert(9, pos(25.0385));
        assert!(stitch_busway_ends(&ways, &np).is_empty());
    }

    #[test]
    fn non_busway_dangling_end_ignored() {
        let ways = [way(10, "service", &[1, 9]), way(20, "secondary", &[2, 3])];
        let mut np = HashMap::new();
        np.insert(1, pos(25.0390));
        np.insert(2, pos(25.03905));
        np.insert(3, pos(25.0400));
        np.insert(9, pos(25.0385));
        assert!(stitch_busway_ends(&ways, &np).is_empty());
    }
}
