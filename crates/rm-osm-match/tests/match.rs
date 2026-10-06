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
