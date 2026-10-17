//! rm-job extend-path: the outward walk over the OSM road graph and its
//! whole-line polyline-override write — planned through
//! `rm_job::extend::plan_extension` and driven end-to-end through
//! `rm_job::run` with a buffered report, no process capture.

use std::path::{Path, PathBuf};

use rm_core::{geo, polyline, LatLng};
use rm_job::{extend, run, Cli, Command, Report};
use rusqlite::Connection;

/// The stdout envelope, parsed — every run's stdout must be exactly one
/// JSON object.
fn envelope_of(report: &Report) -> serde_json::Value {
    serde_json::from_str(&report.out)
        .unwrap_or_else(|e| panic!("stdout not JSON: {e}: {}", report.out))
}

const T0: &str = "2026-01-01T00:00:00Z";

/// read_cmds.rs's chain (station_group → station → stop_version → route →
/// route_version → subroute_version → route_stop_version) plus the
/// id_sequence seeds and batch 1 CLOSED; the src polyline for dir 0 is the
/// caller's geometry (encoded).
fn seed_route(conn: &Connection, dir0_encoded: &str) {
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
             VALUES (1, '1', 0, '{dir0_encoded}');"
    ))
    .unwrap();
}

/// A straight N-S way (100) of four nodes at 0.0001-deg lat steps (~11.1 m
/// per segment), INSERT style mirroring rm-db matched_path's test setup.
/// The main polyline's last vertex sits exactly on node 1.
fn seed_line_graph(conn: &Connection) {
    conn.execute_batch(
        "INSERT INTO osm_way (osm_way_id, highway, oneway, name)
             VALUES (100, 'residential', 0, '直線路');
         INSERT INTO osm_node (osm_node_id, lat, lon) VALUES
            (1, 24.1400, 120.6800),
            (2, 24.1401, 120.6800),
            (3, 24.1402, 120.6800),
            (4, 24.1403, 120.6800);
         INSERT INTO osm_way_node (osm_way_id, ordinal, osm_node_id) VALUES
            (100, 1, 1), (100, 2, 2), (100, 3, 3), (100, 4, 4);",
    )
    .unwrap();
}

/// The main fixture: route + a src polyline (24.1399,120.68) → node 1 of
/// the straight way, so the "last" end extends due north along way 100.
fn line_db() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let conn = rm_db::open(&db).unwrap();
    let encoded = polyline::encode(&[
        LatLng::new(24.1399, 120.6800),
        LatLng::new(24.1400, 120.6800),
    ]);
    seed_route(&conn, &encoded);
    seed_line_graph(&conn);
    (dir, db)
}

/// The fork fixture: the path arrives due north at junction node 2; the
/// straight way (100) continues north in 11.1 m hops while a fork way (401)
/// branches at ~45° with a longer first hop. Extending 20 m must follow the
/// straight way through the junction — the fork never enters way_ids.
fn fork_db() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let conn = rm_db::open(&db).unwrap();
    let encoded = polyline::encode(&[
        LatLng::new(24.1400, 120.6800),
        LatLng::new(24.1401, 120.6800),
    ]);
    seed_route(&conn, &encoded);
    conn.execute_batch(
        "INSERT INTO osm_way (osm_way_id, highway, oneway, name) VALUES
            (300, 'residential', 0, '進站路'),
            (100, 'residential', 0, '直線路'),
            (401, 'residential', 0, '岔路');
         INSERT INTO osm_node (osm_node_id, lat, lon) VALUES
            (1, 24.1400000, 120.6800000),
            (2, 24.1401000, 120.6800000),
            (3, 24.1402000, 120.6800000),
            (4, 24.1403000, 120.6800000),
            (5, 24.1401762, 120.6800835);
         INSERT INTO osm_way_node (osm_way_id, ordinal, osm_node_id) VALUES
            (300, 1, 1), (300, 2, 2),
            (100, 1, 2), (100, 2, 3), (100, 3, 4),
            (401, 1, 2), (401, 2, 5);",
    )
    .unwrap();
    (dir, db)
}

