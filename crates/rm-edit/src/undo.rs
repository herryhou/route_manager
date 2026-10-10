//! Scenario 3: undo of a station rename — pure plan from the logged
//! changes JSON. The op's rows are historical facts from an append-only
//! log; rm-db re-validates currency inside its apply tx.

use anyhow::Result;
use rm_db::{
    apply_station_rename_undo, load_station_edit, PoleUndoOp, StationEdit,
    StationRenameUndoOp,
};
use rusqlite::Connection;

use crate::EditError;

#[derive(Debug, PartialEq)]
pub struct RenameUndoOp {
    pub edit_id: i64,
    pub station_id: i64,
    pub old_station_version_id: i64,
    pub new_station_version_id: i64,
    pub poles: Vec<PoleUndo>, // empty for StationOnly
}

#[derive(Debug, PartialEq)]
pub struct PoleUndo {
    pub stop_id: i64,
    pub old_svi: i64,
    pub new_svi: i64,
}

#[derive(Debug, PartialEq)]
pub struct StationMoveUndoOp {
    pub edit_id: i64,
    pub station_id: i64,
    pub old_station_version_id: i64,
    pub new_station_version_id: i64,
}

#[derive(Debug, PartialEq)]
pub enum StationUndoOp {
    Rename(RenameUndoOp),
    Move(StationMoveUndoOp),
}

/// Pure guard + dispatch: only a non-undo station row whose `changes.kind`
/// is a known station edit is invertible. Station-side undo is kind-strict
/// (every station row is post-scenario-1 and carries a kind), unlike the
/// kind-agnostic stop undo.
pub fn plan_station_undo(edit: &StationEdit) -> Result<StationUndoOp, EditError> {
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
) -> Result<RenameUndoOp, EditError> {
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
        poles.push(PoleUndo { stop_id, old_svi, new_svi });
    }
    Ok(RenameUndoOp {
        edit_id,
        station_id,
        old_station_version_id: old_st,
        new_station_version_id: new_st,
        poles,
    })
}

#[derive(Debug)]
pub struct UndoOutcome {
    pub edit_id: i64, // the undo's own edit_log row
    pub restored_station_version_id: i64,
}

/// Application service: load the logged rename → plan (pure) → apply the
/// inverse in one tx. Zero SQL here.
pub fn undo_station_rename(
    conn: &Connection,
    edit_id: i64,
    editor: &str,
    note: &str,
) -> Result<UndoOutcome> {
    let edit = load_station_edit(conn, edit_id)?
        .ok_or(EditError::NotUndoable(edit_id))?;
    let plan = match plan_station_undo(&edit)? {
        StationUndoOp::Rename(p) => p,
        StationUndoOp::Move(_) => return Err(EditError::NotUndoable(edit_id).into()),
    };
    let op = StationRenameUndoOp {
        edit_id: plan.edit_id,
        station_id: plan.station_id,
        old_station_version_id: plan.old_station_version_id,
        new_station_version_id: plan.new_station_version_id,
        poles: plan
            .poles
            .iter()
            .map(|p| PoleUndoOp { stop_id: p.stop_id, old_svi: p.old_svi, new_svi: p.new_svi })
            .collect(),
    };
    let undone = apply_station_rename_undo(conn, &op, editor, note).map_err(crate::map_db_err)?;
    Ok(UndoOutcome {
        edit_id: undone.edit_id,
        restored_station_version_id: undone.restored_station_version_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rm_db::StationEdit;
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
        let StationUndoOp::Rename(op) = op else { panic!("expected rename arm") };
        assert_eq!(op.edit_id, 7);
        assert_eq!(op.station_id, 1);
        assert_eq!(op.old_station_version_id, 20);
        assert_eq!(op.new_station_version_id, 21);
        assert_eq!(op.poles.len(), 2);
        assert_eq!((op.poles[0].stop_id, op.poles[0].old_svi, op.poles[0].new_svi), (1, 10, 12));
    }

    #[test]
    fn plan_accepts_empty_stops_station_only() {
        let mut c = rename_changes();
        c["stops"] = json!([]);
        let op = plan_station_undo(&edit("update", c)).unwrap();
        let StationUndoOp::Rename(op) = op else { panic!("expected rename arm") };
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
        assert!(matches!(plan_station_undo(&edit("update", c)), Err(EditError::NotUndoable(7))));
        let mut c = rename_changes();
        c.as_object_mut().unwrap().remove("kind");
        assert!(matches!(plan_station_undo(&edit("update", c)), Err(EditError::NotUndoable(7))));
    }

    #[test]
    fn plan_rejects_malformed_stops_and_missing_versions() {
        let mut c = rename_changes();
        c["stops"] = json!("not-an-array");
        assert!(matches!(plan_station_undo(&edit("update", c)), Err(EditError::NotUndoable(7))));
        let mut c = rename_changes();
        c["stops"][0].as_object_mut().unwrap().remove("new_svi");
        assert!(matches!(plan_station_undo(&edit("update", c)), Err(EditError::NotUndoable(7))));
        let mut c = rename_changes();
        c.as_object_mut().unwrap().remove("stops");
        assert!(matches!(plan_station_undo(&edit("update", c)), Err(EditError::NotUndoable(7))));
        let e = edit("update", rename_changes());
        let e = StationEdit { new_station_version_id: None, ..e };
        assert!(matches!(plan_station_undo(&e), Err(EditError::NotUndoable(7))));
    }

    #[test]
    fn plan_move_kind_extracts_version_pair() {
        let op = plan_station_undo(&edit("update", move_changes())).unwrap();
        assert_eq!(
            op,
            StationUndoOp::Move(StationMoveUndoOp {
                edit_id: 7,
                station_id: 1,
                old_station_version_id: 20,
                new_station_version_id: 21,
            })
        );
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
}
