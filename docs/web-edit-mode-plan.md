# rm-web Edit Mode — Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Manual stop edits (position, name) in rm-web with version-append semantics, audit log, staleness marks, token auth, and a structural one-shot import guard.

**Architecture:** New `rm_db::edit` module holds the transactional edit primitive (close old version → mint new `source='manual'` version → repoint FKs → write `edit_log` + `stale_mark`). rm-web grows an `--edit` mode that mounts `/api/edit/*` POST routes behind a token extractor; writes serialize through an app-level mutex on mutable connections. Vanilla-JS client adds a token toggle, click-to-place position fix, and rename.

**Tech Stack:** Rust (axum, rusqlite, tokio), SQLite (WAL), vanilla JS + Leaflet.

**Spec:** `docs/web-edit-mode.md` — read it first; this plan argues from it.

**Scope:** Phase 1 only. Phases 2–4 (station curation, refresh runbook, authoring) get their own plans.

## Global Constraints

- Pipeline binaries always release mode: `cargo run -r -p <crate>` (project CLAUDE.md).
- Every write requires editor + note; no silent edits (spec §UI).
- Edit routes exist only when `--edit` is passed — absent means 404, not 401 (spec §Server write path).
- `valid_to IS NULL` remains the single definition of "current version row" (spec §Decisions).
- Rust code follows existing crate style: `anyhow::Result`, `rusqlite::params!`, `rm_db::now_rfc3339()`, `rm_db::mint(&tx, "table_name")`.
- Commits end with `Co-Authored-By: Claude Code <noreply@anthropic.com>`.

## Review Focus

Failure modes the spec implies but happy-path tests won't exercise; each is pinned by a test in the owning task below.

1. **Edit targeting a non-current version id** (stale panel link after someone else edited) → rejected, nothing written. → Task 3 test `edit_stop_rejects_non_current_version`.
2. **Second edit of the same version** (concurrent/duplicate submit) → rejected `NotCurrent`, no partial state. → Task 3 test `second_edit_of_same_version_rejected`.
3. **Empty or invalid edit body** (`{}`, lat=999, lat="x") → 400, no write. → Task 4 test `stop_edit_validates_body`.
4. **Edit routes mounted without `--edit`** → 404, existence not leaked. → Task 4 test `edit_routes_absent_without_edit_flag`.
5. **Undo validation**: undoing an edit that is no longer the current version, or undoing an `undo` row → rejected. → Task 3 test `undo_validates_current_and_rejects_undo_of_undo`.

---

### Task 1: Migration `0010_edit`

**Files:**
- Create: `crates/rm-db/migrations/0010_edit.sql`
- Modify: `crates/rm-db/src/migrate.rs` (MIGRATIONS array + names test)

**Interfaces:**
- Produces: tables `edit_log`, `stale_mark`; columns `stop_version.source TEXT NULL`, `station_version.source TEXT NULL` (`NULL` = pipeline origin, `'manual'` = edited); `edit_log.version_before` / `version_after` used by Task 3's undo.

- [ ] **Step 1: Write the failing test**

In `crates/rm-db/src/migrate.rs` tests module, update the existing `migrations_are_ordered_and_named` expected array to append `"0010_edit"`, and add:

```rust
#[test]
fn edit_tables_exist_after_migrate() {
    let conn = open_in_memory().unwrap();
    for tbl in ["edit_log", "stale_mark"] {
        let n: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM pragma_table_info('{tbl}')"),
                [],
                |r| r.get(0),
            )
            .unwrap();
            assert!(n > 0, "{tbl} exists");
    }
    // version tables gain a nullable source column (NULL = pipeline)
    for tbl in ["stop_version", "station_version"] {
        let n: i64 = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM pragma_table_info('{tbl}') WHERE name = 'source'"
                ),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "{tbl}.source exists");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rm-db edit_tables_exist`
Expected: FAIL (no such table: edit_log)

- [ ] **Step 3: Write the migration**

`crates/rm-db/migrations/0010_edit.sql`:

```sql
-- 0010_edit: manual edit audit trail + staleness marks (docs/web-edit-mode.md).
-- source NULL = pipeline origin, 'manual' = user edit.

ALTER TABLE stop_version ADD COLUMN source TEXT;
ALTER TABLE station_version ADD COLUMN source TEXT;

CREATE TABLE edit_log (
    edit_id     INTEGER PRIMARY KEY,
    ts          TEXT NOT NULL,
    editor      TEXT NOT NULL,
    entity      TEXT NOT NULL,          -- 'stop' | 'station' | ...
    entity_id   INTEGER NOT NULL,       -- stop_id / station_id (stable id)
    action      TEXT NOT NULL,          -- 'update' | 'move_station' | 'undo' | 'create'
    changes     TEXT NOT NULL,          -- JSON {field: {old, new}}
    note        TEXT,
    undo_of     INTEGER REFERENCES edit_log(edit_id),
    version_before INTEGER,             -- version row closed by this edit
    version_after  INTEGER              -- version row made current (undo: re-validates version_before)
);

CREATE TABLE stale_mark (
    table_name    TEXT NOT NULL,
    key           TEXT NOT NULL,        -- unit the refresh step consumes ('*' = whole table)
    cause_edit_id INTEGER NOT NULL REFERENCES edit_log(edit_id),
    marked_at     TEXT NOT NULL,
    PRIMARY KEY (table_name, key)
);
```

Register in `MIGRATIONS`:

```rust
    (
        "0010_edit",
        include_str!("../migrations/0010_edit.sql"),
    ),
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rm-db`
Expected: PASS (including updated names test)

- [ ] **Step 5: Commit**

```bash
git add crates/rm-db/migrations/0010_edit.sql crates/rm-db/src/migrate.rs
git commit -m "rm-db: 0010_edit migration — edit_log, stale_mark, version source column"
```

---

### Task 2: One-shot import guard

**Files:**
- Modify: `crates/rm-identity/src/lib.rs` (`run`, top of function + end)
- Test: `crates/rm-identity/tests/one_shot.rs` (create)

**Interfaces:**
- Consumes: `rm_db::set_batch_status`, existing `import_batch.status` (`OPEN`/`CLOSED`/`FAILED`).
- Produces: `rm_identity::run` now (a) errors unless the batch is `OPEN`, (b) sets it `CLOSED` on success. Behavior other pipeline steps rely on is unchanged (`latest_open_batch` default target still works: ingest creates OPEN, identity consumes it).

