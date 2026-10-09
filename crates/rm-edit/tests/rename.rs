//! Integration: rename_station end-to-end on an in-memory DB.

use rm_edit::{rename_station, EditError, RenameLevel, StationRename};
use rm_db::open_in_memory;
use rusqlite::Connection;

/// Same shape as rm-db's edit tests fixture: station 'S' (sv 20) + poles
/// sv 10 'S' (bearing row, rsv) and sv 11 'S 往' (no bearing).
fn fixture(conn: &Connection) {
    conn.execute(
        "INSERT INTO station_group (station_group_id, created_at) VALUES (1, '2026-01-01T00:00:00Z')", []).unwrap();
    conn.execute(
        "INSERT INTO station_group_version (station_group_version_id, station_group_id, station_group_name, source, valid_from, created_at)
         VALUES (1, 1, 'g', 'capture', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')", []).unwrap();
    conn.execute(
        "INSERT INTO station (station_id, ext_uid, created_at) VALUES (1, 'tcg:node:1', '2026-01-01T00:00:00Z')", []).unwrap();
    conn.execute(
        "INSERT INTO station_version (station_version_id, station_id, station_group_id, station_name, latitude, longitude, valid_from, created_at)
         VALUES (20, 1, 1, 'S', 24.0, 120.0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')", []).unwrap();
    for (svi, sid, name) in [(10, 1, "S"), (11, 2, "S 往")] {
        conn.execute("INSERT INTO stop (stop_id, ext_uid, created_at) VALUES (?1, 'tcg:stop:' || ?1, '2026-01-01T00:00:00Z')", [sid]).unwrap();
        conn.execute(
            "INSERT INTO stop_version (stop_version_id, stop_id, station_id, stop_code, stop_name, direction, latitude, longitude, valid_from, created_at)
             VALUES (?1, ?2, 1, '001', ?3, 0, 24.0, 120.0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            rusqlite::params![svi, sid, name]).unwrap();
    }
    conn.execute(
        "INSERT INTO route (route_id, route_code, ext_uid, created_at, updated_at)
         VALUES (1, '1', 'tcg:1', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')", []).unwrap();
    conn.execute(
        "INSERT INTO route_version (route_version_id, route_id, version_no, valid_from, status, created_at)
         VALUES (1, 1, 1, '2026-01-01T00:00:00Z', 'PUBLISHED', '2026-01-01T00:00:00Z')", []).unwrap();
    conn.execute(
        "INSERT INTO subroute (subroute_id, route_id, subroute_code, created_at)
         VALUES (1, 1, '1-0', '2026-01-01T00:00:00Z')", []).unwrap();
    conn.execute(
        "INSERT INTO subroute_version (subroute_version_id, route_version_id, subroute_id, subroute_name, created_at)
         VALUES (1, 1, 1, '1', '2026-01-01T00:00:00Z')", []).unwrap();
    conn.execute(
        "INSERT INTO route_stop_version (route_stop_version_id, subroute_version_id, stop_version_id, direction, stop_sequence, created_at)
         VALUES (1, 1, 10, 0, 1, '2026-01-01T00:00:00Z')", []).unwrap();
    conn.execute(
        "INSERT INTO stop_bearing (stop_version_id, bearing_deg, source, computed_at)
         VALUES (10, 123.5, 'OSM', '2026-01-02T00:00:00Z')", []).unwrap();
    conn.execute("INSERT INTO id_sequence (table_name, next_value) VALUES ('station_version', 21)", []).unwrap();
    conn.execute("INSERT INTO id_sequence (table_name, next_value) VALUES ('stop_version', 12)", []).unwrap();
}

fn cmd(new_name: &str, level: RenameLevel) -> StationRename {
    StationRename { station_version_id: 20, new_name: new_name.into(), level }
}

#[test]
fn rename_all_poles_end_to_end() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let out = rename_station(&conn, cmd("台北車站", RenameLevel::AllPoles), "herry", "cjk").unwrap();
    assert_eq!(out.poles.len(), 2);
    // station renamed
    let name: String = conn.query_row(
        "SELECT station_name FROM station_version WHERE valid_to IS NULL", [], |r| r.get(0)).unwrap();
    assert_eq!(name, "台北車站");
    // matching pole followed, non-matching also followed (AllPoles), bearing carried
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM stop_bearing b JOIN stop_version sv ON sv.stop_version_id = b.stop_version_id
         WHERE sv.stop_name = '台北車站'", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 1);
    // no stale marks anywhere
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0);
}

#[test]
fn rename_matching_poles_follows_only_exact_name() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    rename_station(&conn, cmd("N", RenameLevel::MatchingPoles), "herry", "").unwrap();
    // pole named 'S' followed; 'S 往' untouched
    let mut stmt = conn.prepare(
        "SELECT stop_name FROM stop_version WHERE valid_to IS NULL ORDER BY stop_id").unwrap();
    let names: Vec<String> = stmt.query_map([], |r| r.get(0)).unwrap()
        .collect::<Result<_, _>>().unwrap();
    assert_eq!(names, vec!["N".to_string(), "S 往".to_string()]);
}

#[test]
fn rename_station_only_touches_no_poles() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let out = rename_station(&conn, cmd("N", RenameLevel::StationOnly), "herry", "").unwrap();
    assert!(out.poles.is_empty());
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM stop_version WHERE source = 'manual'", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0, "no pole churned");
}

#[test]
fn rename_rejects_stale_station_version() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    rename_station(&conn, cmd("N", RenameLevel::AllPoles), "herry", "first").unwrap();
    // double-submit: 20 is no longer current
    let err = rename_station(&conn, cmd("M", RenameLevel::AllPoles), "herry", "second").unwrap_err();
    assert!(err.to_string().contains("not the current version"), "{err}");
    let _ = EditError::StationNotCurrent(0); // variant exists and is public
}

#[test]
fn rename_rejects_no_op_and_empty() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let err = rename_station(&conn, cmd("S", RenameLevel::AllPoles), "herry", "").unwrap_err();
    assert!(err.to_string().contains("equals the current name"), "{err}");
    let err = rename_station(&conn, cmd("  ", RenameLevel::AllPoles), "herry", "").unwrap_err();
    assert!(err.to_string().contains("empty"), "{err}");
}
