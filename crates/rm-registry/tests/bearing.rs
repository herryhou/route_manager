//! Stop/station bearing tests over a hand-built DB: an N-S road, one pole
//! east of it, stops whose chainage lands mid-run.

use rm_core::{polyline, LatLng};

const T0: &str = "2026-01-01T00:00:00Z";

/// Road along lon 120.6857: nodes 1..4 south -> north. One station (pole)
/// 30 m east of the road; two stops (routes 1 and 2, direction 0) on it.
fn setup() -> rusqlite::Connection {
    let conn = rm_db::open_in_memory().unwrap();
    conn.execute_batch(&format!(
        "INSERT INTO route (route_id, route_code, ext_uid, revision, created_at, updated_at)
            VALUES (1, '1', 'tcg:1', 1, '{T0}', '{T0}'),
                   (2, '2', 'tcg:2', 1, '{T0}', '{T0}');
         INSERT INTO route_version
            (route_version_id, route_id, version_no, status, valid_from, created_at)
            VALUES (11, 1, 1, 'PUBLISHED', '{T0}', '{T0}'),
                   (12, 2, 1, 'PUBLISHED', '{T0}', '{T0}');
         UPDATE route SET current_route_version_id = 11 WHERE route_id = 1;
         UPDATE route SET current_route_version_id = 12 WHERE route_id = 2;
         INSERT INTO subroute (subroute_id, route_id, subroute_code, created_at) VALUES
            (11, 1, '1', '{T0}'), (12, 2, '2', '{T0}');
         INSERT INTO subroute_version
            (subroute_version_id, route_version_id, subroute_id, subroute_name, created_at)
            VALUES (11, 11, 11, 'A - B', '{T0}'), (12, 12, 12, 'C - D', '{T0}');
         INSERT INTO station (station_id, ext_uid, created_at)
            VALUES (1, 'tcg:node:1', '{T0}');
         INSERT INTO stop (stop_id, ext_uid, created_at)
            VALUES (1, 'tcg:1:0:1', '{T0}'), (2, 'tcg:2:0:1', '{T0}');
         INSERT INTO stop_version
            (stop_version_id, stop_id, station_id, stop_code, stop_name, direction,
             latitude, longitude, valid_from, created_at)
            VALUES (1, 1, 1, '1', 'X', 0, 24.1407, 120.6860, '{T0}', '{T0}'),
                   (2, 2, 1, '2', 'X', 0, 24.1407, 120.6860, '{T0}', '{T0}');
         INSERT INTO stop_of_route_version
            (stop_of_route_version_id, subroute_version_id, stop_version_id, direction,
             stop_sequence, source_order_no, created_at)
            VALUES (1, 11, 1, 0, 1, 1, '{T0}'),
                   (2, 12, 2, 0, 1, 1, '{T0}');
         INSERT INTO path_stop_projection
            (stop_of_route_version_id, route_version_id, direction, stop_sequence,
             chainage_m, snap_dist_m, method, monotonic_clamped, projected_lat, projected_lon)
            VALUES (1, 11, 0, 1, 400.0, 30.0, 'PROJECTED', 0, 24.1407, 120.6857),
                   (2, 12, 0, 1, 400.0, 30.0, 'PROJECTED', 0, 24.1407, 120.6857);
         INSERT INTO osm_way (osm_way_id, highway, oneway, name)
            VALUES (100, 'residential', 0, '南北路');
         INSERT INTO osm_node (osm_node_id, lat, lon) VALUES
            (1, 24.1400, 120.6857),
            (2, 24.1405, 120.6857),
            (3, 24.1410, 120.6857),
            (4, 24.1415, 120.6857);
         INSERT INTO osm_way_node (osm_way_id, ordinal, osm_node_id) VALUES
            (100, 1, 1), (100, 2, 2), (100, 3, 3), (100, 4, 4);"
    ))
    .unwrap();
    conn
}

/// Way run entry node 2 -> exit node 4 (northbound), cumulative 0..1000.
fn insert_northbound_run(conn: &rusqlite::Connection) {
    conn.execute(
        "INSERT INTO seg_match_way
            (subroute_version_id, direction, ordinal, osm_way_id,
             entry_node_id, exit_node_id, cum_start_m, cum_end_m)
         VALUES (11, 0, 1, 100, 2, 4, 0.0, 1000.0)",
        [],
    )
    .unwrap();
}

