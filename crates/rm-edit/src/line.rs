//! Manual whole-line polyline override (phase 2) — transitional shim over
//! the collapsed rm-db primitive (`rm_db::edit::line`), kept until the last
//! migration step flips rm-web over and deletes this crate.

use anyhow::Result;
use rusqlite::Connection;

use crate::map_db_err;

pub use rm_db::LineEditCmd;

#[derive(Debug)]
pub struct LineOutcome {
    pub edit_id: i64,
    pub updated_at: String,
    pub created: bool,
}

/// Application service: load → plan (pure) → apply (one tx).
pub fn edit_polyline_line(
    conn: &Connection,
    cmd: LineEditCmd,
    editor: &str,
    note: &str,
) -> Result<LineOutcome> {
    let applied = rm_db::edit_polyline_line(conn, cmd, editor, note).map_err(map_db_err)?;
    Ok(LineOutcome {
        edit_id: applied.edit_id,
        updated_at: applied.updated_at,
        created: applied.created,
    })
}

#[derive(Debug)]
pub struct LineUndoOutcome {
    pub edit_id: i64,
    pub deleted: bool,
}

/// Application service: load → plan (pure) → apply (one tx). Forward-write
/// undo — no version chain; the apply's ts currency guard is the authority.
pub fn undo_polyline_line(
    conn: &Connection,
    edit_id: i64,
    editor: &str,
    note: &str,
) -> Result<LineUndoOutcome> {
    let undone = rm_db::undo_polyline_line(conn, edit_id, editor, note).map_err(map_db_err)?;
    Ok(LineUndoOutcome {
        edit_id: undone.edit_id,
        deleted: undone.deleted,
    })
}
