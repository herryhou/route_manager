use rm_edit::{plan_regroup, plan_stop_undo, RegroupStop, StopUndoOp};
use rm_edit::{plan_stop_edit, EditError, StopEdit};
use rm_db::{StopEditRow, StopSnapshot};

fn snap() -> StopSnapshot {
    StopSnapshot {
        stop_id: 1,
        station_id: 1,
        stop_version_id: 10,
        stop_code: "001".into(),
        stop_name: "Old".into(),
        direction: 0,
        latitude: 24.0,
        longitude: 120.0,
    }
}

#[test]
fn plan_rejects_empty_and_no_op() {
    assert!(matches!(plan_stop_edit(&snap(), StopEdit::default()), Err(EditError::EmptyEdit)));
    // every Some field equals the snapshot → no-op
    assert!(matches!(
        plan_stop_edit(&snap(), StopEdit { lat: Some(24.0), ..Default::default() }),
        Err(EditError::NoOpEdit)
    ));
    assert!(matches!(
        plan_stop_edit(&snap(), StopEdit { stop_name: Some("Old".into()), ..Default::default() }),
        Err(EditError::NoOpEdit)
    ));
    // a real change alongside an unchanged field passes
    let p = plan_stop_edit(&snap(), StopEdit {
        lat: Some(24.0),
        stop_name: Some("New".into()),
        ..Default::default()
    }).unwrap();
    assert_eq!(p.stop_name.as_deref(), Some("New"));
    assert_eq!(p.lat, None, "unchanged field is not churned");
}

#[test]
fn plan_copies_snapshot_ids() {
    let p = plan_stop_edit(&snap(), StopEdit { lon: Some(120.5), ..Default::default() }).unwrap();
    assert_eq!((p.stop_id, p.station_id, p.stop_version_id), (1, 1, 10));
    assert_eq!(p.lon, Some(120.5));
}

#[test]
fn plan_regroup_rejects_same_station() {
    assert!(matches!(
        plan_regroup(&snap(), RegroupStop { stop_version_id: 10, new_station_id: 1 }),
        Err(EditError::NoOpEdit)
    ));
    let p = plan_regroup(&snap(), RegroupStop { stop_version_id: 10, new_station_id: 2 }).unwrap();
    assert_eq!((p.stop_id, p.stop_version_id, p.old_station_id, p.new_station_id), (1, 10, 1, 2));
}

fn row(entity: &str, action: &str) -> StopEditRow {
    StopEditRow {
        edit_id: 7,
        entity: entity.into(),
        stop_id: 1,
        action: action.into(),
        old_stop_version_id: Some(10),
        new_stop_version_id: Some(11),
        changes: serde_json::json!({"latitude": {"old": 24.0, "new": 24.001}}),
    }
}

#[test]
fn plan_stop_undo_guards() {
    let p = plan_stop_undo(&row("stop", "update")).unwrap();
    assert_eq!(p, StopUndoOp { edit_id: 7, stop_id: 1, old_svi: 10, cur_svi: 11 });
    // undo rows are not undoable
    assert!(matches!(plan_stop_undo(&row("stop", "undo")), Err(EditError::NotUndoable(7))));
    // wrong entity (a station rename row) is refused
    assert!(matches!(plan_stop_undo(&row("station", "update")), Err(EditError::NotUndoable(7))));
    // missing version ids
    let mut r = row("stop", "update");
    r.old_stop_version_id = None;
    assert!(matches!(plan_stop_undo(&r), Err(EditError::NotUndoable(7))));
}

mod db {
    use super::*;
    use rm_db::open_in_memory;
    use rusqlite::Connection;

    /// Same shape as rm-edit's tests/undo.rs fixture: station 'S' (sv 20) +
    /// poles sv 10 (bearing row, rsv) and sv 11, id sequences seeded.
    /// Returns the current svi of stop 1 (= 10).
    fn fixture(conn: &Connection) -> i64 {
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
        10
    }