- [ ] **Step 1: Write the failing test**

`crates/rm-identity/tests/one_shot.rs`:

```rust
//! One-shot import: identity refuses non-OPEN batches and closes on success.

use rm_db::open_in_memory;
use rm_ingest::testfix;

#[test]
fn identity_consumes_open_batch_then_refuses_reuse() {
    let dir = tempfile::tempdir().unwrap();
    testfix::make_fixture(dir.path());
    let conn = open_in_memory().unwrap();
    let sum = rm_ingest::run(&conn, dir.path()).unwrap();
    let batch = sum.batch_id;

    let first = rm_identity::run(&conn, batch).unwrap();
    assert!(first.stops > 0);

    let status: String = conn
        .query_row("SELECT status FROM import_batch WHERE batch_id = ?1", [batch], |r| r.get(0))
        .unwrap();
    assert_eq!(status, "CLOSED");

    let second = rm_identity::run(&conn, batch);
    assert!(second.is_err(), "closed batch must be refused");
}
```

Note: `rm-ingest` dev-dependency with feature `testfix` must be declared in `crates/rm-identity/Cargo.toml` `[dev-dependencies]` if absent (check first; sibling crates `rm-registry`/`rm-verify` already do this — copy their line).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rm-identity one_shot`
Expected: FAIL — second `run` returns Ok (batch still OPEN, no guard)

- [ ] **Step 3: Implement the guard**

In `rm_identity::run` (`crates/rm-identity/src/lib.rs`), after the `valid_from` query at the top:

```rust
    let status: String = conn.query_row(
        "SELECT status FROM import_batch WHERE batch_id = ?1",
        [batch_id],
        |r| r.get(0),
    )?;
    anyhow::ensure!(
        status == "OPEN",
        "batch {batch_id} is {status} — imports are one-shot; \
         start a fresh DB for a new capture (docs/web-edit-mode.md)"
    );
```

At the end of `run`, just before returning the `Summary`:

```rust
    rm_db::set_batch_status(conn, batch_id, "CLOSED")?;
