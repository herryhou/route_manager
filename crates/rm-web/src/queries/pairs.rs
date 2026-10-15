//! Station-pair traversals: which route directions run A→B as consecutive
//! stops, clustered into corridors by cut length. Identity rule per
//! docs/segment-corridor-split.md — the registry that materialised these
//! clusters was retired in 0011_drop_segment; this derives them live.

use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;
use serde_json::Value;

/// Registry constants (docs/segment-corridor-split.md §2).
const CORRIDOR_GAP_ABS_M: f64 = 100.0;
const CORRIDOR_GAP_REL: f64 = 0.03;

/// One route-direction traversal of a directed station pair. Field names
/// are the wire keys (`GET /api/station-pair/{a}/{b}/routes`).
#[derive(Serialize)]
pub struct Traversal {
    pub route_id: i64,
    pub route_code: String,
    pub subroute_name: Option<String>,
    pub direction: i64,
    pub length_m: f64,
}

/// One corridor: traversals whose cut lengths fall in the same cluster.
#[derive(Serialize)]
pub struct Corridor {
    /// Cluster median length quantized to 10 m (the registry's corridor_key).
    pub corridor_m: f64,
    pub min_m: f64,
    pub max_m: f64,
    pub routes: Vec<Traversal>,
}

/// Corridor split (docs/segment-corridor-split.md §2): sort cuts by length,
/// split where the gap to the previous (shorter) cut exceeds
/// max(100m, 3% × shorter side). Each cluster is labelled with its median
/// length quantized to 10 m — even-count clusters take the lower median.
pub fn split_corridors(mut cuts: Vec<Traversal>) -> Vec<Corridor> {
    cuts.sort_by(|a, b| a.length_m.partial_cmp(&b.length_m).unwrap());
    let mut clusters: Vec<Vec<Traversal>> = Vec::new();
    for t in cuts {
        let split = match clusters.last().and_then(|c| c.last()) {
            Some(prev) => {
                let gap = t.length_m - prev.length_m;
                gap > CORRIDOR_GAP_ABS_M.max(CORRIDOR_GAP_REL * prev.length_m)
            }
            None => false,
        };
        if split || clusters.is_empty() {
            clusters.push(Vec::new());
        }
        clusters.last_mut().unwrap().push(t);
    }
    clusters
        .into_iter()
        .map(|routes| {
            let mut lengths: Vec<f64> = routes.iter().map(|t| t.length_m).collect();
            let median = lower_median(&mut lengths);
            Corridor {
                corridor_m: (median / 10.0).round() * 10.0,
                min_m: lengths[0],
                max_m: lengths[lengths.len() - 1],
                routes,
            }
        })
        .collect()
}

fn lower_median(sorted: &mut [f64]) -> f64 {
    sorted.select_nth_unstable_by(sorted.len() / 2, |a, b| a.partial_cmp(b).unwrap());
    match sorted.len() {
        0 => 0.0,
        n if n % 2 == 1 => sorted[n / 2],
        n => sorted[n / 2 - 1].min(sorted[n / 2]), // lower median; input sorted
    }
}

/// Wire shape of `GET /api/station-pair/{a}/{b}/routes`.
#[derive(Serialize)]
struct PairRoutesResponse {
    corridors: Vec<Corridor>,
}

