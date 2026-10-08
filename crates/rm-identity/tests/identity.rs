//! End-to-end identity tests over an ingested fixture (shares ingest's shapes).

use std::fs;
use std::path::Path;

use rm_db::open_in_memory;
use rm_identity::run;

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
                "providers":{"edges":[]}}}
        ]}}}"#,
    );
    write(
        dir,
        "providers.json",
        r#"{"data":{"providers":{"edges":[
            {"node":{"id":"5","name":"台中客運","telephone":"04-2222","opType":0}}
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
        r#"{"data":{"jointOperationsBuses":[]}}"#,
    );
    // nodes 44 & 46: same name, 28 m apart -> one group
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
}

fn setup() -> (tempfile::TempDir, rusqlite::Connection, i64) {
    let dir = tempfile::tempdir().unwrap();
    make_fixture(&dir.path().join("src"));
    let conn = open_in_memory().unwrap();
    let s = rm_ingest::run(&conn, &dir.path().join("src")).unwrap();
    (dir, conn, s.batch_id)
}

fn count(conn: &rusqlite::Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn mints_identities_and_v1_versions() {
    let (_dir, conn, batch) = setup();
    let s = run(&conn, batch).unwrap();

    assert_eq!(s.routes, 2);
    assert_eq!(s.subroutes, 2);
    assert_eq!(s.stations, 2);
    assert_eq!(s.station_groups, 1, "44+46 same name 28m -> one group");
    assert_eq!(s.stops, 4, "(1,0,11) (1,0,12) (1,1,13) (2,0,21)");
    assert_eq!(s.route_links, 1, "1->2 kept, 999 dangling dropped");
    assert_eq!(s.route_providers, 1);
    assert!(s.warnings.is_empty(), "{:?}", s.warnings);

    // station_version carries group membership + batch valid_from
    let (grp_cnt, valid): (i64, String) = conn
        .query_row(
            "SELECT (SELECT COUNT(DISTINCT station_group_id) FROM station_version),
                    (SELECT valid_from FROM import_batch WHERE batch_id = ?1)",
            [batch],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(grp_cnt, 1);
    assert!(!valid.is_empty());

    // stop_version: stop_code = sid, direction mapped, station FK resolved
    let (code, dirn, station_cnt): (String, i64, i64) = conn
        .query_row(
            "SELECT sv.stop_code, sv.direction, COUNT(DISTINCT sv.station_id)
             FROM stop_version sv
             WHERE sv.stop_id = (SELECT stop_id FROM stop WHERE ext_uid = 'tcg:1:0:11')",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!((code.as_str(), dirn), ("11", 0));
    assert_eq!(station_cnt, 1);

    // route identity: current pointer NULL, revision 0, code from header name
    let (cur, rev, code): (Option<i64>, i64, String) = conn
        .query_row(
            "SELECT current_route_version_id, revision, route_code FROM route WHERE ext_uid='tcg:1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!((cur, rev, code.as_str()), (None, 0, "1"));

    // subroute identity: one per route
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM subroute"), 2);
}

#[test]
fn rerun_is_idempotent_no_new_ids_or_versions() {
    let (_dir, conn, batch) = setup();
    run(&conn, batch).unwrap();
    let stops_before = count(&conn, "SELECT COUNT(*) FROM stop");
    let versions_before = count(&conn, "SELECT COUNT(*) FROM stop_version");
    // one-shot: a second pass over the same capture means a fresh DB with a
    // fresh OPEN batch; identity must produce the same shapes, no more.
    let (_dir2, conn2, batch2) = setup();
    let s2 = run(&conn2, batch2).unwrap();
    assert_eq!(s2.stops, 4);
    assert_eq!(count(&conn2, "SELECT COUNT(*) FROM stop"), stops_before);
    assert_eq!(
        count(&conn2, "SELECT COUNT(*) FROM stop_version"),
        versions_before
    );
    assert_eq!(count(&conn2, "SELECT COUNT(*) FROM station"), 2);
    assert_eq!(count(&conn2, "SELECT COUNT(*) FROM station_group"), 1);
    assert_eq!(count(&conn2, "SELECT COUNT(*) FROM route"), 2);
}

#[test]
fn ambiguous_sid_to_two_nodes_first_wins_with_warning() {
    let (_dir, conn, batch) = setup();
    // sid 13 already maps to node 46; add another row same (route,dir,sid) -> node 44
    conn.execute(
        "INSERT INTO src_stop_edge (batch_id, xno, direction, source_order_no, ivr_no, sid, node_id, station_name, lat, lon, lat_e6, lon_e6)
         VALUES (?1, '1', 1, 2, NULL, 13, '44', '干城站', 24.1409, 120.6857, 24140900, 120685700)",
        [batch],
    )
    .unwrap();
    let s = run(&conn, batch).unwrap();
    assert_eq!(s.stops, 4, "sid 13 still one stop");
    assert!(
        s.warnings.iter().any(|w| w.contains("sid=13")),
        "{:?}",
        s.warnings
    );
    // first occurrence by order_no: node 46
    let station: String = conn
        .query_row(
            "SELECT st.ext_uid FROM stop_version sv
             JOIN stop so ON so.stop_id = sv.stop_id
             JOIN station st ON st.station_id = sv.station_id
             WHERE so.ext_uid = 'tcg:1:1:13'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(station, "tcg:node:46", "first occurrence (order 1) wins");
}

#[test]
fn group_name_is_member_station_name() {
    let (_dir, conn, batch) = setup();
    run(&conn, batch).unwrap();
    let (name, source): (String, String) = conn
        .query_row(
            "SELECT station_group_name, source FROM station_group_version",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(name, "干城站");
    assert_eq!(source, "derived:same-name<80m");
}
