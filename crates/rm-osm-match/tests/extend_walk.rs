//! rm-osm-match's extend walk over a real DB: the fixture helpers and the
//! graph-behaviour tests copied from the retired job CLI's tests/extend.rs,
//! planned through `rm_osm_match::extend::plan_extension` directly.

use std::path::PathBuf;

use rm_core::{geo, polyline, LatLng};
use rm_osm_match::extend::plan_extension;
use rusqlite::Connection;

const T0: &str = "2026-01-01T00:00:00Z";

/// read_cmds.rs's chain (station_group → station → stop_version → route →
/// route_version → subroute_version → route_stop_version) plus the
/// id_sequence seeds and batch 1 CLOSED; the src polyline for dir 0 is the
/// caller's geometry (encoded).
fn seed_route(conn: &Connection, dir0_encoded: &str) {
    conn.execute_batch(&format!(
        "INSERT INTO station_group (station_group_id, created_at) VALUES (1, '{T0}');
         INSERT INTO station_group_version (station_group_version_id, station_group_id, station_group_name, source, valid_from, created_at)
             VALUES (1, 1, 'g', 'capture', '{T0}', '{T0}');
         INSERT INTO station (station_id, ext_uid, created_at) VALUES (1, 'tcg:node:1', '{T0}');
         INSERT INTO station_version (station_version_id, station_id, station_group_id, station_name, latitude, longitude, valid_from, created_at)
             VALUES (1, 1, 1, 'S', 24.0, 120.0, '{T0}', '{T0}');
         INSERT INTO stop (stop_id, ext_uid, created_at) VALUES (1, 'tcg:stop:1', '{T0}');
         INSERT INTO stop_version (stop_version_id, stop_id, station_id, stop_code, stop_name, direction, latitude, longitude, valid_from, created_at)
             VALUES (10, 1, 1, '001', 'Old', 0, 24.0, 120.0, '{T0}', '{T0}');
         INSERT INTO route (route_id, route_code, ext_uid, revision, created_at, updated_at)
             VALUES (1, '1', 'tcg:1', 1, '{T0}', '{T0}');
         INSERT INTO route_version (route_version_id, route_id, version_no, valid_from, status, created_at)
             VALUES (1, 1, 1, '{T0}', 'PUBLISHED', '{T0}');
         UPDATE route SET current_route_version_id = 1 WHERE route_id = 1;
         INSERT INTO subroute (subroute_id, route_id, subroute_code, created_at)
             VALUES (1, 1, '1-0', '{T0}');
         INSERT INTO subroute_version (subroute_version_id, route_version_id, subroute_id, subroute_name, created_at)
             VALUES (1, 1, 1, '1', '{T0}');
         INSERT INTO route_stop_version (route_stop_version_id, subroute_version_id, stop_version_id, direction, stop_sequence, created_at)
             VALUES (1, 1, 10, 0, 1, '{T0}');
         INSERT INTO id_sequence (table_name, next_value) VALUES
             ('station_group', 2), ('station_group_version', 2),
             ('station', 2), ('station_version', 2),
             ('stop', 2), ('stop_version', 11),
             ('route_version', 2), ('subroute_version', 2),
             ('route_stop_version', 2);
         INSERT INTO import_batch (batch_id, source_dir, file_count, captured_at, valid_from, status, created_at)
             VALUES (1, 'assets/x', 1, '{T0}', '{T0}', 'CLOSED', '{T0}');
         INSERT INTO src_route_point (batch_id, xno, direction, encoded)
             VALUES (1, '1', 0, '{dir0_encoded}');"
    ))
    .unwrap();
}

/// A straight N-S way (100) of four nodes at 0.0001-deg lat steps (~11.1 m
/// per segment), INSERT style mirroring rm-db matched_path's test setup.
/// The main polyline's last vertex sits exactly on node 1.
fn seed_line_graph(conn: &Connection) {
    conn.execute_batch(
        "INSERT INTO osm_way (osm_way_id, highway, oneway, name)
             VALUES (100, 'residential', 0, '直線路');
         INSERT INTO osm_node (osm_node_id, lat, lon) VALUES
            (1, 24.1400, 120.6800),
            (2, 24.1401, 120.6800),
            (3, 24.1402, 120.6800),
            (4, 24.1403, 120.6800);
         INSERT INTO osm_way_node (osm_way_id, ordinal, osm_node_id) VALUES
            (100, 1, 1), (100, 2, 2), (100, 3, 3), (100, 4, 4);",
    )
    .unwrap();
}

/// The main fixture: route + a src polyline (24.1399,120.68) → node 1 of
/// the straight way, so the "last" end extends due north along way 100.
fn line_db() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let conn = rm_db::open(&db).unwrap();
    let encoded = polyline::encode(&[
        LatLng::new(24.1399, 120.6800),
        LatLng::new(24.1400, 120.6800),
    ]);
    seed_route(&conn, &encoded);
    seed_line_graph(&conn);
    (dir, db)
}

