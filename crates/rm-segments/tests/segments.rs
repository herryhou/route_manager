//! End-to-end segments tests over the shared fixture.

use rm_core::polyline;
use rm_db::open_in_memory;
use rm_segments::{geometry, run};

/// fixture DB: staging + identities + published V1, ready for segments.
fn setup() -> (tempfile::TempDir, rusqlite::Connection, i64) {
    let dir = tempfile::tempdir().unwrap();
    rm_ingest::testfix::make_fixture(&dir.path().join("src"));
    let mut conn = open_in_memory().unwrap();
    let s = rm_ingest::run(&conn, &dir.path().join("src")).unwrap();
    rm_identity::run(&conn, s.batch_id).unwrap();
    rm_publish::run(&mut conn, s.batch_id).unwrap();
    (dir, conn, s.batch_id)
}

/// chainage + method for one stop, keyed by stop ext_uid.
fn projection_of(conn: &rusqlite::Connection, stop_uid: &str) -> (f64, f64, String, i64) {
    conn.query_row(
        "SELECT p.chainage_m, p.snap_dist_m, p.method, p.monotonic_clamped
         FROM path_stop_projection p
         JOIN stop_of_route_version sorv ON sorv.stop_of_route_version_id = p.stop_of_route_version_id
         JOIN stop_version sv ON sv.stop_version_id = sorv.stop_version_id
         JOIN stop s ON s.stop_id = sv.stop_id
         WHERE s.ext_uid = ?1",
        [stop_uid],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )
    .unwrap()
}

