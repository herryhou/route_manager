//! Scenario 1: station rename — command, pure planning policy, and the
//! load→plan→apply service. The rename acts on the station as a whole, so
//! a stale pole version surfaces as the station-level error.

use anyhow::Result;
use rusqlite::Connection;

use super::{
    apply_station_rename, load_station_snapshot, EditError, PoleOp, StationRenameApplied,
    StationRenameOp, StationSnapshot,
};

/// How aggressively poles follow a station rename. A parameter, not a
/// policy: all three are legitimate user choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameLevel {
    StationOnly,
    MatchingPoles,
    AllPoles,
}

impl RenameLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            RenameLevel::StationOnly => "StationOnly",
            RenameLevel::MatchingPoles => "MatchingPoles",
            RenameLevel::AllPoles => "AllPoles",
        }
    }
}

#[derive(Debug)]
pub struct StationRename {
    pub station_version_id: i64,
    pub new_name: String,
    pub level: RenameLevel,
}

/// Pure planning policy: pick which poles follow the rename. No I/O.
/// The empty-name check trims whitespace; a valid name is stored verbatim
/// (no trimming) — exact user string wins. Emits the apply op directly.
fn plan_station_rename(
    snapshot: &StationSnapshot,
    cmd: StationRename,
) -> Result<StationRenameOp, EditError> {
    if cmd.new_name.trim().is_empty() {
        return Err(EditError::EmptyNewName);
    }
    if cmd.new_name == snapshot.station_name {
        return Err(EditError::NoOpRename);
    }
    let poles = match cmd.level {
        RenameLevel::StationOnly => vec![],
        RenameLevel::MatchingPoles => snapshot
            .poles
            .iter()
            .filter(|p| p.stop_name == snapshot.station_name)
            .map(|p| PoleOp {
                stop_id: p.stop_id,
                old_stop_version_id: p.stop_version_id,
                new_stop_name: cmd.new_name.clone(),
            })
            .collect(),
        RenameLevel::AllPoles => snapshot
            .poles
            .iter()
            .map(|p| PoleOp {
                stop_id: p.stop_id,
                old_stop_version_id: p.stop_version_id,
                new_stop_name: cmd.new_name.clone(),
            })
            .collect(),
    };
    Ok(StationRenameOp {
        station_id: snapshot.station_id,
        old_station_version_id: snapshot.station_version_id,
        old_station_name: snapshot.station_name.clone(),
        new_station_name: cmd.new_name,
        level: cmd.level.as_str().to_string(),
        poles,
    })
}

/// Application service: load → plan (pure) → apply (one tx).
pub fn rename_station(
    conn: &Connection,
    cmd: StationRename,
    editor: &str,
    note: &str,
) -> Result<StationRenameApplied> {
    let snapshot = load_station_snapshot(conn, cmd.station_version_id)?
        .ok_or(EditError::StationNotCurrent(cmd.station_version_id))?;
    let op = plan_station_rename(&snapshot, cmd)?;
    apply_station_rename(conn, &op, editor, note).map_err(|e| {
        // a stale pole (per-pole guard) surfaces as the station-level error —
        // rename acts on the station as a whole
        if let Some(EditError::NotCurrent(svi)) = e.downcast_ref::<EditError>() {
            return EditError::StationNotCurrent(*svi).into();
        }
        e
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PoleSnapshot;

    fn snapshot() -> StationSnapshot {
        StationSnapshot {
            station_id: 7,
            station_version_id: 70,
            station_name: "S".into(),
            latitude: 24.0,
            longitude: 120.0,
            poles: vec![
                PoleSnapshot {
                    stop_id: 1,
                    stop_version_id: 10,
                    stop_name: "S".into(),
                },
                PoleSnapshot {
                    stop_id: 2,
                    stop_version_id: 11,
                    stop_name: "S 往".into(),
                },
                PoleSnapshot {
                    stop_id: 3,
                    stop_version_id: 12,
                    stop_name: "X".into(),
                },
            ],
        }
    }

    fn cmd(new_name: &str, level: RenameLevel) -> StationRename {
        StationRename {
            station_version_id: 70,
            new_name: new_name.into(),
            level,
        }
    }

    #[test]
    fn station_only_churns_no_poles() {
        let op =
            plan_station_rename(&snapshot(), cmd("台北車站", RenameLevel::StationOnly)).unwrap();
        assert!(op.poles.is_empty());
        assert_eq!(
            (op.old_station_name.as_str(), op.new_station_name.as_str()),
            ("S", "台北車站")
        );
        assert_eq!(op.level, "StationOnly");
    }

    #[test]
    fn matching_poles_is_exact_match() {
        let op = plan_station_rename(&snapshot(), cmd("N", RenameLevel::MatchingPoles)).unwrap();
        // "S 往" and "X" are not byte-equal to "S" — only pole 1 follows
        assert_eq!(op.poles.len(), 1);
        assert_eq!(
            (
                op.poles[0].stop_id,
                op.poles[0].old_stop_version_id,
                op.poles[0].new_stop_name.as_str()
            ),
            (1, 10, "N")
        );
    }

    #[test]
    fn matching_poles_with_trailing_space_does_not_match() {
        let mut snap = snapshot();
        snap.poles[0].stop_name = "S ".into(); // trailing space
        let op = plan_station_rename(&snap, cmd("N", RenameLevel::MatchingPoles)).unwrap();
        assert!(op.poles.is_empty(), "no trimming — exact bytes only");
    }

    #[test]
    fn matching_poles_may_select_zero_poles() {
        let mut snap = snapshot();
        snap.station_name = "Q".into(); // no pole named Q
        let op = plan_station_rename(&snap, cmd("N", RenameLevel::MatchingPoles)).unwrap();
        assert!(op.poles.is_empty());
    }

    #[test]
    fn all_poles_churn_every_pole() {
        let op = plan_station_rename(&snapshot(), cmd("N", RenameLevel::AllPoles)).unwrap();
        assert_eq!(op.poles.len(), 3);
        assert_eq!(
            (
                op.poles[2].stop_id,
                op.poles[2].old_stop_version_id,
                op.poles[2].new_stop_name.as_str()
            ),
            (3, 12, "N")
        );
    }

    #[test]
    fn empty_new_name_rejected() {
        let err = plan_station_rename(&snapshot(), cmd("   ", RenameLevel::AllPoles)).unwrap_err();
        assert!(matches!(err, EditError::EmptyNewName));
    }

    #[test]
    fn no_op_rename_rejected() {
        let err = plan_station_rename(&snapshot(), cmd("S", RenameLevel::AllPoles)).unwrap_err();
        assert!(matches!(err, EditError::NoOpRename));
    }
}
