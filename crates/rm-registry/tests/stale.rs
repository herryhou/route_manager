//! `--stale` tests: bearing marks drive a full bearing rebuild + split,
//! then the snapshot marks clear. No marks -> no-op.

use rm_registry::run_stale;

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
         INSERT INTO route_stop_version
            (route_stop_version_id, subroute_version_id, stop_version_id, direction,
             stop_sequence, source_order_no, created_at)
            VALUES (1, 11, 1, 0, 1, 1, '{T0}'),
                   (2, 12, 2, 0, 1, 1, '{T0}');
         INSERT INTO path_stop_projection
            (route_stop_version_id, route_version_id, direction, stop_sequence,
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

/// Way run entries for both subroute versions, northbound, so both stops
/// re-bear via OSM.
fn insert_northbound_runs(conn: &rusqlite::Connection) {
    for sv in [11, 12] {
        conn.execute(
            "INSERT INTO seg_match_way
                (subroute_version_id, direction, ordinal, osm_way_id,
                 entry_node_id, exit_node_id, cum_start_m, cum_end_m)
             VALUES (?1, 0, 1, 100, 2, 4, 0.0, 1000.0)",
            [sv],
        )
        .unwrap();
    }
}

#[test]
fn stale_rebuilds_and_clears_marks() {
    let conn = setup();
    insert_northbound_runs(&conn);
    // stale_mark.cause_edit_id FKs edit_log: seed the log row first
    conn.execute(
        "INSERT INTO edit_log (entity, entity_id, editor, action, changes, ts)
         VALUES ('stop', 1, 'test', 'edit', '{}', '2026-01-02T00:00:00Z')",
        [],
    )
    .unwrap();
    // seed marks the way edits write them
    conn.execute(
        "INSERT INTO stale_mark (table_name, key, cause_edit_id, marked_at)
         VALUES ('stop_bearing', '*', 1, '2026-01-02T00:00:00Z')",
        [],
    )
    .unwrap();

    let run = run_stale(&conn, 1).unwrap().expect("bearing mark present -> refresh ran");
    assert!(
        run.bearings.stops_osm >= 2,
        "both stops re-bearied via OSM, got {:?}",
        run.bearings
    );
    assert_eq!(run.split.stops_moved, 0, "one heading cluster -> no split");
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0,
        "snapshot rows cleared"
    );
    let bearings: i64 = conn
        .query_row("SELECT COUNT(*) FROM stop_bearing", [], |r| r.get(0))
        .unwrap();
    assert!(bearings >= 2, "both stops re-bearied, got {bearings}");
}

#[test]
fn stale_with_no_marks_is_false_and_touches_nothing() {
    let conn = setup();
    assert!(run_stale(&conn, 1).unwrap().is_none());
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM stop_bearing", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0,
        "no rebuild happened"
    );
}
