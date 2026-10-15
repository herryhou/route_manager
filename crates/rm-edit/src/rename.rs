//! Scenario 1: station rename — transitional shim over the collapsed rm-db
//! primitive (`rm_db::edit::station_rename`), kept until the last migration
//! step flips rm-web over and deletes this crate.

use anyhow::Result;
use rusqlite::Connection;

use crate::map_db_err;

pub use rm_db::{RenameLevel, StationRename};

#[derive(Debug)]
pub struct RenameOutcome {
    pub edit_id: i64,
    pub new_station_version_id: i64,
    pub poles: Vec<PoleRenamed>,
}

#[derive(Debug)]
pub struct PoleRenamed {
    pub stop_id: i64,
    pub old_svi: i64,
    pub new_svi: i64,
}

/// Application service: load → plan (pure) → apply (one tx).
pub fn rename_station(
    conn: &Connection,
    cmd: StationRename,
    editor: &str,
    note: &str,
) -> Result<RenameOutcome> {
    let applied = rm_db::rename_station(conn, cmd, editor, note).map_err(map_db_err)?;
    Ok(RenameOutcome {
        edit_id: applied.edit_id,
        new_station_version_id: applied.new_station_version_id,
        poles: applied
            .poles
            .into_iter()
            .map(|p| PoleRenamed {
                stop_id: p.stop_id,
                old_svi: p.old_svi,
                new_svi: p.new_svi,
            })
            .collect(),
    })
}
