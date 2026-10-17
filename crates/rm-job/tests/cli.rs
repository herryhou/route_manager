//! rm-job CLI integration: attach parsing, the dry-run default, apply,
//! rejections and exit codes, and the undo round-trip — all driven through
//! `rm_job::run` with a buffered report, no process capture.

use std::path::{Path, PathBuf};

use rm_job::{parse_attach, run, Cli, Command};
use rusqlite::Connection;

const T0: &str = "2026-01-01T00:00:00Z";

/// rm-web/tests/edit.rs's chain (station_group → station → stop → pole →
/// route → route_version → subroute_version → route_stop_version) plus the
/// id_sequence seeds for every table apply mints — without them mint would
/// auto-seed at 1 and collide with the fixture's own row 1 (rm-db's
/// apply_fixture seeds the same way).
fn fixture(conn: &Connection) {
    conn.execute_batch(&format!(
        "INSERT INTO station_group (station_group_id, created_at) VALUES (1, '{T0}');
         INSERT INTO station_group_version (station_group_version_id, station_group_id, station_group_name, source, valid_from, created_at)
             VALUES (1, 1, 'g', 'capture', '{T0}', '{T0}');
         INSERT INTO station (station_id, ext_uid, created_at) VALUES (1, 'tcg:node:1', '{T0}');
         INSERT INTO station_version (station_version_id, station_id, station_group_id, station_name, latitude, longitude, valid_from, created_at)
             VALUES (1, 1, 1, 'S', 24.0, 120.0, '{T0}', '{T0}');
         INSERT INTO stop (stop_id, ext_uid, created_at) VALUES (1, 'tcg:stop:1', '{T0}');
         INSERT INTO stop_version (stop_version_id, stop_id, station_id, stop_code, stop_name, direction, latitude, longitude, valid_from, created_at)
             VALUES (10, 1, 1, '001', 'Old', 0, 24.0, 120.0, '{T0}', '{T0}');
         INSERT INTO route (route_id, route_code, ext_uid, revision, created_at, updated_at)
             VALUES (1, '1', 'tcg:1', 1, '{T0}', '{T0}');
         INSERT INTO route_version (route_version_id, route_id, version_no, valid_from, status, created_at)
             VALUES (1, 1, 1, '{T0}', 'PUBLISHED', '{T0}');
         UPDATE route SET current_route_version_id = 1 WHERE route_id = 1;
         INSERT INTO subroute (subroute_id, route_id, subroute_code, created_at)
             VALUES (1, 1, '1-0', '{T0}');
         INSERT INTO subroute_version (subroute_version_id, route_version_id, subroute_id, subroute_name, created_at)
             VALUES (1, 1, 1, '1', '{T0}');
         INSERT INTO route_stop_version (route_stop_version_id, subroute_version_id, stop_version_id, direction, stop_sequence, created_at)
             VALUES (1, 1, 10, 0, 1, '{T0}');
         INSERT INTO id_sequence (table_name, next_value) VALUES
             ('station_group', 2), ('station_group_version', 2),
             ('station', 2), ('station_version', 2),
             ('stop', 2), ('stop_version', 11),
             ('route_version', 2), ('subroute_version', 2),
             ('route_stop_version', 2);"
    ))
    .unwrap();
}

/// A fixture db on a tempfile; hold the guard so the dir outlives the test.
fn fixture_db() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let conn = rm_db::open(&db).unwrap();
    fixture(&conn);
    (dir, db)
}

fn add_stop_cli(name: &str, attaches: &[&str], db: &Path, apply: bool) -> Cli {
    Cli {
        command: Command::AddStop {
            name: name.to_string(),
            lat: 24.1405,
            lon: 120.6857,
            attach: attaches.iter().map(|s| s.to_string()).collect(),
            station: None,
            db: db.to_path_buf(),
            editor: "test".to_string(),
            note: None,
            apply,
        },
    }
}

fn undo_cli(edit_id: i64, db: &Path) -> Cli {
    Cli {
        command: Command::Undo {
            edit_id,
            db: db.to_path_buf(),
            editor: "test".to_string(),
        },
    }
}

/// First integer after `label` in the report.
fn grab(report: &str, label: &str) -> i64 {
    let i = report
        .find(label)
        .unwrap_or_else(|| panic!("report missing {label:?}: {report}"));
    let rest = &report[i + label.len()..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end]
        .parse()
        .unwrap_or_else(|_| panic!("no integer after {label:?}: {report}"))
}

