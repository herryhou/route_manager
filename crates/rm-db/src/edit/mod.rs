//! Manual edit primitive: version-append edits + audit + staleness marks.

mod add_stop;
mod detach;
mod line;
mod matched_path;
mod station_move;
mod station_rename;
mod station_undo;
mod stop;

pub use add_stop::{
    add_stop, apply_add_stop, parse_attach, plan_add_stop, qualify_station, undo_stop_add,
    AddStopCmd, AddStopOutcome, AddStopPlan, AttachCmd, PlannedAttach, Qualification, RowCounts,
    StationCandidate, StationVerdict, QUALIFY_BEARING_TOL_DEG, QUALIFY_MAX_DIST_M,
};
pub use detach::{
    apply_detach_stop, detach_stop, plan_detach_stop, undo_detach_stop, DetachCmd,
    DetachOccurrence, DetachOutcome, DetachPlan, DetachUndone,
};
pub use line::{
    edit_polyline_line, plan_polyline_line, undo_polyline_line, LineEditCmd, LineEditPlan,
};
pub use matched_path::{bearing_at, matched_path, project_onto_path, OFF_PATH_MAX_M};
pub use station_move::{move_station, plan_move_station, StationMove, StationMovePlan};
pub use station_rename::{
    plan_rename_station, rename_station, PoleRenamePlan, RenameLevel, StationRename,
    StationRenamePlan,
};
pub use station_undo::undo_station;
pub use stop::{
    edit_stop, plan_regroup_stop, preview_stop_edit, regroup_stop, undo_stop, RegroupStop,
    RegroupStopPlan, StopEdit, StopEditPreview,
};

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::json;
use thiserror::Error;

use crate::stale;

#[derive(Debug, Error)]
pub enum EditError {
    #[error("stop version {0} is not the current version (or does not exist)")]
    NotCurrent(i64),
    #[error("station version {0} is not the current version (or does not exist)")]
    StationNotCurrent(i64),
    #[error("edit {0} cannot be undone (not current, or is itself an undo)")]
    NotUndoable(i64),
    #[error("station {0} does not exist or is not current")]
    UnknownStation(i64),
    #[error("route {0} does not exist")]
    UnknownRoute(String),
    #[error("invalid polyline: {0}")]
    InvalidLine(String),
    #[error("line {0}/{1} was edited by someone else — reload and retry")]
    LineNotCurrent(String, i64),
    #[error("edit changes nothing")]
    EmptyEdit,
    #[error("edit sets no field to a new value")]
    NoOpEdit,
    #[error("new station name is empty")]
    EmptyNewName,
    #[error("new station name equals the current name")]
    NoOpRename,
    #[error("route {0} has no direction {1} on its current version")]
    UnknownDirection(String, i64),
    #[error("route {0} direction {1} has no stop {2}")]
    UnknownAfterStop(String, i64, String),
    #[error("route {xno} direction {direction}: stop is {dist_m:.0} m off the matched path (max {max_m:.0} m)")]
    OffPathStop {
        xno: String,
        direction: i64,
        dist_m: f64,
        max_m: f64,
    },
    #[error("duplicate attachment for route {0} direction {1}")]
    DuplicateAttach(String, i64),
    #[error("route {xno} direction {direction}: stop {stop_code} occurs {count} times — give --sequence or --all")]
    DuplicateStopOccurrence {
        xno: String,
        direction: i64,
        stop_code: String,
        count: i64,
    },
    #[error("{0}; pass --delete-pole (one-way, not undoable)")]
    OrphanPoleRefused(String),
}

#[derive(Debug)]
pub struct EditOutcome {
    pub edit_id: i64,
    pub new_version_id: i64,
    pub stale_routes: Vec<i64>,
}

#[derive(Serialize)]
pub struct EditLogRow {
    pub edit_id: i64,
    pub ts: String,
    pub editor: String,
    pub action: String,
    pub changes: String,
    pub note: Option<String>,
    pub undo_of: Option<i64>,
}

/// Move a `stop_bearing` row from one version key to another, keeping the
/// value and `computed_at` (bearing = f(position); edits that don't move
/// the position don't invalidate it). No-op when `from_svi` has no row.
fn carry_bearing(tx: &Connection, from_svi: i64, to_svi: i64) -> Result<()> {
    tx.execute(
        "INSERT INTO stop_bearing (stop_version_id, bearing_deg, source, computed_at)
         SELECT ?1, bearing_deg, source, computed_at FROM stop_bearing
         WHERE stop_version_id = ?2",
        params![to_svi, from_svi],
    )?;
    tx.execute(
        "DELETE FROM stop_bearing WHERE stop_version_id = ?1",
        [from_svi],
    )?;
    Ok(())
}

#[derive(Debug, Default)]
pub struct StopEditOp {
    pub stop_id: i64,
    pub stop_version_id: i64, // must be the current version
    pub stop_name: Option<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
}

/// Whole-tx stop edit (move and/or rename). The pole's `stop_bearing` row
/// is always carried over with its value and `computed_at` (a move keeps
/// the last bearing visible; stale marks tell the refresh to recompute).
/// Position present ⇒ derived data is marked stale; name-only ⇒ NOTHING
/// goes stale. One edit_log row, `changes = {field: {old, new}}`.
pub fn apply_stop_edit(
    conn: &Connection,
    op: &StopEditOp,
    editor: &str,
    note: &str,
) -> Result<EditOutcome> {
    let tx = conn.unchecked_transaction()?;
    // re-validate inside the tx: the version must still be current
    let old: (i64, String, String, i64, f64, f64) = match tx.query_row(
        "SELECT station_id, stop_code, stop_name, direction, latitude, longitude
         FROM stop_version WHERE stop_version_id = ?1 AND valid_to IS NULL",
        [op.stop_version_id],
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        },
    ) {
        Ok(v) => v,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Err(EditError::NotCurrent(op.stop_version_id).into())
        }
        Err(e) => return Err(e.into()),
    };
    let (station_id, stop_code, old_name, direction, old_lat, old_lon) = old;
    let now = crate::now_rfc3339();
    let new_id = crate::mint(&tx, "stop_version")?;
    let closed = tx.execute(
        "UPDATE stop_version SET valid_to = ?1 WHERE stop_version_id = ?2",
        params![now, op.stop_version_id],
    )?;
    if closed != 1 {
        // the SELECT guard above saw this row current; a 0-row close means it
        // vanished mid-tx — refuse before any further write (tx aborts)
        return Err(EditError::NotCurrent(op.stop_version_id).into());
    }
    tx.execute(
        "INSERT INTO stop_version
            (stop_version_id, stop_id, station_id, stop_code, stop_name, direction,
             latitude, longitude, valid_from, created_at, source)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, 'manual')",
        params![
            new_id,
            op.stop_id,
            station_id,
            stop_code,
            op.stop_name.clone().unwrap_or(old_name.clone()),
            direction,
            op.lat.unwrap_or(old_lat),
            op.lon.unwrap_or(old_lon),
            now
        ],
    )?;
    tx.execute(
        "UPDATE route_stop_version SET stop_version_id = ?1 WHERE stop_version_id = ?2",
        params![new_id, op.stop_version_id],
    )?;

    let moves = op.lat.is_some() || op.lon.is_some();
    carry_bearing(&tx, op.stop_version_id, new_id)?;

    let mut changes = serde_json::Map::new();
    if let Some(v) = &op.stop_name {
        changes.insert("stop_name".into(), json!({"old": old_name, "new": v}));
    }
    if let Some(v) = op.lat {
        changes.insert("latitude".into(), json!({"old": old_lat, "new": v}));
    }
    if let Some(v) = op.lon {
        changes.insert("longitude".into(), json!({"old": old_lon, "new": v}));
    }
    tx.execute(
        "INSERT INTO edit_log (ts, editor, entity, entity_id, action, changes, note, version_before, version_after)
         VALUES (?1, ?2, 'stop', ?3, 'update', ?4, ?5, ?6, ?7)",
        params![
            now,
            editor,
            op.stop_id,
            serde_json::Value::Object(changes).to_string(),
            if note.is_empty() { None } else { Some(note) },
            op.stop_version_id,
            new_id
        ],
    )?;
    let edit_id = tx.last_insert_rowid();

    let mut stale_routes = Vec::new();
    if moves {
        stale::mark_bearings_stale(&tx, edit_id, &now)?;
        stale_routes = stale::mark_position_stale(&tx, new_id, edit_id, &now)?;
    }
    tx.commit()?;
    Ok(EditOutcome {
        edit_id,
        new_version_id: new_id,
        stale_routes,
    })
}

#[derive(Debug)]
pub struct StopRegroupOp {
    pub stop_id: i64,
    pub stop_version_id: i64, // must be the current version
    pub new_station_id: i64,
}

/// Whole-tx regroup: move a pole to another station. Membership lives in
/// the child version (INV-17), so this is single-row churn. Position is
/// untouched ⇒ the bearing value survives (carried over); the station
/// circular mean goes stale on both sides — one whole-table mark covers
/// both (whole-table rebuild). No projection marks: an rsv repoint alone
/// invalidates nothing (scenario-1 precedent).
pub fn apply_stop_regroup(
    conn: &Connection,
    op: &StopRegroupOp,
    editor: &str,
    note: &str,
) -> Result<EditOutcome> {
    let tx = conn.unchecked_transaction()?;
    let old: (i64, String, String, i64, f64, f64) = match tx.query_row(
        "SELECT station_id, stop_code, stop_name, direction, latitude, longitude
         FROM stop_version WHERE stop_version_id = ?1 AND valid_to IS NULL",
        [op.stop_version_id],
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        },
    ) {
        Ok(v) => v,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Err(EditError::NotCurrent(op.stop_version_id).into())
        }
        Err(e) => return Err(e.into()),
    };
    let (old_station_id, stop_code, old_name, direction, lat, lon) = old;
    let n: i64 = tx.query_row(
        "SELECT COUNT(*) FROM station_version WHERE station_id = ?1 AND valid_to IS NULL",
        [op.new_station_id],
        |r| r.get(0),
    )?;
    if n == 0 {
        return Err(EditError::UnknownStation(op.new_station_id).into());
    }
    let now = crate::now_rfc3339();
    let new_id = crate::mint(&tx, "stop_version")?;
    let closed = tx.execute(
        "UPDATE stop_version SET valid_to = ?1 WHERE stop_version_id = ?2",
        params![now, op.stop_version_id],
    )?;
    if closed != 1 {
        // the SELECT guard above saw this row current; a 0-row close means it
        // vanished mid-tx — refuse before any further write (tx aborts)
        return Err(EditError::NotCurrent(op.stop_version_id).into());
    }
    tx.execute(
        "INSERT INTO stop_version
            (stop_version_id, stop_id, station_id, stop_code, stop_name, direction,
             latitude, longitude, valid_from, created_at, source)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, 'manual')",
        params![
            new_id,
            op.stop_id,
            op.new_station_id,
            stop_code,
            old_name,
            direction,
            lat,
            lon,
            now
        ],
    )?;
    tx.execute(
        "UPDATE route_stop_version SET stop_version_id = ?1 WHERE stop_version_id = ?2",
        params![new_id, op.stop_version_id],
    )?;
    carry_bearing(&tx, op.stop_version_id, new_id)?;

    let changes = json!({"station_id": {"old": old_station_id, "new": op.new_station_id}});
    tx.execute(
        "INSERT INTO edit_log (ts, editor, entity, entity_id, action, changes, note, version_before, version_after)
         VALUES (?1, ?2, 'stop', ?3, 'move_station', ?4, ?5, ?6, ?7)",
        params![
            now,
            editor,
            op.stop_id,
            changes.to_string(),
            if note.is_empty() { None } else { Some(note) },
            op.stop_version_id,
            new_id
        ],
    )?;
    let edit_id = tx.last_insert_rowid();

    stale::mark_station_bearings_stale(&tx, edit_id, &now)?;
    tx.commit()?;
    Ok(EditOutcome {
        edit_id,
        new_version_id: new_id,
        stale_routes: Vec::new(),
    })
}

#[derive(Debug)]
pub struct StopEditUndoOp {
    pub edit_id: i64,
    pub stop_id: i64,
    pub old_svi: i64, // edit_log.version_before — the predecessor to reopen
    pub cur_svi: i64, // edit_log.version_after — must still be current
}

