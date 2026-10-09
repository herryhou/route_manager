//! Scenario 3: undo of a station rename — pure plan from the logged
//! changes JSON. The op's rows are historical facts from an append-only
//! log; rm-db re-validates currency inside its apply tx.

use rm_db::StationRenameEdit;

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

/// Pure: reject undo-rows and non-rename kinds, copy the logged
/// stop-version pairs into the inverse op.
pub fn plan_rename_undo(edit: &StationRenameEdit) -> Result<RenameUndoOp, EditError> {
    if edit.action == "undo" {
        return Err(EditError::NotUndoable(edit.edit_id));
    }
    if edit.changes.get("kind").and_then(|k| k.as_str()) != Some("station_rename") {
        return Err(EditError::NotUndoable(edit.edit_id));
    }
    let (Some(old_st), Some(new_st)) = (edit.old_station_version_id, edit.new_station_version_id)
    else {
        return Err(EditError::NotUndoable(edit.edit_id));
    };
    let stops = edit
        .changes
        .get("stops")
        .and_then(|s| s.as_array())
        .ok_or(EditError::NotUndoable(edit.edit_id))?;
    let mut poles = Vec::new();
    for s in stops {
        let (Some(stop_id), Some(old_svi), Some(new_svi)) = (
            s.get("stop_id").and_then(|v| v.as_i64()),
            s.get("old_svi").and_then(|v| v.as_i64()),
            s.get("new_svi").and_then(|v| v.as_i64()),
        ) else {
            return Err(EditError::NotUndoable(edit.edit_id));
        };
        poles.push(PoleUndo { stop_id, old_svi, new_svi });
    }
    Ok(RenameUndoOp {
        edit_id: edit.edit_id,
        station_id: edit.station_id,
        old_station_version_id: old_st,
        new_station_version_id: new_st,
        poles,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rm_db::StationRenameEdit;
    use serde_json::json;

    fn edit(action: &str, changes: serde_json::Value) -> StationRenameEdit {
        StationRenameEdit {
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

    #[test]
    fn plan_parses_recorded_shape() {
        let op = plan_rename_undo(&edit("update", rename_changes())).unwrap();
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
        let op = plan_rename_undo(&edit("update", c)).unwrap();
        assert!(op.poles.is_empty());
    }

    #[test]
    fn plan_rejects_undo_action() {
        let err = plan_rename_undo(&edit("undo", rename_changes())).unwrap_err();
        assert!(matches!(err, EditError::NotUndoable(7)), "{err}");
    }

    #[test]
    fn plan_rejects_unknown_or_missing_kind() {
        let mut c = rename_changes();
        c["kind"] = json!("stop_move");
        assert!(matches!(plan_rename_undo(&edit("update", c)), Err(EditError::NotUndoable(7))));
        let mut c = rename_changes();
        c.as_object_mut().unwrap().remove("kind");
        assert!(matches!(plan_rename_undo(&edit("update", c)), Err(EditError::NotUndoable(7))));
    }

    #[test]
    fn plan_rejects_malformed_stops_and_missing_versions() {
        let mut c = rename_changes();
        c["stops"] = json!("not-an-array");
        assert!(matches!(plan_rename_undo(&edit("update", c)), Err(EditError::NotUndoable(7))));
        let mut c = rename_changes();
        c["stops"][0].as_object_mut().unwrap().remove("new_svi");
        assert!(matches!(plan_rename_undo(&edit("update", c)), Err(EditError::NotUndoable(7))));
        let mut c = rename_changes();
        c.as_object_mut().unwrap().remove("stops");
        assert!(matches!(plan_rename_undo(&edit("update", c)), Err(EditError::NotUndoable(7))));
        let e = edit("update", rename_changes());
        let e = StationRenameEdit { new_station_version_id: None, ..e };
        assert!(matches!(plan_rename_undo(&e), Err(EditError::NotUndoable(7))));
    }
}
