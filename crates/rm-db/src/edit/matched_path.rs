//! Matched-path loader + projection helpers for the add_stop edit: the
//! direction's matched OSM path in travel order, the nearest-point
//! projection of a proposed coordinate onto it, and the forward tangent
//! at a chainage.

use anyhow::Result;
use rm_core::{geo, LatLng};
use rusqlite::{Connection, OptionalExtension};

/// Maximum off-path distance (m) for a proposed stop coordinate to count
/// as lying on the route's matched path.
pub const OFF_PATH_MAX_M: f64 = 500.0;

/// The direction's matched OSM path in travel order:
/// `route.current_route_version_id` -> the version's subroute_version ->
/// its `seg_match_way` runs -> each way's node slice entry -> exit,
/// concatenated (registry `osm_bearing`'s slicing, across the whole run).
/// `None` when the route has no current version, no subroute_version, or
/// no matched runs for the direction.
pub fn matched_path(
    conn: &Connection,
    route_id: i64,
    direction: i64,
) -> Result<Option<Vec<LatLng>>> {
    let route_version_id: Option<i64> = conn
        .query_row(
            "SELECT current_route_version_id FROM route WHERE route_id = ?1",
            [route_id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let Some(route_version_id) = route_version_id else {
        return Ok(None);
    };
    let subroute_version_id: Option<i64> = conn
        .query_row(
            "SELECT subroute_version_id FROM subroute_version
             WHERE route_version_id = ?1
             ORDER BY subroute_version_id LIMIT 1",
            [route_version_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(subroute_version_id) = subroute_version_id else {
        return Ok(None);
    };
    let mut runs_stmt = conn.prepare(
        "SELECT osm_way_id, entry_node_id, exit_node_id FROM seg_match_way
         WHERE subroute_version_id = ?1 AND direction = ?2
         ORDER BY ordinal",
    )?;
    let runs: Vec<(i64, i64, i64)> = runs_stmt
        .query_map([subroute_version_id, direction], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if runs.is_empty() {
        return Ok(None);
    }

    let mut path: Vec<LatLng> = Vec::new();
    for (way_id, entry_node_id, exit_node_id) in runs {
        // way nodes in ordinal order, coords joined in (no FK keeps
        // osm_way_node -> osm_node honest, so a missing row is loud here
        // rather than a silently shortened path)
        let mut nodes_stmt = conn.prepare(
            "SELECT wn.osm_node_id, n.lat, n.lon
             FROM osm_way_node wn
             JOIN osm_node n ON n.osm_node_id = wn.osm_node_id
             WHERE wn.osm_way_id = ?1
             ORDER BY wn.ordinal",
        )?;
        let nodes: Vec<(i64, f64, f64)> = nodes_stmt
            .query_map([way_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        let entry = nodes.iter().position(|(n, _, _)| *n == entry_node_id);
        let exit = nodes.iter().position(|(n, _, _)| *n == exit_node_id);
        let (Some(i), Some(j)) = (entry, exit) else {
            anyhow::bail!(
                "seg_match_way way {way_id}: run endpoint \
                 {entry_node_id}/{exit_node_id} not among the way's nodes"
            );
        };
        if i == j {
            anyhow::bail!("seg_match_way way {way_id}: entry == exit ({entry_node_id})");
        }
        let mut slice: Vec<LatLng> = if i <= j {
            nodes[i..=j]
                .iter()
                .map(|(_, lat, lon)| LatLng::new(*lat, *lon))
                .collect()
        } else {
            nodes[j..=i]
                .iter()
                .rev()
                .map(|(_, lat, lon)| LatLng::new(*lat, *lon))
                .collect()
        };
        // consecutive ways share the boundary node — keep it once so no
        // zero-length leg lands at the seam
        if path.last() == slice.first() {
            slice.remove(0);
        }
        path.extend(slice);
    }
    Ok(Some(path))
}

/// Nearest point on `path` for `p`: `(chainage_m, dist_m)` — arc position
/// along the path and the off-path distance. Per-segment nearest point in
/// equirectangular-scaled metres (registry `point_to_segment_m`); the
/// chainage accumulates `haversine_m` over whole legs plus the in-leg
/// offset. `None` for paths shorter than 2 points.
pub fn project_onto_path(path: &[LatLng], p: LatLng) -> Option<(f64, f64)> {
    if path.len() < 2 {
        return None;
    }
    let mut best: Option<(f64, f64)> = None;
    let mut cum = 0.0;
    for w in path.windows(2) {
        let (proj, dist) = geo::point_to_segment_m(p, w[0], w[1]);
        if best.map(|(_, d)| dist < d).unwrap_or(true) {
            let chainage = cum + geo::haversine_m(w[0], proj);
            best = Some((chainage, dist));
        }
        cum += geo::haversine_m(w[0], w[1]);
    }
    best
}

/// Forward tangent of `path` at `chainage_m`; at/beyond the end, the
/// final leg (registry `polyline_bearing` semantics — arrival heading
/// for a stop at the run's tail). `None` for paths shorter than 2 points.
pub fn bearing_at(path: &[LatLng], chainage_m: f64) -> Option<f64> {
    if path.len() < 2 {
        return None;
    }
    let mut cum = 0.0;
    for w in path.windows(2) {
        let leg = geo::haversine_m(w[0], w[1]);
        if chainage_m <= cum + leg || w[1] == *path.last().unwrap() {
            return Some(geo::bearing_deg(w[0], w[1]));
        }
        cum += leg;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_in_memory;
    use rm_core::{geo, LatLng};
    use rusqlite::Connection;

    const T0: &str = "2026-01-01T00:00:00Z";

    /// N-S road at lon 120.6857 split into two ways sharing node 3:
    /// way 100 = nodes 1..3, way 101 = nodes 3..5. Route 1 published
    /// via route_version 11 -> subroute_version 11.
    fn setup() -> Connection {
        let conn = open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "INSERT INTO route (route_id, route_code, ext_uid, revision, created_at, updated_at)
                VALUES (1, '1', 'tcg:1', 1, '{T0}', '{T0}');
             INSERT INTO route_version
                (route_version_id, route_id, version_no, status, valid_from, created_at)
                VALUES (11, 1, 1, 'PUBLISHED', '{T0}', '{T0}');
             UPDATE route SET current_route_version_id = 11 WHERE route_id = 1;
             INSERT INTO subroute (subroute_id, route_id, subroute_code, created_at)
                VALUES (11, 1, '1', '{T0}');
             INSERT INTO subroute_version
                (subroute_version_id, route_version_id, subroute_id, subroute_name, created_at)
                VALUES (11, 11, 11, 'A - B', '{T0}');
             INSERT INTO osm_way (osm_way_id, highway, oneway, name)
                VALUES (100, 'residential', 0, '南北路'),
                       (101, 'residential', 0, '南北路');
             INSERT INTO osm_node (osm_node_id, lat, lon) VALUES
                (1, 24.1400, 120.6857),
                (2, 24.1405, 120.6857),
                (3, 24.1410, 120.6857),
                (4, 24.1415, 120.6857),
                (5, 24.1420, 120.6857);
             INSERT INTO osm_way_node (osm_way_id, ordinal, osm_node_id) VALUES
                (100, 1, 1), (100, 2, 2), (100, 3, 3),
                (101, 1, 3), (101, 2, 4), (101, 3, 5);"
        ))
        .unwrap();
        conn
    }

    /// Two-way run along the road: way 100 entry 1 -> exit 3, then
    /// way 101 entry 3 -> exit 5.
    fn insert_two_way_run(conn: &Connection) {
        conn.execute(
            "INSERT INTO seg_match_way
                (subroute_version_id, direction, ordinal, osm_way_id,
                 entry_node_id, exit_node_id, cum_start_m, cum_end_m)
             VALUES (11, 0, 1, 100, 1, 3, 0.0, 200.0),
                    (11, 0, 2, 101, 3, 5, 200.0, 400.0)",
            [],
        )
        .unwrap();
    }

    #[test]
    fn matched_path_returns_points_in_travel_order() {
        let conn = setup();
        insert_two_way_run(&conn);

        let path = matched_path(&conn, 1, 0).unwrap().unwrap();

        assert_eq!(
            path,
            vec![
                LatLng::new(24.1400, 120.6857),
                LatLng::new(24.1405, 120.6857),
                LatLng::new(24.1410, 120.6857),
                LatLng::new(24.1415, 120.6857),
                LatLng::new(24.1420, 120.6857),
            ],
            "two ways sliced entry -> exit; shared boundary node kept once"
        );
    }

    #[test]
    fn matched_path_none_without_match_rows() {
        let conn = setup(); // current version + subroute_version, zero runs

        assert!(matched_path(&conn, 1, 0).unwrap().is_none());
    }

    #[test]
    fn matched_path_none_for_unpublished_route() {
        let conn = setup();
        insert_two_way_run(&conn);
        conn.execute("UPDATE route SET current_route_version_id = NULL", [])
            .unwrap();

        assert!(matched_path(&conn, 1, 0).unwrap().is_none());
    }

    #[test]
    fn project_onto_path_distance_and_chainage() {
        // shorter than 2 points: no projection
        let solo = [LatLng::new(24.1400, 120.6857)];
        assert!(project_onto_path(&solo, LatLng::new(24.1450, 120.6860)).is_none());

        // meridian path; p sits 0.0003 deg due east of its midpoint, so
        // the perpendicular foot lands at half the leg:
        // chainage = haversine over 0.005 deg of latitude,
        // dist     = 0.0003 deg of longitude at lat 24.14
        let path = [
            LatLng::new(24.1400, 120.6857),
            LatLng::new(24.1500, 120.6857),
        ];
        let (chainage, dist) = project_onto_path(&path, LatLng::new(24.1450, 120.6860)).unwrap();
        assert!(
            (chainage - 555.975401167554).abs() < 1e-6,
            "chainage {chainage}"
        );
        assert!((dist - 30.4412839909219).abs() < 1e-6, "dist {dist}");
    }

    #[test]
    fn bearing_at_mid_and_beyond_end() {
        let a = LatLng::new(24.1400, 120.6857);
        let b = LatLng::new(24.1500, 120.6857);
        let c = LatLng::new(24.1500, 120.6957);
        let path = [a, b, c];
        let (leg1, leg2) = (geo::haversine_m(a, b), geo::haversine_m(b, c));

        // shorter than 2 points: no tangent
        assert!(bearing_at(&[a], 0.0).is_none());
        // mid first leg: due north
        let mid = bearing_at(&path, leg1 * 0.5).unwrap();
        assert!(mid.abs() < 0.01, "mid got {mid}");
        // past the end: final leg, due east (great-circle initial bearing
        // at equal latitudes sits a few millidegrees off 90)
        let past = bearing_at(&path, leg1 + leg2 + 1000.0).unwrap();
        assert!((past - 90.0).abs() < 0.01, "past-end got {past}");
    }
}
