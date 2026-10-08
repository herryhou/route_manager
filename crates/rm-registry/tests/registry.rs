//! End-to-end registry tests over the shared fixture + a hand-built road.

use rm_core::{polyline, LatLng};

fn setup() -> (tempfile::TempDir, rusqlite::Connection, i64) {
    let dir = tempfile::tempdir().unwrap();
    rm_ingest::testfix::make_fixture(&dir.path().join("src"));
    let mut conn = rm_db::open_in_memory().unwrap();
    let s = rm_ingest::run(&conn, &dir.path().join("src")).unwrap();
    rm_identity::run(&conn, s.batch_id).unwrap();
    rm_publish::run(&mut conn, s.batch_id).unwrap();
    (dir, conn, s.batch_id)
}

/// staging-only DB (batch still OPEN): for tests that craft staging before
/// identity's one-shot run.
fn setup_staging() -> (tempfile::TempDir, rusqlite::Connection, i64) {
    let dir = tempfile::tempdir().unwrap();
    rm_ingest::testfix::make_fixture(&dir.path().join("src"));
    let conn = rm_db::open_in_memory().unwrap();
    let s = rm_ingest::run(&conn, &dir.path().join("src")).unwrap();
    (dir, conn, s.batch_id)
}

/// road along lon 120.6857: nodes 1 (24.1405) - 2 (24.1408) - 3 (24.1412)
fn insert_road(conn: &rusqlite::Connection) {
    conn.execute_batch(
        "INSERT INTO osm_way (osm_way_id, highway, oneway, name) VALUES (100, 'residential', 0, '測試路');
         INSERT INTO osm_node (osm_node_id, lat, lon) VALUES
            (1, 24.1405, 120.6857),
            (2, 24.1408, 120.6857),
            (3, 24.1412, 120.6857);
         INSERT INTO osm_way_node (osm_way_id, ordinal, osm_node_id) VALUES
            (100, 1, 1), (100, 2, 2), (100, 3, 3);",
    )
    .unwrap();
}

fn road_polyline() -> String {
    // southward: chainage origin at the north end (24.1412), so stops
    // ordered 44 (.1411) -> 50 (.1409) -> 46 (.1407) have increasing chainage
    polyline::encode(&[
        LatLng {
            lat: 24.1412,
            lon: 120.6857,
        },
        LatLng {
            lat: 24.1410,
            lon: 120.6857,
        },
        LatLng {
            lat: 24.1407,
            lon: 120.6857,
        },
        LatLng {
            lat: 24.1405,
            lon: 120.6857,
        },
    ])
}

