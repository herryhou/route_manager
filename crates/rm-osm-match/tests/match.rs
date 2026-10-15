//! End-to-end matching tests over the shared fixture plus a hand-built
//! OSM road graph.

use rm_core::{polyline, LatLng};
use rm_db::open_in_memory;

const D: f64 = 0.001; // ~111 m

fn setup() -> (tempfile::TempDir, rusqlite::Connection, i64) {
    let dir = tempfile::tempdir().unwrap();
    rm_ingest::testfix::make_fixture(&dir.path().join("src"));
    let mut conn = open_in_memory().unwrap();
    let s = rm_ingest::run(&conn, &dir.path().join("src")).unwrap();
    rm_identity::run(&conn, s.batch_id).unwrap();
    rm_publish::run(&mut conn, s.batch_id).unwrap();
    (dir, conn, s.batch_id)
}

/// one straight residential way 1-2-3 north of lat 24.14
fn insert_straight_road(conn: &rusqlite::Connection) {
    conn.execute_batch(
        "INSERT INTO osm_way (osm_way_id, highway, oneway, name) VALUES (100, 'residential', 0, '測試路');
         INSERT INTO osm_node (osm_node_id, lat, lon) VALUES
            (1, 24.1400, 120.6800),
            (2, 24.1400 + 0.001, 120.6800),
            (3, 24.1400 + 0.002, 120.6800);
         INSERT INTO osm_way_node (osm_way_id, ordinal, osm_node_id) VALUES
            (100, 1, 1), (100, 2, 2), (100, 3, 3);",
    )
    .unwrap();
}

fn straight_polyline() -> String {
    polyline::encode(&[
        LatLng {
            lat: 24.1400,
            lon: 120.6800,
        },
        LatLng {
            lat: 24.1400 + D / 2.0,
            lon: 120.6800,
        },
        LatLng {
            lat: 24.1400 + D,
            lon: 120.6800,
        },
        LatLng {
            lat: 24.1400 + 3.0 * D / 2.0,
            lon: 120.6800,
        },
        LatLng {
            lat: 24.1400 + 2.0 * D,
            lon: 120.6800,
        },
    ])
}

