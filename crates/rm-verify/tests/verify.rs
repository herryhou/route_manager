//! Verify checks over the shared fixture.

use rm_verify::Expectations;

fn setup() -> (tempfile::TempDir, rusqlite::Connection, i64) {
    let dir = tempfile::tempdir().unwrap();
    rm_ingest::testfix::make_fixture(&dir.path().join("src"));
    let mut conn = rm_db::open_in_memory().unwrap();
    let s = rm_ingest::run(&conn, &dir.path().join("src")).unwrap();
    rm_identity::run(&conn, s.batch_id).unwrap();
    rm_publish::run(&mut conn, s.batch_id).unwrap();
    (dir, conn, s.batch_id)
}

fn fixture_expectations() -> Expectations {
    Expectations {
        routes: Some(2),
        stations: Some(2),
        stop_rows: Some(4),
        sids: Some(4),
        stops: Some(4),
        groups: Some(1),
        providers: Some(1),
        cycled: Some(1),
        single_dir: Some(1),
        no_polyline: None, // depends on empty-string rows, not asserted here
        main_side_groups: Some(1),
    }
}

#[test]
fn healthy_fixture_passes_and_writes_exports() {
    let (_dir, conn, batch) = setup();
    let qa = tempfile::tempdir().unwrap();

    let report = rm_verify::run(&conn, batch, &fixture_expectations(), qa.path()).unwrap();

    assert!(report.all_pass(), "failing checks: {report:#?}");
    for name in ["report.json", "segments.geojson", "outlier_stops.csv"] {
        assert!(qa.path().join(name).exists(), "missing export {name}");
    }
    // GeoJSON is a parseable FeatureCollection
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(qa.path().join("segments.geojson")).unwrap())
            .unwrap();
    assert_eq!(v["type"], "FeatureCollection");
    // CSV has a header line
    let csv = std::fs::read_to_string(qa.path().join("outlier_stops.csv")).unwrap();
    assert!(csv.starts_with("route,"));
}

#[test]
fn wrong_count_expectation_fails() {
    let (_dir, conn, batch) = setup();
    let mut exp = fixture_expectations();
    exp.routes = Some(99);
    let qa = tempfile::tempdir().unwrap();

    let report = rm_verify::run(&conn, batch, &exp, qa.path()).unwrap();

    assert!(!report.all_pass());
    assert!(
        report
            .checks
            .iter()
            .any(|c| c.id == "count:routes" && !c.pass),
        "{report:#?}"
    );
}

#[test]
fn sequence_gap_is_caught() {
    let (_dir, conn, batch) = setup();
    conn.execute(
        "UPDATE stop_of_route_version SET stop_sequence = stop_sequence + 10
         WHERE direction = 0 AND stop_sequence = 2",
        [],
    )
    .unwrap();
    let qa = tempfile::tempdir().unwrap();

    let report = rm_verify::run(&conn, batch, &fixture_expectations(), qa.path()).unwrap();

    assert!(!report.all_pass());
    assert!(
        report
            .checks
            .iter()
            .any(|c| c.id == "seq:contiguous" && !c.pass),
        "{report:#?}"
    );
}

