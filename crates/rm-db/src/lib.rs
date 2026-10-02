//! rm-db: SQLite migrations, id allocator, import-batch lifecycle.
//!
//! All published-table writes go through repos in this crate (INSERT-only);
//! UPDATE is reserved for pointers/revision inside a publish transaction.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::Connection;

mod allocator;
mod batch;
mod migrate;

pub use allocator::mint;
pub use batch::{create_batch, latest_open_batch, set_batch_status};
pub use migrate::migrate;

/// RFC3339 UTC timestamp for created_at/valid_from columns.
pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Open (create if needed) the pipeline DB: pragmas + migrations applied.
pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
    apply_pragmas(&conn)?;
    migrate(&conn)?;
    Ok(conn)
}

/// In-memory DB with the same pragmas + migrations (tests, verify).
pub fn open_in_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    apply_pragmas(&conn)?;
    migrate(&conn)?;
    Ok(conn)
}

/// Pragmas shared by file and in-memory connections.
fn apply_pragmas(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.busy_timeout(std::time::Duration::from_secs(30))?;
    // WAL is a no-op on :memory: — ignore the returned mode.
    let _ = conn.pragma_update(None, "journal_mode", "WAL");
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrate_is_idempotent_and_tracked() {
        let conn = open_in_memory().unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
            .unwrap();
        assert!(n >= 1, "at least 0001 applied");
        migrate(&conn).unwrap(); // second run: no-op, no error
        let n2: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, n2);
    }

    #[test]
    fn mint_yields_sequential_never_reused_ids() {
        let conn = open_in_memory().unwrap();
        assert_eq!(mint(&conn, "stop").unwrap(), 1);
        assert_eq!(mint(&conn, "stop").unwrap(), 2);
        assert_eq!(mint(&conn, "station").unwrap(), 1); // per-table sequence
    }

    #[test]
    fn mint_persists_across_connections() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let conn = open(&db).unwrap();
        assert_eq!(mint(&conn, "route").unwrap(), 1);
        drop(conn);
        let conn = open(&db).unwrap();
        assert_eq!(mint(&conn, "route").unwrap(), 2, "never restart from 1");
    }

    #[test]
    fn foreign_keys_are_enforced() {
        let conn = open_in_memory().unwrap();
        conn.execute("INSERT INTO station (station_id, ext_uid, created_at) VALUES (1, 'tcg:node:1', '2026-10-02T00:00:00Z')", [])
            .unwrap();
        // stop_version -> station FK: station 999 does not exist
        let r = conn.execute(
            "INSERT INTO stop_version (stop_version_id, stop_id, station_id, stop_code, stop_name, direction, latitude, longitude, valid_from, created_at)
             VALUES (1, 1, 999, 'S1', 'X', 0, 24.0, 120.0, '2026-10-02T00:00:00Z', '2026-10-02T00:00:00Z')",
            [],
        );
        assert!(r.is_err(), "FK violation must fail");
    }

    #[test]
    fn route_version_no_is_unique_per_route() {
        let conn = open_in_memory().unwrap();
        conn.execute("INSERT INTO route (route_id, route_code, ext_uid, created_at, updated_at) VALUES (1, '3', 'tcg:3', '2026-10-02T00:00:00Z', '2026-10-02T00:00:00Z')", []).unwrap();
        conn.execute("INSERT INTO route_version (route_version_id, route_id, version_no, valid_from, status, created_at) VALUES (10, 1, 1, '2026-10-02T00:00:00Z', 'DRAFT', '2026-10-02T00:00:00Z')", []).unwrap();
        let r = conn.execute(
            "INSERT INTO route_version (route_version_id, route_id, version_no, valid_from, status, created_at) VALUES (11, 1, 1, '2026-10-02T00:00:00Z', 'DRAFT', '2026-10-02T00:00:00Z')",
            [],
        );
        assert!(r.is_err(), "(route_id, version_no) must be UNIQUE");
    }

    #[test]
    fn batch_lifecycle_open_and_close() {
        let conn = open_in_memory().unwrap();
        let id = create_batch(
            &conn,
            "/data/raw",
            392,
            "2026-10-02T00:00:00Z",
            "2026-10-02T00:00:00Z",
        )
        .unwrap();
        assert_eq!(latest_open_batch(&conn).unwrap(), Some(id));
        set_batch_status(&conn, id, "CLOSED").unwrap();
        assert_eq!(
            latest_open_batch(&conn).unwrap(),
            None,
            "closed batch is not open"
        );
        let id2 = create_batch(
            &conn,
            "/data/raw2",
            5,
            "2026-10-03T00:00:00Z",
            "2026-10-03T00:00:00Z",
        )
        .unwrap();
        assert_eq!(latest_open_batch(&conn).unwrap(), Some(id2));
    }

    #[test]
    fn ext_uid_is_unique() {
        let conn = open_in_memory().unwrap();
        conn.execute("INSERT INTO station (station_id, ext_uid, created_at) VALUES (1, 'tcg:node:1', '2026-10-02T00:00:00Z')", []).unwrap();
        let r = conn.execute(
            "INSERT INTO station (station_id, ext_uid, created_at) VALUES (2, 'tcg:node:1', '2026-10-02T00:00:00Z')",
            [],
        );
        assert!(r.is_err());
    }
}
