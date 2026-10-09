//! Viewer-side schema probing: friendly errors for missing/stale DBs.

use anyhow::{Context, Result};
use rusqlite::Connection;

/// Probe for the pipeline schema; the server never migrates.
pub fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| {
        r.get::<_, i64>(0)
    })
    .context("DB has no pipeline schema — run the pipeline stages first (data/taichung.db)")?;
    conn.query_row("SELECT COUNT(*) FROM seg_geometry_draft", [], |r| {
        r.get::<_, i64>(0)
    })
    .context("DB predates the segment tables — re-run the pipeline (migrations 0003+)")?;
    let dev_col: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('seg_match_stat')
             WHERE name = 'dev_p95_m'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    anyhow::ensure!(
        dev_col == 1,
        "seg_match_stat has no dev columns — re-run the pipeline (migration 0006+)"
    );
    conn.query_row("SELECT COUNT(*) FROM stale_mark", [], |r| {
        r.get::<_, i64>(0)
    })
    .context("DB predates the edit tables — re-run the pipeline (or open once with a writable tool) to migrate (migration 0010+)")?;
    Ok(())
}

/// Latest import batch id (staging lookups scope to it); 0 when no batches.
pub fn latest_batch_id(conn: &Connection) -> Result<i64> {
    let id: Option<i64> =
        conn.query_row("SELECT MAX(batch_id) FROM import_batch", [], |r| r.get(0))?;
    Ok(id.unwrap_or(0))
}
