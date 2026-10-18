//! rm-job station-level commands: regroup-stop, rename-station,
//! move-station — dry-run/apply/undo round-trips, resolution and guards,
//! all driven through `rm_job::run` with a buffered report.

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

/// cli.rs's chain plus a second station (id 2, svi 20 'T') and a second
/// pole named after the station ('S 往') — the rename spread's discriminator
/// — plus the id_sequence seeds the applies mint from.
fn fixture(conn: &Connection) {
    conn.execute_batch(&format!(
        "INSERT INTO station_group (station_group_id, created_at) VALUES (1, '{T0}');
         INSERT INTO station_group_version (station_group_version_id, station_group_id, station_group_name, source, valid_from, created_at)
             VALUES (1, 1, 'g', 'capture', '{T0}', '{T0}');
         INSERT INTO station (station_id, ext_uid, created_at) VALUES (1, 'tcg:node:1', '{T0}');
         INSERT INTO station_version (station_version_id, station_id, station_group_id, station_name, latitude, longitude, valid_from, created_at)
             VALUES (1, 1, 1, 'S', 24.0, 120.0, '{T0}', '{T0}');
         INSERT INTO station (station_id, ext_uid, created_at) VALUES (2, 'tcg:node:2', '{T0}');
         INSERT INTO station_version (station_version_id, station_id, station_group_id, station_name, latitude, longitude, valid_from, created_at)
             VALUES (20, 2, 1, 'T', 24.1, 120.1, '{T0}', '{T0}');
         INSERT INTO stop (stop_id, ext_uid, created_at) VALUES (1, 'tcg:stop:1', '{T0}');
         INSERT INTO stop_version (stop_version_id, stop_id, station_id, stop_code, stop_name, direction, latitude, longitude, valid_from, created_at)
             VALUES (10, 1, 1, '001', 'S', 0, 24.0, 120.0, '{T0}', '{T0}');
         INSERT INTO stop (stop_id, ext_uid, created_at) VALUES (2, 'tcg:stop:2', '{T0}');
         INSERT INTO stop_version (stop_version_id, stop_id, station_id, stop_code, stop_name, direction, latitude, longitude, valid_from, created_at)
             VALUES (11, 2, 1, '002', 'S 往', 0, 24.0, 120.2, '{T0}', '{T0}');
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
             ('station', 3), ('station_version', 21),
             ('stop', 3), ('stop_version', 12),
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

fn regroup_cli(
    stop_version_id: Option<i64>,
    xno: Option<&str>,
    code: Option<&str>,
    station: i64,
    db: &Path,
    apply: bool,
) -> Cli {
    Cli {
        command: Command::RegroupStop {
            stop_version_id,
            xno: xno.map(str::to_string),
            code: code.map(str::to_string),
            station: Some(station),
            db: db.to_path_buf(),
            editor: "test".to_string(),
            note: None,
            apply,
            json: None,
        },
    }
}

fn rename_cli(station: i64, new_name: &str, level: &str, db: &Path, apply: bool) -> Cli {
    Cli {
        command: Command::RenameStation {
            station: Some(station),
            new_name: Some(new_name.to_string()),
            level: level.to_string(),
            db: db.to_path_buf(),
            editor: "test".to_string(),
            note: None,
            apply,
            json: None,
        },
    }
}

fn move_station_cli(station: i64, lat: f64, lon: f64, db: &Path, apply: bool) -> Cli {
    Cli {
        command: Command::MoveStation {
            station: Some(station),
            lat: Some(lat),
            lon: Some(lon),
            db: db.to_path_buf(),
            editor: "test".to_string(),
            note: None,
            apply,
            json: None,
        },
    }
}

fn undo_cli(edit_id: i64, db: &Path) -> Cli {
    Cli {
        command: Command::Undo {
            edit_id: Some(edit_id),
            db: db.to_path_buf(),
            editor: "test".to_string(),
            json: None,
        },
    }
}

/// Every user table's full contents, name-then-rowid order — the
/// byte-identical read-only check.
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

/// The audit rows (undo writes its own), the stale marks (regroup's are the
/// job's own) and the id allocator (ids are never reused).
const ROUNDTRIP_SKIP: &[&str] = &["edit_log", "stale_mark", "id_sequence"];

#[test]
fn regroup_stop_dry_run_then_apply_and_undo() {
    let (_dir, db) = fixture_db();
    let b0 = dump_at(&db, ROUNDTRIP_SKIP);
    // dry: reports both stations, writes nothing
    let (code, report) = run(regroup_cli(Some(10), None, None, 2, &db, false));
    assert_eq!(code, 0, "{}{}", report.out, report.err);
    let v = envelope_of(&report)["data"].clone();
    assert_eq!(v["stop_version_id"], 10);
    assert_eq!(v["old_station_id"], 1);
    assert_eq!(v["old_station_name"], "S");
    assert_eq!(v["new_station_id"], 2);
    assert_eq!(v["new_station_name"], "T");
    assert_eq!(dump_at(&db, ROUNDTRIP_SKIP), b0, "dry run must not write");
    // apply: the pole's current version lands on station 2
    let (code, report) = run(regroup_cli(Some(10), None, None, 2, &db, true));
    assert_eq!(code, 0, "{}{}", report.out, report.err);
    let v = envelope_of(&report)["data"].clone();
    let edit_id = v["edit_id"].as_i64().unwrap();
    assert!(v["new_stop_version_id"].as_i64().unwrap() > 10);
    assert_eq!(v["old_station_id"], 1);
    assert_eq!(v["new_station_id"], 2);
    let conn = rm_db::open(&db).unwrap();
    let (station, svi): (i64, i64) = conn
        .query_row(
            "SELECT station_id, stop_version_id FROM stop_version
             WHERE stop_id = 1 AND valid_to IS NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(station, 2, "pole now belongs to station 2");
    assert!(svi > 10, "apply minted a new version");
    drop(conn);
    // undo reopens the OLD version (tombstones for the minted one stay —
    // update-undo preserves history, unlike add-stop's delete-style undo)
    let (code, report) = run(undo_cli(edit_id, &db));
    assert_eq!(code, 0, "{}{}", report.out, report.err);
    let conn = rm_db::open(&db).unwrap();
    let (station, svi): (i64, i64) = conn
        .query_row(
            "SELECT station_id, stop_version_id FROM stop_version
             WHERE stop_id = 1 AND valid_to IS NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((station, svi), (1, 10), "undo restored the old version");
    drop(conn);
}

#[test]
fn regroup_stop_resolves_by_xno_code() {
    let (_dir, db) = fixture_db();
    let (code, report) = run(regroup_cli(None, Some("1"), Some("001"), 2, &db, true));
    assert_eq!(code, 0, "{}{}", report.out, report.err);
    let v = envelope_of(&report)["data"].clone();
    assert!(v["new_stop_version_id"].as_i64().unwrap() > 10);
}

#[test]
fn regroup_stop_rejects_unknown_station_and_same_station() {
    let (_dir, db) = fixture_db();
    // unknown station id
    let (code, report) = run(regroup_cli(Some(10), None, None, 999, &db, false));
    assert_eq!(code, 1, "{}{}", report.out, report.err);
    assert!(
        envelope_of(&report)["data"]["reason"]
            .as_str()
            .unwrap()
            .contains("999"),
        "{}",
        report.out
    );
    // regrouping onto the pole's current station is a no-op → exit 1
    let (code, report) = run(regroup_cli(Some(10), None, None, 1, &db, false));
    assert_eq!(code, 1, "{}{}", report.out, report.err);
    assert!(report.err.contains("sets no field"), "{}", report.err);
}

#[test]
fn rename_station_levels_and_dry_run_shape() {
    let (_dir, db) = fixture_db();
    let before = dump_at(&db, ROUNDTRIP_SKIP);
    // matching: only the pole byte-equal to the station name follows
    let (code, report) = run(rename_cli(1, "N", "matching", &db, false));
    assert_eq!(code, 0, "{}{}", report.out, report.err);
    let v = envelope_of(&report)["data"].clone();
    assert_eq!(v["station_id"], 1);
    assert_eq!(v["old_name"], "S");
    assert_eq!(v["new_name"], "N");
    assert_eq!(v["poles_follow"], 1);
    assert_eq!(v["poles"][0]["stop_version_id"], 10);
    assert_eq!(v["poles"][0]["old_name"], "S");
    assert_eq!(
        dump_at(&db, ROUNDTRIP_SKIP),
        before,
        "dry run must not write"
    );
    // all: both poles follow
    let (code, report) = run(rename_cli(1, "N", "all", &db, false));
    assert_eq!(code, 0, "{}{}", report.out, report.err);
    let v = envelope_of(&report)["data"].clone();
    assert_eq!(v["poles_follow"], 2);
    assert_eq!(v["poles"][1]["old_name"], "S 往");
}

#[test]
fn rename_station_apply_and_undo() {
    let (_dir, db) = fixture_db();
    let (code, report) = run(rename_cli(1, "N", "all", &db, true));
    assert_eq!(code, 0, "{}{}", report.out, report.err);
    let v = envelope_of(&report)["data"].clone();
    let edit_id = v["edit_id"].as_i64().unwrap();
    assert!(v["new_station_version_id"].as_i64().unwrap() > 1);
    assert_eq!(v["poles_follow"], 2);
    // both poles churned onto new versions with the new name
    let conn = rm_db::open(&db).unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM stop_version WHERE valid_to IS NULL AND stop_name = 'N'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 2, "both poles renamed");
    drop(conn);
    // undo reopens the old versions and closes the minted ones (history rows
    // stay — update-undo preserves history, unlike add-stop's delete-style undo)
    let (code, report) = run(undo_cli(edit_id, &db));
    assert_eq!(code, 0, "{}{}", report.out, report.err);
    let conn = rm_db::open(&db).unwrap();
    let (station_name, svi): (String, i64) = conn
        .query_row(
            "SELECT station_name, station_version_id FROM station_version
             WHERE station_id = 1 AND valid_to IS NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((station_name.as_str(), svi), ("S", 1), "station restored");
    let rows: Vec<(i64, String)> = conn
        .prepare(
            "SELECT stop_version_id, stop_name FROM stop_version
             WHERE valid_to IS NULL ORDER BY stop_version_id",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![(10, "S".into()), (11, "S 往".into())],
        "both poles restored to their old versions and names"
    );
    drop(conn);
}

#[test]
fn rename_station_rejects_bad_level() {
    // parse before open: a bad level must not create the db
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let (code, report) = run(rename_cli(1, "N", "bogus", &db, false));
    assert_eq!(code, 1, "{}{}", report.out, report.err);
    assert!(
        report.err.contains("expected station, matching or all"),
        "{}",
        report.err
    );
    assert!(!db.exists(), "rejected parse must not create the db");
}

#[test]
fn move_station_dry_run_apply_and_range_check() {
    let (_dir, db) = fixture_db();
    let b0 = dump_at(&db, ROUNDTRIP_SKIP);
    // dry: copies the snapshot, writes nothing
    let (code, report) = run(move_station_cli(1, 24.5, 120.5, &db, false));
    assert_eq!(code, 0, "{}{}", report.out, report.err);
    let v = envelope_of(&report)["data"].clone();
    assert_eq!(v["station_id"], 1);
    assert_eq!(v["station_name"], "S");
    assert_eq!(v["old_lat"], 24.0);
    assert_eq!(v["old_lon"], 120.0);
    assert_eq!(v["new_lat"], 24.5);
    assert_eq!(v["new_lon"], 120.5);
    assert_eq!(dump_at(&db, ROUNDTRIP_SKIP), b0, "dry run must not write");
    // apply: mints the next station_version (fixture seed → 21) with the
    // new coords
    let (code, report) = run(move_station_cli(1, 24.5, 120.5, &db, true));
    assert_eq!(code, 0, "{}{}", report.out, report.err);
    let v = envelope_of(&report)["data"].clone();
    assert_eq!(v["new_station_version_id"], 21);
    let conn = rm_db::open(&db).unwrap();
    let (lat, lon): (f64, f64) = conn
        .query_row(
            "SELECT latitude, longitude FROM station_version
             WHERE station_id = 1 AND valid_to IS NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((lat, lon), (24.5, 120.5));
    drop(conn);
    // range checks reject before the db opens
    let fresh = tempfile::tempdir().unwrap();
    let missing = fresh.path().join("t.db");
    let (code, report) = run(move_station_cli(1, 95.0, 120.0, &missing, false));
    assert_eq!(code, 1, "{}{}", report.out, report.err);
    assert!(report.err.contains("lat out of range"), "{}", report.err);
    let (code, report) = run(move_station_cli(1, 24.0, 200.0, &missing, false));
    assert_eq!(code, 1, "{}{}", report.out, report.err);
    assert!(report.err.contains("lon out of range"), "{}", report.err);
    assert!(!missing.exists(), "rejected range must not create the db");
}

#[test]
fn move_station_resolves_station_id_not_svi() {
    let (_dir, db) = fixture_db();
    // station 2's version id is 20 — addressing by station_id must work and
    // addressing by the version id must not
    let (code, report) = run(move_station_cli(2, 24.2, 120.2, &db, true));
    assert_eq!(code, 0, "{}{}", report.out, report.err);
    let v = envelope_of(&report)["data"].clone();
    assert_eq!(v["new_station_version_id"], 21);
    let (code, report) = run(move_station_cli(20, 24.2, 120.2, &db, false));
    assert_eq!(code, 1, "{}{}", report.out, report.err);
    assert!(
        report
            .err
            .contains("station 20 does not exist or is not current"),
        "{}",
        report.err
    );
}
