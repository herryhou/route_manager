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
        shared_pair: None, // fixture registers no segments
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
        "UPDATE route_stop_version SET stop_sequence = stop_sequence + 10
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
         INSERT INTO route_stop_version
            (route_stop_version_id, subroute_version_id, stop_version_id, direction,
             stop_sequence, source_order_no, created_at)
            VALUES (1, 5, 1, 0, 1, 1, '2026-01-01T00:00:00Z'),
                   (2, 5, 1, 0, 2, 2, '2026-01-01T00:00:00Z'),
                   (3, 6, 1, 0, 1, 1, '2026-01-01T00:00:00Z'),
                   (4, 6, 1, 0, 2, 2, '2026-01-01T00:00:00Z');
         INSERT INTO seg_traversal
            (route_version_id, subroute_version_id, direction,
             from_route_stop_version_id, to_route_stop_version_id, segment_id)
            VALUES (5, 5, 0, 1, 2, 's1'), (6, 6, 0, 3, 4, 's1');",
    )
    .unwrap();
    let qa = tempfile::tempdir().unwrap();

    let report = rm_verify::run(
        &conn,
        1,
        &Expectations {
            shared_pair: Some(("tcg:1", "tcg:9221")),
            ..Expectations::default()
        },
        qa.path(),
    )
    .unwrap();

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
            FROM route_stop_version;
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
    // members carry their route-relative direction for eyeballing
    for m in v["violating_stations"][0]["members"].as_array().unwrap() {
        assert!(
            m.get("direction").is_some(),
            "member missing direction: {m:?}"
        );
    }
}

