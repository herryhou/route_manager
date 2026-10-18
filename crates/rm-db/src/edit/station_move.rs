//! Station move: reposition the station's reference point. Command, pure
//! plan, load→plan→apply service.

use anyhow::Result;
use rusqlite::Connection;

use super::{
    apply_station_move, load_station_snapshot, EditError, StationMoveApplied, StationMoveOp,
    StationSnapshot,
};

#[derive(Debug, Clone)]
pub struct StationMove {
    pub station_version_id: i64, // must be the current version
    pub latitude: f64,
    pub longitude: f64,
}

/// Pure planning policy: a station position is a point, so both coords are
/// required and stored verbatim — no range check (rm-web's job) and no
/// no-op guard (moving onto the same spot mints a version, like stop move).
/// Total over the snapshot: the only failure is the stale-svi load upstream.
fn plan_station_move(snapshot: &StationSnapshot, cmd: StationMove) -> StationMoveOp {
    StationMoveOp {
        station_id: snapshot.station_id,
        old_station_version_id: snapshot.station_version_id,
        new_latitude: cmd.latitude,
        new_longitude: cmd.longitude,
    }
}

/// What `move_station` would do, without doing it: the current station row
/// plus the new coords. Same guard as the apply path (`StationNotCurrent`);
/// zero writes.
#[derive(Debug)]
pub struct StationMovePlan {
    pub station_id: i64,
    pub station_version_id: i64,
    pub station_name: String,
    pub old_latitude: f64,
    pub old_longitude: f64,
    pub new_latitude: f64,
    pub new_longitude: f64,
    pub op: StationMoveOp,
}

/// Dry-run twin of `move_station`: load → the plan → the plan struct.
pub fn plan_move_station(conn: &Connection, cmd: StationMove) -> Result<StationMovePlan> {
    let snapshot = load_station_snapshot(conn, cmd.station_version_id)?
        .ok_or(EditError::StationNotCurrent(cmd.station_version_id))?;
    let op = plan_station_move(&snapshot, cmd);
    Ok(StationMovePlan {
        station_id: snapshot.station_id,
        station_version_id: snapshot.station_version_id,
        station_name: snapshot.station_name,
        old_latitude: snapshot.latitude,
        old_longitude: snapshot.longitude,
        new_latitude: op.new_latitude,
        new_longitude: op.new_longitude,
        op,
    })
}

