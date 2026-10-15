//! Stop edit (move/name), regroup, and their undo — the whole primitive in
//! one module: command types, private plan policy, and the load→plan→apply
//! service entries. Plans emit the apply ops directly; no intermediate
//! copy structs.

use anyhow::Result;
use rusqlite::Connection;

use super::{
    apply_stop_edit, apply_stop_regroup, apply_stop_undo, load_stop_edit, load_stop_snapshot,
    station_is_current, EditError, EditOutcome, StopEditOp, StopEditRow, StopEditUndoOp,
    StopRegroupOp, StopSnapshot,
};

#[derive(Debug, Default, Clone)]
pub struct StopEdit {
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub stop_name: Option<String>,
}

/// Pure planning policy: an empty edit and a no-op (every Some field
/// equals the current value) are refused; unchanged fields are dropped so
/// the apply churns only what moved. No I/O.
fn plan_stop_edit(snapshot: &StopSnapshot, cmd: StopEdit) -> Result<StopEditOp, EditError> {
    if cmd.lat.is_none() && cmd.lon.is_none() && cmd.stop_name.is_none() {
        return Err(EditError::EmptyEdit);
    }
    let no_op = cmd.lat.is_none_or(|v| v == snapshot.latitude)
        && cmd.lon.is_none_or(|v| v == snapshot.longitude)
        && cmd
            .stop_name
            .as_ref()
            .is_none_or(|v| *v == snapshot.stop_name);
    if no_op {
        return Err(EditError::NoOpEdit);
    }
    Ok(StopEditOp {
        stop_id: snapshot.stop_id,
        stop_version_id: snapshot.stop_version_id,
        stop_name: cmd.stop_name.filter(|v| *v != snapshot.stop_name),
        lat: cmd.lat.filter(|v| *v != snapshot.latitude),
        lon: cmd.lon.filter(|v| *v != snapshot.longitude),
    })
}

/// Application service: load → plan (pure) → apply (one tx).
pub fn edit_stop(
    conn: &Connection,
    stop_version_id: i64,
    cmd: StopEdit,
    editor: &str,
    note: &str,
) -> Result<EditOutcome> {
    let snapshot =
        load_stop_snapshot(conn, stop_version_id)?.ok_or(EditError::NotCurrent(stop_version_id))?;
    let op = plan_stop_edit(&snapshot, cmd)?;
    apply_stop_edit(conn, &op, editor, note)
}

#[derive(Debug)]
pub struct RegroupStop {
    pub stop_version_id: i64,
    pub new_station_id: i64,
}

/// Pure policy: regrouping to the current station is a no-op. Target
/// existence/currency is a load-time fact, checked by the service.
fn plan_regroup(snapshot: &StopSnapshot, cmd: RegroupStop) -> Result<StopRegroupOp, EditError> {
    if cmd.new_station_id == snapshot.station_id {
        return Err(EditError::NoOpEdit);
    }
    Ok(StopRegroupOp {
        stop_id: snapshot.stop_id,
        stop_version_id: snapshot.stop_version_id,
        new_station_id: cmd.new_station_id,
    })
}

/// Application service: load → target check → plan (pure) → apply (one tx).
pub fn regroup_stop(
    conn: &Connection,
    cmd: RegroupStop,
    editor: &str,
    note: &str,
) -> Result<EditOutcome> {
    let snapshot = load_stop_snapshot(conn, cmd.stop_version_id)?
        .ok_or(EditError::NotCurrent(cmd.stop_version_id))?;
    if !station_is_current(conn, cmd.new_station_id)? {
        return Err(EditError::UnknownStation(cmd.new_station_id).into());
    }
    let op = plan_regroup(&snapshot, cmd)?;
    apply_stop_regroup(conn, &op, editor, note)
}

/// Pure guard: only a non-undo stop row with both version ids is
/// invertible. Kind-agnostic — legacy rows whose `changes` carry no
/// `kind` stay undoable; the generic predecessor restore covers every
/// single-row stop edit (move, name, regroup).
fn plan_stop_undo(edit: &StopEditRow) -> Result<StopEditUndoOp, EditError> {
    if edit.entity != "stop" || edit.action == "undo" {
        return Err(EditError::NotUndoable(edit.edit_id));
    }
    let (Some(old_svi), Some(cur_svi)) = (edit.old_stop_version_id, edit.new_stop_version_id)
    else {
        return Err(EditError::NotUndoable(edit.edit_id));
    };
    if old_svi == cur_svi {
        return Err(EditError::NotUndoable(edit.edit_id));
    }
    Ok(StopEditUndoOp {
        edit_id: edit.edit_id,
        stop_id: edit.stop_id,
        old_svi,
        cur_svi,
    })
}

