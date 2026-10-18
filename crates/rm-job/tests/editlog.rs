//! rm-job edit-log: the read-only audit-log query behind the `edit-log`
//! command — newest-first rows and the empty case.

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

/// cli.rs's chain — enough for two move-stop applies writing audit rows.
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

fn fixture_db() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let conn = rm_db::open(&db).unwrap();
    fixture(&conn);
    (dir, db)
}

fn move_stop_cli(
    stop_version_id: Option<i64>,
    xno: Option<&str>,
    code: Option<&str>,
    lat: f64,
    db: &Path,
    apply: bool,
) -> Cli {
    Cli {
        command: Command::MoveStop {
            stop_version_id,
            xno: xno.map(str::to_string),
            code: code.map(str::to_string),
            lat: Some(lat),
            lon: None,
            stop_name: None,
            db: db.to_path_buf(),
            editor: "test".to_string(),
            note: None,
            apply,
            json: None,
        },
    }
}

fn edit_log_cli(entity: &str, entity_id: i64, db: &Path) -> Cli {
    Cli {
        command: Command::EditLog {
            entity: Some(entity.to_string()),
            entity_id: Some(entity_id),
            db: db.to_path_buf(),
            json: None,
        },
    }
}

#[test]
fn edit_log_lists_stop_rows_newest_first() {
    let (_dir, db) = fixture_db();
    // two moves on the same pole → two 'update' audit rows; the second
    // move resolves by xno+code because the first minted a new version
    let (code, report) = run(move_stop_cli(Some(10), None, None, 24.1, &db, true));
    assert_eq!(code, 0, "{}{}", report.out, report.err);
    let (code, report) = run(move_stop_cli(None, Some("1"), Some("001"), 24.2, &db, true));
    assert_eq!(code, 0, "{}{}", report.out, report.err);
    let (code, report) = run(edit_log_cli("stop", 1, &db));
    assert_eq!(code, 0, "{}{}", report.out, report.err);
    let rows = envelope_of(&report)["data"]["rows"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(rows.len(), 2, "{}", report.out);
    assert_eq!(rows[0]["action"], "update");
    assert_eq!(rows[0]["editor"], "test");
    assert!(
        rows[0]["edit_id"].as_i64().unwrap() > rows[1]["edit_id"].as_i64().unwrap(),
        "newest first: {}",
        report.out
    );
    // the newest row carries the second move's coordinates
    assert!(
        rows[0]["changes"].as_str().unwrap().contains("24.2"),
        "{}",
        report.out
    );
}

#[test]
fn edit_log_empty_rows_ok() {
    let (_dir, db) = fixture_db();
    let (code, report) = run(edit_log_cli("station", 999, &db));
    assert_eq!(code, 0, "{}{}", report.out, report.err);
    let rows = envelope_of(&report)["data"]["rows"]
        .as_array()
        .unwrap()
        .clone();
    assert!(rows.is_empty(), "{}", report.out);
}
