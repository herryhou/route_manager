//! Station finalize: bearing derivation + dual-carriageway station split.

pub mod bearing;
pub mod split;

/// `--stale`: any bearing mark -> whole-table rebuild -> split ->
/// conditional station re-bear -> clear the snapshot marks. Snapshot is
/// (rowid, cause_edit_id) pairs and the clear matches both: a mark
/// re-inserted onto a reused rowid mid-rebuild (concurrent edit) carries
/// a different cause, so it survives the clear; marks inserted after
/// startup survive because their rowids were never snapshotted.
/// What one `--stale` refresh did; `None` means nothing was stale and the
/// DB was left untouched.
pub struct StaleRun {
    pub bearings: bearing::Summary,
    pub split: split::Summary,
}

pub fn run_stale(conn: &rusqlite::Connection, batch_id: i64) -> anyhow::Result<Option<StaleRun>> {
    let marks = rm_db::stale::snapshot(
        conn,
        &[
            rm_db::stale::StaleTable::StopBearing,
            rm_db::stale::StaleTable::StationBearing,
        ],
    )?;
    if marks.is_empty() {
        return Ok(None);
    }

    let b = bearing::run(conn, batch_id)?;
    let sp = split::run(conn)?;
    if sp.stops_moved > 0 {
        bearing::stations(conn)?;
    }

    let tx = conn.unchecked_transaction()?;
    let tokens: Vec<(i64, i64)> = marks.iter().map(|m| (m.rowid, m.cause_edit_id)).collect();
    rm_db::stale::clear_all_if_unchanged(&tx, &tokens)?;
    tx.commit()?;
    Ok(Some(StaleRun {
        bearings: b,
        split: sp,
    }))
}
