//! rm-job read commands (find-stop / show-route / show-path): envelope
//! shape, the old-DB degrade for `polyline_override`, and rejections —
//! driven through `rm_job::run` with a buffered report, no process capture.

use std::path::{Path, PathBuf};

use rm_job::{run, Cli, Command, Report};
use rusqlite::Connection;

/// The stdout envelope, parsed — every run's stdout must be exactly one
/// JSON object.
fn envelope_of(report: &Report) -> serde_json::Value {
    serde_json::from_str(&report.out)
        .unwrap_or_else(|e| panic!("stdout not JSON: {e}: {}", report.out))
}

const T0: &str = "2026-01-01T00:00:00Z";

/// tests/cli.rs's chain (station_group → station → stop → route →
/// route_version → subroute_version → route_stop_version) plus the
/// id_sequence seeds, extended with the src polyline rows for dir 0: the
/// 2-point Google reference vector (38.5,-120.2)→(40.7,-120.95), so the
/// read commands have a src to report.
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
             ('route_stop_version', 2);
         INSERT INTO import_batch (batch_id, source_dir, file_count, captured_at, valid_from, status, created_at)
             VALUES (1, 'assets/x', 1, '{T0}', '{T0}', 'CLOSED', '{T0}');
         INSERT INTO src_route_point (batch_id, xno, direction, encoded)
             VALUES (1, '1', 0, '_p~iF~ps|U_ulLnnqC');"
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

fn run_find_stop(db: &Path, xno: &str, name: Option<&str>, code: Option<&str>) -> (i32, Report) {
    run(Cli {
        command: Command::FindStop {
            xno: Some(xno.to_string()),
            name: name.map(str::to_string),
            code: code.map(str::to_string),
            db: db.to_path_buf(),
            json: None,
        },
    })
}

/// One ok find-stop run's `data` payload.
fn find_stop_ok(db: &Path, xno: &str, name: Option<&str>, code: Option<&str>) -> serde_json::Value {
    let (exit, report) = run_find_stop(db, xno, name, code);
    assert_eq!(exit, 0, "{}{}", report.out, report.err);
    envelope_of(&report)["data"].clone()
}

fn run_show_route(db: &Path, xno: &str) -> (i32, Report) {
    run(Cli {
        command: Command::ShowRoute {
            xno: Some(xno.to_string()),
            db: db.to_path_buf(),
            json: None,
        },
    })
}

/// One ok show-route run's `data` payload.
fn show_route_ok(db: &Path, xno: &str) -> serde_json::Value {
    let (exit, report) = run_show_route(db, xno);
    assert_eq!(exit, 0, "{}{}", report.out, report.err);
    envelope_of(&report)["data"].clone()
}

fn run_show_path(db: &Path, xno: &str, direction: i64) -> (i32, Report) {
    run(Cli {
        command: Command::ShowPath {
            xno: Some(xno.to_string()),
            direction: Some(direction),
            db: db.to_path_buf(),
            json: None,
        },
    })
}

/// One ok show-path run's `data` payload.
fn show_path_ok(db: &Path, xno: &str, direction: i64) -> serde_json::Value {
    let (exit, report) = run_show_path(db, xno, direction);
    assert_eq!(exit, 0, "{}{}", report.out, report.err);
    envelope_of(&report)["data"].clone()
}

