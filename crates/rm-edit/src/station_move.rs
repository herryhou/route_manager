//! Station move — transitional shim over the collapsed rm-db primitive
//! (`rm_db::edit::station_move`), kept until the last migration step flips
//! rm-web over and deletes this crate.

use anyhow::Result;
use rusqlite::Connection;

use crate::map_db_err;

pub use rm_db::StationMove;

#[derive(Debug)]
pub struct MoveOutcome {
    pub edit_id: i64,
    pub new_station_version_id: i64,
}

/// Application service: load → plan (pure) → apply (one tx).
pub fn move_station(
    conn: &Connection,
    cmd: StationMove,
    editor: &str,
    note: &str,
) -> Result<MoveOutcome> {
    let applied = rm_db::move_station(conn, cmd, editor, note).map_err(map_db_err)?;
    Ok(MoveOutcome {
        edit_id: applied.edit_id,
        new_station_version_id: applied.new_station_version_id,
    })
}