/// Application service: load → plan (pure) → apply (one tx).
pub fn undo_stop(conn: &Connection, edit_id: i64, editor: &str, note: &str) -> Result<EditOutcome> {
    let row = load_stop_edit(conn, edit_id)?.ok_or(EditError::NotUndoable(edit_id))?;
    let op = plan_stop_undo(&row)?;
    apply_stop_undo(conn, &op, editor, note)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_in_memory;

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
        assert!(matches!(
            plan_stop_edit(&snap(), StopEdit::default()),
            Err(EditError::EmptyEdit)
        ));
        // every Some field equals the snapshot → no-op
        assert!(matches!(
            plan_stop_edit(
                &snap(),
                StopEdit {
                    lat: Some(24.0),
                    ..Default::default()
                }
            ),
            Err(EditError::NoOpEdit)
        ));
        assert!(matches!(
            plan_stop_edit(
                &snap(),
                StopEdit {
                    stop_name: Some("Old".into()),
                    ..Default::default()
                }
            ),
            Err(EditError::NoOpEdit)
        ));
        // a real change alongside an unchanged field passes
        let op = plan_stop_edit(
            &snap(),
            StopEdit {
                lat: Some(24.0),
                stop_name: Some("New".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(op.stop_name.as_deref(), Some("New"));
        assert_eq!(op.lat, None, "unchanged field is not churned");
    }

    #[test]
    fn plan_copies_snapshot_ids() {
        let op = plan_stop_edit(
            &snap(),
            StopEdit {
                lon: Some(120.5),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(op.stop_id, 1);
        assert_eq!(op.stop_version_id, 10);
        assert_eq!(op.lon, Some(120.5));
    }

    #[test]
    fn plan_regroup_rejects_same_station() {
        assert!(matches!(
            plan_regroup(
                &snap(),
                RegroupStop {
                    stop_version_id: 10,
                    new_station_id: 1
                }
            ),
            Err(EditError::NoOpEdit)
        ));
        let op = plan_regroup(
            &snap(),
            RegroupStop {
                stop_version_id: 10,
                new_station_id: 2,
            },
        )
        .unwrap();
        assert_eq!(
            (op.stop_id, op.stop_version_id, op.new_station_id),
            (1, 10, 2)
        );
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
        let op = plan_stop_undo(&row("stop", "update")).unwrap();
        assert_eq!(
            (op.edit_id, op.stop_id, op.old_svi, op.cur_svi),
            (7, 1, 10, 11)
        );
        // undo rows are not undoable
        assert!(matches!(
            plan_stop_undo(&row("stop", "undo")),
            Err(EditError::NotUndoable(7))
        ));
        // wrong entity (a station rename row) is refused
        assert!(matches!(
            plan_stop_undo(&row("station", "update")),
            Err(EditError::NotUndoable(7))
        ));
        // missing version ids
        let mut r = row("stop", "update");
        r.old_stop_version_id = None;
        assert!(matches!(plan_stop_undo(&r), Err(EditError::NotUndoable(7))));
    }

    /// Station 'S' (sv 20) + poles sv 10 (bearing row, rsv) and sv 11, id
    /// sequences seeded. Returns the current svi of stop 1 (= 10).
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
             VALUES (1, '1', 'tcg:1', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO route_version (route_version_id, route_id, version_no, valid_from, status, created_at)
             VALUES (1, 1, 1, '2026-01-01T00:00:00Z', 'PUBLISHED', '2026-01-01T00:00:00Z')", []).unwrap();
        conn.execute(
            "INSERT INTO subroute (subroute_id, route_id, subroute_code, created_at)
             VALUES (1, 1, '1-0', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO subroute_version (subroute_version_id, route_version_id, subroute_id, subroute_name, created_at)
             VALUES (1, 1, 1, '1', '2026-01-01T00:00:00Z')", []).unwrap();
        conn.execute(
            "INSERT INTO route_stop_version (route_stop_version_id, subroute_version_id, stop_version_id, direction, stop_sequence, created_at)
             VALUES (1, 1, 10, 0, 1, '2026-01-01T00:00:00Z')", []).unwrap();
        conn.execute(
            "INSERT INTO stop_bearing (stop_version_id, bearing_deg, source, computed_at)
             VALUES (10, 123.5, 'OSM', '2026-01-02T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO id_sequence (table_name, next_value) VALUES ('station_version', 21)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO id_sequence (table_name, next_value) VALUES ('stop_version', 12)",
            [],
        )
        .unwrap();
        10
    }

    /// Second station for regroup targets.
    fn second_station(conn: &Connection) {
        conn.execute(
            "INSERT INTO station (station_id, ext_uid, created_at)
             VALUES (2, 'tcg:node:2', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO station_version (station_version_id, station_id, station_group_id, station_name, latitude, longitude, valid_from, created_at)
             VALUES (21, 2, 1, 'T', 24.1, 120.1, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')", []).unwrap();
    }

    #[test]
    fn edit_stop_service_moves_and_reports_outcome() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        let out = edit_stop(
            &conn,
            old,
            StopEdit {
                lat: Some(24.001),
                ..Default::default()
            },
            "herry",
            "move",
        )
        .unwrap();
        assert!(out.new_version_id > old);
        assert_eq!(out.stale_routes, vec![1]);
    }

    #[test]
    fn edit_stop_service_refuses_stale_version() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        edit_stop(
            &conn,
            old,
            StopEdit {
                stop_name: Some("A".into()),
                ..Default::default()
            },
            "h",
            "n",
        )
        .unwrap();
        let err = edit_stop(
            &conn,
            old,
            StopEdit {
                stop_name: Some("B".into()),
                ..Default::default()
            },
            "h",
            "n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("not the current version"), "{err}");
    }

    #[test]
    fn regroup_service_roundtrip_and_guards() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        second_station(&conn);
        // unknown target
        let err = regroup_stop(
            &conn,
            RegroupStop {
                stop_version_id: old,
                new_station_id: 42,
            },
            "h",
            "n",
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("does not exist or is not current"),
            "{err}"
        );
        // same station
        let err = regroup_stop(
            &conn,
            RegroupStop {
                stop_version_id: old,
                new_station_id: 1,
            },
            "h",
            "n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("sets no field"), "{err}");
        // happy path
        let out = regroup_stop(
            &conn,
            RegroupStop {
                stop_version_id: old,
                new_station_id: 2,
            },
            "herry",
            "regroup",
        )
        .unwrap();
        assert!(out.new_version_id > old);
        assert!(out.stale_routes.is_empty());
    }

    #[test]
    fn undo_roundtrips_all_three_kinds() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        second_station(&conn);

        // move
        let m = edit_stop(
            &conn,
            old,
            StopEdit {
                lat: Some(24.001),
                ..Default::default()
            },
            "h",
            "m",
        )
        .unwrap();
        let u = undo_stop(&conn, m.edit_id, "h", "rev").unwrap();
        assert_eq!(u.new_version_id, old);
        assert_eq!(u.stale_routes, vec![1], "move undo re-marks projections");

        // regroup
        let g = regroup_stop(
            &conn,
            RegroupStop {
                stop_version_id: old,
                new_station_id: 2,
            },
            "h",
            "g",
        )
        .unwrap();
        let u = undo_stop(&conn, g.edit_id, "h", "rev").unwrap();
        assert_eq!(u.new_version_id, old);

        // name
        let n = edit_stop(
            &conn,
            old,
            StopEdit {
                stop_name: Some("N".into()),
                ..Default::default()
            },
            "h",
            "n",
        )
        .unwrap();
        let u = undo_stop(&conn, n.edit_id, "h", "rev").unwrap();
        assert_eq!(u.new_version_id, old);
        assert!(u.stale_routes.is_empty());
    }

    #[test]
    fn undo_of_undo_refused() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        let e = edit_stop(
            &conn,
            old,
            StopEdit {
                stop_name: Some("N".into()),
                ..Default::default()
            },
            "h",
            "n",
        )
        .unwrap();
        let u = undo_stop(&conn, e.edit_id, "h", "rev").unwrap();
        let err = undo_stop(&conn, u.edit_id, "h", "rev").unwrap_err();
        assert!(err.to_string().contains("cannot be undone"), "{err}");
    }

    #[test]
    fn undo_stop_accepts_legacy_row_without_kind() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        // synthesize a pre-scenario legacy row: {field:{old,new}} without a
        // kind marker — exactly what the pre-rm-edit stop edit wrote
        let e = edit_stop(
            &conn,
            old,
            StopEdit {
                stop_name: Some("N".into()),
                ..Default::default()
            },
            "h",
            "n",
        )
        .unwrap();
        conn.execute(
            "UPDATE edit_log SET changes = '{\"stop_name\":{\"old\":\"Old\",\"new\":\"N\"}}' WHERE edit_id = ?1",
            [e.edit_id]).unwrap();
        let u = undo_stop(&conn, e.edit_id, "h", "rev").unwrap();
        assert_eq!(u.new_version_id, old);
    }
}
