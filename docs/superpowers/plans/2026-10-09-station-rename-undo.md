# Scenario 3 — Multi-Row Undo of Station Rename — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a station rename (`entity='station'`, `kind='station_rename'` edit_log row) undoable — the exact inverse of `apply_station_rename` in one transaction.

**Architecture:** Scenario-1 layering, mirrored: `rm-edit` owns a pure `plan_rename_undo` (parse/guard the logged changes JSON) and an `undo_station_rename` app service; `rm-db` owns `load_station_rename_edit` (read) and `apply_station_rename_undo` (one tx: currency guards, inverse steps, undo edit_log row). Zero stale marks; bearing rows move back (PK invariant).

**Tech Stack:** Rust workspace crates `rm-edit`, `rm-db`; rusqlite; serde_json; in-memory SQLite tests.

**Spec:** `docs/superpowers/specs/2026-10-09-station-rename-undo-design.md`

## Global Constraints

- Zero SQL in `rm-edit`; dependency stays one-way `rm-edit → rm-db`.
- Whole-tx primitive: any guard failure writes nothing.
- Undo writes **zero** `stale_mark` rows (bearings move, not delete; no position/name-derived datum touched).
- `stop_bearing` rows MOVE back new_svi → old_svi (INSERT…SELECT then DELETE; skip when absent). Original `computed_at` kept.
- Undo's edit_log row: `entity='station'`, `action='undo'`, `changes='{}'`, `undo_of=<edit_id>`, version_before/after = new/old station_version_id (swapped).
- No `id_sequence` minting — undo reopens existing rows only.
- No rm-web changes.
- Workspace `cargo test` green after every task.
- Pipeline binaries (rm-osm-match etc.) always run with `cargo run -r` — not used in this plan (tests only), but never regress this rule.