/// replace route 2's stops: node 44 -> mid node 50 -> node 46, and give it
/// the road polyline; route 1 keeps nodes 44 -> 46 with the same polyline.
/// NOTE stop coords follow the STATION (first-seen node coords), so nodes
/// 44/46 sit at their fixture positions; node 50 is new and lands mid-road.
fn rewrite_route2(conn: &rusqlite::Connection, batch: i64, enc: &str) {
    conn.execute(
        "UPDATE src_route_point SET encoded = ?1 WHERE batch_id = ?2 AND xno IN ('1','2')",
        rusqlite::params![enc, batch],
    )
    .unwrap();
    let rows: [(i64, i64, &str, f64, f64); 3] = [
        (1, 21, "44", 24.1409, 120.6857),
        (2, 22, "50", 24.14085, 120.6858), // new mid-road station
        (3, 23, "46", 24.1408, 120.6859),
    ];
    conn.execute(
        "DELETE FROM src_stop_edge WHERE batch_id = ?1 AND xno = '2'",
        [batch],
    )
    .unwrap();
    for (order, sid, node, lat, lon) in rows {
        conn.execute(
            "INSERT OR REPLACE INTO src_stop_edge
                (batch_id, xno, direction, source_order_no, ivr_no, sid, node_id, station_name, lat, lon, lat_e6, lon_e6)
             VALUES (?1, '2', 0, ?2, NULL, ?3, ?4, '干城站', ?5, ?6, ?7, ?8)",
            rusqlite::params![
                batch, order, sid, node, lat, lon,
                (lat * 1e6).round() as i64,
                (lon * 1e6).round() as i64
            ],
        )
        .unwrap();
    }
    conn.execute_batch(
        "UPDATE route SET current_route_version_id = NULL, revision = 0 WHERE ext_uid = 'tcg:2';
         DELETE FROM route_stop_version WHERE subroute_version_id IN
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
}

fn pipeline(conn: &mut rusqlite::Connection, batch: i64) {
    rm_identity::run(conn, batch).unwrap();
    rm_publish::run(conn, batch).unwrap();
    rm_segments::run(conn, batch, 50.0).unwrap();
    rm_osm_match::run(conn, batch).unwrap();
}

/// road 100 (straight) + way 101 (parallel dogleg, longer): two paths
/// between the same stations so one pair yields two way-list variants
fn insert_two_roads(conn: &rusqlite::Connection) {
    conn.execute_batch(
        "INSERT INTO osm_way (osm_way_id, highway, oneway, name) VALUES
            (100, 'residential', 0, '測試路'), (101, 'residential', 0, '測試側線');
         INSERT INTO osm_node (osm_node_id, lat, lon) VALUES
            (1, 24.1405, 120.6857), (2, 24.1408, 120.6857), (3, 24.1412, 120.6857),
            (4, 24.1412, 120.68595), (5, 24.1408, 120.68615), (6, 24.1405, 120.68595);
         INSERT INTO osm_way_node (osm_way_id, ordinal, osm_node_id) VALUES
            (100, 1, 1), (100, 2, 2), (100, 3, 3),
            (101, 1, 4), (101, 2, 5), (101, 3, 6);",
    )
    .unwrap();
}

fn dogleg_polyline() -> String {
    polyline::encode(&[
        LatLng {
            lat: 24.1412,
            lon: 120.68595,
        },
        LatLng {
            lat: 24.1408,
            lon: 120.68615,
        },
        LatLng {
            lat: 24.1405,
            lon: 120.68595,
        },
    ])
}

/// route 2 stops directly at nodes 44 -> 46 (no mid stop) on its own polyline
fn rewrite_route2_direct(conn: &rusqlite::Connection, batch: i64, enc: &str) {
    conn.execute(
        "UPDATE src_route_point SET encoded = ?1 WHERE batch_id = ?2 AND xno = '2'",
        rusqlite::params![enc, batch],
    )
    .unwrap();
    let rows: [(i64, &str, f64, f64); 2] =
        [(1, "44", 24.1409, 120.6857), (2, "46", 24.1408, 120.6859)];
    conn.execute(
        "DELETE FROM src_stop_edge WHERE batch_id = ?1 AND xno = '2'",
        [batch],
    )
    .unwrap();
    for (order, node, lat, lon) in rows {
        conn.execute(
            "INSERT OR REPLACE INTO src_stop_edge
                (batch_id, xno, direction, source_order_no, ivr_no, sid, node_id, station_name, lat, lon, lat_e6, lon_e6)
             VALUES (?1, '2', 0, ?2, NULL, ?2, ?3, '干城站', ?4, ?5, ?6, ?7)",
            rusqlite::params![
                batch, order, node, lat, lon,
                (lat * 1e6).round() as i64,
                (lon * 1e6).round() as i64
            ],
        )
        .unwrap();
    }
    conn.execute_batch(
        "UPDATE route SET current_route_version_id = NULL, revision = 0 WHERE ext_uid = 'tcg:2';
         DELETE FROM route_stop_version WHERE subroute_version_id IN
            (SELECT subroute_version_id FROM subroute_version WHERE route_version_id =
                (SELECT route_version_id FROM route_version WHERE route_id =
                    (SELECT route_id FROM route WHERE ext_uid = 'tcg:2')));
         DELETE FROM subroute_version WHERE route_version_id IN
            (SELECT route_version_id FROM route_version WHERE route_id IN
                (SELECT route_id FROM route WHERE ext_uid = 'tcg:2'));
         DELETE FROM route_version WHERE route_id IN
            (SELECT route_id FROM route WHERE ext_uid = 'tcg:2');
         DELETE FROM stop_version WHERE stop_id IN
            (SELECT stop_id FROM stop WHERE ext_uid LIKE 'tcg:2:%');
         DELETE FROM stop WHERE ext_uid LIKE 'tcg:2:%';",
    )
    .unwrap();
}

/// way 100 straight + way 101 a long detour (4 nodes, ~1.2 km) between the
/// same station pair: gap far beyond the corridor split threshold
fn insert_road_and_long_detour(conn: &rusqlite::Connection) {
    conn.execute_batch(
        "INSERT INTO osm_way (osm_way_id, highway, oneway, name) VALUES
            (100, 'residential', 0, '測試路'), (101, 'residential', 0, '測試遠繞');
         INSERT INTO osm_node (osm_node_id, lat, lon) VALUES
            (1, 24.1405, 120.6857), (2, 24.1408, 120.6857), (3, 24.1412, 120.6857),
            (4, 24.1410, 120.6856), (5, 24.1440, 120.6880),
            (6, 24.1390, 120.6880), (7, 24.1408, 120.6862);
         INSERT INTO osm_way_node (osm_way_id, ordinal, osm_node_id) VALUES
            (100, 1, 1), (100, 2, 2), (100, 3, 3),
            (101, 1, 4), (101, 2, 5), (101, 3, 6), (101, 4, 7);",
    )
    .unwrap();
}

fn long_detour_polyline() -> String {
    polyline::encode(&[
        LatLng {
            lat: 24.1410,
            lon: 120.6856,
        },
        LatLng {
            lat: 24.1440,
            lon: 120.6880,
        },
        LatLng {
            lat: 24.1390,
            lon: 120.6880,
        },
        LatLng {
            lat: 24.1408,
            lon: 120.6862,
        },
    ])
}

/// route {xno}: stops 44 -> 46 in BOTH directions on the given polyline,
/// so one route contributes two traversals of the same directed pair
/// (dir 1 has no fixture polyline row — insert it)
fn rewrite_stops_both_directions(conn: &rusqlite::Connection, batch: i64, xno: &str, enc: &str) {
    conn.execute(
        "INSERT OR REPLACE INTO src_route_point (batch_id, xno, direction, encoded)
         VALUES (?1, ?2, 0, ?3), (?1, ?2, 1, ?3)",
        rusqlite::params![batch, xno, enc],
    )
    .unwrap();
    conn.execute(
        "DELETE FROM src_stop_edge WHERE batch_id = ?1 AND xno = ?2",
        rusqlite::params![batch, xno],
    )
    .unwrap();
    for (order, node, lat, lon) in [
        (1i64, "44", 24.1409f64, 120.6857f64),
        (2, "46", 24.1408, 120.6859),
    ] {
        for dir in [0, 1] {
            conn.execute(
                "INSERT OR REPLACE INTO src_stop_edge
                    (batch_id, xno, direction, source_order_no, ivr_no, sid, node_id,
                     station_name, lat, lon, lat_e6, lon_e6)
                 VALUES (?1, ?2, ?9, ?3, NULL, ?4, ?4, '干城站', ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    batch,
                    xno,
                    order,
                    node,
                    lat,
                    lon,
                    (lat * 1e6).round() as i64,
                    (lon * 1e6).round() as i64,
                    dir
                ],
            )
            .unwrap();
        }
    }
    conn.execute_batch(&format!(
        "UPDATE route SET current_route_version_id = NULL, revision = 0
             WHERE ext_uid = 'tcg:{xno}';
         DELETE FROM route_stop_version WHERE subroute_version_id IN
            (SELECT subroute_version_id FROM subroute_version WHERE route_version_id IN
                (SELECT route_version_id FROM route_version WHERE route_id IN
                    (SELECT route_id FROM route WHERE ext_uid = 'tcg:{xno}')));
         DELETE FROM subroute_version WHERE route_version_id IN
            (SELECT route_version_id FROM route_version WHERE route_id IN
                (SELECT route_id FROM route WHERE ext_uid = 'tcg:{xno}'));
         DELETE FROM route_version WHERE route_id IN
            (SELECT route_id FROM route WHERE ext_uid = 'tcg:{xno}');
         DELETE FROM stop_version WHERE stop_id IN
            (SELECT stop_id FROM stop WHERE ext_uid LIKE 'tcg:{xno}:%');
         DELETE FROM stop WHERE ext_uid LIKE 'tcg:{xno}:%';",
    ))
    .unwrap();
}

#[test]
fn corridor_length_gap_splits_pair_into_two_segments() {
    let (_dir, mut conn, batch) = setup_staging();
    insert_road_and_long_detour(&conn);
    // route 1: 2 direct traversals (both directions on way 100);
    // route 2: 2 detour traversals (both directions on way 101, ~1.2 km)
    rewrite_stops_both_directions(&conn, batch, "1", &road_polyline());
    rewrite_stops_both_directions(&conn, batch, "2", &long_detour_polyline());
    pipeline(&mut conn, batch);

    let s = rm_registry::run(&conn, batch).unwrap();
    assert_eq!(s.traversals, 4, "{s:?}");
    assert_eq!(s.segments, 2, "direct + detour corridors split: {s:?}");

    let rows: Vec<(i64, String, String, String, f64, i64)> = {
        let mut stmt = conn
            .prepare(
                "SELECT rowid, segment_id, pair_key, corridor_key, length_m,
                        (SELECT COUNT(DISTINCT rv.route_version_id)
                         FROM seg_traversal t JOIN route_version rv
                           ON rv.route_version_id = t.route_version_id
                         WHERE t.segment_id = seg_segment.segment_id) AS routes
                 FROM seg_segment ORDER BY length_m",
            )
            .unwrap();
        let it = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })
            .unwrap();
        it.collect::<Result<_, _>>().unwrap()
    };
    assert_eq!(rows.len(), 2, "{rows:?}");
    let (short_rowid, short_id, pair, short_ck, short_len, short_routes) = &rows[0];
    let (long_rowid, long_id, long_pair, long_ck, long_len, long_routes) = &rows[1];

    // same pair, distinct corridor segment ids, gap beyond the threshold
    assert_eq!(pair, long_pair, "both belong to one station pair: {rows:?}");
    assert_ne!(short_id, long_id);
    assert_ne!(short_ck, long_ck);
    assert!(
        long_len - short_len > 100.0,
        "corridor gap beyond threshold: {rows:?}"
    );

    // ids freeze the spec formulas (corridor key inside segment_id)
    let (a_uid, b_uid): (String, String) = conn
        .query_row(
            "SELECT sa.ext_uid, sb.ext_uid
             FROM seg_segment sg
             JOIN station sa ON sa.station_id = sg.start_station_id
             JOIN station sb ON sb.station_id = sg.end_station_id
             LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(*pair, rm_registry::hash::pair_key(&a_uid, &b_uid));
    assert_eq!(
        *short_id,
        rm_registry::hash::segment_id(&a_uid, &b_uid, short_ck)
    );
    assert_eq!(
        *long_id,
        rm_registry::hash::segment_id(&a_uid, &b_uid, long_ck)
    );

    // corridor key = own-cluster median bucketed down to 10 m
    for (ck, len) in [(short_ck, *short_len), (long_ck, *long_len)] {
        let bucket: f64 = ck.parse().unwrap();
        assert_eq!(bucket % 10.0, 0.0, "10 m bucket: {rows:?}");
        assert!((len - bucket).abs() < 10.0, "bucket within 10 m: {rows:?}");
    }

    // clusters write in ascending median order (stable rowids)
    assert!(short_rowid < long_rowid, "ascending insert: {rows:?}");

    // traversals link to their OWN corridor segment: route 1 (2 dirs) on
    // the short segment, route 2 (2 dirs, single-route cluster) on the long
    let per_route: Vec<(String, f64)> = {
        let mut stmt = conn
            .prepare(
                "SELECT r.ext_uid, sg.length_m
                 FROM seg_traversal t
                 JOIN route_version rv ON rv.route_version_id = t.route_version_id
                 JOIN route r ON r.route_id = rv.route_id
                 JOIN seg_segment sg ON sg.segment_id = t.segment_id
                 ORDER BY r.ext_uid",
            )
            .unwrap();
        let it = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        it.collect::<Result<_, _>>().unwrap()
    };
    assert_eq!(per_route.len(), 4, "{per_route:?}");
    assert!(
        per_route
            .iter()
            .filter(|(r, _)| r == "tcg:1")
            .all(|(_, l)| *l == *short_len),
        "route 1 traversals on the direct cluster: {per_route:?}"
    );
    assert!(
        per_route
            .iter()
            .filter(|(r, _)| r == "tcg:2")
            .all(|(_, l)| *l == *long_len),
        "route 2 traversals on the detour cluster: {per_route:?}"
    );
    assert_eq!(*short_routes, 1, "one route on the direct corridor");
    assert_eq!(*long_routes, 1, "single-route long variant owns a segment");
}