    /// Second station for regroup targets.
    fn second_station(conn: &Connection) {
        conn.execute(
            "INSERT INTO station (station_id, ext_uid, created_at)
             VALUES (2, 'tcg:node:2', '2026-01-01T00:00:00Z')", []).unwrap();
        conn.execute(
            "INSERT INTO station_version (station_version_id, station_id, station_group_id, station_name, latitude, longitude, valid_from, created_at)
             VALUES (21, 2, 1, 'T', 24.1, 120.1, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')", []).unwrap();
    }

    #[test]
    fn edit_stop_service_moves_and_reports_outcome() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        let out = rm_edit::edit_stop(&conn, old, StopEdit { lat: Some(24.001), ..Default::default() }, "herry", "move").unwrap();
        assert!(out.new_stop_version_id > old);
        assert_eq!(out.stale_routes, vec![1]);
    }

    #[test]
    fn edit_stop_service_refuses_stale_version() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        rm_edit::edit_stop(&conn, old, StopEdit { stop_name: Some("A".into()), ..Default::default() }, "h", "n").unwrap();
        let err = rm_edit::edit_stop(&conn, old, StopEdit { stop_name: Some("B".into()), ..Default::default() }, "h", "n").unwrap_err();
        assert!(err.to_string().contains("not the current version"), "{err}");
    }

    #[test]
    fn regroup_service_roundtrip_and_guards() {
        let conn = rm_db::open_in_memory().unwrap();
        let old = fixture(&conn);
        second_station(&conn);
        // unknown target
        let err = rm_edit::regroup_stop(&conn, RegroupStop { stop_version_id: old, new_station_id: 42 }, "h", "n").unwrap_err();
        assert!(err.to_string().contains("does not exist or is not current"), "{err}");
        // same station
        let err = rm_edit::regroup_stop(&conn, RegroupStop { stop_version_id: old, new_station_id: 1 }, "h", "n").unwrap_err();
        assert!(err.to_string().contains("sets no field"), "{err}");
        // happy path
        let out = rm_edit::regroup_stop(&conn, RegroupStop { stop_version_id: old, new_station_id: 2 }, "herry", "regroup").unwrap();
        assert!(out.new_stop_version_id > old);
        assert!(out.stale_routes.is_empty());
    }

    #[test]
    fn undo_roundtrips_all_three_kinds() {
        let conn = rm_db::open_in_memory().unwrap();
        let old = fixture(&conn);
        second_station(&conn);

        // move
        let m = rm_edit::edit_stop(&conn, old, StopEdit { lat: Some(24.001), ..Default::default() }, "h", "m").unwrap();
        let u = rm_edit::undo_stop(&conn, m.edit_id, "h", "rev").unwrap();
        assert_eq!(u.new_stop_version_id, old);
        assert_eq!(u.stale_routes, vec![1], "move undo re-marks projections");

        // regroup
        let g = rm_edit::regroup_stop(&conn, RegroupStop { stop_version_id: old, new_station_id: 2 }, "h", "g").unwrap();
        let u = rm_edit::undo_stop(&conn, g.edit_id, "h", "rev").unwrap();
        assert_eq!(u.new_stop_version_id, old);

        // name
        let n = rm_edit::edit_stop(&conn, old, StopEdit { stop_name: Some("N".into()), ..Default::default() }, "h", "n").unwrap();
        let u = rm_edit::undo_stop(&conn, n.edit_id, "h", "rev").unwrap();
        assert_eq!(u.new_stop_version_id, old);
        assert!(u.stale_routes.is_empty());
    }

    #[test]
    fn undo_of_undo_refused() {
        let conn = rm_db::open_in_memory().unwrap();
        let old = fixture(&conn);
        let e = rm_edit::edit_stop(&conn, old, StopEdit { stop_name: Some("N".into()), ..Default::default() }, "h", "n").unwrap();
        let u = rm_edit::undo_stop(&conn, e.edit_id, "h", "rev").unwrap();
        let err = rm_edit::undo_stop(&conn, u.edit_id, "h", "rev").unwrap_err();
        assert!(err.to_string().contains("cannot be undone"), "{err}");
    }

    #[test]
    fn undo_stop_accepts_legacy_row_without_kind() {
        let conn = rm_db::open_in_memory().unwrap();
        let old = fixture(&conn);
        // synthesize a pre-scenario legacy row: {field:{old,new}} without a
        // kind marker — exactly what the pre-rm-edit stop edit wrote
        let e = rm_edit::edit_stop(&conn, old, StopEdit { stop_name: Some("N".into()), ..Default::default() }, "h", "n").unwrap();
        conn.execute(
            "UPDATE edit_log SET changes = '{\"stop_name\":{\"old\":\"Old\",\"new\":\"N\"}}' WHERE edit_id = ?1",
            [e.edit_id]).unwrap();
        let u = rm_edit::undo_stop(&conn, e.edit_id, "h", "rev").unwrap();
        assert_eq!(u.new_stop_version_id, old);
    }
}
