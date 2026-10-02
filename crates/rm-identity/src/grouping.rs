//! Same-name <80 m union-find clustering for StationGroup derivation.

use std::collections::HashMap;

use rm_core::{geo, LatLng};

/// One station candidate for clustering.
#[derive(Debug, Clone)]
pub struct ClusterInput {
    pub ext_uid: String,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
}

const CHAIN_M: f64 = 80.0;

/// Cluster stations: union-find over identical names, chained when a pair
/// sits under `CHAIN_M` (haversine). Every input lands in exactly one
/// cluster; singletons become their own cluster. Cluster order follows
/// first-appearance order of members.
pub fn group_stations(inputs: &[ClusterInput]) -> Vec<Vec<usize>> {
    let mut parent: Vec<usize> = (0..inputs.len()).collect();

    fn find(parent: &mut [usize], i: usize) -> usize {
        if parent[i] != i {
            let root = find(parent, parent[i]);
            parent[i] = root;
        }
        parent[i]
    }
    fn union(parent: &mut [usize], a: usize, b: usize) {
        let (ra, rb) = (find(parent, a), find(parent, b));
        if ra != rb {
            parent[rb] = ra;
        }
    }

    let mut by_name: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, s) in inputs.iter().enumerate() {
        by_name.entry(s.name.as_str()).or_default().push(i);
    }
    for bucket in by_name.values() {
        for a in 0..bucket.len() {
            for b in (a + 1)..bucket.len() {
                let (i, j) = (bucket[a], bucket[b]);
                let d = geo::haversine_m(
                    LatLng::new(inputs[i].lat, inputs[i].lon),
                    LatLng::new(inputs[j].lat, inputs[j].lon),
                );
                if d < CHAIN_M {
                    union(&mut parent, i, j);
                }
            }
        }
    }

    let mut clusters: Vec<Vec<usize>> = Vec::new();
    let mut root_index: HashMap<usize, usize> = HashMap::new();
    for i in 0..inputs.len() {
        let root = find(&mut parent, i);
        match root_index.get(&root) {
            Some(&ci) => clusters[ci].push(i),
            None => {
                root_index.insert(root, clusters.len());
                clusters.push(vec![i]);
            }
        }
    }
    clusters
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(uid: &str, name: &str, lat: f64, lon: f64) -> ClusterInput {
        ClusterInput {
            ext_uid: uid.into(),
            name: name.into(),
            lat,
            lon,
        }
    }

    /// 干城站 area: ~28 m steps (24.1409,120.6857) -> (24.1408,120.6859) -> (24.1407,120.6861)
    #[test]
    fn chains_same_name_under_80m_transitively() {
        let inputs = vec![
            st("a", "干城站", 24.1409, 120.6857),
            st("b", "干城站", 24.1408, 120.6859),
            st("c", "干城站", 24.1407, 120.6861),
        ];
        let groups = group_stations(&inputs);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0], vec![0, 1, 2], "a-b 28m, b-c 28m chain into one");
    }

    #[test]
    fn different_name_never_groups_even_at_same_point() {
        let inputs = vec![
            st("a", "干城站", 24.1409, 120.6857),
            st("d", "樂成公園", 24.1409, 120.6857),
        ];
        let groups = group_stations(&inputs);
        assert_eq!(groups.len(), 2);
    }

    #[test]
    fn same_name_far_apart_stays_separate() {
        let inputs = vec![
            st("e", "遠方", 24.1, 120.7),
            st("f", "遠方", 24.4, 120.7), // ~33 km
            st("g", "遠方", 24.8, 120.7),
        ];
        let groups = group_stations(&inputs);
        assert_eq!(groups.len(), 3);
    }

    #[test]
    fn boundary_distance_just_under_and_over() {
        // ~0.0007 deg lat = 78 m < 80 -> group; ~0.0008 deg = 89 m -> separate
        let close = vec![st("a", "X", 24.0000, 120.7), st("b", "X", 24.0007, 120.7)];
        assert_eq!(group_stations(&close).len(), 1);
        let far = vec![st("a", "X", 24.0000, 120.7), st("b", "X", 24.0008, 120.7)];
        assert_eq!(group_stations(&far).len(), 2);
    }

    #[test]
    fn distance_check_matches_haversine() {
        let a = LatLng::new(24.0, 120.0);
        let b = LatLng::new(24.0007, 120.0);
        assert!(geo::haversine_m(a, b) < CHAIN_M && geo::haversine_m(a, b) > 70.0);
    }
}
