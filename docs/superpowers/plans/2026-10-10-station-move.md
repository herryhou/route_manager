# Station Move (Scenario 6) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the station-move primitive (`station_version` lat/lon reposition, append-only) plus a kind-agnostic station undo that handles both rename and move.

**Architecture:** rm-edit keeps owning domain + services (zero SQL); rm-db gains `apply_station_move` and a generalized `apply_station_undo` dispatching on a `StationUndoOp` enum. `load_station_rename_edit`/`StationRenameEdit` rename to `load_station_edit`/`StationEdit`. rm-web untouched.

**Tech Stack:** Rust workspace, rusqlite, serde_json, thiserror; tests run on in-memory SQLite via `rm_db::open_in_memory()`.

**Spec:** `docs/superpowers/specs/2026-10-10-station-move-design.md`

## Global Constraints

- rm-edit contains ZERO SQL; all persistence is rm-db. Dependency one-way rm-edit → rm-db.
- One transaction per edit; ALL currency guards run before ANY write; guard failure ⇒ zero partial state.
- Every close/reopen UPDATE carries a `== 1` rows-affected check (bogus id ⇒ typed error, never a silent 0-row).
- Minted rows get `source = 'manual'`; undo mints nothing.
- Station move writes **zero stale marks** and touches no poles, no `stop_bearing`, no `route_stop_version`.
- edit_log move row: `entity='station'`, `action='update'`, `changes={"kind":"station_move","latitude":{"old":…,"new":…},"longitude":{"old":…,"new":…}}`, `version_before/after` = old/new station version ids. Undo row: `action='undo'`, `changes='{}'`, `undo_of` set, versions swapped.
- Coords stored verbatim — no range or no-op guard in the plan (split by design with rm-web's future 400s).
- Station-side undo is kind-strict: only `kind ∈ {"station_rename","station_move"}` is undoable; kind-less rows ⇒ `NotUndoable`.
- Tests run with `cargo test -p <crate>`; workspace green at the end (235 tests today).

## Review Focus

1. **Race: version goes stale between load and apply** — expectation: typed `StationNotCurrent`, database byte-identical (zero partial). Pinned: Task 2 (move) and Task 7 (undo) stale-guard tests.
2. **Currency drift: undoing a move after a second move/rename** — expectation: refusal, zero partial, no undo log row. Pinned: Task 7 test, Task 8 service-level test.
3. **Foreign/corrupt log rows fed to station undo** (stop row, kind-less row, undo row) — expectation: `NotUndoable`, nothing written. Pinned: Task 6 unit tests + existing `undo_station_rename_refuses_stop_edit_row` regression.
4. **Bogus ids to the db undo** (unknown version ids) — expectation: `== 1` / COUNT guards reject before any write. Pinned: Task 7 zero-partial tests.
5. **Large manual move flips rm-verify `station:recompute` clustering** — expectation: verify-time warning only; grouping repair is out of scope by spec, so no test pins it (documented accepted semantics).

---

### Task 1: rm-db — `StationSnapshot` carries coords

**Files:**
- Modify: `crates/rm-db/src/edit.rs:452-508` (`StationSnapshot` struct + `load_station_snapshot`)
- Test: `crates/rm-db/src/edit.rs` (tests mod, `load_station_snapshot_returns_station_and_current_poles`)

**Interfaces:**
- Produces: `StationSnapshot { station_id: i64, station_version_id: i64, station_name: String, latitude: f64, longitude: f64, poles: Vec<PoleSnapshot> }` — Task 4's plan consumes `latitude`/`longitude`.

- [ ] **Step 1: Write the failing test** — extend the existing test `load_station_snapshot_returns_station_and_current_poles` (crates/rm-db/src/edit.rs:1016) with coord asserts:

```rust
    #[test]
    fn load_station_snapshot_returns_station_and_current_poles() {
        let conn = open_in_memory().unwrap();
        let _ = fixture(&conn);
        let snap = load_station_snapshot(&conn, 1).unwrap().expect("current station version");
        assert_eq!(snap.station_id, 1);
        assert_eq!(snap.station_version_id, 1);
        assert_eq!(snap.station_name, "S");
        assert_eq!((snap.latitude, snap.longitude), (24.0, 120.0));
        assert_eq!(snap.poles.len(), 1);
        assert_eq!((snap.poles[0].stop_id, snap.poles[0].stop_version_id, snap.poles[0].stop_name.as_str()), (1, 10, "Old"));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rm-db load_station_snapshot_returns`
Expected: FAIL — "no field `latitude` on `StationSnapshot`"

- [ ] **Step 3: Implement** — struct gains two fields, SELECT extends by two columns:

```rust
/// Read-side snapshot for station-level edits (rm-edit::rename_station /
/// rm-edit::move_station): the current station version plus its poles'
/// current stop versions.
#[derive(Debug)]
pub struct StationSnapshot {
    pub station_id: i64,
    pub station_version_id: i64,
    pub station_name: String,
    pub latitude: f64,
    pub longitude: f64,
    pub poles: Vec<PoleSnapshot>,
}
```

In `load_station_snapshot` change the row query and unpack:

```rust
    let row: (i64, String, f64, f64) = match conn
        .query_row(
            "SELECT station_id, station_name, latitude, longitude FROM station_version
             WHERE station_version_id = ?1 AND valid_to IS NULL",
            [station_version_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?
    {
        Some(v) => v,
        None => return Ok(None),
    };
    let (station_id, station_name, latitude, longitude) = row;
```

and the constructor at the end:

```rust
    Ok(Some(StationSnapshot {
        station_id,
        station_version_id,
        station_name,
        latitude,
        longitude,
        poles,
    }))
```

- [ ] **Step 4: Fix struct-literal constructors the change broke.** Grep for `StationSnapshot {` literals:

Run: `grep -rn 'StationSnapshot {' crates/ --include='*.rs'`
Known: `crates/rm-edit/src/rename.rs` tests `fn snapshot()` (adds `latitude: 24.0, longitude: 120.0,` after `station_name`). Fix every literal the same way; rename callers that only read fields need nothing.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p rm-db -p rm-edit`
Expected: PASS (all)

- [ ] **Step 6: Commit**

```bash
git add crates/rm-db/src/edit.rs crates/rm-edit/src/rename.rs
git commit -m "rm-db: StationSnapshot carries station coords"
```

---

### Task 2: rm-db — `apply_station_move`

**Files:**
- Modify: `crates/rm-db/src/edit.rs` (new op/result structs + fn, placed after `apply_station_rename`, before the undo section at line ~821)
- Modify: `crates/rm-db/src/lib.rs:18-27` (re-export list)
- Test: `crates/rm-db/src/edit.rs` (tests mod)

**Interfaces:**
- Produces:
  - `pub struct StationMoveOp { pub station_id: i64, pub old_station_version_id: i64, pub new_latitude: f64, pub new_longitude: f64 }`
  - `pub struct StationMoveApplied { pub edit_id: i64, pub new_station_version_id: i64 }`
  - `pub fn apply_station_move(conn: &Connection, op: &StationMoveOp, editor: &str, note: &str) -> Result<StationMoveApplied>`

- [ ] **Step 1: Write the failing tests** — add to the tests mod (helpers `rename_fixture`, `params!` already in scope):

```rust
    fn move_op(old: i64) -> StationMoveOp {
        StationMoveOp {
            station_id: 1,
            old_station_version_id: old,
            new_latitude: 24.001,
            new_longitude: 120.001,
        }
    }

    #[test]
    fn apply_station_move_appends_version_zero_marks() {
        let conn = open_in_memory().unwrap();
        let old = rename_fixture(&conn);
        let (sv_before, sb_before, rsv_before): (i64, i64, i64) = (
            conn.query_row("SELECT COUNT(*) FROM stop_version", [], |r| r.get(0)).unwrap(),
            conn.query_row("SELECT COUNT(*) FROM stop_bearing", [], |r| r.get(0)).unwrap(),
            conn.query_row("SELECT COUNT(*) FROM route_stop_version", [], |r| r.get(0)).unwrap(),
        );
        let out = apply_station_move(&conn, &move_op(old), "herry", "fix").unwrap();
        assert_eq!(out.new_station_version_id, 21);
        // old closed, new current + manual, coords applied, name/group carried
        let (to, _): (Option<String>, Option<String>) = conn.query_row(
            "SELECT valid_to, source FROM station_version WHERE station_version_id = ?1",
            [old], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert!(to.is_some());
        let (lat, lon, name, group, src): (f64, f64, String, i64, String) = conn.query_row(
            "SELECT latitude, longitude, station_name, station_group_id, source
             FROM station_version WHERE station_version_id = ?1",
            [out.new_station_version_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).unwrap();
        assert_eq!((lat, lon, name.as_str(), group, src.as_str()), (24.001, 120.001, "S", 1, "manual"));
        // zero collateral: poles, bearings, rsv untouched; zero stale marks
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM stop_version", [], |r| r.get(0)).unwrap(), sv_before);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM stop_bearing", [], |r| r.get(0)).unwrap(), sb_before);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM route_stop_version", [], |r| r.get(0)).unwrap(), rsv_before);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0)).unwrap(), 0);
        // edit_log: {kind + field:{old,new}} shape
        let (ent, act, changes, vb, va): (String, String, String, i64, i64) = conn.query_row(
            "SELECT entity, action, changes, version_before, version_after FROM edit_log WHERE edit_id = ?1",
            [out.edit_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).unwrap();
        assert_eq!((ent.as_str(), act.as_str(), vb, va), ("station", "update", old, out.new_station_version_id));
        let c: serde_json::Value = serde_json::from_str(&changes).unwrap();
        assert_eq!(c["kind"], "station_move");
        assert_eq!((c["latitude"]["old"], c["latitude"]["new"]), (24.0, 24.001));
        assert_eq!((c["longitude"]["old"], c["longitude"]["new"]), (120.0, 120.001));
    }

    #[test]
    fn apply_station_move_refuses_stale_zero_partial() {
        let conn = open_in_memory().unwrap();
        let old = rename_fixture(&conn);
        let first = apply_station_move(&conn, &move_op(old), "herry", "a").unwrap();
        let err = apply_station_move(&conn, &move_op(old), "herry", "b").unwrap_err();
        assert!(err.to_string().contains("station version"), "{err}");
        // zero partial: still exactly the two versions and one log row
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM station_version", [], |r| r.get(0)).unwrap(), 2);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM station_version WHERE valid_to IS NULL", [], |r| r.get(0)).unwrap(), 1);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM edit_log", [], |r| r.get(0)).unwrap(), 1);
        let _ = first;
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rm-db apply_station_move`
Expected: FAIL — cannot find function `apply_station_move`

- [ ] **Step 3: Implement** — insert after `apply_station_rename` (edit.rs:819):

```rust
/// Write-side op for the station move (rm-edit maps a StationMovePlan into
/// this; rm-db knows nothing about the domain types).
#[derive(Debug)]
pub struct StationMoveOp {
    pub station_id: i64,
    pub old_station_version_id: i64,
    pub new_latitude: f64,
    pub new_longitude: f64,
}

#[derive(Debug)]
pub struct StationMoveApplied {
    pub edit_id: i64,
    pub new_station_version_id: i64,
}

/// Apply a station move in ONE transaction: version-append the station with
/// the new coords (`source='manual'`), everything else carried. Nothing
/// references station_version_id and no derived table reads station coords,
/// so there is no repoint step and ZERO stale marks. One edit_log row whose
/// changes carry `{kind, latitude:{old,new}, longitude:{old,new}}` for the
/// kind-agnostic station undo.
pub fn apply_station_move(
    conn: &Connection,
    op: &StationMoveOp,
    editor: &str,
    note: &str,
) -> Result<StationMoveApplied> {
    let tx = conn.unchecked_transaction()?;
    let now = crate::now_rfc3339();

    // re-validate inside the tx: the old station version must still be current
    let (station_group_id, station_name, old_lat, old_lon): (i64, String, f64, f64) = match tx
        .query_row(
            "SELECT station_group_id, station_name, latitude, longitude FROM station_version
             WHERE station_version_id = ?1 AND valid_to IS NULL",
            [op.old_station_version_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
    {
        Ok(v) => v,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Err(EditError::StationNotCurrent(op.old_station_version_id).into())
        }
        Err(e) => return Err(e.into()),
    };
    let n = tx.execute(
        "UPDATE station_version SET valid_to = ?1 WHERE station_version_id = ?2",
        params![now, op.old_station_version_id],
    )?;
    if n != 1 {
        return Err(EditError::StationNotCurrent(op.old_station_version_id).into());
    }
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
            station_name,
            op.new_latitude,
            op.new_longitude,
            now
        ],
    )?;

    let changes = json!({
        "kind": "station_move",
        "latitude": {"old": old_lat, "new": op.new_latitude},
        "longitude": {"old": old_lon, "new": op.new_longitude},
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
    Ok(StationMoveApplied { edit_id, new_station_version_id })
}
```

(The `== 1` close check is defense-in-depth behind the in-tx SELECT; it is unreachable via the public API — no dedicated test.)

- [ ] **Step 4: Export** — in `crates/rm-db/src/lib.rs` add `StationMoveOp, apply_station_move, StationMoveApplied,` to the `pub use edit::{ … }` list.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p rm-db`
Expected: PASS (all, including the two new)

- [ ] **Step 6: Commit**

```bash
git add crates/rm-db/src/edit.rs crates/rm-db/src/lib.rs
git commit -m "rm-db: apply_station_move — append-only station reposition, zero stale marks"
```

---

### Task 3: rm-edit — shared db-error mappers

**Files:**
- Modify: `crates/rm-edit/src/lib.rs` (two `pub(crate)` fns)
- Modify: `crates/rm-edit/src/stop.rs:59-68` (delete local `map_db_err`, use the shared one)
- Modify: `crates/rm-edit/src/rename.rs:156-164` (inline match → `map_station_db_err`)
- Modify: `crates/rm-edit/src/undo.rs:96-105` (inline match → `map_db_err`)
- Test: `crates/rm-edit/tests/rename.rs` (one new test)

**Interfaces:**
- Produces: `crate::map_db_err(anyhow::Error) -> anyhow::Error` (1:1 four-variant map) and `crate::map_station_db_err(anyhow::Error) -> anyhow::Error` (additionally remaps `NotCurrent` → `StationNotCurrent`). Task 4's service uses `map_station_db_err`; Task 8's `undo_station` uses `map_db_err`.

- [ ] **Step 1: Write the failing test** — add to `crates/rm-edit/tests/rename.rs` (fixture + helpers already there; this pins the scenario-1 remap semantics that the helper must preserve):

```rust
#[test]
fn rename_surfaces_stale_pole_as_station_error() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    // close pole 1's current version behind apply's back: the rename op
    // still lists svi 10, so apply's per-pole guard fires
    conn.execute_batch(
        "UPDATE stop_version SET valid_to = '2026-01-02T00:00:00Z' WHERE stop_version_id = 10",
    )
    .unwrap();
    let err = rename_station(&conn, cmd("N", RenameLevel::AllPoles), "herry", "").unwrap_err();
    // scenario-1 semantics: a stale pole surfaces as the station-level error
    assert!(err.to_string().contains("station version"), "{err}");
}
```

- [ ] **Step 2: Run test to verify it fails for the right reason**

Run: `cargo test -p rm-edit --test rename rename_surfaces_stale_pole`
Expected: this may already PASS (the inline match exists today). If it passes, it is a characterization test — keep it and continue; the step's real gate is Step 5 (all suites green after the refactor).

- [ ] **Step 3: Implement the shared mappers** — in `crates/rm-edit/src/lib.rs`:

```rust
//! rm-edit: manual-edit domain and application services. Zero SQL —
//! persistence lives in rm-db; this crate orchestrates and decides.

mod rename;
mod station_move;
mod stop;
mod undo;

pub use rename::*;
pub use station_move::*;
pub use stop::*;
pub use undo::*;

/// rm-db's internal edit errors surface 1:1 as this crate's public
/// EditError (stop-side semantics).
pub(crate) fn map_db_err(e: anyhow::Error) -> anyhow::Error {
    match e.downcast_ref::<rm_db::EditError>() {
        Some(rm_db::EditError::NotCurrent(id)) => EditError::NotCurrent(*id).into(),
        Some(rm_db::EditError::StationNotCurrent(id)) => EditError::StationNotCurrent(*id).into(),
        Some(rm_db::EditError::UnknownStation(id)) => EditError::UnknownStation(*id).into(),
        Some(rm_db::EditError::NotUndoable(id)) => EditError::NotUndoable(*id).into(),
        _ => e,
    }
}

/// Station-flavored mapping: a stale pole version (NotCurrent, fired by a
/// rename's per-pole guard) surfaces as the station-level error — rename
/// and move act on the station as a whole.
pub(crate) fn map_station_db_err(e: anyhow::Error) -> anyhow::Error {
    if let Some(rm_db::EditError::NotCurrent(id)) = e.downcast_ref::<rm_db::EditError>() {
        return EditError::StationNotCurrent(*id).into();
    }
    map_db_err(e)
}
```

(`mod station_move;` lands with Task 4 — add it there instead if Task 4 has not run yet. In this task, add only the two fns and leave the module list untouched.)

- [ ] **Step 4: Rewire call sites**
  - `stop.rs`: delete the local `map_db_err` (lines 59-68); add `use crate::map_db_err;` alongside `use crate::EditError;`.
  - `rename.rs`: replace the inline `match e.downcast_ref::<rm_db::EditError>() { … }` block in `rename_station` with `.map_err(crate::map_station_db_err)?`.
  - `undo.rs`: replace the inline match in `undo_station_rename` with `.map_err(crate::map_db_err)?`.

- [ ] **Step 5: Run all rm-edit + rm-db tests**

Run: `cargo test -p rm-edit -p rm-db`
Expected: PASS (all — behavior byte-identical)

- [ ] **Step 6: Commit**

```bash
git add crates/rm-edit/src/
git commit -m "rm-edit: shared map_db_err/map_station_db_err — fold inline rm-db error matches"
```

---

### Task 4: rm-edit — `StationMove` plan + `move_station` service

**Files:**
- Create: `crates/rm-edit/src/station_move.rs`
- Modify: `crates/rm-edit/src/lib.rs` (`mod station_move;` + `pub use station_move::*;` — if not already added in Task 3)
- Test: `crates/rm-edit/tests/move.rs` (new file)

**Interfaces:**
- Consumes: `rm_db::load_station_snapshot` (Task 1 shape, with `latitude`/`longitude`), `rm_db::apply_station_move` (Task 2), `crate::map_station_db_err` (Task 3).
- Produces:
  - `pub struct StationMove { pub station_version_id: i64, pub latitude: f64, pub longitude: f64 }`
  - `pub struct StationMovePlan { pub station_id: i64, pub station_version_id: i64, pub old_latitude: f64, pub old_longitude: f64, pub new_latitude: f64, pub new_longitude: f64 }`
  - `pub struct MoveOutcome { pub edit_id: i64, pub new_station_version_id: i64 }`
  - `pub fn plan_station_move(snapshot: &rm_db::StationSnapshot, cmd: StationMove) -> Result<StationMovePlan, EditError>` (pure, total — the only failure is the upstream stale-svi load)
  - `pub fn move_station(conn: &Connection, cmd: StationMove, editor: &str, note: &str) -> anyhow::Result<MoveOutcome>`

- [ ] **Step 1: Write the failing tests** — create `crates/rm-edit/tests/move.rs`:

```rust
//! Station move: load → plan → apply roundtrips through the service.

use rm_db::open_in_memory;
use rm_edit::{EditError, StationMove};

/// Same shape as tests/rename.rs's fixture: station 20 ("S") with poles
/// 10/11, rsv row, bearing row, id sequences seeded.
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

fn cmd(svi: i64) -> StationMove {
    StationMove { station_version_id: svi, latitude: 24.5, longitude: 120.5 }
}

#[test]
fn move_station_moves_end_to_end() {
    let conn = open_in_memory().unwrap();
    let old = fixture(&conn);
    let out = move_station(&conn, cmd(old), "herry", "fix").unwrap();
    assert_eq!(out.new_station_version_id, 21);
    let (lat, lon, name, src): (f64, f64, String, String) = conn
        .query_row(
            "SELECT latitude, longitude, station_name, source FROM station_version
             WHERE valid_to IS NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!((lat, lon, name.as_str(), src.as_str()), (24.5, 120.5, "S", "manual"));
    // zero stale marks, poles untouched (no manual stop versions minted)
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0);
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM stop_version WHERE source = 'manual'", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0);
    let _ = EditError::StationNotCurrent(0); // variant is public
}

#[test]
fn move_station_rejects_stale_version() {
    let conn = open_in_memory().unwrap();
    let old = fixture(&conn);
    move_station(&conn, cmd(old), "herry", "first").unwrap();
    // double-submit: 20 is no longer current
    let err = move_station(&conn, cmd(old), "herry", "second").unwrap_err();
    assert!(err.to_string().contains("not the current version"), "{err}");
}

#[test]
fn move_station_rejects_unknown_version() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let err = move_station(&conn, cmd(999), "herry", "").unwrap_err();
    assert!(err.to_string().contains("not the current version"), "{err}");
}

use rm_edit::move_station;
```

(`use` lines at top in the real file — shown at bottom only to keep the diff obvious; put all `use` statements at the top: `use rm_db::open_in_memory; use rm_edit::{move_station, EditError, StationMove};`)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rm-edit --test move`
Expected: FAIL — cannot find `move_station`/`StationMove`

- [ ] **Step 3: Implement** — create `crates/rm-edit/src/station_move.rs`:

```rust
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
```

In `crates/rm-edit/src/lib.rs` add `mod station_move;` and `pub use station_move::*;` (skip if Task 3 already added them).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rm-edit`
Expected: PASS (all, including the 3 new)

- [ ] **Step 5: Commit**

```bash
git add crates/rm-edit/src/station_move.rs crates/rm-edit/src/lib.rs crates/rm-edit/tests/move.rs
git commit -m "rm-edit: move_station — station reposition primitive (load→plan→apply)"
```

---

### Task 5: rename the edit-log loader — `load_station_edit` / `StationEdit`

**Files:**
- Modify: `crates/rm-db/src/edit.rs:564-605` (DTO + fn rename)
- Modify: `crates/rm-db/src/lib.rs:18-27` (re-export names)
- Modify: `crates/rm-edit/src/undo.rs` (import + call site)
- Test: none new — rename is behavior-preserving; all suites must stay green

**Interfaces:**
- Produces: `pub struct StationEdit { pub edit_id: i64, pub station_id: i64, pub action: String, pub old_station_version_id: Option<i64>, pub new_station_version_id: Option<i64>, pub changes: serde_json::Value }` and `pub fn load_station_edit(conn: &Connection, edit_id: i64) -> Result<Option<StationEdit>>` — Task 6's planner consumes `&StationEdit`.

- [ ] **Step 1: Rename mechanically.** In `crates/rm-db/src/edit.rs`: `StationRenameEdit` → `StationEdit` (struct at :568, fn return type at :583, constructor at :597), `load_station_rename_edit` → `load_station_edit` (:580). Update the doc comment to say "logged station edit (rename or move — consumed by rm-edit::plan_station_undo)". In `crates/rm-db/src/lib.rs` re-export list: `StationRenameEdit, load_station_rename_edit` → `StationEdit, load_station_edit`. In `crates/rm-edit/src/undo.rs`: `use rm_db::StationRenameEdit;` → `use rm_db::StationEdit;` and the `load_station_rename_edit` call → `load_station_edit` (plus every `StationRenameEdit` in undo.rs's `mod tests` helper).

Run: `grep -rn 'StationRenameEdit\|load_station_rename_edit' crates/ --include='*.rs'` to catch every site (known: rm-db edit.rs + lib.rs, rm-edit src/undo.rs incl. tests mod).

- [ ] **Step 2: Run tests to verify nothing broke**

Run: `cargo test --workspace`
Expected: PASS (all 235+)

- [ ] **Step 3: Commit**

```bash
git add crates/rm-db/src/ crates/rm-edit/src/
git commit -m "rm-db: load_station_rename_edit → load_station_edit — kind-agnostic station log read"
```

---

### Task 6: rm-edit — `plan_station_undo` (kind dispatch)

**Files:**
- Modify: `crates/rm-edit/src/undo.rs` (planner generalization; service rename lands in Task 8)
- Test: `crates/rm-edit/src/undo.rs` (tests mod — migrate + extend)

**Interfaces:**
- Consumes: `rm_db::StationEdit` (Task 5).
- Produces:
  - `pub struct StationMoveUndoOp { pub edit_id: i64, pub station_id: i64, pub old_station_version_id: i64, pub new_station_version_id: i64 }`
  - `pub enum StationUndoOp { Rename(RenameUndoOp), Move(StationMoveUndoOp) }`
  - `pub fn plan_station_undo(edit: &StationEdit) -> Result<StationUndoOp, EditError>`
  - `RenameUndoOp` / `PoleUndo` keep name + shape. Task 8's service matches on the enum.

- [ ] **Step 1: Migrate the unit tests and add move-kind cases** — in undo.rs's tests mod, the `edit()` helper's return type becomes `StationEdit`; every `plan_rename_undo(&e).unwrap()` becomes `plan_station_undo(&e).unwrap()` with the success asserts going through the `Rename` arm:

```rust
    #[test]
    fn plan_parses_recorded_shape() {
        let op = plan_station_undo(&edit("update", rename_changes())).unwrap();
        let StationUndoOp::Rename(op) = op else { panic!("expected rename arm") };
        assert_eq!(op.edit_id, 7);
        assert_eq!(op.station_id, 1);
        assert_eq!(op.old_station_version_id, 20);
        assert_eq!(op.new_station_version_id, 21);
        assert_eq!(op.poles.len(), 2);
        assert_eq!((op.poles[0].stop_id, op.poles[0].old_svi, op.poles[0].new_svi), (1, 10, 12));
    }
```

New tests:

```rust
    fn move_changes() -> serde_json::Value {
        json!({
            "kind": "station_move",
            "latitude": {"old": 24.0, "new": 24.5},
            "longitude": {"old": 120.0, "new": 120.5}
        })
    }

    #[test]
    fn plan_move_kind_extracts_version_pair() {
        let op = plan_station_undo(&edit("update", move_changes())).unwrap();
        assert_eq!(
            op,
            StationUndoOp::Move(StationMoveUndoOp {
                edit_id: 7,
                station_id: 1,
                old_station_version_id: 20,
                new_station_version_id: 21,
            })
        );
    }

    #[test]
    fn plan_move_reads_nothing_but_kind() {
        // coords are restore-by-reopen; even a corrupt/absent payload under
        // the move kind still plans — the versions come from the log columns
        let op = plan_station_undo(&edit("update", json!({"kind": "station_move"}))).unwrap();
        assert!(matches!(op, StationUndoOp::Move(_)));
    }

    #[test]
    fn plan_rejects_move_action_undo() {
        let err = plan_station_undo(&edit("undo", move_changes())).unwrap_err();
        assert!(matches!(err, EditError::NotUndoable(7)), "{err}");
    }
```

(`StationUndoOp` needs `PartialEq` for the first assert — derive it on the enum and both op structs; `RenameUndoOp` already derives it.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rm-edit --lib`
Expected: FAIL — `plan_station_undo` / `StationUndoOp` not found

- [ ] **Step 3: Implement** — in undo.rs, generalize the planner (guards preserved verbatim, kind dispatch added; `plan_rename_undo` becomes the private rename arm):

```rust
#[derive(Debug, PartialEq)]
pub struct StationMoveUndoOp {
    pub edit_id: i64,
    pub station_id: i64,
    pub old_station_version_id: i64,
    pub new_station_version_id: i64,
}

#[derive(Debug, PartialEq)]
pub enum StationUndoOp {
    Rename(RenameUndoOp),
    Move(StationMoveUndoOp),
}

/// Pure guard + dispatch: only a non-undo station row whose `changes.kind`
/// is a known station edit is invertible. Station-side undo is kind-strict
/// (every station row is post-scenario-1 and carries a kind), unlike the
/// kind-agnostic stop undo.
pub fn plan_station_undo(edit: &StationEdit) -> Result<StationUndoOp, EditError> {
    if edit.action == "undo" {
        return Err(EditError::NotUndoable(edit.edit_id));
    }
    let (Some(old_st), Some(new_st)) = (edit.old_station_version_id, edit.new_station_version_id)
    else {
        return Err(EditError::NotUndoable(edit.edit_id));
    };
    match edit.changes.get("kind").and_then(|k| k.as_str()) {
        Some("station_rename") => Ok(StationUndoOp::Rename(plan_rename_undo_parts(
            edit.edit_id,
            edit.station_id,
            old_st,
            new_st,
            &edit.changes,
        )?)),
        Some("station_move") => Ok(StationUndoOp::Move(StationMoveUndoOp {
            edit_id: edit.edit_id,
            station_id: edit.station_id,
            old_station_version_id: old_st,
            new_station_version_id: new_st,
        })),
        _ => Err(EditError::NotUndoable(edit.edit_id)),
    }
}

/// The rename arm: copy the logged stop-version pairs into the inverse op.
fn plan_rename_undo_parts(
    edit_id: i64,
    station_id: i64,
    old_st: i64,
    new_st: i64,
    changes: &serde_json::Value,
) -> Result<RenameUndoOp, EditError> {
    let stops = changes
        .get("stops")
        .and_then(|s| s.as_array())
        .ok_or(EditError::NotUndoable(edit_id))?;
    let mut poles = Vec::new();
    for s in stops {
        let (Some(stop_id), Some(old_svi), Some(new_svi)) = (
            s.get("stop_id").and_then(|v| v.as_i64()),
            s.get("old_svi").and_then(|v| v.as_i64()),
            s.get("new_svi").and_then(|v| v.as_i64()),
        ) else {
            return Err(EditError::NotUndoable(edit_id));
        };
        poles.push(PoleUndo { stop_id, old_svi, new_svi });
    }
    Ok(RenameUndoOp {
        edit_id,
        station_id,
        old_station_version_id: old_st,
        new_station_version_id: new_st,
        poles,
    })
}
```

Delete the old `pub fn plan_rename_undo` (its body splits into the dispatch above + `plan_rename_undo_parts`). `undo_station_rename` still calls the old shape — give it a temporary shim so the crate compiles (Task 8 removes it):

```rust
    let plan = match plan_station_undo(&edit)? {
        StationUndoOp::Rename(p) => p,
        StationUndoOp::Move(_) => return Err(EditError::NotUndoable(edit_id).into()),
    };
```

(replacing the `let plan = plan_rename_undo(&edit)?;` line; the rest of `undo_station_rename` is untouched this task).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rm-edit`
Expected: PASS (all — including the existing wrong-kind/kind-less/malformed-stops tests, now via `plan_station_undo`)

- [ ] **Step 5: Commit**

```bash
git add crates/rm-edit/src/undo.rs
git commit -m "rm-edit: plan_station_undo — kind-dispatched station undo planner (rename|move)"
```

---

### Task 7: rm-db — `apply_station_undo` (enum dispatch + move branch)

**Files:**
- Modify: `crates/rm-db/src/edit.rs:821-957` (undo op structs + dispatch fn + move branch; rename branch body unchanged)
- Modify: `crates/rm-db/src/lib.rs:18-27` (re-exports)
- Modify: `crates/rm-edit/src/undo.rs` (one-line API swap — full service rewrite lands in Task 8)
- Test: `crates/rm-db/src/edit.rs` (tests mod)

**Interfaces:**
- Consumes: nothing new.
- Produces:
  - `pub struct StationMoveUndoOp { pub edit_id: i64, pub station_id: i64, pub old_station_version_id: i64, pub new_station_version_id: i64 }`
  - `pub enum StationUndoOp { Rename(StationRenameUndoOp), Move(StationMoveUndoOp) }`
  - `pub struct StationUndone { pub edit_id: i64, pub restored_station_version_id: i64 }` (renamed from `StationRenameUndone`)
  - `pub fn apply_station_undo(conn: &Connection, op: &StationUndoOp, editor: &str, note: &str) -> Result<StationUndone>`
  - `apply_station_rename_undo` disappears from the public API (private `apply_rename_undo` keeps the body).

- [ ] **Step 1: Write the failing tests** — add to the tests mod (rename_fixture/move_op/params! in scope; `move_op` comes from Task 2):

```rust
    fn move_undo_op(edit_id: i64, old: i64, new: i64) -> StationUndoOp {
        StationUndoOp::Move(StationMoveUndoOp {
            edit_id,
            station_id: 1,
            old_station_version_id: old,
            new_station_version_id: new,
        })
    }

    #[test]
    fn apply_station_undo_move_restores_predecessor() {
        let conn = open_in_memory().unwrap();
        let old = rename_fixture(&conn);
        let m = apply_station_move(&conn, &move_op(old), "herry", "move").unwrap();
        let u = apply_station_undo(&conn, &move_undo_op(m.edit_id, old, m.new_station_version_id), "h", "rev").unwrap();
        assert_eq!(u.restored_station_version_id, old);
        // predecessor reopened with its ORIGINAL coords (reopen, not rewrite)
        let (to, lat, lon): (Option<String>, f64, f64) = conn.query_row(
            "SELECT valid_to, latitude, longitude FROM station_version WHERE station_version_id = ?1",
            [old], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!((to.is_none(), lat, lon), (true, 24.0, 120.0));
        // minted row closed; undo mints nothing
        let (to, _): (Option<String>, Option<String>) = conn.query_row(
            "SELECT valid_to, source FROM station_version WHERE station_version_id = ?1",
            [m.new_station_version_id], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert!(to.is_some());
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM station_version", [], |r| r.get(0)).unwrap(), 2);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM station_version WHERE valid_to IS NULL", [], |r| r.get(0)).unwrap(), 1);
        // no collateral
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM stop_version", [], |r| r.get(0)).unwrap(), 2);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM stop_bearing", [], |r| r.get(0)).unwrap(), 1);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0)).unwrap(), 0);
        // undo log row: action='undo', changes='{}', undo_of set, versions swapped
        let (act, changes, undo_of, vb, va): (String, String, i64, i64, i64) = conn.query_row(
            "SELECT action, changes, undo_of, version_before, version_after FROM edit_log
             WHERE edit_id = ?1",
            [u.edit_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).unwrap();
        assert_eq!((act.as_str(), changes.as_str(), undo_of, vb, va),
            ("undo", "{}", m.edit_id, m.new_station_version_id, old));
    }

    #[test]
    fn apply_station_undo_move_refuses_after_second_move() {
        let conn = open_in_memory().unwrap();
        let old = rename_fixture(&conn);
        let first = apply_station_move(&conn, &move_op(old), "herry", "a").unwrap();
        let _second = apply_station_move(&conn, &move_op(first.new_station_version_id), "herry", "b").unwrap();
        let err = apply_station_undo(&conn, &move_undo_op(first.edit_id, old, first.new_station_version_id), "h", "rev").unwrap_err();
        assert!(err.to_string().contains("station version"), "{err}");
        // zero partial: three versions, two move rows, no undo row
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM station_version", [], |r| r.get(0)).unwrap(), 3);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM edit_log WHERE action = 'undo'", [], |r| r.get(0)).unwrap(), 0);
    }

    #[test]
    fn apply_station_undo_move_bogus_ids_zero_partial() {
        let conn = open_in_memory().unwrap();
        let old = rename_fixture(&conn);
        let m = apply_station_move(&conn, &move_op(old), "herry", "move").unwrap();
        // unknown predecessor: the reopen UPDATE hits 0 rows
        let err = apply_station_undo(&conn, &move_undo_op(m.edit_id, 999, m.new_station_version_id), "h", "rev").unwrap_err();
        assert!(err.to_string().contains("cannot be undone"), "{err}");
        // unknown minted version: the currency COUNT guard rejects
        let err = apply_station_undo(&conn, &move_undo_op(m.edit_id, old, 999), "h", "rev").unwrap_err();
        assert!(err.to_string().contains("station version"), "{err}");
        // zero partial after both
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM station_version", [], |r| r.get(0)).unwrap(), 2);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM edit_log WHERE action = 'undo'", [], |r| r.get(0)).unwrap(), 0);
    }
```

Also migrate the EXISTING rename-undo tests + call sites to the new API (mechanical): keep the `undo_op()` helper returning `StationRenameUndoOp` unchanged, and wrap at each call site — every `apply_station_rename_undo(&conn, &undo_op(&applied), …)` becomes `apply_station_undo(&conn, &StationUndoOp::Rename(undo_op(&applied)), …)`. Grep: `grep -n 'apply_station_rename_undo\|StationRenameUndone' crates/rm-db/src/edit.rs crates/rm-edit/src/undo.rs` — every hit outside the rewritten fn must end up on the new API.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rm-db apply_station_undo`
Expected: FAIL — `apply_station_undo` / `StationUndoOp` not found

- [ ] **Step 3: Implement** — in edit.rs, replace the undo section (lines ~821-957):

```rust
/// Write-side op for the station-rename undo (rm-edit maps a RenameUndoOp
/// into this; the rows come from the append-only log).
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

/// Write-side op for the station-move undo: restore is reopen-the-old /
/// close-the-minted — the old row already holds the original coords.
#[derive(Debug)]
pub struct StationMoveUndoOp {
    pub edit_id: i64,
    pub station_id: i64,
    pub old_station_version_id: i64,
    pub new_station_version_id: i64,
}

#[derive(Debug)]
pub enum StationUndoOp {
    Rename(StationRenameUndoOp),
    Move(StationMoveUndoOp),
}

#[derive(Debug)]
pub struct StationUndone {
    pub edit_id: i64,
    pub restored_station_version_id: i64,
}

/// Undo a station edit in ONE transaction — the exact inverse of the
/// applied edit, dispatched on the op's kind. Rename branch: close/reopen
/// the station version, per pole close/reopen the stop versions, repoint
/// route_stop_version, move the stop_bearing row back (PK invariant).
/// Move branch: close the minted version, reopen the predecessor — touches
/// no poles, no bearings, no rsv (a station move wrote none of them).
/// One edit_log undo row, ZERO stale marks. Mints nothing.
///
/// Guards (inside the tx, before any write): the edit's station version
/// must still be current and manual (`StationNotCurrent` otherwise); the
/// rename branch additionally requires every minted pole version still
/// current and manual (`NotUndoable` otherwise). Every reopen/close UPDATE
/// checks rows-affected == 1.
pub fn apply_station_undo(
    conn: &Connection,
    op: &StationUndoOp,
    editor: &str,
    note: &str,
) -> Result<StationUndone> {
    match op {
        StationUndoOp::Rename(o) => apply_rename_undo(conn, o, editor, note),
        StationUndoOp::Move(o) => apply_move_undo(conn, o, editor, note),
    }
}

fn apply_rename_undo(
    conn: &Connection,
    op: &StationRenameUndoOp,
    editor: &str,
    note: &str,
) -> Result<StationUndone> {
    // … the ENTIRE former body of apply_station_rename_undo, byte-identical,
    // with the final Ok(...) constructing StationUndone instead of
    // StationRenameUndone …
}

fn apply_move_undo(
    conn: &Connection,
    op: &StationMoveUndoOp,
    editor: &str,
    note: &str,
) -> Result<StationUndone> {
    let tx = conn.unchecked_transaction()?;
    let now = crate::now_rfc3339();

    // guards BEFORE any write: the minted version must still be current and
    // manual (a second move/rename on top makes this undo stale)
    let n: i64 = tx.query_row(
        "SELECT COUNT(*) FROM station_version
         WHERE station_version_id = ?1 AND valid_to IS NULL AND source = 'manual'",
        [op.new_station_version_id],
        |r| r.get(0),
    )?;
    if n == 0 {
        return Err(EditError::StationNotCurrent(op.new_station_version_id).into());
    }

    let n = tx.execute(
        "UPDATE station_version SET valid_to = ?1
         WHERE station_version_id = ?2 AND valid_to IS NULL",
        params![now, op.new_station_version_id],
    )?;
    if n != 1 {
        return Err(EditError::NotUndoable(op.edit_id).into());
    }
    let n = tx.execute(
        "UPDATE station_version SET valid_to = NULL WHERE station_version_id = ?1",
        params![op.old_station_version_id],
    )?;
    if n != 1 {
        return Err(EditError::NotUndoable(op.edit_id).into());
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
    Ok(StationUndone {
        edit_id: undo_edit_id,
        restored_station_version_id: op.old_station_version_id,
    })
}
```

(The "…former body…" elision above is the ONE place in this plan where existing code moves verbatim — copy `apply_station_rename_undo`'s body from the current file, keep every guard, `== 1` check, bearing move-back, and the edit_log INSERT exactly as they are, changing only the return type name.)

- [ ] **Step 4: Update re-exports + keep rm-edit compiling.** In `crates/rm-db/src/lib.rs`, the last re-export line becomes:

```rust
    StationRenameUndoOp, PoleUndoOp, StationMoveUndoOp, StationUndoOp, StationUndone,
    apply_station_undo,
```

`apply_station_rename_undo` leaving rm-db's API would break `crates/rm-edit/src/undo.rs` (import at :7, call at :96) and leave the workspace red until Task 8 — so swap that call site now, mechanically: update undo.rs's `rm_db::` import to `apply_station_undo` + `StationUndoOp` (dropping `apply_station_rename_undo`), and wrap the op the service already built:

```rust
    let undone = rm_db::apply_station_undo(
        conn,
        &rm_db::StationUndoOp::Rename(op),
        editor,
        note,
    )
    .map_err(crate::map_db_err)?;
```

(The `op` on that line is already an `rm_db::StationRenameUndoOp`; nothing else in `undo_station_rename` changes — Task 8 rewrites this fn properly.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p rm-db -p rm-edit`
Expected: PASS (all — the 3 new move-undo tests + every migrated rename-undo test; rm-edit still green through the wrap)

- [ ] **Step 6: Commit**

```bash
git add crates/rm-db/src/edit.rs crates/rm-db/src/lib.rs crates/rm-edit/src/undo.rs
git commit -m "rm-db: apply_station_undo — kind-dispatched undo with move branch"
```

---

### Task 8: rm-edit — `undo_station` service + integration tests

**Files:**
- Modify: `crates/rm-edit/src/undo.rs` (service rename + enum mapping; shim from Task 6 removed)
- Test: `crates/rm-edit/tests/undo.rs`, `crates/rm-edit/tests/rename.rs`, `crates/rm-edit/tests/move.rs`

**Interfaces:**
- Consumes: `rm_db::load_station_edit` (Task 5), `plan_station_undo` (Task 6), `rm_db::apply_station_undo` + `rm_db::StationUndoOp` (Task 7), `crate::map_db_err` (Task 3).
- Produces: `pub fn undo_station(conn: &Connection, edit_id: i64, editor: &str, note: &str) -> Result<UndoOutcome>` (`undo_station_rename` deleted; `UndoOutcome` unchanged).

- [ ] **Step 1: Write the failing integration tests** — in `crates/rm-edit/tests/undo.rs` (its fixture/helpers already build station 20 + rename rows; reuse them). New tests:

```rust
#[test]
fn undo_station_moves_back() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let m = move_station(
        &conn,
        StationMove { station_version_id: 20, latitude: 24.5, longitude: 120.5 },
        "herry",
        "fix",
    )
    .unwrap();
    let u = undo_station(&conn, m.edit_id, "herry", "revert").unwrap();
    assert_eq!(u.restored_station_version_id, 20);
    // predecessor current again with original coords
    let (lat, lon): (f64, f64) = conn
        .query_row(
            "SELECT latitude, longitude FROM station_version WHERE valid_to IS NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((lat, lon), (24.0, 120.0));
    // undo mints nothing
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM station_version", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 2);
}

#[test]
fn undo_station_refuses_move_after_move() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let first = move_station(
        &conn,
        StationMove { station_version_id: 20, latitude: 24.5, longitude: 120.5 },
        "herry",
        "a",
    )
    .unwrap();
    move_station(
        &conn,
        StationMove { station_version_id: first.new_station_version_id, latitude: 24.6, longitude: 120.6 },
        "herry",
        "b",
    )
    .unwrap();
    let err = undo_station(&conn, first.edit_id, "herry", "revert").unwrap_err();
    assert!(err.to_string().contains("not the current version"), "{err}");
    // no undo row written
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM edit_log WHERE action = 'undo'", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0);
}

#[test]
fn undo_station_refuses_undo_of_undo() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let m = move_station(
        &conn,
        StationMove { station_version_id: 20, latitude: 24.5, longitude: 120.5 },
        "herry",
        "fix",
    )
    .unwrap();
    let u = undo_station(&conn, m.edit_id, "herry", "revert").unwrap();
    let err = undo_station(&conn, u.edit_id, "herry", "again").unwrap_err();
    assert!(err.to_string().contains("cannot be undone"), "{err}");
}

#[test]
fn undo_station_refuses_stop_edit_row() {
    // (if tests/undo.rs lacks this e2e variant — scenario-2 folded it in at
    // the db level; pin it at the service level here too)
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    // a stop edit row: entity='stop', kind-less changes
    let applied = rm_db::apply_stop_edit(
        &conn,
        &rm_db::StopEditOp {
            stop_id: 1,
            stop_version_id: 10,
            stop_name: Some("shadow".into()),
            ..Default::default()
        },
        "h",
        "n",
    )
    .unwrap();
    let err = undo_station(&conn, applied.edit_id, "herry", "revert").unwrap_err();
    assert!(err.to_string().contains("cannot be undone"), "{err}");
}
```

Migrate every `undo_station_rename(` call in `crates/rm-edit/tests/undo.rs` and `crates/rm-edit/tests/rename.rs` to `undo_station(` (grep: `grep -rn 'undo_station_rename' crates/ --include='*.rs'`). Add to the test file's use block: `use rm_edit::{move_station, undo_station, StationMove};` (merge into existing `use rm_edit::{…}`).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rm-edit --test undo --test move`
Expected: FAIL — `undo_station` not found

- [ ] **Step 3: Implement** — in undo.rs, rename and finish the service (Task 6's shim goes away):

```rust
/// Application service: load the logged station edit → plan (pure) → apply
/// the inverse in one tx. Zero SQL here. Handles both station kinds
/// (rename, move) via the kind-dispatched plan.
pub fn undo_station(conn: &Connection, edit_id: i64, editor: &str, note: &str) -> Result<UndoOutcome> {
    let edit = rm_db::load_station_edit(conn, edit_id)?.ok_or(EditError::NotUndoable(edit_id))?;
    let db_op = match plan_station_undo(&edit)? {
        StationUndoOp::Rename(p) => rm_db::StationUndoOp::Rename(rm_db::StationRenameUndoOp {
            edit_id: p.edit_id,
            station_id: p.station_id,
            old_station_version_id: p.old_station_version_id,
            new_station_version_id: p.new_station_version_id,
            poles: p
                .poles
                .iter()
                .map(|pole| rm_db::PoleUndoOp {
                    stop_id: pole.stop_id,
                    old_svi: pole.old_svi,
                    new_svi: pole.new_svi,
                })
                .collect(),
        }),
        StationUndoOp::Move(p) => rm_db::StationUndoOp::Move(rm_db::StationMoveUndoOp {
            edit_id: p.edit_id,
            station_id: p.station_id,
            old_station_version_id: p.old_station_version_id,
            new_station_version_id: p.new_station_version_id,
        }),
    };
    let undone = rm_db::apply_station_undo(conn, &db_op, editor, note).map_err(crate::map_db_err)?;
    Ok(UndoOutcome {
        edit_id: undone.edit_id,
        restored_station_version_id: undone.restored_station_version_id,
    })
}
```

Delete `undo_station_rename` entirely (shim + old body). Note `EditError::NotUndoable(...).into()` works because `EditError: Into<anyhow::Error>` via thiserror — the return type is `anyhow::Result`, so `Err(...)` needs `.into()` or `?` coercion as in the existing code.

- [ ] **Step 4: Run the full rm-edit suite**

Run: `cargo test -p rm-edit`
Expected: PASS (all — new move-undo integration tests + migrated rename-undo tests)

- [ ] **Step 5: Commit**

```bash
git add crates/rm-edit/src/undo.rs crates/rm-edit/tests/
git commit -m "rm-edit: undo_station — kind-agnostic station undo service (rename|move)"
```

---

### Task 9: Whole-workspace verification

**Files:**
- None new (fix-ups only if a check fails)

- [ ] **Step 1: Format + lint**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets 2>&1 | tail -20`
Expected: no errors; fix any warning the change introduced (touch only code this plan owns)

- [ ] **Step 2: Full test suite**

Run: `cargo test --workspace 2>&1 | tail -25`
Expected: PASS — 235 existing + 17 new (2 apply-move, 1 characterization, 4 move incl. 1 plan unit, 3 plan-dispatch, 3 apply-undo-move, 4 undo-service) ≈ 252

- [ ] **Step 3: Manual smoke (optional but cheap) — move then undo through a scratch db**

Run: `cargo run -q -p rm-db --example scratch 2>/dev/null || true` — no example exists; skip if so. The service-level integration tests in Task 8 already cover the roundtrip on in-memory dbs; do not build new tooling for this.

- [ ] **Step 4: Commit any stragglers**

```bash
git status --short
```

If clean: nothing to commit — done. If there are fix-ups:

```bash
git add -A && git commit -m "scenario 6: station move + kind-agnostic station undo — workspace green"
```
