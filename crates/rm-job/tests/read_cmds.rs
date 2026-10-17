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