fn bearing(conn: &rusqlite::Connection, stop_version_id: i64) -> (f64, String) {
    conn.query_row(
        "SELECT bearing_deg, source FROM stop_bearing WHERE stop_version_id = ?1",
        [stop_version_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .unwrap()
}

#[test]
fn osm_bearing_follows_run_travel_direction() {
    let conn = setup();
    insert_northbound_run(&conn);

    rm_registry::bearing::run(&conn, 1).unwrap();

    let (deg, src) = bearing(&conn, 1);
    assert!((deg - 0.0).abs() < 0.5, "northbound run got {deg}");
    assert_eq!(src, "OSM");

    // same run reversed: travel southbound
    conn.execute(
        "UPDATE seg_match_way SET entry_node_id = 4, exit_node_id = 2",
        [],
    )
    .unwrap();
    rm_registry::bearing::run(&conn, 1).unwrap(); // idempotent rebuild

    let (deg, _) = bearing(&conn, 1);
    assert!((deg - 180.0).abs() < 0.5, "reversed run got {deg}");
}

#[test]
fn polyline_fallback_when_no_runs() {
    let conn = setup();
    // eastbound polyline: two ~950 m legs due east
    let enc = polyline::encode(&[
        LatLng::new(24.1407, 120.6857),
        LatLng::new(24.1407, 120.6957),
        LatLng::new(24.1407, 120.7057),
    ]);
    conn.execute(
        "INSERT INTO src_route_point (batch_id, xno, direction, encoded)
         VALUES (1, '1', 0, ?1)",
        [&enc],
    )
    .unwrap();

    rm_registry::bearing::run(&conn, 1).unwrap();

    let (deg, src) = bearing(&conn, 1);
    assert!((deg - 90.0).abs() < 0.5, "eastbound polyline got {deg}");
    assert_eq!(src, "POLYLINE");
}

#[test]
fn stop_without_geometry_is_skipped() {
    let conn = setup(); // no runs, no polyline

    let s = rm_registry::bearing::run(&conn, 1).unwrap();

    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM stop_bearing", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
    let st: i64 = conn
        .query_row("SELECT COUNT(*) FROM station_bearing", [], |r| r.get(0))
        .unwrap();
    assert_eq!(st, 0);
    assert_eq!(s.stops_skipped, 2);
}

#[test]
fn stop_visited_twice_in_direction_yields_one_row() {
    let conn = setup();
    insert_northbound_run(&conn);
    // cycled route: the same stop_version (route 1, seq 1) visited again
    // at sequence 2 — one bearing row, last visit wins
    conn.execute(
        "INSERT INTO stop_of_route_version
            (stop_of_route_version_id, subroute_version_id, stop_version_id, direction,
             stop_sequence, source_order_no, created_at)
         VALUES (3, 11, 1, 0, 2, 2, '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO path_stop_projection
            (stop_of_route_version_id, route_version_id, direction, stop_sequence,
             chainage_m, snap_dist_m, method, monotonic_clamped, projected_lat, projected_lon)
         VALUES (3, 11, 0, 2, 800.0, 30.0, 'PROJECTED', 0, 24.1412, 120.6857)",
        [],
    )
    .unwrap();

    rm_registry::bearing::run(&conn, 1).unwrap();

    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM stop_bearing WHERE stop_version_id = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn station_aggregates_member_bearings() {
    let conn = setup();
    insert_northbound_run(&conn); // stop 1 -> bearing ~0 (OSM)
                                  // stop 2 via eastbound polyline -> bearing ~90
    let enc = polyline::encode(&[
        LatLng::new(24.1407, 120.6857),
        LatLng::new(24.1407, 120.6957),
        LatLng::new(24.1407, 120.7057),
    ]);
    conn.execute(
        "INSERT INTO src_route_point (batch_id, xno, direction, encoded)
         VALUES (1, '2', 0, ?1)",
        [&enc],
    )
    .unwrap();

    rm_registry::bearing::run(&conn, 1).unwrap();

    let (mean, members, spread): (f64, i64, f64) = conn
        .query_row(
            "SELECT bearing_deg, member_count, max_spread_deg
             FROM station_bearing WHERE station_id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(members, 2);
    assert!((mean - 45.0).abs() < 0.5, "circular mean got {mean}");
    assert!((spread - 90.0).abs() < 0.5, "max spread got {spread}");
}
