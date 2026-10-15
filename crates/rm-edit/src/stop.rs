//! Stop edit (move/name), regroup, and their undo — transitional shims over
//! the collapsed rm-db primitive (`rm_db::edit::stop`), kept until the last
//! migration step flips rm-web over and deletes this crate.

use anyhow::Result;
use rusqlite::Connection;

use crate::map_db_err;

pub use rm_db::{RegroupStop, StopEdit};

#[derive(Debug)]
pub struct StopOutcome {
    pub edit_id: i64,
    pub new_stop_version_id: i64,
    pub stale_routes: Vec<i64>,
}

/// Application service: load → plan (pure) → apply (one tx).
pub fn edit_stop(
    conn: &Connection,
    stop_version_id: i64,
    cmd: StopEdit,
    editor: &str,
    note: &str,
) -> Result<StopOutcome> {
    let applied = rm_db::edit_stop(conn, stop_version_id, cmd, editor, note).map_err(map_db_err)?;
    Ok(StopOutcome {
        edit_id: applied.edit_id,
        new_stop_version_id: applied.new_version_id,
        stale_routes: applied.stale_routes,
    })
}

/// Application service: load → target check → plan (pure) → apply (one tx).
pub fn regroup_stop(
    conn: &Connection,
    cmd: RegroupStop,
    editor: &str,
    note: &str,
) -> Result<StopOutcome> {
    let applied = rm_db::regroup_stop(conn, cmd, editor, note).map_err(map_db_err)?;
    Ok(StopOutcome {
        edit_id: applied.edit_id,
        new_stop_version_id: applied.new_version_id,
        stale_routes: applied.stale_routes,
    })
}

/// Application service: load → plan (pure) → apply (one tx).
pub fn undo_stop(conn: &Connection, edit_id: i64, editor: &str, note: &str) -> Result<StopOutcome> {
    let applied = rm_db::undo_stop(conn, edit_id, editor, note).map_err(map_db_err)?;
    Ok(StopOutcome {
        edit_id: applied.edit_id,
        new_stop_version_id: applied.new_version_id,
        stale_routes: applied.stale_routes,
    })
}
