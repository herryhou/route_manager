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

/// Application service: load → plan (pure) → apply (one tx).
pub fn move_station(
    conn: &Connection,
    cmd: StationMove,
    editor: &str,
    note: &str,
) -> Result<StationMoveApplied> {
    let snapshot = load_station_snapshot(conn, cmd.station_version_id)?
        .ok_or(EditError::StationNotCurrent(cmd.station_version_id))?;
    let op = plan_station_move(&snapshot, cmd);
    apply_station_move(conn, &op, editor, note)
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
}
