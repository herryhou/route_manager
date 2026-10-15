//! Scenario 3: undo of a station rename — pure plan from the logged
//! changes JSON, emitting the write-side undo op directly. The op's rows
//! are historical facts from an append-only log; apply re-validates
//! currency inside its tx.

use anyhow::Result;
use rusqlite::Connection;

use super::{
    apply_station_undo, load_station_edit, EditError, PoleUndoOp, StationEdit, StationMoveUndoOp,
    StationRenameUndoOp, StationUndoOp, StationUndone,
};

/// Pure guard + dispatch: only a non-undo station row whose `changes.kind`
/// is a known station edit is invertible. Station-side undo is kind-strict
/// (every station row is post-scenario-1 and carries a kind), unlike the
/// kind-agnostic stop undo.
fn plan_station_undo(edit: &StationEdit) -> Result<StationUndoOp, EditError> {
    if edit.action == "undo" {
        return Err(EditError::NotUndoable(edit.edit_id));
    }
    let (Some(old_st), Some(new_st)) = (edit.old_station_version_id, edit.new_station_version_id)
    else {
        return Err(EditError::NotUndoable(edit.edit_id));
    };
    match edit.changes.get("kind").and_then(|k| k.as_str()) {
        Some("station_rename") => Ok(StationUndoOp::Rename(plan_rename_undo_parts(
            edit.edit_id,
            edit.station_id,
            old_st,
            new_st,
            &edit.changes,
        )?)),
        Some("station_move") => Ok(StationUndoOp::Move(StationMoveUndoOp {
            edit_id: edit.edit_id,
            station_id: edit.station_id,
            old_station_version_id: old_st,
            new_station_version_id: new_st,
        })),
        _ => Err(EditError::NotUndoable(edit.edit_id)),
    }
}

/// The rename arm: copy the logged stop-version pairs into the inverse op.
fn plan_rename_undo_parts(
    edit_id: i64,
    station_id: i64,
    old_st: i64,
    new_st: i64,
    changes: &serde_json::Value,
) -> Result<StationRenameUndoOp, EditError> {
    let stops = changes
        .get("stops")
        .and_then(|s| s.as_array())
        .ok_or(EditError::NotUndoable(edit_id))?;
    let mut poles = Vec::new();
    for s in stops {
        let (Some(stop_id), Some(old_svi), Some(new_svi)) = (
            s.get("stop_id").and_then(|v| v.as_i64()),
            s.get("old_svi").and_then(|v| v.as_i64()),
            s.get("new_svi").and_then(|v| v.as_i64()),
        ) else {
            return Err(EditError::NotUndoable(edit_id));
        };
        poles.push(PoleUndoOp {
            stop_id,
            old_svi,
            new_svi,
        });
    }
    Ok(StationRenameUndoOp {
        edit_id,
        station_id,
        old_station_version_id: old_st,
        new_station_version_id: new_st,
        poles,
    })
}

