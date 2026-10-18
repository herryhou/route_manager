//! rm-job refresh: the dry-run pending-mark report (read-only) and the
//! --apply match → segments → registry chain — batch/zero-mark guards and
//! the unconsumable-mark rule, driven through `rm_job::run`.

use std::path::{Path, PathBuf};

use rm_job::{run, Cli, Command, Report};

/// The stdout envelope, parsed — every run's stdout must be exactly one
/// JSON object.
fn envelope_of(report: &Report) -> serde_json::Value {
    serde_json::from_str(&report.out)
        .unwrap_or_else(|e| panic!("stdout not JSON: {e}: {}", report.out))
}

/// rm-web/tests/refresh.rs's recipe: schema + one CLOSED batch + a non-empty
/// road graph + a seg_match mark for unpublished route 999 (the match step
/// warns and KEEPS it — a run can be Done with marks left).
fn fixture_db() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let conn = rm_db::open(&db).unwrap();
    conn.execute(
        "INSERT INTO import_batch (batch_id, source_dir, file_count, captured_at, valid_from, status, created_at)
         VALUES (1, 'assets/x', 1, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'CLOSED', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute_batch(
        "INSERT INTO osm_way (osm_way_id, highway, oneway, name) VALUES (100, 'residential', 0, '測試路');
         INSERT INTO osm_node (osm_node_id, lat, lon) VALUES
            (1, 24.1405, 120.6857), (2, 24.1408, 120.6857), (3, 24.1412, 120.6857);
         INSERT INTO osm_way_node (osm_way_id, ordinal, osm_node_id) VALUES
            (100, 1, 1), (100, 2, 2), (100, 3, 3);
         INSERT INTO edit_log (ts, editor, entity, entity_id, action, changes)
         VALUES ('2026-01-01T00:00:00Z', 't', 'polyline', 999, 'create', '{}');
         INSERT INTO stale_mark VALUES ('seg_match', '999:0', 1, '2026-01-01T00:00:00Z');",
    )
    .unwrap();
    (dir, db)
}

fn refresh_cli(db: &Path, apply: bool) -> Cli {
    Cli {
        command: Command::Refresh {
            outlier_m: 50.0,
            db: db.to_path_buf(),
            apply,
            json: None,
        },
    }
}

/// Every user table's full contents, name-then-rowid order — the
/// byte-identical read-only proof.
fn dump_at(db: &Path) -> String {
    use rusqlite::types::ValueRef;
    let conn = rm_db::open(db).unwrap();
    let names: Vec<String> = conn
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let mut out = String::new();
    for t in names {
        out.push_str(&format!("-- {t}\n"));
        let mut stmt = conn
            .prepare(&format!("SELECT * FROM \"{t}\" ORDER BY rowid"))
            .unwrap();
        let ncols = stmt.column_count();
        let mut rows = stmt.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            for i in 0..ncols {
                match row.get_ref(i).unwrap() {
                    ValueRef::Null => out.push_str("· "),
                    ValueRef::Integer(n) => out.push_str(&format!("{n} ")),
                    ValueRef::Real(f) => out.push_str(&format!("{f:?} ")),
                    ValueRef::Text(s) => {
                        out.push_str(&format!("{:?} ", String::from_utf8_lossy(s)))
                    }
                    ValueRef::Blob(b) => out.push_str(&format!("blob:{b:?} ")),
                }
            }
            out.push('\n');
        }
    }
    out
}

#[test]
fn refresh_dry_run_reports_pending_marks() {
    let (_dir, db) = fixture_db();
    let before = dump_at(&db);
    let (code, report) = run(refresh_cli(&db, false));
    assert_eq!(code, 0, "{}{}", report.out, report.err);
    let v = envelope_of(&report)["data"].clone();
    assert_eq!(v["latest_batch"], 1);
    assert_eq!(v["pending_marks"], 1);
    assert_eq!(v["tables"]["seg_match"], 1);
    assert_eq!(v["tables"]["stop_bearing"], 0);
    assert_eq!(v["marks"][0]["table"], "seg_match");
    assert_eq!(v["marks"][0]["key"], "999:0");
    assert_eq!(v["marks"][0]["cause_edit_id"], 1);
    assert_eq!(dump_at(&db), before, "dry run must not write");
}

#[test]
fn refresh_apply_runs_chain_and_keeps_unconsumable_mark() {
    let (_dir, db) = fixture_db();
    let (code, report) = run(refresh_cli(&db, true));
    assert_eq!(code, 0, "{}{}", report.out, report.err);
    let logs: Vec<String> = envelope_of(&report)["logs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap().to_string())
        .collect();
    assert!(
        logs.iter().any(|l| l.starts_with("refresh: match:")),
        "{logs:?}"
    );
    assert!(
        logs.iter().any(|l| l.starts_with("refresh: registry:")),
        "{logs:?}"
    );
    let v = envelope_of(&report)["data"].clone();
    assert_eq!(v["batch_id"], 1);
    assert_eq!(v["steps"]["match"]["ran"], true);
    assert_eq!(v["steps"]["segments"]["ran"], false, "no projection marks");
    // the kept mark never becomes a bearing mark, so registry finds nothing
    assert_eq!(v["steps"]["registry"]["ran"], false);
    assert_eq!(v["pending_marks"], 1, "unconsumable mark survives");
    let conn = rm_db::open_readonly(&db).unwrap();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn refresh_apply_without_batch_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    rm_db::open(&db).unwrap(); // schema, but NO import_batch row
    let (code, report) = run(refresh_cli(&db, true));
    assert_eq!(code, 1, "{}{}", report.out, report.err);
    assert!(report.err.contains("no import batch"), "{}", report.err);
}

#[test]
fn refresh_apply_with_zero_marks_noop() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let conn = rm_db::open(&db).unwrap();
    conn.execute(
        "INSERT INTO import_batch (batch_id, source_dir, file_count, captured_at, valid_from, status, created_at)
         VALUES (1, 'assets/x', 1, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'CLOSED', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    drop(conn);
    let (code, report) = run(refresh_cli(&db, true));
    assert_eq!(code, 2, "{}{}", report.out, report.err);
    assert!(
        report.err.contains("0 pending stale marks"),
        "{}",
        report.err
    );
}

#[test]
fn refresh_dry_run_rejects_missing_db_without_creating_it() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let (code, report) = run(refresh_cli(&db, false));
    assert_eq!(code, 1, "{}{}", report.out, report.err);
    assert!(!db.exists(), "dry run must not create the db");
}
