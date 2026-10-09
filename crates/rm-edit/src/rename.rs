//! Scenario 1: station rename — domain model and pure planning policy.

use rm_db::StationSnapshot;

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

#[derive(Debug, PartialEq)]
pub struct PoleChange {
    pub stop_id: i64,
    pub old_stop_version_id: i64,
    pub new_stop_name: String,
}

#[derive(Debug)]
pub struct StationRenamePlan {
    pub station_id: i64,
    pub station_version_id: i64,
    pub old_name: String,
    pub new_name: String,
    pub level: RenameLevel,
    pub poles: Vec<PoleChange>,
}

#[derive(Debug, thiserror::Error)]
pub enum EditError {
    #[error("station version {0} is not the current version (or does not exist)")]
    StationNotCurrent(i64),
    #[error("new station name is empty")]
    EmptyNewName,
    #[error("new station name equals the current name")]
    NoOpRename,
}

/// Pure planning policy: pick which poles follow the rename. No I/O.
/// The empty-name check trims whitespace; a valid name is stored verbatim
/// (no trimming) — exact user string wins.
pub fn plan_station_rename(
    snapshot: &StationSnapshot,
    cmd: StationRename,
) -> Result<StationRenamePlan, EditError> {
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
            .map(|p| PoleChange {
                stop_id: p.stop_id,
                old_stop_version_id: p.stop_version_id,
                new_stop_name: cmd.new_name.clone(),
            })
            .collect(),
        RenameLevel::AllPoles => snapshot
            .poles
            .iter()
            .map(|p| PoleChange {
                stop_id: p.stop_id,
                old_stop_version_id: p.stop_version_id,
                new_stop_name: cmd.new_name.clone(),
            })
            .collect(),
    };
    Ok(StationRenamePlan {
        station_id: snapshot.station_id,
        station_version_id: snapshot.station_version_id,
        old_name: snapshot.station_name.clone(),
        new_name: cmd.new_name,
        level: cmd.level,
        poles,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rm_db::PoleSnapshot;

    fn snapshot() -> StationSnapshot {
        StationSnapshot {
            station_id: 7,
            station_version_id: 70,
            station_name: "S".into(),
            poles: vec![
                PoleSnapshot { stop_id: 1, stop_version_id: 10, stop_name: "S".into() },
                PoleSnapshot { stop_id: 2, stop_version_id: 11, stop_name: "S 往".into() },
                PoleSnapshot { stop_id: 3, stop_version_id: 12, stop_name: "X".into() },
            ],
        }
    }

    fn cmd(new_name: &str, level: RenameLevel) -> StationRename {
        StationRename { station_version_id: 70, new_name: new_name.into(), level }
    }

    #[test]
    fn station_only_churns_no_poles() {
        let plan = plan_station_rename(&snapshot(), cmd("台北車站", RenameLevel::StationOnly)).unwrap();
        assert_eq!(plan.poles, vec![]);
        assert_eq!((plan.old_name.as_str(), plan.new_name.as_str()), ("S", "台北車站"));
        assert_eq!(plan.level, RenameLevel::StationOnly);
    }

    #[test]
    fn matching_poles_is_exact_match() {
        let plan = plan_station_rename(&snapshot(), cmd("N", RenameLevel::MatchingPoles)).unwrap();
        // "S 往" and "X" are not byte-equal to "S" — only pole 1 follows
        assert_eq!(plan.poles, vec![PoleChange {
            stop_id: 1, old_stop_version_id: 10, new_stop_name: "N".into(),
        }]);
    }

    #[test]
    fn matching_poles_with_trailing_space_does_not_match() {
        let mut snap = snapshot();
        snap.poles[0].stop_name = "S ".into(); // trailing space
        let plan = plan_station_rename(&snap, cmd("N", RenameLevel::MatchingPoles)).unwrap();
        assert_eq!(plan.poles, vec![], "no trimming — exact bytes only");
    }

    #[test]
    fn matching_poles_may_select_zero_poles() {
        let mut snap = snapshot();
        snap.station_name = "Q".into(); // no pole named Q
        let plan = plan_station_rename(&snap, cmd("N", RenameLevel::MatchingPoles)).unwrap();
        assert_eq!(plan.poles, vec![]);
    }

    #[test]
    fn all_poles_churn_every_pole() {
        let plan = plan_station_rename(&snapshot(), cmd("N", RenameLevel::AllPoles)).unwrap();
        assert_eq!(plan.poles.len(), 3);
        assert_eq!(plan.poles[2], PoleChange {
            stop_id: 3, old_stop_version_id: 12, new_stop_name: "N".into(),
        });
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