/// Application service: load → plan (pure) → apply (one tx).
pub fn move_station(
    conn: &Connection,
    cmd: StationMove,
    editor: &str,
    note: &str,
) -> Result<StationMoveApplied> {
    let plan = plan_move_station(conn, cmd)?;
    apply_station_move(conn, &plan.op, editor, note)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_copies_snapshot_ids_and_coords() {
        let snap = StationSnapshot {
            station_id: 7,
            station_version_id: 70,
            station_name: "S".into(),
            latitude: 24.0,
            longitude: 120.0,
            poles: vec![],
        };
        let op = plan_station_move(
            &snap,
            StationMove {
                station_version_id: 70,
                latitude: 25.0,
                longitude: 121.0,
            },
        );
        assert_eq!(op.station_id, 7);
        assert_eq!(op.old_station_version_id, 70);
        assert_eq!((op.new_latitude, op.new_longitude), (25.0, 121.0));
    }

    // ---------- service end-to-end (rehomed from rm-edit/tests/move.rs) ----------

    /// station 20 ("S") with poles 10/11, rsv row, bearing row, id sequences
    /// seeded. Same shape as station_rename/station_undo's fixture.
    fn fixture(conn: &rusqlite::Connection) -> i64 {
        conn.execute_batch(
            "INSERT INTO station_group (station_group_id, created_at) VALUES (1, '2026-01-01T00:00:00Z');
             INSERT INTO station_group_version (station_group_version_id, station_group_id, station_group_name, source, valid_from, created_at)
                 VALUES (1, 1, 'g', 'capture', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
             INSERT INTO station (station_id, ext_uid, created_at) VALUES (1, 'tcg:node:1', '2026-01-01T00:00:00Z');
             INSERT INTO station_version (station_version_id, station_id, station_group_id, station_name, latitude, longitude, valid_from, created_at)
                 VALUES (20, 1, 1, 'S', 24.0, 120.0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
             INSERT INTO stop (stop_id, ext_uid, created_at) VALUES (1, 'tcg:stop:1', '2026-01-01T00:00:00Z');
             INSERT INTO stop (stop_id, ext_uid, created_at) VALUES (2, 'tcg:stop:2', '2026-01-01T00:00:00Z');
             INSERT INTO stop_version (stop_version_id, stop_id, station_id, stop_code, stop_name, direction, latitude, longitude, valid_from, created_at)
                 VALUES (10, 1, 1, '001', 'S', 0, 24.0, 120.0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
             INSERT INTO stop_version (stop_version_id, stop_id, station_id, stop_code, stop_name, direction, latitude, longitude, valid_from, created_at)
                 VALUES (11, 2, 1, '001', 'S 往', 0, 24.0, 120.0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
             INSERT INTO route (route_id, route_code, ext_uid, created_at, updated_at)
                 VALUES (1, '1', 'tcg:1', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
             INSERT INTO route_version (route_version_id, route_id, version_no, valid_from, status, created_at)
                 VALUES (1, 1, 1, '2026-01-01T00:00:00Z', 'PUBLISHED', '2026-01-01T00:00:00Z');
             INSERT INTO subroute (subroute_id, route_id, subroute_code, created_at)
                 VALUES (1, 1, '1-0', '2026-01-01T00:00:00Z');
             INSERT INTO subroute_version (subroute_version_id, route_version_id, subroute_id, subroute_name, created_at)
                 VALUES (1, 1, 1, '1', '2026-01-01T00:00:00Z');
             INSERT INTO route_stop_version (route_stop_version_id, subroute_version_id, stop_version_id, direction, stop_sequence, created_at)
                 VALUES (1, 1, 10, 0, 1, '2026-01-01T00:00:00Z');
             INSERT INTO stop_bearing (stop_version_id, bearing_deg, source, computed_at)
                 VALUES (10, 123.5, 'OSM', '2026-01-02T00:00:00Z');
             INSERT INTO id_sequence (table_name, next_value) VALUES ('station_version', 21);
             INSERT INTO id_sequence (table_name, next_value) VALUES ('stop_version', 12);",
        )
        .unwrap();
        20
    }

    #[test]
    fn move_station_moves_end_to_end() {
        let conn = crate::open_in_memory().unwrap();
        let old = fixture(&conn);
        let out = move_station(
            &conn,
            StationMove {
                station_version_id: old,
                latitude: 24.5,
                longitude: 120.5,
            },
            "herry",
            "fix",
        )
        .unwrap();
        assert_eq!(out.new_station_version_id, 21);
        let (lat, lon, name, src): (f64, f64, String, String) = conn
            .query_row(
                "SELECT latitude, longitude, station_name, source FROM station_version
                 WHERE valid_to IS NULL",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            (lat, lon, name.as_str(), src.as_str()),
            (24.5, 120.5, "S", "manual")
        );
        // zero stale marks, poles untouched (no manual stop versions minted)
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM stop_version WHERE source = 'manual'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn move_station_rejects_stale_version() {
        let conn = crate::open_in_memory().unwrap();
        let old = fixture(&conn);
        move_station(
            &conn,
            StationMove {
                station_version_id: old,
                latitude: 24.5,
                longitude: 120.5,
            },
            "herry",
            "first",
        )
        .unwrap();
        // double-submit: 20 is no longer current
        let err = move_station(
            &conn,
            StationMove {
                station_version_id: old,
                latitude: 24.5,
                longitude: 120.5,
            },
            "herry",
            "second",
        )
        .unwrap_err();
        assert!(err.to_string().contains("not the current version"), "{err}");
    }

    #[test]
    fn move_station_rejects_unknown_version() {
        let conn = crate::open_in_memory().unwrap();
        fixture(&conn);
        let err = move_station(
            &conn,
            StationMove {
                station_version_id: 999,
                latitude: 24.5,
                longitude: 120.5,
            },
            "herry",
            "",
        )
        .unwrap_err();
        assert!(err.to_string().contains("not the current version"), "{err}");
    }

    // ---------- dry-run plan ----------

    #[test]
    fn plan_move_station_copies_snapshot_and_writes_nothing() {
        let conn = crate::open_in_memory().unwrap();
        fixture(&conn);
        let count = || {
            conn.query_row("SELECT COUNT(*) FROM station_version", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap()
        };
        let before = count();
        let plan = plan_move_station(
            &conn,
            StationMove {
                station_version_id: 20,
                latitude: 24.5,
                longitude: 120.5,
            },
        )
        .unwrap();
        assert_eq!(
            (
                plan.station_id,
                plan.station_version_id,
                plan.station_name.as_str()
            ),
            (1, 20, "S")
        );
        assert_eq!(
            (
                plan.old_latitude,
                plan.old_longitude,
                plan.new_latitude,
                plan.new_longitude
            ),
            (24.0, 120.0, 24.5, 120.5)
        );
        assert_eq!(plan.op.old_station_version_id, 20);
        assert_eq!(count(), before, "zero writes");
    }

    #[test]
    fn plan_move_station_refuses_stale_version() {
        let conn = crate::open_in_memory().unwrap();
        let old = fixture(&conn);
        move_station(
            &conn,
            StationMove {
                station_version_id: old,
                latitude: 24.5,
                longitude: 120.5,
            },
            "herry",
            "first",
        )
        .unwrap();
        let err = plan_move_station(
            &conn,
            StationMove {
                station_version_id: old,
                latitude: 24.6,
                longitude: 120.6,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("not the current version"), "{err}");
    }
}