#[test]
fn same_pair_different_ways_share_one_segment() {
    let (_dir, mut conn, batch) = setup_staging();
    insert_two_roads(&conn);
    // route 1 rides way 100, route 2 rides the parallel dogleg way 101,
    // both between the shared stations 44 -> 46
    rewrite_route2_direct(&conn, batch, &dogleg_polyline());
    conn.execute(
        "UPDATE src_route_point SET encoded = ?1 WHERE batch_id = ?2 AND xno = '1'",
        rusqlite::params![road_polyline(), batch],
    )
    .unwrap();
    pipeline(&mut conn, batch);

    let s = rm_registry::run(&conn, batch).unwrap();
    assert_eq!(s.traversals, 2, "{s:?}");
    assert_eq!(s.segments, 1, "one station pair -> one segment: {s:?}");

    let seg: (i64, f64, i64, f64) = conn
        .query_row(
            "SELECT way_count, length_m, variant_count, length_spread_m
             FROM seg_segment",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(seg.0, 1, "representative way list = route 1's way 100");
    assert_eq!(seg.2, 2, "two path variants recorded: {seg:?}");
    assert!(seg.3 > 5.0, "dogleg is the longer variant: {seg:?}");

    // every traversal keeps its own variant signature
    let sigs: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT t.way_sig FROM seg_traversal t
                 JOIN route_version rv ON rv.route_version_id = t.route_version_id
                 JOIN route r ON r.route_id = rv.route_id
                 ORDER BY r.ext_uid",
            )
            .unwrap();
        let it = stmt.query_map([], |r| r.get(0)).unwrap();
        it.collect::<Result<_, _>>().unwrap()
    };
    assert_eq!(sigs, vec!["100".to_string(), "101".to_string()]);
}

