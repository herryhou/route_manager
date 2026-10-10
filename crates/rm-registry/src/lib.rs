//! Station finalize: bearing derivation + dual-carriageway station split.

pub mod bearing;
pub mod split;

/// `--stale`: any bearing mark -> whole-table rebuild -> split ->
/// conditional station re-bear -> clear the snapshot rowids. Rowid
/// snapshot: marks inserted after startup survive (concurrent edit).
pub fn run_stale(conn: &rusqlite::Connection, batch_id: i64) -> anyhow::Result<bool> {
    let snapshot: Vec<i64> = {
        let mut stmt = conn.prepare(
            "SELECT rowid FROM stale_mark
             WHERE table_name IN ('stop_bearing', 'station_bearing')",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
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
        let mut del = tx.prepare("DELETE FROM stale_mark WHERE rowid = ?1")?;
        for rid in &snapshot {
            del.execute([rid])?;
        }
    }
    tx.commit()?;
    Ok(true)
}