**Spec deviations resolved in this plan** (flagged to herry at plan review):
1. `StationRenameEdit` is defined in **rm-db** (like `StationSnapshot`), not rm-edit — rm-db cannot return a type from a crate it doesn't depend on. rm-edit imports it; it migrates with the rest later.
2. Guard error flavors (spec names both `NotUndoable` and `StationNotCurrent` for staleness): station version no longer current → `StationNotCurrent(new_station_version_id)` (mirrors rename's own stale error); pole no longer current → `NotUndoable(edit_id)`.
3. `StationRenameEdit`'s version fields are `Option<i64>` — the edit_log columns are nullable; plan rejects `None` with `NotUndoable`.

## Review Focus

Failure modes the spec implies but task tests must pin explicitly:

1. **Undo after the station was renamed again** → refuse (`StationNotCurrent`), no third version row, no log row. → Task 4 test `undo_refuses_when_station_renamed_again`.
2. **Pole edited post-rename (edit_stop) makes rename non-undoable** → refuse whole tx even when earlier poles were already inverted mid-tx (rollback). → Task 4 test `undo_rolls_back_earlier_pole_inversions_when_later_pole_stale`.
3. **Undo of an undo row** (`action='undo'`) → plan rejects. → Task 2 test `plan_rejects_undo_action`.
4. **Unknown edit_id** → `NotUndoable`, not a crash. → Task 2/5 tests `plan`/`undo_station_rename_unknown_edit_id`.
5. **Pole with no bearing row at undo time** → no row after, no error (INSERT…SELECT is a no-op). → Task 3 test `undo_moves_bearing_row_back_first` (pole 11) and `undo_station_only_touches_no_poles`.

---

### Task 1: rm-db — `load_station_rename_edit` (read side)

**Files:**
- Modify: `crates/rm-db/src/edit.rs` (add after `load_station_snapshot`, ~line 365)
- Modify: `crates/rm-db/src/lib.rs:18-22` (re-export)

**Interfaces:**
- Consumes: existing `rename_fixture` / `all_poles_op` / `apply_station_rename` (test module, edit.rs:750+, :792+)
- Produces: `pub struct StationRenameEdit { edit_id, station_id, action, old_station_version_id: Option<i64>, new_station_version_id: Option<i64>, changes: serde_json::Value }`; `pub fn load_station_rename_edit(conn: &Connection, edit_id: i64) -> Result<Option<StationRenameEdit>>`. Task 3/5 consume these names.

- [ ] **Step 1: Write the failing tests** (in `crates/rm-db/src/edit.rs` test module, after `load_station_snapshot_none_for_stale_or_missing_version`)

```rust
#[test]
fn load_station_rename_edit_returns_parsed_row() {
    let conn = open_in_memory().unwrap();
    rename_fixture(&conn);
    let out = apply_station_rename(&conn, &all_poles_op(), "herry", "n").unwrap();
    let e = load_station_rename_edit(&conn, out.edit_id).unwrap().expect("row exists");
    assert_eq!(e.edit_id, out.edit_id);
    assert_eq!(e.station_id, 1);
    assert_eq!(e.action, "update");
    assert_eq!(e.old_station_version_id, Some(20));
    assert_eq!(e.new_station_version_id, Some(out.new_station_version_id));
    assert_eq!(e.changes["kind"], "station_rename");
    assert_eq!(e.changes["stops"].as_array().unwrap().len(), 2);
}

#[test]
fn load_station_rename_edit_none_for_missing_id() {
    let conn = open_in_memory().unwrap();
    assert!(load_station_rename_edit(&conn, 999).unwrap().is_none());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rm-db load_station_rename_edit`
Expected: FAIL — `cannot find function load_station_rename_edit` (and type).

- [ ] **Step 3: Implement** (edit.rs, after `load_station_snapshot`)

```rust
/// Read-side DTO for the logged station rename (consumed by
/// rm-edit::plan_rename_undo). edit_log is append-only, so a plan parsed
/// from this row is immutable; apply re-validates currency in its tx.
#[derive(Debug)]
pub struct StationRenameEdit {
    pub edit_id: i64,
    pub station_id: i64,
    pub action: String,
    pub old_station_version_id: Option<i64>, // edit_log.version_before
    pub new_station_version_id: Option<i64>, // edit_log.version_after
    pub changes: serde_json::Value,
}

/// Load one edit_log row (any entity — the plan's kind guard is the
/// discriminator) with `changes` parsed as JSON. `None` when `edit_id`
/// does not exist.
pub fn load_station_rename_edit(
    conn: &Connection,
    edit_id: i64,
) -> Result<Option<StationRenameEdit>> {
    let row: (i64, String, Option<i64>, Option<i64>, String)> = match conn
        .query_row(
            "SELECT entity_id, action, version_before, version_after, changes
             FROM edit_log WHERE edit_id = ?1",
            [edit_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?
    {
        Some(v) => v,
        None => return Ok(None),
    };
    let (station_id, action, vb, va, changes) = row;
    Ok(Some(StationRenameEdit {
        edit_id,
        station_id,
        action,
        old_station_version_id: vb,
        new_station_version_id: va,
        changes: serde_json::from_str(&changes)?,
    }))
}
```

Add to the `pub use edit::{...}` list in `crates/rm-db/src/lib.rs`:
`StationRenameEdit, load_station_rename_edit`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rm-db load_station_rename_edit`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/rm-db/src/edit.rs crates/rm-db/src/lib.rs
git commit -m "rm-db: load_station_rename_edit — read side for scenario-3 undo"
```

---

### Task 2: rm-edit — pure `plan_rename_undo` + `EditError::NotUndoable`

**Files:**
- Modify: `crates/rm-edit/src/rename.rs:51-59` (add error variant)
- Create: `crates/rm-edit/src/undo.rs`
- Modify: `crates/rm-edit/src/lib.rs` (add `mod undo; pub use undo::*;`)
- Modify: `crates/rm-edit/Cargo.toml` (add `serde_json.workspace = true`)

**Interfaces:**
- Consumes: `rm_db::StationRenameEdit` (Task 1)
- Produces: `pub struct RenameUndoOp { edit_id: i64, station_id: i64, old_station_version_id: i64, new_station_version_id: i64, poles: Vec<PoleUndo> }`; `pub struct PoleUndo { stop_id: i64, old_svi: i64, new_svi: i64 }`; `pub fn plan_rename_undo(edit: &rm_db::StationRenameEdit) -> Result<RenameUndoOp, EditError>`; `EditError::NotUndoable(i64)`. Task 3/5 consume these.

- [ ] **Step 1: Write the failing tests** (bottom of new `crates/rm-edit/src/undo.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use rm_db::StationRenameEdit;
    use serde_json::json;

    fn edit(action: &str, changes: serde_json::Value) -> StationRenameEdit {
        StationRenameEdit {
            edit_id: 7,
            station_id: 1,
            action: action.into(),
            old_station_version_id: Some(20),
            new_station_version_id: Some(21),
            changes,
        }
    }

    fn rename_changes() -> serde_json::Value {
        json!({
            "kind": "station_rename",
            "level": "AllPoles",
            "old_name": "S",
            "new_name": "台北車站",
            "stops": [
                {"stop_id": 1, "old_svi": 10, "new_svi": 12},
                {"stop_id": 2, "old_svi": 11, "new_svi": 13}
            ]
        })
    }

    #[test]
    fn plan_parses_recorded_shape() {
        let op = plan_rename_undo(&edit("update", rename_changes())).unwrap();
        assert_eq!(op.edit_id, 7);
        assert_eq!(op.station_id, 1);
        assert_eq!(op.old_station_version_id, 20);
        assert_eq!(op.new_station_version_id, 21);
        assert_eq!(op.poles.len(), 2);
        assert_eq!((op.poles[0].stop_id, op.poles[0].old_svi, op.poles[0].new_svi), (1, 10, 12));
    }

    #[test]
    fn plan_accepts_empty_stops_station_only() {
        let mut c = rename_changes();
        c["stops"] = json!([]);
        let op = plan_rename_undo(&edit("update", c)).unwrap();
        assert!(op.poles.is_empty());
    }

    #[test]
    fn plan_rejects_undo_action() {
        let err = plan_rename_undo(&edit("undo", rename_changes())).unwrap_err();
        assert!(matches!(err, EditError::NotUndoable(7)), "{err}");
    }

    #[test]
    fn plan_rejects_unknown_or_missing_kind() {
        let mut c = rename_changes();
        c["kind"] = json!("stop_move");
        assert!(matches!(plan_rename_undo(&edit("update", c)), Err(EditError::NotUndoable(7))));
        let mut c = rename_changes();
        c.as_object_mut().unwrap().remove("kind");
        assert!(matches!(plan_rename_undo(&edit("update", c)), Err(EditError::NotUndoable(7))));
    }

    #[test]
    fn plan_rejects_malformed_stops_and_missing_versions() {
        let mut c = rename_changes();
        c["stops"] = json!("not-an-array");
        assert!(matches!(plan_rename_undo(&edit("update", c)), Err(EditError::NotUndoable(7))));
        let mut c = rename_changes();
        c["stops"][0].as_object_mut().unwrap().remove("new_svi");
        assert!(matches!(plan_rename_undo(&edit("update", c)), Err(EditError::NotUndoable(7))));
        let mut c = rename_changes();
        c.as_object_mut().unwrap().remove("stops");
        assert!(matches!(plan_rename_undo(&edit("update", c)), Err(EditError::NotUndoable(7))));
        let e = edit("update", rename_changes());
        let e = StationRenameEdit { new_station_version_id: None, ..e };
        assert!(matches!(plan_rename_undo(&e), Err(EditError::NotUndoable(7))));
    }
}
```

- [ ] **Step 2: Add the error variant** (`crates/rm-edit/src/rename.rs`, in `enum EditError`)

```rust
    #[error("edit {0} cannot be undone (not current, or is itself an undo)")]
    NotUndoable(i64),
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p rm-edit`
Expected: FAIL — `cannot find function plan_rename_undo in this scope`.

- [ ] **Step 4: Implement** (`crates/rm-edit/src/undo.rs`)

```rust
//! Scenario 3: undo of a station rename — pure plan from the logged
//! changes JSON. The op's rows are historical facts from an append-only
//! log; rm-db re-validates currency inside its apply tx.

use rm_db::StationRenameEdit;

use crate::EditError;

#[derive(Debug, PartialEq)]
pub struct RenameUndoOp {
    pub edit_id: i64,
    pub station_id: i64,
    pub old_station_version_id: i64,
    pub new_station_version_id: i64,
    pub poles: Vec<PoleUndo>, // empty for StationOnly
}

#[derive(Debug, PartialEq)]
pub struct PoleUndo {
    pub stop_id: i64,
    pub old_svi: i64,
    pub new_svi: i64,
}

/// Pure: reject undo-rows and non-rename kinds, copy the logged
/// stop-version pairs into the inverse op.
pub fn plan_rename_undo(edit: &StationRenameEdit) -> Result<RenameUndoOp, EditError> {
    if edit.action == "undo" {
        return Err(EditError::NotUndoable(edit.edit_id));
    }
    if edit.changes.get("kind").and_then(|k| k.as_str()) != Some("station_rename") {
        return Err(EditError::NotUndoable(edit.edit_id));
    }
    let (Some(old_st), Some(new_st)) = (edit.old_station_version_id, edit.new_station_version_id)
    else {
        return Err(EditError::NotUndoable(edit.edit_id));
    };
    let stops = edit
        .changes
        .get("stops")
        .and_then(|s| s.as_array())
        .ok_or(EditError::NotUndoable(edit.edit_id))?;
    let mut poles = Vec::new();
    for s in stops {
        let (Some(stop_id), Some(old_svi), Some(new_svi)) = (
            s.get("stop_id").and_then(|v| v.as_i64()),
            s.get("old_svi").and_then(|v| v.as_i64()),
            s.get("new_svi").and_then(|v| v.as_i64()),
        ) else {
            return Err(EditError::NotUndoable(edit.edit_id));
        };
        poles.push(PoleUndo { stop_id, old_svi, new_svi });
    }
    Ok(RenameUndoOp {
        edit_id: edit.edit_id,
        station_id: edit.station_id,
        old_station_version_id: old_st,
        new_station_version_id: new_st,
        poles,
    })
}
```

Wire up: `crates/rm-edit/src/lib.rs` gets `mod undo;` + `pub use undo::*;` (next to `mod rename;`). `crates/rm-edit/Cargo.toml` `[dependencies]` gets `serde_json.workspace = true` (test `json!` + Value in signatures).

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p rm-edit`
Expected: PASS (existing rename unit + integration tests plus the 5 new).

- [ ] **Step 6: Commit**

```bash
git add crates/rm-edit/src/undo.rs crates/rm-edit/src/rename.rs crates/rm-edit/src/lib.rs crates/rm-edit/Cargo.toml Cargo.lock
git commit -m "rm-edit: plan_rename_undo — pure inverse op from edit_log changes JSON"
```

---

### Task 3: rm-db — `apply_station_rename_undo` (happy inverse, one tx)

**Files:**
- Modify: `crates/rm-db/src/edit.rs` (add after `apply_station_rename`, ~line 542)
- Modify: `crates/rm-db/src/lib.rs:18-22` (re-export)

**Interfaces:**
- Consumes: `rename_fixture`, `all_poles_op`, `apply_station_rename` (existing test helpers)
- Produces: `pub struct StationRenameUndoOp { edit_id, station_id, old_station_version_id, new_station_version_id, poles: Vec<PoleUndoOp> }`; `pub struct PoleUndoOp { stop_id, old_svi, new_svi }`; `pub struct StationRenameUndone { edit_id, restored_station_version_id }`; `pub fn apply_station_rename_undo(conn: &Connection, op: &StationRenameUndoOp, editor: &str, note: &str) -> Result<StationRenameUndone>`. Task 5 consumes all of these.

- [ ] **Step 1: Add test helpers** (test module, after `all_poles_op`)

```rust
    fn station_only_op() -> StationRenameOp {
        StationRenameOp {
            station_id: 1,
            old_station_version_id: 20,
            old_station_name: "S".into(),
            new_station_name: "台北車站".into(),
            level: "StationOnly".into(),
            poles: vec![],
        }
    }

    fn seed_renamed(conn: &Connection) -> crate::StationRenameApplied {
        rename_fixture(conn);
        apply_station_rename(conn, &all_poles_op(), "herry", "rename").unwrap()
    }

    fn undo_op(applied: &crate::StationRenameApplied) -> StationRenameUndoOp {
        StationRenameUndoOp {
            edit_id: applied.edit_id,
            station_id: 1,
            old_station_version_id: 20,
            new_station_version_id: applied.new_station_version_id,
            poles: applied
                .poles
                .iter()
                .map(|p| PoleUndoOp { stop_id: p.stop_id, old_svi: p.old_svi, new_svi: p.new_svi })
                .collect(),
        }
    }
```

- [ ] **Step 2: Write the failing tests — bearing mirror-test FIRST**

```rust
    #[test]
    fn undo_moves_bearing_row_back_first() {
        let conn = open_in_memory().unwrap();
        let applied = seed_renamed(&conn);
        // PK invariant: the row MOVES back new_svi → old_svi, values intact
        apply_station_rename_undo(&conn, &undo_op(&applied), "herry", "revert").unwrap();
        let (deg, src, at): (f64, String, String) = conn.query_row(
            "SELECT bearing_deg, source, computed_at FROM stop_bearing WHERE stop_version_id = 10", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!((deg, src.as_str(), at.as_str()), (123.5, "OSM", "2026-01-02T00:00:00Z"));
        // exactly one row in the table; pole 11 still has none
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM stop_bearing", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn undo_restores_station_and_poles() {
        let conn = open_in_memory().unwrap();
        let applied = seed_renamed(&conn);
        let out = apply_station_rename_undo(&conn, &undo_op(&applied), "herry", "revert").unwrap();
        assert_eq!(out.restored_station_version_id, 20);
        // station: old row current with old name, minted row closed
        let (name, to): (String, Option<String>) = conn.query_row(
            "SELECT station_name, valid_to FROM station_version WHERE station_version_id = 20", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((name.as_str(), to), ("S", None));
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM station_version WHERE valid_to IS NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1, "only the restored row is current");
        // poles: old rows current with original names, minted rows closed,
        // nothing re-minted (still exactly 4 stop_version rows)
        let mut stmt = conn.prepare(
            "SELECT stop_name FROM stop_version WHERE valid_to IS NULL ORDER BY stop_version_id").unwrap();
        let names: Vec<String> = stmt.query_map([], |r| r.get(0)).unwrap()
            .collect::<Result<_, _>>().unwrap();
        assert_eq!(names, vec!["S".to_string(), "S 往".to_string()]);
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM stop_version", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 4, "undo mints nothing");
        // rsv repointed back to the original pole version
        let sv: i64 = conn.query_row(
            "SELECT stop_version_id FROM route_stop_version WHERE route_stop_version_id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(sv, 10);
    }

    #[test]
    fn undo_writes_edit_log_row_and_zero_stale_marks() {
        let conn = open_in_memory().unwrap();
        let applied = seed_renamed(&conn);
        let out = apply_station_rename_undo(&conn, &undo_op(&applied), "herry", "revert!").unwrap();
        let (entity, eid, action, changes, note, undo_of, vb, va): (String, i64, String, String, Option<String>, Option<i64>, Option<i64>, Option<i64>) = conn.query_row(
            "SELECT entity, entity_id, action, changes, note, undo_of, version_before, version_after FROM edit_log WHERE edit_id = ?1",
            [out.edit_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?))).unwrap();
        assert_eq!((entity.as_str(), eid, action.as_str()), ("station", 1, "undo"));
        assert_eq!(changes, "{}");
        assert_eq!(note.as_deref(), Some("revert!"));
        assert_eq!(undo_of, Some(applied.edit_id));
        assert_eq!((vb, va), (Some(applied.new_station_version_id), Some(20)));
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0, "undo of a rename invalidates no derived data");
    }

    #[test]
    fn undo_station_only_touches_no_poles() {
        let conn = open_in_memory().unwrap();
        rename_fixture(&conn);
        let applied = apply_station_rename(&conn, &station_only_op(), "herry", "n").unwrap();
        assert!(applied.poles.is_empty());
        apply_station_rename_undo(&conn, &undo_op(&applied), "herry", "revert").unwrap();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM stop_version WHERE valid_to IS NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2, "both original poles untouched and current");
        let name: String = conn.query_row(
            "SELECT station_name FROM station_version WHERE valid_to IS NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(name, "S");
    }
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p rm-db undo_`
Expected: FAIL — `cannot find type StationRenameUndoOp` / `function apply_station_rename_undo`.

- [ ] **Step 4: Implement** (edit.rs, after `apply_station_rename`)

```rust
/// Write-side op for the station-rename undo (rm-edit maps a
/// RenameUndoOp into this; the rows come from the append-only log).
#[derive(Debug)]
pub struct StationRenameUndoOp {
    pub edit_id: i64,
    pub station_id: i64,
    pub old_station_version_id: i64,
    pub new_station_version_id: i64,
    pub poles: Vec<PoleUndoOp>,
}

#[derive(Debug)]
pub struct PoleUndoOp {
    pub stop_id: i64,
    pub old_svi: i64,
    pub new_svi: i64,
}

#[derive(Debug)]
pub struct StationRenameUndone {
    pub edit_id: i64,
    pub restored_station_version_id: i64,
}

/// Undo a station rename in ONE transaction — the exact inverse of
/// `apply_station_rename`: close/reopen the station version, per pole
/// close/reopen the stop versions, repoint route_stop_version, move the
/// stop_bearing row back (PK invariant). One edit_log undo row, ZERO
/// stale marks (nothing derived changed). Mints nothing.
///
/// Guards (inside the tx): the rename's station version must still be
/// current (`StationNotCurrent` otherwise); every minted pole version
/// must still be current (`NotUndoable` otherwise — a post-rename
/// edit_stop on any pole makes the rename non-undoable).
pub fn apply_station_rename_undo(
    conn: &Connection,
    op: &StationRenameUndoOp,
    editor: &str,
    note: &str,
) -> Result<StationRenameUndone> {
    let tx = conn.unchecked_transaction()?;
    let now = crate::now_rfc3339();

    let n: i64 = tx.query_row(
        "SELECT COUNT(*) FROM station_version WHERE station_version_id = ?1 AND valid_to IS NULL",
        [op.new_station_version_id],
        |r| r.get(0),
    )?;
    if n == 0 {
        return Err(EditError::StationNotCurrent(op.new_station_version_id).into());
    }
    for pole in &op.poles {
        let n: i64 = tx.query_row(
            "SELECT COUNT(*) FROM stop_version WHERE stop_version_id = ?1 AND valid_to IS NULL",
            [pole.new_svi],
            |r| r.get(0),
        )?;
        if n == 0 {
            return Err(EditError::NotUndoable(op.edit_id).into());
        }
    }

    tx.execute(
        "UPDATE station_version SET valid_to = ?1 WHERE station_version_id = ?2",
        params![now, op.new_station_version_id],
    )?;
    tx.execute(
        "UPDATE station_version SET valid_to = NULL WHERE station_version_id = ?1",
        params![op.old_station_version_id],
    )?;

    for pole in &op.poles {
        tx.execute(
            "UPDATE stop_version SET valid_to = ?1 WHERE stop_version_id = ?2",
            params![now, pole.new_svi],
        )?;
        tx.execute(
            "UPDATE stop_version SET valid_to = NULL WHERE stop_version_id = ?1",
            params![pole.old_svi],
        )?;
        tx.execute(
            "UPDATE route_stop_version SET stop_version_id = ?1 WHERE stop_version_id = ?2",
            params![pole.old_svi, pole.new_svi],
        )?;
        // move the bearing row back (none → no-op), mirroring apply's carry
        tx.execute(
            "INSERT INTO stop_bearing (stop_version_id, bearing_deg, source, computed_at)
             SELECT ?1, bearing_deg, source, computed_at FROM stop_bearing
             WHERE stop_version_id = ?2",
            params![pole.old_svi, pole.new_svi],
        )?;
        tx.execute(
            "DELETE FROM stop_bearing WHERE stop_version_id = ?1",
            params![pole.new_svi],
        )?;
    }

    tx.execute(
        "INSERT INTO edit_log (ts, editor, entity, entity_id, action, changes, note, undo_of, version_before, version_after)
         VALUES (?1, ?2, 'station', ?3, 'undo', '{}', ?4, ?5, ?6, ?7)",
        params![
            now,
            editor,
            op.station_id,
            if note.is_empty() { None } else { Some(note) },
            op.edit_id,
            op.new_station_version_id,
            op.old_station_version_id
        ],
    )?;
    let undo_edit_id = tx.last_insert_rowid();
    tx.commit()?;
    Ok(StationRenameUndone {
        edit_id: undo_edit_id,
        restored_station_version_id: op.old_station_version_id,
    })
}
```

Add to `crates/rm-db/src/lib.rs` re-exports: `StationRenameUndoOp, PoleUndoOp, StationRenameUndone, apply_station_rename_undo`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p rm-db`
Expected: PASS — new 4 + all existing (incl. rename apply tests untouched).

- [ ] **Step 6: Commit**

```bash
git add crates/rm-db/src/edit.rs crates/rm-db/src/lib.rs
git commit -m "rm-db: apply_station_rename_undo — one-tx inverse, bearing moves back, zero stale marks"
```

---

### Task 4: rm-db — guard tests (refuse + zero partial state)

**Files:**
- Test: `crates/rm-db/src/edit.rs` (test module, after Task 3's tests)

**Interfaces:**
- Consumes: `apply_station_rename_undo` + helpers from Task 3; `edit_stop` (existing).
- Produces: no new API — pins the guards the spec requires.

- [ ] **Step 1: Write the failing tests** (they exercise implemented code; they must FAIL only if a guard is missing — run them expecting PASS after writing; if any fails, the guard is absent, fix implementation)

```rust
    #[test]
    fn undo_refuses_when_station_renamed_again() {
        let conn = open_in_memory().unwrap();
        let first = seed_renamed(&conn);
        // rename again on the minted version + minted pole versions →
        // first rename no longer current
        let second_op = StationRenameOp {
            old_station_version_id: first.new_station_version_id,
            new_station_name: "站".into(),
            poles: first
                .poles
                .iter()
                .map(|p| PoleOp {
                    stop_id: p.stop_id,
                    old_stop_version_id: p.new_svi,
                    new_stop_name: "站".into(),
                })
                .collect(),
            ..all_poles_op()
        };
        apply_station_rename(&conn, &second_op, "herry", "second").unwrap();
        let err = apply_station_rename_undo(&conn, &undo_op(&first), "herry", "revert").unwrap_err();
        assert!(err.to_string().contains("not the current version"), "{err}");
        // zero partial state: still exactly two manual station versions,
        // two station edit_log rows (two renames, no undo row)
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM station_version WHERE source = 'manual'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2);
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM edit_log WHERE entity = 'station'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn undo_refuses_when_pole_edited_after_rename() {
        let conn = open_in_memory().unwrap();
        let applied = seed_renamed(&conn);
        // post-rename pole edit: edit_stop churns pole 1's minted version
        edit_stop(&conn, applied.poles[0].new_svi,
            &StopEdit { stop_name: Some("shadowed".into()), ..Default::default() }, "h", "n").unwrap();
        let err = apply_station_rename_undo(&conn, &undo_op(&applied), "herry", "revert").unwrap_err();
        assert!(err.to_string().contains("cannot be undone"), "{err}");
        // zero partial state: minted station version still current, no undo row
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM station_version WHERE valid_to IS NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM edit_log WHERE entity = 'station' AND action = 'undo'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn undo_rolls_back_earlier_pole_inversions_when_later_pole_stale() {
        let conn = open_in_memory().unwrap();
        let applied = seed_renamed(&conn);
        // stale pole listed LAST: pole 1 inverts fully inside the tx before
        // pole 2's staleness aborts it — the rollback of that inversion is
        // the property under test
        edit_stop(&conn, applied.poles[1].new_svi,
            &StopEdit { stop_name: Some("shadowed".into()), ..Default::default() }, "h", "n").unwrap();
        let err = apply_station_rename_undo(&conn, &undo_op(&applied), "herry", "revert").unwrap_err();
        assert!(err.to_string().contains("cannot be undone"), "{err}");
        // pole 1's inversion rolled back: minted version current again,
        // original closed, bearing still on the minted svi, rsv untouched
        let (to, on_new): (Option<String>, i64) = conn.query_row(
            "SELECT valid_to, (SELECT COUNT(*) FROM stop_bearing WHERE stop_version_id = ?1)
             FROM stop_version WHERE stop_version_id = ?1",
            [applied.poles[0].new_svi], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert!(to.is_none(), "pole 1's minted version unclosed by rollback");
        assert_eq!(on_new, 1, "bearing row still on pole 1's minted svi");
        let to: Option<String> = conn.query_row(
            "SELECT valid_to FROM stop_version WHERE stop_version_id = 10", [], |r| r.get(0)).unwrap();
        assert!(to.is_some(), "pole 1's original still closed");
        let sv: i64 = conn.query_row(
            "SELECT stop_version_id FROM route_stop_version WHERE route_stop_version_id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(sv, applied.poles[0].new_svi, "rsv still on the minted svi");
    }
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p rm-db undo_`
Expected: PASS (guards implemented in Task 3; any failure = a guard or rollback is genuinely missing → fix `apply_station_rename_undo`, never the test).

- [ ] **Step 3: Run the full crate**

Run: `cargo test -p rm-db`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/rm-db/src/edit.rs
git commit -m "rm-db: undo guard tests — stale station/pole refuse with zero partial state"
```

---

### Task 5: rm-edit — `undo_station_rename` app service + e2e

**Files:**
- Modify: `crates/rm-edit/src/undo.rs` (add service + outcome)
- Create: `crates/rm-edit/tests/undo.rs`

**Interfaces:**
- Consumes: `rm_db::{load_station_rename_edit, apply_station_rename_undo, StationRenameUndoOp, PoleUndoOp}` (Tasks 1, 3); `plan_rename_undo`, `RenameUndoOp` (Task 2); `rename_station` (existing).
- Produces: `pub struct UndoOutcome { pub edit_id: i64, pub restored_station_version_id: i64 }`; `pub fn undo_station_rename(conn: &Connection, edit_id: i64, editor: &str, note: &str) -> anyhow::Result<UndoOutcome>`.

- [ ] **Step 1: Write the failing tests** (new `crates/rm-edit/tests/undo.rs`; fixture is the same shape as `tests/rename.rs` — copy it verbatim)

```rust
//! Integration: undo_station_rename end-to-end on an in-memory DB.

use rm_db::open_in_memory;
use rm_edit::{rename_station, undo_station_rename, EditError, RenameLevel, StationRename};
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
        conn.execute("INSERT INTO stop (stop_id, ext_uid, created_at) VALUES (?1, 'tcg:stop:' || ?1, '2026-01-01T00:00:00Z')", [sid]).unwrap();
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
fn undo_station_rename_restores_names() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let renamed = rename_station(&conn, cmd("台北車站", RenameLevel::AllPoles), "herry", "cjk").unwrap();
    let out = undo_station_rename(&conn, renamed.edit_id, "herry", "revert").unwrap();
    assert_eq!(out.restored_station_version_id, 20);
    let name: String = conn.query_row(
        "SELECT station_name FROM station_version WHERE valid_to IS NULL", [], |r| r.get(0)).unwrap();
    assert_eq!(name, "S");
    let mut stmt = conn.prepare(
        "SELECT stop_name FROM stop_version WHERE valid_to IS NULL ORDER BY stop_version_id").unwrap();
    let names: Vec<String> = stmt.query_map([], |r| r.get(0)).unwrap()
        .collect::<Result<_, _>>().unwrap();
    assert_eq!(names, vec!["S".to_string(), "S 往".to_string()]);
    // bearing back on the original pole version
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM stop_bearing WHERE stop_version_id = 10", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 1);
}

#[test]
fn undo_station_rename_refuses_undo_of_undo() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let renamed = rename_station(&conn, cmd("台北車站", RenameLevel::AllPoles), "herry", "").unwrap();
    let u = undo_station_rename(&conn, renamed.edit_id, "herry", "").unwrap();
    let err = undo_station_rename(&conn, u.edit_id, "herry", "").unwrap_err();
    assert!(err.to_string().contains("cannot be undone"), "{err}");
    let _ = EditError::NotUndoable(0); // variant exists and is public
}

#[test]
fn undo_station_rename_unknown_edit_id() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let err = undo_station_rename(&conn, 999, "herry", "").unwrap_err();
    assert!(err.to_string().contains("cannot be undone"), "{err}");
}

