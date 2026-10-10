//! Station finalize: bearing derivation + dual-carriageway station split.

pub mod bearing;
pub mod split;

/// `--stale`: any bearing mark -> whole-table rebuild -> split ->
/// conditional station re-bear -> clear the snapshot marks. Snapshot is
/// (rowid, cause_edit_id) pairs and the clear matches both: a mark
/// re-inserted onto a reused rowid mid-rebuild (concurrent edit) carries
/// a different cause, so it survives the clear; marks inserted after
/// startup survive because their rowids were never snapshotted.
pub fn run_stale(conn: &rusqlite::Connection, batch_id: i64) -> anyhow::Result<bool> {
    let snapshot: Vec<(i64, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT rowid, cause_edit_id FROM stale_mark
             WHERE table_name IN ('stop_bearing', 'station_bearing')",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    if snapshot.is_empty() {
        return Ok(false);
    }

    let _b = bearing::run(conn, batch_id)?;
    let sp = split::run(conn)?;
    if sp.stops_moved > 0 {
        bearing::stations(conn)?;
    }

    let tx = conn.unchecked_transaction()?;
    {
        let mut del =
            tx.prepare("DELETE FROM stale_mark WHERE rowid = ?1 AND cause_edit_id = ?2")?;
        for (rowid, cause) in &snapshot {
            del.execute([rowid, cause])?;
        }
    }
    tx.commit()?;
    Ok(true)
}