```

- [ ] **Step 4: Run the crate's tests**

Run: `cargo test -p rm-identity`
Expected: PASS (existing identity tests use their own OPEN batches; if any re-run `run` on the same batch, update them to ingest into a fresh DB — the error message will say so)

- [ ] **Step 5: Commit**

```bash
git add crates/rm-identity/src/lib.rs crates/rm-identity/tests/one_shot.rs crates/rm-identity/Cargo.toml
git commit -m "rm-identity: one-shot import guard — refuse non-OPEN batches, close on success"
```

---

### Task 3: Edit primitive (`rm_db::edit`)

**Files:**
- Create: `crates/rm-db/src/edit.rs`
- Modify: `crates/rm-db/src/lib.rs` (add `mod edit;` + `pub use edit::{...};`)

**Interfaces:**
- Consumes: `rm_db::mint`, `rm_db::now_rfc3339`, schema from Task 1.
- Produces (Task 4/5 consume; exact signatures):

```rust
pub struct StopEdit {
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub stop_name: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum EditError {
    #[error("stop version {0} is not the current version")]
    NotCurrent(i64),
    #[error("edit {0} cannot be undone (not current, or is itself an undo)")]
    NotUndoable(i64),
    #[error("entity not found: {0}")]
    NotFound(String),
}

pub struct EditOutcome {
    pub edit_id: i64,
    pub new_version_id: i64,   // undo: the re-validated predecessor id
    pub stale_routes: Vec<i64> // route ids marked for projection refresh
}

pub fn edit_stop(
    conn: &Connection,
    stop_version_id: i64,
    edit: &StopEdit,
    editor: &str,
    note: &str,
) -> Result<EditOutcome>;          // Err(EditError) wrapped in anyhow

pub fn undo(conn: &Connection, edit_id: i64, editor: &str, note: &str)
    -> Result<EditOutcome>;

#[derive(serde::Serialize)]
pub struct EditLogRow {
    pub edit_id: i64, pub ts: String, pub editor: String,
    pub action: String, pub changes: String, pub note: Option<String>,
    pub undo_of: Option<i64>,
}

pub fn entity_history(conn: &Connection, entity: &str, entity_id: i64)
    -> Result<Vec<EditLogRow>>;
```

(`thiserror` and `serde` are already workspace deps; `serde` derive used only on `EditLogRow`.)

Semantics (spec §Edit primitive):
- `edit_stop`: one tx — assert version current; `UPDATE … SET valid_to = now`; mint new `stop_version` (`source='manual'`); `UPDATE route_stop_version SET stop_version_id = new WHERE stop_version_id = old`; `DELETE FROM stop_bearing WHERE stop_version_id = old`; insert `edit_log` (`changes` JSON `{field: {old, new}}` only for fields present in `edit`, `version_before/after`); if lat or lon changed: insert `stale_mark` `('path_stop_projection', route_id)` per serving route + `('stop_bearing','*')` + `('station_bearing','*')` (`INSERT OR IGNORE`).
- Serving routes SQL:

```sql
SELECT DISTINCT rv.route_id
FROM route_stop_version rsv
JOIN subroute_version sv ON sv.subroute_version_id = rsv.subroute_version_id
JOIN route_version rv ON rv.route_version_id = sv.route_version_id
WHERE rsv.stop_version_id = ?1
```

- `undo`: load `edit_log` row; require `entity='stop'`, `action != 'undo'`, and that `version_after` is still the current version (`NotCurrent` otherwise); tx — close current, `UPDATE stop_version SET valid_to = NULL WHERE stop_version_id = version_before`, repoint `route_stop_version` to `version_before`, `DELETE FROM stop_bearing WHERE stop_version_id = version_after`, log `action='undo'` with `undo_of = edit_id`, `version_after = version_before`.

- [ ] **Step 1: Write the failing tests**

In `crates/rm-db/src/edit.rs` bottom `#[cfg(test)] mod tests` (fixture = raw INSERTs, matching existing rm-db test style). Minimal fixture helper:

```rust
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
             VALUES (1, 1, 1, 'S', 24.0, 120.0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')", []).unwrap();
        conn.execute(
            "INSERT INTO stop (stop_id, created_at) VALUES (1, '2026-01-01T00:00:00Z')", []).unwrap();
        conn.execute(
            "INSERT INTO stop_version (stop_version_id, stop_id, station_id, stop_code, stop_name, direction, latitude, longitude, valid_from, created_at)
             VALUES (10, 1, 1, '001', 'Old', 0, 24.0, 120.0, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')", []).unwrap();
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
        10
    }
```

Tests:

```rust
    use super::*;
    use crate::open_in_memory;
    use rusqlite::Connection;

    #[test]
    fn edit_stop_appends_version_and_repoints_links() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        let out = edit_stop(&conn, old, &StopEdit { lat: Some(24.001), lon: None, stop_name: Some("New".into()) }, "herry", "fix").unwrap();
        // old closed, new current + manual
        let (to, src): (Option<String>, Option<String>) = conn.query_row(
            "SELECT valid_to, source FROM stop_version WHERE stop_version_id = ?1", [old], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert!(to.is_some());
        let (name, lat, src2): (String, f64, String) = conn.query_row(
            "SELECT stop_name, latitude, source FROM stop_version WHERE stop_version_id = ?1", [out.new_version_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!((name.as_str(), lat, src2.as_str()), ("New", 24.001, "manual"));
        // FK repointed
        let sv: i64 = conn.query_row("SELECT stop_version_id FROM route_stop_version WHERE route_stop_version_id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(sv, out.new_version_id);
        // edit_log row
        let (ent, eid, act): (String, i64, String) = conn.query_row(
            "SELECT entity, entity_id, action FROM edit_log WHERE edit_id = ?1", [out.edit_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!((ent.as_str(), eid, act.as_str()), ("stop", 1, "update"));
        // stale marks: position changed → projection route + both bearing tables
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 3);
    }

    #[test]
    fn name_only_edit_writes_no_stale_marks() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        let out = edit_stop(&conn, old, &StopEdit { lat: None, lon: None, stop_name: Some("N2".into()) }, "herry", "rename").unwrap();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        assert!(out.stale_routes.is_empty());
    }

    #[test]
    fn edit_stop_rejects_non_current_version() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        edit_stop(&conn, old, &StopEdit { stop_name: Some("a".into()), ..Default::default() }, "herry", "n").unwrap();
        let err = edit_stop(&conn, old, &StopEdit { stop_name: Some("b".into()), ..Default::default() }, "herry", "n").unwrap_err();
        assert!(err.to_string().contains("not the current version"), "{err}");
    }

    #[test]
    fn second_edit_of_same_version_rejected() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        let a = edit_stop(&conn, old, &StopEdit { lat: Some(24.001), lon: None, stop_name: None }, "herry", "first").unwrap();
        // duplicate submit of the SAME version id: a.new_version_id is current now
        let b = edit_stop(&conn, old, &StopEdit { lat: Some(24.002), lon: None, stop_name: None }, "herry", "second");
        assert!(b.is_err(), "second edit of same version must be rejected");
        // and no partial state: exactly one manual version exists
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM stop_version WHERE source = 'manual'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        // the current version is a's
        let cur: i64 = conn.query_row(
            "SELECT stop_version_id FROM stop_version WHERE valid_to IS NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(cur, a.new_version_id);
    }

    #[test]
    fn undo_restores_predecessor() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        let out = edit_stop(&conn, old, &StopEdit { stop_name: Some("New".into()), ..Default::default() }, "herry", "fix").unwrap();
        let u = undo(&conn, out.edit_id, "herry", "revert").unwrap();
        assert_eq!(u.new_version_id, old);
        let (to, name): (Option<String>, String) = conn.query_row(
            "SELECT valid_to, stop_name FROM stop_version WHERE stop_version_id = ?1", [old], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert!(to.is_none());
        assert_eq!(name, "Old");
        let sv: i64 = conn.query_row("SELECT stop_version_id FROM route_stop_version WHERE route_stop_version_id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(sv, old);
    }

    #[test]
    fn undo_validates_current_and_rejects_undo_of_undo() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        let a = edit_stop(&conn, old, &StopEdit { stop_name: Some("A".into()), ..Default::default() }, "h", "n").unwrap();
        // edit again → a.version_after no longer current
        edit_stop(&conn, a.new_version_id, &StopEdit { stop_name: Some("B".into()), ..Default::default() }, "h", "n").unwrap();
        assert!(undo(&conn, a.edit_id, "h", "n").is_err());
        // fresh chain: edit then undo, then undo the undo → rejected
        let c = edit_stop(&conn, old, &StopEdit { stop_name: Some("C".into()), ..Default::default() }, "h", "n").unwrap();
        let u = undo(&conn, c.edit_id, "h", "n").unwrap();
        assert!(undo(&conn, u.edit_id, "h", "n").is_err());
    }

    #[test]
    fn entity_history_lists_newest_first() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        let a = edit_stop(&conn, old, &StopEdit { stop_name: Some("A".into()), ..Default::default() }, "h", "n1").unwrap();
        undo(&conn, a.edit_id, "h", "n2").unwrap();
        let rows = entity_history(&conn, "stop", 1).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].action, "undo");
        assert_eq!(rows[1].action, "update");
        assert_eq!(rows[0].undo_of, Some(a.edit_id));
    }
```

(`StopEdit` derives `Default` for the `..Default::default()` spread.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rm-db edit`
Expected: FAIL — `edit` module doesn't exist

- [ ] **Step 3: Implement `edit.rs`**

Structure (full SQL per the Interfaces semantics above — single `conn.unchecked_transaction()`, mirror `split.rs`/`bearing.rs` style):

```rust
//! Manual edit primitive: version-append edits + audit + staleness marks.

use anyhow::{ensure, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Default)]
pub struct StopEdit { pub lat: Option<f64>, pub lon: Option<f64>, pub stop_name: Option<String> }

#[derive(Debug, Error)]
pub enum EditError {
    #[error("stop version {0} is not the current version")]
    NotCurrent(i64),
    #[error("edit {0} cannot be undone (not current, or is itself an undo)")]
    NotUndoable(i64),
}

pub struct EditOutcome { pub edit_id: i64, pub new_version_id: i64, pub stale_routes: Vec<i64> }

#[derive(Serialize)]
pub struct EditLogRow {
    pub edit_id: i64, pub ts: String, pub editor: String,
    pub action: String, pub changes: String, pub note: Option<String>,
    pub undo_of: Option<i64>,
}

pub fn edit_stop(conn: &Connection, stop_version_id: i64, edit: &StopEdit, editor: &str, note: &str) -> Result<EditOutcome> {
    ensure!(edit.lat.is_some() || edit.lon.is_some() || edit.stop_name.is_some(), "empty edit");
    let tx = conn.unchecked_transaction()?;
    let old: (i64, i64, String, String, i64, f64, f64) = match tx.query_row(
        "SELECT stop_id, station_id, stop_code, stop_name, direction, latitude, longitude
         FROM stop_version WHERE stop_version_id = ?1 AND valid_to IS NULL",
        [stop_version_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
    ) {
        Ok(v) => v,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Err(EditError::NotCurrent(stop_version_id).into()),
        Err(e) => return Err(e.into()),
    };
    let (stop_id, station_id, stop_code, old_name, direction, old_lat, old_lon) = old;
    let now = crate::now_rfc3339();
    let new_id = crate::mint(&tx, "stop_version")?;
    tx.execute("UPDATE stop_version SET valid_to = ?1 WHERE stop_version_id = ?2",
        params![now, stop_version_id])?;
    tx.execute(
        "INSERT INTO stop_version
            (stop_version_id, stop_id, station_id, stop_code, stop_name, direction,
             latitude, longitude, valid_from, created_at, source)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, 'manual')",
        params![new_id, stop_id, station_id, stop_code,
                edit.stop_name.clone().unwrap_or(old_name), direction,
                edit.lat.unwrap_or(old_lat), edit.lon.unwrap_or(old_lon), now],
    )?;
    tx.execute("UPDATE route_stop_version SET stop_version_id = ?1 WHERE stop_version_id = ?2",
        params![new_id, stop_version_id])?;
    tx.execute("DELETE FROM stop_bearing WHERE stop_version_id = ?1", params![stop_version_id])?;

    let mut changes = serde_json::Map::new();
    if let Some(v) = &edit.stop_name { changes.insert("stop_name".into(), json!({"old": old_name, "new": v})); }
    if let Some(v) = edit.lat { changes.insert("latitude".into(), json!({"old": old_lat, "new": v})); }
    if let Some(v) = edit.lon { changes.insert("longitude".into(), json!({"old": old_lon, "new": v})); }

    tx.execute(
        "INSERT INTO edit_log (ts, editor, entity, entity_id, action, changes, note, version_before, version_after)
         VALUES (?1, ?2, 'stop', ?3, 'update', ?4, ?5, ?6, ?7)",
        params![now, editor, stop_id, serde_json::Value::Object(changes).to_string(),
                if note.is_empty() { None } else { Some(note) }, stop_version_id, new_id],
    )?;
    let edit_id = tx.last_insert_rowid();

    let mut stale_routes = Vec::new();
    if edit.lat.is_some() || edit.lon.is_some() {
        {
            let mut stmt = tx.prepare(
                "SELECT DISTINCT rv.route_id
                 FROM route_stop_version rsv
                 JOIN subroute_version sv ON sv.subroute_version_id = rsv.subroute_version_id
                 JOIN route_version rv ON rv.route_version_id = sv.route_version_id
                 WHERE rsv.stop_version_id = ?1",
            )?;
            let rows = stmt.query_map([new_id], |r| r.get::<_, i64>(0))?;
            for rid in rows { stale_routes.push(rid?); }
        }
        for rid in &stale_routes {
            tx.execute("INSERT OR IGNORE INTO stale_mark (table_name, key, cause_edit_id, marked_at)
                        VALUES ('path_stop_projection', ?1, ?2, ?3)",
                params![rid.to_string(), edit_id, now])?;
        }
        for tbl in ["stop_bearing", "station_bearing"] {
            tx.execute("INSERT OR IGNORE INTO stale_mark (table_name, key, cause_edit_id, marked_at)
                        VALUES (?1, '*', ?2, ?3)", params![tbl, edit_id, now])?;
        }
    }
    tx.commit()?;
    Ok(EditOutcome { edit_id, new_version_id: new_id, stale_routes })
}
```

(`undo` and `entity_history` follow the same structure — their exact row operations are enumerated in the Interfaces block above: undo = close current / `valid_to = NULL` on `version_before` / repoint / delete bearing rows of `version_after` / log `action='undo'`; history = the one SELECT with `ORDER BY edit_id DESC`. `undo` reads its `edit_log` row first with `query_row(...).optional()` and maps `None` → `EditError::NotUndoable(edit_id)`, `Some(row)` with `action == 'undo'` or non-current `version_after` → `NotUndoable` too.)

Export from `crates/rm-db/src/lib.rs`:

```rust
mod edit;
pub use edit::{edit_stop, entity_history, undo, EditError, EditLogRow, EditOutcome, StopEdit};
```

Add `serde` (with `derive`) and `thiserror` to `rm-db`'s `Cargo.toml` dependencies if not already present (check the workspace first — likely already inherited).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rm-db`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/rm-db/src/edit.rs crates/rm-db/src/lib.rs crates/rm-db/Cargo.toml
git commit -m "rm-db: edit primitive — version-append stop edits, undo, history, stale marks"
```

---

### Task 4: rm-web write path, auth, `POST /api/edit/stop`

**Files:**
- Modify: `crates/rm-web/src/state.rs` (tokens, write mutex, `run_db_write`, accessors)
- Create: `crates/rm-web/src/handlers/edit.rs` (Editor extractor + stop endpoint)
- Modify: `crates/rm-web/src/handlers/mod.rs` (mount edit routes when enabled)
- Modify: `crates/rm-web/src/handlers/stats.rs` (+`edit_enabled`)
- Modify: `crates/rm-web/src/lib.rs` (`serve` signature), `crates/rm-web/src/main.rs` (`--edit`, `--edit-tokens` / env)
- Test: `crates/rm-web/tests/edit.rs` (create)

**Interfaces:**
- Consumes: Task 3's `rm_db::{edit_stop, StopEdit, EditError}`; `AppState::with_tiles`.
- Produces (Task 5/7 consume):
  - `AppState::with_edit(db_path, tokens: HashMap<String,String>) -> AppState` (token→editor map); `AppState::edit_enabled(&self) -> bool`; `AppState::db_path(&self) -> &Path`.
  - `crate::state::run_db_write<T, F>(state: AppState, f: F) -> Result<T, ApiError>` (same shape as `run_db`, mutable `rm_db::open`, serialized by `state.write_lock()`).
  - `POST /api/edit/stop/{stop_version_id}` body `{"lat"?, "lon"?, "stop_name"?, "note"?}` → `{"edit_id", "new_stop_version_id", "stale_routes": [..]}`; auth `Authorization: Bearer <token>`.
  - `/api/stats` response gains `"edit_enabled": bool`.

- [ ] **Step 1: Write the failing integration tests**

`crates/rm-web/tests/edit.rs` (file-backed temp DB — `run_db_write` needs a path; fixture reuses Task 3's table chain, plus one more route so stale marks have content):

```rust
//! Edit-mode integration: auth gating, stop edit endpoint, readback.

use std::collections::HashMap;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use rusqlite::Connection;
use tower::ServiceExt; // oneshot

fn edit_state(dir: &tempfile::TempDir) -> rm_web::state::AppState {
    let conn = rm_db::open(&dir.path().join("t.db")).unwrap();
    // copy the `fixture` helper VERBATIM from Task 3 Step 1's tests module
    // (station_group → station → stop → stop_version 10 → route chain →
    // route_stop_version; route 1 is the serving route tests assert on)
    fixture(&conn);
    drop(conn);
    let mut tokens = HashMap::new();
    tokens.insert("sekrit".to_string(), "herry".to_string());
    rm_web::state::AppState::with_edit(dir.path().join("t.db"), tokens)
}

async fn body_json(res: axum::response::Response) -> serde_json::Value {
    let b = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&b).unwrap()
}

#[tokio::test]
async fn stop_edit_requires_token() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    for (auth, want) in [(None, StatusCode::UNAUTHORIZED), (Some("Bearer nope"), StatusCode::FORBIDDEN)] {
        let mut req = Request::builder().method("POST").uri("/api/edit/stop/10")
            .header("content-type", "application/json");
        if let Some(a) = auth { req = req.header("authorization", a); }
        let res = rm_web::router_with(state.clone()).oneshot(req.body(Body::from("{}")).unwrap()).await.unwrap();
        assert_eq!(res.status(), want);
    }
    // no partial writes
    let conn = rm_db::open_readonly(&dir.path().join("t.db")).unwrap();
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM edit_log", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn stop_edit_happy_path_writes_version_and_marks() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    let res = rm_web::router_with(state).oneshot(
        Request::builder().method("POST").uri("/api/edit/stop/10")
            .header("authorization", "Bearer sekrit")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"lat": 24.001, "stop_name": "New", "note": "fix"}"#)).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert!(v["new_stop_version_id"].as_i64().unwrap() > 10);
    // readback: stats reports edit_enabled; stop detail is served
    let conn = rm_db::open_readonly(&dir.path().join("t.db")).unwrap();
    let src: String = conn.query_row(
        "SELECT source FROM stop_version WHERE valid_to IS NULL", [], |r| r.get(0)).unwrap();
    assert_eq!(src, "manual");
}

