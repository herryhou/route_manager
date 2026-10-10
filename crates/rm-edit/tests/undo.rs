//! Integration: undo_station end-to-end on an in-memory DB.

use rm_db::open_in_memory;
use rm_edit::{
    move_station, rename_station, undo_station, EditError, RenameLevel, StationMove, StationRename,
};
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
fn undo_station_rename_restores_names() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let renamed = rename_station(&conn, cmd("台北車站", RenameLevel::AllPoles), "herry", "cjk").unwrap();
    let out = undo_station(&conn, renamed.edit_id, "herry", "revert").unwrap();
    assert_eq!(out.restored_station_version_id, 20);
    let name: String = conn.query_row(
        "SELECT station_name FROM station_version WHERE valid_to IS NULL", [], |r| r.get(0)).unwrap();
    assert_eq!(name, "S");
    let mut stmt = conn.prepare(
        "SELECT stop_name FROM stop_version WHERE valid_to IS NULL ORDER BY stop_version_id").unwrap();
    let names: Vec<String> = stmt.query_map([], |r| r.get(0)).unwrap()
        .collect::<Result<_, _>>().unwrap();
    assert_eq!(names, vec!["S".to_string(), "S 往".to_string()]);
    // bearing back on the original pole version
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM stop_bearing WHERE stop_version_id = 10", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 1);
}

#[test]
fn undo_station_rename_refuses_undo_of_undo() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let renamed = rename_station(&conn, cmd("台北車站", RenameLevel::AllPoles), "herry", "").unwrap();
    let u = undo_station(&conn, renamed.edit_id, "herry", "").unwrap();
    let err = undo_station(&conn, u.edit_id, "herry", "").unwrap_err();
    assert!(err.to_string().contains("cannot be undone"), "{err}");
    let _ = EditError::NotUndoable(0); // variant exists and is public
}

#[test]
fn undo_station_rename_unknown_edit_id() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let err = undo_station(&conn, 999, "herry", "").unwrap_err();
    assert!(err.to_string().contains("cannot be undone"), "{err}");
}

#[test]
fn undo_station_rename_refuses_when_pole_edited_after_rename() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let renamed = rename_station(&conn, cmd("台北車站", RenameLevel::AllPoles), "herry", "").unwrap();
    rm_edit::edit_stop(&conn, renamed.poles[0].new_svi,
        rm_edit::StopEdit { stop_name: Some("shadowed".into()), ..Default::default() }, "h", "n").unwrap();
    let err = undo_station(&conn, renamed.edit_id, "herry", "").unwrap_err();
    assert!(err.to_string().contains("cannot be undone"), "{err}");
}

#[test]
fn undo_station_rename_refuses_stop_edit_row() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    // a plain stop-edit row: entity='stop', no station_rename kind
    let out = rm_edit::edit_stop(&conn, 10, rm_edit::StopEdit {
        stop_name: Some("N".into()), ..Default::default()
    }, "h", "n").unwrap();
    let err = undo_station(&conn, out.edit_id, "herry", "").unwrap_err();
    assert!(err.to_string().contains("cannot be undone"), "{err}");
}

#[test]
fn undo_station_moves_back() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let m = move_station(
        &conn,
        StationMove { station_version_id: 20, latitude: 24.5, longitude: 120.5 },
        "herry",
        "fix",
    )
    .unwrap();
    let u = undo_station(&conn, m.edit_id, "herry", "revert").unwrap();
    assert_eq!(u.restored_station_version_id, 20);
    // predecessor current again with original coords
    let (lat, lon): (f64, f64) = conn
        .query_row(
            "SELECT latitude, longitude FROM station_version WHERE valid_to IS NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((lat, lon), (24.0, 120.0));
    // undo mints nothing
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM station_version", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 2);
}

#[test]
fn undo_station_refuses_move_after_move() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let first = move_station(
        &conn,
        StationMove { station_version_id: 20, latitude: 24.5, longitude: 120.5 },
        "herry",
        "a",
    )
    .unwrap();
    move_station(
        &conn,
        StationMove { station_version_id: first.new_station_version_id, latitude: 24.6, longitude: 120.6 },
        "herry",
        "b",
    )
    .unwrap();
    let err = undo_station(&conn, first.edit_id, "herry", "revert").unwrap_err();
    assert!(err.to_string().contains("not the current version"), "{err}");
    // no undo row written
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM edit_log WHERE action = 'undo'", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0);
}

#[test]
fn undo_station_refuses_undo_of_undo() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let m = move_station(
        &conn,
        StationMove { station_version_id: 20, latitude: 24.5, longitude: 120.5 },
        "herry",
        "fix",
    )
    .unwrap();
    let u = undo_station(&conn, m.edit_id, "herry", "revert").unwrap();
    let err = undo_station(&conn, u.edit_id, "herry", "again").unwrap_err();
    assert!(err.to_string().contains("cannot be undone"), "{err}");
}

#[test]
fn undo_station_refuses_stop_edit_row() {
    // (if tests/undo.rs lacks this e2e variant — scenario-2 folded it in at
    // the db level; pin it at the service level here too)
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    // a stop edit row: entity='stop', kind-less changes
    let applied = rm_db::apply_stop_edit(
        &conn,
        &rm_db::StopEditOp {
            stop_id: 1,
            stop_version_id: 10,
            stop_name: Some("shadow".into()),
            ..Default::default()
        },
        "h",
        "n",
    )
    .unwrap();
    let err = undo_station(&conn, applied.edit_id, "herry", "revert").unwrap_err();
    assert!(err.to_string().contains("cannot be undone"), "{err}");
}
