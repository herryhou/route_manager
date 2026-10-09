# Scenario 2 — Stop Move ⇒ Membership Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Migrate the stop edit (move/name) and add the stop regroup (station membership) primitives into the `rm-edit` domain crate, with a generic stop undo, and rewire rm-web's existing endpoints onto them.

**Architecture:** Scenario-1/3 pattern — `rm-edit` owns commands, pure plan functions, and load→plan→apply services (zero SQL); `rm-db::edit` owns read/write repository functions (one tx per write, currency re-validated inside the tx); dependency stays one-way `rm-edit → rm-db`. `rm_db::edit_stop`/`undo` are deleted once rm-web is rewired.

**Tech Stack:** Rust workspace, rusqlite, anyhow, thiserror, serde_json, axum (rm-web rewire only).

**Spec:** `docs/superpowers/specs/2026-10-10-stop-move-regroup-design.md`

## Global Constraints

- `rm-edit` contains ZERO SQL. All queries live in `rm-db`.
- Dependency direction: `rm-edit → rm-db`, one-way. Never the reverse.
- Every write function is ONE transaction (`conn.unchecked_transaction()`), re-validates currency inside the tx, and leaves zero partial state on any guard failure.
- Undo mints nothing: reopen the predecessor row (`valid_to = NULL`), close the minted row, repoint `route_stop_version` back.
- Every UPDATE on a version row checks rows-affected `== 1`.
- Stale marks exactly per the spec table: move ⇒ `stop_bearing` + per-route `path_stop_projection` + `station_bearing`; name-only ⇒ ZERO marks (bearing row carried over); regroup ⇒ `station_bearing` only.
- `edit_log` shapes: edit ⇒ `action='update'`, `changes={field:{old,new}}`; regroup ⇒ `action='move_station'`, `changes={"station_id":{"old":…,"new":…}}`; undo ⇒ `action='undo'`, `changes='{}'`, versions swapped, `undo_of` set.
- Match existing code style in `crates/rm-db/src/edit.rs` (test fixture helpers, `params!`, `crate::mint`, `crate::now_rfc3339`).
- Workspace `cargo test` must be green at every task boundary (210 tests today).

## Review Focus

1. **Name-only regression:** the old `edit_stop` deleted `stop_bearing` rows on a name-only edit. The new path must carry the row over and write zero marks. Pinned in Task 2 (`apply_stop_edit_name_only_carries_bearing_zero_marks`).
2. **Legacy kind-less rows stay undoable:** all pre-existing `edit_log` stop rows have no `kind` in `changes`. `plan_stop_undo` must not inspect `changes` kinds — only `entity`/`action`. Pinned in Task 8 (`undo_stop_accepts_legacy_row_without_kind`).
3. **Undo after a refresh recomputed bearings:** `stop_bearing` may exist at the churned svi when the undo runs; the undo must DELETE it and, for position edits, leave the predecessor without a bearing row + re-mark. Pinned in Task 4 (`apply_stop_undo_deletes_refreshed_bearing_at_cur_svi`).
4. **Undo mark derivation from recorded changes, not table state:** undo of a regroup must mark `station_bearing` even though `stop_bearing` looks fine; undo of a move must re-mark projections via the repointed rsv. Pinned in Task 4 (`apply_stop_undo_marks_per_changes_fields`).
5. **Regroup target validation:** regroup to a missing/old station id must fail with `UnknownStation` (409), regroup to the current station with `NoOpEdit` (400). Pinned in Task 7.

---

### Task 1: rm-db — `load_stop_snapshot` + `station_is_current`

**Files:**
- Modify: `crates/rm-db/src/edit.rs` (add after `load_station_snapshot`, ~line 366)
- Modify: `crates/rm-db/src/lib.rs:18-24` (re-export)
- Test: `crates/rm-db/src/edit.rs` (`#[cfg(test)] mod tests`, existing `fixture` at line 711)

**Interfaces:**
- Consumes: nothing new (existing `fixture` test helper, `Connection`).
- Produces (Task 2/3/6/7 consume):
  - `pub struct StopSnapshot { pub stop_id: i64, pub station_id: i64, pub stop_version_id: i64, pub stop_code: String, pub stop_name: String, pub direction: i64, pub latitude: f64, pub longitude: f64 }`
  - `pub fn load_stop_snapshot(conn: &Connection, stop_version_id: i64) -> Result<Option<StopSnapshot>>`
  - `pub fn station_is_current(conn: &Connection, station_id: i64) -> Result<bool>`

- [ ] **Step 1: Write the failing tests** (inside `mod tests` in `crates/rm-db/src/edit.rs`)

```rust
    #[test]
    fn load_stop_snapshot_returns_current_row() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        let s = load_stop_snapshot(&conn, old).unwrap().expect("current");
        assert_eq!(
            (s.stop_id, s.station_id, s.stop_version_id),
            (1, 1, old)
        );
        assert_eq!(s.stop_name, "Old");
        assert_eq!((s.latitude, s.longitude), (24.0, 120.0));
    }

    #[test]
    fn load_stop_snapshot_none_for_non_current_or_missing() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        edit_stop(&conn, old, &StopEdit { stop_name: Some("N".into()), ..Default::default() }, "h", "n").unwrap();
        // old is now closed; the minted version is current
        assert!(load_stop_snapshot(&conn, old).unwrap().is_none());
        assert!(load_stop_snapshot(&conn, 999).unwrap().is_none());
    }

    #[test]
    fn station_is_current_checks_valid_to() {
        let conn = open_in_memory().unwrap();
        fixture(&conn);
        assert!(station_is_current(&conn, 1).unwrap());
        assert!(!station_is_current(&conn, 42).unwrap());
    }
```