#[test]
fn find_stop_by_name_returns_candidate_with_ids() {
    let (_dir, db) = fixture_db();
    // fixture route xno "1" has stop "Old" (assert on name + ids, not codes)
    let v = find_stop_ok(&db, "1", Some("Old"), None);
    let c = &v["candidates"][0];
    assert_eq!(c["stop_name"], "Old");
    assert!(c["stop_version_id"].as_i64().unwrap() > 0);
    assert!(c["directions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d.as_i64() == Some(0)));
}

/// Code-only filter: exercises the `stop_code =` branch and its placeholder
/// binding (code-only must give ?2 bound with 2 args).
#[test]
fn find_stop_by_code_only_returns_fixture_row() {
    let (_dir, db) = fixture_db();
    let v = find_stop_ok(&db, "1", None, Some("001"));
    let c = &v["candidates"][0];
    assert_eq!(c["stop_code"], "001");
    assert_eq!(c["stop_name"], "Old");
    assert!(c["stop_version_id"].as_i64().unwrap() > 0);
}

#[test]
fn find_stop_no_match_is_ok_empty() {
    let (_dir, db) = fixture_db();
    let (code, report) = run_find_stop(&db, "1", Some("不存在"), None);
    assert_eq!(code, 0);
    assert_eq!(
        envelope_of(&report)["data"]["candidates"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn show_route_lists_stops_and_path_state() {
    let (_dir, db) = fixture_db();
    let v = show_route_ok(&db, "1");
    assert!(v["directions"].as_array().unwrap().len() >= 1);
    // fixture has a src polyline for dir 0 (same shape src_fixture seeds in rm-db tests)
    assert_eq!(v["directions"][0]["path"], "src");
}

#[test]
fn show_path_reports_source_and_endpoints() {
    let (_dir, db) = fixture_db();
    let v = show_path_ok(&db, "1", 0);
    assert_eq!(v["source"], "src");
    assert!(v["endpoints"].as_array().unwrap().len() == 2);
}

/// Dir 1 has no src row and no override row: pathless — "none" with null
/// encoded/endpoints, still a 0 exit (the `path_source` "none" arm).
#[test]
fn show_path_none_when_no_src_or_override() {
    let (_dir, db) = fixture_db();
    let (code, report) = run_show_path(&db, "1", 1);
    assert_eq!(code, 0, "{}{}", report.out, report.err);
    let v = envelope_of(&report)["data"].clone();
    assert_eq!(v["source"], "none");
    assert!(v["encoded"].is_null(), "{}", report.out);
    assert!(v["endpoints"].is_null(), "{}", report.out);
}

#[test]
fn show_path_degrades_without_override_table() {
    let (_dir, db) = fixture_db();
    {
        let c = rm_db::open(&db).unwrap();
        c.execute("DROP TABLE polyline_override", []).unwrap();
    }
    let (code, report) = run_show_path(&db, "1", 0);
    assert_eq!(code, 0, "{}", report.out);
    let v = envelope_of(&report);
    assert!(v["logs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l.as_str().unwrap().contains("polyline_override")));
}

#[test]
fn unknown_xno_rejected() {
    let (_dir, db) = fixture_db();
    let (code, _) = run_show_route(&db, "999");
    assert_eq!(code, 1);
}

/// One move-stop run: the payload carries the whole invocation; `apply`
/// overrides the payload's `apply` flag. Exit 0 ok, 1 rejected, 2 no-op.
fn run_move_stop(db: &Path, mut payload: serde_json::Value, apply: bool) -> (i32, Report) {
    payload["apply"] = serde_json::Value::Bool(apply);
    run(Cli {
        command: Command::MoveStop {
            stop_version_id: None,
            xno: None,
            code: None,
            lat: None,
            lon: None,
            stop_name: None,
            db: db.to_path_buf(),
            editor: "test".to_string(),
            note: None,
            apply,
            json: Some(payload.to_string()),
        },
    })
}

fn run_undo(edit_id: i64, db: &Path) -> (i32, Report) {
    run(Cli {
        command: Command::Undo {
            edit_id: Some(edit_id),
            db: db.to_path_buf(),
            editor: "test".to_string(),
            json: None,
        },
    })
}

/// Every user table's full contents, name-then-rowid order — the
/// byte-identical read-only check (cli.rs's dump helper, same shape).
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

/// Excluded from the round-trip dump: the audit rows (the undo writes its
/// own by design), the stale marks (the undo deletes the job's own), the id
/// allocator (apply mints ids; ids are never reused), and `stop_version` —
/// a stop-edit undo keeps the closed minted version as history
/// (`apply_stop_undo` UPDATEs valid_to, deletes nothing; only add-stop's
/// undo removes its rows), so the byte-identical gate runs without it and
/// the restored current row is asserted explicitly instead.
const ROUNDTRIP_SKIP: &[&str] = &["edit_log", "stale_mark", "id_sequence", "stop_version"];

#[test]
fn move_stop_dry_run_then_apply_and_undo() {
    let (_dir, db) = fixture_db();
    let svi = find_stop_ok(&db, "1", Some("Old"), None)["candidates"][0]["stop_version_id"]
        .as_i64()
        .unwrap();
    let b0 = dump_at(&db, &[]);
    let (code, report) = run_move_stop(
        &db,
        serde_json::json!({"stop_version_id": svi, "lat": 24.15, "db": db.display().to_string()}),
        false,
    );
    assert_eq!(code, 0, "{}", report.out);
    assert_eq!(dump_at(&db, &[]), b0, "dry run must not write");
    let d = envelope_of(&report)["data"].clone();
    assert_eq!(d["changes"]["lat"], 24.15);

    let b0 = dump_at(&db, ROUNDTRIP_SKIP);
    let (code, report) = run_move_stop(
        &db,
        serde_json::json!({"stop_version_id": svi, "lat": 24.15, "db": db.display().to_string(), "apply": true}),
        true,
    );
    assert_eq!(code, 0, "{}", report.out);
    let edit_id = envelope_of(&report)["data"]["edit_id"].as_i64().unwrap();
    assert!(edit_id > 0);

    let (code, report) = run_undo(edit_id, &db);
    assert_eq!(code, 0, "{}{}", report.out, report.err);
    assert_eq!(
        dump_at(&db, ROUNDTRIP_SKIP),
        b0,
        "apply then undo is byte-identical (sans retained version history)"
    );
    // the version history the dump above excludes: the predecessor is
    // current again, the minted version stays closed
    let conn = rm_db::open(&db).unwrap();
    let (cur_svi, cur_lat): (i64, f64) = conn
        .query_row(
            "SELECT stop_version_id, latitude FROM stop_version \
             WHERE stop_id = 1 AND valid_to IS NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (cur_svi, cur_lat),
        (10, 24.0),
        "undo restores the predecessor"
    );
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM stop_version WHERE stop_id = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 2, "the closed minted version is kept as history");
}

#[test]
fn move_stop_by_xno_code_ambiguous_rejected() {
    let (_dir, db) = fixture_db();
    let (code, report) = run_move_stop(
        &db,
        serde_json::json!({"xno": "1", "code": "不存在", "lat": 24.15, "db": db.display().to_string()}),
        false,
    );
    assert_eq!(code, 1);
    assert!(report.err.contains("resolve"), "{}", report.err);
}

/// Resolution happy path: xno+code (no stop_version_id) resolves the
/// fixture's only pole, and the dry run previews it without writing.
#[test]
fn move_stop_resolves_by_xno_code_and_previews() {
    let (_dir, db) = fixture_db();
    let b0 = dump_at(&db, &[]);
    let (code, report) = run_move_stop(
        &db,
        serde_json::json!({"xno": "1", "code": "001", "lat": 24.15, "db": db.display().to_string()}),
        false,
    );
    assert_eq!(code, 0, "{}{}", report.out, report.err);
    assert_eq!(dump_at(&db, &[]), b0, "dry run must not write");
    let d = envelope_of(&report)["data"].clone();
    assert_eq!(
        d["stop_version_id"], 10,
        "resolution picked the fixture pole"
    );
    assert_eq!(d["stop_id"], 1);
    assert_eq!(d["changes"]["lat"], 24.15);
}

/// One remove-stop run: the payload carries the whole invocation; flags
/// are ignored when `--json` is given.
fn run_remove_stop(db: &Path, payload: serde_json::Value) -> (i32, Report) {
    run(Cli {
        command: Command::RemoveStop {
            xno: None,
            direction: None,
            code: None,
            sequence: None,
            all: false,
            delete_pole: false,
            db: db.to_path_buf(),
            editor: "test".to_string(),
            note: None,
            apply: false,
            json: Some(payload.to_string()),
        },
    })
}

/// Serve the fixture's pole (stop_version 10, code '001') from dir 1 as
/// well: a detach from dir 0 then does not orphan the pole, so apply is
/// undoable and the one-way warning has its non-orphan trigger. The
/// explicit rsv id moves the allocator past it (apply mints rsv rows).
fn seed_second_direction(db: &Path) {
    let conn = rm_db::open(db).unwrap();
    conn.execute_batch(
        "INSERT INTO route_stop_version
             (route_stop_version_id, subroute_version_id, stop_version_id, direction, stop_sequence, created_at)
         VALUES (2, 1, 10, 1, 1, '2026-01-01T00:00:00Z');
         UPDATE id_sequence SET next_value = 3 WHERE table_name = 'route_stop_version';",
    )
    .unwrap();
}

#[test]
fn remove_stop_dry_run_reports_plan_then_apply_roundtrips() {
    let (_dir, db) = fixture_db();
    seed_second_direction(&db);
    // resolve the code on route "1" via find-stop: the fixture's stop "Old"
    let code_on_route = find_stop_ok(&db, "1", Some("Old"), None)["candidates"][0]["stop_code"]
        .as_str()
        .unwrap()
        .to_string();
    let b0 = dump_at(&db, &[]);
    let (code, report) = run_remove_stop(
        &db,
        serde_json::json!({"xno":"1","direction":0,"code":code_on_route,"db":db.display().to_string()}),
    );
    assert_eq!(code, 0, "{}", report.out);
    assert_eq!(dump_at(&db, &[]), b0, "dry run must not write");
    let d = &envelope_of(&report)["data"];
    assert_eq!(d["occurrences"].as_array().unwrap().len(), 1);

    let b0 = dump_at(&db, ROUNDTRIP_SKIP);
    let (code, report) = run_remove_stop(
        &db,
        serde_json::json!({"xno":"1","direction":0,"code":code_on_route,"db":db.display().to_string(),"apply":true}),
    );
    assert_eq!(code, 0, "{}", report.out);
    let eid = envelope_of(&report)["data"]["edit_id"].as_i64().unwrap();
    run_undo(eid, &db);
    assert_eq!(
        dump_at(&db, ROUNDTRIP_SKIP),
        b0,
        "apply+undo byte-identical (B2==B0)"
    );
}

/// The one-way `--delete-pole` warning must reach the envelope: dry run
/// with the flag on a pole still served by another direction (the merged
/// primitive's warning fires exactly there) — status ok, never a write.
#[test]
fn remove_stop_dry_run_warns_one_way_delete_pole() {
    let (_dir, db) = fixture_db();
    seed_second_direction(&db);
    let b0 = dump_at(&db, &[]);
    let (code, report) = run_remove_stop(
        &db,
        serde_json::json!({"xno":"1","direction":0,"code":"001","delete_pole":true,"db":db.display().to_string()}),
    );
    assert_eq!(code, 0, "{}", report.out);
    assert_eq!(dump_at(&db, &[]), b0, "dry run must not write");
    let d = envelope_of(&report)["data"].clone();
    assert_eq!(d["pole_orphan"], false, "{}", report.out);
    assert!(
        d["warnings"].as_array().unwrap().iter().any(|w| w
            .as_str()
            .is_some_and(|w| w.contains("one-way") && w.contains("not undoable"))),
        "{}",
        report.out
    );
}

/// The fixture's code twice in ONE direction (loop shape): no
/// disambiguator — exit 1, stderr says how many times the code occurs.
#[test]
fn remove_stop_duplicate_needs_sequence() {
    let (_dir, db) = fixture_db();
    {
        let conn = rm_db::open(&db).unwrap();
        conn.execute_batch(
            "INSERT INTO stop (stop_id, ext_uid, created_at) VALUES (2, 'tcg:stop:2', '2026-01-01T00:00:00Z');
             INSERT INTO stop_version (stop_version_id, stop_id, station_id, stop_code, stop_name, direction, latitude, longitude, valid_from, created_at)
                 VALUES (12, 2, 1, '001', 'Old 同碼', 0, 24.1, 120.1, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
             INSERT INTO route_stop_version (route_stop_version_id, subroute_version_id, stop_version_id, direction, stop_sequence, created_at)
                 VALUES (2, 1, 12, 0, 2, '2026-01-01T00:00:00Z');",
        )
        .unwrap();
    }
    let (code, report) = run_remove_stop(
        &db,
        serde_json::json!({"xno":"1","direction":0,"code":"001","db":db.display().to_string()}),
    );
    assert_eq!(code, 1);
    assert!(report.err.contains("occurs 2 times"), "{}", report.err);
}

/// A second pole sharing the code (other direction) makes xno+code
/// ambiguous — rejected with a "resolve" reason naming both candidate ids.
#[test]
fn move_stop_by_xno_code_two_matches_rejected() {
    let (_dir, db) = fixture_db();
    {
        let conn = rm_db::open(&db).unwrap();
        conn.execute_batch(
            "INSERT INTO stop (stop_id, ext_uid, created_at) VALUES (2, 'tcg:stop:2', '2026-01-01T00:00:00Z');
             INSERT INTO stop_version (stop_version_id, stop_id, station_id, stop_code, stop_name, direction, latitude, longitude, valid_from, created_at)
                 VALUES (12, 2, 1, '001', 'Old 往', 1, 24.1, 120.1, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
             INSERT INTO route_stop_version (route_stop_version_id, subroute_version_id, stop_version_id, direction, stop_sequence, created_at)
                 VALUES (2, 1, 12, 1, 1, '2026-01-01T00:00:00Z');",
        )
        .unwrap();
    }
    let (code, report) = run_move_stop(
        &db,
        serde_json::json!({"xno": "1", "code": "001", "lat": 24.15, "db": db.display().to_string()}),
        false,
    );
    assert_eq!(code, 1);
    assert!(report.err.contains("resolve"), "{}", report.err);
    assert!(
        report.err.contains("10") && report.err.contains("12"),
        "reason names both candidates: {}",
        report.err
    );
}