/// Whole-tx stop-edit undo: reopen the predecessor, close the minted row,
/// repoint rsv back, MINT NOTHING. The bearing row at the churned svi (the
/// carried original, or one a refresh recomputed) moves back to the
/// predecessor with its `computed_at`; stale marks re-derive from the
/// undone edit's recorded `changes`. All guards run before any write;
/// every version UPDATE checks rows-affected == 1.
pub fn apply_stop_undo(
    conn: &Connection,
    op: &StopEditUndoOp,
    editor: &str,
    note: &str,
) -> Result<EditOutcome> {
    let tx = conn.unchecked_transaction()?;
    // guards BEFORE any write
    let cur: (Option<String>, String) = match tx.query_row(
        "SELECT valid_to, source FROM stop_version WHERE stop_version_id = ?1",
        [op.cur_svi],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ) {
        Ok(v) => v,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Err(EditError::NotUndoable(op.edit_id).into())
        }
        Err(e) => return Err(e.into()),
    };
    if cur.0.is_some() || cur.1 != "manual" {
        return Err(EditError::NotUndoable(op.edit_id).into());
    }
    let n: i64 = tx.query_row(
        "SELECT COUNT(*) FROM stop_version WHERE stop_version_id = ?1",
        [op.old_svi],
        |r| r.get(0),
    )?;
    if n == 0 {
        return Err(EditError::NotUndoable(op.edit_id).into());
    }
    let changes: String = match tx.query_row(
        "SELECT changes FROM edit_log WHERE edit_id = ?1",
        [op.edit_id],
        |r| r.get(0),
    ) {
        Ok(v) => v,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Err(EditError::NotUndoable(op.edit_id).into())
        }
        Err(e) => return Err(e.into()),
    };
    let cj: serde_json::Value = serde_json::from_str(&changes)?;
    let position_changed = cj.get("latitude").is_some() || cj.get("longitude").is_some();
    let regrouped = cj.get("station_id").is_some();

    let now = crate::now_rfc3339();
    let n = tx.execute(
        "UPDATE stop_version SET valid_to = ?1
         WHERE stop_version_id = ?2 AND valid_to IS NULL",
        params![now, op.cur_svi],
    )?;
    if n != 1 {
        return Err(EditError::NotUndoable(op.edit_id).into());
    }
    let n = tx.execute(
        "UPDATE stop_version SET valid_to = NULL WHERE stop_version_id = ?1",
        [op.old_svi],
    )?;
    if n != 1 {
        return Err(EditError::NotUndoable(op.edit_id).into());
    }
    tx.execute(
        "UPDATE route_stop_version SET stop_version_id = ?1 WHERE stop_version_id = ?2",
        params![op.old_svi, op.cur_svi],
    )?;

    carry_bearing(&tx, op.cur_svi, op.old_svi)?;

    tx.execute(
        "INSERT INTO edit_log (ts, editor, entity, entity_id, action, changes, note, undo_of, version_before, version_after)
         VALUES (?1, ?2, 'stop', ?3, 'undo', '{}', ?4, ?5, ?6, ?7)",
        params![
            now,
            editor,
            op.stop_id,
            if note.is_empty() { None } else { Some(note) },
            op.edit_id,
            op.cur_svi,
            op.old_svi
        ],
    )?;
    let undo_edit_id = tx.last_insert_rowid();

    let mut stale_routes = Vec::new();
    if position_changed {
        stale::mark_bearings_stale(&tx, undo_edit_id, &now)?;
        // rsv already points at `old_svi` (repointed above)
        stale_routes = stale::mark_position_stale(&tx, op.old_svi, undo_edit_id, &now)?;
    } else if regrouped {
        stale::mark_station_bearings_stale(&tx, undo_edit_id, &now)?;
    }
    tx.commit()?;
    Ok(EditOutcome {
        edit_id: undo_edit_id,
        new_version_id: op.old_svi,
        stale_routes,
    })
}

/// Which entity an edit_log row belongs to (`"stop"`, `"station"`); `None`
/// for unknown ids. The undo dispatcher's peek.
pub fn edit_entity(conn: &Connection, edit_id: i64) -> Result<Option<String>> {
    conn.query_row(
        "SELECT entity FROM edit_log WHERE edit_id = ?1",
        [edit_id],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .map_err(Into::into)
}

/// One tagged outcome for every undo kind; the `kind` tag mirrors the
/// `edit_log.entity` value. `edit_id` is the newly written undo row's id
/// (not the undone edit's) — same field the undo endpoint always returned.
/// Serialized uniformly so neither the handler nor the client re-branches
/// on shape — the server's dispatch knowledge travels with the payload.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UndoOutcome {
    Stop {
        edit_id: i64,
        restored_version_id: i64,
    },
    /// An add-stop undo deleted the job's own rows — nothing was restored.
    StopAdd {
        edit_id: i64,
    },
    /// A detach undo restored the route pointer to the pre-detach version.
    StopDetach {
        edit_id: i64,
        restored_route_version_id: i64,
    },
    Station {
        edit_id: i64,
        restored_version_id: i64,
    },
    Polyline {
        edit_id: i64,
        deleted: bool,
    },
}

/// Undo any edit by id: peek the entity and dispatch to that kind's undo
/// service. Unknown ids run down the stop arm and surface as its 409
/// `NotUndoable` — the same behavior the handler's old branch had.
pub fn undo_edit(conn: &Connection, edit_id: i64, editor: &str, note: &str) -> Result<UndoOutcome> {
    match edit_entity(conn, edit_id)?.as_deref() {
        Some("polyline") => {
            undo_polyline_line(conn, edit_id, editor, note).map(|o| UndoOutcome::Polyline {
                edit_id: o.edit_id,
                deleted: o.deleted,
            })
        }
        Some("station") => {
            undo_station(conn, edit_id, editor, note).map(|o| UndoOutcome::Station {
                edit_id: o.edit_id,
                restored_version_id: o.restored_station_version_id,
            })
        }
        Some("stop_add") => undo_stop_add(conn, edit_id, editor, note)
            .map(|o| UndoOutcome::StopAdd { edit_id: o.edit_id }),
        // before the stop fallthrough — "stop_detach" is its own entity
        Some("stop_detach") => {
            undo_detach_stop(conn, edit_id, editor, note).map(|o| UndoOutcome::StopDetach {
                edit_id: o.edit_id,
                restored_route_version_id: o.restored_route_version_id,
            })
        }
        // "stop" and any unknown id alike — the stop service's plan guard
        // refuses foreign rows and missing rows with `NotUndoable`.
        _ => undo_stop(conn, edit_id, editor, note).map(|o| UndoOutcome::Stop {
            edit_id: o.edit_id,
            restored_version_id: o.new_version_id,
        }),
    }
}