#[tokio::test]
async fn stop_edit_validates_body() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    for body in ["{}", r#"{"lat": 999}"#, r#"{"lat": "x"}"#] {
        let res = rm_web::router_with(state.clone()).oneshot(
            Request::builder().method("POST").uri("/api/edit/stop/10")
                .header("authorization", "Bearer sekrit")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string())).unwrap(),
        ).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "body {body}");
    }
}

#[tokio::test]
async fn edit_routes_absent_without_edit_flag() {
    let dir = tempfile::tempdir().unwrap();
    let conn = rm_db::open(&dir.path().join("t.db")).unwrap();
    fixture(&conn);
    drop(conn);
    let state = rm_web::state::AppState::with_tiles(dir.path().join("t.db"), None);
    let res = rm_web::router_with(state).oneshot(
        Request::builder().method("POST").uri("/api/edit/stop/10")
            .header("authorization", "Bearer sekrit")
            .body(Body::from("{}")).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
```

Dev-dependencies for rm-web tests (add what's missing): `tempfile`, `http-body-util`, `tower` (oneshot), `serde_json`, `axum` body types already in deps.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rm-web edit`
Expected: FAIL — no `with_edit`, no edit routes

- [ ] **Step 3: Implement state + extractor + endpoint + wiring**

`state.rs` additions:

```rust
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex as AsyncMutex;

// AppState gains:
//   edit_tokens: Option<Arc<HashMap<String, String>>>,   // token → editor
//   write_lock: Arc<AsyncMutex<()>>,
// with_edit(db_path, tokens) builds it; edit_enabled() = edit_tokens.is_some();
// db_path() -> &Path accessor; write_tokens() -> Option<&HashMap>.

/// Run one sync WRITE closure, serialized app-wide on a mutable connection.
pub async fn run_db_write<T, F>(state: AppState, f: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce(&Connection) -> Result<T> + Send + 'static,
{
    let _guard = state.write_lock().lock().await;
    let path = state.db_path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        let conn = rm_db::open(&path)?;
        f(&conn)
    })
    .await
    .expect("spawn_blocking join")
    .map_err(ApiError::from)
}
```

`handlers/edit.rs`:

```rust
//! Edit endpoints — mounted only when --edit passes tokens.

use axum::extract::{FromRef, FromRequestParts, Path, State};
use axum::http::{request::Parts, header::AUTHORIZATION, StatusCode};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::error::ApiError;
use crate::state::{run_db_write, AppState};

/// Bearer-token → editor name. 401 absent, 403 unknown.
pub struct Editor(pub String);

impl<S> FromRequestParts<S> for Editor
where
    S: Send + Sync,
    AppState: FromRef<S>,
{
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app = AppState::from_ref(state);
        let tokens = app.write_tokens().ok_or_else(|| ApiError::not_found("edit"))?;
        let hdr = parts.headers.get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(|| ApiError::unauthorized("missing bearer token"))?;
        let editor = tokens.get(hdr)
            .ok_or_else(|| ApiError::forbidden("unknown token"))?;
        Ok(Editor(editor.clone()))
    }
}

#[derive(Deserialize)]
pub struct StopEditBody { lat: Option<f64>, lon: Option<f64>, stop_name: Option<String>, note: Option<String> }

pub async fn edit_stop(
    Path(stop_version_id): Path<i64>,
    State(state): State<AppState>,
    Editor(editor): Editor,
    Json(b): Json<StopEditBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if b.lat.is_none() && b.lon.is_none() && b.stop_name.is_none() {
        return Err(ApiError::bad_request("no fields to edit"));
    }
    if let Some(lat) = b.lat { if !(-90.0..=90.0).contains(&lat) { return Err(ApiError::bad_request("lat out of range")); } }
    if let Some(lon) = b.lon { if !(-180.0..=180.0).contains(&lon) { return Err(ApiError::bad_request("lon out of range")); } }
    let edit = rm_db::StopEdit { lat: b.lat, lon: b.lon, stop_name: b.stop_name };
    let out = run_db_write(state, move |conn| {
        rm_db::edit_stop(conn, stop_version_id, &edit, &editor, b.note.as_deref().unwrap_or(""))
    }).await?;
    Ok(Json(json!({
        "edit_id": out.edit_id,
        "new_stop_version_id": out.new_version_id,
        "stale_routes": out.stale_routes,
    })))
}
```

`ApiError` gains helpers (error.rs):

```rust
    pub fn unauthorized(msg: &str) -> Self { Self { err: anyhow::anyhow!("{msg}"), status: Some(StatusCode::UNAUTHORIZED) } }
    pub fn forbidden(msg: &str) -> Self { Self { err: anyhow::anyhow!("{msg}"), status: Some(StatusCode::FORBIDDEN) } }
    pub fn not_found(msg: &str) -> Self { Self { err: anyhow::anyhow!("{msg}"), status: Some(StatusCode::NOT_FOUND) } }
```

(Name-shares with the existing free fn `not_found(&str) -> Response` — different call path (`ApiError::not_found`), no conflict.)

`handlers/mod.rs` — conditional mount:

```rust
pub mod edit;
// in router(state-dependent; signature becomes fn router(state: &AppState) or the
// edit routes merge in lib.rs router_with):
```

Simplest: `router()` keeps static routes; `router_with` (lib.rs) layers the edit sub-router when `state.edit_enabled()`:

```rust
pub fn router_with(state: AppState) -> Router {
    let r = handlers::router().with_state(state.clone());
    if state.edit_enabled() {
        r.route("/api/edit/stop/{stop_version_id}", axum::routing::post(handlers::edit::edit_stop))
        // Task 5 appends: log + undo routes here
    } else { r }
}
```

`stats.rs` — after fetching `v`, insert flag (handler owns state):

```rust
    let mut v = run_db(state.clone(), crate::queries::qa::stats).await?;
    v["edit_enabled"] = json!(state.edit_enabled());
```

(`queries::qa::stats` returns `serde_json::Value` — mutate as object.)

`lib.rs` / `main.rs`:

```rust
// lib.rs
pub async fn serve(db_path: PathBuf, addr: SocketAddr, tiles: Option<String>,
                   edit_tokens: Option<HashMap<String, String>>) -> Result<()> { /* pass through to with_edit */ }

// main.rs args:
    /// Enable edit endpoints (comma list name:token; or RM_EDIT_TOKENS env)
    #[arg(long, requires = "edit_tokens_set")]
    edit: bool,
// parse "name:tok,name2:tok2" → HashMap; require --edit for tokens; call serve(..., tokens)
```

(Token parsing helper in main.rs: split on `,`, then split_once(':'); error on malformed entries.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rm-web`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/rm-web/src crates/rm-web/tests crates/rm-web/Cargo.toml
git commit -m "rm-web: edit mode — token auth, write path, POST /api/edit/stop"
```

---

### Task 5: History + undo endpoints

**Files:**
- Modify: `crates/rm-web/src/handlers/edit.rs` (+ two handlers)
- Modify: `crates/rm-web/src/lib.rs` (mount the two routes next to Task 4's)
- Test: `crates/rm-web/tests/edit.rs` (append)

**Interfaces:**
- Consumes: Task 3 `rm_db::{undo, entity_history}`; Task 4 `Editor`, `run_db_write`.
- Produces (Task 7 client consumes):
  - `GET /api/edit/log?entity=stop&entity_id=1` → `{"rows": [EditLogRow…]}` (unauthenticated read — audit is not secret; note field visible to team).
  - `POST /api/edit/undo/{edit_id}` body `{"note"?}` → `{"edit_id", "new_stop_version_id"}`.

- [ ] **Step 1: Write the failing tests** (append to `tests/edit.rs`)

```rust
#[tokio::test]
async fn log_and_undo_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    // edit stop 10 (name only — no stale marks needed for this test)
    let res = rm_web::router_with(state.clone()).oneshot(
        Request::builder().method("POST").uri("/api/edit/stop/10")
            .header("authorization", "Bearer sekrit")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"stop_name": "New", "note": "fix"}"#)).unwrap(),
    ).await.unwrap();
    let v = body_json(res).await;
    let edit_id = v["edit_id"].as_i64().unwrap();

    // log lists it
    let res = rm_web::router_with(state.clone()).oneshot(
        Request::builder().uri("/api/edit/log?entity=stop&entity_id=1").body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["rows"].as_array().unwrap().len(), 1);

    // undo restores, requires token
    let res = rm_web::router_with(state).oneshot(
        Request::builder().method("POST").uri(format!("/api/edit/undo/{edit_id}"))
            .header("authorization", "Bearer sekrit")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"note": "revert"}"#)).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["new_stop_version_id"].as_i64().unwrap(), 10);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rm-web log_and_undo`
Expected: FAIL — 404 (routes absent)

- [ ] **Step 3: Implement handlers + mount**

```rust
pub async fn log(
    axum::extract::Query(q): axum::extract::Query<LogParams>,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    #[derive(Deserialize)]
    struct LogParams { entity: String, entity_id: i64 }
    // (defined at module level, not inline)
    let rows = crate::state::run_db(state, move |conn| {
        rm_db::entity_history(conn, &q.entity, q.entity_id)
    }).await?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

pub async fn undo(
    Path(edit_id): Path<i64>,
    State(state): State<AppState>,
    Editor(editor): Editor,
    Json(b): Json<UndoBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    #[derive(Deserialize)]
    struct UndoBody { note: Option<String> }
    let out = run_db_write(state, move |conn| {
        rm_db::undo(conn, edit_id, &editor, b.note.as_deref().unwrap_or(""))
    }).await?;
    Ok(Json(serde_json::json!({ "edit_id": out.edit_id, "new_stop_version_id": out.new_version_id })))
}
```

Mount in `router_with` next to the stop route: `.route("/api/edit/log", get(edit::log)).route("/api/edit/undo/{edit_id}", post(edit::undo))`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p rm-web`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/rm-web/src/handlers/edit.rs crates/rm-web/src/lib.rs crates/rm-web/tests/edit.rs
git commit -m "rm-web: edit log + undo endpoints"
```

---

### Task 6: Read-side staleness badges

**Files:**
- Modify: `crates/rm-web/src/queries/stops.rs` (`detail` + `station_detail`)
- Test: `crates/rm-web/tests/edit.rs` (append)

**Interfaces:**
- Consumes: `stale_mark` rows written by Task 3.
- Produces: stop detail JSON gains `"stale": {"projections": [route_id…], "bearings": bool}`; station detail gains `"stale": {"bearings": bool}` (Task 7 renders the badge from these).

- [ ] **Step 1: Write the failing test** (append)

```rust
#[tokio::test]
async fn stop_detail_reports_staleness_after_position_edit() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    let res = rm_web::router_with(state.clone()).oneshot(
        Request::builder().method("POST").uri("/api/edit/stop/10")
            .header("authorization", "Bearer sekrit")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"lat": 24.001, "note": "move"}"#)).unwrap(),
    ).await.unwrap();
    let new_id = body_json(res).await["new_stop_version_id"].as_i64().unwrap();

    let res = rm_web::router_with(state.clone()).oneshot(
        Request::builder().uri(format!("/api/stops/{new_id}")).body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["stale"]["projections"], serde_json::json!([1]));
    assert_eq!(v["stale"]["bearings"], true);

    // simulated refresh clears the marks → stale flags gone
    let conn = rm_db::open(&dir.path().join("t.db")).unwrap();
    conn.execute("DELETE FROM stale_mark", []).unwrap();
    drop(conn);
    let res = rm_web::router_with(state).oneshot(
        Request::builder().uri(format!("/api/stops/{new_id}")).body(Body::empty()).unwrap(),
    ).await.unwrap();
    let v = body_json(res).await;
    assert_eq!(v["stale"]["projections"], serde_json::json!([]));
    assert_eq!(v["stale"]["bearings"], false);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rm-web stop_detail_reports`
Expected: FAIL — response has no `stale` key

- [ ] **Step 3: Implement**

In `queries/stops.rs::detail`, after assembling `found`, add:

```rust
    let stale_projections: Vec<i64> = {
        let mut stmt = conn.prepare(
            "SELECT CAST(key AS INTEGER) FROM stale_mark
             WHERE table_name = 'path_stop_projection' AND key IN (
                 SELECT DISTINCT CAST(rv.route_id AS TEXT)
                 FROM route_stop_version rsv
                 JOIN subroute_version sv ON sv.subroute_version_id = rsv.subroute_version_id
                 JOIN route_version rv ON rv.route_version_id = sv.route_version_id
                 WHERE rsv.stop_version_id = ?1)",
        )?;
        // note: route_stop_version now points at the NEW version id — pass it in
    };
    let bearings_stale: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM stale_mark WHERE table_name IN ('stop_bearing','station_bearing'))",
        [], |r| r.get(0))?;
```

Add `"stale": json!({"projections": stale_projections, "bearings": bearings_stale})` to the response object. `station_detail` adds the same `bearings_stale` flag as `"stale": {"bearings": …}`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p rm-web`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/rm-web/src/queries/stops.rs crates/rm-web/tests/edit.rs
git commit -m "rm-web: stale badges in stop/station detail responses"
```

---

### Task 7: Client — token toggle, position fix, rename, stale chip

**Files:**
- Modify: `crates/rm-web/assets/index.html` (edit toggle button in topbar)
- Modify: `crates/rm-web/assets/js/api.js` (edit helpers)
- Modify: `crates/rm-web/assets/js/panels.js` (edit buttons + stale chip + `setEditable`)
- Modify: `crates/rm-web/assets/js/app.js` (toggle flow, place-mode, rename handler, stats wiring)
- Modify: `crates/rm-web/assets/style.css` (edit-mode affordances)

**Interfaces:**
- Consumes: Task 4/5 endpoints; `stats.edit_enabled`; Task 6 `stale` fields.
- Produces: phase-1 editing UX. No JS test infra exists — verified manually (steps below).

**Spec deviation, necessary:** stop markers are `L.circleMarker` (not draggable in Leaflet). Position fix = "移動位置" button in the stop panel → click-to-place mode (next map click proposes the new position) → confirm dialog with distance → POST. Same intent as drag.

- [ ] **Step 1: api.js helpers** (append)

```javascript
// --- edit mode (phase 1) ---
export const editToken = () => sessionStorage.getItem('rm-edit-token') ?? '';

async function editFetch(path, body) {
  const res = await fetch(path, {
    method: 'POST',
    headers: { 'content-type': 'application/json', authorization: `Bearer ${editToken()}` },
    body: JSON.stringify(body),
  });
  if (!res.ok) {
    let msg = '';
    try { msg = (await res.json()).error ?? ''; } catch { /* not json */ }
    throw new Error(`HTTP ${res.status}: ${msg || res.statusText}`);
  }
  return res.json();
}

export const editStop = (svi, body) => editFetch(`/api/edit/stop/${svi}`, body);
export const undoEdit = (id, note) => editFetch(`/api/edit/undo/${id}`, { note });
export const editLog = (entity, id) => fetchJson(`/api/edit/log?entity=${entity}&entity_id=${id}`);
```

- [ ] **Step 2: panels.js — editable flag, buttons, stale chip**

Module state + setter near top:

```javascript
let editable = false;
export const setEditable = (b) => { editable = b; };
```

In `stopPanel` (signature gains `onEditStop`):

```javascript
export function stopPanel(d, { onPickRoute, onPickSegment, onPickStation, onEditStop }) {
```

Inside the kv table, after the stop row:

```javascript
      <tr><td>stop</td><td>${d.stop.stop_name} (${d.stop.stop_code})
        ${editable ? '<button class="mini-btn" data-act="rename" title="rename stop">✎</button>' : ''}
        ${editable ? '<button class="mini-btn" data-act="move" title="move stop position">移動位置</button>' : ''}</td></tr>
      ${d.stale && (d.stale.projections?.length || d.stale.bearings)
        ? `<tr><td>stale</td><td><span class="pp-hot">⚠ ${d.stale.projections?.length ?? 0} route projections · bearings ${d.stale.bearings ? 'stale' : 'ok'}</span></td></tr>`
        : ''}
```

Wire after `showPane` (next to the existing `.st-pick` wiring):

```javascript
  if (editable && onEditStop) {
    pane.querySelectorAll('.mini-btn').forEach((el) =>
      el.addEventListener('click', () => onEditStop(el.dataset.act, d.stop)));
  }
```

`stationPanel`: same stale chip pattern using `d.stale?.bearings`.

- [ ] **Step 3: app.js — toggle, place-mode, rename, stats wiring**

```javascript
let editMode = false;

async function enableEditMode() {
  if (!sessionStorage.getItem('rm-edit-token')) {
    const tok = prompt('edit token:');
    if (!tok) return;
    sessionStorage.setItem('rm-edit-token', tok);
  }
  editMode = true;
  setEditable(true);
  document.getElementById('edit-toggle').classList.add('on');
  status('edit mode on');
}

function disableEditMode() {
  editMode = false;
  setEditable(false);
  document.getElementById('edit-toggle').classList.remove('on');
  status('');
}

/// stop-panel edit actions: rename now, move = one-shot place mode.
function onEditStop(act, stop) {
  if (!editMode) return;
  if (act === 'rename') {
    const name = prompt(`rename stop #${stop.stop_code}:`, stop.stop_name);
    if (!name || name === stop.stop_name) return;
    const note = prompt('note (required):', '') ?? '';
    if (!note) return;
    api.editStop(stop.stop_version_id, { stop_name: name, note })
      .then((r) => { status('renamed'); return selectStop(r.new_stop_version_id); })
      .catch((e) => status(`rename failed: ${e.message}`));
  } else if (act === 'move') {
    status('click the map to place the stop');
    const handler = (e) => {
      map.off('click', handler);
      const dist = Math.round(L.latLng([stop.lat, stop.lon]).distanceTo(e.latlng));
      if (!confirm(`move stop #${stop.stop_code} ${dist} m?\n(${stop.lat.toFixed(5)},${stop.lon.toFixed(5)}) → (${e.latlng.lat.toFixed(5)},${e.latlng.lng.toFixed(5)})`)) return status('');
      const note = prompt('note (required):', '') ?? '';
      if (!note) return status('');
      api.editStop(stop.stop_version_id, { lat: e.latlng.lat, lon: e.latlng.lng, note })
        .then((r) => { status(`moved ${dist} m`); return selectStop(r.new_stop_version_id); })
        .catch((err) => status(`move failed: ${err.message}`));
    };
    map.on('click', handler);
  }
}
```

In `boot()`: after the stats fetch fills the chips, add:

```javascript
  if (d.edit_enabled) {
    const btn = document.getElementById('edit-toggle');
    btn.hidden = false;
    btn.addEventListener('click', () => (editMode ? disableEditMode() : enableEditMode()));
  }
```

Pass `onEditStop` into every `stopPanel(...)` call site (three: `openStopPopup`, `selectStop`, `popupStop`).

- [ ] **Step 4: index.html + style.css**

index.html topbar (after `chip-variants`):

```html
    <button id="edit-toggle" class="chip chip-click" hidden title="toggle edit mode">✎ edit</button>
```

style.css:

```css
/* edit mode */
#edit-toggle.on { background: var(--amber, #f59e0b); color: #000; }
.mini-btn { margin-left: 4px; padding: 0 6px; font-size: 12px; cursor: pointer; }
```

- [ ] **Step 5: Manual verification** (server must run against a copy of real data — edits write!)

```bash
cp data/taipei.db /tmp/edit-test.db
cargo run -r -p rm-web -- --db /tmp/edit-test.db --edit --edit-tokens "herry:dev-token-1"
```

Then in the browser (`http://127.0.0.1:8787`):
1. Topbar shows ✎ edit → click → enter `dev-token-1` → button turns amber.
2. Open a route → click a stop → point pane shows ✎ + 移動位置.
3. Rename: ✎ → new name + note → panel re-renders with the new name; DB check: `sqlite3 /tmp/edit-test.db 'SELECT stop_name, source FROM stop_version WHERE valid_to IS NULL AND source="manual" LIMIT 3'`.
4. Move: 移動位置 → click map elsewhere → confirm dialog shows distance → panel re-renders at the new spot; stop detail now shows the ⚠ stale row.
5. Token wrong → status bar shows `HTTP 403`.
6. Without `--edit`: no ✎ button (stats `edit_enabled: false`).

- [ ] **Step 6: Commit**

```bash
git add crates/rm-web/assets
git commit -m "rm-web: phase-1 edit UI — token toggle, click-to-place position fix, rename, stale chip"
```

---

## Self-review notes

- Spec coverage: edit primitive ✓ (T3), migration ✓ (T1), one-shot guard ✓ (T2), auth + `--edit` + absent-routes-404 ✓ (T4), endpoints ✓ (T4/T5), `edit_enabled` in stats ✓ (T4), stale marks + badge ✓ (T3/T6), UI toggle/rename/position/stale chip ✓ (T7), history endpoint ✓ (T5). Phase-1 spec items not in this plan: none. Drag-to-fix replaced by click-to-place (Leaflet circleMarker constraint) — flagged above.
- Undo endpoint included per spec §Server write path endpoint list; undo **UI** is phase 2 per spec phasing.
- `ensure_schema` (viewer probe) only counts `schema_migrations`/`seg_segment` — new migration doesn't affect read-only viewers; `run_db_write`'s `rm_db::open` applies 0010 on first write. Existing production DBs gain the migration lazily; safe because ALTER ADD COLUMN on populated tables is metadata-only.
