//! rm-db: SQLite migrations, id allocator, import-batch lifecycle.
//!
//! All published-table writes go through repos in this crate (INSERT-only);
//! UPDATE is reserved for pointers/revision inside a publish transaction.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};

mod allocator;
mod batch;
mod edit;
mod migrate;

pub use allocator::mint;
pub use batch::{create_batch, latest_batch, latest_open_batch, set_batch_status};
pub use edit::{
    edit_entity, entity_history, EditError, EditLogRow, EditOutcome,
    StopEditOp, apply_stop_edit, StopRegroupOp, apply_stop_regroup,
    StopEditRow, load_stop_edit, StopEditUndoOp, apply_stop_undo,
    StationSnapshot, PoleSnapshot, load_station_snapshot, StopSnapshot, load_stop_snapshot,
    station_is_current,
    StationRenameOp, PoleOp, StationRenameApplied, PoleApplied, apply_station_rename,
    StationEdit, load_station_edit,
    StationMoveOp, apply_station_move, StationMoveApplied,
    StationRenameUndoOp, PoleUndoOp, StationMoveUndoOp, StationUndoOp, StationUndone,
    apply_station_undo,
    PieceRow, PolylineSnapshot, load_polyline_snapshot,
    PieceEditOp, PieceApplied, apply_piece_edit,
    PolylineEditRow, load_polyline_edit,
    PieceRestoreState, PieceUndoOp, PieceUndone, apply_piece_undo,
    OverrideRow, LineSnapshot, load_polyline_line,
    LineEditOp, LineApplied, apply_line_edit,
    LineUndoOp, LineUndone, apply_line_undo,
    effective_encoded,
};
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

/// Open an existing DB read-only (viewer mode): no pragmas that write,
/// no migrations. WAL readers coexist with a pipeline writer.
pub fn open_readonly(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("open readonly {}", path.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(30))?;
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
    fn open_readonly_reads_but_cannot_write() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ro.db");
        let conn = open(&db).unwrap();
        conn.execute(
            "INSERT INTO id_sequence (table_name, next_value) VALUES ('t', 41)",
            [],
        )
        .unwrap();

        let ro = open_readonly(&db).unwrap();
        let n: i64 = ro
            .query_row(
                "SELECT next_value FROM id_sequence WHERE table_name = 't'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 41);
        assert!(
            ro.execute(
                "INSERT INTO id_sequence (table_name, next_value) VALUES ('u', 1)",
                []
            )
            .is_err(),
            "read-only connection must reject writes"
        );
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
    fn current_views_expose_only_current_versions() {
        let conn = open_in_memory().unwrap();
        let t = "2026-10-06T00:00:00Z";
        // seed: closed + open versions of station/stop, superseded + current
        // route version whose subroute stop lists an old and a new row
        conn.execute_batch(&format!(
            "INSERT INTO station_group (station_group_id, created_at) VALUES (1, '{t}');
             INSERT INTO station_group_version (station_group_version_id, station_group_id, station_group_name, valid_from, created_at)
               VALUES (1, 1, 'G1', '{t}', '{t}');
             INSERT INTO station (station_id, ext_uid, created_at) VALUES (1, 'tcg:node:1', '{t}');
             INSERT INTO station_version (station_version_id, station_id, station_group_id, station_name, latitude, longitude, valid_from, valid_to, created_at)
               VALUES (1, 1, 1, '舊名', 24.0, 120.0, '{t}', '{t}', '{t}');
             INSERT INTO station_version (station_version_id, station_id, station_group_id, station_name, latitude, longitude, valid_from, created_at)
               VALUES (2, 1, 1, '新名', 24.0, 120.0, '{t}', '{t}');
             INSERT INTO stop (stop_id, created_at) VALUES (1, '{t}');
             INSERT INTO stop_version (stop_version_id, stop_id, station_id, stop_code, stop_name, direction, latitude, longitude, valid_from, valid_to, created_at)
               VALUES (1, 1, 1, 'S1', '舊', 0, 24.0, 120.0, '{t}', '{t}', '{t}');
             INSERT INTO stop_version (stop_version_id, stop_id, station_id, stop_code, stop_name, direction, latitude, longitude, valid_from, created_at)
               VALUES (2, 1, 1, 'S1', '新', 0, 24.0, 120.0, '{t}', '{t}');
             INSERT INTO route (route_id, route_code, ext_uid, created_at, updated_at)
               VALUES (1, '3', 'tcg:3', '{t}', '{t}');
             INSERT INTO route_version (route_version_id, route_id, version_no, valid_from, valid_to, status, created_at)
               VALUES (10, 1, 1, '{t}', '{t}', 'SUPERSEDED', '{t}');
             INSERT INTO route_version (route_version_id, route_id, version_no, valid_from, status, created_at)
               VALUES (20, 1, 2, '{t}', 'PUBLISHED', '{t}');
             UPDATE route SET current_route_version_id = 20 WHERE route_id = 1;
             INSERT INTO subroute (subroute_id, route_id, subroute_code, created_at) VALUES (1, 1, 'A', '{t}');
             INSERT INTO subroute_version (subroute_version_id, route_version_id, subroute_id, subroute_name, created_at)
               VALUES (1, 20, 1, '往北', '{t}');
             INSERT INTO subroute_version (subroute_version_id, route_version_id, subroute_id, subroute_name, created_at)
               VALUES (2, 10, 1, '往北舊', '{t}');
             INSERT INTO route_stop_version (route_stop_version_id, subroute_version_id, stop_version_id, direction, stop_sequence, created_at)
               VALUES (1, 1, 2, 0, 1, '{t}');
             INSERT INTO route_stop_version (route_stop_version_id, subroute_version_id, stop_version_id, direction, stop_sequence, created_at)
               VALUES (2, 2, 1, 0, 1, '{t}');"
        ))
        .unwrap();

        let (name,): (String,) = conn
            .query_row(
                "SELECT station_name FROM station_current WHERE station_id = 1",
                [],
                |r| Ok((r.get(0)?,)),
            )
            .unwrap();
        assert_eq!(name, "新名");

        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM station_group_current", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);

        let (sv_id, sname): (i64, String) = conn
            .query_row(
                "SELECT stop_version_id, stop_name FROM stop_current",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((sv_id, sname.as_str()), (2, "新"));

        let (rv_id, version_no, status): (i64, i64, String) = conn
            .query_row(
                "SELECT route_version_id, version_no, status FROM route_current",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((rv_id, version_no, status.as_str()), (20, 2, "PUBLISHED"));

        let rows: Vec<(i64, i64, String)> = conn
            .prepare(
                "SELECT route_stop_version_id, stop_version_id, subroute_name
                 FROM route_stop_current
                 ORDER BY route_stop_version_id",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![(1, 2, "往北".to_string())],
            "only the current route version's stops appear"
        );
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