fn count(conn: &rusqlite::Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn fixture_degenerate_paths_are_endpoint_and_no_path() {
    let (_dir, conn, batch) = setup();
    let s = run(&conn, batch, 50.0).unwrap();

    // 3 directions total: route1/dir0 (1-pt polyline), route1/dir1 (''),
    // route2/dir0 (''); 4 stops across them
    assert_eq!(s.directions, 3);
    assert_eq!(s.projections, 4);
    assert_eq!(s.endpoint, 1);
    assert_eq!(s.no_path, 2);
    assert_eq!(s.outliers, 0);
    assert_eq!(s.clamped, 0);

    // route1/dir0: two stops onto a single-point path => ENDPOINT, chainage 0
    for uid in ["tcg:1:0:11", "tcg:1:0:12"] {
        let (ch, _snap, method, clamped) = projection_of(&conn, uid);
        assert_eq!(method, "ENDPOINT", "{uid}");
        assert!(ch.abs() < 1e-9, "{uid} chainage {ch}");
        assert_eq!(clamped, 0);
    }
    // route1/dir1 and route2/dir0: '' => NO_PATH
    let (_, _, m1, _) = projection_of(&conn, "tcg:1:1:13");
    assert_eq!(m1, "NO_PATH");
    let (_, _, m2, _) = projection_of(&conn, "tcg:2:0:21");
    assert_eq!(m2, "NO_PATH");

    // exactly one draft: route1/dir0 between its two stops, straight line
    // (fixture nodes 44/46 sit ~28 m apart)
    assert_eq!(s.draft_rows, 1);
    let (enc, len, gm): (String, f64, String) = conn
        .query_row(
            "SELECT encoded_polyline, length_m, geometry_method FROM seg_geometry_draft",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(gm, "straight");
    assert!((20.0..=40.0).contains(&len), "len {len}");
    assert_eq!(polyline::decode(&enc).unwrap().len(), 2);
}

#[test]
fn crafted_path_projects_and_cuts_from_polyline() {
    let (_dir, mut conn, batch) = setup();
    // route 2 dir 0: give it the 3-point reference path and two extra stops,
    // one exactly on p[1] (40.7,-120.95), one ~14 m from p[0]
    conn.execute(
        "UPDATE src_route_point SET encoded = ?1 WHERE batch_id = ?2 AND xno = '2' AND direction = 0",
        rusqlite::params![geometry::REF_PATH_ENC, batch],
    )
    .unwrap();
    let extra: [(i64, i64, &str, f64, f64); 3] = [
        (5i64, 21i64, "44", 24.1408, 120.6859), // existing node 44 far away => OUTLIER
        (6, 22, "99", 40.7, -120.95),           // exactly on p[1] => PROJECTED
        (7, 23, "98", 38.5001, -120.2001),      // near p[0] => PROJECTED, chainage regresses
    ];
    for (order, sid, node, lat, lon) in extra {
        conn.execute(
            "INSERT OR REPLACE INTO src_stop_edge
                (batch_id, xno, direction, source_order_no, ivr_no, sid, node_id, station_name, lat, lon, lat_e6, lon_e6)
             VALUES (?1, '2', 0, ?2, NULL, ?3, ?4, '測試站', ?5, ?6, ?7, ?8)",
            rusqlite::params![
                batch, order, sid, node, lat, lon,
                (lat * 1e6).round() as i64,
                (lon * 1e6).round() as i64
            ],
        )
        .unwrap();
    }
    // wipe identity rows for route 2 so re-run mints new stops (fixture stops)
    conn.execute_batch(
        "UPDATE route SET current_route_version_id = NULL, revision = 0 WHERE ext_uid = 'tcg:2';
         DELETE FROM stop_of_route_version WHERE subroute_version_id IN
            (SELECT subroute_version_id FROM subroute_version WHERE route_version_id =
                (SELECT route_version_id FROM route_version WHERE route_id =
                    (SELECT route_id FROM route WHERE ext_uid = 'tcg:2')));
         DELETE FROM subroute_version WHERE route_version_id IN
            (SELECT route_version_id FROM route_version WHERE route_id =
                (SELECT route_id FROM route WHERE ext_uid = 'tcg:2'));
         DELETE FROM route_version WHERE route_id IN
            (SELECT route_id FROM route WHERE ext_uid = 'tcg:2');
         DELETE FROM stop_version WHERE stop_id IN
            (SELECT stop_id FROM stop WHERE ext_uid LIKE 'tcg:2:%');
         DELETE FROM stop WHERE ext_uid LIKE 'tcg:2:%';",
    )
    .unwrap();
    rm_identity::run(&conn, batch).unwrap();
    rm_publish::run(&mut conn, batch).unwrap();

    let s = run(&conn, batch, 50.0).unwrap();
    assert_eq!(s.outliers, 1);
    assert_eq!(s.clamped, 1);
    // draft: route1/dir0 straight + route2/dir0 path-cut x2
    assert_eq!(s.draft_rows, 3);

    // node 99 exactly on p[1]: projected with ~zero snap at cum[1]
    let path = polyline::decode(geometry::REF_PATH_ENC).unwrap();
    let cum = geometry::cumulative_m(&path);
    let (ch99, snap99, m99, _) = projection_of(&conn, "tcg:2:0:22");
    assert_eq!(m99, "PROJECTED");
    assert!(snap99 < 1.0, "snap {snap99}");
    assert!(
        (ch99 - cum[1]).abs() < 5.0,
        "chainage {ch99} want ~{}",
        cum[1]
    );

    // node 44 (Taiwan) far off the path => OUTLIER
    let (_, snap44, m44, _) = projection_of(&conn, "tcg:2:0:21");
    assert_eq!(m44, "OUTLIER");
    assert!(snap44 > 1_000_000.0, "snap {snap44}");

    // node 98 near p[0] comes after node 99 in sequence => clamped to cum[1]
    let (ch98, _, m98, clamped98) = projection_of(&conn, "tcg:2:0:23");
    assert_eq!(m98, "PROJECTED");
    assert_eq!(clamped98, 1);
    assert!(
        (ch98 - cum[1]).abs() < 1e-9,
        "clamped chainage {ch98} vs {}",
        cum[1]
    );

    // drafts for route2/dir0: ordinal 1 (stop21->22) and 2 (stop22->23)
    let rows: Vec<(i64, String, f64)> = {
        let mut stmt = conn
            .prepare(
                "SELECT ordinal, geometry_method, length_m FROM seg_geometry_draft
                 WHERE subroute_version_id IN (
                     SELECT sv.subroute_version_id FROM subroute_version sv
                     JOIN route_version rv ON rv.route_version_id = sv.route_version_id
                     WHERE rv.route_id = (SELECT route_id FROM route WHERE ext_uid = 'tcg:2')
                 ) ORDER BY ordinal",
            )
            .unwrap();
        let it = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap();
        it.collect::<Result<_, _>>().unwrap()
    };
    assert_eq!(rows.len(), 2);
    let (o1, gm1, l1) = &rows[0];
    let (o2, gm2, l2) = &rows[1];
    assert_eq!((*o1, gm1.as_str()), (1, "path"));
    assert_eq!((*o2, gm2.as_str()), (2, "path"));
    // stop21 (Taiwan) projects to the p[1] vertex (t clamps to 1 on leg 0):
    // same chainage as stop22 => zero-width draft, no clamp flag (equal, not <)
    assert!(*l1 < 1.0, "draft1 len {l1}");
    // clamped stop98 shares stop22's chainage => zero-length draft
    assert!(l2.abs() < 1.0, "draft2 len {l2}");
}

#[test]
fn rerun_is_idempotent() {
    let (_dir, conn, batch) = setup();
    run(&conn, batch, 50.0).unwrap();
    let before = (
        count(&conn, "SELECT COUNT(*) FROM path_stop_projection"),
        count(&conn, "SELECT COUNT(*) FROM seg_geometry_draft"),
    );
    let s = run(&conn, batch, 50.0).unwrap();
    let after = (
        count(&conn, "SELECT COUNT(*) FROM path_stop_projection"),
        count(&conn, "SELECT COUNT(*) FROM seg_geometry_draft"),
    );
    assert_eq!(before, after);
    assert_eq!(s.projections as i64, before.0);
    assert_eq!(s.draft_rows as i64, before.1);
}
