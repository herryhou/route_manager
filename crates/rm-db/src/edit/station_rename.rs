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

/// One pole the rename would churn, with its current name for the report.
#[derive(Debug)]
pub struct PoleRenamePlan {
    pub stop_id: i64,
    pub stop_version_id: i64,
    pub old_name: String,
    pub new_name: String,
}

/// What `rename_station` would do, without doing it: the current station
/// row plus the poles each level would churn. Same guards as the apply path
/// (`StationNotCurrent`, `EmptyNewName`, `NoOpRename`); zero writes.
#[derive(Debug)]
pub struct StationRenamePlan {
    pub station_id: i64,
    pub station_version_id: i64,
    pub old_name: String,
    pub new_name: String,
    pub level: String,
    pub poles: Vec<PoleRenamePlan>,
    pub op: StationRenameOp,
}

/// Dry-run twin of `rename_station`: load → the same plan guards → the
/// plan, with the pole list enriched by old names.
pub fn plan_rename_station(conn: &Connection, cmd: StationRename) -> Result<StationRenamePlan> {
    let snapshot = load_station_snapshot(conn, cmd.station_version_id)?
        .ok_or(EditError::StationNotCurrent(cmd.station_version_id))?;
    let op = plan_station_rename(&snapshot, cmd)?;
    let poles = op
        .poles
        .iter()
        .map(|p| {
            let old_name = snapshot
                .poles
                .iter()
                .find(|s| s.stop_version_id == p.old_stop_version_id)
                .map(|s| s.stop_name.clone())
                .unwrap_or_default();
            PoleRenamePlan {
                stop_id: p.stop_id,
                stop_version_id: p.old_stop_version_id,
                old_name,
                new_name: p.new_stop_name.clone(),
            }
        })
        .collect();
    Ok(StationRenamePlan {
        station_id: op.station_id,
        station_version_id: op.old_station_version_id,
        old_name: op.old_station_name.clone(),
        new_name: op.new_station_name.clone(),
        level: op.level.clone(),
        poles,
        op,
    })
}

