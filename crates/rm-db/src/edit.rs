//! Manual edit primitive: version-append edits + audit + staleness marks.

use anyhow::{ensure, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Default)]
pub struct StopEdit {
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub stop_name: Option<String>,
}

#[derive(Debug, Error)]
pub enum EditError {
    #[error("stop version {0} is not the current version (or does not exist)")]
    NotCurrent(i64),
    #[error("station version {0} is not the current version (or does not exist)")]
    StationNotCurrent(i64),
    #[error("edit {0} cannot be undone (not current, or is itself an undo)")]
    NotUndoable(i64),
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

pub fn edit_stop(
    conn: &Connection,
    stop_version_id: i64,
    edit: &StopEdit,
    editor: &str,
    note: &str,
) -> Result<EditOutcome> {
    ensure!(
        edit.lat.is_some() || edit.lon.is_some() || edit.stop_name.is_some(),
        "empty edit"
    );
    let tx = conn.unchecked_transaction()?;
    let old: (i64, i64, String, String, i64, f64, f64) = match tx.query_row(
        "SELECT stop_id, station_id, stop_code, stop_name, direction, latitude, longitude
         FROM stop_version WHERE stop_version_id = ?1 AND valid_to IS NULL",
        [stop_version_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
    ) {
        Ok(v) => v,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Err(EditError::NotCurrent(stop_version_id).into())
        }
        Err(e) => return Err(e.into()),
    };
    let (stop_id, station_id, stop_code, old_name, direction, old_lat, old_lon) = old;
    let now = crate::now_rfc3339();
    let new_id = crate::mint(&tx, "stop_version")?;
    tx.execute(
        "UPDATE stop_version SET valid_to = ?1 WHERE stop_version_id = ?2",
        params![now, stop_version_id],
    )?;
    tx.execute(
        "INSERT INTO stop_version
            (stop_version_id, stop_id, station_id, stop_code, stop_name, direction,
             latitude, longitude, valid_from, created_at, source)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, 'manual')",
        params![
            new_id,
            stop_id,
            station_id,
            stop_code,
            edit.stop_name.clone().unwrap_or(old_name.clone()),
            direction,
            edit.lat.unwrap_or(old_lat),
            edit.lon.unwrap_or(old_lon),
            now
        ],
    )?;
    tx.execute(
        "UPDATE route_stop_version SET stop_version_id = ?1 WHERE stop_version_id = ?2",
        params![new_id, stop_version_id],
    )?;
    tx.execute(
        "DELETE FROM stop_bearing WHERE stop_version_id = ?1",
        params![stop_version_id],
    )?;

    let mut changes = serde_json::Map::new();
    if let Some(v) = &edit.stop_name {
        changes.insert("stop_name".into(), json!({"old": old_name, "new": v}));
    }
    if let Some(v) = edit.lat {
        changes.insert("latitude".into(), json!({"old": old_lat, "new": v}));
    }
    if let Some(v) = edit.lon {
        changes.insert("longitude".into(), json!({"old": old_lon, "new": v}));
    }

    tx.execute(
        "INSERT INTO edit_log (ts, editor, entity, entity_id, action, changes, note, version_before, version_after)
         VALUES (?1, ?2, 'stop', ?3, 'update', ?4, ?5, ?6, ?7)",
        params![
            now,
            editor,
            stop_id,
            serde_json::Value::Object(changes).to_string(),
            if note.is_empty() { None } else { Some(note) },
            stop_version_id,
            new_id
        ],
    )?;
    let edit_id = tx.last_insert_rowid();

    let mut stale_routes = Vec::new();
    // stop_bearing rows are deleted unconditionally above → always marked;
    // position-dependent data only when the position moved
    mark_bearings_stale(&tx, edit_id, &now)?;
    if edit.lat.is_some() || edit.lon.is_some() {
        stale_routes = mark_position_stale(&tx, new_id, edit_id, &now)?;
    }
    tx.commit()?;
    Ok(EditOutcome {
        edit_id,
        new_version_id: new_id,
        stale_routes,
    })
}

/// The edited/restored version's `stop_bearing` rows were deleted (every
/// edit_stop/undo does), so the whole table's derived data is stale until
/// the bearing refresh re-runs. Idempotent via INSERT OR IGNORE.
fn mark_bearings_stale(tx: &Connection, edit_id: i64, now: &str) -> Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO stale_mark (table_name, key, cause_edit_id, marked_at)
         VALUES ('stop_bearing', '*', ?1, ?2)",
        params![edit_id, now],
    )?;
    Ok(())
}