#[test]
fn undo_station_rename_refuses_when_pole_edited_after_rename() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let renamed = rename_station(&conn, cmd("台北車站", RenameLevel::AllPoles), "herry", "").unwrap();
    rm_db::edit_stop(&conn, renamed.poles[0].new_svi,
        &rm_db::StopEdit { stop_name: Some("shadowed".into()), ..Default::default() }, "h", "n").unwrap();
    let err = undo_station_rename(&conn, renamed.edit_id, "herry", "").unwrap_err();
    assert!(err.to_string().contains("cannot be undone"), "{err}");
}
```

Note: `rename_station`'s `RenameOutcome.poles` entries expose `new_svi` (rename.rs:114-118) — used above.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rm-edit --test undo`
Expected: FAIL — `cannot find function undo_station_rename`.

- [ ] **Step 3: Implement** (`crates/rm-edit/src/undo.rs`, after `plan_rename_undo`)

```rust
use anyhow::Result;
use rm_db::{apply_station_rename_undo, load_station_rename_edit, PoleUndoOp, StationRenameUndoOp};
use rusqlite::Connection;

#[derive(Debug)]
pub struct UndoOutcome {
    pub edit_id: i64, // the undo's own edit_log row
    pub restored_station_version_id: i64,
}

/// Application service: load the logged rename → plan (pure) → apply the
/// inverse in one tx. Zero SQL here.
pub fn undo_station_rename(
    conn: &Connection,
    edit_id: i64,
    editor: &str,
    note: &str,
) -> Result<UndoOutcome> {
    let edit = load_station_rename_edit(conn, edit_id)?
        .ok_or(EditError::NotUndoable(edit_id))?;
    let plan = plan_rename_undo(&edit)?;
    let op = StationRenameUndoOp {
        edit_id: plan.edit_id,
        station_id: plan.station_id,
        old_station_version_id: plan.old_station_version_id,
        new_station_version_id: plan.new_station_version_id,
        poles: plan
            .poles
            .iter()
            .map(|p| PoleUndoOp { stop_id: p.stop_id, old_svi: p.old_svi, new_svi: p.new_svi })
            .collect(),
    };
    let undone = apply_station_rename_undo(conn, &op, editor, note).map_err(|e| {
        // rm-db re-validates inside its tx; surface its typed errors as
        // this crate's public EditError (same mapping as rename_station)
        match e.downcast_ref::<rm_db::EditError>() {
            Some(rm_db::EditError::StationNotCurrent(id)) => {
                EditError::StationNotCurrent(*id).into()
            }
            Some(rm_db::EditError::NotUndoable(id)) => EditError::NotUndoable(*id).into(),
            _ => e,
        }
    })?;
    Ok(UndoOutcome {
        edit_id: undone.edit_id,
        restored_station_version_id: undone.restored_station_version_id,
    })
}
```

Fix the module's imports: `use crate::EditError;` already present from Task 2; merge the two `use rm_db::…` groups into one.

- [ ] **Step 4: Run all tests**

Run: `cargo test -p rm-edit`
Expected: PASS — new 4 integration + Task 2 unit tests + existing rename tests.

- [ ] **Step 5: Workspace green**

Run: `cargo test --workspace`
Expected: PASS (192 prior tests + ~14 new).

- [ ] **Step 6: Commit**

```bash
git add crates/rm-edit/src/undo.rs crates/rm-edit/tests/undo.rs
git commit -m "rm-edit: undo_station_rename — load/plan/apply service, typed NotUndoable errors"
```
