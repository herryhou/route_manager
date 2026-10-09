//! Stop edit (move/name), regroup, and their undo — commands, pure plans,
//! load→plan→apply services. Zero SQL: persistence is rm-db's.

use anyhow::Result;
use rusqlite::Connection;

use crate::EditError;

#[derive(Debug, Default, Clone)]
pub struct StopEdit {
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub stop_name: Option<String>,
}

#[derive(Debug)]
pub struct StopEditPlan {
    pub stop_id: i64,
    pub station_id: i64,
    pub stop_version_id: i64,
    pub stop_name: Option<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
}

/// Pure planning policy: an empty edit and a no-op (every Some field
/// equals the current value) are refused; unchanged fields are dropped so
/// the apply churns only what moved. No I/O.
pub fn plan_stop_edit(
    snapshot: &rm_db::StopSnapshot,
    cmd: StopEdit,
) -> Result<StopEditPlan, EditError> {
    if cmd.lat.is_none() && cmd.lon.is_none() && cmd.stop_name.is_none() {
        return Err(EditError::EmptyEdit);
    }
    let no_op = cmd.lat.map_or(true, |v| v == snapshot.latitude)
        && cmd.lon.map_or(true, |v| v == snapshot.longitude)
        && cmd.stop_name.as_ref().map_or(true, |v| *v == snapshot.stop_name);
    if no_op {
        return Err(EditError::NoOpEdit);
    }
    Ok(StopEditPlan {
        stop_id: snapshot.stop_id,
        station_id: snapshot.station_id,
        stop_version_id: snapshot.stop_version_id,
        stop_name: cmd.stop_name.filter(|v| *v != snapshot.stop_name),
        lat: cmd.lat.filter(|v| *v != snapshot.latitude),
        lon: cmd.lon.filter(|v| *v != snapshot.longitude),
    })
}

#[derive(Debug)]
pub struct StopOutcome {
    pub edit_id: i64,
    pub new_stop_version_id: i64,
    pub stale_routes: Vec<i64>,
}

/// rm-db's internal errors surface 1:1 as this crate's public EditError.
fn map_db_err(e: anyhow::Error) -> anyhow::Error {
    match e.downcast_ref::<rm_db::EditError>() {
        Some(rm_db::EditError::NotCurrent(id)) => EditError::NotCurrent(*id).into(),
        Some(rm_db::EditError::StationNotCurrent(id)) => EditError::StationNotCurrent(*id).into(),
        Some(rm_db::EditError::UnknownStation(id)) => EditError::UnknownStation(*id).into(),
        Some(rm_db::EditError::NotUndoable(id)) => EditError::NotUndoable(*id).into(),
        _ => e,
    }
}

/// Application service: load → plan (pure) → apply (one tx).
pub fn edit_stop(
    conn: &Connection,
    stop_version_id: i64,
    cmd: StopEdit,
    editor: &str,
    note: &str,
) -> Result<StopOutcome> {
    let snapshot = rm_db::load_stop_snapshot(conn, stop_version_id)?
        .ok_or(EditError::NotCurrent(stop_version_id))?;
    let plan = plan_stop_edit(&snapshot, cmd)?;
    let op = rm_db::StopEditOp {
        stop_id: plan.stop_id,
        stop_version_id: plan.stop_version_id,
        stop_name: plan.stop_name,
        lat: plan.lat,
        lon: plan.lon,
    };
    let applied = rm_db::apply_stop_edit(conn, &op, editor, note).map_err(map_db_err)?;
    Ok(StopOutcome {
        edit_id: applied.edit_id,
        new_stop_version_id: applied.new_version_id,
        stale_routes: applied.stale_routes,
    })
}

#[derive(Debug)]
pub struct RegroupStop {
    pub stop_version_id: i64,
    pub new_station_id: i64,
}

