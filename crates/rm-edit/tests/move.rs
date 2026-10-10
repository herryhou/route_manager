//! Station move: load → plan → apply roundtrips through the service.

use rm_db::open_in_memory;
use rm_edit::{move_station, EditError, StationMove};

/// Same shape as tests/rename.rs's fixture: station 20 ("S") with poles
/// 10/11, rsv row, bearing row, id sequences seeded.
fn fixture(conn: &rusqlite::Connection) -> i64 {
    conn.execute_batch(
        "INSERT INTO station_group (station_group_id, created_at) VALUES (1, '2026-01-01T00:00:00Z');
         INSERT INTO station_group_version (station_group_version_id, station_group_id, station_group_name, source, valid_from, created_at)
             VALUES (1, 1, 'g', 'capture', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
         INSERT INTO station (station_id, ext_uid, created_at) VALUES (1, 'tcg:node:1', '2026-01-01T00:00:00Z');
         INSERT INTO station_version (station_version_id, station_id, station_group_id, station_name, latitude, longitude, valid_from, created_at)
             VALUES (20, 1, 1, 'S', 24.0, 120.0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
         INSERT INTO stop (stop_id, ext_uid, created_at) VALUES (1, 'tcg:stop:1', '2026-01-01T00:00:00Z');
         INSERT INTO stop (stop_id, ext_uid, created_at) VALUES (2, 'tcg:stop:2', '2026-01-01T00:00:00Z');
         INSERT INTO stop_version (stop_version_id, stop_id, station_id, stop_code, stop_name, direction, latitude, longitude, valid_from, created_at)
             VALUES (10, 1, 1, '001', 'S', 0, 24.0, 120.0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
         INSERT INTO stop_version (stop_version_id, stop_id, station_id, stop_code, stop_name, direction, latitude, longitude, valid_from, created_at)
             VALUES (11, 2, 1, '001', 'S 往', 0, 24.0, 120.0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
         INSERT INTO route (route_id, route_code, ext_uid, created_at, updated_at)
             VALUES (1, '1', 'tcg:1', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
         INSERT INTO route_version (route_version_id, route_id, version_no, valid_from, status, created_at)
             VALUES (1, 1, 1, '2026-01-01T00:00:00Z', 'PUBLISHED', '2026-01-01T00:00:00Z');
         INSERT INTO subroute (subroute_id, route_id, subroute_code, created_at)
             VALUES (1, 1, '1-0', '2026-01-01T00:00:00Z');
         INSERT INTO subroute_version (subroute_version_id, route_version_id, subroute_id, subroute_name, created_at)
             VALUES (1, 1, 1, '1', '2026-01-01T00:00:00Z');
         INSERT INTO route_stop_version (route_stop_version_id, subroute_version_id, stop_version_id, direction, stop_sequence, created_at)
             VALUES (1, 1, 10, 0, 1, '2026-01-01T00:00:00Z');
         INSERT INTO stop_bearing (stop_version_id, bearing_deg, source, computed_at)
             VALUES (10, 123.5, 'OSM', '2026-01-02T00:00:00Z');
         INSERT INTO id_sequence (table_name, next_value) VALUES ('station_version', 21);
         INSERT INTO id_sequence (table_name, next_value) VALUES ('stop_version', 12);",
    )
    .unwrap();
    20
}

fn cmd(svi: i64) -> StationMove {
    StationMove { station_version_id: svi, latitude: 24.5, longitude: 120.5 }
}

#[test]
fn move_station_moves_end_to_end() {
    let conn = open_in_memory().unwrap();
    let old = fixture(&conn);
    let out = move_station(&conn, cmd(old), "herry", "fix").unwrap();
    assert_eq!(out.new_station_version_id, 21);
    let (lat, lon, name, src): (f64, f64, String, String) = conn
        .query_row(
            "SELECT latitude, longitude, station_name, source FROM station_version
             WHERE valid_to IS NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!((lat, lon, name.as_str(), src.as_str()), (24.5, 120.5, "S", "manual"));
    // zero stale marks, poles untouched (no manual stop versions minted)
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0);
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM stop_version WHERE source = 'manual'", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0);
    let _ = EditError::StationNotCurrent(0); // variant is public
}

#[test]
fn move_station_rejects_stale_version() {
    let conn = open_in_memory().unwrap();
    let old = fixture(&conn);
    move_station(&conn, cmd(old), "herry", "first").unwrap();
    // double-submit: 20 is no longer current
    let err = move_station(&conn, cmd(old), "herry", "second").unwrap_err();
    assert!(err.to_string().contains("not the current version"), "{err}");
}

#[test]
fn move_station_rejects_unknown_version() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let err = move_station(&conn, cmd(999), "herry", "").unwrap_err();
    assert!(err.to_string().contains("not the current version"), "{err}");
}
