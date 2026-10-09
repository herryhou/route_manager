# Scenario 1 — Station Rename Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A whole-transaction station-rename primitive: new `rm-edit` domain crate (zero SQL) orchestrating new `rm-db` repository functions (`load_station_snapshot` / `apply_station_rename`).

**Architecture:** DDD-style incremental layering — `rm-edit` owns the domain (`RenameLevel`, `StationRename`, `StationRenamePlan`, pure `plan_station_rename`, public `EditError`) and maps a Plan to an op DTO defined in `rm-db`; `rm-db` owns all SQL. Dependency strictly one-way `rm-edit → rm-db`. Existing `edit_stop`/`undo`/`entity_history` are untouched.

**Tech Stack:** Rust 2021 workspace crate, rusqlite 0.37 (bundled), thiserror 2, anyhow, serde_json (rm-db only), in-memory SQLite tests.

**Spec:** `docs/superpowers/specs/2026-10-09-station-rename-design.md`

## Global Constraints

- `rm-edit` contains **zero SQL** — no `prepare`/`execute`/`query_row`; `rusqlite::Connection` appears only as a passthrough argument. It depends on `rm-db`; `rm-db` must NOT reference `rm-edit` (one-way dependency).
- No repository trait, no HTTP/UI changes, no changes to `edit_stop`/`undo`/`entity_history`.
- Version-append pattern everywhere: close old row (`valid_to = now`), mint new id via `crate::mint(&tx, "<table>")`, new row `source = 'manual'`, never UPDATE published columns.
- **Zero stale marks** from a rename: `stop_bearing` rows are **carried** to new version ids (`INSERT ... SELECT`, original `computed_at`), `station_bearing` untouched, no `path_stop_projection` marks.
- Workspace auto-includes the new crate (`members = ["crates/*"]`); do not edit the root `Cargo.toml`.
- Tests run in debug (`cargo test`) — fine for DB unit/integration tests; the CLAUDE.md release-mode rule applies to pipeline binaries, not this plan.
- Commit messages follow repo convention: `rm-db: …` / `rm-edit: …`, each ending with `Co-Authored-By: Claude Code <noreply@anthropic.com>`.

## Review Focus

Failure modes the spec implies but a happy-path test would miss — each pinned by a test in the owning task:

1. **Double-submit / stale `station_version_id` at apply time** — second rename of the same version must fail `StationNotCurrent` and leave zero partial rows (edit_log, versions). → Task 3 test `apply_rejects_stale_station_version`.
2. **Pole version churned between plan and apply** — a pole whose old svi is no longer current must abort the whole tx (no partial station/pole mix). → Task 3 test `apply_aborts_when_pole_not_current`.
3. **MatchingPoles is exact-match** — pole named `"S 往"` or `"S "` must NOT match station `"S"`; only byte-equal names match. → Task 2 test `matching_poles_is_exact_match`.
4. **Pole without a bearing row** — carry-over is a no-op for it; rename still succeeds. → Task 3 test `apply_carries_bearing_rows_only_where_present`.
5. **CJK names survive the edit_log JSON round-trip** — `old_name`/`new_name` with Chinese characters come back identical from `edit_log.changes`. → Task 3 test `apply_records_cjk_names_in_edit_log`.

---

### Task 1: rm-db — `StationSnapshot` + `load_station_snapshot`

**Files:**
- Modify: `crates/rm-db/src/edit.rs` (types + fn, after `entity_history`)
- Modify: `crates/rm-db/src/lib.rs:18` (re-export)
- Test: `crates/rm-db/src/edit.rs` (existing `mod tests`, existing `fixture`)

**Interfaces:**
- Consumes: existing `fixture(&Connection) -> i64` (seeds station 1 / station_version 1 named `'S'` / stop_version 10 named `'Old'`, current).
- Produces (Task 2 consumes): 
  `pub struct StationSnapshot { pub station_id: i64, pub station_version_id: i64, pub station_name: String, pub poles: Vec<PoleSnapshot> }`
  `pub struct PoleSnapshot { pub stop_id: i64, pub stop_version_id: i64, pub stop_name: String }`
  `pub fn load_station_snapshot(conn: &Connection, station_version_id: i64) -> Result<Option<StationSnapshot>>` — `None` when the id is not a **current** station version.

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `crates/rm-db/src/edit.rs`:

```rust
    #[test]
    fn load_station_snapshot_returns_station_and_current_poles() {
        let conn = open_in_memory().unwrap();
        let _ = fixture(&conn);
        let snap = load_station_snapshot(&conn, 1).unwrap().expect("current station version");
        assert_eq!(snap.station_id, 1);
        assert_eq!(snap.station_version_id, 1);
        assert_eq!(snap.station_name, "S");
        assert_eq!(snap.poles.len(), 1);
        assert_eq!((snap.poles[0].stop_id, snap.poles[0].stop_version_id, snap.poles[0].stop_name.as_str()), (1, 10, "Old"));
    }

    #[test]
    fn load_station_snapshot_none_for_stale_or_missing_version() {
        let conn = open_in_memory().unwrap();
        let _ = fixture(&conn);
        // close the station version → no longer current
        conn.execute("UPDATE station_version SET valid_to = '2026-01-02T00:00:00Z' WHERE station_version_id = 1", []).unwrap();
        assert!(load_station_snapshot(&conn, 1).unwrap().is_none());
        assert!(load_station_snapshot(&conn, 999).unwrap().is_none());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rm-db load_station_snapshot`
Expected: FAIL — `cannot find function load_station_snapshot in this scope`.

- [ ] **Step 3: Implement**

In `crates/rm-db/src/edit.rs`, after `entity_history`:

```rust
/// Read-side snapshot for the station-rename use case (rm-edit::rename_station):
/// the current station version plus its poles' current stop versions.
#[derive(Debug)]
pub struct StationSnapshot {
    pub station_id: i64,
    pub station_version_id: i64,
    pub station_name: String,
    pub poles: Vec<PoleSnapshot>,
}

#[derive(Debug)]
pub struct PoleSnapshot {
    pub stop_id: i64,
    pub stop_version_id: i64,
    pub stop_name: String,
}

/// Load the current `station_version` (matched by id AND `valid_to IS NULL`)
/// with its current poles. `None` when `station_version_id` is not current.
pub fn load_station_snapshot(
    conn: &Connection,
    station_version_id: i64,
) -> Result<Option<StationSnapshot>> {
    let row: (i64, String) = match conn
        .query_row(
            "SELECT station_id, station_name FROM station_version
             WHERE station_version_id = ?1 AND valid_to IS NULL",
            [station_version_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
    {
        Some(v) => v,
        None => return Ok(None),
    };
    let (station_id, station_name) = row;
    let mut stmt = conn.prepare(
        "SELECT stop_id, stop_version_id, stop_name FROM stop_version
         WHERE station_id = ?1 AND valid_to IS NULL
         ORDER BY stop_version_id",
    )?;
    let poles = stmt
        .query_map([station_id], |r| {
            Ok(PoleSnapshot {
                stop_id: r.get(0)?,
                stop_version_id: r.get(1)?,
                stop_name: r.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(StationSnapshot {
        station_id,
        station_version_id,
        station_name,
        poles,
    }))
}
```

In `crates/rm-db/src/lib.rs:18`, extend the re-export:

```rust
pub use edit::{
    edit_stop, entity_history, undo, EditError, EditLogRow, EditOutcome, StopEdit,
    StationSnapshot, PoleSnapshot, load_station_snapshot,
};
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rm-db`
Expected: PASS (all, including existing suites).

- [ ] **Step 5: Commit**

```bash
git add crates/rm-db/src/edit.rs crates/rm-db/src/lib.rs
git commit -m "rm-db: load_station_snapshot — current station + poles read for rename

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 2: rm-edit crate — domain types + pure `plan_station_rename`

**Files:**
- Create: `crates/rm-edit/Cargo.toml`
- Create: `crates/rm-edit/src/lib.rs`
- Create: `crates/rm-edit/src/rename.rs`
- Test: `crates/rm-edit/src/rename.rs` (`mod tests`, pure unit tests — no DB)

**Interfaces:**
- Consumes: `rm_db::{StationSnapshot, PoleSnapshot}` (Task 1).
- Produces (Tasks 3–4 consume):
  `pub enum RenameLevel { StationOnly, MatchingPoles, AllPoles }` with `pub fn as_str(&self) -> &'static str`
  `pub struct StationRename { pub station_version_id: i64, pub new_name: String, pub level: RenameLevel }`
  `pub struct PoleChange { pub stop_id: i64, pub old_stop_version_id: i64, pub new_stop_name: String }`
  `pub struct StationRenamePlan { pub station_id: i64, pub station_version_id: i64, pub old_name: String, pub new_name: String, pub level: RenameLevel, pub poles: Vec<PoleChange> }`
  `pub enum EditError { StationNotCurrent(i64), EmptyNewName, NoOpRename }` (derives `Debug, Error`)
  `pub fn plan_station_rename(snapshot: &StationSnapshot, cmd: StationRename) -> Result<StationRenamePlan, EditError>`

- [ ] **Step 1: Create the crate skeleton**

`crates/rm-edit/Cargo.toml`:

```toml
[package]
name = "rm-edit"
description = "Manual edit domain + application services (no SQL)"
edition.workspace = true
version.workspace = true

[dependencies]
rm-db.workspace = true
rusqlite.workspace = true
anyhow.workspace = true
thiserror.workspace = true
```

`crates/rm-edit/src/lib.rs`:

```rust
//! rm-edit: manual-edit domain and application services. Zero SQL —
//! persistence lives in rm-db; this crate orchestrates and decides.

mod rename;

pub use rename::*;
```

`crates/rm-edit/src/rename.rs` starts with only the types below so the crate compiles; the plan function comes in Step 3.

```rust
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
```

- [ ] **Step 2: Write the failing tests**

Append to `crates/rm-edit/src/rename.rs`:

```rust
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
```

Run: `cargo test -p rm-edit`
Expected: FAIL — `cannot find function plan_station_rename`.

- [ ] **Step 3: Implement the pure planner**

Add to `crates/rm-edit/src/rename.rs` (before `mod tests`):

```rust
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
            .cloned()
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rm-edit`
Expected: PASS (7 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/rm-edit
git commit -m "rm-edit: crate scaffold — RenameLevel, StationRenamePlan, pure planner

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 3: rm-db — `StationRenameOp` + `apply_station_rename` (one tx)

**Files:**
- Modify: `crates/rm-db/src/edit.rs` (op DTOs, `EditError::StationNotCurrent` variant, apply fn)
- Modify: `crates/rm-db/src/lib.rs:18` (re-exports)
- Test: `crates/rm-db/src/edit.rs` (`mod tests` — new `rename_fixture`)

**Interfaces:**
- Consumes: `crate::{mint, now_rfc3339}`; existing `EditError` enum.
- Produces (Task 4 consumes):
  `pub struct StationRenameOp { pub station_id: i64, pub old_station_version_id: i64, pub old_station_name: String, pub new_station_name: String, pub level: String, pub poles: Vec<PoleOp> }`
  `pub struct PoleOp { pub stop_id: i64, pub old_stop_version_id: i64, pub new_stop_name: String }`
  `pub struct StationRenameApplied { pub edit_id: i64, pub new_station_version_id: i64, pub poles: Vec<PoleApplied> }`
  `pub struct PoleApplied { pub stop_id: i64, pub old_svi: i64, pub new_svi: i64 }`
  `pub fn apply_station_rename(conn: &Connection, op: &StationRenameOp, editor: &str, note: &str) -> Result<StationRenameApplied>`
  New `EditError` variant: `#[error("station version {0} is not the current version (or does not exist)")] StationNotCurrent(i64)`

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `crates/rm-db/src/edit.rs`. New fixture: station `'S'` (station_version 20), poles stop_version 10 named `'S'` (has a bearing row) and 11 named `'S 往'` (no bearing row), route_stop_version pointing at 10.

```rust
    /// Station-rename fixture: station 'S' (sv 20) + poles sv 10 'S' (with
    /// bearing) and sv 11 'S 往' (no bearing), rsv pointing at sv 10.
    fn rename_fixture(conn: &Connection) -> i64 {
        conn.execute(
            "INSERT INTO station_group (station_group_id, created_at) VALUES (1, '2026-01-01T00:00:00Z')", []).unwrap();
        conn.execute(
            "INSERT INTO station_group_version (station_group_version_id, station_group_id, station_group_name, source, valid_from, created_at)
             VALUES (1, 1, 'g', 'capture', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')", []).unwrap();
        conn.execute(
            "INSERT INTO station (station_id, ext_uid, created_at) VALUES (1, 'tcg:node:1', '2026-01-01T00:00:00Z')", []).unwrap();
        conn.execute(
            "INSERT INTO station_version (station_version_id, station_id, station_group_id, station_name, latitude, longitude, valid_from, created_at)
             VALUES (20, 1, 1, 'S', 24.0, 120.0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')", []).unwrap();
        for (svi, sid, name) in [(10, 1, "S"), (11, 2, "S 往")] {
            conn.execute(
                "INSERT INTO stop (stop_id, created_at) VALUES (?1, '2026-01-01T00:00:00Z')", [sid]).unwrap();
            conn.execute(
                "INSERT INTO stop_version (stop_version_id, stop_id, station_id, stop_code, stop_name, direction, latitude, longitude, valid_from, created_at)
                 VALUES (?1, ?2, 1, '001', ?3, 0, 24.0, 120.0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
                params![svi, sid, name]).unwrap();
        }
        conn.execute(
            "INSERT INTO route (route_id, route_code, ext_uid, created_at, updated_at)
             VALUES (1, '1', 'tcg:1', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')", []).unwrap();
        conn.execute(
            "INSERT INTO route_version (route_version_id, route_id, version_no, valid_from, status, created_at)
             VALUES (1, 1, 1, '2026-01-01T00:00:00Z', 'PUBLISHED', '2026-01-01T00:00:00Z')", []).unwrap();
        conn.execute(
            "INSERT INTO subroute (subroute_id, route_id, subroute_code, created_at)
             VALUES (1, 1, '1-0', '2026-01-01T00:00:00Z')", []).unwrap();
        conn.execute(
            "INSERT INTO subroute_version (subroute_version_id, route_version_id, subroute_id, subroute_name, created_at)
             VALUES (1, 1, 1, '1', '2026-01-01T00:00:00Z')", []).unwrap();
        conn.execute(
            "INSERT INTO route_stop_version (route_stop_version_id, subroute_version_id, stop_version_id, direction, stop_sequence, created_at)
             VALUES (1, 1, 10, 0, 1, '2026-01-01T00:00:00Z')", []).unwrap();
        conn.execute(
            "INSERT INTO stop_bearing (stop_version_id, bearing_deg, source, computed_at)
             VALUES (10, 123.5, 'OSM', '2026-01-02T00:00:00Z')", []).unwrap();
        conn.execute("INSERT INTO id_sequence (table_name, next_value) VALUES ('station_version', 21)", []).unwrap();
        conn.execute("INSERT INTO id_sequence (table_name, next_value) VALUES ('stop_version', 12)", []).unwrap();
        20
    }

    fn all_poles_op() -> StationRenameOp {
        StationRenameOp {
            station_id: 1,
            old_station_version_id: 20,
            old_station_name: "S".into(),
            new_station_name: "台北車站".into(),
            level: "AllPoles".into(),
            poles: vec![
                PoleOp { stop_id: 1, old_stop_version_id: 10, new_stop_name: "台北車站".into() },
                PoleOp { stop_id: 2, old_stop_version_id: 11, new_stop_name: "台北車站".into() },
            ],
        }
    }

    #[test]
    fn apply_renames_station_and_all_poles_one_tx() {
        let conn = open_in_memory().unwrap();
        let old = rename_fixture(&conn);
        let out = apply_station_rename(&conn, &all_poles_op(), "herry", "fix").unwrap();
        // station version appended: old closed, new current + manual + new name
        let (to, _): (Option<String>, Option<String>) = conn.query_row(
            "SELECT valid_to, source FROM station_version WHERE station_version_id = ?1", [old], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert!(to.is_some());
        let (name, src): (String, String) = conn.query_row(
            "SELECT station_name, source FROM station_version WHERE station_version_id = ?1",
            [out.new_station_version_id], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((name.as_str(), src.as_str()), ("台北車站", "manual"));
        // both poles churned to the new name, source manual
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM stop_version WHERE stop_name = '台北車站' AND source = 'manual' AND valid_to IS NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2);
        // rsv repointed from sv 10 to the churned pole
        let sv: i64 = conn.query_row(
            "SELECT stop_version_id FROM route_stop_version WHERE route_stop_version_id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(sv, out.poles[0].new_svi);
        assert_eq!(out.poles.len(), 2);
    }

    #[test]
    fn apply_leaves_zero_stale_marks() {
        let conn = open_in_memory().unwrap();
        let old = rename_fixture(&conn);
        apply_station_rename(&conn, &all_poles_op(), "herry", "fix").unwrap();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0, "rename invalidates no derived data");
        // station_bearing untouched by design; none seeded — still none
        let _ = old;
    }

    #[test]
    fn apply_carries_bearing_rows_only_where_present() {
        let conn = open_in_memory().unwrap();
        rename_fixture(&conn);
        let out = apply_station_rename(&conn, &all_poles_op(), "herry", "fix").unwrap();
        // pole 1 (had bearing 123.5 @ computed_at): row now on new svi, same values
        let (deg, src, at): (f64, String, String) = conn.query_row(
            "SELECT bearing_deg, source, computed_at FROM stop_bearing WHERE stop_version_id = ?1",
            [out.poles[0].new_svi], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!((deg, src.as_str(), at.as_str()), (123.5, "OSM", "2026-01-02T00:00:00Z"));
        // old svi row gone (PK moved), pole 2 still has none
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM stop_bearing", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn apply_records_edit_log_row_shaped_for_undo() {
        let conn = open_in_memory().unwrap();
        let old = rename_fixture(&conn);
        let out = apply_station_rename(&conn, &all_poles_op(), "herry", "note!").unwrap();
        let (entity, eid, action, changes, note, vb, va): (String, i64, String, String, Option<String>, Option<i64>, Option<i64>) = conn.query_row(
            "SELECT entity, entity_id, action, changes, note, version_before, version_after FROM edit_log WHERE edit_id = ?1",
            [out.edit_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))).unwrap();
        assert_eq!((entity.as_str(), eid, action.as_str()), ("station", 1, "update"));
        assert_eq!(note.as_deref(), Some("note!"));
        assert_eq!((vb, va), (Some(old), Some(out.new_station_version_id)));
        // CJK round-trip + undo-ready shape
        let c: serde_json::Value = serde_json::from_str(&changes).unwrap();
        assert_eq!(c["kind"], "station_rename");
        assert_eq!(c["level"], "AllPoles");
        assert_eq!(c["old_name"], "S");
        assert_eq!(c["new_name"], "台北車站");
        assert_eq!(c["stops"].as_array().unwrap().len(), 2);
        assert_eq!(c["stops"][0]["stop_id"], 1);
        assert_eq!(c["stops"][0]["old_svi"], 10);
        assert_eq!(c["stops"][0]["new_svi"], out.poles[0].new_svi);
    }

    #[test]
    fn apply_rejects_stale_station_version() {
        let conn = open_in_memory().unwrap();
        let old = rename_fixture(&conn);
        apply_station_rename(&conn, &all_poles_op(), "herry", "first").unwrap();
        // double-submit of the same station version id
        let err = apply_station_rename(&conn, &all_poles_op(), "herry", "second").unwrap_err();
        assert!(err.to_string().contains("not the current version"), "{err}");
        // no partial state from the refused attempt: still exactly one manual
        // station version, one edit_log row, one bearing row
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM station_version WHERE source = 'manual'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM edit_log", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM stop_bearing", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        let _ = old;
    }

    #[test]
    fn apply_aborts_when_pole_not_current() {
        let conn = open_in_memory().unwrap();
        rename_fixture(&conn);
        // churn pole 10 behind the plan's back (name-only edit_stop)
        edit_stop(&conn, 10, &StopEdit { stop_name: Some("shadowed".into()), ..Default::default() }, "h", "n").unwrap();
        let err = apply_station_rename(&conn, &all_poles_op(), "herry", "stale pole").unwrap_err();
        assert!(err.to_string().contains("not the current version"), "{err}");
        // whole tx rolled back (the edit_stop's own manual stop_version and
        // stop_bearing mark stay — they are not this tx's): no manual
        // station version, no station edit_log row
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM station_version WHERE source = 'manual'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM edit_log WHERE entity = 'station'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rm-db apply_`
Expected: FAIL — `cannot find type StationRenameOp` / `cannot find function apply_station_rename`.

- [ ] **Step 3: Implement**

In `crates/rm-db/src/edit.rs`:

3a. Add the variant to `EditError`:

```rust
#[derive(Debug, Error)]
pub enum EditError {
    #[error("stop version {0} is not the current version (or does not exist)")]
    NotCurrent(i64),
    #[error("station version {0} is not the current version (or does not exist)")]
    StationNotCurrent(i64),
    #[error("edit {0} cannot be undone (not current, or is itself an undo)")]
    NotUndoable(i64),
}
```

3b. Add op DTOs and `apply_station_rename` (after `load_station_snapshot`):

```rust
/// Write-side op for the station rename (rm-edit maps a StationRenamePlan
/// into this; rm-db knows nothing about the domain types).
#[derive(Debug)]
pub struct StationRenameOp {
    pub station_id: i64,
    pub old_station_version_id: i64,
    pub old_station_name: String, // for the edit_log changes JSON only
    pub new_station_name: String,
    pub level: String, // RenameLevel::as_str
    pub poles: Vec<PoleOp>,
}

#[derive(Debug)]
pub struct PoleOp {
    pub stop_id: i64,
    pub old_stop_version_id: i64,
    pub new_stop_name: String,
}

#[derive(Debug)]
pub struct StationRenameApplied {
    pub edit_id: i64,
    pub new_station_version_id: i64,
    pub poles: Vec<PoleApplied>,
}

#[derive(Debug)]
pub struct PoleApplied {
    pub stop_id: i64,
    pub old_svi: i64,
    pub new_svi: i64,
}

/// Apply a station rename in ONE transaction: version-append the station,
/// churn the listed poles, repoint route_stop_version, carry stop_bearing
/// rows to the new pole versions (bearing value = f(position, path
/// geometry) — a rename changes neither, so nothing goes stale). One
/// edit_log row shaped for scenario-3 multi-row undo.
pub fn apply_station_rename(
    conn: &Connection,
    op: &StationRenameOp,
    editor: &str,
    note: &str,
) -> Result<StationRenameApplied> {
    let tx = conn.unchecked_transaction()?;
    let now = crate::now_rfc3339();

    // re-validate inside the tx: the old station version must still be current
    let (station_group_id, lat, lon): (i64, f64, f64) = match tx.query_row(
        "SELECT station_group_id, latitude, longitude FROM station_version
         WHERE station_version_id = ?1 AND valid_to IS NULL",
        [op.old_station_version_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ) {
        Ok(v) => v,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Err(EditError::StationNotCurrent(op.old_station_version_id).into())
        }
        Err(e) => return Err(e.into()),
    };
    tx.execute(
        "UPDATE station_version SET valid_to = ?1 WHERE station_version_id = ?2",
        params![now, op.old_station_version_id],
    )?;
    let new_station_version_id = crate::mint(&tx, "station_version")?;
    tx.execute(
        "INSERT INTO station_version
            (station_version_id, station_id, station_group_id, station_name,
             latitude, longitude, valid_from, created_at, source)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, 'manual')",
        params![
            new_station_version_id,
            op.station_id,
            station_group_id,
            op.new_station_name,
            lat,
            lon,
            now
        ],
    )?;

    let mut poles_applied = Vec::new();
    for pole in &op.poles {
        // pole must still be current; abort the whole tx otherwise
        let (stop_station_id, stop_code, direction, slat, slon): (i64, String, i64, f64, f64) =
            match tx.query_row(
                "SELECT station_id, stop_code, direction, latitude, longitude
                 FROM stop_version WHERE stop_version_id = ?1 AND valid_to IS NULL",
                [pole.old_stop_version_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            ) {
                Ok(v) => v,
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    return Err(EditError::NotCurrent(pole.old_stop_version_id).into())
                }
                Err(e) => return Err(e.into()),
            };
        tx.execute(
            "UPDATE stop_version SET valid_to = ?1 WHERE stop_version_id = ?2",
            params![now, pole.old_stop_version_id],
        )?;
        let new_svi = crate::mint(&tx, "stop_version")?;
        tx.execute(
            "INSERT INTO stop_version
                (stop_version_id, stop_id, station_id, stop_code, stop_name, direction,
                 latitude, longitude, valid_from, created_at, source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, 'manual')",
            params![
                new_svi,
                pole.stop_id,
                stop_station_id,
                stop_code,
                pole.new_stop_name,
                direction,
                slat,
                slon,
                now
            ],
        )?;
        tx.execute(
            "UPDATE route_stop_version SET stop_version_id = ?1 WHERE stop_version_id = ?2",
            params![new_svi, pole.old_stop_version_id],
        )?;
        // carry the bearing row to the new version (none → no-op)
        tx.execute(
            "INSERT INTO stop_bearing (stop_version_id, bearing_deg, source, computed_at)
             SELECT ?1, bearing_deg, source, computed_at FROM stop_bearing
             WHERE stop_version_id = ?2",
            params![new_svi, pole.old_stop_version_id],
        )?;
        poles_applied.push(PoleApplied {
            stop_id: pole.stop_id,
            old_svi: pole.old_stop_version_id,
            new_svi,
        });
    }

    let stops_json: Vec<serde_json::Value> = poles_applied
        .iter()
        .map(|p| json!({"stop_id": p.stop_id, "old_svi": p.old_svi, "new_svi": p.new_svi}))
        .collect();
    let changes = json!({
        "kind": "station_rename",
        "level": op.level,
        "old_name": op.old_station_name,
        "new_name": op.new_station_name,
        "stops": stops_json,
    })
    .to_string();
    tx.execute(
        "INSERT INTO edit_log (ts, editor, entity, entity_id, action, changes, note, version_before, version_after)
         VALUES (?1, ?2, 'station', ?3, 'update', ?4, ?5, ?6, ?7)",
        params![
            now,
            editor,
            op.station_id,
            changes,
            if note.is_empty() { None } else { Some(note) },
            op.old_station_version_id,
            new_station_version_id
        ],
    )?;
    let edit_id = tx.last_insert_rowid();

    tx.commit()?;
    Ok(StationRenameApplied {
        edit_id,
        new_station_version_id,
        poles: poles_applied,
    })
}
```

3c. In `crates/rm-db/src/lib.rs:18`, extend the re-export:

```rust
pub use edit::{
    edit_stop, entity_history, undo, EditError, EditLogRow, EditOutcome, StopEdit,
    StationSnapshot, PoleSnapshot, load_station_snapshot,
    StationRenameOp, PoleOp, StationRenameApplied, PoleApplied, apply_station_rename,
};
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rm-db`
Expected: PASS (all, including the six new apply_* tests).

- [ ] **Step 5: Commit**

```bash
git add crates/rm-db/src/edit.rs crates/rm-db/src/lib.rs
git commit -m "rm-db: apply_station_rename — one-tx station + pole churn, bearing carry-over

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 4: rm-edit — `rename_station` orchestration + integration tests

**Files:**
- Modify: `crates/rm-edit/src/rename.rs` (service fn + outcome type)
- Test: `crates/rm-edit/tests/rename.rs` (integration, in-memory DB)

**Interfaces:**
- Consumes: `rm_db::{open_in_memory, load_station_snapshot, apply_station_rename, StationRenameOp, PoleOp}` (Tasks 1+3); Task 2 types.
- Produces (rm-web consumes in a later phase):
  `pub struct RenameOutcome { pub edit_id: i64, pub new_station_version_id: i64, pub poles: Vec<PoleRenamed> }`
  `pub struct PoleRenamed { pub stop_id: i64, pub old_svi: i64, pub new_svi: i64 }`
  `pub fn rename_station(conn: &Connection, cmd: StationRename, editor: &str, note: &str) -> anyhow::Result<RenameOutcome>` — typed `EditError` values returned via `.into()`, matching `rm_db::edit` style.

- [ ] **Step 1: Write the failing integration tests**

`crates/rm-edit/tests/rename.rs`:

```rust
//! Integration: rename_station end-to-end on an in-memory DB.

use rm_edit::{rename_station, EditError, RenameLevel, StationRename};
use rm_db::open_in_memory;
use rusqlite::Connection;

/// Same shape as rm-db's edit tests fixture: station 'S' (sv 20) + poles
/// sv 10 'S' (bearing row, rsv) and sv 11 'S 往' (no bearing).
fn fixture(conn: &Connection) {
    conn.execute(
        "INSERT INTO station_group (station_group_id, created_at) VALUES (1, '2026-01-01T00:00:00Z')", []).unwrap();
    conn.execute(
        "INSERT INTO station_group_version (station_group_version_id, station_group_id, station_group_name, source, valid_from, created_at)
         VALUES (1, 1, 'g', 'capture', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')", []).unwrap();
    conn.execute(
        "INSERT INTO station (station_id, ext_uid, created_at) VALUES (1, 'tcg:node:1', '2026-01-01T00:00:00Z')", []).unwrap();
    conn.execute(
        "INSERT INTO station_version (station_version_id, station_id, station_group_id, station_name, latitude, longitude, valid_from, created_at)
         VALUES (20, 1, 1, 'S', 24.0, 120.0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')", []).unwrap();
    for (svi, sid, name) in [(10, 1, "S"), (11, 2, "S 往")] {
        conn.execute("INSERT INTO stop (stop_id, created_at) VALUES (?1, '2026-01-01T00:00:00Z')", [sid]).unwrap();
        conn.execute(
            "INSERT INTO stop_version (stop_version_id, stop_id, station_id, stop_code, stop_name, direction, latitude, longitude, valid_from, created_at)
             VALUES (?1, ?2, 1, '001', ?3, 0, 24.0, 120.0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            rusqlite::params![svi, sid, name]).unwrap();
    }
    conn.execute(
        "INSERT INTO route (route_id, route_code, ext_uid, created_at, updated_at)
         VALUES (1, '1', 'tcg:1', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')", []).unwrap();
    conn.execute(
        "INSERT INTO route_version (route_version_id, route_id, version_no, valid_from, status, created_at)
         VALUES (1, 1, 1, '2026-01-01T00:00:00Z', 'PUBLISHED', '2026-01-01T00:00:00Z')", []).unwrap();
    conn.execute(
        "INSERT INTO subroute (subroute_id, route_id, subroute_code, created_at)
         VALUES (1, 1, '1-0', '2026-01-01T00:00:00Z')", []).unwrap();
    conn.execute(
        "INSERT INTO subroute_version (subroute_version_id, route_version_id, subroute_id, subroute_name, created_at)
         VALUES (1, 1, 1, '1', '2026-01-01T00:00:00Z')", []).unwrap();
    conn.execute(
        "INSERT INTO route_stop_version (route_stop_version_id, subroute_version_id, stop_version_id, direction, stop_sequence, created_at)
         VALUES (1, 1, 10, 0, 1, '2026-01-01T00:00:00Z')", []).unwrap();
    conn.execute(
        "INSERT INTO stop_bearing (stop_version_id, bearing_deg, source, computed_at)
         VALUES (10, 123.5, 'OSM', '2026-01-02T00:00:00Z')", []).unwrap();
    conn.execute("INSERT INTO id_sequence (table_name, next_value) VALUES ('station_version', 21)", []).unwrap();
    conn.execute("INSERT INTO id_sequence (table_name, next_value) VALUES ('stop_version', 12)", []).unwrap();
}

fn cmd(new_name: &str, level: RenameLevel) -> StationRename {
    StationRename { station_version_id: 20, new_name: new_name.into(), level }
}

#[test]
fn rename_all_poles_end_to_end() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let out = rename_station(&conn, cmd("台北車站", RenameLevel::AllPoles), "herry", "cjk").unwrap();
    assert_eq!(out.poles.len(), 2);
    // station renamed
    let name: String = conn.query_row(
        "SELECT station_name FROM station_version WHERE valid_to IS NULL", [], |r| r.get(0)).unwrap();
    assert_eq!(name, "台北車站");
    // matching pole followed, non-matching also followed (AllPoles), bearing carried
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM stop_bearing b JOIN stop_version sv ON sv.stop_version_id = b.stop_version_id
         WHERE sv.stop_name = '台北車站'", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 1);
    // no stale marks anywhere
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0);
}

#[test]
fn rename_matching_poles_follows_only_exact_name() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    rename_station(&conn, cmd("N", RenameLevel::MatchingPoles), "herry", "").unwrap();
    // pole named 'S' followed; 'S 往' untouched
    let mut stmt = conn.prepare(
        "SELECT stop_name FROM stop_version WHERE valid_to IS NULL ORDER BY stop_id").unwrap();
    let names: Vec<String> = stmt.query_map([], |r| r.get(0)).unwrap()
        .collect::<Result<_, _>>().unwrap();
    assert_eq!(names, vec!["N".to_string(), "S 往".to_string()]);
}

#[test]
fn rename_station_only_touches_no_poles() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let out = rename_station(&conn, cmd("N", RenameLevel::StationOnly), "herry", "").unwrap();
    assert!(out.poles.is_empty());
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM stop_version WHERE source = 'manual'", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0, "no pole churned");
}

#[test]
fn rename_rejects_stale_station_version() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    rename_station(&conn, cmd("N", RenameLevel::AllPoles), "herry", "first").unwrap();
    // double-submit: 20 is no longer current
    let err = rename_station(&conn, cmd("M", RenameLevel::AllPoles), "herry", "second").unwrap_err();
    assert!(err.to_string().contains("not the current version"), "{err}");
    let _ = EditError::StationNotCurrent(0); // variant exists and is public
}

#[test]
fn rename_rejects_no_op_and_empty() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let err = rename_station(&conn, cmd("S", RenameLevel::AllPoles), "herry", "").unwrap_err();
    assert!(err.to_string().contains("equals the current name"), "{err}");
    let err = rename_station(&conn, cmd("  ", RenameLevel::AllPoles), "herry", "").unwrap_err();
    assert!(err.to_string().contains("empty"), "{err}");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rm-edit --test rename`
Expected: FAIL — `cannot find function rename_station in this scope`.

- [ ] **Step 3: Implement the application service**

Add to `crates/rm-edit/src/rename.rs`:

```rust
use anyhow::Result;
use rm_db::{apply_station_rename, load_station_snapshot, PoleOp, StationRenameOp};
use rusqlite::Connection;

#[derive(Debug)]
pub struct RenameOutcome {
    pub edit_id: i64,
    pub new_station_version_id: i64,
    pub poles: Vec<PoleRenamed>,
}

#[derive(Debug)]
pub struct PoleRenamed {
    pub stop_id: i64,
    pub old_svi: i64,
    pub new_svi: i64,
}

/// Application service: load → plan (pure) → apply (one tx). Zero SQL here.
pub fn rename_station(
    conn: &Connection,
    cmd: StationRename,
    editor: &str,
    note: &str,
) -> Result<RenameOutcome> {
    let snapshot = load_station_snapshot(conn, cmd.station_version_id)?
        .ok_or(EditError::StationNotCurrent(cmd.station_version_id))?;
    let plan = plan_station_rename(&snapshot, cmd)?;
    let op = StationRenameOp {
        station_id: plan.station_id,
        old_station_version_id: plan.station_version_id,
        old_station_name: plan.old_name.clone(),
        new_station_name: plan.new_name.clone(),
        level: plan.level.as_str().to_string(),
        poles: plan
            .poles
            .iter()
            .map(|p| PoleOp {
                stop_id: p.stop_id,
                old_stop_version_id: p.old_stop_version_id,
                new_stop_name: p.new_stop_name.clone(),
            })
            .collect(),
    };
    let applied = apply_station_rename(conn, &op, editor, note).map_err(|e| {
        // rm-db re-validates inside its tx; surface its typed stale-version
        // error as this crate's public EditError
        match e.downcast_ref::<rm_db::EditError>() {
            Some(rm_db::EditError::StationNotCurrent(id)) => EditError::StationNotCurrent(*id).into(),
            Some(rm_db::EditError::NotCurrent(id)) => EditError::StationNotCurrent(*id).into(),
            _ => e,
        }
    })?;
    Ok(RenameOutcome {
        edit_id: applied.edit_id,
        new_station_version_id: applied.new_station_version_id,
        poles: applied
            .poles
            .into_iter()
            .map(|p| PoleRenamed { stop_id: p.stop_id, old_svi: p.old_svi, new_svi: p.new_svi })
            .collect(),
    })
}
```

`NotCurrent(id)` from rm-db (a stale pole) also maps to `EditError::StationNotCurrent` — the id names the offending version; message wording says "station version" which is acceptable for now and revisited when `edit_stop` migrates.

- [ ] **Step 4: Run all tests + clippy**

Run: `cargo test -p rm-edit -p rm-db && cargo clippy -p rm-edit -p rm-db --all-targets`
Expected: PASS, no clippy warnings on new code.

- [ ] **Step 5: Run the full workspace suite**

Run: `cargo test`
Expected: PASS (211+ tests; rm-edit adds 12).

- [ ] **Step 6: Commit**

```bash
git add crates/rm-edit
git commit -m "rm-edit: rename_station — load/plan/apply orchestration, typed stale errors

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```