/// Mark the derived data that depends on a stop's position stale: one
/// `path_stop_projection` row per route serving `stop_version_id` (whose
/// `route_stop_version` rows must already point at it) plus the whole
/// `station_bearing` table (`stop_bearing` is marked unconditionally —
/// see `mark_bearings_stale`). Idempotent via INSERT OR IGNORE
/// on the `(table_name, key)` primary key.
fn mark_position_stale(
    tx: &Connection,
    stop_version_id: i64,
    edit_id: i64,
    now: &str,
) -> Result<Vec<i64>> {
    let mut stale_routes = Vec::new();
    {
        let mut stmt = tx.prepare(
            "SELECT DISTINCT rv.route_id
             FROM route_stop_version rsv
             JOIN subroute_version sv ON sv.subroute_version_id = rsv.subroute_version_id
             JOIN route_version rv ON rv.route_version_id = sv.route_version_id
             WHERE rsv.stop_version_id = ?1",
        )?;
        let rows = stmt.query_map([stop_version_id], |r| r.get::<_, i64>(0))?;
        for rid in rows {
            stale_routes.push(rid?);
        }
    }
    for rid in &stale_routes {
        tx.execute(
            "INSERT OR IGNORE INTO stale_mark (table_name, key, cause_edit_id, marked_at)
             VALUES ('path_stop_projection', ?1, ?2, ?3)",
            params![rid.to_string(), edit_id, now],
        )?;
    }
    tx.execute(
        "INSERT OR IGNORE INTO stale_mark (table_name, key, cause_edit_id, marked_at)
         VALUES ('station_bearing', '*', ?1, ?2)",
        params![edit_id, now],
    )?;
    Ok(stale_routes)
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

/// Whole-tx stop edit (move and/or rename). Position present ⇒ the pole's
/// `stop_bearing` row is DELETED (value = f(position) — invalid now) and
/// the derived data is marked stale. Name-only ⇒ the bearing row is
/// carried over and NOTHING goes stale. One edit_log row,
/// `changes = {field: {old, new}}`.
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
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
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
    tx.execute(
        "UPDATE stop_version SET valid_to = ?1 WHERE stop_version_id = ?2",
        params![now, op.stop_version_id],
    )?;
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
    if moves {
        tx.execute(
            "DELETE FROM stop_bearing WHERE stop_version_id = ?1",
            [op.stop_version_id],
        )?;
    } else {
        carry_bearing(&tx, op.stop_version_id, new_id)?;
    }

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
        mark_bearings_stale(&tx, edit_id, &now)?;
        stale_routes = mark_position_stale(&tx, new_id, edit_id, &now)?;
    }
    tx.commit()?;
    Ok(EditOutcome {
        edit_id,
        new_version_id: new_id,
        stale_routes,
    })
}