#[test]
fn shared_pair_joins_through_route_version() {
    // route ids deliberately MISALIGNED from route_version ids: joining
    // route.route_id = seg_traversal.route_version_id misattributes routes
    let conn = rm_db::open_in_memory().unwrap();
    conn.execute_batch(
        "INSERT INTO route (route_id, route_code, ext_uid, revision, created_at, updated_at)
            VALUES (1, '1', 'tcg:1', 1, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'),
                   (2, '9221', 'tcg:9221', 1, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
         INSERT INTO route_version
            (route_version_id, route_id, version_no, status, valid_from, created_at)
            VALUES (5, 1, 1, 'PUBLISHED', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'),
                   (6, 2, 1, 'PUBLISHED', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
         UPDATE route SET current_route_version_id = 5 WHERE route_id = 1;
         UPDATE route SET current_route_version_id = 6 WHERE route_id = 2;
         INSERT INTO subroute (subroute_id, route_id, subroute_code, created_at) VALUES
            (5, 1, '1', '2026-01-01T00:00:00Z'), (6, 2, '9221', '2026-01-01T00:00:00Z');
         INSERT INTO subroute_version
            (subroute_version_id, route_version_id, subroute_id, subroute_name, created_at)
            VALUES (5, 5, 5, 'A - B', '2026-01-01T00:00:00Z'),
                   (6, 6, 6, 'C - D', '2026-01-01T00:00:00Z');
         INSERT INTO station (station_id, ext_uid, created_at) VALUES
            (1, 'tcg:node:1', '2026-01-01T00:00:00Z'),
            (2, 'tcg:node:2', '2026-01-01T00:00:00Z');
         INSERT INTO stop (stop_id, ext_uid, created_at) VALUES
            (1, 'tcg:1:0:1', '2026-01-01T00:00:00Z');
         INSERT INTO stop_version
            (stop_version_id, stop_id, station_id, stop_code, stop_name, direction,
             latitude, longitude, valid_from, created_at)
            VALUES (1, 1, 1, '1', 'X', 0, 24.14, 120.68, '2026-01-01T00:00:00Z',
                    '2026-01-01T00:00:00Z');
         INSERT INTO seg_segment
            (segment_id, start_station_id, end_station_id, encoded_polyline, length_m, way_count)
            VALUES ('s1', 1, 2, '_p~iF~ps|U', 100.0, 1);
         INSERT INTO stop_of_route_version
            (stop_of_route_version_id, subroute_version_id, stop_version_id, direction,
             stop_sequence, source_order_no, created_at)
            VALUES (1, 5, 1, 0, 1, 1, '2026-01-01T00:00:00Z'),
                   (2, 5, 1, 0, 2, 2, '2026-01-01T00:00:00Z'),
                   (3, 6, 1, 0, 1, 1, '2026-01-01T00:00:00Z'),
                   (4, 6, 1, 0, 2, 2, '2026-01-01T00:00:00Z');
         INSERT INTO seg_traversal
            (route_version_id, subroute_version_id, direction,
             from_stop_of_route_version_id, to_stop_of_route_version_id, segment_id)
            VALUES (5, 5, 0, 1, 2, 's1'), (6, 6, 0, 3, 4, 's1');",
    )
    .unwrap();
    let qa = tempfile::tempdir().unwrap();

    let report = rm_verify::run(&conn, 1, &Expectations::default(), qa.path()).unwrap();

    let pair = report
        .checks
        .iter()
        .find(|c| c.id == "seg:shared-pair")
        .expect("shared-pair check present");
    assert!(
        pair.pass,
        "misaligned ids must not hide the shared corridor: {pair:?}"
    );
    assert!(
        pair.detail.contains('1'),
        "expected 1 shared segment: {pair:?}"
    );
}

#[test]
fn bearing_spread_warns_but_does_not_fail() {
    let (_dir, conn, batch) = setup();
    // one station, two member stops 50° apart — over the 45° threshold
    conn.execute_batch(
        "INSERT INTO stop_bearing (stop_version_id, bearing_deg, source, computed_at)
            SELECT stop_version_id, CASE stop_sequence WHEN 1 THEN 10.0 ELSE 60.0 END,
                   'OSM', '2026-01-01T00:00:00Z'
            FROM stop_of_route_version;
         INSERT INTO station_bearing
            (station_id, bearing_deg, member_count, max_spread_deg, computed_at)
            SELECT station_id, 35.0, 2, 50.0, '2026-01-01T00:00:00Z'
            FROM station LIMIT 1;",
    )
    .unwrap();
    let qa = tempfile::tempdir().unwrap();

    let report = rm_verify::run(&conn, batch, &fixture_expectations(), qa.path()).unwrap();

    // warn-only: exit status unchanged
    assert!(report.all_pass(), "spread must not fail: {report:#?}");
    let warn = report
        .warnings
        .iter()
        .find(|w| w.id == "bearing:station-spread")
        .expect("bearing warning present");
    assert!(warn.detail.contains('1'), "one station flagged: {warn:?}");

    // QA detail file with per-station members
    let v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(qa.path().join("bearing_report.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(v["violating_stations"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        v["violating_stations"][0]["members"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
}

#[test]
fn tight_bearing_spread_does_not_warn() {
    let (_dir, conn, batch) = setup();
    conn.execute_batch(
        "INSERT INTO stop_bearing (stop_version_id, bearing_deg, source, computed_at)
            SELECT stop_version_id, 12.0, 'OSM', '2026-01-01T00:00:00Z'
            FROM stop_of_route_version;
         INSERT INTO station_bearing
            (station_id, bearing_deg, member_count, max_spread_deg, computed_at)
            SELECT station_id, 12.0, 2, 3.0, '2026-01-01T00:00:00Z'
            FROM station LIMIT 1;",
    )
    .unwrap();
    let qa = tempfile::tempdir().unwrap();

    let report = rm_verify::run(&conn, batch, &fixture_expectations(), qa.path()).unwrap();

    assert!(
        !report
            .warnings
            .iter()
            .any(|w| w.id == "bearing:station-spread"),
        "tight spread must not warn: {report:#?}"
    );
}
