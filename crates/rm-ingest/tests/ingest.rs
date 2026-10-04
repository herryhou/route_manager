//! Fixture-driven ingest tests. Fixtures mirror the real capture shapes
//! documented in assets/Taichung/Taichung_API.md.

use std::fs;
use std::path::Path;

use rm_db::open_in_memory;
use rm_ingest::run;

fn write(dir: &Path, rel: &str, content: &str) {
    let p = dir.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}

fn make_fixture(dir: &Path) {
    write(
        dir,
        "routes.json",
        r#"{"data":{"routes":{"edges":[
            {"node":{"id":"1","name":"1","seq":1,"departure":"A","destination":"B","description":"A - B","isCycled":true,"opType":0,
                "providers":{"edges":[{"node":{"id":"5","name":"台中客運","telephone":"04-2222"}}]}}},
            {"node":{"id":"2","name":"2","seq":2,"departure":"C","destination":"D","description":"C - D","isCycled":false,"opType":0,
                "providers":{"edges":[]}}},
            {"node":{"id":"3","name":"3","seq":3,"departure":"E","destination":"F","description":"E - F","isCycled":false,"opType":0,
                "providers":{"edges":[]}}}
        ]}}}"#,
    );
    write(
        dir,
        "providers.json",
        r#"{"data":{"providers":{"edges":[
            {"node":{"id":"5","name":"台中客運","telephone":"04-2222","opType":0}},
            {"node":{"id":"6","name":"統聯客運","telephone":"04-3333","opType":0}}
        ]}}}"#,
    );
    write(
        dir,
        "main-side-routes.json",
        r#"[{"xno":1,"sideRoutes":"2,999","children":[2,999],"_id":"m1","createdAt":"2026-01-01T00:00:00Z","updatedAt":"2026-01-01T00:00:00Z","__v":0}]"#,
    );
    write(
        dir,
        "jointOperationsBuses.json",
        r#"{"data":{"jointOperationsBuses":[{"busProvider":"Ubus","providerId":6,"routeId":2}]}}"#,
    );
    // route 1: both directions; go polyline present, back missing (null)
    write(
        dir,
        "raw/route_1.json",
        r#"{"data":{"route":{
            "stations":{"edges":[
                {"goBack":1,"orderNo":1,"ivrNo":101,"sid":11,"node":{"id":"44","name":"干城站","lat":24.1409,"lon":120.6857}},
                {"goBack":1,"orderNo":2,"ivrNo":102,"sid":12,"node":{"id":"46","name":"干城站","lat":24.1408,"lon":120.6859}},
                {"goBack":2,"orderNo":1,"ivrNo":201,"sid":13,"node":{"id":"46","name":"干城站","lat":24.1408,"lon":120.6859}}
            ]},
            "routePoint":{"go":"_p~iF~ps|U","back":null},
            "providers":{"edges":[{"node":{"id":"5","name":"台中客運","opType":0}}]}
        }}}"#,
    );
    // route 2: single direction, no routePoint key at all
    write(
        dir,
        "raw/route_2.json",
        r#"{"data":{"route":{
            "stations":{"edges":[
                {"goBack":1,"orderNo":5,"ivrNo":null,"sid":21,"node":{"id":"44","name":"干城站","lat":24.1409,"lon":120.6857}}
            ]},
            "routePoint":null,
            "providers":{"edges":[]}
        }}}"#,
    );
    // header route "3" has no raw capture -> missing-detail warning
}