#[derive(Debug)]
pub struct RegroupPlan {
    pub stop_id: i64,
    pub stop_version_id: i64,
    pub old_station_id: i64,
    pub new_station_id: i64,
}

/// Pure policy: regrouping to the current station is a no-op. Target
/// existence/currency is a load-time fact, checked by the service.
pub fn plan_regroup(
    snapshot: &rm_db::StopSnapshot,
    cmd: RegroupStop,
) -> Result<RegroupPlan, EditError> {
    if cmd.new_station_id == snapshot.station_id {
        return Err(EditError::NoOpEdit);
    }
    Ok(RegroupPlan {
        stop_id: snapshot.stop_id,
        stop_version_id: snapshot.stop_version_id,
        old_station_id: snapshot.station_id,
        new_station_id: cmd.new_station_id,
    })
}

/// Application service: load → target check → plan (pure) → apply (one tx).
pub fn regroup_stop(
    conn: &Connection,
    cmd: RegroupStop,
    editor: &str,
    note: &str,
) -> Result<StopOutcome> {
    let snapshot = rm_db::load_stop_snapshot(conn, cmd.stop_version_id)?
        .ok_or(EditError::NotCurrent(cmd.stop_version_id))?;
    if !rm_db::station_is_current(conn, cmd.new_station_id)? {
        return Err(EditError::UnknownStation(cmd.new_station_id).into());
    }
    let plan = plan_regroup(&snapshot, cmd)?;
    let op = rm_db::StopRegroupOp {
        stop_id: plan.stop_id,
        stop_version_id: plan.stop_version_id,
        new_station_id: plan.new_station_id,
    };
    let applied = rm_db::apply_stop_regroup(conn, &op, editor, note).map_err(map_db_err)?;
    Ok(StopOutcome {
        edit_id: applied.edit_id,
        new_stop_version_id: applied.new_version_id,
        stale_routes: applied.stale_routes,
    })
}

#[derive(Debug, PartialEq)]
pub struct StopUndoOp {
    pub edit_id: i64,
    pub stop_id: i64,
    pub old_svi: i64,
    pub cur_svi: i64,
}

/// Pure guard: only a non-undo stop row with both version ids is
/// invertible. Kind-agnostic — legacy rows whose `changes` carry no
/// `kind` stay undoable; the generic predecessor restore covers every
/// single-row stop edit (move, name, regroup).
pub fn plan_stop_undo(edit: &rm_db::StopEditRow) -> Result<StopUndoOp, EditError> {
    if edit.entity != "stop" || edit.action == "undo" {
        return Err(EditError::NotUndoable(edit.edit_id));
    }
    let (Some(old_svi), Some(cur_svi)) = (edit.old_stop_version_id, edit.new_stop_version_id) else {
        return Err(EditError::NotUndoable(edit.edit_id));
    };
    if old_svi == cur_svi {
        return Err(EditError::NotUndoable(edit.edit_id));
    }
    Ok(StopUndoOp { edit_id: edit.edit_id, stop_id: edit.stop_id, old_svi, cur_svi })
}

/// Application service: load → plan (pure) → apply (one tx).
pub fn undo_stop(
    conn: &Connection,
    edit_id: i64,
    editor: &str,
    note: &str,
) -> Result<StopOutcome> {
    let row = rm_db::load_stop_edit(conn, edit_id)?
        .ok_or(EditError::NotUndoable(edit_id))?;
    let plan = plan_stop_undo(&row)?;
    let op = rm_db::StopEditUndoOp {
        edit_id: plan.edit_id,
        stop_id: plan.stop_id,
        old_svi: plan.old_svi,
        cur_svi: plan.cur_svi,
    };
    let applied = rm_db::apply_stop_undo(conn, &op, editor, note).map_err(map_db_err)?;
    Ok(StopOutcome {
        edit_id: applied.edit_id,
        new_stop_version_id: applied.new_version_id,
        stale_routes: applied.stale_routes,
    })
}