#[test]
fn shared_corridor_and_cut_boundaries() {
    let (_dir, mut conn, batch) = setup_staging();
    insert_road(&conn);
    rewrite_route2(&conn, batch, &road_polyline());
    pipeline(&mut conn, batch);

    let s = rm_registry::run(&conn, batch).unwrap();
    // route1/dir0 (44->46) and route2/dir0 (44->50, 50->46)
    assert_eq!(s.traversals, 3, "{s:?}");
    assert_eq!(s.skipped_pairs, 0, "{s:?}");
    // distinct segments: 44->46 (route1), 44->50, 50->46 => 3
    assert_eq!(s.segments, 3, "{s:?}");

    // route1's 44->46 segment is NOT shared with route2's cut pair, but the
    // way appears in all three with partial lengths
    let ways: Vec<(String, i64, f64)> = {
        let mut stmt = conn
            .prepare(
                "SELECT sw.segment_id, sw.osm_way_id, sw.length_m
                 FROM seg_segment_way sw ORDER BY sw.segment_id, sw.ordinal",
            )
            .unwrap();
        let it = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap();
        it.collect::<Result<_, _>>().unwrap()
    };
    assert_eq!(ways.len(), 3, "one way row per segment: {ways:?}");
    assert!(ways.iter().all(|(_, w, _)| *w == 100));
    // partial lengths positive: ~11 m (route1) + ~6 m + ~6 m (route2 halves)
    let lens: Vec<f64> = ways.iter().map(|(_, _, l)| *l).collect();
    assert!(lens.iter().all(|&l| l > 5.0), "lens {lens:?}");
    let total: f64 = lens.iter().sum::<f64>();
    assert!(total > 15.0 && total < 30.0, "total {total}");

    // every traversal references an existing segment with matching stations
    let bad: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM seg_traversal t
             JOIN seg_segment sg ON sg.segment_id = t.segment_id
             JOIN route_stop_version f ON f.route_stop_version_id = t.from_route_stop_version_id
             JOIN stop_version fv ON fv.stop_version_id = f.stop_version_id
             JOIN station fs ON fs.station_id = fv.station_id
             WHERE fs.station_id != sg.start_station_id",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(bad, 0);
}