fn route2_ids(conn: &rusqlite::Connection) -> (i64, i64) {
    conn.query_row(
        "SELECT rv.route_version_id, sv.subroute_version_id
         FROM route r
         JOIN route_version rv ON rv.route_id = r.route_id AND rv.route_version_id = r.current_route_version_id
         JOIN subroute_version sv ON sv.route_version_id = rv.route_version_id
         WHERE r.ext_uid = 'tcg:2'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .unwrap()
}

#[test]
fn matches_direction_to_way_runs_and_stats() {
    let (_dir, conn, batch) = setup();
    insert_straight_road(&conn);
    conn.execute(
        "UPDATE src_route_point SET encoded = ?1 WHERE batch_id = ?2 AND xno = '2' AND direction = 0",
        rusqlite::params![straight_polyline(), batch],
    )
    .unwrap();

    let s = rm_osm_match::run(&conn, batch).unwrap();

    // 4 direction rows; only route2/dir0 has a real polyline (route2/dir1
    // exists as an empty polyline with no stops)
    assert_eq!(s.directions, 4, "{s:?}");
    assert_eq!(s.matched, 1, "{s:?}");
    assert_eq!(s.unmatched, 3, "{s:?}");
    assert_eq!(s.failed, 0, "{s:?}");

    let (rv_id, sv_id) = route2_ids(&conn);
    let run_row: (i64, i64, i64, i64, f64, f64) = conn
        .query_row(
            "SELECT ordinal, osm_way_id, entry_node_id, exit_node_id, cum_start_m, cum_end_m
             FROM seg_match_way WHERE subroute_version_id = ?1 AND direction = 0",
            [sv_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(run_row.0, 1, "single merged run");
    assert_eq!((run_row.1, run_row.2, run_row.3), (100, 1, 3));
    assert!(run_row.4 < 5.0, "cum_start {}", run_row.4);
    assert!((run_row.5 - 222.4).abs() < 5.0, "cum_end {}", run_row.5);

    let stat: (i64, f64, f64, f64, f64, i64, i64, f64, f64) = conn
        .query_row(
            "SELECT stops, path_length_m, matched_length_m, snap_dist_p50, snap_dist_max, outliers, gap_repairs,
                    dev_p50_m, dev_max_m
             FROM seg_match_stat WHERE route_version_id = ?1 AND direction = 0",
            [rv_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(stat.0, 1, "fixture route2/dir0 has one stop");
    assert!(stat.1 > 220.0, "path len {}", stat.1);
    assert!((stat.2 - stat.1).abs() < 10.0, "matched {}", stat.2);
    assert!(stat.3 < 1.0, "p50 {}", stat.3);
    assert!(stat.4 < 1.0, "max {}", stat.4);
    assert_eq!(stat.5, 0);
    assert_eq!(stat.6, 0);
    // path nodes lie on the straight polyline — deviation ~0
    assert!(stat.7 < 1.0, "dev_p50 {}", stat.7);
    assert!(stat.8 < 1.0, "dev_max {}", stat.8);

    // degenerate directions leave no way rows
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM seg_match_way WHERE subroute_version_id != ?1",
            [sv_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 0);
}

#[test]
fn rerun_is_idempotent() {
    let (_dir, conn, batch) = setup();
    insert_straight_road(&conn);
    conn.execute(
        "UPDATE src_route_point SET encoded = ?1 WHERE batch_id = ?2 AND xno = '2' AND direction = 0",
        rusqlite::params![straight_polyline(), batch],
    )
    .unwrap();
    rm_osm_match::run(&conn, batch).unwrap();
    let before: (i64, i64) = conn
        .query_row(
            "SELECT (SELECT COUNT(*) FROM seg_match_way), (SELECT COUNT(*) FROM seg_match_stat)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let s = rm_osm_match::run(&conn, batch).unwrap();
    let after: (i64, i64) = conn
        .query_row(
            "SELECT (SELECT COUNT(*) FROM seg_match_way), (SELECT COUNT(*) FROM seg_match_stat)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(before, after);
    assert_eq!(s.matched, 1);
}

#[test]
fn empty_osm_tables_is_an_error() {
    let (_dir, conn, batch) = setup();
    let err = rm_osm_match::run(&conn, batch).unwrap_err();
    assert!(err.to_string().contains("osm-prepare"), "{err}");
}

#[test]
fn full_run_consumes_override_over_src() {
    let (_dir, conn, batch) = setup();
    insert_straight_road(&conn);
    // kill the src polyline; the override is now the only geometry — if the
    // run still read src, route2/dir0 would land in `unmatched`
    conn.execute(
        "UPDATE src_route_point SET encoded = '' WHERE batch_id = ?1 AND xno = '2' AND direction = 0",
        rusqlite::params![batch],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO polyline_override (xno, direction, encoded, updated_at)
         VALUES ('2', 0, ?1, '2026-01-01T00:00:00Z')",
        rusqlite::params![straight_polyline()],
    )
    .unwrap();
    let s = rm_osm_match::run(&conn, batch).unwrap();
    assert_eq!(
        s.matched, 1,
        "override matched where empty src would not: {s:?}"
    );
}

fn route2_rid(conn: &rusqlite::Connection) -> i64 {
    conn.query_row(
        "SELECT route_id FROM route WHERE ext_uid = 'tcg:2'",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

fn seg_mark(conn: &rusqlite::Connection, rid: i64, dir: i64, key: Option<String>) {
    let key = key.unwrap_or(format!("{rid}:{dir}"));
    // cause_edit_id carries an FK into edit_log (foreign_keys=ON in rm-db) —
    // seed the cause row first
    conn.execute(
        "INSERT INTO edit_log (ts, editor, entity, entity_id, action, changes)
         VALUES ('2026-01-01T00:00:00Z', 'test', 'polyline', ?1, 'update', '{}')",
        rusqlite::params![rid],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO stale_mark (table_name, key, cause_edit_id, marked_at)
         VALUES ('seg_match', ?1, 1, '2026-01-01T00:00:00Z')",
        rusqlite::params![key],
    )
    .unwrap();
}

#[test]
fn stale_run_recomputes_marked_direction_and_clears() {
    let (_dir, conn, batch) = setup();
    insert_straight_road(&conn);
    conn.execute(
        "UPDATE src_route_point SET encoded = '' WHERE batch_id = ?1 AND xno = '2' AND direction = 0",
        rusqlite::params![batch],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO polyline_override (xno, direction, encoded, updated_at)
         VALUES ('2', 0, ?1, '2026-01-01T00:00:00Z')",
        rusqlite::params![straight_polyline()],
    )
    .unwrap();
    let s = rm_osm_match::run(&conn, batch).unwrap();
    assert_eq!(s.matched, 1, "{s:?}");
    let (ways_before, stats_before): (i64, i64) = conn
        .query_row(
            "SELECT (SELECT COUNT(*) FROM seg_match_way), (SELECT COUNT(*) FROM seg_match_stat)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(ways_before >= 1 && stats_before >= 1);

    seg_mark(&conn, route2_rid(&conn), 0, None);
    // the line moved on: garbage override → recompute drops route2's rows
    conn.execute(
        "UPDATE polyline_override SET encoded = 'zzzz' WHERE xno = '2' AND direction = 0",
        [],
    )
    .unwrap();
    let out = rm_osm_match::run_stale(&conn, batch).unwrap().unwrap();
    assert_eq!(out.cleared, 1);
    assert_eq!(out.kept, 0);
    assert_eq!(out.summary.directions, 1, "{:?}", out.summary);
    assert_eq!(
        out.summary.failed, 1,
        "garbage polyline fails decode: {:?}",
        out.summary
    );
    // route2's rows are gone (deleted, not replaced), nothing else touched
    let ways_after: i64 = conn
        .query_row("SELECT COUNT(*) FROM seg_match_way", [], |r| r.get(0))
        .unwrap();
    let stats_after: i64 = conn
        .query_row("SELECT COUNT(*) FROM seg_match_stat", [], |r| r.get(0))
        .unwrap();
    assert_eq!((ways_after, stats_after), (0, 0));
    let marks: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM stale_mark WHERE table_name = 'seg_match'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(marks, 0, "cleared");
}

#[test]
fn stale_run_keeps_unknown_and_unmatched_marks() {
    let (_dir, conn, batch) = setup();
    insert_straight_road(&conn);
    // fixture polyline doesn't follow the straight road — seed it so the
    // full run writes rows the kept marks must NOT delete
    conn.execute(
        "UPDATE src_route_point SET encoded = ?1 WHERE batch_id = ?2 AND xno = '2' AND direction = 0",
        rusqlite::params![straight_polyline(), batch],
    )
    .unwrap();
    rm_osm_match::run(&conn, batch).unwrap();
    // unknown route id + unparseable key: warn + keep, no deletes
    seg_mark(&conn, 999, 0, Some("999:0".into()));
    seg_mark(&conn, 0, 0, Some("bogus".into()));
    let out = rm_osm_match::run_stale(&conn, batch).unwrap().unwrap();
    assert_eq!((out.cleared, out.kept), (0, 2), "{out:?}");
    assert_eq!(out.summary.directions, 0);
    let marks: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM stale_mark WHERE table_name = 'seg_match'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(marks, 2, "kept");
    // REVIEW-FOCUS pin: unknown marks never delete matched rows
    let ways: i64 = conn
        .query_row("SELECT COUNT(*) FROM seg_match_way", [], |r| r.get(0))
        .unwrap();
    assert!(ways >= 1, "route2's runs survived");
}

#[test]
fn stale_run_keeps_mark_for_direction_without_staging_row() {
    let (_dir, conn, batch) = setup();
    insert_straight_road(&conn);
    conn.execute(
        "UPDATE src_route_point SET encoded = ?1 WHERE batch_id = ?2 AND xno = '2' AND direction = 0",
        rusqlite::params![straight_polyline(), batch],
    )
    .unwrap();
    rm_osm_match::run(&conn, batch).unwrap();
    // published route, but direction 5 has no staging row in this batch
    // (re-ingest dropped it): warn + keep — never delete rows not recomputed
    seg_mark(&conn, route2_rid(&conn), 5, None);
    let out = rm_osm_match::run_stale(&conn, batch).unwrap().unwrap();
    assert_eq!((out.cleared, out.kept), (0, 1), "{out:?}");
    assert_eq!(out.summary.directions, 0);
    let marks: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM stale_mark WHERE table_name = 'seg_match'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(marks, 1, "mark kept for a later ingest to restore");
    // REVIEW-FOCUS pin: kept mark never deletes matched rows
    let ways: i64 = conn
        .query_row("SELECT COUNT(*) FROM seg_match_way", [], |r| r.get(0))
        .unwrap();
    assert!(ways >= 1, "route2's runs survived");
}

#[test]
fn stale_run_without_marks_is_none() {
    let (_dir, conn, batch) = setup();
    assert!(rm_osm_match::run_stale(&conn, batch).unwrap().is_none());
}
