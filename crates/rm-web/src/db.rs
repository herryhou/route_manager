//! Viewer-side schema probing: friendly errors for missing/stale DBs.

use anyhow::{Context, Result};
use rusqlite::Connection;

/// Probe for the pipeline schema; the server never migrates.
pub fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| {
        r.get::<_, i64>(0)
    })
    .context("DB has no pipeline schema — run the pipeline stages first (data/taichung.db)")?;
    conn.query_row("SELECT COUNT(*) FROM seg_segment", [], |r| r.get::<_, i64>(0))
        .context("DB predates the segment tables — re-run the pipeline (migrations 0003+)")?;
    Ok(())
}

/// Latest import batch id (staging lookups scope to it); 0 when no batches.
pub fn latest_batch_id(conn: &Connection) -> Result<i64> {
    let id: Option<i64> =
        conn.query_row("SELECT MAX(batch_id) FROM import_batch", [], |r| r.get(0))?;
    Ok(id.unwrap_or(0))
}
