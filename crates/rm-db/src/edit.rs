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
}