#[test]
fn unmatchable_pairs_are_skipped_not_registered() {
    let (_dir, conn, batch) = setup();
    // no osm tables content: matcher will match nothing
    conn.execute_batch(
        "INSERT INTO osm_way (osm_way_id, highway, oneway, name) VALUES (1, 'residential', 0, 'x');
         INSERT INTO osm_node (osm_node_id, lat, lon) VALUES (1, 24.1400, 120.6800);
         INSERT INTO osm_way_node (osm_way_id, ordinal, osm_node_id) VALUES (1, 1, 1);",
    )
    .unwrap();
    rm_segments::run(&conn, batch, 50.0).unwrap();
    rm_osm_match::run(&conn, batch).unwrap();

    let s = rm_registry::run(&conn, batch).unwrap();
    assert_eq!(s.traversals, 0, "{s:?}");
    assert!(s.skipped_pairs >= 1, "{s:?}");
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM seg_segment", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0, "zero-way segments never registered");
}

#[test]
fn rerun_is_idempotent() {
    let (_dir, mut conn, batch) = setup_staging();
    insert_road(&conn);
    rewrite_route2(&conn, batch, &road_polyline());
    pipeline(&mut conn, batch);
    rm_registry::run(&conn, batch).unwrap();
    let before: (i64, i64, i64) = conn
        .query_row(
            "SELECT (SELECT COUNT(*) FROM seg_segment),
                    (SELECT COUNT(*) FROM seg_segment_way),
                    (SELECT COUNT(*) FROM seg_traversal)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    let s = rm_registry::run(&conn, batch).unwrap();
    let after: (i64, i64, i64) = conn
        .query_row(
            "SELECT (SELECT COUNT(*) FROM seg_segment),
                    (SELECT COUNT(*) FROM seg_segment_way),
                    (SELECT COUNT(*) FROM seg_traversal)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(before, after);
    assert_eq!(s.traversals as i64, after.2);
}