#[test]
fn segment_variant_split_pair_warns_and_reports() {
    let (_dir, conn, batch) = setup();
    // one pair split into two corridor segments (same pair_key) + one
    // unsplit pair that must not appear
    let (st_a, st_b): (i64, i64) = conn
        .query_row(
            "SELECT MIN(station_id), MAX(station_id) FROM station",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let (rv, sv): (i64, i64) = conn
        .query_row(
            "SELECT rv.route_version_id, sv.subroute_version_id
             FROM route_version rv
             JOIN subroute_version sv ON sv.route_version_id = rv.route_version_id LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let mut rsvs: Vec<i64> = conn
        .prepare("SELECT route_stop_version_id FROM route_stop_version ORDER BY 1")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    rsvs.truncate(4);
    assert_eq!(rsvs.len(), 4, "fixture needs two stop pairs: {rsvs:?}");
    let (rsv_a, rsv_b, rsv_c, rsv_d) = (rsvs[0], rsvs[1], rsvs[2], rsvs[3]);
    conn.execute_batch(&format!(
        "INSERT INTO osm_way (osm_way_id, highway, oneway, name)
            VALUES (100, 'primary', 1, '主線'), (300, 'tertiary', 0, '側線');
         INSERT INTO seg_segment
            (segment_id, pair_key, corridor_key, start_station_id, end_station_id,
             encoded_polyline, length_m, way_count, variant_count, length_spread_m)
         VALUES
            ('segx', 'pk1', '390', {st_a}, {st_b}, '', 395.0, 1, 1, 0.0),
            ('segy', 'pk1', '600', {st_a}, {st_b}, '', 604.0, 1, 1, 0.0),
            ('segz', 'pk2', '120', {st_a}, {st_b}, '', 118.0, 1, 1, 0.0);
         INSERT INTO seg_segment_way (segment_id, ordinal, osm_way_id, length_m)
         VALUES ('segx', 1, 100, 395.0), ('segy', 1, 100, 604.0),
                ('segz', 1, 100, 118.0);
         INSERT INTO seg_traversal
            (route_version_id, subroute_version_id, direction,
             from_route_stop_version_id, to_route_stop_version_id, segment_id, way_sig)
         VALUES
            ({rv}, {sv}, 0, {rsv_a}, {rsv_b}, 'segx', '100'),
            ({rv}, {sv}, 0, {rsv_c}, {rsv_d}, 'segx', '100|300');",
    ))
    .unwrap();
    let qa = tempfile::tempdir().unwrap();

    let report = rm_verify::run(&conn, batch, &fixture_expectations(), qa.path()).unwrap();

    assert!(
        report.all_pass(),
        "corridor split must not fail: {report:#?}"
    );
    let warn = report
        .warnings
        .iter()
        .find(|w| w.id == "seg:variants")
        .expect("seg-variants warning present");
    assert!(
        warn.detail.contains("1 pair"),
        "one split pair flagged: {warn:?}"
    );
    assert_eq!(
        report.stats.get("seg_split_pairs"),
        Some(&1.0),
        "{:?}",
        report.stats
    );

    let v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(qa.path().join("segment_variant_report.json")).unwrap(),
    )
    .unwrap();
    let pairs = v["pairs"].as_array().expect("pairs array");
    assert_eq!(pairs.len(), 1, "unsplit pair not listed: {pairs:?}");
    assert_eq!(pairs[0]["pair_key"], "pk1");
    assert!(
        pairs[0]["from"].is_string() && pairs[0]["to"].is_string(),
        "station names for triage: {pairs:?}"
    );
    let segments = pairs[0]["segments"].as_array().expect("segments array");
    assert_eq!(segments.len(), 2, "{segments:?}");
    // ascending length: the 395 m corridor first
    assert_eq!(segments[0]["segment_id"], "segx");
    assert_eq!(segments[0]["corridor_key"], "390");
    assert_eq!(segments[0]["length_m"], 395.0);
    assert_eq!(segments[0]["way_sig"], "100");
    assert_eq!(segments[0]["ways"][0]["name"], "主線");
    // segx carries BOTH variants: majority way list shown (tie -> first),
    // routes + traversals span every variant, not just the majority
    assert_eq!(segments[0]["traversals"], 2, "all variants counted");
    assert_eq!(
        segments[0]["routes"].as_array().unwrap().len(),
        2,
        "segment carries route users: {segments:?}"
    );
    assert_eq!(segments[1]["segment_id"], "segy");
    assert_eq!(segments[1]["traversals"], 0);
    assert_eq!(
        segments[1]["routes"].as_array().unwrap().len(),
        0,
        "traversal-less member: {segments:?}"
    );
}

#[test]
fn match_dev_over_threshold_warns_but_does_not_fail() {
    let (_dir, conn, batch) = setup();
    // two matched directions: one strays 80 m off the src polyline (p95),
    // one sits tight — only the first is flagged
    let rv: i64 = conn
        .query_row(
            "SELECT rv.route_version_id FROM route r
             JOIN route_version rv ON rv.route_id = r.route_id
                  AND rv.route_version_id = r.current_route_version_id
             WHERE r.ext_uid = 'tcg:2'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    conn.execute_batch(&format!(
        "INSERT INTO seg_match_stat
            (route_version_id, direction, stops, path_length_m, matched_length_m,
             snap_dist_p50, snap_dist_p95, snap_dist_max, outliers, gap_repairs,
             dev_p50_m, dev_p95_m, dev_max_m)
         VALUES
            ({rv}, 0, 1, 1000.0, 1000.0, 5.0, 10.0, 20.0, 0, 0, 40.0, 80.0, 90.0),
            ({rv}, 1, 1, 1000.0, 1000.0, 5.0, 10.0, 20.0, 0, 0, 5.0, 10.0, 12.0);",
    ))
    .unwrap();
    let qa = tempfile::tempdir().unwrap();

    let report = rm_verify::run(&conn, batch, &fixture_expectations(), qa.path()).unwrap();

    // warn-only: exit status unchanged
    assert!(report.all_pass(), "deviation must not fail: {report:#?}");
    let warn = report
        .warnings
        .iter()
        .find(|w| w.id == "match:dev-p95")
        .expect("match-dev warning present");
    assert!(warn.detail.contains('1'), "one direction flagged: {warn:?}");
    assert_eq!(
        report.stats.get("dirs_dev_p95_over_50m"),
        Some(&1.0),
        "{:?}",
        report.stats
    );

    // QA detail file lists only the offending direction, worst first
    let v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(qa.path().join("match_dev_report.json")).unwrap(),
    )
    .unwrap();
    let dirs = v["directions"].as_array().expect("directions array");
    assert_eq!(dirs.len(), 1, "{dirs:?}");
    assert_eq!(dirs[0]["direction"], 0);
    assert_eq!(dirs[0]["dev_p95_m"], 80.0);
    assert!(
        dirs[0].get("route_code").is_some() && dirs[0].get("matched_ratio").is_some(),
        "row carries route + ratio for triage: {dirs:?}"
    );
}

/// Bump the id allocator past manually inserted rows so INV-04 stays quiet.
fn bump_sequence(conn: &rusqlite::Connection, table: &str, next: i64) {
    conn.execute(
        "INSERT INTO id_sequence (table_name, next_value) VALUES (?1, ?2)
         ON CONFLICT(table_name) DO UPDATE SET next_value = excluded.next_value",
        rusqlite::params![table, next],
    )
    .unwrap();
}

#[test]
fn recompute_flags_tampered_group_membership() {
    let (_dir, conn, batch) = setup();
    // move node 44 into its own group: stored partition no longer matches
    // what the grouping algorithm produces from the same current versions
    conn.execute_batch(
        "INSERT INTO station_group (station_group_id, created_at)
            VALUES (99, '2026-01-01T00:00:00Z');
         INSERT INTO station_group_version
            (station_group_id, station_group_name, source, valid_from, created_at)
            VALUES (99, '干城站', 'derived:same-name<80m',
                    '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
         UPDATE station_version SET station_group_id = 99
          WHERE station_id = (SELECT station_id FROM station
                              WHERE ext_uid = 'tcg:node:44');",
    )
    .unwrap();
    bump_sequence(&conn, "station_group", 100);
    bump_sequence(&conn, "station_group_version", 100);

    let qa = tempfile::tempdir().unwrap();
    let mut exp = fixture_expectations();
    exp.groups = Some(2); // the extra group is legal; only the membership is wrong
    let report = rm_verify::run(&conn, batch, &exp, qa.path()).unwrap();

    // only the tampered grouping is broken — no other check may fail
    let failed: Vec<&str> = report
        .checks
        .iter()
        .filter(|c| !c.pass)
        .map(|c| c.id.as_str())
        .collect();
    assert_eq!(failed, vec!["station:recompute"], "{report:#?}");
}

#[test]
fn station_without_current_version_is_caught() {
    let (_dir, conn, batch) = setup();
    conn.execute(
        "UPDATE station_version SET valid_to = '2026-01-02T00:00:00Z'
         WHERE station_id = (SELECT station_id FROM station
                             WHERE ext_uid = 'tcg:node:44')",
        [],
    )
    .unwrap();
    let qa = tempfile::tempdir().unwrap();

    let report = rm_verify::run(&conn, batch, &fixture_expectations(), qa.path()).unwrap();

    assert!(!report.all_pass());
    let c = report
        .checks
        .iter()
        .find(|c| c.id == "station:current-unique")
        .expect("current-unique check present");
    assert!(!c.pass, "station without current version must fail: {c:?}");
}

#[test]
fn chain_drift_warns_but_does_not_fail() {
    let (_dir, conn, batch) = setup();
    // 6 same-name stations in a ~70 m chain inside one group: every hop is
    // under 80 m so recompute agrees, but the diameter drifts to ~350 m
    let mut sql = String::from(
        "INSERT INTO station_group (station_group_id, created_at)
            VALUES (99, '2026-01-01T00:00:00Z');
         INSERT INTO station_group_version
            (station_group_id, station_group_name, source, valid_from, created_at)
            VALUES (99, '漂移站', 'derived:same-name<80m',
                    '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');",
    );
    for i in 0..6i64 {
        sql.push_str(&format!(
            "INSERT INTO station (station_id, ext_uid, created_at)
                VALUES ({sid}, 'tcg:node:{sid}', '2026-01-01T00:00:00Z');
             INSERT INTO station_version
                (station_version_id, station_id, station_group_id, station_name,
                 latitude, longitude, valid_from, created_at)
             VALUES ({vid}, {sid}, 99, '漂移站', {lat}, 120.6857,
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');",
            sid = 100 + i,
            vid = 200 + i,
            lat = 24.1409 + (i as f64) * 0.000_63,
        ));
    }
    conn.execute_batch(&sql).unwrap();
    bump_sequence(&conn, "station", 300);
    bump_sequence(&conn, "station_version", 300);
    bump_sequence(&conn, "station_group", 100);
    bump_sequence(&conn, "station_group_version", 100);

    let mut exp = fixture_expectations();
    exp.stations = Some(8);
    exp.groups = Some(2);
    let qa = tempfile::tempdir().unwrap();

    let report = rm_verify::run(&conn, batch, &exp, qa.path()).unwrap();

    assert!(report.all_pass(), "drift must not fail: {report:#?}");
    let c = report
        .checks
        .iter()
        .find(|c| c.id == "station:recompute")
        .expect("recompute check present");
    assert!(c.pass, "chained group is a legal partition: {c:?}");
    let warn = report
        .warnings
        .iter()
        .find(|w| w.id == "grouping:chain-drift")
        .expect("drift warning present");
    assert!(warn.detail.contains('1'), "one group flagged: {warn:?}");
    assert!(
        report.stats["grouping_max_diameter_m"] > 300.0,
        "{:?}",
        report.stats
    );

    // QA detail file with per-group members
    let v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(qa.path().join("grouping_report.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(v["violating_groups"].as_array().map(Vec::len), Some(1));
    let g = &v["violating_groups"][0];
    assert_eq!(g["member_count"], 6);
    assert_eq!(g["members"].as_array().map(Vec::len), Some(6));
    assert!(g["diameter_m"].as_f64().unwrap() > 300.0);
}

#[test]
fn sid_ambiguity_warns_but_does_not_fail() {
    let (_dir, conn, batch) = setup();
    // sid 21 already maps to node 44 on route 2; add it mapping to node 77
    conn.execute(
        "INSERT INTO src_stop_edge
            (batch_id, xno, direction, source_order_no, ivr_no, sid, node_id,
             station_name, lat, lon, lat_e6, lon_e6)
         VALUES (?1, '2', 0, 6, NULL, 21, '77', '干城站',
                 24.1409, 120.6857, 24140900, 120685700)",
        [batch],
    )
    .unwrap();
    let mut exp = fixture_expectations();
    exp.stop_rows = Some(5);
    let qa = tempfile::tempdir().unwrap();

    let report = rm_verify::run(&conn, batch, &exp, qa.path()).unwrap();

    assert!(
        report.all_pass(),
        "ambiguity is resolved first-wins: {report:#?}"
    );
    let warn = report
        .warnings
        .iter()
        .find(|w| w.id == "grouping:sid-ambiguous")
        .expect("ambiguity warning present");
    assert!(warn.detail.contains('1'), "one sid flagged: {warn:?}");
    assert_eq!(report.counts["sid_ambiguous"], 1);
}

#[test]
fn untrimmed_station_names_warn_but_do_not_fail() {
    let (_dir, conn, batch) = setup();
    // whitespace on BOTH members: same name on both sides of the recompute,
    // so the partition still matches — only the hygiene warning fires
    conn.execute(
        "UPDATE station_version SET station_name = ' ' || station_name || ' '",
        [],
    )
    .unwrap();
    let qa = tempfile::tempdir().unwrap();

    let report = rm_verify::run(&conn, batch, &fixture_expectations(), qa.path()).unwrap();

    assert!(
        report.all_pass(),
        "untrimmed names must not fail: {report:#?}"
    );
    let warn = report
        .warnings
        .iter()
        .find(|w| w.id == "grouping:name-untrimmed")
        .expect("untrimmed warning present");
    assert!(warn.detail.contains('2'), "two names flagged: {warn:?}");
}

#[test]
fn tight_bearing_spread_does_not_warn() {
    let (_dir, conn, batch) = setup();
    conn.execute_batch(
        "INSERT INTO stop_bearing (stop_version_id, bearing_deg, source, computed_at)
            SELECT stop_version_id, 12.0, 'OSM', '2026-01-01T00:00:00Z'
            FROM route_stop_version;
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
