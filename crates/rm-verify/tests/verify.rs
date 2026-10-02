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
