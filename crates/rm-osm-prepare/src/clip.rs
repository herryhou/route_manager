//! `--clip` bounding-box filter (pure, unit-tested).

use std::collections::{HashMap, HashSet};

use rm_core::LatLng;

/// Geographic bounds: south/west/north/east in degrees.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bbox {
    pub south: f64,
    pub west: f64,
    pub north: f64,
    pub east: f64,
}

impl Bbox {
    pub fn contains(&self, p: &LatLng) -> bool {
        p.lat >= self.south && p.lat <= self.north && p.lon >= self.west && p.lon <= self.east
    }

    /// Parse `south,west,north,east` in degrees; None unless exactly four
    /// floats arrive in that order.
    pub fn parse(s: &str) -> Option<Bbox> {
        let mut it = s.split(',');
        let v = [it.next()?, it.next()?, it.next()?, it.next()?];
        if it.next().is_some() {
            return None;
        }
        let mut n = [0.0f64; 4];
        for (i, x) in v.iter().enumerate() {
            n[i] = x.trim().parse().ok()?;
        }
        Some(Bbox {
            south: n[0],
            west: n[1],
            north: n[2],
            east: n[3],
        })
    }
}

/// One loaded way, as `run` holds it (only what the filter needs).
pub struct WayRow {
    pub id: i64,
    pub refs: Vec<i64>,
}

/// Keep ways with any node inside `bbox` (a way crossing the edge stays
/// whole — its outside nodes included — so connectivity across the
/// boundary survives). Generic over the caller's way type; `refs`
/// exposes each way's node refs. Returns the kept ways plus the node
/// ids they reference, for filtering the node table.
pub fn clip<T>(
    ways: Vec<T>,
    refs: impl Fn(&T) -> &[i64],
    node_pos: &HashMap<i64, LatLng>,
    bbox: &Bbox,
) -> (Vec<T>, HashSet<i64>) {
    let mut kept: Vec<T> = Vec::new();
    let mut nodes: HashSet<i64> = HashSet::new();
    for w in ways {
        let inside = refs(&w)
            .iter()
            .any(|n| node_pos.get(n).is_some_and(|p| bbox.contains(p)));
        if inside {
            nodes.extend(refs(&w).iter().copied());
            kept.push(w);
        }
    }
    (kept, nodes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(lat: f64, lon: f64) -> LatLng {
        LatLng::new(lat, lon)
    }

    fn way(id: i64, refs: &[i64]) -> WayRow {
        WayRow {
            id,
            refs: refs.to_vec(),
        }
    }

    fn node_pos() -> HashMap<i64, LatLng> {
        HashMap::from([
            (1, pos(24.00, 120.00)), // inside
            (2, pos(24.50, 120.50)), // outside (north-east)
            (3, pos(23.50, 119.50)), // outside (south-west)
            (4, pos(23.90, 119.90)), // just outside
            (5, pos(23.95, 120.05)), // inside
        ])
    }

    // bbox lat 23.95..24.05, lon 120.00..120.10
    fn bbox() -> Bbox {
        Bbox {
            south: 23.95,
            west: 120.00,
            north: 24.05,
            east: 120.10,
        }
    }

    #[test]
    fn parse_accepts_south_west_north_east_and_rejects_garbage() {
        let b = Bbox::parse("23.95,120.0,24.05,120.1").unwrap();
        assert_eq!(b, bbox());
        assert!(Bbox::parse("23.95,120.0").is_none());
        assert!(Bbox::parse("a,b,c,d").is_none());
        assert!(Bbox::parse("").is_none());
    }

    #[test]
    fn way_with_any_node_inside_is_kept_whole() {
        // node 1 inside, node 2 outside: the way stays intact and both
        // its nodes survive in the returned set (edge connectivity)
        let (kept, nodes) = clip(
            vec![way(10, &[1, 2])],
            |w: &WayRow| &w.refs,
            &node_pos(),
            &bbox(),
        );
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id, 10);
        assert!(nodes.contains(&1) && nodes.contains(&2));
    }

    #[test]
    fn fully_outside_way_is_dropped_with_its_nodes() {
        let (kept, nodes) = clip(
            vec![way(11, &[3, 4])],
            |w: &WayRow| &w.refs,
            &node_pos(),
            &bbox(),
        );
        assert!(kept.is_empty());
        assert!(!nodes.contains(&3) && !nodes.contains(&4));
    }

    #[test]
    fn boundary_touch_counts_as_inside() {
        // node 1 sits exactly on the south-west corner
        let bb = Bbox {
            south: 24.00,
            west: 120.00,
            north: 24.05,
            east: 120.10,
        };
        let (kept, _) = clip(
            vec![way(12, &[1, 2])],
            |w: &WayRow| &w.refs,
            &node_pos(),
            &bb,
        );
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn shared_node_survives_via_kept_way_when_other_way_dropped() {
        // way A dropped (all nodes outside), way B kept and shares node 4
        let mut np = node_pos();
        np.insert(6, pos(24.00, 120.05)); // inside, shared by B
        let (kept, nodes) = clip(
            vec![way(20, &[3, 4]), way(21, &[4, 6])],
            |w: &WayRow| &w.refs,
            &np,
            &bbox(),
        );
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id, 21);
        assert!(nodes.contains(&4) && nodes.contains(&6));
        assert!(!nodes.contains(&3));
    }
}
