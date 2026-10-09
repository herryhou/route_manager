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