/// Every user table's full contents, name-then-rowid order — the
/// byte-identical read-only check. `skip` drops tables the comparison
/// means to exclude (same shape as rm-db's edit-test dump).
fn dump_at(db: &Path, skip: &[&str]) -> String {
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
        if skip.contains(&t.as_str()) {
            continue;
        }
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
fn attach_parses_xno_dir_and_code() {
    // valid forms parse to (xno, direction, insert-after)
    let a = parse_attach("1:0").unwrap();
    assert_eq!(a.xno, "1");
    assert_eq!(a.direction, 0);
    assert_eq!(a.after_stop_code, None);
    let b = parse_attach("23:1@C").unwrap();
    assert_eq!(b.xno, "23");
    assert_eq!(b.direction, 1);
    assert_eq!(b.after_stop_code.as_deref(), Some("C"));

    // invalid forms: exit 1 with a usage line, before the db is touched
    for bad in ["1", "1:x", "1:0@"] {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let (code, report) = run(add_stop_cli("X", &[bad], &db, false));
        assert_eq!(code, 1, "{bad}: {}", report.err);
        assert!(report.err.contains("usage"), "{bad}: {}", report.err);
        assert!(!db.exists(), "{bad}: rejected parse must not create the db");
    }
}

#[test]
fn dry_run_is_default_exit_0_prints_verdict_and_counts() {
    let (_dir, db) = fixture_db();
    let before = dump_at(&db, &[]);
    let (code, report) = run(add_stop_cli("X", &["1:0"], &db, false));
    assert_eq!(code, 0, "{}{}", report.out, report.err);
    let out = &report.out;
    assert!(out.contains("create new station"), "{out}");
    assert!(out.contains("route 1 dir 0: insert after end"), "{out}");
    assert!(out.contains("between Old → -"), "{out}");
    assert!(out.contains("stale_mark 4"), "{out}");
    assert!(out.contains("refresh needed for routes: 1"), "{out}");
    assert_eq!(dump_at(&db, &[]), before, "dry run must not write");
}

#[test]
fn apply_writes_and_reports_ids() {
    let (_dir, db) = fixture_db();
    let (code, report) = run(add_stop_cli("X", &["1:0"], &db, true));
    assert_eq!(code, 0, "{}{}", report.out, report.err);
    let out = &report.out;
    let edit_id = grab(out, "edit_id ");
    let stop_id = grab(out, "stop_id ");
    let station_id = grab(out, "station_id ");
    assert!(edit_id > 0 && stop_id > 0 && station_id > 0, "{out}");
    assert!(out.contains("refresh needed for routes: 1"), "{out}");
    // readback: the minted stop exists
    let conn = rm_db::open(&db).unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM stop WHERE stop_id = ?1",
            [stop_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1, "stop {stop_id} exists after --apply");
}

#[test]
fn unknown_route_exit_1_reason_on_stderr_sink() {
    let (_dir, db) = fixture_db();
    let (code, report) = run(add_stop_cli("X", &["99:0"], &db, false));
    assert_eq!(code, 1);
    assert!(report.err.contains("99"), "{}", report.err);
}

#[test]
fn empty_attachments_exit_2() {
    let (_dir, db) = fixture_db();
    let (code, report) = run(add_stop_cli("X", &[], &db, false));
    assert_eq!(code, 2);
    assert!(report.err.contains("attach"), "{}", report.err);
}

/// Excluded from the round-trip dump: the audit rows (the undo writes its
/// own by design), the stale marks (the undo deletes the job's own), and
/// the id allocator (apply mints ids; ids are never reused) — the same
/// exclusions rm-db's `ROUNDTRIP_SKIP` uses for the primitive's B2==B0 gate.
const ROUNDTRIP_SKIP: &[&str] = &["edit_log", "stale_mark", "id_sequence"];

#[test]
fn undo_subcommand_roundtrips() {
    let (_dir, db) = fixture_db();
    let b0 = dump_at(&db, ROUNDTRIP_SKIP);
    let (code, report) = run(add_stop_cli("X", &["1:0"], &db, true));
    assert_eq!(code, 0, "{}{}", report.out, report.err);
    let edit_id = grab(&report.out, "edit_id ");
    let (code, report) = run(undo_cli(edit_id, &db));
    assert_eq!(code, 0, "{}{}", report.out, report.err);
    assert!(report.out.contains("undo"), "{}", report.out);
    assert_eq!(
        dump_at(&db, ROUNDTRIP_SKIP),
        b0,
        "apply then undo is byte-identical (B2==B0)"
    );
}