(The existing `fixture` builds station 1 with station_version 20 current, stop 1 = stop_version 10 current — see `rename_fixture` doc comment. Note `fixture` seeds `station_version` valid and `stop_version` 10 current, name "Old", lat 24.0, lon 120.0. If the exact field values differ from this fixture, read `fixture` first and assert against ITS values.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rm-db load_stop_snapshot 2>&1 | tail -5`
Expected: FAIL — `load_stop_snapshot` not found (compile error).

- [ ] **Step 3: Implement** (in `crates/rm-db/src/edit.rs`, after `load_station_snapshot`'s helpers, before `StationRenameEdit`)

```rust
/// Current row of one stop (pole) — current-row discipline
/// (`valid_to IS NULL`). `None` when the version is closed or missing.
#[derive(Debug)]
pub struct StopSnapshot {
    pub stop_id: i64,
    pub station_id: i64,
    pub stop_version_id: i64,
    pub stop_code: String,
    pub stop_name: String,
    pub direction: i64,
    pub latitude: f64,
    pub longitude: f64,
}

pub fn load_stop_snapshot(
    conn: &Connection,
    stop_version_id: i64,
) -> Result<Option<StopSnapshot>> {
    let res = conn.query_row(
        "SELECT stop_id, station_id, stop_version_id, stop_code, stop_name,
                direction, latitude, longitude
         FROM stop_version WHERE stop_version_id = ?1 AND valid_to IS NULL",
        [stop_version_id],
        |r| {
            Ok(StopSnapshot {
                stop_id: r.get(0)?,
                station_id: r.get(1)?,
                stop_version_id: r.get(2)?,
                stop_code: r.get(3)?,
                stop_name: r.get(4)?,
                direction: r.get(5)?,
                latitude: r.get(6)?,
                longitude: r.get(7)?,
            })
        },
    );
    match res {
        Ok(s) => Ok(Some(s)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Does `station_id` have a current `station_version`? (Regroup target check.)
pub fn station_is_current(conn: &Connection, station_id: i64) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM station_version
         WHERE station_id = ?1 AND valid_to IS NULL",
        [station_id],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}
```

- [ ] **Step 4: Re-export** — in `crates/rm-db/src/lib.rs`, extend the `pub use edit::{…}` list with `StopSnapshot, load_stop_snapshot, station_is_current`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p rm-db 2>&1 | tail -5`
Expected: PASS (all, including the 3 new ones).

- [ ] **Step 6: Commit**

```bash
git add crates/rm-db/src/edit.rs crates/rm-db/src/lib.rs
git commit -m "rm-db: load_stop_snapshot + station_is_current reads"
```

---

### Task 2: rm-db — `apply_stop_edit` (move/name with per-field staleness)

**Files:**
- Modify: `crates/rm-db/src/edit.rs` (new fn near `edit_stop`; new helper `carry_bearing`)
- Modify: `crates/rm-db/src/lib.rs` (re-export)
- Test: `crates/rm-db/src/edit.rs` (`mod tests`)

**Interfaces:**
- Consumes: `EditOutcome`, `EditError::NotCurrent`, `mark_bearings_stale`, `mark_position_stale`, `crate::mint`, `crate::now_rfc3339` (all existing).
- Produces (Task 6 consumes):
  - `#[derive(Debug, Default)] pub struct StopEditOp { pub stop_id: i64, pub stop_version_id: i64, pub stop_name: Option<String>, pub lat: Option<f64>, pub lon: Option<f64> }`
  - `pub fn apply_stop_edit(conn: &Connection, op: &StopEditOp, editor: &str, note: &str) -> Result<EditOutcome>`
  - (internal) `fn carry_bearing(tx: &Connection, from_svi: i64, to_svi: i64) -> Result<()>` — reused by Tasks 3 and 4.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn apply_stop_edit_moves_position_deletes_bearing_marks_three() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        let op = StopEditOp {
            stop_id: 1,
            stop_version_id: old,
            lat: Some(24.001),
            lon: None,
            stop_name: None,
        };
        let out = apply_stop_edit(&conn, &op, "herry", "move").unwrap();
        assert!(out.new_version_id > old);
        // old closed, new current + manual, position applied
        let (to, lat, src): (Option<String>, f64, String) = conn.query_row(
            "SELECT valid_to, latitude, source FROM stop_version WHERE stop_version_id = ?1",
            [out.new_version_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!((to.is_some(), lat, src.as_str()), (false, 24.001, "manual"));
        // rsv repointed
        let sv: i64 = conn.query_row(
            "SELECT stop_version_id FROM route_stop_version WHERE route_stop_version_id = 1",
            [], |r| r.get(0)).unwrap();
        assert_eq!(sv, out.new_version_id);
        // bearing deleted at the old svi
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM stop_bearing WHERE stop_version_id = ?1", [old], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        // marks: stop_bearing + projection(route 1) + station_bearing
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 3);
        assert_eq!(out.stale_routes, vec![1]);
        // edit_log: {field:{old,new}} shape
        let (ent, act, changes): (String, String, String) = conn.query_row(
            "SELECT entity, action, changes FROM edit_log WHERE edit_id = ?1",
            [out.edit_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!((ent.as_str(), act.as_str()), ("stop", "update"));
        let c: serde_json::Value = serde_json::from_str(&changes).unwrap();
        assert_eq!(c["latitude"]["new"], 24.001);
        assert!(c["stop_name"].is_null());
    }

    #[test]
    fn apply_stop_edit_name_only_carries_bearing_zero_marks() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn); // stop_version 10 HAS a bearing row (see fixture)
        let op = StopEditOp {
            stop_id: 1,
            stop_version_id: old,
            stop_name: Some("New".into()),
            ..Default::default()
        };
        let out = apply_stop_edit(&conn, &op, "herry", "rename").unwrap();
        // bearing row MOVED to the new svi, identical values + computed_at
        let (deg, computed): (f64, String) = conn.query_row(
            "SELECT bearing_deg, computed_at FROM stop_bearing WHERE stop_version_id = ?1",
            [out.new_version_id], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((deg, computed.as_str()), (123.5, "2026-01-02T00:00:00Z"));
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM stop_bearing WHERE stop_version_id = ?1", [old], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        // zero stale marks — a rename invalidates no derived data
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        assert!(out.stale_routes.is_empty());
    }

    #[test]
    fn apply_stop_edit_refuses_stale_version_zero_partial() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        let first = StopEditOp { stop_id: 1, stop_version_id: old, stop_name: Some("A".into()), ..Default::default() };
        apply_stop_edit(&conn, &first, "h", "n").unwrap();
        let err = apply_stop_edit(&conn, &first, "h", "n").unwrap_err();
        assert!(err.to_string().contains("not the current version"), "{err}");
        // zero partial: exactly one minted version exists
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM stop_version WHERE stop_id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2);
    }
```

(If `fixture`'s bearing values differ — `123.5` / `2026-01-02T00:00:00Z` come from the existing `rename_fixture`; check `fixture` and use its values. If `fixture` has no bearing row, INSERT one in the test: `INSERT INTO stop_bearing VALUES (10, 123.5, 'OSM', '2026-01-02T00:00:00Z')`.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rm-db apply_stop_edit 2>&1 | tail -5`
Expected: FAIL — `StopEditOp`/`apply_stop_edit` not found.

- [ ] **Step 3: Implement** (in `crates/rm-db/src/edit.rs`, after `mark_position_stale`)

```rust
/// Move a `stop_bearing` row from one version key to another, keeping the
/// value and `computed_at` (bearing = f(position); edits that don't move
/// the position don't invalidate it). No-op when `from_svi` has no row.
fn carry_bearing(tx: &Connection, from_svi: i64, to_svi: i64) -> Result<()> {
    tx.execute(
        "INSERT INTO stop_bearing (stop_version_id, bearing_deg, source, computed_at)
         SELECT ?1, bearing_deg, source, computed_at FROM stop_bearing
         WHERE stop_version_id = ?2",
        params![to_svi, from_svi],
    )?;
    tx.execute(
        "DELETE FROM stop_bearing WHERE stop_version_id = ?1",
        [from_svi],
    )?;
    Ok(())
}

#[derive(Debug, Default)]
pub struct StopEditOp {
    pub stop_id: i64,
    pub stop_version_id: i64, // must be the current version
    pub stop_name: Option<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
}

/// Whole-tx stop edit (move and/or rename). Position present ⇒ the pole's
/// `stop_bearing` row is DELETED (value = f(position) — invalid now) and
/// the derived data is marked stale. Name-only ⇒ the bearing row is
/// carried over and NOTHING goes stale. One edit_log row,
/// `changes = {field: {old, new}}`.
pub fn apply_stop_edit(
    conn: &Connection,
    op: &StopEditOp,
    editor: &str,
    note: &str,
) -> Result<EditOutcome> {
    let tx = conn.unchecked_transaction()?;
    // re-validate inside the tx: the version must still be current
    let old: (i64, String, String, i64, f64, f64) = match tx.query_row(
        "SELECT station_id, stop_code, stop_name, direction, latitude, longitude
         FROM stop_version WHERE stop_version_id = ?1 AND valid_to IS NULL",
        [op.stop_version_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
    ) {
        Ok(v) => v,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Err(EditError::NotCurrent(op.stop_version_id).into())
        }
        Err(e) => return Err(e.into()),
    };
    let (station_id, stop_code, old_name, direction, old_lat, old_lon) = old;
    let now = crate::now_rfc3339();
    let new_id = crate::mint(&tx, "stop_version")?;
    tx.execute(
        "UPDATE stop_version SET valid_to = ?1 WHERE stop_version_id = ?2",
        params![now, op.stop_version_id],
    )?;
    tx.execute(
        "INSERT INTO stop_version
            (stop_version_id, stop_id, station_id, stop_code, stop_name, direction,
             latitude, longitude, valid_from, created_at, source)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, 'manual')",
        params![
            new_id,
            op.stop_id,
            station_id,
            stop_code,
            op.stop_name.clone().unwrap_or(old_name),
            direction,
            op.lat.unwrap_or(old_lat),
            op.lon.unwrap_or(old_lon),
            now
        ],
    )?;
    tx.execute(
        "UPDATE route_stop_version SET stop_version_id = ?1 WHERE stop_version_id = ?2",
        params![new_id, op.stop_version_id],
    )?;

    let moves = op.lat.is_some() || op.lon.is_some();
    if moves {
        tx.execute(
            "DELETE FROM stop_bearing WHERE stop_version_id = ?1",
            [op.stop_version_id],
        )?;
    } else {
        carry_bearing(&tx, op.stop_version_id, new_id)?;
    }

    let mut changes = serde_json::Map::new();
    if let Some(v) = &op.stop_name {
        changes.insert("stop_name".into(), json!({"old": old_name, "new": v}));
    }
    if let Some(v) = op.lat {
        changes.insert("latitude".into(), json!({"old": old_lat, "new": v}));
    }
    if let Some(v) = op.lon {
        changes.insert("longitude".into(), json!({"old": old_lon, "new": v}));
    }
    tx.execute(
        "INSERT INTO edit_log (ts, editor, entity, entity_id, action, changes, note, version_before, version_after)
         VALUES (?1, ?2, 'stop', ?3, 'update', ?4, ?5, ?6, ?7)",
        params![
            now,
            editor,
            op.stop_id,
            serde_json::Value::Object(changes).to_string(),
            if note.is_empty() { None } else { Some(note) },
            op.stop_version_id,
            new_id
        ],
    )?;
    let edit_id = tx.last_insert_rowid();

    let mut stale_routes = Vec::new();
    if moves {
        mark_bearings_stale(&tx, edit_id, &now)?;
        stale_routes = mark_position_stale(&tx, new_id, edit_id, &now)?;
    }
    tx.commit()?;
    Ok(EditOutcome {
        edit_id,
        new_version_id: new_id,
        stale_routes,
    })
}
```

- [ ] **Step 4: Re-export** `StopEditOp, apply_stop_edit` in `crates/rm-db/src/lib.rs`.

- [ ] **Step 5: Run the full rm-db suite**

Run: `cargo test -p rm-db 2>&1 | tail -5`
Expected: PASS — old `edit_stop` tests still green (old fn untouched until Task 9).

- [ ] **Step 6: Commit**

```bash
git add crates/rm-db/src/edit.rs crates/rm-db/src/lib.rs
git commit -m "rm-db: apply_stop_edit — per-field staleness, name-only carries bearing"
```

---

### Task 3: rm-db — `apply_stop_regroup`

**Files:**
- Modify: `crates/rm-db/src/edit.rs` (new fn after `apply_stop_edit`; new `EditError` variant)
- Modify: `crates/rm-db/src/lib.rs` (re-export)
- Test: `crates/rm-db/src/edit.rs` (`mod tests`)

**Interfaces:**
- Consumes: `carry_bearing` (Task 2), `mark_bearings_stale` is NOT used here — regroup marks `station_bearing` only.
- Produces (Task 7 consumes):
  - `EditError::UnknownStation(i64)` — new variant: `#[error("station {0} does not exist or is not current")]`.
  - `#[derive(Debug)] pub struct StopRegroupOp { pub stop_id: i64, pub stop_version_id: i64, pub new_station_id: i64 }`
  - `pub fn apply_stop_regroup(conn: &Connection, op: &StopRegroupOp, editor: &str, note: &str) -> Result<EditOutcome>`

- [ ] **Step 1: Add the `UnknownStation` variant** to `rm_db::edit::EditError`:

```rust
    #[error("station {0} does not exist or is not current")]
    UnknownStation(i64),
```

- [ ] **Step 2: Write the failing tests**

```rust
    fn regroup_op(old: i64, new_station: i64) -> StopRegroupOp {
        StopRegroupOp { stop_id: 1, stop_version_id: old, new_station_id: new_station }
    }

    fn second_station(conn: &Connection) {
        conn.execute(
            "INSERT INTO station (station_id, ext_uid, created_at)
             VALUES (2, 'tcg:node:2', '2026-01-01T00:00:00Z')", []).unwrap();
        conn.execute(
            "INSERT INTO station_version (station_version_id, station_id, station_group_id, station_name, latitude, longitude, valid_from, created_at)
             VALUES (21, 2, 1, 'T', 24.1, 120.1, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')", []).unwrap();
        conn.execute(
            "INSERT INTO id_sequence (table_name, next_value) VALUES ('station_version', 22)", []).unwrap();
    }

    #[test]
    fn apply_stop_regroup_churns_membership_carries_bearing() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        second_station(&conn);
        let out = apply_stop_regroup(&conn, &regroup_op(old, 2), "herry", "regroup").unwrap();
        // new version carries the new station_id, same position, manual
        let (st, lat, lon, src): (i64, f64, f64, String) = conn.query_row(
            "SELECT station_id, latitude, longitude, source FROM stop_version WHERE stop_version_id = ?1",
            [out.new_version_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap();
        assert_eq!((st, lat, lon, src.as_str()), (2, 24.0, 120.0, "manual"));
        // old closed
        let to: Option<String> = conn.query_row(
            "SELECT valid_to FROM stop_version WHERE stop_version_id = ?1", [old], |r| r.get(0)).unwrap();
        assert!(to.is_some());
        // bearing carried over to the new svi (position unchanged)
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM stop_bearing WHERE stop_version_id = ?1",
            [out.new_version_id], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        // rsv repointed
        let sv: i64 = conn.query_row(
            "SELECT stop_version_id FROM route_stop_version WHERE route_stop_version_id = 1",
            [], |r| r.get(0)).unwrap();
        assert_eq!(sv, out.new_version_id);
        // marks: station_bearing ONLY
        let rows: Vec<(String, String)> = conn.prepare("SELECT table_name, key FROM stale_mark").unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap()
            .collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(rows, vec![("station_bearing".into(), "*".into())]);
        assert!(out.stale_routes.is_empty());
        // edit_log: action='move_station', changes={station_id:{old,new}}
        let (act, changes): (String, String) = conn.query_row(
            "SELECT action, changes FROM edit_log WHERE edit_id = ?1",
            [out.edit_id], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(act, "move_station");
        let c: serde_json::Value = serde_json::from_str(&changes).unwrap();
        assert_eq!(c["station_id"]["old"], 1);
        assert_eq!(c["station_id"]["new"], 2);
    }

    #[test]
    fn apply_stop_regroup_refuses_unknown_target_and_stale_source() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        second_station(&conn);
        let err = apply_stop_regroup(&conn, &regroup_op(old, 42), "h", "n").unwrap_err();
        assert!(err.to_string().contains("does not exist or is not current"), "{err}");
        // stale source: regroup twice from the same version
        let out = apply_stop_regroup(&conn, &regroup_op(old, 2), "h", "n").unwrap();
        let err = apply_stop_regroup(&conn, &regroup_op(old, 2), "h", "n").unwrap_err();
        assert!(err.to_string().contains("not the current version"), "{err}");
        let _ = out;
    }
```

(Note: `second_station` inserts station_version 21 — must not collide with `fixture`'s ids; `fixture` seeds `id_sequence` for stop_version only per `rename_fixture`. Check `fixture` and adjust the id/sequence rows if it already seeds `station_version`.)

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p rm-db regroup 2>&1 | tail -5`
Expected: FAIL — `StopRegroupOp` not found.

- [ ] **Step 4: Implement** (after `apply_stop_edit`)

```rust
#[derive(Debug)]
pub struct StopRegroupOp {
    pub stop_id: i64,
    pub stop_version_id: i64, // must be the current version
    pub new_station_id: i64,
}

/// Whole-tx regroup: move a pole to another station. Membership lives in
/// the child version (INV-17), so this is single-row churn. Position is
/// untouched ⇒ the bearing value survives (carried over); the station
/// circular mean goes stale on both sides — one whole-table mark covers
/// both (whole-table rebuild). No projection marks: an rsv repoint alone
/// invalidates nothing (scenario-1 precedent).
pub fn apply_stop_regroup(
    conn: &Connection,
    op: &StopRegroupOp,
    editor: &str,
    note: &str,
) -> Result<EditOutcome> {
    let tx = conn.unchecked_transaction()?;
    let old: (i64, String, String, i64, f64, f64) = match tx.query_row(
        "SELECT station_id, stop_code, stop_name, direction, latitude, longitude
         FROM stop_version WHERE stop_version_id = ?1 AND valid_to IS NULL",
        [op.stop_version_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
    ) {
        Ok(v) => v,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Err(EditError::NotCurrent(op.stop_version_id).into())
        }
        Err(e) => return Err(e.into()),
    };
    let (old_station_id, stop_code, old_name, direction, lat, lon) = old;
    let n: i64 = tx.query_row(
        "SELECT COUNT(*) FROM station_version WHERE station_id = ?1 AND valid_to IS NULL",
        [op.new_station_id],
        |r| r.get(0),
    )?;
    if n == 0 {
        return Err(EditError::UnknownStation(op.new_station_id).into());
    }
    let now = crate::now_rfc3339();
    let new_id = crate::mint(&tx, "stop_version")?;
    tx.execute(
        "UPDATE stop_version SET valid_to = ?1 WHERE stop_version_id = ?2",
        params![now, op.stop_version_id],
    )?;
    tx.execute(
        "INSERT INTO stop_version
            (stop_version_id, stop_id, station_id, stop_code, stop_name, direction,
             latitude, longitude, valid_from, created_at, source)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, 'manual')",
        params![new_id, op.stop_id, op.new_station_id, stop_code, old_name, direction, lat, lon, now],
    )?;
    tx.execute(
        "UPDATE route_stop_version SET stop_version_id = ?1 WHERE stop_version_id = ?2",
        params![new_id, op.stop_version_id],
    )?;
    carry_bearing(&tx, op.stop_version_id, new_id)?;

    let changes = json!({"station_id": {"old": old_station_id, "new": op.new_station_id}});
    tx.execute(
        "INSERT INTO edit_log (ts, editor, entity, entity_id, action, changes, note, version_before, version_after)
         VALUES (?1, ?2, 'stop', ?3, 'move_station', ?4, ?5, ?6, ?7)",
        params![
            now,
            editor,
            op.stop_id,
            changes.to_string(),
            if note.is_empty() { None } else { Some(note) },
            op.stop_version_id,
            new_id
        ],
    )?;
    let edit_id = tx.last_insert_rowid();

    tx.execute(
        "INSERT OR IGNORE INTO stale_mark (table_name, key, cause_edit_id, marked_at)
         VALUES ('station_bearing', '*', ?1, ?2)",
        params![edit_id, now],
    )?;
    tx.commit()?;
    Ok(EditOutcome {
        edit_id,
        new_version_id: new_id,
        stale_routes: Vec::new(),
    })
}
```

- [ ] **Step 5: Re-export** `StopRegroupOp, apply_stop_regroup` in `crates/rm-db/src/lib.rs`.

- [ ] **Step 6: Run the full rm-db suite**

Run: `cargo test -p rm-db 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/rm-db/src/edit.rs crates/rm-db/src/lib.rs
git commit -m "rm-db: apply_stop_regroup — membership churn, station_bearing mark"
```

---

### Task 4: rm-db — `load_stop_edit` + `apply_stop_undo`

**Files:**
- Modify: `crates/rm-db/src/edit.rs` (new DTO + read fn near `load_station_rename_edit`; new undo fn after `apply_stop_regroup`)
- Modify: `crates/rm-db/src/lib.rs` (re-export)
- Test: `crates/rm-db/src/edit.rs` (`mod tests`)

**Interfaces:**
- Consumes: `carry_bearing` (Task 2), `mark_bearings_stale`, `mark_position_stale` (existing).
- Produces (Task 8 consumes):
  - `#[derive(Debug)] pub struct StopEditRow { pub edit_id: i64, pub entity: String, pub stop_id: i64, pub action: String, pub old_stop_version_id: Option<i64>, pub new_stop_version_id: Option<i64>, pub changes: serde_json::Value }`
  - `pub fn load_stop_edit(conn: &Connection, edit_id: i64) -> Result<Option<StopEditRow>>` — loads ANY entity's row (the plan's entity guard discriminates, as in scenario 3).
  - `#[derive(Debug)] pub struct StopEditUndoOp { pub edit_id: i64, pub stop_id: i64, pub old_svi: i64, pub cur_svi: i64 }`
  - `pub fn apply_stop_undo(conn: &Connection, op: &StopEditUndoOp, editor: &str, note: &str) -> Result<EditOutcome>`

- [ ] **Step 1: Write the failing tests**

```rust
    fn stop_edit_undo_op(edit_id: i64, stop_id: i64, old: i64, cur: i64) -> StopEditUndoOp {
        StopEditUndoOp { edit_id, stop_id, old_svi: old, cur_svi: cur }
    }

    #[test]
    fn load_stop_edit_returns_parsed_row_any_entity() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        let out = apply_stop_edit(&conn, &StopEditOp {
            stop_id: 1, stop_version_id: old, stop_name: Some("N".into()), ..Default::default()
        }, "h", "n").unwrap();
        let e = load_stop_edit(&conn, out.edit_id).unwrap().expect("row");
        assert_eq!((e.entity.as_str(), e.stop_id, e.action.as_str()), ("stop", 1, "update"));
        assert_eq!((e.old_stop_version_id, e.new_stop_version_id), (Some(old), Some(out.new_version_id)));
        assert!(load_stop_edit(&conn, 999).unwrap().is_none());
    }

    #[test]
    fn apply_stop_undo_restores_predecessor_mints_nothing() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        let out = apply_stop_edit(&conn, &StopEditOp {
            stop_id: 1, stop_version_id: old, stop_name: Some("N".into()), ..Default::default()
        }, "h", "n").unwrap();
        let u = apply_stop_undo(&conn, &stop_edit_undo_op(out.edit_id, 1, old, out.new_version_id), "h", "rev").unwrap();
        assert_eq!(u.new_version_id, old);
        // predecessor reopened, minted row closed, count unchanged (no mint)
        let (to, name): (Option<String>, String) = conn.query_row(
            "SELECT valid_to, stop_name FROM stop_version WHERE stop_version_id = ?1",
            [old], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((to.is_none(), name.as_str()), (true, "Old"));
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM stop_version WHERE stop_id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2, "undo mints nothing");
        // rsv repointed back
        let sv: i64 = conn.query_row(
            "SELECT stop_version_id FROM route_stop_version WHERE route_stop_version_id = 1",
            [], |r| r.get(0)).unwrap();
        assert_eq!(sv, old);
        // name-only undo: bearing carried back, ZERO marks
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM stop_bearing WHERE stop_version_id = ?1", [old], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        // undo edit_log row: action='undo', changes='{}', versions swapped
        let (act, changes, ub, ua): (String, String, Option<i64>, Option<i64>) = conn.query_row(
            "SELECT action, changes, version_before, version_after FROM edit_log WHERE edit_id = ?1",
            [u.edit_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap();
        assert_eq!((act.as_str(), changes.as_str()), ("undo", "{}"));
        assert_eq!((ub, ua), (Some(out.new_version_id), Some(old)));
    }

    #[test]
    fn apply_stop_undo_after_move_remarks_and_leaves_no_bearing() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        let out = apply_stop_edit(&conn, &StopEditOp {
            stop_id: 1, stop_version_id: old, lat: Some(24.001), ..Default::default()
        }, "h", "move").unwrap();
        // simulate a refresh having consumed the marks AND recomputed a
        // bearing at the churned svi
        conn.execute("DELETE FROM stale_mark", []).unwrap();
        conn.execute(
            "INSERT INTO stop_bearing (stop_version_id, bearing_deg, source, computed_at)
             VALUES (?1, 9.0, 'OSM', '2026-01-03T00:00:00Z')", [out.new_version_id]).unwrap();

        let u = apply_stop_undo(&conn, &stop_edit_undo_op(out.edit_id, 1, old, out.new_version_id), "h", "rev").unwrap();
        // refreshed row at the closed version deleted; predecessor has none
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM stop_bearing", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        // marks re-derived from changes fields: stop_bearing + projection(route 1) + station_bearing
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 3);
        let causes: Vec<i64> = conn.prepare("SELECT DISTINCT cause_edit_id FROM stale_mark").unwrap()
            .query_map([], |r| r.get(0)).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(causes, vec![u.edit_id], "marks come from the undo itself");
        assert_eq!(u.stale_routes, vec![1]);
    }

    #[test]
    fn apply_stop_undo_after_regroup_marks_station_only() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        second_station(&conn);
        let out = apply_stop_regroup(&conn, &regroup_op(old, 2), "h", "n").unwrap();
        conn.execute("DELETE FROM stale_mark", []).unwrap();
        let u = apply_stop_undo(&conn, &stop_edit_undo_op(out.edit_id, 1, old, out.new_version_id), "h", "rev").unwrap();
        // station membership restored
        let st: i64 = conn.query_row(
            "SELECT station_id FROM stop_version WHERE stop_version_id = ?1", [old], |r| r.get(0)).unwrap();
        assert_eq!(st, 1);
        // bearing carried back
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM stop_bearing WHERE stop_version_id = ?1", [old], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        // station_bearing ONLY
        let rows: Vec<(String, String)> = conn.prepare("SELECT table_name, key FROM stale_mark").unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap()
            .collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(rows, vec![("station_bearing".into(), "*".into())]);
        assert!(u.stale_routes.is_empty());
    }

    #[test]
    fn apply_stop_undo_guards_fail_zero_partial() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        let a = apply_stop_edit(&conn, &StopEditOp {
            stop_id: 1, stop_version_id: old, stop_name: Some("A".into()), ..Default::default()
        }, "h", "n").unwrap();
        // undoing an unknown edit id
        let err = apply_stop_undo(&conn, &stop_edit_undo_op(999, 1, old, a.new_version_id), "h", "n").unwrap_err();
        assert!(err.to_string().contains("cannot be undone"), "{err}");
        // stale cur_svi: edit again, then try to undo `a`
        let b = apply_stop_edit(&conn, &StopEditOp {
            stop_id: 1, stop_version_id: a.new_version_id, stop_name: Some("B".into()), ..Default::default()
        }, "h", "n").unwrap();
        let err = apply_stop_undo(&conn, &stop_edit_undo_op(a.edit_id, 1, old, a.new_version_id), "h", "n").unwrap_err();
        assert!(err.to_string().contains("cannot be undone"), "{err}");
        // bogus old_svi: rows-affected guard
        let err = apply_stop_undo(&conn, &stop_edit_undo_op(b.edit_id, 1, 999, b.new_version_id), "h", "n").unwrap_err();
        assert!(err.to_string().contains("cannot be undone"), "{err}");
        // zero partial state: no undo rows, versions as after edit b
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM edit_log WHERE action = 'undo'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        let cur: i64 = conn.query_row(
            "SELECT stop_version_id FROM stop_version WHERE stop_id = 1 AND valid_to IS NULL",
            [], |r| r.get(0)).unwrap();
        assert_eq!(cur, b.new_version_id);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rm-db apply_stop_undo 2>&1 | tail -5`
Expected: FAIL — `StopEditRow`/`load_stop_edit`/`StopEditUndoOp`/`apply_stop_undo` not found.

- [ ] **Step 3: Implement the read** (near `load_station_rename_edit`)

```rust
/// One edit_log row for the stop-undo plan (ANY entity — the plan's
/// entity guard is the discriminator). `changes` parsed as JSON;
/// `None` when the id does not exist.
#[derive(Debug)]
pub struct StopEditRow {
    pub edit_id: i64,
    pub entity: String,
    pub stop_id: i64, // edit_log.entity_id (stable)
    pub action: String,
    pub old_stop_version_id: Option<i64>, // version_before
    pub new_stop_version_id: Option<i64>, // version_after
    pub changes: serde_json::Value,
}

pub fn load_stop_edit(conn: &Connection, edit_id: i64) -> Result<Option<StopEditRow>> {
    let row = conn.query_row(
        "SELECT edit_id, entity, entity_id, action, version_before, version_after, changes
         FROM edit_log WHERE edit_id = ?1",
        [edit_id],
        |r| {
            Ok(StopEditRow {
                edit_id: r.get(0)?,
                entity: r.get(1)?,
                stop_id: r.get(2)?,
                action: r.get(3)?,
                old_stop_version_id: r.get(4)?,
                new_stop_version_id: r.get(5)?,
                changes: serde_json::from_str(&r.get::<_, String>(6)?)?,
            })
        },
    );
    match row {
        Ok(e) => Ok(Some(e)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}
```

- [ ] **Step 4: Implement the undo** (after `apply_stop_regroup`)

```rust
#[derive(Debug)]
pub struct StopEditUndoOp {
    pub edit_id: i64,
    pub stop_id: i64,
    pub old_svi: i64, // edit_log.version_before — the predecessor to reopen
    pub cur_svi: i64, // edit_log.version_after — must still be current
}

/// Whole-tx stop-edit undo: reopen the predecessor, close the minted row,
/// repoint rsv back, MINT NOTHING. Bearing handling derives from the
/// undone edit's recorded `changes`: a position edit deleted the
/// predecessor's bearing (and a refresh may have recomputed one at the
/// churned svi — deleted here); otherwise the carried row moves back with
/// its `computed_at`. Stale marks re-derive from the same fields.
/// All guards run before any write; every version UPDATE checks
/// rows-affected == 1.
pub fn apply_stop_undo(
    conn: &Connection,
    op: &StopEditUndoOp,
    editor: &str,
    note: &str,
) -> Result<EditOutcome> {
    let tx = conn.unchecked_transaction()?;
    // guards BEFORE any write
    let cur: (Option<String>, String) = match tx.query_row(
        "SELECT valid_to, source FROM stop_version WHERE stop_version_id = ?1",
        [op.cur_svi],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ) {
        Ok(v) => v,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Err(EditError::NotUndoable(op.edit_id).into())
        }
        Err(e) => return Err(e.into()),
    };
    if cur.0.is_some() || cur.1 != "manual" {
        return Err(EditError::NotUndoable(op.edit_id).into());
    }
    let n: i64 = tx.query_row(
        "SELECT COUNT(*) FROM stop_version WHERE stop_version_id = ?1",
        [op.old_svi],
        |r| r.get(0),
    )?;
    if n == 0 {
        return Err(EditError::NotUndoable(op.edit_id).into());
    }
    let changes: String = match tx.query_row(
        "SELECT changes FROM edit_log WHERE edit_id = ?1", [op.edit_id], |r| r.get(0),
    ) {
        Ok(v) => v,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Err(EditError::NotUndoable(op.edit_id).into())
        }
        Err(e) => return Err(e.into()),
    };
    let cj: serde_json::Value = serde_json::from_str(&changes)?;
    let position_changed =
        cj.get("latitude").is_some() || cj.get("longitude").is_some();
    let regrouped = cj.get("station_id").is_some();

    let now = crate::now_rfc3339();
    let n = tx.execute(
        "UPDATE stop_version SET valid_to = ?1
         WHERE stop_version_id = ?2 AND valid_to IS NULL",
        params![now, op.cur_svi],
    )?;
    if n != 1 {
        return Err(EditError::NotUndoable(op.edit_id).into());
    }
    let n = tx.execute(
        "UPDATE stop_version SET valid_to = NULL WHERE stop_version_id = ?1",
        [op.old_svi],
    )?;
    if n != 1 {
        return Err(EditError::NotUndoable(op.edit_id).into());
    }
    tx.execute(
        "UPDATE route_stop_version SET stop_version_id = ?1 WHERE stop_version_id = ?2",
        params![op.old_svi, op.cur_svi],
    )?;

    tx.execute(
        "DELETE FROM stop_bearing WHERE stop_version_id = ?1",
        [op.cur_svi],
    )?;
    if !position_changed {
        carry_bearing(&tx, op.cur_svi, op.old_svi)?;
    }

    tx.execute(
        "INSERT INTO edit_log (ts, editor, entity, entity_id, action, changes, note, undo_of, version_before, version_after)
         VALUES (?1, ?2, 'stop', ?3, 'undo', '{}', ?4, ?5, ?6, ?7)",
        params![
            now,
            editor,
            op.stop_id,
            if note.is_empty() { None } else { Some(note) },
            op.edit_id,
            op.cur_svi,
            op.old_svi
        ],
    )?;
    let undo_edit_id = tx.last_insert_rowid();

    let mut stale_routes = Vec::new();
    if position_changed {
        mark_bearings_stale(&tx, undo_edit_id, &now)?;
        // rsv already points at `old_svi` (repointed above)
        stale_routes = mark_position_stale(&tx, op.old_svi, undo_edit_id, &now)?;
    } else if regrouped {
        tx.execute(
            "INSERT OR IGNORE INTO stale_mark (table_name, key, cause_edit_id, marked_at)
             VALUES ('station_bearing', '*', ?1, ?2)",
            params![undo_edit_id, now],
        )?;
    }
    tx.commit()?;
    Ok(EditOutcome {
        edit_id: undo_edit_id,
        new_version_id: op.old_svi,
        stale_routes,
    })
}
```

- [ ] **Step 5: Re-export** `StopEditRow, load_stop_edit, StopEditUndoOp, apply_stop_undo` in `crates/rm-db/src/lib.rs`.

- [ ] **Step 6: Run the full rm-db suite**

Run: `cargo test -p rm-db 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/rm-db/src/edit.rs crates/rm-db/src/lib.rs
git commit -m "rm-db: load_stop_edit + apply_stop_undo — generic predecessor restore"
```

---

### Task 5: rm-db — fold scenario-3 minors into `apply_station_rename_undo`

**Files:**
- Modify: `crates/rm-db/src/edit.rs:621-703` (`apply_station_rename_undo` guards + reopen UPDATEs)
- Modify: `crates/rm-edit/tests/undo.rs` (new e2e test)
- Test: same

**Interfaces:**
- Consumes: existing `apply_station_rename_undo`, `StationRenameUndoOp` (signatures unchanged).
- Produces: same signature, stronger guards — callers unaffected.

- [ ] **Step 1: Write the failing e2e test** (append to `crates/rm-edit/tests/undo.rs`, which already has `fixture(&conn)` building stop_version 10 with a bearing row)

```rust
#[test]
fn undo_station_rename_refuses_stop_edit_row() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    // a plain stop-edit row: entity='stop', no station_rename kind
    let out = rm_db::edit_stop(&conn, 10, &rm_db::StopEdit {
        stop_name: Some("N".into()), ..Default::default()
    }, "h", "n").unwrap();
    let err = undo_station_rename(&conn, out.edit_id, "herry", "").unwrap_err();
    assert!(err.to_string().contains("cannot be undone"), "{err}");
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p rm-edit undo_station_rename_refuses_stop_edit_row 2>&1 | tail -5`
Expected: this test may already pass via the kind guard (`plan_rename_undo` rejects `kind != station_rename`) — that's fine; the point of the task's test is pinning the behavior e2e. If it passes, proceed; do not weaken it.

- [ ] **Step 3: Strengthen `apply_station_rename_undo` guards** (in `crates/rm-db/src/edit.rs:630-647`)

Every currency guard also asserts the row is a manual edit (undo only reverts manual rows), and every reopen/close UPDATE checks rows-affected:

```rust
    let n: i64 = tx.query_row(
        "SELECT COUNT(*) FROM station_version
         WHERE station_version_id = ?1 AND valid_to IS NULL AND source = 'manual'",
        [op.new_station_version_id],
        |r| r.get(0),
    )?;
    if n == 0 {
        return Err(EditError::StationNotCurrent(op.new_station_version_id).into());
    }
    for pole in &op.poles {
        let n: i64 = tx.query_row(
            "SELECT COUNT(*) FROM stop_version
             WHERE stop_version_id = ?1 AND valid_to IS NULL AND source = 'manual'",
            [pole.new_svi],
            |r| r.get(0),
        )?;
        if n == 0 {
            return Err(EditError::NotUndoable(op.edit_id).into());
        }
    }
```

Wait — the pole rows minted by a rename ARE `source='manual'`, and so is the minted station version. The PREDECESSOR rows being reopened are pipeline rows (often `source != 'manual'`) — the assertion applies to the rows being CLOSED (the manual rename output), never to the predecessors. Keep the assertion exactly on `new_station_version_id` / `new_svi` as above.

Then wrap every reopen/close UPDATE with a count check:

```rust
    let n = tx.execute(
        "UPDATE station_version SET valid_to = ?1 WHERE station_version_id = ?2 AND valid_to IS NULL",
        params![now, op.new_station_version_id],
    )?;
    if n != 1 {
        return Err(EditError::NotUndoable(op.edit_id).into());
    }
    let n = tx.execute(
        "UPDATE station_version SET valid_to = NULL WHERE station_version_id = ?1",
        [op.old_station_version_id],
    )?;
    if n != 1 {
        return Err(EditError::NotUndoable(op.edit_id).into());
    }
    // same == 1 pattern for each pole's two stop_version UPDATEs
```

- [ ] **Step 4: Run both crates' suites**

Run: `cargo test -p rm-db -p rm-edit 2>&1 | tail -5`
Expected: PASS — existing rename-undo tests stay green (their fixtures mint manual rows, so the source assertion holds).

- [ ] **Step 5: Commit**

```bash
git add crates/rm-db/src/edit.rs crates/rm-edit/tests/undo.rs
git commit -m "rm-db: rename-undo guards — ==1 rows checks, source='manual' assertion; e2e wrong-entity test"
```

---

### Task 6: rm-edit — `StopEdit` command, `plan_stop_edit`, `edit_stop` service

**Files:**
- Create: `crates/rm-edit/src/stop.rs`
- Modify: `crates/rm-edit/src/lib.rs` (add `mod stop; pub use stop::*;`)
- Modify: `crates/rm-edit/src/rename.rs` (add 4 `EditError` variants)
- Test: `crates/rm-edit/tests/stop_edit.rs` (new)

**Interfaces:**
- Consumes: `rm_db::{StopSnapshot, load_stop_snapshot, StopEditOp, apply_stop_edit}` (Tasks 1–2).
- Produces (Task 9 consumes):
  - `#[derive(Debug, Default, Clone)] pub struct StopEdit { pub lat: Option<f64>, pub lon: Option<f64>, pub stop_name: Option<String> }`
  - `#[derive(Debug)] pub struct StopEditPlan { pub stop_id: i64, pub station_id: i64, pub stop_version_id: i64, pub stop_name: Option<String>, pub lat: Option<f64>, pub lon: Option<f64> }`
  - `pub fn plan_stop_edit(snapshot: &StopSnapshot, cmd: StopEdit) -> Result<StopEditPlan, EditError>`
  - `#[derive(Debug)] pub struct StopOutcome { pub edit_id: i64, pub new_stop_version_id: i64, pub stale_routes: Vec<i64> }`
  - `pub fn edit_stop(conn: &Connection, stop_version_id: i64, cmd: StopEdit, editor: &str, note: &str) -> Result<StopOutcome>`
  - `EditError` gains `NotCurrent(i64)`, `UnknownStation(i64)`, `EmptyEdit`, `NoOpEdit` (exact messages in Step 3).

- [ ] **Step 1: Add the `EditError` variants** (in `crates/rm-edit/src/rename.rs`, existing enum at line 51):

```rust
    #[error("stop version {0} is not the current version (or does not exist)")]
    NotCurrent(i64),
    #[error("station {0} does not exist or is not current")]
    UnknownStation(i64),
    #[error("edit changes nothing")]
    EmptyEdit,
    #[error("edit sets no field to a new value")]
    NoOpEdit,
```

- [ ] **Step 2: Write the failing unit tests** (`crates/rm-edit/tests/stop_edit.rs` — integration style over a real in-memory DB, like `tests/rename.rs`; pure plan tests build a `StopSnapshot` literal, no DB)

```rust
use rm_edit::{plan_stop_edit, EditError, StopEdit};
use rm_db::StopSnapshot;

fn snap() -> StopSnapshot {
    StopSnapshot {
        stop_id: 1,
        station_id: 1,
        stop_version_id: 10,
        stop_code: "001".into(),
        stop_name: "Old".into(),
        direction: 0,
        latitude: 24.0,
        longitude: 120.0,
    }
}

#[test]
fn plan_rejects_empty_and_no_op() {
    assert!(matches!(plan_stop_edit(&snap(), StopEdit::default()), Err(EditError::EmptyEdit)));
    // every Some field equals the snapshot → no-op
    assert!(matches!(
        plan_stop_edit(&snap(), StopEdit { lat: Some(24.0), ..Default::default() }),
        Err(EditError::NoOpEdit)
    ));
    assert!(matches!(
        plan_stop_edit(&snap(), StopEdit { stop_name: Some("Old".into()), ..Default::default() }),
        Err(EditError::NoOpEdit)
    ));
    // a real change alongside an unchanged field passes
    let p = plan_stop_edit(&snap(), StopEdit {
        lat: Some(24.0),
        stop_name: Some("New".into()),
        ..Default::default()
    }).unwrap();
    assert_eq!(p.stop_name.as_deref(), Some("New"));
    assert_eq!(p.lat, None, "unchanged field is not churned");
}

#[test]
fn plan_copies_snapshot_ids() {
    let p = plan_stop_edit(&snap(), StopEdit { lon: Some(120.5), ..Default::default() }).unwrap();
    assert_eq!((p.stop_id, p.station_id, p.stop_version_id), (1, 1, 10));
    assert_eq!(p.lon, Some(120.5));
}
```

- [ ] **Step 3: Implement `stop.rs`**

```rust
//! Stop edit (move/name), regroup, and their undo — commands, pure plans,
//! load→plan→apply services. Zero SQL: persistence is rm-db's.

use rusqlite::Connection;

use crate::EditError;

#[derive(Debug, Default, Clone)]
pub struct StopEdit {
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub stop_name: Option<String>,
}

#[derive(Debug)]
pub struct StopEditPlan {
    pub stop_id: i64,
    pub station_id: i64,
    pub stop_version_id: i64,
    pub stop_name: Option<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
}

#[derive(Debug)]
pub struct StopOutcome {
    pub edit_id: i64,
    pub new_stop_version_id: i64,
    pub stale_routes: Vec<i64>,
}

/// Pure planning policy: an empty edit and a no-op (every Some field
/// equals the current value) are refused; unchanged fields are dropped so
/// the apply churns only what moved. No I/O.
pub fn plan_stop_edit(
    snapshot: &rm_db::StopSnapshot,
    cmd: StopEdit,
) -> Result<StopEditPlan, EditError> {
    if cmd.lat.is_none() && cmd.lon.is_none() && cmd.stop_name.is_none() {
        return Err(EditError::EmptyEdit);
    }
    let no_op = cmd.lat.map_or(true, |v| v == snapshot.latitude)
        && cmd.lon.map_or(true, |v| v == snapshot.longitude)
        && cmd.stop_name.as_ref().map_or(true, |v| *v == snapshot.stop_name);
    if no_op {
        return Err(EditError::NoOpEdit);
    }
    Ok(StopEditPlan {
        stop_id: snapshot.stop_id,
        station_id: snapshot.station_id,
        stop_version_id: snapshot.stop_version_id,
        stop_name: cmd.stop_name.filter(|v| *v != snapshot.stop_name),
        lat: cmd.lat.filter(|v| *v != snapshot.latitude),
        lon: cmd.lon.filter(|v| *v != snapshot.longitude),
    })
}

/// rm-db's internal errors surface 1:1 as this crate's public EditError.
fn map_db_err(e: anyhow::Error) -> anyhow::Error {
    match e.downcast_ref::<rm_db::EditError>() {
        Some(rm_db::EditError::NotCurrent(id)) => EditError::NotCurrent(*id).into(),
        Some(rm_db::EditError::StationNotCurrent(id)) => EditError::StationNotCurrent(*id).into(),
        Some(rm_db::EditError::UnknownStation(id)) => EditError::UnknownStation(*id).into(),
        Some(rm_db::EditError::NotUndoable(id)) => EditError::NotUndoable(*id).into(),
        _ => e,
    }
}

/// Application service: load → plan (pure) → apply (one tx).
pub fn edit_stop(
    conn: &Connection,
    stop_version_id: i64,
    cmd: StopEdit,
    editor: &str,
    note: &str,
) -> Result<StopOutcome> {
    let snapshot = rm_db::load_stop_snapshot(conn, stop_version_id)?
        .ok_or(EditError::NotCurrent(stop_version_id))?;
    let plan = plan_stop_edit(&snapshot, cmd)?;
    let op = rm_db::StopEditOp {
        stop_id: plan.stop_id,
        stop_version_id: plan.stop_version_id,
        stop_name: plan.stop_name,
        lat: plan.lat,
        lon: plan.lon,
    };
    let applied = rm_db::apply_stop_edit(conn, &op, editor, note).map_err(map_db_err)?;
    Ok(StopOutcome {
        edit_id: applied.edit_id,
        new_stop_version_id: applied.new_version_id,
        stale_routes: applied.stale_routes,
    })
}
```

Note: `not_current` field-drop uses `filter`, so the plan carries only real changes — matches "every Some field equals the snapshot → NoOpEdit" already handled above.

- [ ] **Step 4: Wire the module** — `crates/rm-edit/src/lib.rs`:

```rust
mod stop;

pub use stop::*;
```

- [ ] **Step 5: Write the failing integration tests** (append to `crates/rm-edit/tests/stop_edit.rs`; copy the DB fixture pattern from `tests/rename.rs` — build station + stop_version 10 + bearing row + route chain with `rm_db::open_in_memory()`)

```rust
mod db {
    use super::*;
    use rm_db::open_in_memory;
    use rusqlite::Connection;

    /// Same shape as rm-edit's tests/undo.rs fixture: station 'S' (sv 20) +
    /// poles sv 10 (bearing row, rsv) and sv 11, id sequences seeded.
    /// Returns the current svi of stop 1 (= 10).
    fn fixture(conn: &Connection) -> i64 {
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
        10
    }

    /// Second station for regroup targets.
    fn second_station(conn: &Connection) {
        conn.execute(
            "INSERT INTO station (station_id, ext_uid, created_at)
             VALUES (2, 'tcg:node:2', '2026-01-01T00:00:00Z')", []).unwrap();
        conn.execute(
            "INSERT INTO station_version (station_version_id, station_id, station_group_id, station_name, latitude, longitude, valid_from, created_at)
             VALUES (21, 2, 1, 'T', 24.1, 120.1, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')", []).unwrap();
    }

    #[test]
    fn edit_stop_service_moves_and_reports_outcome() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        let out = rm_edit::edit_stop(&conn, old, StopEdit { lat: Some(24.001), ..Default::default() }, "herry", "move").unwrap();
        assert!(out.new_stop_version_id > old);
        assert_eq!(out.stale_routes, vec![1]);
    }

    #[test]
    fn edit_stop_service_refuses_stale_version() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        rm_edit::edit_stop(&conn, old, StopEdit { stop_name: Some("A".into()), ..Default::default() }, "h", "n").unwrap();
        let err = rm_edit::edit_stop(&conn, old, StopEdit { stop_name: Some("B".into()), ..Default::default() }, "h", "n").unwrap_err();
        assert!(err.to_string().contains("not the current version"), "{err}");
    }
}
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p rm-edit 2>&1 | tail -5`
Expected: PASS (unit plan tests + integration). (Run the "verify fails first" discipline per test batch: unit tests fail before Step 3, integration before Step 3 too — `rm_edit::edit_stop` won't exist.)

- [ ] **Step 7: Commit**

```bash
git add crates/rm-edit/src/stop.rs crates/rm-edit/src/lib.rs crates/rm-edit/src/rename.rs crates/rm-edit/tests/stop_edit.rs
git commit -m "rm-edit: StopEdit plan + edit_stop service (move/name)"
```

---

### Task 7: rm-edit — `RegroupStop`, `plan_regroup`, `regroup_stop` service

**Files:**
- Modify: `crates/rm-edit/src/stop.rs`
- Test: `crates/rm-edit/tests/stop_edit.rs`

**Interfaces:**
- Consumes: `rm_db::{StopSnapshot, load_stop_snapshot, station_is_current, StopRegroupOp, apply_stop_regroup}`, `map_db_err` (Task 6), `EditError::{NotCurrent, UnknownStation, NoOpEdit}`.
- Produces (Task 9 consumes):
  - `#[derive(Debug)] pub struct RegroupStop { pub stop_version_id: i64, pub new_station_id: i64 }`
  - `#[derive(Debug)] pub struct RegroupPlan { pub stop_id: i64, pub stop_version_id: i64, pub old_station_id: i64, pub new_station_id: i64 }`
  - `pub fn plan_regroup(snapshot: &StopSnapshot, cmd: RegroupStop) -> Result<RegroupPlan, EditError>`
  - `pub fn regroup_stop(conn: &Connection, cmd: RegroupStop, editor: &str, note: &str) -> Result<StopOutcome>`

- [ ] **Step 1: Write the failing tests** (unit plan tests in `stop_edit.rs`; `snap()` helper from Task 6)

```rust
use rm_edit::{plan_regroup, RegroupStop};

#[test]
fn plan_regroup_rejects_same_station() {
    assert!(matches!(
        plan_regroup(&snap(), RegroupStop { stop_version_id: 10, new_station_id: 1 }),
        Err(EditError::NoOpEdit)
    ));
    let p = plan_regroup(&snap(), RegroupStop { stop_version_id: 10, new_station_id: 2 }).unwrap();
    assert_eq!((p.stop_id, p.stop_version_id, p.old_station_id, p.new_station_id), (1, 10, 1, 2));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p rm-edit plan_regroup 2>&1 | tail -3`
Expected: FAIL — `plan_regroup` not found.

- [ ] **Step 3: Implement** (append to `crates/rm-edit/src/stop.rs`)

```rust
#[derive(Debug)]
pub struct RegroupStop {
    pub stop_version_id: i64,
    pub new_station_id: i64,
}

#[derive(Debug)]
pub struct RegroupPlan {
    pub stop_id: i64,
    pub stop_version_id: i64,
    pub old_station_id: i64,
    pub new_station_id: i64,
}

/// Pure policy: regrouping to the current station is a no-op. Target
/// existence/currency is a load-time fact, checked by the service.
pub fn plan_regroup(
    snapshot: &rm_db::StopSnapshot,
    cmd: RegroupStop,
) -> Result<RegroupPlan, EditError> {
    if cmd.new_station_id == snapshot.station_id {
        return Err(EditError::NoOpEdit);
    }
    Ok(RegroupPlan {
        stop_id: snapshot.stop_id,
        stop_version_id: snapshot.stop_version_id,
        old_station_id: snapshot.station_id,
        new_station_id: cmd.new_station_id,
    })
}

/// Application service: load → target check → plan (pure) → apply (one tx).
pub fn regroup_stop(
    conn: &Connection,
    cmd: RegroupStop,
    editor: &str,
    note: &str,
) -> Result<StopOutcome> {
    let snapshot = rm_db::load_stop_snapshot(conn, cmd.stop_version_id)?
        .ok_or(EditError::NotCurrent(cmd.stop_version_id))?;
    if !rm_db::station_is_current(conn, cmd.new_station_id)? {
        return Err(EditError::UnknownStation(cmd.new_station_id).into());
    }
    let plan = plan_regroup(&snapshot, cmd)?;
    let op = rm_db::StopRegroupOp {
        stop_id: plan.stop_id,
        stop_version_id: plan.stop_version_id,
        new_station_id: plan.new_station_id,
    };
    let applied = rm_db::apply_stop_regroup(conn, &op, editor, note).map_err(map_db_err)?;
    Ok(StopOutcome {
        edit_id: applied.edit_id,
        new_stop_version_id: applied.new_version_id,
        stale_routes: applied.stale_routes,
    })
}
```

- [ ] **Step 4: Integration test** (in `tests/stop_edit.rs` `mod db`; extend the fixture with a second station — reuse Task 3's `second_station` INSERTs)

```rust
    #[test]
    fn regroup_service_roundtrip_and_guards() {
        let conn = rm_db::open_in_memory().unwrap();
        let old = fixture(&conn);
        second_station(&conn);
        // unknown target
        let err = rm_edit::regroup_stop(&conn, RegroupStop { stop_version_id: old, new_station_id: 42 }, "h", "n").unwrap_err();
        assert!(err.to_string().contains("does not exist or is not current"), "{err}");
        // same station
        let err = rm_edit::regroup_stop(&conn, RegroupStop { stop_version_id: old, new_station_id: 1 }, "h", "n").unwrap_err();
        assert!(err.to_string().contains("sets no field"), "{err}");
        // happy path
        let out = rm_edit::regroup_stop(&conn, RegroupStop { stop_version_id: old, new_station_id: 2 }, "herry", "regroup").unwrap();
        assert!(out.new_stop_version_id > old);
        assert!(out.stale_routes.is_empty());
    }
```

- [ ] **Step 5: Run and commit**

Run: `cargo test -p rm-edit 2>&1 | tail -3` — Expected: PASS.

```bash
git add crates/rm-edit/src/stop.rs crates/rm-edit/tests/stop_edit.rs
git commit -m "rm-edit: RegroupStop plan + regroup_stop service"
```

---

### Task 8: rm-edit — `plan_stop_undo`, `undo_stop` service

**Files:**
- Modify: `crates/rm-edit/src/stop.rs`
- Test: `crates/rm-edit/tests/stop_edit.rs`

**Interfaces:**
- Consumes: `rm_db::{StopEditRow, load_stop_edit, StopEditUndoOp, apply_stop_undo}` (Task 4), `map_db_err`, `EditError::NotUndoable`.
- Produces (Task 9 consumes):
  - `#[derive(Debug, PartialEq)] pub struct StopUndoOp { pub edit_id: i64, pub stop_id: i64, pub old_svi: i64, pub cur_svi: i64 }`
  - `pub fn plan_stop_undo(edit: &rm_db::StopEditRow) -> Result<StopUndoOp, EditError>`
  - `pub fn undo_stop(conn: &Connection, edit_id: i64, editor: &str, note: &str) -> Result<StopOutcome>`

- [ ] **Step 1: Write the failing tests** (unit — no DB; build `rm_db::StopEditRow` literals)

```rust
use rm_edit::{plan_stop_undo, StopUndoOp};
use rm_db::StopEditRow;

fn row(entity: &str, action: &str) -> StopEditRow {
    StopEditRow {
        edit_id: 7,
        entity: entity.into(),
        stop_id: 1,
        action: action.into(),
        old_stop_version_id: Some(10),
        new_stop_version_id: Some(11),
        changes: serde_json::json!({"latitude": {"old": 24.0, "new": 24.001}}),
    }
}

#[test]
fn plan_stop_undo_guards() {
    let p = plan_stop_undo(&row("stop", "update")).unwrap();
    assert_eq!(p, StopUndoOp { edit_id: 7, stop_id: 1, old_svi: 10, cur_svi: 11 });
    // undo rows are not undoable
    assert!(matches!(plan_stop_undo(&row("stop", "undo")), Err(EditError::NotUndoable(7))));
    // wrong entity (a station rename row) is refused
    assert!(matches!(plan_stop_undo(&row("station", "update")), Err(EditError::NotUndoable(7))));
    // missing version ids
    let mut r = row("stop", "update");
    r.old_stop_version_id = None;
    assert!(matches!(plan_stop_undo(&r), Err(EditError::NotUndoable(7))));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p rm-edit plan_stop_undo 2>&1 | tail -3`
Expected: FAIL — not found.

- [ ] **Step 3: Implement** (append to `crates/rm-edit/src/stop.rs`)

```rust
#[derive(Debug, PartialEq)]
pub struct StopUndoOp {
    pub edit_id: i64,
    pub stop_id: i64,
    pub old_svi: i64,
    pub cur_svi: i64,
}

/// Pure guard: only a non-undo stop row with both version ids is
/// invertible. Kind-agnostic — legacy rows whose `changes` carry no
/// `kind` stay undoable; the generic predecessor restore covers every
/// single-row stop edit (move, name, regroup).
pub fn plan_stop_undo(edit: &rm_db::StopEditRow) -> Result<StopUndoOp, EditError> {
    if edit.entity != "stop" || edit.action == "undo" {
        return Err(EditError::NotUndoable(edit.edit_id));
    }
    let (Some(old_svi), Some(cur_svi)) = (edit.old_stop_version_id, edit.new_stop_version_id) else {
        return Err(EditError::NotUndoable(edit.edit_id));
    };
    if old_svi == cur_svi {
        return Err(EditError::NotUndoable(edit.edit_id));
    }
    Ok(StopUndoOp { edit_id: edit.edit_id, stop_id: edit.stop_id, old_svi, cur_svi })
}

/// Application service: load → plan (pure) → apply (one tx).
pub fn undo_stop(
    conn: &Connection,
    edit_id: i64,
    editor: &str,
    note: &str,
) -> Result<StopOutcome> {
    let row = rm_db::load_stop_edit(conn, edit_id)?
        .ok_or(EditError::NotUndoable(edit_id))?;
    let plan = plan_stop_undo(&row)?;
    let op = rm_db::StopEditUndoOp {
        edit_id: plan.edit_id,
        stop_id: plan.stop_id,
        old_svi: plan.old_svi,
        cur_svi: plan.cur_svi,
    };
    let applied = rm_db::apply_stop_undo(conn, &op, editor, note).map_err(map_db_err)?;
    Ok(StopOutcome {
        edit_id: applied.edit_id,
        new_stop_version_id: applied.new_version_id,
        stale_routes: applied.stale_routes,
    })
}
```

- [ ] **Step 4: Integration tests** (in `tests/stop_edit.rs` `mod db` — undo roundtrips for all three edit kinds + undo-of-undo + legacy row)

```rust
    #[test]
    fn undo_roundtrips_all_three_kinds() {
        let conn = rm_db::open_in_memory().unwrap();
        let old = fixture(&conn);
        second_station(&conn);

        // move
        let m = rm_edit::edit_stop(&conn, old, StopEdit { lat: Some(24.001), ..Default::default() }, "h", "m").unwrap();
        let u = rm_edit::undo_stop(&conn, m.edit_id, "h", "rev").unwrap();
        assert_eq!(u.new_stop_version_id, old);
        assert_eq!(u.stale_routes, vec![1], "move undo re-marks projections");

        // regroup
        let g = rm_edit::regroup_stop(&conn, RegroupStop { stop_version_id: old, new_station_id: 2 }, "h", "g").unwrap();
        let u = rm_edit::undo_stop(&conn, g.edit_id, "h", "rev").unwrap();
        assert_eq!(u.new_stop_version_id, old);

        // name
        let n = rm_edit::edit_stop(&conn, old, StopEdit { stop_name: Some("N".into()), ..Default::default() }, "h", "n").unwrap();
        let u = rm_edit::undo_stop(&conn, n.edit_id, "h", "rev").unwrap();
        assert_eq!(u.new_stop_version_id, old);
        assert!(u.stale_routes.is_empty());
    }

    #[test]
    fn undo_of_undo_refused() {
        let conn = rm_db::open_in_memory().unwrap();
        let old = fixture(&conn);
        let e = rm_edit::edit_stop(&conn, old, StopEdit { stop_name: Some("N".into()), ..Default::default() }, "h", "n").unwrap();
        let u = rm_edit::undo_stop(&conn, e.edit_id, "h", "rev").unwrap();
        let err = rm_edit::undo_stop(&conn, u.edit_id, "h", "rev").unwrap_err();
        assert!(err.to_string().contains("cannot be undone"), "{err}");
    }

    #[test]
    fn undo_stop_accepts_legacy_row_without_kind() {
        let conn = rm_db::open_in_memory().unwrap();
        let old = fixture(&conn);
        // synthesize a pre-scenario legacy row: {field:{old,new}} without a
        // kind marker — exactly what old rm_db::edit_stop wrote
        let e = rm_edit::edit_stop(&conn, old, StopEdit { stop_name: Some("N".into()), ..Default::default() }, "h", "n").unwrap();
        conn.execute(
            "UPDATE edit_log SET changes = '{\"stop_name\":{\"old\":\"Old\",\"new\":\"N\"}}' WHERE edit_id = ?1",
            [e.edit_id]).unwrap();
        let u = rm_edit::undo_stop(&conn, e.edit_id, "h", "rev").unwrap();
        assert_eq!(u.new_stop_version_id, old);
    }
```

- [ ] **Step 5: Run and commit**

Run: `cargo test -p rm-edit 2>&1 | tail -3` — Expected: PASS.

```bash
git add crates/rm-edit/src/stop.rs crates/rm-edit/tests/stop_edit.rs
git commit -m "rm-edit: plan_stop_undo + undo_stop — generic kind-agnostic stop undo"
```

---

### Task 9: rm-web rewire + delete `rm_db::{edit_stop, undo, StopEdit}`

**Files:**
- Modify: `crates/rm-web/src/handlers/edit.rs` (handlers at lines 83–124 and 152–167, `edit_conflict` at 75)
- Modify: `crates/rm-web/Cargo.toml` (add `rm-edit.workspace = true` if absent)
- Modify: `crates/rm-db/src/edit.rs` (DELETE `StopEdit`, `edit_stop` at 44–142, `undo` at 195–283, and their tests: `edit_stop_appends_version_and_repoints_links`, `name_only_edit_marks_only_bearings`, `edit_stop_rejects_non_current_version`, `second_edit_of_same_version_rejected`, `undo_restores_predecessor`, `undo_validates_current_and_rejects_undo_of_undo`, `undo_of_position_edit_remarks_stale`, `undo_of_name_only_edit_marks_only_bearings`)
- Modify: `crates/rm-db/src/lib.rs:18-24` (drop `edit_stop, undo, StopEdit` from the re-export)
- Modify: `crates/rm-db/src/edit.rs` tests that CALL `edit_stop`/`undo` as setup (e.g. `load_stop_snapshot_none_for_non_current_or_missing` from Task 1) — switch them to `apply_stop_edit`
- Modify: `crates/rm-edit/tests/undo.rs` (`undo_station_rename_refuses_when_pole_edited_after_rename` + Task 5's `undo_station_rename_refuses_stop_edit_row` use `rm_db::edit_stop`/`rm_db::StopEdit`) and `crates/rm-edit/tests/rename.rs` (same, wherever it calls them) — switch to `rm_edit::edit_stop` / `rm_edit::StopEdit`
- Test: `crates/rm-web/tests/edit.rs` (existing — must pass unchanged)

**Interfaces:**
- Consumes: `rm_edit::{edit_stop, regroup_stop, undo_stop, StopEdit, RegroupStop, EditError, StopOutcome}` (Tasks 6–8).
- Produces: same HTTP surface (`POST /api/edit/stop/{svi}`, `POST /api/edit/undo/{edit_id}`), same response JSON keys. No regroup endpoint.

- [ ] **Step 1: Rewire `edit_conflict`** (`crates/rm-web/src/handlers/edit.rs:75`):

```rust
/// `rm_edit::EditError` splits into request problems (400: empty/no-op
/// edit) and races / bad targets (409: version no longer current,
/// un-undoable edit, unknown regroup target).
fn edit_conflict(err: ApiError) -> ApiError {
    if let Some(e) = err.downcast_ref::<rm_edit::EditError>() {
        return match e {
            rm_edit::EditError::EmptyEdit | rm_edit::EditError::NoOpEdit
            | rm_edit::EditError::EmptyNewName | rm_edit::EditError::NoOpRename => {
                ApiError::bad_request(&e.to_string())
            }
            rm_edit::EditError::NotCurrent(_)
            | rm_edit::EditError::StationNotCurrent(_)
            | rm_edit::EditError::NotUndoable(_)
            | rm_edit::EditError::UnknownStation(_) => {
                ApiError::conflict(&e.to_string())
            }
        };
    }
    err
}
```

- [ ] **Step 2: Rewire the two handlers** — routes, auth, validation, response keys unchanged. Note the outcome field renames: `rm_edit::StopOutcome.new_stop_version_id` (was `rm_db::EditOutcome.new_version_id`).

`edit_stop` handler body after `let note = note.unwrap_or_default();`:

```rust
    let out = run_db_write(state, move |conn| {
        rm_edit::edit_stop(
            conn,
            stop_version_id,
            rm_edit::StopEdit { lat, lon, stop_name },
            &editor,
            &note,
        )
    })
    .await
    .map_err(edit_conflict)?;
    Ok(Json(json!({
        "edit_id": out.edit_id,
        "new_stop_version_id": out.new_stop_version_id,
        "stale_routes": out.stale_routes,
    })))
```

`undo` handler body after the `ApiJson(b)` extraction:

```rust
    let out = run_db_write(state, move |conn| {
        rm_edit::undo_stop(conn, edit_id, &editor, b.note.as_deref().unwrap_or(""))
    })
    .await
    .map_err(edit_conflict)?;
    Ok(Json(json!({
        "edit_id": out.edit_id,
        "new_stop_version_id": out.new_stop_version_id,
    })))
```

- [ ] **Step 3: Add `rm-edit` to rm-web's deps** if missing (`crates/rm-web/Cargo.toml`): `rm-edit.workspace = true` (if `[workspace.dependencies]` in the root Cargo.toml has no `rm-edit` entry, add `rm-edit = { path = "../rm-edit" }` there first).

- [ ] **Step 4: Update the rm-edit integration tests** — in `crates/rm-edit/tests/undo.rs` and `crates/rm-edit/tests/rename.rs`, replace every `rm_db::edit_stop(&conn, …, &rm_db::StopEdit { … }, …)` call with `rm_edit::edit_stop(&conn, …, rm_edit::StopEdit { … }, …)` (note: value, not reference — the rm-edit command is taken by value).

- [ ] **Step 5: Delete the old rm-db paths** — `StopEdit` struct, `edit_stop`, `undo`, their 8 tests, and the re-export entries. Switch any remaining test-callers (Task 1's `load_stop_snapshot_none_for_non_current_or_missing`, and any other test using `edit_stop`/`undo` as setup) to `apply_stop_edit` + `StopEditOp`.

- [ ] **Step 6: Run the workspace suite**

Run: `cargo test --workspace 2>&1 | tail -8`
Expected: PASS — rm-web handler tests (`stop_edit_happy_path_writes_version_and_marks`, `log_and_undo_roundtrip`, `stop_edit_validates_body`, `edit_routes_absent_without_edit_flag`) unchanged and green.

- [ ] **Step 7: Commit**

```bash
git add crates/rm-web/src/handlers/edit.rs crates/rm-web/Cargo.toml crates/rm-db/src/edit.rs crates/rm-db/src/lib.rs
git commit -m "rm-web: rewire edit/undo onto rm-edit; delete rm_db::edit_stop/undo"
```

---

### Task 10: Workspace verification + memory update

**Files:**
- No code changes. Verification + docs touch only.

- [ ] **Step 1: Full workspace test + clippy**

Run: `cargo test --workspace 2>&1 | tail -5 && cargo clippy --workspace 2>&1 | tail -5`
Expected: all tests pass; no new clippy warnings.

- [ ] **Step 2: Grep for stragglers**

Run: `grep -rn "rm_db::edit_stop\|rm_db::undo\b\|rm_db::StopEdit" crates/ | grep -v target`
Expected: no hits (except `apply_stop_undo`/`undo_stop` variants).

- [ ] **Step 3: Commit any fixups, then report**

The executor reports: final test count, and that `rm_db::{edit_stop, undo, StopEdit}` are gone from the public API.
