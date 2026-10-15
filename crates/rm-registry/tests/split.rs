//! Station split tests: one station holding antipodal member bearings (both
//! carriageways of a dual carriageway collapsed onto identical Tainan
//! coordinates) must split into per-side stations; aligned members and
//! members without bearings never move; re-running is a no-op.

const T0: &str = "2026-01-01T00:00:00Z";

/// One station (id 1, "X") with three stop versions; bearings are inserted
/// per test.
fn setup() -> rusqlite::Connection {
    let conn = rm_db::open_in_memory().unwrap();
    conn.execute_batch(&format!(
        "INSERT INTO station_group (station_group_id, created_at) VALUES (1, '{T0}');
         INSERT INTO station_group_version
            (station_group_id, station_group_name, valid_from, created_at)
            VALUES (1, 'X', '{T0}', '{T0}');
         INSERT INTO station (station_id, ext_uid, created_at)
            VALUES (1, 'tcg:node:1', '{T0}');
         INSERT INTO station_version
            (station_id, station_group_id, station_name, latitude, longitude,
             valid_from, created_at)
            VALUES (1, 1, 'X', 24.1400, 120.6850, '{T0}', '{T0}');
         INSERT INTO stop (stop_id, ext_uid, created_at) VALUES
            (1, 'tcg:1:0:1', '{T0}'), (2, 'tcg:2:0:1', '{T0}'), (3, 'tcg:3:0:1', '{T0}');
         INSERT INTO stop_version
            (stop_version_id, stop_id, station_id, stop_code, stop_name, direction,
             latitude, longitude, valid_from, created_at)
            VALUES (1, 1, 1, '1', 'X', 0, 24.1407, 120.6860, '{T0}', '{T0}'),
                   (2, 2, 1, '2', 'X', 0, 24.1412, 120.6864, '{T0}', '{T0}'),
                   (3, 3, 1, '3', 'X', 0, 24.1407, 120.6860, '{T0}', '{T0}');
         INSERT INTO id_sequence (table_name, next_value) VALUES
            ('station', 2), ('station_version', 2);"
    ))
    .unwrap();
    conn
}

fn bearings(conn: &rusqlite::Connection, rows: &[(i64, f64)]) {
    for (svid, deg) in rows {
        conn.execute(
            &format!(
                "INSERT INTO stop_bearing (stop_version_id, bearing_deg, source, computed_at)
                 VALUES (?1, ?2, 'OSM', '{T0}')"
            ),
            rusqlite::params![svid, deg],
        )
        .unwrap();
    }
}

/// The station a stop version currently belongs to.
fn station_of(conn: &rusqlite::Connection, svid: i64) -> i64 {
    conn.query_row(
        "SELECT station_id FROM stop_version WHERE stop_version_id = ?1",
        [svid],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn antipodal_members_split_into_two_stations() {
    let conn = setup();
    bearings(&conn, &[(1, 10.0), (2, 190.0)]);

    let s = rm_registry::split::run(&conn).unwrap();
    assert_eq!(s.stations_created, 1, "{s:?}");
    assert_eq!(s.stops_moved, 1, "{s:?}");

    // tie 1v1: the smaller heading keeps the parent station
    assert_eq!(station_of(&conn, 1), 1, "10° side stays on parent");
    let child = station_of(&conn, 2);
    assert_ne!(child, 1, "190° side moves to a new station");

    // parent coords move to its retained member's mean (fixture station
    // coords deliberately differ from every member)
    let (plat, plon): (f64, f64) = conn
        .query_row(
            "SELECT latitude, longitude FROM station_version
             WHERE station_id = 1 AND valid_to IS NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!((plat - 24.1407).abs() < 1e-9 && (plon - 120.6860).abs() < 1e-9);

    // child station: suffixed ext_uid + copied current station_version
    let (uid,): (String,) = conn
        .query_row(
            "SELECT ext_uid FROM station WHERE station_id = ?1",
            [child],
            |r| Ok((r.get(0)?,)),
        )
        .unwrap();
    assert_eq!(uid, "tcg:node:1:190");
    let (name, group, lat, lon, vto): (String, i64, f64, f64, Option<String>) = conn
        .query_row(
            "SELECT station_name, station_group_id, latitude, longitude, valid_to
             FROM station_version WHERE station_id = ?1 AND valid_to IS NULL",
            [child],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!((name.as_str(), group, vto), ("X", 1, None));
    // child coords = its members' mean, not the parent's copy
    assert!((lat - 24.1412).abs() < 1e-9 && (lon - 120.6864).abs() < 1e-9);
}

#[test]
fn split_then_bearing_rebuild_is_signed_per_side() {
    let conn = setup();
    bearings(&conn, &[(1, 10.0), (2, 11.0), (3, 190.0)]);

    rm_registry::split::run(&conn).unwrap();
    let n = rm_registry::bearing::stations(&conn).unwrap();
    assert_eq!(n, 2, "one station_bearing row per side");

    let rows: Vec<(i64, f64, i64, f64)> = {
        let mut stmt = conn
            .prepare(
                "SELECT station_id, bearing_deg, member_count, max_spread_deg
                 FROM station_bearing ORDER BY bearing_deg",
            )
            .unwrap();
        let it = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap();
        it.collect::<Result<_, _>>().unwrap()
    };
    assert_eq!(rows.len(), 2);
    assert!((rows[0].1 - 10.5).abs() < 0.5, "{rows:?}");
    assert!((rows[0].3 - 0.5).abs() < 0.1, "signed heading, tiny spread");
    assert_eq!(rows[0].2, 2);
    assert!((rows[1].1 - 190.0).abs() < 0.5, "{rows:?}");
    assert_eq!(rows[1].2, 1);
}

#[test]
fn rerun_is_idempotent() {
    let conn = setup();
    bearings(&conn, &[(1, 10.0), (2, 190.0)]);
    rm_registry::split::run(&conn).unwrap();

    let s = rm_registry::split::run(&conn).unwrap();
    assert_eq!(
        (s.stations_split, s.stations_created, s.stops_moved),
        (0, 0, 0),
        "{s:?}"
    );
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM station", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 2);
}

#[test]
fn aligned_headings_do_not_split() {
    let conn = setup();
    bearings(&conn, &[(1, 10.0), (2, 40.0), (3, 350.0)]); // chained ≤ 90° gaps
    let s = rm_registry::split::run(&conn).unwrap();
    assert_eq!(
        (s.stations_split, s.stations_created, s.stops_moved),
        (0, 0, 0)
    );
    assert_eq!(station_of(&conn, 1), 1);
    assert_eq!(station_of(&conn, 2), 1);
    assert_eq!(station_of(&conn, 3), 1);
}

#[test]
fn bearingless_members_stay_on_parent() {
    let conn = setup();
    bearings(&conn, &[(1, 10.0), (3, 190.0)]); // stop 2 has no bearing
    rm_registry::split::run(&conn).unwrap();
    // parent keeps the 10° cluster by tie-break, so the bearingless stop
    // lands with it
    assert_eq!(station_of(&conn, 2), 1);
    assert_ne!(station_of(&conn, 3), 1);
}