/// Route + src polyline but zero osm rows — `RoadGraph::from_conn`'s own
/// "run rm-osm-prepare" error must surface verbatim.
fn graphless_db() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let conn = rm_db::open(&db).unwrap();
    let encoded = polyline::encode(&[
        LatLng::new(24.1399, 120.6800),
        LatLng::new(24.1400, 120.6800),
    ]);
    seed_route(&conn, &encoded);
    (dir, db)
}

fn run_extend_flags(
    db: &Path,
    xno: &str,
    direction: i64,
    meters: f64,
    end: &str,
    apply: bool,
) -> (i32, Report) {
    run(Cli {
        command: Command::ExtendPath {
            xno: Some(xno.to_string()),
            direction: Some(direction),
            meters,
            end: end.to_string(),
            db: db.to_path_buf(),
            editor: "test".to_string(),
            note: None,
            apply,
            json: None,
        },
    })
}

/// One extend-path run: the payload carries the whole invocation; flags are
/// ignored when `--json` is given.
fn run_extend_json(db: &Path, payload: serde_json::Value) -> (i32, Report) {
    run(Cli {
        command: Command::ExtendPath {
            xno: None,
            direction: None,
            meters: 10.0,
            end: "both".to_string(),
            db: db.to_path_buf(),
            editor: "test".to_string(),
            note: None,
            apply: false,
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
/// byte-identical read-only check (read_cmds.rs's dump helper, same shape).
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

/// Excluded from the apply+undo round-trip dump: the audit rows (the undo
/// writes its own by design), the stale marks, and the id allocator.
const ROUNDTRIP_SKIP: &[&str] = &["edit_log", "stale_mark", "id_sequence"];

#[test]
fn extend_appends_vertices_along_the_way() {
    let (_dir, db) = line_db();
    let conn = rm_db::open_readonly(&db).unwrap();
    let original = [
        LatLng::new(24.1399, 120.6800),
        LatLng::new(24.1400, 120.6800),
    ];
    let mut logs = Vec::new();
    let (candidate, exts, source) =
        extend::plan_extension(&conn, "1", 0, 10.0, "last", &mut logs).unwrap();
    assert_eq!(source, "src");
    assert_eq!(exts.len(), 1);
    assert_eq!(exts[0].end, "last");
    // the original vertices are a prefix of the candidate
    assert_eq!(candidate[..original.len()], original[..]);
    // first new vertex ≈ 10 m beyond the old end, due north along the way
    let v = exts[0].new_vertices.first().unwrap();
    let d = geo::haversine_m(original[1], LatLng::new(v[0], v[1]));
    assert!((9.5..11.0).contains(&d), "first new vertex {d:.3} m out");
    assert!(
        v[0] > original[1].lat && (v[1] - original[1].lon).abs() < 1e-9,
        "extension runs due north: {v:?}"
    );
    assert_eq!(exts[0].way_ids, vec![100]);
    assert!(exts[0].snap_dist_m < 1e-9, "endpoint sits on the way");

    // "both" plans the two ends in order: first (nothing outward — its only
    // edge doubles back under the path) then last (the walked extension)
    let (_c2, exts2, _s2) = extend::plan_extension(&conn, "1", 0, 10.0, "both", &mut logs).unwrap();
    assert_eq!(exts2.len(), 2);
    assert_eq!(exts2[0].end, "first");
    assert!(exts2[0].new_vertices.is_empty());
    assert_eq!(exts2[1].end, "last");
    assert!(!exts2[1].new_vertices.is_empty());
}

#[test]
fn dry_run_prints_way_ids_and_writes_nothing() {
    let (_dir, db) = line_db();
    let b0 = dump_at(&db, &[]);
    let (code, report) = run_extend_flags(&db, "1", 0, 10.0, "last", false);
    assert_eq!(code, 0, "{}{}", report.out, report.err);
    assert_eq!(dump_at(&db, &[]), b0, "dry run must not write");
    let d = envelope_of(&report)["data"].clone();
    assert_eq!(d["source"], "src");
    assert!(
        !d["extensions"][0]["way_ids"].as_array().unwrap().is_empty(),
        "{}",
        report.out
    );
}

#[test]
fn apply_creates_override_and_undo_removes_it() {
    let (_dir, db) = line_db();
    let b0 = dump_at(&db, ROUNDTRIP_SKIP);
    let (code, report) = run_extend_json(
        &db,
        serde_json::json!({
            "xno": "1", "direction": 0, "meters": 10.0, "end": "last",
            "db": db.display().to_string(), "apply": true,
        }),
    );
    assert_eq!(code, 0, "{}{}", report.out, report.err);
    let d = envelope_of(&report)["data"].clone();
    assert_eq!(d["created"], true, "{}", report.out);
    let edit_id = d["edit_id"].as_i64().unwrap();
    assert!(d["extensions"].as_array().unwrap().len() == 1);
    {
        let conn = rm_db::open(&db).unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM polyline_override WHERE xno = '1' AND direction = 0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "override row written");
    }
    let (code, report) = run_undo(edit_id, &db);
    assert_eq!(code, 0, "{}{}", report.out, report.err);
    {
        let conn = rm_db::open(&db).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM polyline_override", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "undo removes the override");
    }
    assert_eq!(
        dump_at(&db, ROUNDTRIP_SKIP),
        b0,
        "apply+undo is byte-identical (B2==B0)"
    );
}

#[test]
fn fork_takes_straightest_continuation() {
    let (_dir, db) = fork_db();
    let conn = rm_db::open_readonly(&db).unwrap();
    let mut logs = Vec::new();
    let (_candidate, exts, _source) =
        extend::plan_extension(&conn, "1", 0, 20.0, "last", &mut logs).unwrap();
    let ext = &exts[0];
    assert!(
        ext.way_ids.contains(&100),
        "straight continuation (way 100) taken: {:?}",
        ext.way_ids
    );
    assert!(
        !ext.way_ids.contains(&401),
        "the 45° fork (way 401) rejected: {:?}",
        ext.way_ids
    );
    assert!(
        !ext.way_ids.contains(&300),
        "the arrival way (way 300) is not re-walked: {:?}",
        ext.way_ids
    );
    // the final vertex lands at the 20 m mark due north of the junction
    let last = ext.new_vertices.last().unwrap();
    let d = geo::haversine_m(
        LatLng::new(24.1401, 120.6800),
        LatLng::new(last[0], last[1]),
    );
    assert!((19.5..20.5).contains(&d), "final vertex {d:.3} m out");
    assert!(last[0] > 24.1401, "extension runs due north: {last:?}");
}

#[test]
fn endpoint_far_from_graph_rejected() {
    let (_dir, db) = line_db();
    {
        // dir 1 polyline ends ~2.2 km north of the graph
        let conn = rm_db::open(&db).unwrap();
        conn.execute(
            "INSERT INTO src_route_point (batch_id, xno, direction, encoded)
             VALUES (1, '1', 1, ?1)",
            [polyline::encode(&[
                LatLng::new(24.1399, 120.6800),
                LatLng::new(24.1600, 120.6800),
            ])],
        )
        .unwrap();
    }
    let (code, report) = run_extend_flags(&db, "1", 1, 10.0, "last", false);
    assert_eq!(code, 1);
    assert!(
        report.err.contains("50"),
        "names the snap limit: {}",
        report.err
    );
}

#[test]
fn empty_osm_tables_rejected_with_prepare_hint() {
    let (_dir, db) = graphless_db();
    let (code, report) = run_extend_flags(&db, "1", 0, 10.0, "last", false);
    assert_eq!(code, 1);
    assert!(
        report.err.contains("rm-osm-prepare"),
        "prepare hint surfaced verbatim: {}",
        report.err
    );
}