/// `GET /api/station-pair/{a}/{b}/routes` — route directions traversing the
/// directed station pair (A→B) as consecutive stops, with each traversal's
/// cut length from seg_geometry_draft, clustered into corridors.
pub fn pair_routes(conn: &Connection, from_station: i64, to_station: i64) -> Result<Value> {
    let mut stmt = conn.prepare(
        "SELECT r.route_id, r.route_code, sv.subroute_name, r1.direction, gd.length_m
         FROM route_stop_current r1
         JOIN route_stop_current r2
           ON r2.subroute_version_id = r1.subroute_version_id
          AND r2.direction = r1.direction
          AND r2.stop_sequence = r1.stop_sequence + 1
         JOIN stop_current s1 ON s1.stop_version_id = r1.stop_version_id
         JOIN stop_current s2 ON s2.stop_version_id = r2.stop_version_id
         JOIN subroute_version sv ON sv.subroute_version_id = r1.subroute_version_id
         JOIN route r ON r.route_id = r1.route_id
         JOIN seg_geometry_draft gd
           ON gd.from_route_stop_version_id = r1.route_stop_version_id
          AND gd.to_route_stop_version_id = r2.route_stop_version_id
         WHERE s1.station_id = ?1 AND s2.station_id = ?2
         ORDER BY gd.length_m, r.route_id, r1.direction",
    )?;
    let cuts = stmt
        .query_map([from_station, to_station], |r| {
            Ok(Traversal {
                route_id: r.get("route_id")?,
                route_code: r.get("route_code")?,
                subroute_name: r.get("subroute_name")?,
                direction: r.get("direction")?,
                length_m: r.get("length_m")?,
            })
        })?
        .collect::<Result<_, _>>()?;
    Ok(serde_json::to_value(PairRoutesResponse {
        corridors: split_corridors(cuts),
    })?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(route_code: &str, length_m: f64) -> Traversal {
        Traversal {
            route_id: route_code.parse().unwrap(),
            route_code: route_code.into(),
            subroute_name: None,
            direction: 0,
            length_m,
        }
    }

    #[test]
    fn empty_input_yields_no_corridors() {
        assert!(split_corridors(vec![]).is_empty());
    }

    #[test]
    fn single_cut_is_one_corridor() {
        let cs = split_corridors(vec![t("1", 604.0)]);
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].corridor_m, 600.0); // quantized to 10 m
        assert_eq!(cs[0].min_m, 604.0);
        assert_eq!(cs[0].max_m, 604.0);
    }

    #[test]
    fn gap_over_absolute_threshold_splits() {
        // shorter side 1000 → threshold max(100, 30) = 100; gap 150 splits
        let cs = split_corridors(vec![t("1", 1000.0), t("2", 1150.0)]);
        assert_eq!(cs.len(), 2);
        assert_eq!(cs[0].routes[0].route_code, "1");
        assert_eq!(cs[1].routes[0].route_code, "2");
    }

    #[test]
    fn gap_under_absolute_threshold_stays() {
        let cs = split_corridors(vec![t("1", 1000.0), t("2", 1050.0)]);
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].routes.len(), 2);
    }

    #[test]
    fn relative_threshold_scales_with_shorter_side() {
        // shorter side 5000 → threshold max(100, 150) = 150; gap 200 splits
        let cs = split_corridors(vec![t("1", 5000.0), t("2", 5200.0)]);
        assert_eq!(cs.len(), 2);
    }

    #[test]
    fn unsorted_input_is_sorted_before_clustering() {
        let cs = split_corridors(vec![t("2", 1150.0), t("1", 1000.0), t("3", 1020.0)]);
        assert_eq!(cs.len(), 2);
        assert_eq!(cs[0].routes[0].route_code, "1");
        assert_eq!(cs[0].routes[1].route_code, "3");
        assert_eq!(cs[1].routes[0].route_code, "2");
    }

    #[test]
    fn even_cluster_takes_lower_median_quantized() {
        // sorted [600, 620] → lower median 600 → "600"
        let cs = split_corridors(vec![t("1", 620.0), t("2", 600.0)]);
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].corridor_m, 600.0);
        // odd cluster [604, 610, 620] → median 610
        let cs = split_corridors(vec![t("1", 604.0), t("2", 610.0), t("3", 620.0)]);
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].corridor_m, 610.0);
    }

    #[test]
    fn corridor_span_covers_cluster() {
        let cs = split_corridors(vec![t("1", 100.0), t("2", 150.0), t("3", 400.0)]);
        assert_eq!(cs.len(), 2);
        assert_eq!(cs[0].min_m, 100.0);
        assert_eq!(cs[0].max_m, 150.0);
        assert_eq!(cs[1].min_m, 400.0);
        assert_eq!(cs[1].max_m, 400.0);
    }
}