pub fn undo(conn: &Connection, edit_id: i64, editor: &str, note: &str) -> Result<EditOutcome> {
    let tx = conn.unchecked_transaction()?;
    let row: (String, i64, String, Option<i64>, Option<i64>, String) = match tx.query_row(
        "SELECT entity, entity_id, action, version_before, version_after, changes
         FROM edit_log WHERE edit_id = ?1",
        [edit_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
    ) {
        Ok(v) => v,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Err(EditError::NotUndoable(edit_id).into())
        }
        Err(e) => return Err(e.into()),
    };
    let (entity, stable_id, action, version_before, version_after, changes) = row;
    if entity != "stop" || action == "undo" {
        return Err(EditError::NotUndoable(edit_id).into());
    }
    let (Some(before), Some(after)) = (version_before, version_after) else {
        return Err(EditError::NotUndoable(edit_id).into());
    };
    // `after` must still be the current version for the undo to apply
    let valid_to: Option<String> = match tx
        .query_row(
            "SELECT valid_to FROM stop_version WHERE stop_version_id = ?1",
            [after],
            |r| r.get(0),
        )
        .optional()?
    {
        Some(v) => v,
        None => return Err(EditError::NotUndoable(edit_id).into()),
    };
    if valid_to.is_some() {
        return Err(EditError::NotUndoable(edit_id).into());
    }
    // a position change was undone: the derived data recomputed at the
    // edited position is stale again (spec §Staleness — any position change)
    let changes_json: serde_json::Value = serde_json::from_str(&changes)?;
    let position_changed = changes_json.get("latitude").is_some()
        || changes_json.get("longitude").is_some();

    let now = crate::now_rfc3339();
    tx.execute(
        "UPDATE stop_version SET valid_to = ?1 WHERE stop_version_id = ?2",
        params![now, after],
    )?;
    tx.execute(
        "UPDATE stop_version SET valid_to = NULL WHERE stop_version_id = ?1",
        params![before],
    )?;
    tx.execute(
        "UPDATE route_stop_version SET stop_version_id = ?1 WHERE stop_version_id = ?2",
        params![before, after],
    )?;
    tx.execute(
        "DELETE FROM stop_bearing WHERE stop_version_id = ?1",
        params![after],
    )?;
    tx.execute(
        "INSERT INTO edit_log (ts, editor, entity, entity_id, action, changes, note, undo_of, version_before, version_after)
         VALUES (?1, ?2, 'stop', ?3, 'undo', '{}', ?4, ?5, ?6, ?7)",
        params![
            now,
            editor,
            stable_id,
            if note.is_empty() { None } else { Some(note) },
            edit_id,
            after,
            before
        ],
    )?;
    let undo_edit_id = tx.last_insert_rowid();

    let mut stale_routes = Vec::new();
    // the undo deleted the restored-from edit's bearing rows → always mark;
    // position-dependent data only when the undone edit moved the stop
    mark_bearings_stale(&tx, undo_edit_id, &now)?;
    if position_changed {
        // route_stop_version already points at `before` (repointed above)
        stale_routes = mark_position_stale(&tx, before, undo_edit_id, &now)?;
    }
    tx.commit()?;
    Ok(EditOutcome {
        edit_id: undo_edit_id,
        new_version_id: before,
        stale_routes,
    })
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

/// Read-side snapshot for the station-rename use case (rm-edit::rename_station):
/// the current station version plus its poles' current stop versions.
#[derive(Debug)]
pub struct StationSnapshot {
    pub station_id: i64,
    pub station_version_id: i64,
    pub station_name: String,
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
    let row: (i64, String) = match conn
        .query_row(
            "SELECT station_id, station_name FROM station_version
             WHERE station_version_id = ?1 AND valid_to IS NULL",
            [station_version_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
    {
        Some(v) => v,
        None => return Ok(None),
    };
    let (station_id, station_name) = row;
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

pub fn load_stop_snapshot(
    conn: &Connection,
    stop_version_id: i64,
) -> Result<Option<StopSnapshot>> {
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

/// Read-side DTO for the logged station rename (consumed by
/// rm-edit::plan_rename_undo). edit_log is append-only, so a plan parsed
/// from this row is immutable; apply re-validates currency in its tx.
#[derive(Debug)]
pub struct StationRenameEdit {
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
pub fn load_station_rename_edit(
    conn: &Connection,
    edit_id: i64,
) -> Result<Option<StationRenameEdit>> {
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
    Ok(Some(StationRenameEdit {
        edit_id,
        station_id,
        action,
        old_station_version_id: vb,
        new_station_version_id: va,
        changes: serde_json::from_str(&changes)?,
    }))
}

/// Write-side op for the station rename (rm-edit maps a StationRenamePlan
/// into this; rm-db knows nothing about the domain types).
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

/// Write-side op for the station-rename undo (rm-edit maps a
/// RenameUndoOp into this; the rows come from the append-only log).
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

#[derive(Debug)]
pub struct StationRenameUndone {
    pub edit_id: i64,
    pub restored_station_version_id: i64,
}

/// Undo a station rename in ONE transaction — the exact inverse of
/// `apply_station_rename`: close/reopen the station version, per pole
/// close/reopen the stop versions, repoint route_stop_version, move the
/// stop_bearing row back (PK invariant). One edit_log undo row, ZERO
/// stale marks (nothing derived changed). Mints nothing.
///
/// Guards (inside the tx): the rename's station version must still be
/// current (`StationNotCurrent` otherwise); every minted pole version
/// must still be current (`NotUndoable` otherwise — a post-rename
/// edit_stop on any pole makes the rename non-undoable).
pub fn apply_station_rename_undo(
    conn: &Connection,
    op: &StationRenameUndoOp,
    editor: &str,
    note: &str,
) -> Result<StationRenameUndone> {
    let tx = conn.unchecked_transaction()?;
    let now = crate::now_rfc3339();

    let n: i64 = tx.query_row(
        "SELECT COUNT(*) FROM station_version WHERE station_version_id = ?1 AND valid_to IS NULL",
        [op.new_station_version_id],
        |r| r.get(0),
    )?;
    if n == 0 {
        return Err(EditError::StationNotCurrent(op.new_station_version_id).into());
    }
    for pole in &op.poles {
        let n: i64 = tx.query_row(
            "SELECT COUNT(*) FROM stop_version WHERE stop_version_id = ?1 AND valid_to IS NULL",
            [pole.new_svi],
            |r| r.get(0),
        )?;
        if n == 0 {
            return Err(EditError::NotUndoable(op.edit_id).into());
        }
    }

    tx.execute(
        "UPDATE station_version SET valid_to = ?1 WHERE station_version_id = ?2",
        params![now, op.new_station_version_id],
    )?;
    tx.execute(
        "UPDATE station_version SET valid_to = NULL WHERE station_version_id = ?1",
        params![op.old_station_version_id],
    )?;

    for pole in &op.poles {
        tx.execute(
            "UPDATE stop_version SET valid_to = ?1 WHERE stop_version_id = ?2",
            params![now, pole.new_svi],
        )?;
        tx.execute(
            "UPDATE stop_version SET valid_to = NULL WHERE stop_version_id = ?1",
            params![pole.old_svi],
        )?;
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
    Ok(StationRenameUndone {
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
            "INSERT INTO stop (stop_id, created_at) VALUES (1, '2026-01-01T00:00:00Z')", []).unwrap();
        conn.execute(
            "INSERT INTO stop_version (stop_version_id, stop_id, station_id, stop_code, stop_name, direction, latitude, longitude, valid_from, created_at)
             VALUES (10, 1, 1, '001', 'Old', 0, 24.0, 120.0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')", []).unwrap();
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
        conn.execute("INSERT INTO id_sequence (table_name, next_value) VALUES ('stop_version', 11)", []).unwrap();
        10
    }

    #[test]
    fn edit_stop_appends_version_and_repoints_links() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        let out = edit_stop(&conn, old, &StopEdit { lat: Some(24.001), lon: None, stop_name: Some("New".into()) }, "herry", "fix").unwrap();
        // old closed, new current + manual
        let (to, _src): (Option<String>, Option<String>) = conn.query_row(
            "SELECT valid_to, source FROM stop_version WHERE stop_version_id = ?1", [old], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert!(to.is_some());
        let (name, lat, src2): (String, f64, String) = conn.query_row(
            "SELECT stop_name, latitude, source FROM stop_version WHERE stop_version_id = ?1", [out.new_version_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!((name.as_str(), lat, src2.as_str()), ("New", 24.001, "manual"));
        // FK repointed
        let sv: i64 = conn.query_row("SELECT stop_version_id FROM route_stop_version WHERE route_stop_version_id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(sv, out.new_version_id);
        // edit_log row
        let (ent, eid, act): (String, i64, String) = conn.query_row(
            "SELECT entity, entity_id, action FROM edit_log WHERE edit_id = ?1", [out.edit_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!((ent.as_str(), eid, act.as_str()), ("stop", 1, "update"));
        // stale marks: position changed → projection route + both bearing tables
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 3);
    }

    #[test]
    fn name_only_edit_marks_only_bearings() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        let out = edit_stop(&conn, old, &StopEdit { lat: None, lon: None, stop_name: Some("N2".into()) }, "herry", "rename").unwrap();
        // the rename deleted the stop's bearing rows → only that mark; no
        // route projections or station bearing
        let (tbl, key): (String, String) = conn.query_row(
            "SELECT table_name, key FROM stale_mark", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((tbl.as_str(), key.as_str()), ("stop_bearing", "*"));
        assert!(out.stale_routes.is_empty());
    }

    #[test]
    fn edit_stop_rejects_non_current_version() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        edit_stop(&conn, old, &StopEdit { stop_name: Some("a".into()), ..Default::default() }, "herry", "n").unwrap();
        let err = edit_stop(&conn, old, &StopEdit { stop_name: Some("b".into()), ..Default::default() }, "herry", "n").unwrap_err();
        assert!(err.to_string().contains("not the current version"), "{err}");
    }

    #[test]
    fn second_edit_of_same_version_rejected() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        let a = edit_stop(&conn, old, &StopEdit { lat: Some(24.001), lon: None, stop_name: None }, "herry", "first").unwrap();
        // duplicate submit of the SAME version id: a.new_version_id is current now
        let b = edit_stop(&conn, old, &StopEdit { lat: Some(24.002), lon: None, stop_name: None }, "herry", "second");
        assert!(b.is_err(), "second edit of same version must be rejected");
        // and no partial state: exactly one manual version exists
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM stop_version WHERE source = 'manual'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        // the current version is a's
        let cur: i64 = conn.query_row(
            "SELECT stop_version_id FROM stop_version WHERE valid_to IS NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(cur, a.new_version_id);
    }

    #[test]
    fn undo_restores_predecessor() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        let out = edit_stop(&conn, old, &StopEdit { stop_name: Some("New".into()), ..Default::default() }, "herry", "fix").unwrap();
        let u = undo(&conn, out.edit_id, "herry", "revert").unwrap();
        assert_eq!(u.new_version_id, old);
        let (to, name): (Option<String>, String) = conn.query_row(
            "SELECT valid_to, stop_name FROM stop_version WHERE stop_version_id = ?1", [old], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert!(to.is_none());
        assert_eq!(name, "Old");
        let sv: i64 = conn.query_row("SELECT stop_version_id FROM route_stop_version WHERE route_stop_version_id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(sv, old);
    }

    #[test]
    fn undo_validates_current_and_rejects_undo_of_undo() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        let a = edit_stop(&conn, old, &StopEdit { stop_name: Some("A".into()), ..Default::default() }, "h", "n").unwrap();
        // edit again → a.version_after no longer current
        let b = edit_stop(&conn, a.new_version_id, &StopEdit { stop_name: Some("B".into()), ..Default::default() }, "h", "n").unwrap();
        assert!(undo(&conn, a.edit_id, "h", "n").is_err());
        // fresh chain: edit then undo, then undo the undo → rejected
        // (the brief targeted `old` here, but old was closed by edit a — the
        // current version is b.new_version_id)
        let c = edit_stop(&conn, b.new_version_id, &StopEdit { stop_name: Some("C".into()), ..Default::default() }, "h", "n").unwrap();
        let u = undo(&conn, c.edit_id, "h", "n").unwrap();
        assert!(undo(&conn, u.edit_id, "h", "n").is_err());
    }

    #[test]
    fn undo_of_position_edit_remarks_stale() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        let out = edit_stop(&conn, old, &StopEdit { lat: Some(24.001), lon: None, stop_name: None }, "herry", "move").unwrap();
        // simulate a refresh having consumed the edit's marks: undo must
        // re-mark the derived data for the restored position
        conn.execute("DELETE FROM stale_mark", []).unwrap();
        let u = undo(&conn, out.edit_id, "herry", "revert").unwrap();
        // route 1 (serving the repointed version) + both bearing tables
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 3);
        let causes: Vec<i64> = conn.prepare("SELECT DISTINCT cause_edit_id FROM stale_mark").unwrap()
            .query_map([], |r| r.get(0)).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(causes, vec![u.edit_id], "marks must come from the undo itself");
        assert_eq!(u.stale_routes, vec![1]);
    }

    #[test]
    fn undo_of_name_only_edit_marks_only_bearings() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        let out = edit_stop(&conn, old, &StopEdit { stop_name: Some("N".into()), ..Default::default() }, "herry", "rename").unwrap();
        // simulate a refresh having consumed the edit's mark: the undo
        // deletes stop_bearing rows too, so it must re-mark them itself
        conn.execute("DELETE FROM stale_mark", []).unwrap();
        let u = undo(&conn, out.edit_id, "herry", "revert").unwrap();
        let (tbl, key): (String, String) = conn.query_row(
            "SELECT table_name, key FROM stale_mark", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((tbl.as_str(), key.as_str()), ("stop_bearing", "*"));
        assert!(u.stale_routes.is_empty());
    }

    #[test]
    fn entity_history_lists_newest_first() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        let a = edit_stop(&conn, old, &StopEdit { stop_name: Some("A".into()), ..Default::default() }, "h", "n1").unwrap();
        undo(&conn, a.edit_id, "h", "n2").unwrap();
        let rows = entity_history(&conn, "stop", 1).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].action, "undo");
        assert_eq!(rows[1].action, "update");
        assert_eq!(rows[0].undo_of, Some(a.edit_id));
    }

    #[test]
    fn load_station_snapshot_returns_station_and_current_poles() {
        let conn = open_in_memory().unwrap();
        let _ = fixture(&conn);
        let snap = load_station_snapshot(&conn, 1).unwrap().expect("current station version");
        assert_eq!(snap.station_id, 1);
        assert_eq!(snap.station_version_id, 1);
        assert_eq!(snap.station_name, "S");
        assert_eq!(snap.poles.len(), 1);
        assert_eq!((snap.poles[0].stop_id, snap.poles[0].stop_version_id, snap.poles[0].stop_name.as_str()), (1, 10, "Old"));
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
        assert_eq!(
            (s.stop_id, s.station_id, s.stop_version_id),
            (1, 1, old)
        );
        assert_eq!(s.stop_name, "Old");
        assert_eq!((s.latitude, s.longitude), (24.0, 120.0));
    }

    #[test]
    fn load_stop_snapshot_none_for_non_current_or_missing() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        edit_stop(&conn, old, &StopEdit { stop_name: Some("N".into()), ..Default::default() }, "h", "n").unwrap();
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
    fn load_station_rename_edit_returns_parsed_row() {
        let conn = open_in_memory().unwrap();
        rename_fixture(&conn);
        let out = apply_station_rename(&conn, &all_poles_op(), "herry", "n").unwrap();
        let e = load_station_rename_edit(&conn, out.edit_id).unwrap().expect("row exists");
        assert_eq!(e.edit_id, out.edit_id);
        assert_eq!(e.station_id, 1);
        assert_eq!(e.action, "update");
        assert_eq!(e.old_station_version_id, Some(20));
        assert_eq!(e.new_station_version_id, Some(out.new_station_version_id));
        assert_eq!(e.changes["kind"], "station_rename");
        assert_eq!(e.changes["stops"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn load_station_rename_edit_none_for_missing_id() {
        let conn = open_in_memory().unwrap();
        assert!(load_station_rename_edit(&conn, 999).unwrap().is_none());
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
                PoleOp { stop_id: 1, old_stop_version_id: 10, new_stop_name: "台北車站".into() },
                PoleOp { stop_id: 2, old_stop_version_id: 11, new_stop_name: "台北車站".into() },
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
                .map(|p| PoleUndoOp { stop_id: p.stop_id, old_svi: p.old_svi, new_svi: p.new_svi })
                .collect(),
        }
    }

    #[test]
    fn apply_renames_station_and_all_poles_one_tx() {
        let conn = open_in_memory().unwrap();
        let old = rename_fixture(&conn);
        let out = apply_station_rename(&conn, &all_poles_op(), "herry", "fix").unwrap();
        // station version appended: old closed, new current + manual + new name
        let (to, _): (Option<String>, Option<String>) = conn.query_row(
            "SELECT valid_to, source FROM station_version WHERE station_version_id = ?1", [old], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert!(to.is_some());
        let (name, src): (String, String) = conn.query_row(
            "SELECT station_name, source FROM station_version WHERE station_version_id = ?1",
            [out.new_station_version_id], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((name.as_str(), src.as_str()), ("台北車站", "manual"));
        // both poles churned to the new name, source manual
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM stop_version WHERE stop_name = '台北車站' AND source = 'manual' AND valid_to IS NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2);
        // rsv repointed from sv 10 to the churned pole
        let sv: i64 = conn.query_row(
            "SELECT stop_version_id FROM route_stop_version WHERE route_stop_version_id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(sv, out.poles[0].new_svi);
        assert_eq!(out.poles.len(), 2);
    }

    #[test]
    fn apply_leaves_zero_stale_marks() {
        let conn = open_in_memory().unwrap();
        let old = rename_fixture(&conn);
        apply_station_rename(&conn, &all_poles_op(), "herry", "fix").unwrap();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0)).unwrap();
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
        assert_eq!((deg, src.as_str(), at.as_str()), (123.5, "OSM", "2026-01-02T00:00:00Z"));
        // old svi row gone (PK moved), pole 2 still has none
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM stop_bearing", [], |r| r.get(0)).unwrap();
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
        assert_eq!((entity.as_str(), eid, action.as_str()), ("station", 1, "update"));
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
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM station_version WHERE source = 'manual'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM edit_log", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM stop_bearing", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        let _ = old;
    }

    #[test]
    fn apply_aborts_when_pole_not_current() {
        let conn = open_in_memory().unwrap();
        rename_fixture(&conn);
        // churn pole 10 behind the plan's back (name-only edit_stop)
        edit_stop(&conn, 10, &StopEdit { stop_name: Some("shadowed".into()), ..Default::default() }, "h", "n").unwrap();
        let err = apply_station_rename(&conn, &all_poles_op(), "herry", "stale pole").unwrap_err();
        assert!(err.to_string().contains("not the current version"), "{err}");
        // whole tx rolled back (the edit_stop's own manual stop_version and
        // stop_bearing mark stay — they are not this tx's): no manual
        // station version, no station edit_log row
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM station_version WHERE source = 'manual'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM edit_log WHERE entity = 'station'", [], |r| r.get(0)).unwrap();
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
        edit_stop(&conn, 11, &StopEdit { stop_name: Some("shadowed".into()), ..Default::default() }, "h", "n").unwrap();
        let err = apply_station_rename(&conn, &all_poles_op(), "herry", "stale last").unwrap_err();
        assert!(err.to_string().contains("not the current version"), "{err}");
        // pole 10's churn rolled back: original row current again, name 'S'
        let (to, name): (Option<String>, String) = conn.query_row(
            "SELECT valid_to, stop_name FROM stop_version WHERE stop_version_id = 10", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert!(to.is_none(), "pole 10 unclosed by rollback");
        assert_eq!(name, "S");
        // its bearing row came back to svi 10 (exactly one row, on svi 10)
        let (n, on10): (i64, i64) = conn.query_row(
            "SELECT COUNT(*), SUM(CASE WHEN stop_version_id = 10 THEN 1 ELSE 0 END) FROM stop_bearing", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((n, on10), (1, 1));
        // rsv still points at svi 10
        let sv: i64 = conn.query_row(
            "SELECT stop_version_id FROM route_stop_version WHERE route_stop_version_id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(sv, 10);
        // no trace of this tx: no manual station version, no station edit_log,
        // and the only manual stop_version is edit_stop's churn of pole 11
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM station_version WHERE source = 'manual'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM edit_log WHERE entity = 'station'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM stop_version WHERE source = 'manual'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1, "only edit_stop's churn of pole 11");
    }

    #[test]
    fn undo_moves_bearing_row_back_first() {
        let conn = open_in_memory().unwrap();
        let applied = seed_renamed(&conn);
        // PK invariant: the row MOVES back new_svi → old_svi, values intact
        apply_station_rename_undo(&conn, &undo_op(&applied), "herry", "revert").unwrap();
        let (deg, src, at): (f64, String, String) = conn.query_row(
            "SELECT bearing_deg, source, computed_at FROM stop_bearing WHERE stop_version_id = 10", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!((deg, src.as_str(), at.as_str()), (123.5, "OSM", "2026-01-02T00:00:00Z"));
        // exactly one row in the table; pole 11 still has none
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM stop_bearing", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn undo_restores_station_and_poles() {
        let conn = open_in_memory().unwrap();
        let applied = seed_renamed(&conn);
        let out = apply_station_rename_undo(&conn, &undo_op(&applied), "herry", "revert").unwrap();
        assert_eq!(out.restored_station_version_id, 20);
        // station: old row current with old name, minted row closed
        let (name, to): (String, Option<String>) = conn.query_row(
            "SELECT station_name, valid_to FROM station_version WHERE station_version_id = 20", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((name.as_str(), to), ("S", None));
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM station_version WHERE valid_to IS NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1, "only the restored row is current");
        // poles: old rows current with original names, minted rows closed,
        // nothing re-minted (still exactly 4 stop_version rows)
        let mut stmt = conn.prepare(
            "SELECT stop_name FROM stop_version WHERE valid_to IS NULL ORDER BY stop_version_id").unwrap();
        let names: Vec<String> = stmt.query_map([], |r| r.get(0)).unwrap()
            .collect::<Result<_, _>>().unwrap();
        assert_eq!(names, vec!["S".to_string(), "S 往".to_string()]);
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM stop_version", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 4, "undo mints nothing");
        // rsv repointed back to the original pole version
        let sv: i64 = conn.query_row(
            "SELECT stop_version_id FROM route_stop_version WHERE route_stop_version_id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(sv, 10);
    }

    #[test]
    fn undo_writes_edit_log_row_and_zero_stale_marks() {
        let conn = open_in_memory().unwrap();
        let applied = seed_renamed(&conn);
        let out = apply_station_rename_undo(&conn, &undo_op(&applied), "herry", "revert!").unwrap();
        let (entity, eid, action, changes, note, undo_of, vb, va): (String, i64, String, String, Option<String>, Option<i64>, Option<i64>, Option<i64>) = conn.query_row(
            "SELECT entity, entity_id, action, changes, note, undo_of, version_before, version_after FROM edit_log WHERE edit_id = ?1",
            [out.edit_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?))).unwrap();
        assert_eq!((entity.as_str(), eid, action.as_str()), ("station", 1, "undo"));
        assert_eq!(changes, "{}");
        assert_eq!(note.as_deref(), Some("revert!"));
        assert_eq!(undo_of, Some(applied.edit_id));
        assert_eq!((vb, va), (Some(applied.new_station_version_id), Some(20)));
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0, "undo of a rename invalidates no derived data");
    }

    #[test]
    fn undo_station_only_touches_no_poles() {
        let conn = open_in_memory().unwrap();
        rename_fixture(&conn);
        let applied = apply_station_rename(&conn, &station_only_op(), "herry", "n").unwrap();
        assert!(applied.poles.is_empty());
        apply_station_rename_undo(&conn, &undo_op(&applied), "herry", "revert").unwrap();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM stop_version WHERE valid_to IS NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2, "both original poles untouched and current");
        let name: String = conn.query_row(
            "SELECT station_name FROM station_version WHERE valid_to IS NULL", [], |r| r.get(0)).unwrap();
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
        let err = apply_station_rename_undo(&conn, &undo_op(&first), "herry", "revert").unwrap_err();
        assert!(err.to_string().contains("not the current version"), "{err}");
        // zero partial state: still exactly two manual station versions,
        // two station edit_log rows (two renames, no undo row)
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM station_version WHERE source = 'manual'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2);
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM edit_log WHERE entity = 'station'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn undo_refuses_when_pole_edited_after_rename() {
        let conn = open_in_memory().unwrap();
        let applied = seed_renamed(&conn);
        // post-rename pole edit: edit_stop churns pole 1's minted version
        edit_stop(&conn, applied.poles[0].new_svi,
            &StopEdit { stop_name: Some("shadowed".into()), ..Default::default() }, "h", "n").unwrap();
        let err = apply_station_rename_undo(&conn, &undo_op(&applied), "herry", "revert").unwrap_err();
        assert!(err.to_string().contains("cannot be undone"), "{err}");
        // zero partial state: minted station version still current, no undo row
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM station_version WHERE valid_to IS NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM edit_log WHERE entity = 'station' AND action = 'undo'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn undo_rolls_back_earlier_pole_inversions_when_later_pole_stale() {
        let conn = open_in_memory().unwrap();
        let applied = seed_renamed(&conn);
        // stale pole listed LAST: all pole guards run before any UPDATE, so
        // pole 2's staleness refuses the tx before pole 1 is touched —
        // pole 1's untouched state is the property under test
        edit_stop(&conn, applied.poles[1].new_svi,
            &StopEdit { stop_name: Some("shadowed".into()), ..Default::default() }, "h", "n").unwrap();
        let err = apply_station_rename_undo(&conn, &undo_op(&applied), "herry", "revert").unwrap_err();
        assert!(err.to_string().contains("cannot be undone"), "{err}");
        // pole 1's inversion rolled back: minted version current again,
        // original closed, bearing still on the minted svi, rsv untouched
        let (to, on_new): (Option<String>, i64) = conn.query_row(
            "SELECT valid_to, (SELECT COUNT(*) FROM stop_bearing WHERE stop_version_id = ?1)
             FROM stop_version WHERE stop_version_id = ?1",
            [applied.poles[0].new_svi], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert!(to.is_none(), "pole 1's minted version unclosed by rollback");
        assert_eq!(on_new, 1, "bearing row still on pole 1's minted svi");
        let to: Option<String> = conn.query_row(
            "SELECT valid_to FROM stop_version WHERE stop_version_id = 10", [], |r| r.get(0)).unwrap();
        assert!(to.is_some(), "pole 1's original still closed");
        let sv: i64 = conn.query_row(
            "SELECT stop_version_id FROM route_stop_version WHERE route_stop_version_id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(sv, applied.poles[0].new_svi, "rsv still on the minted svi");
    }

    #[test]
    fn apply_stop_edit_moves_position_deletes_bearing_marks_three() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
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
        let (to, lat, src): (Option<String>, f64, String) = conn.query_row(
            "SELECT valid_to, latitude, source FROM stop_version WHERE stop_version_id = ?1",
            [out.new_version_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!((to.is_some(), lat, src.as_str()), (false, 24.001, "manual"));
        // rsv repointed
        let sv: i64 = conn.query_row(
            "SELECT stop_version_id FROM route_stop_version WHERE route_stop_version_id = 1",
            [], |r| r.get(0)).unwrap();
        assert_eq!(sv, out.new_version_id);
        // bearing deleted at the old svi
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM stop_bearing WHERE stop_version_id = ?1", [old], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        // marks: stop_bearing + projection(route 1) + station_bearing
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 3);
        assert_eq!(out.stale_routes, vec![1]);
        // edit_log: {field:{old,new}} shape
        let (ent, act, changes): (String, String, String) = conn.query_row(
            "SELECT entity, action, changes FROM edit_log WHERE edit_id = ?1",
            [out.edit_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
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
             VALUES (10, 123.5, 'OSM', '2026-01-02T00:00:00Z')", []).unwrap();
        let op = StopEditOp {
            stop_id: 1,
            stop_version_id: old,
            stop_name: Some("New".into()),
            ..Default::default()
        };
        let out = apply_stop_edit(&conn, &op, "herry", "rename").unwrap();
        // bearing row MOVED to the new svi, identical values + computed_at
        let (deg, computed): (f64, String) = conn.query_row(
            "SELECT bearing_deg, computed_at FROM stop_bearing WHERE stop_version_id = ?1",
            [out.new_version_id], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((deg, computed.as_str()), (123.5, "2026-01-02T00:00:00Z"));
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM stop_bearing WHERE stop_version_id = ?1", [old], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        // zero stale marks — a rename invalidates no derived data
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        assert!(out.stale_routes.is_empty());
    }

    #[test]
    fn apply_stop_edit_refuses_stale_version_zero_partial() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        let first = StopEditOp { stop_id: 1, stop_version_id: old, stop_name: Some("A".into()), ..Default::default() };
        apply_stop_edit(&conn, &first, "h", "n").unwrap();
        let err = apply_stop_edit(&conn, &first, "h", "n").unwrap_err();
        assert!(err.to_string().contains("not the current version"), "{err}");
        // zero partial: exactly one minted version exists
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM stop_version WHERE stop_id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2);
    }
}