fn count(conn: &rusqlite::Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn ingests_fixture_counts_and_direction_mapping() {
    let dir = tempfile::tempdir().unwrap();
    make_fixture(&dir.path().join("src"));
    let conn = open_in_memory().unwrap();
    let s = run(&conn, &dir.path().join("src")).unwrap();

    assert_eq!(s.routes, 3, "3 route headers");
    assert_eq!(s.stop_edges, 4, "3+1 stop edges");
    assert_eq!(s.route_points, 4, "2 routes x 2 directions");
    assert_eq!(s.providers, 2);
    assert_eq!(s.route_providers, 1, "only route 1 has providers");
    assert_eq!(s.main_side_links, 2, "1->2 and 1->999 stored as-is");
    assert_eq!(s.joint_operations, 1);

    // warnings: side ref 999 dangling + route 3 header without raw detail
    assert_eq!(
        s.warnings.len(),
        2,
        "warnings: {warnings}",
        warnings = s.warnings.join("; ")
    );

    // goBack 1 -> direction 0, orderNo kept as source_order_no
    let (dir0, son, lat_e6): (i64, i64, i64) = conn
        .query_row(
            "SELECT direction, source_order_no, lat_e6 FROM src_stop_edge
             WHERE batch_id = ?1 AND xno = '1' AND sid = 11",
            [s.batch_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!((dir0, son), (0, 1));
    assert_eq!(lat_e6, 24140900);

    // go polyline (去程 = direction 0) verbatim; back (direction 1) '' when null
    let (go, back): (String, String) = conn
        .query_row(
            "SELECT (SELECT encoded FROM src_route_point WHERE batch_id = ?1 AND xno = '1' AND direction = 0),
                    (SELECT encoded FROM src_route_point WHERE batch_id = ?1 AND xno = '1' AND direction = 1)",
            [s.batch_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(go, "_p~iF~ps|U");
    assert_eq!(back, "");

    // batch is OPEN, src_file audit rows exist with sha256
    let (status, files): (String, i64) = conn
        .query_row(
            "SELECT (SELECT status FROM import_batch WHERE batch_id = ?1),
                    (SELECT COUNT(*) FROM src_file WHERE batch_id = ?1)",
            [s.batch_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((status.as_str(), files), ("OPEN", 6));
}

#[test]
fn station_name_trimmed_into_staging() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    make_fixture(&src);
    // the provider occasionally emits surrounding whitespace on station
    // names; grouping compares names exactly, so trim at the staging door
    write(
        &src,
        "raw/route_1.json",
        r#"{"data":{"route":{
            "stations":{"edges":[
                {"goBack":1,"orderNo":1,"ivrNo":101,"sid":11,"node":{"id":"44","name":" 干城站 ","lat":24.1409,"lon":120.6857}},
                {"goBack":1,"orderNo":2,"ivrNo":102,"sid":12,"node":{"id":"46","name":"干城站","lat":24.1408,"lon":120.6859}},
                {"goBack":2,"orderNo":1,"ivrNo":201,"sid":13,"node":{"id":"46","name":"干城站","lat":24.1408,"lon":120.6859}}
            ]},
            "routePoint":{"go":"_p~iF~ps|U","back":null},
            "providers":{"edges":[]}
        }}}"#,
    );
    let conn = open_in_memory().unwrap();
    run(&conn, &src).unwrap();

    let names: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT station_name FROM src_stop_edge
                 WHERE batch_id = (SELECT MAX(batch_id) FROM import_batch)
                   AND xno = '1'
                 ORDER BY sid",
            )
            .unwrap();
        let it = stmt
            .query_map([], |r| r.get(0))
            .unwrap();
        it.collect::<Result<_, _>>().unwrap()
    };
    assert_eq!(names.len(), 3);
    assert!(
        names.iter().all(|n| n == "干城站"),
        "names must land trimmed: {names:?}"
    );
}

#[test]
fn rerun_same_source_is_idempotent_same_batch() {
    let dir = tempfile::tempdir().unwrap();
    make_fixture(&dir.path().join("src"));
    let conn = open_in_memory().unwrap();
    let a = run(&conn, &dir.path().join("src")).unwrap();
    let b = run(&conn, &dir.path().join("src")).unwrap();
    assert_eq!(
        a.batch_id, b.batch_id,
        "reuse OPEN batch for same source_dir"
    );
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM src_stop_edge"),
        4,
        "no duplicate rows"
    );
}

#[test]
fn bad_json_fails_without_writing_and_marks_reused_batch_failed() {
    let dir = tempfile::tempdir().unwrap();
    make_fixture(&dir.path().join("src"));
    let conn = open_in_memory().unwrap();
    let first = run(&conn, &dir.path().join("src")).unwrap();

    // corrupt one raw file
    fs::write(dir.path().join("src/raw/route_1.json"), "{not json").unwrap();
    let err = run(&conn, &dir.path().join("src"));
    assert!(err.is_err());
    let status: String = conn
        .query_row(
            "SELECT status FROM import_batch WHERE batch_id = ?1",
            [first.batch_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(status, "FAILED");
    // staging still holds the first good run
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM src_stop_edge"), 4);
}

#[test]
fn new_open_batch_for_different_source_dir() {
    let dir = tempfile::tempdir().unwrap();
    make_fixture(&dir.path().join("one"));
    make_fixture(&dir.path().join("two"));
    let conn = open_in_memory().unwrap();
    let a = run(&conn, &dir.path().join("one")).unwrap();
    let b = run(&conn, &dir.path().join("two")).unwrap();
    assert_ne!(a.batch_id, b.batch_id);
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM src_stop_edge"),
        8,
        "both batches kept"
    );
}