pub fn entity_history(conn: &Connection, entity: &str, entity_id: i64) -> Result<Vec<EditLogRow>> {
    let mut stmt = conn.prepare(
        "SELECT edit_id, ts, editor, action, changes, note, undo_of
         FROM edit_log WHERE entity = ?1 AND entity_id = ?2
         ORDER BY edit_id DESC",
    )?;
    let rows = stmt.query_map(params![entity, entity_id], |r| {
        Ok(EditLogRow {
            edit_id: r.get(0)?,
            ts: r.get(1)?,
            editor: r.get(2)?,
            action: r.get(3)?,
            changes: r.get(4)?,
            note: r.get(5)?,
            undo_of: r.get(6)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Read-side snapshot for station-level edits (rename_station /
/// move_station): the current station version plus its poles'
/// current stop versions.
#[derive(Debug)]
pub struct StationSnapshot {
    pub station_id: i64,
    pub station_version_id: i64,
    pub station_name: String,
    pub latitude: f64,
    pub longitude: f64,
    pub poles: Vec<PoleSnapshot>,
}

#[derive(Debug)]
pub struct PoleSnapshot {
    pub stop_id: i64,
    pub stop_version_id: i64,
    pub stop_name: String,
}

/// Load the current `station_version` (matched by id AND `valid_to IS NULL`)
/// with its current poles. `None` when `station_version_id` is not current.
pub fn load_station_snapshot(
    conn: &Connection,
    station_version_id: i64,
) -> Result<Option<StationSnapshot>> {
    let row: (i64, String, f64, f64) = match conn
        .query_row(
            "SELECT station_id, station_name, latitude, longitude FROM station_version
             WHERE station_version_id = ?1 AND valid_to IS NULL",
            [station_version_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?
    {
        Some(v) => v,
        None => return Ok(None),
    };
    let (station_id, station_name, latitude, longitude) = row;
    let mut stmt = conn.prepare(
        "SELECT stop_id, stop_version_id, stop_name FROM stop_version
         WHERE station_id = ?1 AND valid_to IS NULL
         ORDER BY stop_version_id",
    )?;
    let poles = stmt
        .query_map([station_id], |r| {
            Ok(PoleSnapshot {
                stop_id: r.get(0)?,
                stop_version_id: r.get(1)?,
                stop_name: r.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(StationSnapshot {
        station_id,
        station_version_id,
        station_name,
        latitude,
        longitude,
        poles,
    }))
}

/// Current row of one stop (pole) — current-row discipline
/// (`valid_to IS NULL`). `None` when the version is closed or missing.
#[derive(Debug)]
pub struct StopSnapshot {
    pub stop_id: i64,
    pub station_id: i64,
    pub stop_version_id: i64,
    pub stop_code: String,
    pub stop_name: String,
    pub direction: i64,
    pub latitude: f64,
    pub longitude: f64,
}

pub fn load_stop_snapshot(conn: &Connection, stop_version_id: i64) -> Result<Option<StopSnapshot>> {
    let res = conn.query_row(
        "SELECT stop_id, station_id, stop_version_id, stop_code, stop_name,
                direction, latitude, longitude
         FROM stop_version WHERE stop_version_id = ?1 AND valid_to IS NULL",
        [stop_version_id],
        |r| {
            Ok(StopSnapshot {
                stop_id: r.get(0)?,
                station_id: r.get(1)?,
                stop_version_id: r.get(2)?,
                stop_code: r.get(3)?,
                stop_name: r.get(4)?,
                direction: r.get(5)?,
                latitude: r.get(6)?,
                longitude: r.get(7)?,
            })
        },
    );
    match res {
        Ok(s) => Ok(Some(s)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Does `station_id` have a current `station_version`? (Regroup target check.)
pub fn station_is_current(conn: &Connection, station_id: i64) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM station_version
         WHERE station_id = ?1 AND valid_to IS NULL",
        [station_id],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Read-side DTO for the logged station edit (rename or move — consumed
/// by plan_station_undo). edit_log is append-only, so a plan
/// parsed from this row is immutable; apply re-validates currency in its tx.
#[derive(Debug)]
pub struct StationEdit {
    pub edit_id: i64,
    pub station_id: i64,
    pub action: String,
    pub old_station_version_id: Option<i64>, // edit_log.version_before
    pub new_station_version_id: Option<i64>, // edit_log.version_after
    pub changes: serde_json::Value,
}

/// Load one edit_log row (any entity — the plan's kind guard is the
/// discriminator) with `changes` parsed as JSON. `None` when `edit_id`
/// does not exist.
pub fn load_station_edit(conn: &Connection, edit_id: i64) -> Result<Option<StationEdit>> {
    let row: (i64, String, Option<i64>, Option<i64>, String) = match conn
        .query_row(
            "SELECT entity_id, action, version_before, version_after, changes
             FROM edit_log WHERE edit_id = ?1",
            [edit_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?
    {
        Some(v) => v,
        None => return Ok(None),
    };
    let (station_id, action, vb, va, changes) = row;
    Ok(Some(StationEdit {
        edit_id,
        station_id,
        action,
        old_station_version_id: vb,
        new_station_version_id: va,
        changes: serde_json::from_str(&changes)?,
    }))
}

/// One edit_log row for the stop-undo plan (ANY entity — the plan's
/// entity guard is the discriminator). `changes` parsed as JSON;
/// `None` when the id does not exist.
#[derive(Debug)]
pub struct StopEditRow {
    pub edit_id: i64,
    pub entity: String,
    pub stop_id: i64, // edit_log.entity_id (stable)
    pub action: String,
    pub old_stop_version_id: Option<i64>, // version_before
    pub new_stop_version_id: Option<i64>, // version_after
    pub changes: serde_json::Value,
}

pub fn load_stop_edit(conn: &Connection, edit_id: i64) -> Result<Option<StopEditRow>> {
    let row: (i64, String, i64, String, Option<i64>, Option<i64>, String) = match conn.query_row(
        "SELECT edit_id, entity, entity_id, action, version_before, version_after, changes
         FROM edit_log WHERE edit_id = ?1",
        [edit_id],
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        },
    ) {
        Ok(v) => v,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let (edit_id, entity, stop_id, action, vb, va, changes) = row;
    Ok(Some(StopEditRow {
        edit_id,
        entity,
        stop_id,
        action,
        old_stop_version_id: vb,
        new_stop_version_id: va,
        changes: serde_json::from_str(&changes)?,
    }))
}

/// Write-side op for the station rename, emitted by
/// plan_station_rename.
#[derive(Debug)]
pub struct StationRenameOp {
    pub station_id: i64,
    pub old_station_version_id: i64,
    pub old_station_name: String, // for the edit_log changes JSON only
    pub new_station_name: String,
    pub level: String, // RenameLevel::as_str
    pub poles: Vec<PoleOp>,
}

#[derive(Debug)]
pub struct PoleOp {
    pub stop_id: i64,
    pub old_stop_version_id: i64,
    pub new_stop_name: String,
}

#[derive(Debug)]
pub struct StationRenameApplied {
    pub edit_id: i64,
    pub new_station_version_id: i64,
    pub poles: Vec<PoleApplied>,
}

#[derive(Debug)]
pub struct PoleApplied {
    pub stop_id: i64,
    pub old_svi: i64,
    pub new_svi: i64,
}

/// Apply a station rename in ONE transaction: version-append the station,
/// churn the listed poles, repoint route_stop_version, carry stop_bearing
/// rows to the new pole versions (bearing value = f(position, path
/// geometry) — a rename changes neither, so nothing goes stale). One
/// edit_log row shaped for scenario-3 multi-row undo.
pub fn apply_station_rename(
    conn: &Connection,
    op: &StationRenameOp,
    editor: &str,
    note: &str,
) -> Result<StationRenameApplied> {
    let tx = conn.unchecked_transaction()?;
    let now = crate::now_rfc3339();

    // re-validate inside the tx: the old station version must still be current
    let (station_group_id, lat, lon): (i64, f64, f64) = match tx.query_row(
        "SELECT station_group_id, latitude, longitude FROM station_version
         WHERE station_version_id = ?1 AND valid_to IS NULL",
        [op.old_station_version_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ) {
        Ok(v) => v,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Err(EditError::StationNotCurrent(op.old_station_version_id).into())
        }
        Err(e) => return Err(e.into()),
    };
    tx.execute(
        "UPDATE station_version SET valid_to = ?1 WHERE station_version_id = ?2",
        params![now, op.old_station_version_id],
    )?;
    let new_station_version_id = crate::mint(&tx, "station_version")?;
    tx.execute(
        "INSERT INTO station_version
            (station_version_id, station_id, station_group_id, station_name,
             latitude, longitude, valid_from, created_at, source)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, 'manual')",
        params![
            new_station_version_id,
            op.station_id,
            station_group_id,
            op.new_station_name,
            lat,
            lon,
            now
        ],
    )?;

    let mut poles_applied = Vec::new();
    for pole in &op.poles {
        // pole must still be current; abort the whole tx otherwise
        let (stop_station_id, stop_code, direction, slat, slon): (i64, String, i64, f64, f64) =
            match tx.query_row(
                "SELECT station_id, stop_code, direction, latitude, longitude
                 FROM stop_version WHERE stop_version_id = ?1 AND valid_to IS NULL",
                [pole.old_stop_version_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            ) {
                Ok(v) => v,
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    return Err(EditError::NotCurrent(pole.old_stop_version_id).into())
                }
                Err(e) => return Err(e.into()),
            };
        tx.execute(
            "UPDATE stop_version SET valid_to = ?1 WHERE stop_version_id = ?2",
            params![now, pole.old_stop_version_id],
        )?;
        let new_svi = crate::mint(&tx, "stop_version")?;
        tx.execute(
            "INSERT INTO stop_version
                (stop_version_id, stop_id, station_id, stop_code, stop_name, direction,
                 latitude, longitude, valid_from, created_at, source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, 'manual')",
            params![
                new_svi,
                pole.stop_id,
                stop_station_id,
                stop_code,
                pole.new_stop_name,
                direction,
                slat,
                slon,
                now
            ],
        )?;
        tx.execute(
            "UPDATE route_stop_version SET stop_version_id = ?1 WHERE stop_version_id = ?2",
            params![new_svi, pole.old_stop_version_id],
        )?;
        // carry the bearing row to the new version (none → no-op): the row
        // MOVES — old svi's copy is deleted, so no stale-marking is needed
        tx.execute(
            "INSERT INTO stop_bearing (stop_version_id, bearing_deg, source, computed_at)
             SELECT ?1, bearing_deg, source, computed_at FROM stop_bearing
             WHERE stop_version_id = ?2",
            params![new_svi, pole.old_stop_version_id],
        )?;
        tx.execute(
            "DELETE FROM stop_bearing WHERE stop_version_id = ?1",
            params![pole.old_stop_version_id],
        )?;
        poles_applied.push(PoleApplied {
            stop_id: pole.stop_id,
            old_svi: pole.old_stop_version_id,
            new_svi,
        });
    }

    let stops_json: Vec<serde_json::Value> = poles_applied
        .iter()
        .map(|p| json!({"stop_id": p.stop_id, "old_svi": p.old_svi, "new_svi": p.new_svi}))
        .collect();
    let changes = json!({
        "kind": "station_rename",
        "level": op.level,
        "old_name": op.old_station_name,
        "new_name": op.new_station_name,
        "stops": stops_json,
    })
    .to_string();
    tx.execute(
        "INSERT INTO edit_log (ts, editor, entity, entity_id, action, changes, note, version_before, version_after)
         VALUES (?1, ?2, 'station', ?3, 'update', ?4, ?5, ?6, ?7)",
        params![
            now,
            editor,
            op.station_id,
            changes,
            if note.is_empty() { None } else { Some(note) },
            op.old_station_version_id,
            new_station_version_id
        ],
    )?;
    let edit_id = tx.last_insert_rowid();

    tx.commit()?;
    Ok(StationRenameApplied {
        edit_id,
        new_station_version_id,
        poles: poles_applied,
    })
}

/// Write-side op for the station move, emitted by plan_station_move.
#[derive(Debug)]
pub struct StationMoveOp {
    pub station_id: i64,
    pub old_station_version_id: i64,
    pub new_latitude: f64,
    pub new_longitude: f64,
}

#[derive(Debug)]
pub struct StationMoveApplied {
    pub edit_id: i64,
    pub new_station_version_id: i64,
}

/// Apply a station move in ONE transaction: version-append the station with
/// the new coords (`source='manual'`), everything else carried. Nothing
/// references station_version_id and no derived table reads station coords,
/// so there is no repoint step and ZERO stale marks. One edit_log row whose
/// changes carry `{kind, latitude:{old,new}, longitude:{old,new}}` for the
/// kind-agnostic station undo.
pub fn apply_station_move(
    conn: &Connection,
    op: &StationMoveOp,
    editor: &str,
    note: &str,
) -> Result<StationMoveApplied> {
    let tx = conn.unchecked_transaction()?;
    let now = crate::now_rfc3339();

    // re-validate inside the tx: the old station version must still be current
    let (station_group_id, station_name, old_lat, old_lon): (i64, String, f64, f64) = match tx
        .query_row(
            "SELECT station_group_id, station_name, latitude, longitude FROM station_version
             WHERE station_version_id = ?1 AND valid_to IS NULL",
            [op.old_station_version_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        ) {
        Ok(v) => v,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Err(EditError::StationNotCurrent(op.old_station_version_id).into())
        }
        Err(e) => return Err(e.into()),
    };
    let n = tx.execute(
        "UPDATE station_version SET valid_to = ?1 WHERE station_version_id = ?2",
        params![now, op.old_station_version_id],
    )?;
    if n != 1 {
        return Err(EditError::StationNotCurrent(op.old_station_version_id).into());
    }
    let new_station_version_id = crate::mint(&tx, "station_version")?;
    tx.execute(
        "INSERT INTO station_version
            (station_version_id, station_id, station_group_id, station_name,
             latitude, longitude, valid_from, created_at, source)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, 'manual')",
        params![
            new_station_version_id,
            op.station_id,
            station_group_id,
            station_name,
            op.new_latitude,
            op.new_longitude,
            now
        ],
    )?;

    let changes = json!({
        "kind": "station_move",
        "latitude": {"old": old_lat, "new": op.new_latitude},
        "longitude": {"old": old_lon, "new": op.new_longitude},
    })
    .to_string();
    tx.execute(
        "INSERT INTO edit_log (ts, editor, entity, entity_id, action, changes, note, version_before, version_after)
         VALUES (?1, ?2, 'station', ?3, 'update', ?4, ?5, ?6, ?7)",
        params![
            now,
            editor,
            op.station_id,
            changes,
            if note.is_empty() { None } else { Some(note) },
            op.old_station_version_id,
            new_station_version_id
        ],
    )?;
    let edit_id = tx.last_insert_rowid();

    tx.commit()?;
    Ok(StationMoveApplied {
        edit_id,
        new_station_version_id,
    })
}

/// One `entity='polyline'` edit_log row (the undo loader). Filtering on
/// entity in the WHERE keeps other entities' rows returning `None` — the
/// plan's kind guard stays the discriminator within the polyline entity.
#[derive(Debug)]
pub struct PolylineEditRow {
    pub edit_id: i64,
    pub entity_id: i64, // route_id
    pub action: String,
    pub ts: String,
    pub changes: serde_json::Value,
}

pub fn load_polyline_edit(conn: &Connection, edit_id: i64) -> Result<Option<PolylineEditRow>> {
    let row = conn
        .query_row(
            "SELECT entity_id, action, ts, changes FROM edit_log
             WHERE edit_id = ?1 AND entity = 'polyline'",
            [edit_id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
    Ok(row
        .map(
            |(entity_id, action, ts, changes)| -> Result<PolylineEditRow> {
                Ok(PolylineEditRow {
                    edit_id,
                    entity_id,
                    action,
                    ts,
                    changes: serde_json::from_str(&changes)?,
                })
            },
        )
        .transpose()?)
}

// ---------- whole-line override (phase 2) ----------

/// One current polyline_override row, as stored (unversioned — the row IS
/// the current state; `updated_at` doubles as the concurrency token).
#[derive(Debug)]
pub struct OverrideRow {
    pub encoded: String,
    pub updated_at: String,
}

/// Read-side snapshot for whole-line polyline edits: the route id
/// (edit_log's entity_id), the latest-batch src polyline (`None` when the
/// capture has no polyline for this direction), the current override row
/// (`None` = readers fall through to src), and the route's current version.
#[derive(Debug)]
pub struct LineSnapshot {
    pub route_id: i64,
    pub src_encoded: Option<String>,
    pub override_row: Option<OverrideRow>,
    pub route_version_id: Option<i64>,
}

/// `xno` is stable route identity (`route.ext_uid = 'tcg:' || xno`).
/// `None` when no route carries that ext_uid.
pub fn load_polyline_line(
    conn: &Connection,
    xno: &str,
    direction: i64,
) -> Result<Option<LineSnapshot>> {
    let (route_id, route_version_id): (i64, Option<i64>) = match conn.query_row(
        "SELECT route_id, current_route_version_id FROM route WHERE ext_uid = ?1",
        [format!("tcg:{xno}")],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ) {
        Ok(v) => v,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let batch_id: i64 = conn.query_row(
        "SELECT COALESCE(MAX(batch_id), 0) FROM import_batch",
        [],
        |r| r.get(0),
    )?;
    let src_encoded: Option<String> = conn
        .query_row(
            "SELECT encoded FROM src_route_point
             WHERE batch_id = ?1 AND xno = ?2 AND direction = ?3",
            params![batch_id, xno, direction],
            |r| r.get(0),
        )
        .optional()?;
    let override_row = conn
        .query_row(
            "SELECT encoded, updated_at FROM polyline_override
             WHERE xno = ?1 AND direction = ?2",
            params![xno, direction],
            |r| {
                Ok(OverrideRow {
                    encoded: r.get(0)?,
                    updated_at: r.get(1)?,
                })
            },
        )
        .optional()?;
    Ok(Some(LineSnapshot {
        route_id,
        src_encoded,
        override_row,
        route_version_id,
    }))
}

/// The COALESCE every consumer reads: the manual override when one exists,
/// else the latest-batch src polyline, else `""` (readers treat `""` as
/// no_path — same convention as a missing src row). Passes the override
/// through verbatim; decoding is the consumer's business.
pub fn effective_encoded(
    conn: &Connection,
    batch_id: i64,
    xno: &str,
    direction: i64,
) -> Result<String> {
    let enc: String = conn.query_row(
        "SELECT COALESCE(
            (SELECT encoded FROM polyline_override WHERE xno = ?1 AND direction = ?2),
            (SELECT encoded FROM src_route_point
             WHERE batch_id = ?3 AND xno = ?1 AND direction = ?2),
            '')",
        params![xno, direction, batch_id],
        |r| r.get(0),
    )?;
    Ok(enc)
}

/// Write-side op for a whole-line override edit, emitted by
/// plan_line_edit.
#[derive(Debug)]
pub struct LineEditOp {
    pub xno: String,
    pub direction: i64,
    pub encoded: String,
    pub expected_updated_at: Option<String>,
}

#[derive(Debug)]
pub struct LineApplied {
    pub edit_id: i64,
    pub updated_at: String,
    pub created: bool,
}

/// Apply a whole-line override edit in ONE transaction. Unversioned: the
/// row IS current; `updated_at` doubles as the optimistic-concurrency
/// token and the producing edit's ts. Currency guard runs before ANY
/// write; the same tx marks all four derived datasets stale.
pub fn apply_line_edit(
    conn: &Connection,
    op: &LineEditOp,
    editor: &str,
    note: &str,
) -> Result<LineApplied> {
    let tx = conn.unchecked_transaction()?;
    let now = crate::now_rfc3339();

    let route_id: i64 = match tx.query_row(
        "SELECT route_id FROM route WHERE ext_uid = ?1",
        [format!("tcg:{}", op.xno)],
        |r| r.get(0),
    ) {
        Ok(id) => id,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Err(EditError::UnknownRoute(op.xno.clone()).into())
        }
        Err(e) => return Err(e.into()),
    };

    // Currency: guard before any write.
    let mut created = true;
    let mut old: Option<String> = None;
    if let Some(row) = tx
        .query_row(
            "SELECT updated_at, encoded FROM polyline_override
             WHERE xno = ?1 AND direction = ?2",
            params![op.xno, op.direction],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?
    {
        created = false;
        let (updated_at, encoded) = row;
        if Some(&updated_at) != op.expected_updated_at.as_ref() {
            return Err(EditError::LineNotCurrent(op.xno.clone(), op.direction).into());
        }
        old = Some(encoded);
    } else if op.expected_updated_at.is_some() {
        // expectation against a nonexistent override: it moved (undone away)
        return Err(EditError::LineNotCurrent(op.xno.clone(), op.direction).into());
    }

    tx.execute(
        "INSERT INTO polyline_override (xno, direction, encoded, updated_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(xno, direction) DO UPDATE SET encoded = ?3, updated_at = ?4",
        params![op.xno, op.direction, op.encoded, now],
    )?;

    // changes carries old+new so undo is a forward write (`old` absent on create).
    let mut encoded_change = json!({ "new": op.encoded });
    if let Some(enc) = &old {
        encoded_change["old"] = json!(enc);
    }
    let changes = json!({
        "kind": "polyline_override",
        "xno": op.xno,
        "direction": op.direction,
        "encoded": encoded_change,
    })
    .to_string();
    tx.execute(
        "INSERT INTO edit_log (ts, editor, entity, entity_id, action, changes, note)
         VALUES (?1, ?2, 'polyline', ?3, ?4, ?5, ?6)",
        params![
            now,
            editor,
            route_id,
            if created { "create" } else { "update" },
            changes,
            if note.is_empty() { None } else { Some(note) }
        ],
    )?;
    let edit_id = tx.last_insert_rowid();

    stale::mark_line_stale(&tx, route_id, op.direction, edit_id, &now)?;

    tx.commit()?;
    Ok(LineApplied {
        edit_id,
        updated_at: now,
        created,
    })
}

/// Write-side op for the line undo: restore-as-forward-write (or delete).
/// `undone_ts` is the undone edit's ts — the currency token the apply tx
/// re-validates against the row.
#[derive(Debug)]
pub struct LineUndoOp {
    pub edit_id: i64,
    pub route_id: i64,
    pub xno: String,
    pub direction: i64,
    pub undone_ts: String,
    /// Encoded state to restore; `None` = the undone edit was a create →
    /// delete the override row (back to pure src).
    pub restored: Option<String>,
}

#[derive(Debug)]
pub struct LineUndone {
    pub edit_id: i64,
    pub deleted: bool,
}

/// Undo a whole-line override edit in ONE transaction. Currency guard (the
/// row's `updated_at` must still equal the undone edit's ts) runs before
/// any write, so a superseded undo is a clean 409 with zero partial state.
/// Same stale marks as apply — undoing a line re-derives everything too.
pub fn apply_line_undo(
    conn: &Connection,
    op: &LineUndoOp,
    editor: &str,
    note: &str,
) -> Result<LineUndone> {
    let tx = conn.unchecked_transaction()?;
    let now = crate::now_rfc3339();

    let (updated_at, cur_encoded) = match tx
        .query_row(
            "SELECT updated_at, encoded FROM polyline_override
             WHERE xno = ?1 AND direction = ?2",
            params![op.xno, op.direction],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?
    {
        Some(v) => v,
        None => return Err(EditError::LineNotCurrent(op.xno.clone(), op.direction).into()),
    };
    if updated_at != op.undone_ts {
        return Err(EditError::LineNotCurrent(op.xno.clone(), op.direction).into());
    }

    let (deleted, new_encoded) = match &op.restored {
        Some(enc) => {
            tx.execute(
                "INSERT INTO polyline_override (xno, direction, encoded, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(xno, direction) DO UPDATE SET encoded = ?3, updated_at = ?4",
                params![op.xno, op.direction, enc, now],
            )?;
            (false, json!(enc))
        }
        None => {
            tx.execute(
                "DELETE FROM polyline_override WHERE xno = ?1 AND direction = ?2",
                params![op.xno, op.direction],
            )?;
            (true, serde_json::Value::Null)
        }
    };

    let changes = json!({
        "kind": "polyline_override",
        "xno": op.xno,
        "direction": op.direction,
        "encoded": { "old": cur_encoded, "new": new_encoded },
    })
    .to_string();
    tx.execute(
        "INSERT INTO edit_log (ts, editor, entity, entity_id, action, changes, note, undo_of)
         VALUES (?1, ?2, 'polyline', ?3, 'undo', ?4, ?5, ?6)",
        params![
            now,
            editor,
            op.route_id,
            changes,
            if note.is_empty() { None } else { Some(note) },
            op.edit_id
        ],
    )?;
    let edit_id = tx.last_insert_rowid();

    stale::mark_line_stale(&tx, op.route_id, op.direction, edit_id, &now)?;

    tx.commit()?;
    Ok(LineUndone { edit_id, deleted })
}

/// Write-side op for the station-rename undo, emitted by
/// plan_station_undo; the rows come from the append-only log.
#[derive(Debug)]
pub struct StationRenameUndoOp {
    pub edit_id: i64,
    pub station_id: i64,
    pub old_station_version_id: i64,
    pub new_station_version_id: i64,
    pub poles: Vec<PoleUndoOp>,
}

#[derive(Debug)]
pub struct PoleUndoOp {
    pub stop_id: i64,
    pub old_svi: i64,
    pub new_svi: i64,
}

/// Write-side op for the station-move undo: restore is reopen-the-old /
/// close-the-minted — the old row already holds the original coords.
#[derive(Debug)]
pub struct StationMoveUndoOp {
    pub edit_id: i64,
    pub station_id: i64,
    pub old_station_version_id: i64,
    pub new_station_version_id: i64,
}

#[derive(Debug)]
pub enum StationUndoOp {
    Rename(StationRenameUndoOp),
    Move(StationMoveUndoOp),
}

#[derive(Debug)]
pub struct StationUndone {
    pub edit_id: i64,
    pub restored_station_version_id: i64,
}

/// Undo a station edit in ONE transaction — the exact inverse of the
/// applied edit, dispatched on the op's kind. Rename branch: close/reopen
/// the station version, per pole close/reopen the stop versions, repoint
/// route_stop_version, move the stop_bearing row back (PK invariant).
/// Move branch: close the minted version, reopen the predecessor — touches
/// no poles, no bearings, no rsv (a station move wrote none of them).
/// One edit_log undo row, ZERO stale marks. Mints nothing.
///
/// Guards (inside the tx, before any write): the edit's station version
/// must still be current and manual (`StationNotCurrent` otherwise); the
/// rename branch additionally requires every minted pole version still
/// current and manual (`NotUndoable` otherwise). Every reopen/close UPDATE
/// checks rows-affected == 1.
pub fn apply_station_undo(
    conn: &Connection,
    op: &StationUndoOp,
    editor: &str,
    note: &str,
) -> Result<StationUndone> {
    match op {
        StationUndoOp::Rename(o) => apply_rename_undo(conn, o, editor, note),
        StationUndoOp::Move(o) => apply_move_undo(conn, o, editor, note),
    }
}

fn apply_rename_undo(
    conn: &Connection,
    op: &StationRenameUndoOp,
    editor: &str,
    note: &str,
) -> Result<StationUndone> {
    let tx = conn.unchecked_transaction()?;
    let now = crate::now_rfc3339();

    // guards BEFORE any write; the manual-source assertion is on the rows
    // being CLOSED (the rename's output) — the predecessors being reopened
    // are pipeline rows, often source != 'manual'
    let n: i64 = tx.query_row(
        "SELECT COUNT(*) FROM station_version
         WHERE station_version_id = ?1 AND valid_to IS NULL AND source = 'manual'",
        [op.new_station_version_id],
        |r| r.get(0),
    )?;
    if n == 0 {
        return Err(EditError::StationNotCurrent(op.new_station_version_id).into());
    }
    for pole in &op.poles {
        let n: i64 = tx.query_row(
            "SELECT COUNT(*) FROM stop_version
             WHERE stop_version_id = ?1 AND valid_to IS NULL AND source = 'manual'",
            [pole.new_svi],
            |r| r.get(0),
        )?;
        if n == 0 {
            return Err(EditError::NotUndoable(op.edit_id).into());
        }
    }

    let n = tx.execute(
        "UPDATE station_version SET valid_to = ?1
         WHERE station_version_id = ?2 AND valid_to IS NULL",
        params![now, op.new_station_version_id],
    )?;
    if n != 1 {
        return Err(EditError::NotUndoable(op.edit_id).into());
    }
    let n = tx.execute(
        "UPDATE station_version SET valid_to = NULL WHERE station_version_id = ?1",
        params![op.old_station_version_id],
    )?;
    if n != 1 {
        return Err(EditError::NotUndoable(op.edit_id).into());
    }

    for pole in &op.poles {
        let n = tx.execute(
            "UPDATE stop_version SET valid_to = ?1
             WHERE stop_version_id = ?2 AND valid_to IS NULL",
            params![now, pole.new_svi],
        )?;
        if n != 1 {
            return Err(EditError::NotUndoable(op.edit_id).into());
        }
        let n = tx.execute(
            "UPDATE stop_version SET valid_to = NULL WHERE stop_version_id = ?1",
            params![pole.old_svi],
        )?;
        if n != 1 {
            return Err(EditError::NotUndoable(op.edit_id).into());
        }
        tx.execute(
            "UPDATE route_stop_version SET stop_version_id = ?1 WHERE stop_version_id = ?2",
            params![pole.old_svi, pole.new_svi],
        )?;
        // move the bearing row back (none → no-op), mirroring apply's carry
        tx.execute(
            "INSERT INTO stop_bearing (stop_version_id, bearing_deg, source, computed_at)
             SELECT ?1, bearing_deg, source, computed_at FROM stop_bearing
             WHERE stop_version_id = ?2",
            params![pole.old_svi, pole.new_svi],
        )?;
        tx.execute(
            "DELETE FROM stop_bearing WHERE stop_version_id = ?1",
            params![pole.new_svi],
        )?;
    }

    tx.execute(
        "INSERT INTO edit_log (ts, editor, entity, entity_id, action, changes, note, undo_of, version_before, version_after)
         VALUES (?1, ?2, 'station', ?3, 'undo', '{}', ?4, ?5, ?6, ?7)",
        params![
            now,
            editor,
            op.station_id,
            if note.is_empty() { None } else { Some(note) },
            op.edit_id,
            op.new_station_version_id,
            op.old_station_version_id
        ],
    )?;
    let undo_edit_id = tx.last_insert_rowid();
    tx.commit()?;
    Ok(StationUndone {
        edit_id: undo_edit_id,
        restored_station_version_id: op.old_station_version_id,
    })
}

fn apply_move_undo(
    conn: &Connection,
    op: &StationMoveUndoOp,
    editor: &str,
    note: &str,
) -> Result<StationUndone> {
    let tx = conn.unchecked_transaction()?;
    let now = crate::now_rfc3339();

    // guards BEFORE any write: the minted version must still be current and
    // manual (a second move/rename on top makes this undo stale)
    let n: i64 = tx.query_row(
        "SELECT COUNT(*) FROM station_version
         WHERE station_version_id = ?1 AND valid_to IS NULL AND source = 'manual'",
        [op.new_station_version_id],
        |r| r.get(0),
    )?;
    if n == 0 {
        return Err(EditError::StationNotCurrent(op.new_station_version_id).into());
    }

    let n = tx.execute(
        "UPDATE station_version SET valid_to = ?1
         WHERE station_version_id = ?2 AND valid_to IS NULL",
        params![now, op.new_station_version_id],
    )?;
    if n != 1 {
        return Err(EditError::NotUndoable(op.edit_id).into());
    }
    let n = tx.execute(
        "UPDATE station_version SET valid_to = NULL WHERE station_version_id = ?1",
        params![op.old_station_version_id],
    )?;
    if n != 1 {
        return Err(EditError::NotUndoable(op.edit_id).into());
    }

    tx.execute(
        "INSERT INTO edit_log (ts, editor, entity, entity_id, action, changes, note, undo_of, version_before, version_after)
         VALUES (?1, ?2, 'station', ?3, 'undo', '{}', ?4, ?5, ?6, ?7)",
        params![
            now,
            editor,
            op.station_id,
            if note.is_empty() { None } else { Some(note) },
            op.edit_id,
            op.new_station_version_id,
            op.old_station_version_id
        ],
    )?;
    let undo_edit_id = tx.last_insert_rowid();
    tx.commit()?;
    Ok(StationUndone {
        edit_id: undo_edit_id,
        restored_station_version_id: op.old_station_version_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_in_memory;
    use rusqlite::Connection;

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
             VALUES (1, 1, 1, 'S', 24.0, 120.0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')", []).unwrap();
        conn.execute(
            "INSERT INTO stop (stop_id, created_at) VALUES (1, '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO stop_version (stop_version_id, stop_id, station_id, stop_code, stop_name, direction, latitude, longitude, valid_from, created_at)
             VALUES (10, 1, 1, '001', 'Old', 0, 24.0, 120.0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')", []).unwrap();
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
            "INSERT INTO id_sequence (table_name, next_value) VALUES ('stop_version', 11)",
            [],
        )
        .unwrap();
        10
    }

    /// import_batch 1 (CLOSED) + src_route_point for xno '1' dir 1: the
    /// 2-point Google reference polyline (38.5,-120.2)→(40.7,-120.95).
    fn src_fixture(conn: &Connection) {
        conn.execute(
            "INSERT INTO import_batch (batch_id, source_dir, file_count, captured_at, valid_from, status, created_at)
             VALUES (1, 'assets/x', 1, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'CLOSED', '2026-01-01T00:00:00Z')", []).unwrap();
        conn.execute(
            "INSERT INTO src_route_point (batch_id, xno, direction, encoded)
             VALUES (1, '1', 1, '_p~iF~ps|U_ulLnnqC')",
            [],
        )
        .unwrap();
    }

    fn line_op() -> LineEditOp {
        LineEditOp {
            xno: "1".into(),
            direction: 1,
            encoded: "_ulLnnqC_p~iF~ps|U".into(),
            expected_updated_at: None,
        }
    }

    fn line_undo_op(edit_id: i64, undone_ts: String, restored: Option<String>) -> LineUndoOp {
        LineUndoOp {
            edit_id,
            route_id: 1,
            xno: "1".into(),
            direction: 1,
            undone_ts,
            restored,
        }
    }

    #[test]
    fn line_edit_creates_updates_and_marks_four_stale() {
        let conn = open_in_memory().unwrap();
        fixture(&conn);
        src_fixture(&conn);
        let a = apply_line_edit(&conn, &line_op(), "h", "n1").unwrap();
        assert!(a.created);
        let (enc, ts): (String, String) = conn
            .query_row(
                "SELECT encoded, updated_at FROM polyline_override WHERE xno = '1' AND direction = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(enc, "_ulLnnqC_p~iF~ps|U");
        assert_eq!(ts, a.updated_at);
        // log row: entity polyline, entity_id = route_id 1, action create,
        // changes carry new only
        let (ent, eid, act, changes): (String, i64, String, String) = conn
            .query_row(
                "SELECT entity, entity_id, action, changes FROM edit_log",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!((ent.as_str(), eid, act.as_str()), ("polyline", 1, "create"));
        let c: serde_json::Value = serde_json::from_str(&changes).unwrap();
        assert_eq!(c["kind"], "polyline_override");
        assert_eq!(c["encoded"]["new"], "_ulLnnqC_p~iF~ps|U");
        assert!(c["encoded"].get("old").is_none());
        // four stale marks, seg_match keyed by route_id:direction
        let mut marks: Vec<(String, String)> = Vec::new();
        let mut stmt = conn
            .prepare("SELECT table_name, key FROM stale_mark")
            .unwrap();
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        for r in rows {
            marks.push(r.unwrap());
        }
        assert!(
            marks.contains(&("seg_match".into(), "1:1".into())),
            "{marks:?}"
        );
        assert!(marks.contains(&("path_stop_projection".into(), "1".into())));
        assert!(marks.contains(&("stop_bearing".into(), "*".into())));
        assert!(marks.contains(&("station_bearing".into(), "*".into())));
        // update: currency token from create; upsert, not a second row
        let mut op = line_op();
        op.expected_updated_at = Some(a.updated_at.clone());
        op.encoded = "_p~iF~ps|U_ulLnnqC".into();
        let b = apply_line_edit(&conn, &op, "h", "n2").unwrap();
        assert!(!b.created);
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM polyline_override", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        let act: String = conn
            .query_row(
                "SELECT action FROM edit_log ORDER BY edit_id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(act, "update");
    }

    #[test]
    fn line_edit_stale_expectation_refused() {
        let conn = open_in_memory().unwrap();
        fixture(&conn);
        src_fixture(&conn);
        let _ = apply_line_edit(&conn, &line_op(), "h", "n1").unwrap();
        // now_rfc3339 is second-granular — the row's token equals any edit
        // made in the same second, so a stale expectation is simulated with
        // an older token (phase-1 pattern)
        let mut op = line_op();
        op.expected_updated_at = Some("2020-01-01T00:00:00Z".into());
        op.encoded = "different".into();
        let err = apply_line_edit(&conn, &op, "h", "").unwrap_err();
        assert!(err.to_string().contains("edited by someone else"), "{err}");
        // expectation against a NONEXISTENT override also refuses (it was
        // undone away between load and save) — clean 409, never a panic
        let conn2 = open_in_memory().unwrap();
        fixture(&conn2);
        src_fixture(&conn2);
        let mut op = line_op();
        op.expected_updated_at = Some("2020-01-01T00:00:00Z".into());
        let err = apply_line_edit(&conn2, &op, "h", "").unwrap_err();
        assert!(err.to_string().contains("edited by someone else"), "{err}");
    }

    #[test]
    fn line_undo_of_create_deletes_row_and_marks_stale() {
        let conn = open_in_memory().unwrap();
        fixture(&conn);
        src_fixture(&conn);
        let a = apply_line_edit(&conn, &line_op(), "h", "n1").unwrap();
        let out = apply_line_undo(
            &conn,
            &line_undo_op(a.edit_id, a.updated_at, None),
            "h",
            "rev",
        )
        .unwrap();
        assert!(out.deleted);
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM polyline_override", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "back to pure src");
        let (act, undo_of, changes): (String, i64, String) = conn
            .query_row(
                "SELECT action, undo_of, changes FROM edit_log ORDER BY edit_id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(act, "undo");
        assert_eq!(undo_of, a.edit_id);
        let c: serde_json::Value = serde_json::from_str(&changes).unwrap();
        assert_eq!(c["encoded"]["new"], serde_json::Value::Null);
        // same four marks on undo
        let marks: i64 = conn
            .query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0))
            .unwrap();
        assert_eq!(marks, 4);
    }

    #[test]
    fn line_undo_restores_prior_encoded() {
        let conn = open_in_memory().unwrap();
        fixture(&conn);
        src_fixture(&conn);
        let a = apply_line_edit(&conn, &line_op(), "h", "n1").unwrap();
        let mut op = line_op();
        op.expected_updated_at = Some(a.updated_at.clone());
        let b = apply_line_edit(&conn, &op, "h", "n2").unwrap();
        let out = apply_line_undo(
            &conn,
            &line_undo_op(b.edit_id, b.updated_at, Some("_ulLnnqC_p~iF~ps|U".into())),
            "h",
            "rev",
        )
        .unwrap();
        assert!(!out.deleted);
        let enc: String = conn
            .query_row(
                "SELECT encoded FROM polyline_override WHERE xno = '1' AND direction = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(enc, "_ulLnnqC_p~iF~ps|U");
    }

    #[test]
    fn line_undo_superseded_refused() {
        let conn = open_in_memory().unwrap();
        fixture(&conn);
        src_fixture(&conn);
        let a = apply_line_edit(&conn, &line_op(), "h", "n1").unwrap();
        // line moved on after edit a (now_rfc3339 is second-granular, so
        // diverge the row directly to make this deterministic)
        let mut op = line_op();
        op.expected_updated_at = Some(a.updated_at.clone());
        apply_line_edit(&conn, &op, "h", "n2").unwrap();
        conn.execute(
            "UPDATE polyline_override SET updated_at = '2099-01-01T00:00:00Z'
             WHERE xno = '1' AND direction = 1",
            [],
        )
        .unwrap();
        let err = apply_line_undo(
            &conn,
            &line_undo_op(a.edit_id, a.updated_at, None),
            "h",
            "rev",
        )
        .unwrap_err();
        assert!(err.to_string().contains("edited by someone else"), "{err}");
    }

    #[test]
    fn load_polyline_line_resolves_route_src_and_override() {
        let conn = open_in_memory().unwrap();
        fixture(&conn);
        src_fixture(&conn);
        conn.execute("UPDATE route SET current_route_version_id = 1", [])
            .unwrap();
        let snap = load_polyline_line(&conn, "1", 1).unwrap().unwrap();
        assert_eq!(snap.route_id, 1);
        assert_eq!(snap.src_encoded.as_deref(), Some("_p~iF~ps|U_ulLnnqC"));
        assert!(snap.override_row.is_none());
        assert_eq!(snap.route_version_id, Some(1));
        assert!(load_polyline_line(&conn, "999", 1).unwrap().is_none());
        apply_line_edit(&conn, &line_op(), "h", "n").unwrap();
        let snap = load_polyline_line(&conn, "1", 1).unwrap().unwrap();
        assert_eq!(snap.override_row.unwrap().encoded, "_ulLnnqC_p~iF~ps|U");
    }

    #[test]
    fn effective_encoded_prefers_override_then_src() {
        let conn = open_in_memory().unwrap();
        fixture(&conn);
        src_fixture(&conn);
        // src branch
        assert_eq!(
            effective_encoded(&conn, 1, "1", 1).unwrap(),
            "_p~iF~ps|U_ulLnnqC"
        );
        // neither branch: "" (readers treat "" as no_path)
        assert_eq!(effective_encoded(&conn, 1, "1", 2).unwrap(), "");
        // override branch
        let a = apply_line_edit(&conn, &line_op(), "h", "n").unwrap();
        assert_eq!(
            effective_encoded(&conn, 1, "1", 1).unwrap(),
            "_ulLnnqC_p~iF~ps|U"
        );
        // REVIEW-FOCUS pin: garbage encoded passes through VERBATIM — readers
        // own decode failure; effective_encoded never falls back to src
        let mut op2 = line_op();
        op2.expected_updated_at = Some(a.updated_at.clone());
        op2.encoded = "not-a-polyline".into();
        apply_line_edit(&conn, &op2, "h", "n").unwrap();
        assert_eq!(
            effective_encoded(&conn, 1, "1", 1).unwrap(),
            "not-a-polyline"
        );
    }

    #[test]
    fn entity_history_lists_newest_first() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        let a = apply_stop_edit(
            &conn,
            &StopEditOp {
                stop_id: 1,
                stop_version_id: old,
                stop_name: Some("A".into()),
                ..Default::default()
            },
            "h",
            "n1",
        )
        .unwrap();
        apply_stop_undo(
            &conn,
            &stop_edit_undo_op(a.edit_id, 1, old, a.new_version_id),
            "h",
            "n2",
        )
        .unwrap();
        let rows = entity_history(&conn, "stop", 1).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].action, "undo");
        assert_eq!(rows[1].action, "update");
        assert_eq!(rows[0].undo_of, Some(a.edit_id));
    }

    #[test]
    fn re_mark_replaces_row_fresh_rowid_and_cause() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        let e1 = StopEditOp {
            stop_id: 1,
            stop_version_id: old,
            lat: Some(24.001),
            ..Default::default()
        };
        let out1 = apply_stop_edit(&conn, &e1, "h", "first").unwrap();

        let row = |conn: &rusqlite::Connection| -> (i64, i64, String) {
            conn.query_row(
                "SELECT rowid, cause_edit_id, key FROM stale_mark WHERE table_name = 'stop_bearing'",
                [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            ).unwrap()
        };
        let (rid1, cause1, _) = row(&conn);
        assert_eq!(cause1, out1.edit_id);

        // the second position edit re-marks the same key through production code:
        // new cause must win, rowid must change
        let cur = out1.new_version_id;
        let e2 = StopEditOp {
            stop_id: 1,
            stop_version_id: cur,
            lat: Some(24.002),
            ..Default::default()
        };
        let out2 = apply_stop_edit(&conn, &e2, "h", "second").unwrap();
        let (rid2, cause2, key2) = row(&conn);
        assert_eq!(key2, "*");
        assert_eq!(cause2, out2.edit_id, "freshest cause wins");
        assert_ne!(
            rid2, rid1,
            "OR REPLACE mints a fresh rowid (snapshot soundness)"
        );
    }

    #[test]
    fn load_station_snapshot_returns_station_and_current_poles() {
        let conn = open_in_memory().unwrap();
        let _ = fixture(&conn);
        let snap = load_station_snapshot(&conn, 1)
            .unwrap()
            .expect("current station version");
        assert_eq!(snap.station_id, 1);
        assert_eq!(snap.station_version_id, 1);
        assert_eq!(snap.station_name, "S");
        assert_eq!((snap.latitude, snap.longitude), (24.0, 120.0));
        assert_eq!(snap.poles.len(), 1);
        assert_eq!(
            (
                snap.poles[0].stop_id,
                snap.poles[0].stop_version_id,
                snap.poles[0].stop_name.as_str()
            ),
            (1, 10, "Old")
        );
    }

    #[test]
    fn load_station_snapshot_none_for_stale_or_missing_version() {
        let conn = open_in_memory().unwrap();
        let _ = fixture(&conn);
        // close the station version → no longer current
        conn.execute("UPDATE station_version SET valid_to = '2026-01-02T00:00:00Z' WHERE station_version_id = 1", []).unwrap();
        assert!(load_station_snapshot(&conn, 1).unwrap().is_none());
        assert!(load_station_snapshot(&conn, 999).unwrap().is_none());
    }

    #[test]
    fn load_stop_snapshot_returns_current_row() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        let s = load_stop_snapshot(&conn, old).unwrap().expect("current");
        assert_eq!((s.stop_id, s.station_id, s.stop_version_id), (1, 1, old));
        assert_eq!(s.stop_name, "Old");
        assert_eq!((s.latitude, s.longitude), (24.0, 120.0));
    }

    #[test]
    fn load_stop_snapshot_none_for_non_current_or_missing() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        apply_stop_edit(
            &conn,
            &StopEditOp {
                stop_id: 1,
                stop_version_id: old,
                stop_name: Some("N".into()),
                ..Default::default()
            },
            "h",
            "n",
        )
        .unwrap();
        // old is now closed; the minted version is current
        assert!(load_stop_snapshot(&conn, old).unwrap().is_none());
        assert!(load_stop_snapshot(&conn, 999).unwrap().is_none());
    }

    #[test]
    fn station_is_current_checks_valid_to() {
        let conn = open_in_memory().unwrap();
        fixture(&conn);
        assert!(station_is_current(&conn, 1).unwrap());
        assert!(!station_is_current(&conn, 42).unwrap());
    }

    #[test]
    fn load_station_edit_returns_parsed_row() {
        let conn = open_in_memory().unwrap();
        rename_fixture(&conn);
        let out = apply_station_rename(&conn, &all_poles_op(), "herry", "n").unwrap();
        let e = load_station_edit(&conn, out.edit_id)
            .unwrap()
            .expect("row exists");
        assert_eq!(e.edit_id, out.edit_id);
        assert_eq!(e.station_id, 1);
        assert_eq!(e.action, "update");
        assert_eq!(e.old_station_version_id, Some(20));
        assert_eq!(e.new_station_version_id, Some(out.new_station_version_id));
        assert_eq!(e.changes["kind"], "station_rename");
        assert_eq!(e.changes["stops"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn load_station_edit_none_for_missing_id() {
        let conn = open_in_memory().unwrap();
        assert!(load_station_edit(&conn, 999).unwrap().is_none());
    }

    /// Station-rename fixture: station 'S' (sv 20) + poles sv 10 'S' (with
    /// bearing) and sv 11 'S 往' (no bearing), rsv pointing at sv 10.
    fn rename_fixture(conn: &Connection) -> i64 {
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
            conn.execute(
                "INSERT INTO stop (stop_id, ext_uid, created_at) VALUES (?1, 'tcg:stop:' || ?1, '2026-01-01T00:00:00Z')", [sid]).unwrap();
            conn.execute(
                "INSERT INTO stop_version (stop_version_id, stop_id, station_id, stop_code, stop_name, direction, latitude, longitude, valid_from, created_at)
                 VALUES (?1, ?2, 1, '001', ?3, 0, 24.0, 120.0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
                params![svi, sid, name]).unwrap();
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
        20
    }

    fn all_poles_op() -> StationRenameOp {
        StationRenameOp {
            station_id: 1,
            old_station_version_id: 20,
            old_station_name: "S".into(),
            new_station_name: "台北車站".into(),
            level: "AllPoles".into(),
            poles: vec![
                PoleOp {
                    stop_id: 1,
                    old_stop_version_id: 10,
                    new_stop_name: "台北車站".into(),
                },
                PoleOp {
                    stop_id: 2,
                    old_stop_version_id: 11,
                    new_stop_name: "台北車站".into(),
                },
            ],
        }
    }

    fn station_only_op() -> StationRenameOp {
        StationRenameOp {
            station_id: 1,
            old_station_version_id: 20,
            old_station_name: "S".into(),
            new_station_name: "台北車站".into(),
            level: "StationOnly".into(),
            poles: vec![],
        }
    }

    fn seed_renamed(conn: &Connection) -> crate::StationRenameApplied {
        rename_fixture(conn);
        apply_station_rename(conn, &all_poles_op(), "herry", "rename").unwrap()
    }

    fn undo_op(applied: &crate::StationRenameApplied) -> StationRenameUndoOp {
        StationRenameUndoOp {
            edit_id: applied.edit_id,
            station_id: 1,
            old_station_version_id: 20,
            new_station_version_id: applied.new_station_version_id,
            poles: applied
                .poles
                .iter()
                .map(|p| PoleUndoOp {
                    stop_id: p.stop_id,
                    old_svi: p.old_svi,
                    new_svi: p.new_svi,
                })
                .collect(),
        }
    }

    #[test]
    fn apply_renames_station_and_all_poles_one_tx() {
        let conn = open_in_memory().unwrap();
        let old = rename_fixture(&conn);
        let out = apply_station_rename(&conn, &all_poles_op(), "herry", "fix").unwrap();
        // station version appended: old closed, new current + manual + new name
        let (to, _): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT valid_to, source FROM station_version WHERE station_version_id = ?1",
                [old],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(to.is_some());
        let (name, src): (String, String) = conn
            .query_row(
                "SELECT station_name, source FROM station_version WHERE station_version_id = ?1",
                [out.new_station_version_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((name.as_str(), src.as_str()), ("台北車站", "manual"));
        // both poles churned to the new name, source manual
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM stop_version WHERE stop_name = '台北車站' AND source = 'manual' AND valid_to IS NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2);
        // rsv repointed from sv 10 to the churned pole
        let sv: i64 = conn
            .query_row(
                "SELECT stop_version_id FROM route_stop_version WHERE route_stop_version_id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sv, out.poles[0].new_svi);
        assert_eq!(out.poles.len(), 2);
    }

    #[test]
    fn apply_leaves_zero_stale_marks() {
        let conn = open_in_memory().unwrap();
        let old = rename_fixture(&conn);
        apply_station_rename(&conn, &all_poles_op(), "herry", "fix").unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "rename invalidates no derived data");
        // station_bearing untouched by design; none seeded — still none
        let _ = old;
    }

    #[test]
    fn apply_carries_bearing_rows_only_where_present() {
        let conn = open_in_memory().unwrap();
        rename_fixture(&conn);
        let out = apply_station_rename(&conn, &all_poles_op(), "herry", "fix").unwrap();
        // pole 1 (had bearing 123.5 @ computed_at): row now on new svi, same values
        let (deg, src, at): (f64, String, String) = conn.query_row(
            "SELECT bearing_deg, source, computed_at FROM stop_bearing WHERE stop_version_id = ?1",
            [out.poles[0].new_svi], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!(
            (deg, src.as_str(), at.as_str()),
            (123.5, "OSM", "2026-01-02T00:00:00Z")
        );
        // old svi row gone (PK moved), pole 2 still has none
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM stop_bearing", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn apply_records_edit_log_row_shaped_for_undo() {
        let conn = open_in_memory().unwrap();
        let old = rename_fixture(&conn);
        let out = apply_station_rename(&conn, &all_poles_op(), "herry", "note!").unwrap();
        let (entity, eid, action, changes, note, vb, va): (String, i64, String, String, Option<String>, Option<i64>, Option<i64>) = conn.query_row(
            "SELECT entity, entity_id, action, changes, note, version_before, version_after FROM edit_log WHERE edit_id = ?1",
            [out.edit_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))).unwrap();
        assert_eq!(
            (entity.as_str(), eid, action.as_str()),
            ("station", 1, "update")
        );
        assert_eq!(note.as_deref(), Some("note!"));
        assert_eq!((vb, va), (Some(old), Some(out.new_station_version_id)));
        // CJK round-trip + undo-ready shape
        let c: serde_json::Value = serde_json::from_str(&changes).unwrap();
        assert_eq!(c["kind"], "station_rename");
        assert_eq!(c["level"], "AllPoles");
        assert_eq!(c["old_name"], "S");
        assert_eq!(c["new_name"], "台北車站");
        assert_eq!(c["stops"].as_array().unwrap().len(), 2);
        assert_eq!(c["stops"][0]["stop_id"], 1);
        assert_eq!(c["stops"][0]["old_svi"], 10);
        assert_eq!(c["stops"][0]["new_svi"], out.poles[0].new_svi);
    }

    #[test]
    fn apply_rejects_stale_station_version() {
        let conn = open_in_memory().unwrap();
        let old = rename_fixture(&conn);
        apply_station_rename(&conn, &all_poles_op(), "herry", "first").unwrap();
        // double-submit of the same station version id
        let err = apply_station_rename(&conn, &all_poles_op(), "herry", "second").unwrap_err();
        assert!(err.to_string().contains("not the current version"), "{err}");
        // no partial state from the refused attempt: still exactly one manual
        // station version, one edit_log row, one bearing row
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM station_version WHERE source = 'manual'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM edit_log", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM stop_bearing", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        let _ = old;
    }

    #[test]
    fn apply_aborts_when_pole_not_current() {
        let conn = open_in_memory().unwrap();
        rename_fixture(&conn);
        // churn pole 10 behind the plan's back (name-only edit)
        apply_stop_edit(
            &conn,
            &StopEditOp {
                stop_id: 1,
                stop_version_id: 10,
                stop_name: Some("shadowed".into()),
                ..Default::default()
            },
            "h",
            "n",
        )
        .unwrap();
        let err = apply_station_rename(&conn, &all_poles_op(), "herry", "stale pole").unwrap_err();
        assert!(err.to_string().contains("not the current version"), "{err}");
        // whole tx rolled back (the setup name-only edit writes zero marks:
        // bearing carried, no stop_bearing mark; its manual stop_version is
        // not this tx's): no manual station version, no station edit_log row
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM station_version WHERE source = 'manual'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM edit_log WHERE entity = 'station'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn apply_rolls_back_earlier_poles_when_later_pole_stale() {
        let conn = open_in_memory().unwrap();
        rename_fixture(&conn);
        // stale pole listed LAST: pole 10 churns fully inside the tx before
        // pole 11's staleness aborts it — the rollback of that churn is the
        // property under test (no partial station/pole mix). all_poles_op
        // lists [10, 11]; churn 11 behind the plan's back so it's stale.
        apply_stop_edit(
            &conn,
            &StopEditOp {
                stop_id: 2,
                stop_version_id: 11,
                stop_name: Some("shadowed".into()),
                ..Default::default()
            },
            "h",
            "n",
        )
        .unwrap();
        let err = apply_station_rename(&conn, &all_poles_op(), "herry", "stale last").unwrap_err();
        assert!(err.to_string().contains("not the current version"), "{err}");
        // pole 10's churn rolled back: original row current again, name 'S'
        let (to, name): (Option<String>, String) = conn
            .query_row(
                "SELECT valid_to, stop_name FROM stop_version WHERE stop_version_id = 10",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(to.is_none(), "pole 10 unclosed by rollback");
        assert_eq!(name, "S");
        // its bearing row came back to svi 10 (exactly one row, on svi 10)
        let (n, on10): (i64, i64) = conn.query_row(
            "SELECT COUNT(*), SUM(CASE WHEN stop_version_id = 10 THEN 1 ELSE 0 END) FROM stop_bearing", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((n, on10), (1, 1));
        // rsv still points at svi 10
        let sv: i64 = conn
            .query_row(
                "SELECT stop_version_id FROM route_stop_version WHERE route_stop_version_id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sv, 10);
        // no trace of this tx: no manual station version, no station edit_log,
        // and the only manual stop_version is the setup edit's churn of pole 11
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM station_version WHERE source = 'manual'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM edit_log WHERE entity = 'station'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM stop_version WHERE source = 'manual'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "only the setup edit's churn of pole 11");
    }

    #[test]
    fn undo_moves_bearing_row_back_first() {
        let conn = open_in_memory().unwrap();
        let applied = seed_renamed(&conn);
        // PK invariant: the row MOVES back new_svi → old_svi, values intact
        apply_station_undo(
            &conn,
            &StationUndoOp::Rename(undo_op(&applied)),
            "herry",
            "revert",
        )
        .unwrap();
        let (deg, src, at): (f64, String, String) = conn.query_row(
            "SELECT bearing_deg, source, computed_at FROM stop_bearing WHERE stop_version_id = 10", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!(
            (deg, src.as_str(), at.as_str()),
            (123.5, "OSM", "2026-01-02T00:00:00Z")
        );
        // exactly one row in the table; pole 11 still has none
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM stop_bearing", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn undo_restores_station_and_poles() {
        let conn = open_in_memory().unwrap();
        let applied = seed_renamed(&conn);
        let out = apply_station_undo(
            &conn,
            &StationUndoOp::Rename(undo_op(&applied)),
            "herry",
            "revert",
        )
        .unwrap();
        assert_eq!(out.restored_station_version_id, 20);
        // station: old row current with old name, minted row closed
        let (name, to): (String, Option<String>) = conn
            .query_row(
                "SELECT station_name, valid_to FROM station_version WHERE station_version_id = 20",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((name.as_str(), to), ("S", None));
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM station_version WHERE valid_to IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "only the restored row is current");
        // poles: old rows current with original names, minted rows closed,
        // nothing re-minted (still exactly 4 stop_version rows)
        let mut stmt = conn.prepare(
            "SELECT stop_name FROM stop_version WHERE valid_to IS NULL ORDER BY stop_version_id").unwrap();
        let names: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(names, vec!["S".to_string(), "S 往".to_string()]);
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM stop_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 4, "undo mints nothing");
        // rsv repointed back to the original pole version
        let sv: i64 = conn
            .query_row(
                "SELECT stop_version_id FROM route_stop_version WHERE route_stop_version_id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sv, 10);
    }

    #[test]
    fn undo_writes_edit_log_row_and_zero_stale_marks() {
        let conn = open_in_memory().unwrap();
        let applied = seed_renamed(&conn);
        let out = apply_station_undo(
            &conn,
            &StationUndoOp::Rename(undo_op(&applied)),
            "herry",
            "revert!",
        )
        .unwrap();
        let (entity, eid, action, changes, note, undo_of, vb, va): (String, i64, String, String, Option<String>, Option<i64>, Option<i64>, Option<i64>) = conn.query_row(
            "SELECT entity, entity_id, action, changes, note, undo_of, version_before, version_after FROM edit_log WHERE edit_id = ?1",
            [out.edit_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?))).unwrap();
        assert_eq!(
            (entity.as_str(), eid, action.as_str()),
            ("station", 1, "undo")
        );
        assert_eq!(changes, "{}");
        assert_eq!(note.as_deref(), Some("revert!"));
        assert_eq!(undo_of, Some(applied.edit_id));
        assert_eq!((vb, va), (Some(applied.new_station_version_id), Some(20)));
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "undo of a rename invalidates no derived data");
    }

    #[test]
    fn undo_station_only_touches_no_poles() {
        let conn = open_in_memory().unwrap();
        rename_fixture(&conn);
        let applied = apply_station_rename(&conn, &station_only_op(), "herry", "n").unwrap();
        assert!(applied.poles.is_empty());
        apply_station_undo(
            &conn,
            &StationUndoOp::Rename(undo_op(&applied)),
            "herry",
            "revert",
        )
        .unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM stop_version WHERE valid_to IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 2, "both original poles untouched and current");
        let name: String = conn
            .query_row(
                "SELECT station_name FROM station_version WHERE valid_to IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(name, "S");
    }

    #[test]
    fn undo_refuses_when_station_renamed_again() {
        let conn = open_in_memory().unwrap();
        let first = seed_renamed(&conn);
        // rename again on the minted version + minted pole versions →
        // first rename no longer current
        let second_op = StationRenameOp {
            old_station_version_id: first.new_station_version_id,
            new_station_name: "站".into(),
            poles: first
                .poles
                .iter()
                .map(|p| PoleOp {
                    stop_id: p.stop_id,
                    old_stop_version_id: p.new_svi,
                    new_stop_name: "站".into(),
                })
                .collect(),
            ..all_poles_op()
        };
        apply_station_rename(&conn, &second_op, "herry", "second").unwrap();
        let err = apply_station_undo(
            &conn,
            &StationUndoOp::Rename(undo_op(&first)),
            "herry",
            "revert",
        )
        .unwrap_err();
        assert!(err.to_string().contains("not the current version"), "{err}");
        // zero partial state: still exactly two manual station versions,
        // two station edit_log rows (two renames, no undo row)
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM station_version WHERE source = 'manual'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 2);
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM edit_log WHERE entity = 'station'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn undo_refuses_when_pole_edited_after_rename() {
        let conn = open_in_memory().unwrap();
        let applied = seed_renamed(&conn);
        // post-rename pole edit: churns pole 1's minted version
        apply_stop_edit(
            &conn,
            &StopEditOp {
                stop_id: applied.poles[0].stop_id,
                stop_version_id: applied.poles[0].new_svi,
                stop_name: Some("shadowed".into()),
                ..Default::default()
            },
            "h",
            "n",
        )
        .unwrap();
        let err = apply_station_undo(
            &conn,
            &StationUndoOp::Rename(undo_op(&applied)),
            "herry",
            "revert",
        )
        .unwrap_err();
        assert!(err.to_string().contains("cannot be undone"), "{err}");
        // zero partial state: minted station version still current, no undo row
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM station_version WHERE valid_to IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM edit_log WHERE entity = 'station' AND action = 'undo'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn undo_rolls_back_earlier_pole_inversions_when_later_pole_stale() {
        let conn = open_in_memory().unwrap();
        let applied = seed_renamed(&conn);
        // stale pole listed LAST: all pole guards run before any UPDATE, so
        // pole 2's staleness refuses the tx before pole 1 is touched —
        // pole 1's untouched state is the property under test
        apply_stop_edit(
            &conn,
            &StopEditOp {
                stop_id: applied.poles[1].stop_id,
                stop_version_id: applied.poles[1].new_svi,
                stop_name: Some("shadowed".into()),
                ..Default::default()
            },
            "h",
            "n",
        )
        .unwrap();
        let err = apply_station_undo(
            &conn,
            &StationUndoOp::Rename(undo_op(&applied)),
            "herry",
            "revert",
        )
        .unwrap_err();
        assert!(err.to_string().contains("cannot be undone"), "{err}");
        // pole 1's inversion rolled back: minted version current again,
        // original closed, bearing still on the minted svi, rsv untouched
        let (to, on_new): (Option<String>, i64) = conn
            .query_row(
                "SELECT valid_to, (SELECT COUNT(*) FROM stop_bearing WHERE stop_version_id = ?1)
             FROM stop_version WHERE stop_version_id = ?1",
                [applied.poles[0].new_svi],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(to.is_none(), "pole 1's minted version unclosed by rollback");
        assert_eq!(on_new, 1, "bearing row still on pole 1's minted svi");
        let to: Option<String> = conn
            .query_row(
                "SELECT valid_to FROM stop_version WHERE stop_version_id = 10",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(to.is_some(), "pole 1's original still closed");
        let sv: i64 = conn
            .query_row(
                "SELECT stop_version_id FROM route_stop_version WHERE route_stop_version_id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sv, applied.poles[0].new_svi, "rsv still on the minted svi");
    }

    #[test]
    fn apply_stop_edit_moves_position_carries_bearing_marks_three() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn); // fixture has no bearing row — seed one
        conn.execute(
            "INSERT INTO stop_bearing (stop_version_id, bearing_deg, source, computed_at)
             VALUES (10, 123.5, 'OSM', '2026-01-02T00:00:00Z')",
            [],
        )
        .unwrap();
        let op = StopEditOp {
            stop_id: 1,
            stop_version_id: old,
            lat: Some(24.001),
            lon: None,
            stop_name: None,
        };
        let out = apply_stop_edit(&conn, &op, "herry", "move").unwrap();
        assert!(out.new_version_id > old);
        // old closed, new current + manual, position applied
        let (to, lat, src): (Option<String>, f64, String) = conn
            .query_row(
                "SELECT valid_to, latitude, source FROM stop_version WHERE stop_version_id = ?1",
                [out.new_version_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((to.is_some(), lat, src.as_str()), (false, 24.001, "manual"));
        // rsv repointed
        let sv: i64 = conn
            .query_row(
                "SELECT stop_version_id FROM route_stop_version WHERE route_stop_version_id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sv, out.new_version_id);
        // bearing carried to the new svi (value kept, recompute refreshes it later)
        let (deg, computed): (f64, String) = conn
            .query_row(
                "SELECT bearing_deg, computed_at FROM stop_bearing WHERE stop_version_id = ?1",
                [out.new_version_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((deg, computed.as_str()), (123.5, "2026-01-02T00:00:00Z"));
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM stop_bearing WHERE stop_version_id = ?1",
                [old],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
        // marks: stop_bearing + projection(route 1) + station_bearing
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 3);
        assert_eq!(out.stale_routes, vec![1]);
        // edit_log: {field:{old,new}} shape
        let (ent, act, changes): (String, String, String) = conn
            .query_row(
                "SELECT entity, action, changes FROM edit_log WHERE edit_id = ?1",
                [out.edit_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((ent.as_str(), act.as_str()), ("stop", "update"));
        let c: serde_json::Value = serde_json::from_str(&changes).unwrap();
        assert_eq!(c["latitude"]["new"], 24.001);
        assert!(c["stop_name"].is_null());
    }

    #[test]
    fn apply_stop_edit_name_only_carries_bearing_zero_marks() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn); // fixture has no bearing row — seed one
        conn.execute(
            "INSERT INTO stop_bearing (stop_version_id, bearing_deg, source, computed_at)
             VALUES (10, 123.5, 'OSM', '2026-01-02T00:00:00Z')",
            [],
        )
        .unwrap();
        let op = StopEditOp {
            stop_id: 1,
            stop_version_id: old,
            stop_name: Some("New".into()),
            ..Default::default()
        };
        let out = apply_stop_edit(&conn, &op, "herry", "rename").unwrap();
        // bearing row MOVED to the new svi, identical values + computed_at
        let (deg, computed): (f64, String) = conn
            .query_row(
                "SELECT bearing_deg, computed_at FROM stop_bearing WHERE stop_version_id = ?1",
                [out.new_version_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((deg, computed.as_str()), (123.5, "2026-01-02T00:00:00Z"));
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM stop_bearing WHERE stop_version_id = ?1",
                [old],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
        // zero stale marks — a rename invalidates no derived data
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
        assert!(out.stale_routes.is_empty());
    }

    #[test]
    fn apply_stop_edit_refuses_stale_version_zero_partial() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        let first = StopEditOp {
            stop_id: 1,
            stop_version_id: old,
            stop_name: Some("A".into()),
            ..Default::default()
        };
        apply_stop_edit(&conn, &first, "h", "n").unwrap();
        let err = apply_stop_edit(&conn, &first, "h", "n").unwrap_err();
        assert!(err.to_string().contains("not the current version"), "{err}");
        // zero partial: exactly one minted version exists
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM stop_version WHERE stop_id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 2);
    }

    fn regroup_op(old: i64, new_station: i64) -> StopRegroupOp {
        StopRegroupOp {
            stop_id: 1,
            stop_version_id: old,
            new_station_id: new_station,
        }
    }

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
        conn.execute(
            "INSERT INTO id_sequence (table_name, next_value) VALUES ('station_version', 22)",
            [],
        )
        .unwrap();
    }

    #[test]
    fn apply_stop_regroup_churns_membership_carries_bearing() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn); // fixture has no bearing row — seed one
        conn.execute(
            "INSERT INTO stop_bearing (stop_version_id, bearing_deg, source, computed_at)
             VALUES (10, 123.5, 'OSM', '2026-01-02T00:00:00Z')",
            [],
        )
        .unwrap();
        second_station(&conn);
        let out = apply_stop_regroup(&conn, &regroup_op(old, 2), "herry", "regroup").unwrap();
        // new version carries the new station_id, same position, manual
        let (st, lat, lon, src): (i64, f64, f64, String) = conn.query_row(
            "SELECT station_id, latitude, longitude, source FROM stop_version WHERE stop_version_id = ?1",
            [out.new_version_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap();
        assert_eq!((st, lat, lon, src.as_str()), (2, 24.0, 120.0, "manual"));
        // old closed
        let to: Option<String> = conn
            .query_row(
                "SELECT valid_to FROM stop_version WHERE stop_version_id = ?1",
                [old],
                |r| r.get(0),
            )
            .unwrap();
        assert!(to.is_some());
        // bearing carried over to the new svi (position unchanged)
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM stop_bearing WHERE stop_version_id = ?1",
                [out.new_version_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
        // rsv repointed
        let sv: i64 = conn
            .query_row(
                "SELECT stop_version_id FROM route_stop_version WHERE route_stop_version_id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sv, out.new_version_id);
        // marks: station_bearing ONLY
        let rows: Vec<(String, String)> = conn
            .prepare("SELECT table_name, key FROM stale_mark")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(rows, vec![("station_bearing".into(), "*".into())]);
        assert!(out.stale_routes.is_empty());
        // edit_log: action='move_station', changes={station_id:{old,new}}
        let (act, changes): (String, String) = conn
            .query_row(
                "SELECT action, changes FROM edit_log WHERE edit_id = ?1",
                [out.edit_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(act, "move_station");
        let c: serde_json::Value = serde_json::from_str(&changes).unwrap();
        assert_eq!(c["station_id"]["old"], 1);
        assert_eq!(c["station_id"]["new"], 2);
    }

    #[test]
    fn apply_stop_regroup_refuses_unknown_target_and_stale_source() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        second_station(&conn);
        let err = apply_stop_regroup(&conn, &regroup_op(old, 42), "h", "n").unwrap_err();
        assert!(
            err.to_string().contains("does not exist or is not current"),
            "{err}"
        );
        // stale source: regroup twice from the same version
        let out = apply_stop_regroup(&conn, &regroup_op(old, 2), "h", "n").unwrap();
        let err = apply_stop_regroup(&conn, &regroup_op(old, 2), "h", "n").unwrap_err();
        assert!(err.to_string().contains("not the current version"), "{err}");
        let _ = out;
    }

    fn stop_edit_undo_op(edit_id: i64, stop_id: i64, old: i64, cur: i64) -> StopEditUndoOp {
        StopEditUndoOp {
            edit_id,
            stop_id,
            old_svi: old,
            cur_svi: cur,
        }
    }

    #[test]
    fn load_stop_edit_returns_parsed_row_any_entity() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        let out = apply_stop_edit(
            &conn,
            &StopEditOp {
                stop_id: 1,
                stop_version_id: old,
                stop_name: Some("N".into()),
                ..Default::default()
            },
            "h",
            "n",
        )
        .unwrap();
        let e = load_stop_edit(&conn, out.edit_id).unwrap().expect("row");
        assert_eq!(
            (e.entity.as_str(), e.stop_id, e.action.as_str()),
            ("stop", 1, "update")
        );
        assert_eq!(
            (e.old_stop_version_id, e.new_stop_version_id),
            (Some(old), Some(out.new_version_id))
        );
        assert!(load_stop_edit(&conn, 999).unwrap().is_none());
    }

    #[test]
    fn apply_stop_undo_restores_predecessor_mints_nothing() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn); // fixture has no bearing row — seed one
        conn.execute(
            "INSERT INTO stop_bearing (stop_version_id, bearing_deg, source, computed_at)
             VALUES (10, 123.5, 'OSM', '2026-01-02T00:00:00Z')",
            [],
        )
        .unwrap();
        let out = apply_stop_edit(
            &conn,
            &StopEditOp {
                stop_id: 1,
                stop_version_id: old,
                stop_name: Some("N".into()),
                ..Default::default()
            },
            "h",
            "n",
        )
        .unwrap();
        let u = apply_stop_undo(
            &conn,
            &stop_edit_undo_op(out.edit_id, 1, old, out.new_version_id),
            "h",
            "rev",
        )
        .unwrap();
        assert_eq!(u.new_version_id, old);
        // predecessor reopened, minted row closed, count unchanged (no mint)
        let (to, name): (Option<String>, String) = conn
            .query_row(
                "SELECT valid_to, stop_name FROM stop_version WHERE stop_version_id = ?1",
                [old],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((to.is_none(), name.as_str()), (true, "Old"));
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM stop_version WHERE stop_id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 2, "undo mints nothing");
        // rsv repointed back
        let sv: i64 = conn
            .query_row(
                "SELECT stop_version_id FROM route_stop_version WHERE route_stop_version_id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sv, old);
        // name-only undo: bearing carried back, ZERO marks
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM stop_bearing WHERE stop_version_id = ?1",
                [old],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
        // undo edit_log row: action='undo', changes='{}', versions swapped
        let (act, changes, ub, ua): (String, String, Option<i64>, Option<i64>) = conn.query_row(
            "SELECT action, changes, version_before, version_after FROM edit_log WHERE edit_id = ?1",
            [u.edit_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap();
        assert_eq!((act.as_str(), changes.as_str()), ("undo", "{}"));
        assert_eq!((ub, ua), (Some(out.new_version_id), Some(old)));
    }

    #[test]
    fn apply_stop_undo_after_move_remarks_and_carries_bearing_back() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn); // fixture has no bearing row — seed one
        conn.execute(
            "INSERT INTO stop_bearing (stop_version_id, bearing_deg, source, computed_at)
             VALUES (10, 123.5, 'OSM', '2026-01-02T00:00:00Z')",
            [],
        )
        .unwrap();
        let out = apply_stop_edit(
            &conn,
            &StopEditOp {
                stop_id: 1,
                stop_version_id: old,
                lat: Some(24.001),
                ..Default::default()
            },
            "h",
            "move",
        )
        .unwrap();
        // simulate a refresh having consumed the marks AND recomputed a
        // bearing at the churned svi (overwrites the carried row)
        conn.execute("DELETE FROM stale_mark", []).unwrap();
        conn.execute(
            "UPDATE stop_bearing SET bearing_deg = 9.0, computed_at = '2026-01-03T00:00:00Z'
             WHERE stop_version_id = ?1",
            [out.new_version_id],
        )
        .unwrap();

        let u = apply_stop_undo(
            &conn,
            &stop_edit_undo_op(out.edit_id, 1, old, out.new_version_id),
            "h",
            "rev",
        )
        .unwrap();
        // refreshed row moved back to the reopened predecessor
        let (deg, at): (f64, String) = conn
            .query_row(
                "SELECT bearing_deg, computed_at FROM stop_bearing WHERE stop_version_id = ?1",
                [old],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((deg, at.as_str()), (9.0, "2026-01-03T00:00:00Z"));
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM stop_bearing", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        // marks re-derived from changes fields: stop_bearing + projection(route 1) + station_bearing
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 3);
        let causes: Vec<i64> = conn
            .prepare("SELECT DISTINCT cause_edit_id FROM stale_mark")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(causes, vec![u.edit_id], "marks come from the undo itself");
        assert_eq!(u.stale_routes, vec![1]);
    }

    #[test]
    fn apply_stop_undo_after_regroup_marks_station_only() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn); // fixture has no bearing row — seed one
        conn.execute(
            "INSERT INTO stop_bearing (stop_version_id, bearing_deg, source, computed_at)
             VALUES (10, 123.5, 'OSM', '2026-01-02T00:00:00Z')",
            [],
        )
        .unwrap();
        second_station(&conn);
        let out = apply_stop_regroup(&conn, &regroup_op(old, 2), "h", "n").unwrap();
        conn.execute("DELETE FROM stale_mark", []).unwrap();
        let u = apply_stop_undo(
            &conn,
            &stop_edit_undo_op(out.edit_id, 1, old, out.new_version_id),
            "h",
            "rev",
        )
        .unwrap();
        // station membership restored
        let st: i64 = conn
            .query_row(
                "SELECT station_id FROM stop_version WHERE stop_version_id = ?1",
                [old],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(st, 1);
        // bearing carried back
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM stop_bearing WHERE stop_version_id = ?1",
                [old],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
        // station_bearing ONLY
        let rows: Vec<(String, String)> = conn
            .prepare("SELECT table_name, key FROM stale_mark")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(rows, vec![("station_bearing".into(), "*".into())]);
        assert!(u.stale_routes.is_empty());
    }

    #[test]
    fn apply_stop_undo_guards_fail_zero_partial() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        let a = apply_stop_edit(
            &conn,
            &StopEditOp {
                stop_id: 1,
                stop_version_id: old,
                stop_name: Some("A".into()),
                ..Default::default()
            },
            "h",
            "n",
        )
        .unwrap();
        // undoing an unknown edit id
        let err = apply_stop_undo(
            &conn,
            &stop_edit_undo_op(999, 1, old, a.new_version_id),
            "h",
            "n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("cannot be undone"), "{err}");
        // stale cur_svi: edit again, then try to undo `a`
        let b = apply_stop_edit(
            &conn,
            &StopEditOp {
                stop_id: 1,
                stop_version_id: a.new_version_id,
                stop_name: Some("B".into()),
                ..Default::default()
            },
            "h",
            "n",
        )
        .unwrap();
        let err = apply_stop_undo(
            &conn,
            &stop_edit_undo_op(a.edit_id, 1, old, a.new_version_id),
            "h",
            "n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("cannot be undone"), "{err}");
        // bogus old_svi: the pre-write existence (COUNT) guard rejects it
        // before any write; the reopen rows-affected check is
        // defense-in-depth behind it
        let err = apply_stop_undo(
            &conn,
            &stop_edit_undo_op(b.edit_id, 1, 999, b.new_version_id),
            "h",
            "n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("cannot be undone"), "{err}");
        // zero partial state: no undo rows, versions as after edit b
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM edit_log WHERE action = 'undo'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
        let cur: i64 = conn
            .query_row(
                "SELECT stop_version_id FROM stop_version WHERE stop_id = 1 AND valid_to IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cur, b.new_version_id);
    }

    fn move_op(old: i64) -> StationMoveOp {
        StationMoveOp {
            station_id: 1,
            old_station_version_id: old,
            new_latitude: 24.001,
            new_longitude: 120.001,
        }
    }

    #[test]
    fn apply_station_move_appends_version_zero_marks() {
        let conn = open_in_memory().unwrap();
        let old = rename_fixture(&conn);
        let (sv_before, sb_before, rsv_before): (i64, i64, i64) = (
            conn.query_row("SELECT COUNT(*) FROM stop_version", [], |r| r.get(0))
                .unwrap(),
            conn.query_row("SELECT COUNT(*) FROM stop_bearing", [], |r| r.get(0))
                .unwrap(),
            conn.query_row("SELECT COUNT(*) FROM route_stop_version", [], |r| r.get(0))
                .unwrap(),
        );
        let out = apply_station_move(&conn, &move_op(old), "herry", "fix").unwrap();
        assert_eq!(out.new_station_version_id, 21);
        // old closed, new current + manual, coords applied, name/group carried
        let (to, _): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT valid_to, source FROM station_version WHERE station_version_id = ?1",
                [old],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(to.is_some());
        let (lat, lon, name, group, src): (f64, f64, String, i64, String) = conn
            .query_row(
                "SELECT latitude, longitude, station_name, station_group_id, source
             FROM station_version WHERE station_version_id = ?1",
                [out.new_station_version_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(
            (lat, lon, name.as_str(), group, src.as_str()),
            (24.001, 120.001, "S", 1, "manual")
        );
        // zero collateral: poles, bearings, rsv untouched; zero stale marks
        let sv_after: i64 = conn
            .query_row("SELECT COUNT(*) FROM stop_version", [], |r| r.get(0))
            .unwrap();
        let sb_after: i64 = conn
            .query_row("SELECT COUNT(*) FROM stop_bearing", [], |r| r.get(0))
            .unwrap();
        let rsv_after: i64 = conn
            .query_row("SELECT COUNT(*) FROM route_stop_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            (sv_after, sb_after, rsv_after),
            (sv_before, sb_before, rsv_before)
        );
        let marks: i64 = conn
            .query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0))
            .unwrap();
        assert_eq!(marks, 0);
        // edit_log: {kind + field:{old,new}} shape
        let (ent, act, changes, vb, va): (String, String, String, i64, i64) = conn.query_row(
            "SELECT entity, action, changes, version_before, version_after FROM edit_log WHERE edit_id = ?1",
            [out.edit_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).unwrap();
        assert_eq!(
            (ent.as_str(), act.as_str(), vb, va),
            ("station", "update", old, out.new_station_version_id)
        );
        let c: serde_json::Value = serde_json::from_str(&changes).unwrap();
        assert_eq!(c["kind"], "station_move");
        assert_eq!(c["latitude"]["old"], 24.0);
        assert_eq!(c["latitude"]["new"], 24.001);
        assert_eq!(c["longitude"]["old"], 120.0);
        assert_eq!(c["longitude"]["new"], 120.001);
    }

    #[test]
    fn apply_station_move_refuses_stale_zero_partial() {
        let conn = open_in_memory().unwrap();
        let old = rename_fixture(&conn);
        let first = apply_station_move(&conn, &move_op(old), "herry", "a").unwrap();
        let err = apply_station_move(&conn, &move_op(old), "herry", "b").unwrap_err();
        assert!(err.to_string().contains("station version"), "{err}");
        // zero partial: still exactly the two versions and one log row
        let versions: i64 = conn
            .query_row("SELECT COUNT(*) FROM station_version", [], |r| r.get(0))
            .unwrap();
        let current: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM station_version WHERE valid_to IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let logs: i64 = conn
            .query_row("SELECT COUNT(*) FROM edit_log", [], |r| r.get(0))
            .unwrap();
        assert_eq!((versions, current, logs), (2, 1, 1));
        let _ = first;
    }

    fn move_undo_op(edit_id: i64, old: i64, new: i64) -> StationUndoOp {
        StationUndoOp::Move(StationMoveUndoOp {
            edit_id,
            station_id: 1,
            old_station_version_id: old,
            new_station_version_id: new,
        })
    }

    #[test]
    fn apply_station_undo_move_restores_predecessor() {
        let conn = open_in_memory().unwrap();
        let old = rename_fixture(&conn);
        let m = apply_station_move(&conn, &move_op(old), "herry", "move").unwrap();
        let u = apply_station_undo(
            &conn,
            &move_undo_op(m.edit_id, old, m.new_station_version_id),
            "h",
            "rev",
        )
        .unwrap();
        assert_eq!(u.restored_station_version_id, old);
        // predecessor reopened with its ORIGINAL coords (reopen, not rewrite)
        let (to, lat, lon): (Option<String>, f64, f64) = conn.query_row(
            "SELECT valid_to, latitude, longitude FROM station_version WHERE station_version_id = ?1",
            [old], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!((to.is_none(), lat, lon), (true, 24.0, 120.0));
        // minted row closed; undo mints nothing
        let (to, _): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT valid_to, source FROM station_version WHERE station_version_id = ?1",
                [m.new_station_version_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(to.is_some());
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM station_version", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM station_version WHERE valid_to IS NULL",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        // no collateral
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM stop_version", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM stop_bearing", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        // undo log row: action='undo', changes='{}', undo_of set, versions swapped
        let (act, changes, undo_of, vb, va): (String, String, i64, i64, i64) = conn
            .query_row(
                "SELECT action, changes, undo_of, version_before, version_after FROM edit_log
             WHERE edit_id = ?1",
                [u.edit_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(
            (act.as_str(), changes.as_str(), undo_of, vb, va),
            ("undo", "{}", m.edit_id, m.new_station_version_id, old)
        );
    }

    #[test]
    fn apply_station_undo_move_refuses_after_second_move() {
        let conn = open_in_memory().unwrap();
        let old = rename_fixture(&conn);
        let first = apply_station_move(&conn, &move_op(old), "herry", "a").unwrap();
        let _second =
            apply_station_move(&conn, &move_op(first.new_station_version_id), "herry", "b")
                .unwrap();
        let err = apply_station_undo(
            &conn,
            &move_undo_op(first.edit_id, old, first.new_station_version_id),
            "h",
            "rev",
        )
        .unwrap_err();
        assert!(err.to_string().contains("station version"), "{err}");
        // zero partial: three versions, two move rows, no undo row
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM station_version", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            3
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM edit_log WHERE action = 'undo'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }

    #[test]
    fn apply_station_undo_move_bogus_ids_zero_partial() {
        let conn = open_in_memory().unwrap();
        let old = rename_fixture(&conn);
        let m = apply_station_move(&conn, &move_op(old), "herry", "move").unwrap();
        // unknown predecessor: the reopen UPDATE hits 0 rows
        let err = apply_station_undo(
            &conn,
            &move_undo_op(m.edit_id, 999, m.new_station_version_id),
            "h",
            "rev",
        )
        .unwrap_err();
        assert!(err.to_string().contains("cannot be undone"), "{err}");
        // unknown minted version: the currency COUNT guard rejects
        let err =
            apply_station_undo(&conn, &move_undo_op(m.edit_id, old, 999), "h", "rev").unwrap_err();
        assert!(err.to_string().contains("station version"), "{err}");
        // zero partial after both
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM station_version", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM edit_log WHERE action = 'undo'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }

    #[test]
    fn edit_entity_returns_entity_or_none() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        let out = apply_stop_edit(
            &conn,
            &StopEditOp {
                stop_id: 1,
                stop_version_id: old,
                stop_name: Some("N".into()),
                ..Default::default()
            },
            "h",
            "n",
        )
        .unwrap();
        assert_eq!(
            edit_entity(&conn, out.edit_id).unwrap().as_deref(),
            Some("stop")
        );
        assert!(edit_entity(&conn, 999).unwrap().is_none());
    }

    #[test]
    fn load_polyline_edit_filters_entity_and_returns_ts() {
        let conn = open_in_memory().unwrap();
        // single fixture call (PK 1 rows are insert-once); `old` is the
        // stop_version_id reused by the stop-edit leg below.
        let old = fixture(&conn);
        src_fixture(&conn);
        let first = apply_line_edit(&conn, &line_op(), "herry", "a").unwrap();
        let row = load_polyline_edit(&conn, first.edit_id).unwrap().unwrap();
        assert_eq!((row.entity_id, row.action.as_str()), (1, "create"));
        assert_eq!(row.ts, first.updated_at);
        assert_eq!(row.changes["kind"], "polyline_override");
        // stop-entity row → None (kind guard discipline: the caller refuses).
        // Same construction as the existing `edit_entity_returns_entity_or_none`
        // test (fixture returns the stop_version_id).
        let stop_edit = apply_stop_edit(
            &conn,
            &StopEditOp {
                stop_id: 1,
                stop_version_id: old,
                stop_name: Some("N".into()),
                ..Default::default()
            },
            "h",
            "n",
        )
        .unwrap();
        assert!(load_polyline_edit(&conn, stop_edit.edit_id)
            .unwrap()
            .is_none());
    }

    #[test]
    fn undo_edit_dispatches_stop_and_polyline() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        src_fixture(&conn);

        let e = edit_stop(
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
        match undo_edit(&conn, e.edit_id, "h", "rev").unwrap() {
            UndoOutcome::Stop {
                restored_version_id,
                ..
            } => assert_eq!(restored_version_id, old),
            other => panic!("stop edit must dispatch to the stop arm, got {other:?}"),
        }

        // create → undo deletes the override (deleted = true)
        let l = apply_line_edit(&conn, &line_op(), "h", "n1").unwrap();
        match undo_edit(&conn, l.edit_id, "h", "rev").unwrap() {
            UndoOutcome::Polyline { deleted, .. } => assert!(deleted),
            other => panic!("polyline edit must dispatch to the polyline arm, got {other:?}"),
        }
    }

    #[test]
    fn undo_edit_dispatches_station() {
        let conn = open_in_memory().unwrap();
        rename_fixture(&conn);
        let renamed = apply_station_rename(&conn, &all_poles_op(), "herry", "n").unwrap();
        match undo_edit(&conn, renamed.edit_id, "herry", "revert").unwrap() {
            UndoOutcome::Station {
                restored_version_id,
                ..
            } => assert_eq!(restored_version_id, 20), // the pre-rename version
            other => panic!("station edit must dispatch to the station arm, got {other:?}"),
        }
    }

    #[test]
    fn undo_edit_unknown_id_surfaces_not_undoable() {
        let conn = open_in_memory().unwrap();
        fixture(&conn);
        let err = undo_edit(&conn, 999, "h", "").unwrap_err();
        assert!(err.to_string().contains("cannot be undone"), "{err}");
    }
}
