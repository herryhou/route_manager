use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

/// Register a new import batch (status OPEN). Returns its id.
pub fn create_batch(
    conn: &Connection,
    source_dir: &str,
    file_count: i64,
    captured_at: &str,
    valid_from: &str,
) -> Result<i64> {
    let id = crate::mint(conn, "import_batch")?;
    conn.execute(
        "INSERT INTO import_batch (batch_id, source_dir, file_count, captured_at, valid_from, status, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'OPEN', ?6)",
        params![id, source_dir, file_count, captured_at, valid_from, crate::now_rfc3339()],
    )?;
    Ok(id)
}

/// OPEN -> CLOSED / FAILED (or back). Publishing a batch requirement is
/// enforced by callers; this only moves the flag.
pub fn set_batch_status(conn: &Connection, batch_id: i64, status: &str) -> Result<()> {
    conn.execute(
        "UPDATE import_batch SET status = ?1 WHERE batch_id = ?2",
        params![status, batch_id],
    )?;
    Ok(())
}

/// Most recent OPEN batch, if any. Default `--batch` target for every tool.
pub fn latest_open_batch(conn: &Connection) -> Result<Option<i64>> {
    conn.query_row(
        "SELECT batch_id FROM import_batch WHERE status = 'OPEN' ORDER BY batch_id DESC LIMIT 1",
        [],
        |r| r.get(0),
    )
    .optional()
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_in_memory;

    #[test]
    fn create_batch_records_metadata() {
        let conn = open_in_memory().unwrap();
        let id = create_batch(
            &conn,
            "/x",
            3,
            "2026-10-02T00:00:00Z",
            "2026-10-02T00:00:00Z",
        )
        .unwrap();
        let (dir, status, valid): (String, String, String) = conn
            .query_row(
                "SELECT source_dir, status, valid_from FROM import_batch WHERE batch_id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            (dir.as_str(), status.as_str(), valid.as_str()),
            ("/x", "OPEN", "2026-10-02T00:00:00Z")
        );
    }
}