/// Mid-segment snap fixture: the path ends traveling due east at a point
/// sitting mid-way along E-W way 500 — nearer to west node 1 (behind the
/// travel direction) than east node 2 (ahead of it). Way 501 continues
/// east from node 2, so the outward walk must start on the east side.
fn midseg_db() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let conn = rm_db::open(&db).unwrap();
    let encoded = polyline::encode(&[
        LatLng::new(24.1400, 120.68005),
        LatLng::new(24.1400, 120.68010),
    ]);
    seed_route(&conn, &encoded);
    conn.execute_batch(
        "INSERT INTO osm_way (osm_way_id, highway, oneway, name) VALUES
            (500, 'residential', 0, '幹道'),
            (501, 'residential', 0, '延伸路');
         INSERT INTO osm_node (osm_node_id, lat, lon) VALUES
            (1, 24.1400000, 120.6800000),
            (2, 24.1400000, 120.6803000),
            (3, 24.1400000, 120.6804000),
            (4, 24.1400000, 120.6805000);
         INSERT INTO osm_way_node (osm_way_id, ordinal, osm_node_id) VALUES
            (500, 1, 1), (500, 2, 2),
            (501, 1, 2), (501, 2, 3), (501, 3, 4);",
    )
    .unwrap();
    (dir, db)
}

/// Route + src polyline but zero osm rows — `RoadGraph::from_conn`'s own
/// "run rm-osm-prepare" error must surface verbatim.
fn graphless_db() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let conn = rm_db::open(&db).unwrap();
    let encoded = polyline::encode(&[
        LatLng::new(24.1399, 120.6800),
        LatLng::new(24.1400, 120.6800),
    ]);
    seed_route(&conn, &encoded);
    (dir, db)
}

/// Mid-segment snap with the nearer node behind the outward bearing: every
/// extension vertex must project non-negatively onto the outward bearing —
/// no vertex lands behind the endpoint in the travel direction. Proximity-
/// only start selection walked to the nearer (west) node first, spiking the
/// override backward along the snap segment.
#[test]
fn midseg_snap_starts_on_the_outward_side() {
    let (_dir, db) = midseg_db();
    let conn = rm_db::open_readonly(&db).unwrap();
    let endpoint = LatLng::new(24.1400, 120.68010);
    let outward = geo::bearing_deg(LatLng::new(24.1400, 120.68005), endpoint);
    let mut logs = Vec::new();
    let (_candidate, exts, _source) =
        plan_extension(&conn, "1", 0, 15.0, "last", &mut logs).unwrap();
    let ext = &exts[0];
    assert!(ext.snap_dist_m < 1e-9, "endpoint sits on the snap segment");
    assert!(!ext.new_vertices.is_empty(), "walk reaches way 501");
    for v in &ext.new_vertices {
        let b = geo::bearing_deg(endpoint, LatLng::new(v[0], v[1]));
        let diff = ((b - outward + 540.0) % 360.0) - 180.0;
        assert!(
            diff.abs() <= 90.0,
            "vertex {v:?} lies behind the endpoint (bearing {b:.1} vs outward {outward:.1})"
        );
    }
    assert_eq!(
        ext.way_ids,
        vec![501],
        "snap seg 500 not walked backward: {:?}",
        ext.way_ids
    );
}

#[test]
fn endpoint_far_from_graph_rejected() {
    let (_dir, db) = line_db();
    {
        // dir 1 polyline ends ~2.2 km north of the graph
        let conn = rm_db::open(&db).unwrap();
        conn.execute(
            "INSERT INTO src_route_point (batch_id, xno, direction, encoded)
             VALUES (1, '1', 1, ?1)",
            [polyline::encode(&[
                LatLng::new(24.1399, 120.6800),
                LatLng::new(24.1600, 120.6800),
            ])],
        )
        .unwrap();
    }
    let conn = rm_db::open_readonly(&db).unwrap();
    let mut logs = Vec::new();
    let err = plan_extension(&conn, "1", 1, 10.0, "last", &mut logs).unwrap_err();
    assert!(
        err.to_string().contains("50"),
        "names the snap limit: {err}"
    );
}

#[test]
fn empty_osm_tables_rejected_with_prepare_hint() {
    let (_dir, db) = graphless_db();
    let conn = rm_db::open_readonly(&db).unwrap();
    let mut logs = Vec::new();
    let err = plan_extension(&conn, "1", 0, 10.0, "last", &mut logs).unwrap_err();
    assert!(
        err.to_string().contains("rm-osm-prepare"),
        "prepare hint surfaced verbatim: {err}"
    );
}

/// Pre-0013 DB degrade (the /raw incident rule): with `polyline_override`
/// dropped, planning still works — the snapshot falls back to the manual
/// src queries, the source reports src, and `logs` names the missing table
/// instead of erroring.
#[test]
fn plan_extension_degrades_without_override_table() {
    let (_dir, db) = line_db();
    {
        let conn = rm_db::open(&db).unwrap();
        conn.execute("DROP TABLE polyline_override", []).unwrap();
    }
    let conn = rm_db::open_readonly(&db).unwrap();
    let mut logs = Vec::new();
    let (_candidate, exts, source) =
        plan_extension(&conn, "1", 0, 15.0, "last", &mut logs).unwrap();
    assert_eq!(source, "src", "degraded snapshot loads the src polyline");
    assert!(
        logs.iter()
            .any(|l| l.contains("polyline_override table missing")),
        "degrade entry names the missing table: {logs:?}"
    );
    assert!(
        !exts[0].new_vertices.is_empty(),
        "walk still extends off the src path"
    );
}
