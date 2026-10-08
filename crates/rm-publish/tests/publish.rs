//! End-to-end publish tests over the shared fixture.

use rm_db::open_in_memory;
use rm_publish::run;

/// fixture DB with staging + identities ready for publish.
fn setup() -> (tempfile::TempDir, rusqlite::Connection, i64) {
    let dir = tempfile::tempdir().unwrap();
    rm_ingest::testfix::make_fixture(&dir.path().join("src"));
    let conn = open_in_memory().unwrap();
    let s = rm_ingest::run(&conn, &dir.path().join("src")).unwrap();
    rm_identity::run(&conn, s.batch_id).unwrap();
    (dir, conn, s.batch_id)
}

fn count(conn: &rusqlite::Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn publishes_all_routes_atomically_with_pointer_and_outbox() {
    let (_dir, mut conn, batch) = setup();
    let s = run(&mut conn, batch).unwrap();
    assert_eq!(s.published, 2);
    assert_eq!(s.failed, 0);
    assert_eq!(s.route_stop_rows, 4, "3 + 1 stop rows");

    // route_version V1 PUBLISHED per route, pointer + revision
    let rows: Vec<(String, i64, String, Option<i64>, i64)> = {
        let mut stmt = conn
            .prepare(
                "SELECT r.ext_uid, rv.version_no, rv.status, r.current_route_version_id, r.revision
                 FROM route r JOIN route_version rv ON rv.route_version_id = r.current_route_version_id
                 ORDER BY r.ext_uid",
            )
            .unwrap();
        let it = stmt
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .unwrap();
        it.collect::<Result<_, _>>().unwrap()
    };
    assert_eq!(rows.len(), 2);
    for (uid, vno, status, ptr, rev) in &rows {
        assert_eq!(vno, &1);
        assert_eq!(status, "PUBLISHED");
        assert!(ptr.is_some());
        assert_eq!(rev, &1);
        assert!(uid.starts_with("tcg:"));
    }

    // subroute_version: one per route_version, name = header description
    let (name, cnt): (String, i64) = conn
        .query_row(
            "SELECT sv.subroute_name, COUNT(*) FROM subroute_version sv
             JOIN route_version rv ON rv.route_version_id = sv.route_version_id
             JOIN route r ON r.route_id = rv.route_id
             WHERE r.ext_uid = 'tcg:1' GROUP BY sv.subroute_name",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((name.as_str(), cnt), ("A - B", 1));

    // route_stop: renumbered 1..N per direction, source_order_no kept
    let rows: Vec<(i64, i64, Option<i64>)> = {
        let mut stmt = conn
            .prepare(
                "SELECT rsv.direction, rsv.stop_sequence, rsv.source_order_no
                 FROM route_stop_version rsv
                 JOIN subroute_version sv ON sv.subroute_version_id = rsv.subroute_version_id
                 JOIN route_version rv ON rv.route_version_id = sv.route_version_id
                 JOIN route r ON r.route_id = rv.route_id
                 WHERE r.ext_uid = 'tcg:1'
                 ORDER BY rsv.direction, rsv.stop_sequence",
            )
            .unwrap();
        let it = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap();
        it.collect::<Result<_, _>>().unwrap()
    };
    assert_eq!(
        rows,
        vec![(0, 1, Some(1)), (0, 2, Some(2)), (1, 1, Some(1))]
    );

    // route 2: orderNo 5 renumbered to 1
    let (seq, src): (i64, Option<i64>) = conn
        .query_row(
            "SELECT rsv.stop_sequence, rsv.source_order_no
             FROM route_stop_version rsv
             JOIN subroute_version sv ON sv.subroute_version_id = rsv.subroute_version_id
             JOIN route_version rv ON rv.route_version_id = sv.route_version_id
             JOIN route r ON r.route_id = rv.route_id
             WHERE r.ext_uid = 'tcg:2'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((seq, src), (1, Some(5)));

    // outbox: one route.published event per route, payload parses
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM outbox_event WHERE event_type='route.published'"
        ),
        2
    );
    let payload: String = conn
        .query_row("SELECT payload FROM outbox_event LIMIT 1", [], |r| r.get(0))
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert!(v["route_version_id"].is_i64());
    assert!(v["stops"].is_i64());
}

#[test]
fn rerun_skips_already_published() {
    let (_dir, mut conn, batch) = setup();
    run(&mut conn, batch).unwrap();
    let s2 = run(&mut conn, batch).unwrap();
    assert_eq!(s2.published, 0);
    assert_eq!(s2.skipped, 2);
    assert_eq!(
        count(&conn, "SELECT COUNT(*) FROM route_version"),
        2,
        "no V2 created"
    );
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM outbox_event"), 2);
}

#[test]
fn header_without_stops_fails_alone_others_publish() {
    let (_dir, mut conn, batch) = setup();
    // add a route header with no raw detail (identity minted, no stops)
    conn.execute(
        "INSERT INTO src_route_header (batch_id, xno, route_code, departure, destination, description, seq, is_cycled, op_type)
         VALUES (?1, '9', '9', 'X', 'Y', 'X - Y', 9, 0, 0)",
        [batch],
    )
    .unwrap();
    conn.execute("INSERT INTO route (route_id, route_code, ext_uid, revision, created_at, updated_at) VALUES (900, '9', 'tcg:9', 0, '2026-10-02T00:00:00Z', '2026-10-02T00:00:00Z')", []).unwrap();
    conn.execute("INSERT INTO subroute (subroute_id, route_id, subroute_code, created_at) VALUES (901, 900, '9', '2026-10-02T00:00:00Z')", []).unwrap();

    let s = run(&mut conn, batch).unwrap();
    assert_eq!(s.published, 2);
    assert_eq!(s.failed, 1);
    assert!(s.failures[0].0 == "9");
    assert!(s.failures[0].1.contains("no stops"), "{}", s.failures[0].1);

    // failed route untouched: no version, pointer still NULL
    let (cnt, ptr): (i64, Option<i64>) = conn
        .query_row("SELECT (SELECT COUNT(*) FROM route_version WHERE route_id=900), current_route_version_id FROM route WHERE route_id=900", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_eq!((cnt, ptr), (0, None));
}

#[test]
fn cyclic_route_same_stop_twice_keeps_both_rows() {
    // one-shot import: stage the extra loop pole BEFORE the single identity run
    let dir = tempfile::tempdir().unwrap();
    rm_ingest::testfix::make_fixture(&dir.path().join("src"));
    let mut conn = open_in_memory().unwrap();
    let s = rm_ingest::run(&conn, &dir.path().join("src")).unwrap();
    let batch = s.batch_id;
    // loop pole: sid 13 visited twice in route 1 dir 1
    conn.execute(
        "INSERT INTO src_stop_edge (batch_id, xno, direction, source_order_no, ivr_no, sid, node_id, station_name, lat, lon, lat_e6, lon_e6)
         VALUES (?1, '1', 1, 2, NULL, 13, '46', '干城站', 24.1408, 120.6859, 24140800, 120685900)",
        [batch],
    )
    .unwrap();
    rm_identity::run(&conn, batch).unwrap();
    let s = run(&mut conn, batch).unwrap();
    assert_eq!(s.published, 2);
    assert_eq!(s.route_stop_rows, 5);

    // two rows, same stop_version_id, sequences 1 & 2
    let rows: Vec<(i64, i64)> = {
        let mut stmt = conn
            .prepare(
                "SELECT rsv.stop_sequence, COUNT(DISTINCT rsv.stop_version_id)
                 FROM route_stop_version rsv
                 JOIN subroute_version sv ON sv.subroute_version_id = rsv.subroute_version_id
                 JOIN route_version rv ON rv.route_version_id = sv.route_version_id
                 JOIN route r ON r.route_id = rv.route_id
                 WHERE r.ext_uid = 'tcg:1' AND rsv.direction = 1
                 GROUP BY rsv.stop_sequence ORDER BY rsv.stop_sequence",
            )
            .unwrap();
        let it = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        it.collect::<Result<_, _>>().unwrap()
    };
    assert_eq!(
        rows,
        vec![(1, 1), (2, 1)],
        "both sequences, one distinct stop each"
    );
    let same_stop: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM (SELECT stop_version_id FROM route_stop_version rsv
             JOIN subroute_version sv ON sv.subroute_version_id = rsv.subroute_version_id
             JOIN route_version rv ON rv.route_version_id = sv.route_version_id
             JOIN route r ON r.route_id = rv.route_id
             WHERE r.ext_uid='tcg:1' AND rsv.direction=1
             GROUP BY stop_version_id HAVING COUNT(*)=2)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(same_stop, 1, "same stop visited twice (cyclic)");
}

#[test]
fn route_stop_sequence_is_contiguous_per_direction() {
    let (_dir, mut conn, batch) = setup();
    run(&mut conn, batch).unwrap();
    let gaps: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM (
                SELECT rsv.subroute_version_id, rsv.direction, rsv.stop_sequence
                FROM route_stop_version rsv
                JOIN (
                    SELECT subroute_version_id, direction, COUNT(*) AS n, MIN(stop_sequence) AS mn, MAX(stop_sequence) AS mx
                    FROM route_stop_version GROUP BY subroute_version_id, direction
                ) t ON t.subroute_version_id = rsv.subroute_version_id AND t.direction = rsv.direction
                GROUP BY rsv.subroute_version_id, rsv.direction
                HAVING COUNT(DISTINCT rsv.stop_sequence) != t.n OR t.mn != 1 OR t.mx != t.n
             )",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(gaps, 0, "1..N contiguous, no dups (spec §10)");
}