/// Application service: load → plan (pure) → apply (one tx).
pub fn rename_station(
    conn: &Connection,
    cmd: StationRename,
    editor: &str,
    note: &str,
) -> Result<StationRenameApplied> {
    let plan = plan_rename_station(conn, cmd)?;
    apply_station_rename(conn, &plan.op, editor, note).map_err(|e| {
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

    // ---------- service end-to-end (rehomed from rm-edit/tests/rename.rs) ----------

    /// station 'S' (sv 20) + poles sv 10 'S' (bearing row, rsv) and sv 11
    /// 'S 往' (no bearing).
    fn fixture(conn: &rusqlite::Connection) {
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
    }

    fn e2e_cmd(new_name: &str, level: RenameLevel) -> StationRename {
        StationRename {
            station_version_id: 20,
            new_name: new_name.into(),
            level,
        }
    }

    #[test]
    fn rename_all_poles_end_to_end() {
        let conn = crate::open_in_memory().unwrap();
        fixture(&conn);
        let out = rename_station(
            &conn,
            e2e_cmd("台北車站", RenameLevel::AllPoles),
            "herry",
            "cjk",
        )
        .unwrap();
        assert_eq!(out.poles.len(), 2);
        // station renamed
        let name: String = conn
            .query_row(
                "SELECT station_name FROM station_version WHERE valid_to IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(name, "台北車站");
        // matching pole followed, non-matching also followed (AllPoles), bearing carried
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM stop_bearing b JOIN stop_version sv ON sv.stop_version_id = b.stop_version_id
             WHERE sv.stop_name = '台北車站'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        // no stale marks anywhere
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn rename_matching_poles_follows_only_exact_name() {
        let conn = crate::open_in_memory().unwrap();
        fixture(&conn);
        rename_station(&conn, e2e_cmd("N", RenameLevel::MatchingPoles), "herry", "").unwrap();
        // pole named 'S' followed; 'S 往' untouched
        let mut stmt = conn
            .prepare("SELECT stop_name FROM stop_version WHERE valid_to IS NULL ORDER BY stop_id")
            .unwrap();
        let names: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(names, vec!["N".to_string(), "S 往".to_string()]);
    }

    #[test]
    fn rename_station_only_touches_no_poles() {
        let conn = crate::open_in_memory().unwrap();
        fixture(&conn);
        let out =
            rename_station(&conn, e2e_cmd("N", RenameLevel::StationOnly), "herry", "").unwrap();
        assert!(out.poles.is_empty());
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM stop_version WHERE source = 'manual'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "no pole churned");
    }

    #[test]
    fn rename_rejects_stale_station_version() {
        let conn = crate::open_in_memory().unwrap();
        fixture(&conn);
        rename_station(&conn, e2e_cmd("N", RenameLevel::AllPoles), "herry", "first").unwrap();
        // double-submit: 20 is no longer current
        let err = rename_station(
            &conn,
            e2e_cmd("M", RenameLevel::AllPoles),
            "herry",
            "second",
        )
        .unwrap_err();
        assert!(err.to_string().contains("not the current version"), "{err}");
    }

    #[test]
    fn rename_rejects_no_op_and_empty() {
        let conn = crate::open_in_memory().unwrap();
        fixture(&conn);
        let err =
            rename_station(&conn, e2e_cmd("S", RenameLevel::AllPoles), "herry", "").unwrap_err();
        assert!(err.to_string().contains("equals the current name"), "{err}");
        let err =
            rename_station(&conn, e2e_cmd("  ", RenameLevel::AllPoles), "herry", "").unwrap_err();
        assert!(err.to_string().contains("empty"), "{err}");
    }

    // ---------- dry-run plan ----------

    #[test]
    fn plan_rename_station_reports_poles_and_writes_nothing() {
        let conn = crate::open_in_memory().unwrap();
        fixture(&conn);
        let count = || {
            conn.query_row("SELECT COUNT(*) FROM stop_version", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap()
        };
        let before = count();
        // MatchingPoles: only the pole byte-equal to "S" follows
        let plan = plan_rename_station(
            &conn,
            StationRename {
                station_version_id: 20,
                new_name: "N".into(),
                level: RenameLevel::MatchingPoles,
            },
        )
        .unwrap();
        assert_eq!(
            (
                plan.station_id,
                plan.station_version_id,
                plan.old_name.as_str(),
                plan.new_name.as_str(),
                plan.level.as_str()
            ),
            (1, 20, "S", "N", "MatchingPoles")
        );
        assert_eq!(plan.poles.len(), 1);
        assert_eq!(
            (
                plan.poles[0].stop_id,
                plan.poles[0].stop_version_id,
                plan.poles[0].old_name.as_str(),
                plan.poles[0].new_name.as_str()
            ),
            (1, 10, "S", "N")
        );
        // AllPoles lists every pole with old names
        let plan = plan_rename_station(
            &conn,
            StationRename {
                station_version_id: 20,
                new_name: "N".into(),
                level: RenameLevel::AllPoles,
            },
        )
        .unwrap();
        assert_eq!(plan.poles.len(), 2);
        assert_eq!(plan.poles[1].old_name.as_str(), "S 往");
        assert_eq!(count(), before, "zero writes");
    }

    #[test]
    fn plan_rename_station_refuses_stale_version_empty_and_noop() {
        let conn = crate::open_in_memory().unwrap();
        fixture(&conn);
        // after a real rename, the old svi is stale
        rename_station(
            &conn,
            StationRename {
                station_version_id: 20,
                new_name: "N".into(),
                level: RenameLevel::AllPoles,
            },
            "herry",
            "first",
        )
        .unwrap();
        let err = plan_rename_station(
            &conn,
            StationRename {
                station_version_id: 20,
                new_name: "M".into(),
                level: RenameLevel::AllPoles,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("not the current version"), "{err}");
        // empty + no-op through the plan entry (fixture is stale now; replan
        // on the NEW current version)
        let new_svi: i64 = conn
            .query_row(
                "SELECT station_version_id FROM station_version WHERE valid_to IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let err = plan_rename_station(
            &conn,
            StationRename {
                station_version_id: new_svi,
                new_name: "  ".into(),
                level: RenameLevel::AllPoles,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("empty"), "{err}");
        let err = plan_rename_station(
            &conn,
            StationRename {
                station_version_id: new_svi,
                new_name: "N".into(),
                level: RenameLevel::AllPoles,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("equals the current name"), "{err}");
    }
}
