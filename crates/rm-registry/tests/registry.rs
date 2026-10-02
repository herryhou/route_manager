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
}

fn pipeline(conn: &mut rusqlite::Connection, batch: i64) {
    rm_identity::run(conn, batch).unwrap();
    rm_publish::run(conn, batch).unwrap();
    rm_segments::run(conn, batch, 50.0).unwrap();
    rm_osm_match::run(conn, batch).unwrap();
}

#[test]
fn shared_corridor_and_cut_boundaries() {
    let (_dir, mut conn, batch) = setup();
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
             JOIN stop_of_route_version f ON f.stop_of_route_version_id = t.from_stop_of_route_version_id
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
    let (_dir, mut conn, batch) = setup();
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
