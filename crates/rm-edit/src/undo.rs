//! Station undo (rename + move) — transitional shim over the collapsed
//! rm-db primitive (`rm_db::edit::station_undo`), kept until the last
//! migration step flips rm-web over and deletes this crate.

use anyhow::Result;
use rusqlite::Connection;

use crate::map_db_err;

#[derive(Debug)]
pub struct UndoOutcome {
    pub edit_id: i64, // the undo's own edit_log row
    pub restored_station_version_id: i64,
}

/// Application service: load the logged station edit → plan (pure) → apply
/// the inverse in one tx. Handles both station kinds (rename, move) via the
/// kind-dispatched plan.
pub fn undo_station(
    conn: &Connection,
    edit_id: i64,
    editor: &str,
    note: &str,
) -> Result<UndoOutcome> {
    let undone = rm_db::undo_station(conn, edit_id, editor, note).map_err(map_db_err)?;
    Ok(UndoOutcome {
        edit_id: undone.edit_id,
        restored_station_version_id: undone.restored_station_version_id,
    })
}