/// Application service: load the logged station edit → plan (pure) → apply
/// the inverse in one tx. Handles both station kinds (rename, move) via the
/// kind-dispatched plan.
pub fn undo_station(
    conn: &Connection,
    edit_id: i64,
    editor: &str,
    note: &str,
) -> Result<StationUndone> {
    let edit = load_station_edit(conn, edit_id)?.ok_or(EditError::NotUndoable(edit_id))?;
    let op = plan_station_undo(&edit)?;
    apply_station_undo(conn, &op, editor, note)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{move_station, rename_station};
    use serde_json::json;

    fn edit(action: &str, changes: serde_json::Value) -> StationEdit {
        StationEdit {
            edit_id: 7,
            station_id: 1,
            action: action.into(),
            old_station_version_id: Some(20),
            new_station_version_id: Some(21),
            changes,
        }
    }

    fn rename_changes() -> serde_json::Value {
        json!({
            "kind": "station_rename",
            "level": "AllPoles",
            "old_name": "S",
            "new_name": "台北車站",
            "stops": [
                {"stop_id": 1, "old_svi": 10, "new_svi": 12},
                {"stop_id": 2, "old_svi": 11, "new_svi": 13}
            ]
        })
    }

    fn move_changes() -> serde_json::Value {
        json!({
            "kind": "station_move",
            "latitude": {"old": 24.0, "new": 24.5},
            "longitude": {"old": 120.0, "new": 120.5}
        })
    }

    #[test]
    fn plan_parses_recorded_shape() {
        let op = plan_station_undo(&edit("update", rename_changes())).unwrap();
        let StationUndoOp::Rename(op) = op else {
            panic!("expected rename arm")
        };
        assert_eq!(op.edit_id, 7);
        assert_eq!(op.station_id, 1);
        assert_eq!(op.old_station_version_id, 20);
        assert_eq!(op.new_station_version_id, 21);
        assert_eq!(op.poles.len(), 2);
        assert_eq!(
            (
                op.poles[0].stop_id,
                op.poles[0].old_svi,
                op.poles[0].new_svi
            ),
            (1, 10, 12)
        );
    }

    #[test]
    fn plan_accepts_empty_stops_station_only() {
        let mut c = rename_changes();
        c["stops"] = json!([]);
        let op = plan_station_undo(&edit("update", c)).unwrap();
        let StationUndoOp::Rename(op) = op else {
            panic!("expected rename arm")
        };
        assert!(op.poles.is_empty());
    }

    #[test]
    fn plan_rejects_undo_action() {
        let err = plan_station_undo(&edit("undo", rename_changes())).unwrap_err();
        assert!(matches!(err, EditError::NotUndoable(7)), "{err}");
    }

    #[test]
    fn plan_rejects_unknown_or_missing_kind() {
        let mut c = rename_changes();
        c["kind"] = json!("stop_move");
        assert!(matches!(
            plan_station_undo(&edit("update", c)),
            Err(EditError::NotUndoable(7))
        ));
        let mut c = rename_changes();
        c.as_object_mut().unwrap().remove("kind");
        assert!(matches!(
            plan_station_undo(&edit("update", c)),
            Err(EditError::NotUndoable(7))
        ));
    }

    #[test]
    fn plan_rejects_malformed_stops_and_missing_versions() {
        let mut c = rename_changes();
        c["stops"] = json!("not-an-array");
        assert!(matches!(
            plan_station_undo(&edit("update", c)),
            Err(EditError::NotUndoable(7))
        ));
        let mut c = rename_changes();
        c["stops"][0].as_object_mut().unwrap().remove("new_svi");
        assert!(matches!(
            plan_station_undo(&edit("update", c)),
            Err(EditError::NotUndoable(7))
        ));
        let mut c = rename_changes();
        c.as_object_mut().unwrap().remove("stops");
        assert!(matches!(
            plan_station_undo(&edit("update", c)),
            Err(EditError::NotUndoable(7))
        ));
        let e = edit("update", rename_changes());
        let e = StationEdit {
            new_station_version_id: None,
            ..e
        };
        assert!(matches!(
            plan_station_undo(&e),
            Err(EditError::NotUndoable(7))
        ));
    }

    #[test]
    fn plan_move_kind_extracts_version_pair() {
        let op = plan_station_undo(&edit("update", move_changes())).unwrap();
        let StationUndoOp::Move(op) = op else {
            panic!("expected move arm")
        };
        assert_eq!(op.edit_id, 7);
        assert_eq!(op.station_id, 1);
        assert_eq!(op.old_station_version_id, 20);
        assert_eq!(op.new_station_version_id, 21);
    }

    #[test]
    fn plan_move_reads_nothing_but_kind() {
        // coords are restore-by-reopen; even a corrupt/absent payload under
        // the move kind still plans — the versions come from the log columns
        let op = plan_station_undo(&edit("update", json!({"kind": "station_move"}))).unwrap();
        assert!(matches!(op, StationUndoOp::Move(_)));
    }

    #[test]
    fn plan_rejects_move_action_undo() {
        let err = plan_station_undo(&edit("undo", move_changes())).unwrap_err();
        assert!(matches!(err, EditError::NotUndoable(7)), "{err}");
    }

    // ---------- service end-to-end (rehomed from rm-edit/tests/undo.rs) ----------

    /// station 'S' (sv 20) + poles sv 10 'S' (bearing row, rsv) and sv 11
    /// 'S 往' (no bearing). Same shape as station_rename/station_move's.
    fn fixture(conn: &rusqlite::Connection) {
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
    }

    #[test]
    fn undo_station_rename_restores_names() {
        let conn = crate::open_in_memory().unwrap();
        fixture(&conn);
        let renamed = rename_station(
            &conn,
            crate::StationRename { station_version_id: 20, new_name: "台北車站".into(), level: crate::RenameLevel::AllPoles },
            "herry",
            "cjk",
        )
        .unwrap();
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
        let conn = crate::open_in_memory().unwrap();
        fixture(&conn);
        let renamed = rename_station(
            &conn,
            crate::StationRename { station_version_id: 20, new_name: "台北車站".into(), level: crate::RenameLevel::AllPoles },
            "herry",
            "",
        )
        .unwrap();
        let u = undo_station(&conn, renamed.edit_id, "herry", "").unwrap();
        let err = undo_station(&conn, u.edit_id, "herry", "").unwrap_err();
        assert!(err.to_string().contains("cannot be undone"), "{err}");
    }

    #[test]
    fn undo_station_rename_unknown_edit_id() {
        let conn = crate::open_in_memory().unwrap();
        fixture(&conn);
        let err = undo_station(&conn, 999, "herry", "").unwrap_err();
        assert!(err.to_string().contains("cannot be undone"), "{err}");
    }

    #[test]
    fn undo_station_rename_refuses_when_pole_edited_after_rename() {
        let conn = crate::open_in_memory().unwrap();
        fixture(&conn);
        let renamed = rename_station(
            &conn,
            crate::StationRename { station_version_id: 20, new_name: "台北車站".into(), level: crate::RenameLevel::AllPoles },
            "herry",
            "",
        )
        .unwrap();
        crate::edit_stop(&conn, renamed.poles[0].new_svi,
            crate::StopEdit { stop_name: Some("shadowed".into()), ..Default::default() }, "h", "n").unwrap();
        let err = undo_station(&conn, renamed.edit_id, "herry", "").unwrap_err();
        assert!(err.to_string().contains("cannot be undone"), "{err}");
    }

    #[test]
    fn undo_station_rename_refuses_stop_edit_row() {
        let conn = crate::open_in_memory().unwrap();
        fixture(&conn);
        // a plain stop-edit row: entity='stop', no station_rename kind
        let out = crate::edit_stop(&conn, 10, crate::StopEdit {
            stop_name: Some("N".into()), ..Default::default()
        }, "h", "n").unwrap();
        let err = undo_station(&conn, out.edit_id, "herry", "").unwrap_err();
        assert!(err.to_string().contains("cannot be undone"), "{err}");
    }

    #[test]
    fn undo_station_moves_back() {
        let conn = crate::open_in_memory().unwrap();
        fixture(&conn);
        let m = move_station(
            &conn,
            crate::StationMove { station_version_id: 20, latitude: 24.5, longitude: 120.5 },
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
        let conn = crate::open_in_memory().unwrap();
        fixture(&conn);
        let first = move_station(
            &conn,
            crate::StationMove { station_version_id: 20, latitude: 24.5, longitude: 120.5 },
            "herry",
            "a",
        )
        .unwrap();
        move_station(
            &conn,
            crate::StationMove { station_version_id: first.new_station_version_id, latitude: 24.6, longitude: 120.6 },
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
        let conn = crate::open_in_memory().unwrap();
        fixture(&conn);
        let m = move_station(
            &conn,
            crate::StationMove { station_version_id: 20, latitude: 24.5, longitude: 120.5 },
            "herry",
            "fix",
        )
        .unwrap();
        let u = undo_station(&conn, m.edit_id, "herry", "revert").unwrap();
        let err = undo_station(&conn, u.edit_id, "herry", "again").unwrap_err();
        assert!(err.to_string().contains("cannot be undone"), "{err}");
    }

    #[test]
    fn undo_station_refuses_stop_edit_row_service_level() {
        let conn = crate::open_in_memory().unwrap();
        fixture(&conn);
        // a stop edit row: entity='stop', kind-less changes
        let applied = crate::apply_stop_edit(
            &conn,
            &crate::StopEditOp {
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
}
