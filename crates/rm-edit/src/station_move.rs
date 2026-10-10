//! Station move: reposition the station's reference point. Commands, pure
//! plan, load→plan→apply service. Zero SQL: persistence is rm-db's.

use anyhow::Result;
use rusqlite::Connection;

use crate::{map_station_db_err, EditError};

#[derive(Debug, Clone)]
pub struct StationMove {
    pub station_version_id: i64, // must be the current version
    pub latitude: f64,
    pub longitude: f64,
}

#[derive(Debug)]
pub struct StationMovePlan {
    pub station_id: i64,
    pub station_version_id: i64, // old (current) version
    pub old_latitude: f64,
    pub old_longitude: f64,
    pub new_latitude: f64,
    pub new_longitude: f64,
}

/// Pure planning policy: a station position is a point, so both coords are
/// required and stored verbatim — no range check (rm-web's future job) and
/// no no-op guard (moving onto the same spot mints a version, like stop
/// move). Total over the snapshot: the only failure is the stale-svi load
/// upstream.
pub fn plan_station_move(
    snapshot: &rm_db::StationSnapshot,
    cmd: StationMove,
) -> Result<StationMovePlan, EditError> {
    Ok(StationMovePlan {
        station_id: snapshot.station_id,
        station_version_id: snapshot.station_version_id,
        old_latitude: snapshot.latitude,
        old_longitude: snapshot.longitude,
        new_latitude: cmd.latitude,
        new_longitude: cmd.longitude,
    })
}

#[derive(Debug)]
pub struct MoveOutcome {
    pub edit_id: i64,
    pub new_station_version_id: i64,
}

/// Application service: load → plan (pure) → apply (one tx). Zero SQL here.
pub fn move_station(
    conn: &Connection,
    cmd: StationMove,
    editor: &str,
    note: &str,
) -> Result<MoveOutcome> {
    let snapshot = rm_db::load_station_snapshot(conn, cmd.station_version_id)?
        .ok_or(EditError::StationNotCurrent(cmd.station_version_id))?;
    let plan = plan_station_move(&snapshot, cmd)?;
    let op = rm_db::StationMoveOp {
        station_id: plan.station_id,
        old_station_version_id: plan.station_version_id,
        new_latitude: plan.new_latitude,
        new_longitude: plan.new_longitude,
    };
    let applied = rm_db::apply_station_move(conn, &op, editor, note).map_err(map_station_db_err)?;
    Ok(MoveOutcome {
        edit_id: applied.edit_id,
        new_station_version_id: applied.new_station_version_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_copies_snapshot_ids_and_coords() {
        let snap = rm_db::StationSnapshot {
            station_id: 7,
            station_version_id: 70,
            station_name: "S".into(),
            latitude: 24.0,
            longitude: 120.0,
            poles: vec![],
        };
        let plan = plan_station_move(
            &snap,
            StationMove { station_version_id: 70, latitude: 25.0, longitude: 121.0 },
        )
        .unwrap();
        assert_eq!(plan.station_id, 7);
        assert_eq!(plan.station_version_id, 70);
        assert_eq!((plan.old_latitude, plan.old_longitude), (24.0, 120.0));
        assert_eq!((plan.new_latitude, plan.new_longitude), (25.0, 121.0));
    }
}
