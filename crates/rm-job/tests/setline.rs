//! rm-job set-line: whole-line override create/update, the
//! `override_updated_at` concurrency-token loop, and the parse/route
//! guards — all driven through `rm_job::run`.

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

/// Route 1 '1' with a current version — enough for the override table; the
/// src capture may be absent (the plan reports `replaces: "src"` either way
/// when no override exists).
fn fixture(conn: &Connection) {
    conn.execute_batch(&format!(
        "INSERT INTO route (route_id, route_code, ext_uid, revision, created_at, updated_at)
             VALUES (1, '1', 'tcg:1', 1, '{T0}', '{T0}');
         INSERT INTO route_version (route_version_id, route_id, version_no, valid_from, status, created_at)
             VALUES (1, 1, 1, '{T0}', 'PUBLISHED', '{T0}');
         UPDATE route SET current_route_version_id = 1 WHERE route_id = 1;"
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

fn set_line_cli(
    xno: &str,
    direction: i64,
    points: &[&str],
    token: Option<&str>,
    db: &Path,
    apply: bool,
) -> Cli {
    Cli {
        command: Command::SetLine {
            xno: Some(xno.to_string()),
            direction: Some(direction),
            point: points.iter().map(|s| s.to_string()).collect(),
            expected_updated_at: token.map(str::to_string),
            db: db.to_path_buf(),
            editor: "test".to_string(),
            note: None,
            apply,
            json: None,
        },
    }
}

/// Every user table's full contents, name-then-rowid order — the
/// byte-identical read-only check.
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

const PTS: &[&str] = &["24.140,120.685", "24.141,120.686", "24.142,120.687"];
/// A different polyline — the overwrite legs must actually change the line
/// or the primitive rejects NoOpEdit ("sets no field to a new value").
const PTS2: &[&str] = &["24.150,120.690", "24.151,120.691"];

#[test]
fn set_line_create_dry_run_then_apply() {
    let (_dir, db) = fixture_db();
    let before = dump_at(&db);
    // dry: create shape, no token yet, writes nothing
    let (code, report) = run(set_line_cli("1", 0, PTS, None, &db, false));
    assert_eq!(code, 0, "{}{}", report.out, report.err);
    let v = envelope_of(&report)["data"].clone();
    assert_eq!(v["route_id"], 1);
    assert_eq!(v["route_version_id"], 1);
    assert_eq!(v["replaces"], "src");
    assert_eq!(v["created"], true);
    assert_eq!(v["points"], 3);
    assert!(v["override_updated_at"].is_null(), "{}", report.out);
    let encoded = v["encoded"].as_str().unwrap().to_string();
    assert!(!encoded.is_empty());
    assert_eq!(dump_at(&db), before, "dry run must not write");
    // apply: the override row lands with the same encoded payload
    let (code, report) = run(set_line_cli("1", 0, PTS, None, &db, true));
    assert_eq!(code, 0, "{}{}", report.out, report.err);
    let v = envelope_of(&report)["data"].clone();
    assert_eq!(v["created"], true);
    assert_eq!(v["points"], 3);
    assert!(v["edit_id"].as_i64().unwrap() > 0);
    assert!(!v["updated_at"].as_str().unwrap().is_empty());
    let conn = rm_db::open(&db).unwrap();
    let (row_encoded, updated_at): (String, String) = conn
        .query_row(
            "SELECT encoded, updated_at FROM polyline_override WHERE xno = '1' AND direction = 0",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(row_encoded, encoded, "override row matches the plan");
    assert_eq!(updated_at, v["updated_at"].as_str().unwrap());
}

#[test]
fn set_line_update_requires_token_and_reports_it() {
    let (_dir, db) = fixture_db();
    // create the first override
    let (code, report) = run(set_line_cli("1", 0, PTS, None, &db, true));
    assert_eq!(code, 0, "{}{}", report.out, report.err);
    let token = envelope_of(&report)["data"]["updated_at"]
        .as_str()
        .unwrap()
        .to_string();
    // the overwrite dry run surfaces the token and the override source —
    // the token guard is the same on the dry leg (the plan mirrors apply)
    let (code, report) = run(set_line_cli("1", 0, PTS2, Some(&token), &db, false));
    assert_eq!(code, 0, "{}{}", report.out, report.err);
    let v = envelope_of(&report)["data"].clone();
    assert_eq!(v["replaces"], "override");
    assert_eq!(v["created"], false);
    assert_eq!(v["override_updated_at"], token.as_str());
    // applying the overwrite WITHOUT the token rejects as a lost race
    let (code, report) = run(set_line_cli("1", 0, PTS2, None, &db, true));
    assert_eq!(code, 1, "{}{}", report.out, report.err);
    assert!(
        report.err.contains("edited by someone else"),
        "{}",
        report.err
    );
    // with the token the overwrite lands and reports its new token —
    // updated_at is second-resolution, so a same-second overwrite can
    // carry the same string; only its presence is asserted here
    let (code, report) = run(set_line_cli("1", 0, PTS2, Some(&token), &db, true));
    assert_eq!(code, 0, "{}{}", report.out, report.err);
    let v = envelope_of(&report)["data"].clone();
    assert_eq!(v["created"], false);
    assert!(
        !v["updated_at"].as_str().unwrap().is_empty(),
        "{}",
        report.out
    );
}

#[test]
fn set_line_rejects_single_point_bad_coords_unknown_route() {
    // single point: the primitive needs at least two
    let (_dir, db) = fixture_db();
    let (code, report) = run(set_line_cli("1", 0, &PTS[..1], None, &db, false));
    assert_eq!(code, 1, "{}{}", report.out, report.err);
    assert!(report.err.contains("invalid polyline"), "{}", report.err);
    // bad coords: parse before open — must not create the db
    let fresh = tempfile::tempdir().unwrap();
    let missing = fresh.path().join("t.db");
    let (code, report) = run(set_line_cli("1", 0, &["24.1"], None, &missing, false));
    assert_eq!(code, 1, "{}{}", report.out, report.err);
    assert!(report.err.contains("--point"), "{}", report.err);
    assert!(!missing.exists(), "rejected parse must not create the db");
    // unknown route
    let (code, report) = run(set_line_cli("99", 0, PTS, None, &db, false));
    assert_eq!(code, 1, "{}{}", report.out, report.err);
    assert!(
        report.err.contains("route 99 does not exist"),
        "{}",
        report.err
    );
}

#[test]
fn set_line_identical_resave_noop() {
    let (_dir, db) = fixture_db();
    let (code, report) = run(set_line_cli("1", 0, PTS, None, &db, true));
    assert_eq!(code, 0, "{}{}", report.out, report.err);
    let token = envelope_of(&report)["data"]["updated_at"]
        .as_str()
        .unwrap()
        .to_string();
    // resaving the identical polyline changes nothing → rejected (exit 1)
    let (code, report) = run(set_line_cli("1", 0, PTS, Some(&token), &db, true));
    assert_eq!(code, 1, "{}{}", report.out, report.err);
    assert!(report.err.contains("sets no field"), "{}", report.err);
}
