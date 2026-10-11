# Polyline Override 手動修線 (phase 2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the phase-1 piece model with a whole-line override: `effective = polyline_override.encoded ?? src.encoded` at every read site, plus per-direction `--stale` recompute wiring for all three downstream binaries.

**Architecture:** Unversioned `polyline_override` row per `(xno, direction)` with `updated_at` as the optimistic-concurrency token; undo stays a forward write through `edit_log`. rm-edit plans (zero SQL), rm-db applies (one tx + four stale marks), readers call one new `rm_db::effective_encoded`. UI edits the whole line in the raw-compare layer; no span picking, no anchors, no assembly.

**Tech Stack:** Rust workspace (rusqlite, axum, clap, rayon), vanilla-JS Leaflet frontend (`include_str!`-baked assets).

**Spec:** `docs/superpowers/specs/2026-10-11-polyline-override-design.md` (commit `44523f4`) — the binding authority; this plan argues from it.

## Global Constraints

- **Release builds for pipeline binaries** (`rm-osm-match`, `rm-segments`, `rm-registry`, `rm-web`): always `cargo run -r -p <crate>` — debug builds are 10–30× slower on this workload (CLAUDE.md incident 2026-10-06).
- **House crate split:** `rm-edit` zero SQL (plan + orchestrate), `rm-db` owns every SQL statement and transaction, `rm-core` owns pure math. No SQL in rm-edit, no domain geometry in rm-db.
- **`effective = override ?? src`, unconditionally.** `effective_encoded` returns `""` when neither exists; readers treat `""` as no_path (existing convention).
- **edit_log is append-only** and is the audit trail — `polyline_override` carries no `updated_by` column. Undo rows carry `undo_of`; `action='undo'` rows are not undoable.
- **`changes` JSON shape (spec-pinned):** `{"kind":"polyline_override","xno":…,"direction":…,"encoded":{"old"?:…,"new":…}}` — `old` absent on create; undo rows carry the restored state as `new`.
- **Stale-mark conventions:** `(table_name, key, cause_edit_id, marked_at)`, PK `(table_name, key)`, re-mark = `INSERT OR REPLACE` (fresh rowid, freshest cause wins). Phase 2 marks all four in the apply tx: `('seg_match','{route_id}:{direction}')`, `('path_stop_projection','{route_id}')`, `('stop_bearing','*')`, `('station_bearing','*')`.
- **Undo = forward write, one tx, currency guard before any write** (`updated_at` must equal the undone edit's ts, else 409).
- **`now_rfc3339()` is second-granularity** — two writes in the same second share a token; known house design limit (phase-1 tests diverge rows manually to test superseded-refusal).
- **Single-editor tool:** the token → 409 is the whole concurrency story; no extra locking.
- **Validation = decode round-trip + ≥2 points. No anchors, no seam/overlap checks anywhere.**
- **Repo base branch is `master`** (not `main`).
- **Assets are `include_str!`-baked:** any `assets/js` change needs `cargo build -r -p rm-web` + server restart to take effect.
- **Migration `0013` is authored inside this branch and never shipped early:** Task 1 creates it WITHOUT the `DROP TABLE polyline_piece;` line (phase-1 code still runs); Task 8 adds that line when the last consumer dies. Do not reorder.
- **`rm_db::EditError` and `rm_edit::EditError` are two distinct enums** (rm-db/src/edit.rs:9, rm-edit/src/rename.rs:52). New variants go into BOTH; `map_db_err` (rm-edit/src/lib.rs) and `edit_conflict` (rm-web/src/handlers/edit.rs:77) both need matching arms.

## Review Focus

Five inputs the spec implies but no task's tests fully exercise, most likely first. Each line's pinning test is noted on the owning task.

1. **Garbage `encoded` in an existing override row** (hand-edited DB, partial write): `effective_encoded` must pass it through verbatim — readers' existing decode-failure paths (warn + no_path) absorb it; nothing may panic or silently fall back to src. → T2 test `effective_encoded_prefers_override_then_src` asserts pass-through.
2. **Exactly-2-point override** (legal minimum): match/segments must treat it as a real line, not degenerate. → T3 `line_create_end_to_end` saves a 2-point line; T5's match test matches a 2+-point override.
3. **`expected_updated_at` sent when no override row exists** (override undone away between load and save): clean 409 `LineNotCurrent`, never a 500 or panic. → T2 test `line_edit_stale_expectation_refused` (second half) + T3 `line_expectation_without_override_refused`.
4. **`seg_match` mark whose direction has no staging row in the latest batch** (re-ingest dropped it): stale run keeps the mark and warns — never deletes rows it did not recompute. → T6 test `stale_run_keeps_unknown_and_unmatched_marks`.
5. **Phase-1 `polyline_piece` log rows after retirement** (history stays, undo must refuse): `plan_line_undo` rejects foreign kinds with `NotUndoable`, not a 500. → T3 test `plan_line_undo_rejects_legacy_piece_rows`.

---

### Task 1: Migration `0013_polyline_override` (table + unique index, no DROP yet)

**Files:**
- Create: `crates/rm-db/migrations/0013_polyline_override.sql`
- Modify: `crates/rm-db/src/migrate.rs` (append to `MIGRATIONS`, add test)

**Interfaces:**
- Consumes: nothing (leaf task).
- Produces: table `polyline_override (xno TEXT NOT NULL, direction INTEGER NOT NULL, encoded TEXT NOT NULL, updated_at TEXT NOT NULL)` + `UNIQUE INDEX polyline_override_dir ON polyline_override (xno, direction)`. Every later task relies on the `(xno, direction)` conflict target for upserts.

- [ ] **Step 1: Write the migration file**

`crates/rm-db/migrations/0013_polyline_override.sql`:

```sql
-- Whole-line polyline override (phase 2). One current override per
-- (xno, direction); unversioned — the row IS the current state and
-- `updated_at` doubles as the optimistic-concurrency token. No surrogate
-- PK: (xno, direction) is the identity, so no id_sequence involvement.
-- No `updated_by`: edit_log is the audit trail (house rule).
CREATE TABLE polyline_override (
  xno         TEXT NOT NULL,
  direction   INTEGER NOT NULL,
  encoded     TEXT NOT NULL,   -- full replacement geometry, ≥2 points
  updated_at  TEXT NOT NULL    -- = producing edit's ts (concurrency token)
);
-- exactly one current override per direction
CREATE UNIQUE INDEX polyline_override_dir ON polyline_override (xno, direction);
```

NOTE: deliberately NO `DROP TABLE polyline_piece;` yet — phase-1 code still reads that table until Task 8. Task 8 edits this same file (branch-internal, never shipped early).

- [ ] **Step 2: Register the migration**

In `crates/rm-db/src/migrate.rs`, append to the `MIGRATIONS` list (after `("0012_polyline_piece", …)`):

```rust
    (
        "0013_polyline_override",
        include_str!("../migrations/0013_polyline_override.sql"),
    ),
```

- [ ] **Step 3: Write the failing test**

In `crates/rm-db/src/migrate.rs` `mod tests`, add (self-contained; adjust nothing else):

```rust
#[test]
fn migration_0013_creates_override_table_and_unique_index() {
    let conn = open_in_memory().unwrap();
    let cols: Vec<String> = {
        let mut stmt = conn
            .prepare("PRAGMA table_info(polyline_override)")
            .unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
        rows.collect()
    };
    assert_eq!(
        cols,
        vec!["xno".into(), "direction".into(), "encoded".into(), "updated_at".into()]
    );
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'index' AND name = 'polyline_override_dir'",
        [],
        |r| r.get(0),
    )
    .unwrap();
    assert_eq!(n, 1, "unique index on (xno, direction)");
}
```

(If the existing tests mod uses a different in-memory helper name or `?` is not in scope in that mod, mirror the neighboring migration tests' style — the assertions above are the contract.)

- [ ] **Step 4: Run the tests**

Run: `cargo test -p rm-db migrate`
Expected: PASS (new test + all existing migration tests).

- [ ] **Step 5: Commit**

```bash
git add crates/rm-db/migrations/0013_polyline_override.sql crates/rm-db/src/migrate.rs
git commit -m "rm-db: migration 0013 — polyline_override table + (xno,direction) unique index

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 2: rm-db — line storage, stale marks, `effective_encoded`

**Files:**
- Modify: `crates/rm-db/src/edit.rs` (EditError variants; new "whole-line override" region after the piece region, before `StationRenameUndoOp`; tests in `mod tests`)
- Test: inline `#[cfg(test)] mod tests` in the same file

**Interfaces:**
- Consumes: Task 1's `polyline_override` table; existing `stale_mark`, `edit_log`, `route`, `src_route_point` tables; `crate::now_rfc3339()`; existing `PolylineEditRow` + `load_polyline_edit` (kept as-is — they are entity-generic).
- Produces (all `pub`, in `rm_db`):
  - `struct OverrideRow { encoded: String, updated_at: String }`
  - `struct LineSnapshot { route_id: i64, src_encoded: Option<String>, override_row: Option<OverrideRow>, route_version_id: Option<i64> }`
  - `fn load_polyline_line(conn: &Connection, xno: &str, direction: i64) -> Result<Option<LineSnapshot>>`
  - `struct LineEditOp { xno: String, direction: i64, encoded: String, expected_updated_at: Option<String> }`
  - `struct LineApplied { edit_id: i64, updated_at: String, created: bool }`
  - `fn apply_line_edit(conn: &Connection, op: &LineEditOp, editor: &str, note: &str) -> Result<LineApplied>`
  - `struct LineUndoOp { edit_id: i64, route_id: i64, xno: String, direction: i64, undone_ts: String, restored: Option<String> }`
  - `struct LineUndone { edit_id: i64, deleted: bool }`
  - `fn apply_line_undo(conn: &Connection, op: &LineUndoOp, editor: &str, note: &str) -> Result<LineUndone>`
  - `fn effective_encoded(conn: &Connection, batch_id: i64, xno: &str, direction: i64) -> Result<String>`
  - New `EditError` variants: `InvalidLine(String)` (display `invalid polyline: {0}`), `LineNotCurrent(String, i64)` (display `line {0}/{1} was edited by someone else — reload and retry`)

- [ ] **Step 1: Write the failing tests**

In `crates/rm-db/src/edit.rs` `mod tests`, add after the existing `src_fixture` helper (reuse it — do not duplicate):

```rust
fn line_op() -> LineEditOp {
    LineEditOp {
        xno: "1".into(),
        direction: 1,
        encoded: "_ulLnnqC_p~iF~ps|U".into(),
        expected_updated_at: None,
    }
}

fn line_undo_op(edit_id: i64, undone_ts: String, restored: Option<String>) -> LineUndoOp {
    LineUndoOp {
        edit_id,
        route_id: 1,
        xno: "1".into(),
        direction: 1,
        undone_ts,
        restored,
    }
}

#[test]
fn line_edit_creates_updates_and_marks_four_stale() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    src_fixture(&conn);
    let a = apply_line_edit(&conn, &line_op(), "h", "n1").unwrap();
    assert!(a.created);
    let (enc, ts): (String, String) = conn
        .query_row(
            "SELECT encoded, updated_at FROM polyline_override WHERE xno = '1' AND direction = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(enc, "_ulLnnqC_p~iF~ps|U");
    assert_eq!(ts, a.updated_at);
    // log row: entity polyline, entity_id = route_id 1, action create,
    // changes carry new only
    let (ent, eid, act, changes): (String, i64, String, String) = conn
        .query_row(
            "SELECT entity, entity_id, action, changes FROM edit_log",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!((ent.as_str(), eid, act.as_str()), ("polyline", 1, "create"));
    let c: serde_json::Value = serde_json::from_str(&changes).unwrap();
    assert_eq!(c["kind"], "polyline_override");
    assert_eq!(c["encoded"]["new"], "_ulLnnqC_p~iF~ps|U");
    assert!(c["encoded"].get("old").is_none());
    // four stale marks, seg_match keyed by route_id:direction
    let mut marks: Vec<(String, String)> = Vec::new();
    let mut stmt = conn.prepare("SELECT table_name, key FROM stale_mark").unwrap();
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    for r in rows {
        marks.push(r.unwrap());
    }
    assert!(marks.contains(&("seg_match".into(), "1:1".into())), "{marks:?}");
    assert!(marks.contains(&("path_stop_projection".into(), "1".into())));
    assert!(marks.contains(&("stop_bearing".into(), "*".into())));
    assert!(marks.contains(&("station_bearing".into(), "*".into())));
    // update: currency token from create; upsert, not a second row
    let mut op = line_op();
    op.expected_updated_at = Some(a.updated_at.clone());
    op.encoded = "_p~iF~ps|U_ulLnnqC".into();
    let b = apply_line_edit(&conn, &op, "h", "n2").unwrap();
    assert!(!b.created);
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM polyline_override", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
    let act: String = conn
        .query_row("SELECT action FROM edit_log ORDER BY edit_id DESC LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(act, "update");
}

#[test]
fn line_edit_stale_expectation_refused() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    src_fixture(&conn);
    let a = apply_line_edit(&conn, &line_op(), "h", "n1").unwrap();
    let mut op = line_op();
    op.expected_updated_at = Some(a.updated_at.clone());
    op.encoded = "different".into();
    let err = apply_line_edit(&conn, &op, "h", "").unwrap_err();
    assert!(err.to_string().contains("edited by someone else"), "{err}");
    // expectation against a NONEXISTENT override also refuses (it was
    // undone away between load and save) — clean 409, never a panic
    let conn2 = open_in_memory().unwrap();
    fixture(&conn2);
    src_fixture(&conn2);
    let mut op = line_op();
    op.expected_updated_at = Some("2020-01-01T00:00:00Z".into());
    let err = apply_line_edit(&conn2, &op, "h", "").unwrap_err();
    assert!(err.to_string().contains("edited by someone else"), "{err}");
}

#[test]
fn line_undo_of_create_deletes_row_and_marks_stale() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    src_fixture(&conn);
    let a = apply_line_edit(&conn, &line_op(), "h", "n1").unwrap();
    let out = apply_line_undo(&conn, &line_undo_op(a.edit_id, a.updated_at, None), "h", "rev").unwrap();
    assert!(out.deleted);
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM polyline_override", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0, "back to pure src");
    let (act, undo_of, changes): (String, i64, String) = conn
        .query_row(
            "SELECT action, undo_of, changes FROM edit_log ORDER BY edit_id DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(act, "undo");
    assert_eq!(undo_of, a.edit_id);
    let c: serde_json::Value = serde_json::from_str(&changes).unwrap();
    assert_eq!(c["encoded"]["new"], serde_json::Value::Null);
    // same four marks on undo
    let marks: i64 = conn
        .query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0))
        .unwrap();
    assert_eq!(marks, 4);
}

#[test]
fn line_undo_restores_prior_encoded() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    src_fixture(&conn);
    let a = apply_line_edit(&conn, &line_op(), "h", "n1").unwrap();
    let mut op = line_op();
    op.expected_updated_at = Some(a.updated_at.clone());
    let b = apply_line_edit(&conn, &op, "h", "n2").unwrap();
    let out = apply_line_undo(
        &conn,
        &line_undo_op(b.edit_id, b.updated_at, Some("_ulLnnqC_p~iF~ps|U".into())),
        "h",
        "rev",
    )
    .unwrap();
    assert!(!out.deleted);
    let enc: String = conn
        .query_row(
            "SELECT encoded FROM polyline_override WHERE xno = '1' AND direction = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(enc, "_ulLnnqC_p~iF~ps|U");
}

#[test]
fn line_undo_superseded_refused() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    src_fixture(&conn);
    let a = apply_line_edit(&conn, &line_op(), "h", "n1").unwrap();
    // line moved on after edit a (now_rfc3339 is second-granular, so
    // diverge the row directly to make this deterministic)
    let mut op = line_op();
    op.expected_updated_at = Some(a.updated_at.clone());
    apply_line_edit(&conn, &op, "h", "n2").unwrap();
    let err = apply_line_undo(&conn, &line_undo_op(a.edit_id, a.updated_at, None), "h", "rev").unwrap_err();
    assert!(err.to_string().contains("edited by someone else"), "{err}");
}

#[test]
fn load_polyline_line_resolves_route_src_and_override() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    src_fixture(&conn);
    let snap = load_polyline_line(&conn, "1", 1).unwrap().unwrap();
    assert_eq!(snap.route_id, 1);
    assert_eq!(snap.src_encoded.as_deref(), Some("_p~iF~ps|U_ulLnnqC"));
    assert!(snap.override_row.is_none());
    assert_eq!(snap.route_version_id, Some(1));
    assert!(load_polyline_line(&conn, "999", 1).unwrap().is_none());
    apply_line_edit(&conn, &line_op(), "h", "n").unwrap();
    let snap = load_polyline_line(&conn, "1", 1).unwrap().unwrap();
    assert_eq!(snap.override_row.unwrap().encoded, "_ulLnnqC_p~iF~ps|U");
}

#[test]
fn effective_encoded_prefers_override_then_src() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    src_fixture(&conn);
    // src branch
    assert_eq!(effective_encoded(&conn, 1, "1", 1).unwrap(), "_p~iF~ps|U_ulLnnqC");
    // neither branch: "" (readers treat "" as no_path)
    assert_eq!(effective_encoded(&conn, 1, "1", 2).unwrap(), "");
    // override branch
    apply_line_edit(&conn, &line_op(), "h", "n").unwrap();
    assert_eq!(effective_encoded(&conn, 1, "1", 1).unwrap(), "_ulLnnqC_p~iF~ps|U");
    // REVIEW-FOCUS pin: garbage encoded passes through VERBATIM — readers
    // own decode failure; effective_encoded never falls back to src
    let a = apply_line_edit(&conn, &line_op(), "h", "n").unwrap();
    let mut op2 = line_op();
    op2.expected_updated_at = Some(a.updated_at.clone());
    op2.encoded = "not-a-polyline".into();
    apply_line_edit(&conn, &op2, "h", "n").unwrap();
    assert_eq!(effective_encoded(&conn, 1, "1", 1).unwrap(), "not-a-polyline");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rm-db edit::tests::line 2>&1 | tail -20`
Expected: COMPILE FAIL — `LineEditOp` / `apply_line_edit` / `LineUndoOp` / `apply_line_undo` / `load_polyline_line` / `effective_encoded` not found.

- [ ] **Step 3: Add the EditError variants**

In `crates/rm-db/src/edit.rs`, inside `pub enum EditError` (after `PieceScope`), add:

```rust
    #[error("invalid polyline: {0}")]
    InvalidLine(String),
    #[error("line {0}/{1} was edited by someone else — reload and retry")]
    LineNotCurrent(String, i64),
```

- [ ] **Step 4: Write the line region**

In `crates/rm-db/src/edit.rs`, insert this region AFTER `apply_piece_undo` (i.e. between it and the `StationRenameUndoOp` section). `PolylineEditRow` / `load_polyline_edit` stay where they are, untouched — the line undo reuses them.

```rust
// ---------- whole-line override (phase 2) ----------

/// One current polyline_override row, as stored (unversioned — the row IS
/// the current state; `updated_at` doubles as the concurrency token).
#[derive(Debug)]
pub struct OverrideRow {
    pub encoded: String,
    pub updated_at: String,
}

/// Read-side snapshot for whole-line polyline edits: the route id
/// (edit_log's entity_id), the latest-batch src polyline (`None` when the
/// capture has no polyline for this direction), the current override row
/// (`None` = readers fall through to src), and the route's current version.
#[derive(Debug)]
pub struct LineSnapshot {
    pub route_id: i64,
    pub src_encoded: Option<String>,
    pub override_row: Option<OverrideRow>,
    pub route_version_id: Option<i64>,
}

/// `xno` is stable route identity (`route.ext_uid = 'tcg:' || xno`).
/// `None` when no route carries that ext_uid.
pub fn load_polyline_line(
    conn: &Connection,
    xno: &str,
    direction: i64,
) -> Result<Option<LineSnapshot>> {
    let (route_id, route_version_id): (i64, Option<i64>) = match conn.query_row(
        "SELECT route_id, current_route_version_id FROM route WHERE ext_uid = ?1",
        [format!("tcg:{xno}")],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ) {
        Ok(v) => v,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let batch_id: i64 = conn.query_row(
        "SELECT COALESCE(MAX(batch_id), 0) FROM import_batch",
        [],
        |r| r.get(0),
    )?;
    let src_encoded: Option<String> = conn
        .query_row(
            "SELECT encoded FROM src_route_point
             WHERE batch_id = ?1 AND xno = ?2 AND direction = ?3",
            params![batch_id, xno, direction],
            |r| r.get(0),
        )
        .optional()?;
    let override_row = conn
        .query_row(
            "SELECT encoded, updated_at FROM polyline_override
             WHERE xno = ?1 AND direction = ?2",
            params![xno, direction],
            |r| {
                Ok(OverrideRow {
                    encoded: r.get(0)?,
                    updated_at: r.get(1)?,
                })
            },
        )
        .optional()?;
    Ok(Some(LineSnapshot {
        route_id,
        src_encoded,
        override_row,
        route_version_id,
    }))
}

/// The COALESCE every consumer reads: the manual override when one exists,
/// else the latest-batch src polyline, else `""` (readers treat `""` as
/// no_path — same convention as a missing src row). Passes the override
/// through verbatim; decoding is the consumer's business.
pub fn effective_encoded(
    conn: &Connection,
    batch_id: i64,
    xno: &str,
    direction: i64,
) -> Result<String> {
    let enc: String = conn.query_row(
        "SELECT COALESCE(
            (SELECT encoded FROM polyline_override WHERE xno = ?1 AND direction = ?2),
            (SELECT encoded FROM src_route_point
             WHERE batch_id = ?3 AND xno = ?1 AND direction = ?2),
            '')",
        params![xno, direction, batch_id],
        |r| r.get(0),
    )?;
    Ok(enc)
}

/// Write-side op for a whole-line override edit (rm-edit maps a
/// LineEditPlan into this; rm-db knows nothing about domain geometry).
#[derive(Debug)]
pub struct LineEditOp {
    pub xno: String,
    pub direction: i64,
    pub encoded: String,
    pub expected_updated_at: Option<String>,
}

#[derive(Debug)]
pub struct LineApplied {
    pub edit_id: i64,
    pub updated_at: String,
    pub created: bool,
}

/// Mark every derived dataset that depends on the direction's polyline
/// stale. Keys use route_id (stable across route versions); the match
/// stale run resolves the current route_version at recompute time.
/// Re-marks replace: fresh rowid, freshest `cause_edit_id` wins.
fn mark_line_stale(
    tx: &Connection,
    route_id: i64,
    direction: i64,
    edit_id: i64,
    now: &str,
) -> Result<()> {
    for (table, key) in [
        ("seg_match", format!("{route_id}:{direction}")),
        ("path_stop_projection", route_id.to_string()),
        ("stop_bearing", "*".to_string()),
        ("station_bearing", "*".to_string()),
    ] {
        tx.execute(
            "INSERT OR REPLACE INTO stale_mark (table_name, key, cause_edit_id, marked_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![table, key, edit_id, now],
        )?;
    }
    Ok(())
}

/// Apply a whole-line override edit in ONE transaction. Unversioned: the
/// row IS current; `updated_at` doubles as the optimistic-concurrency
/// token and the producing edit's ts. Currency guard runs before ANY
/// write; the same tx marks all four derived datasets stale.
pub fn apply_line_edit(
    conn: &Connection,
    op: &LineEditOp,
    editor: &str,
    note: &str,
) -> Result<LineApplied> {
    let tx = conn.unchecked_transaction()?;
    let now = crate::now_rfc3339();

    let route_id: i64 = match tx.query_row(
        "SELECT route_id FROM route WHERE ext_uid = ?1",
        [format!("tcg:{}", op.xno)],
        |r| r.get(0),
    ) {
        Ok(id) => id,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Err(EditError::UnknownRoute(op.xno.clone()).into())
        }
        Err(e) => return Err(e.into()),
    };

    // Currency: guard before any write.
    let mut created = true;
    let mut old: Option<String> = None;
    if let Some(row) = tx
        .query_row(
            "SELECT updated_at, encoded FROM polyline_override
             WHERE xno = ?1 AND direction = ?2",
            params![op.xno, op.direction],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?
    {
        created = false;
        let (updated_at, encoded) = row;
        if Some(&updated_at) != op.expected_updated_at.as_ref() {
            return Err(EditError::LineNotCurrent(op.xno.clone(), op.direction).into());
        }
        old = Some(encoded);
    } else if op.expected_updated_at.is_some() {
        // expectation against a nonexistent override: it moved (undone away)
        return Err(EditError::LineNotCurrent(op.xno.clone(), op.direction).into());
    }

    tx.execute(
        "INSERT INTO polyline_override (xno, direction, encoded, updated_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(xno, direction) DO UPDATE SET encoded = ?3, updated_at = ?4",
        params![op.xno, op.direction, op.encoded, now],
    )?;

    // changes carries old+new so undo is a forward write (`old` absent on create).
    let mut encoded_change = json!({ "new": op.encoded });
    if let Some(enc) = &old {
        encoded_change["old"] = json!(enc);
    }
    let changes = json!({
        "kind": "polyline_override",
        "xno": op.xno,
        "direction": op.direction,
        "encoded": encoded_change,
    })
    .to_string();
    tx.execute(
        "INSERT INTO edit_log (ts, editor, entity, entity_id, action, changes, note)
         VALUES (?1, ?2, 'polyline', ?3, ?4, ?5, ?6)",
        params![
            now,
            editor,
            route_id,
            if created { "create" } else { "update" },
            changes,
            if note.is_empty() { None } else { Some(note) }
        ],
    )?;
    let edit_id = tx.last_insert_rowid();

    mark_line_stale(&tx, route_id, op.direction, edit_id, &now)?;

    tx.commit()?;
    Ok(LineApplied {
        edit_id,
        updated_at: now,
        created,
    })
}

/// Write-side op for the line undo: restore-as-forward-write (or delete).
/// `undone_ts` is the undone edit's ts — the currency token the apply tx
/// re-validates against the row.
#[derive(Debug)]
pub struct LineUndoOp {
    pub edit_id: i64,
    pub route_id: i64,
    pub xno: String,
    pub direction: i64,
    pub undone_ts: String,
    /// Encoded state to restore; `None` = the undone edit was a create →
    /// delete the override row (back to pure src).
    pub restored: Option<String>,
}

#[derive(Debug)]
pub struct LineUndone {
    pub edit_id: i64,
    pub deleted: bool,
}

/// Undo a whole-line override edit in ONE transaction. Currency guard (the
/// row's `updated_at` must still equal the undone edit's ts) runs before
/// any write, so a superseded undo is a clean 409 with zero partial state.
/// Same stale marks as apply — undoing a line re-derives everything too.
pub fn apply_line_undo(
    conn: &Connection,
    op: &LineUndoOp,
    editor: &str,
    note: &str,
) -> Result<LineUndone> {
    let tx = conn.unchecked_transaction()?;
    let now = crate::now_rfc3339();

    let (updated_at, cur_encoded) = match tx
        .query_row(
            "SELECT updated_at, encoded FROM polyline_override
             WHERE xno = ?1 AND direction = ?2",
            params![op.xno, op.direction],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?
    {
        Some(v) => v,
        None => return Err(EditError::LineNotCurrent(op.xno.clone(), op.direction).into()),
    };
    if updated_at != op.undone_ts {
        return Err(EditError::LineNotCurrent(op.xno.clone(), op.direction).into());
    }

    let (deleted, new_encoded) = match &op.restored {
        Some(enc) => {
            tx.execute(
                "INSERT INTO polyline_override (xno, direction, encoded, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(xno, direction) DO UPDATE SET encoded = ?3, updated_at = ?4",
                params![op.xno, op.direction, enc, now],
            )?;
            (false, json!(enc))
        }
        None => {
            tx.execute(
                "DELETE FROM polyline_override WHERE xno = ?1 AND direction = ?2",
                params![op.xno, op.direction],
            )?;
            (true, serde_json::Value::Null)
        }
    };

    let changes = json!({
        "kind": "polyline_override",
        "xno": op.xno,
        "direction": op.direction,
        "encoded": { "old": cur_encoded, "new": new_encoded },
    })
    .to_string();
    tx.execute(
        "INSERT INTO edit_log (ts, editor, entity, entity_id, action, changes, note, undo_of)
         VALUES (?1, ?2, 'polyline', ?3, 'undo', ?4, ?5, ?6)",
        params![
            now,
            editor,
            op.route_id,
            changes,
            if note.is_empty() { None } else { Some(note) },
            op.edit_id
        ],
    )?;
    let edit_id = tx.last_insert_rowid();

    mark_line_stale(&tx, op.route_id, op.direction, edit_id, &now)?;

    tx.commit()?;
    Ok(LineUndone { edit_id, deleted })
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p rm-db edit`
Expected: PASS — all new line tests green, all existing tests (piece included) still green.

- [ ] **Step 6: Commit**

```bash
git add crates/rm-db/src/edit.rs
git commit -m "rm-db: whole-line override storage — load/apply/undo + effective_encoded + four stale marks

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 3: rm-edit — `line.rs` planning layer (alongside piece.rs)

**Files:**
- Create: `crates/rm-edit/src/line.rs`
- Create: `crates/rm-edit/tests/line.rs`
- Modify: `crates/rm-edit/src/lib.rs` (`mod line; pub use line::*;`, `map_db_err` arms)
- Modify: `crates/rm-edit/src/rename.rs` (EditError variants)

NOTE: `piece.rs` and `tests/piece.rs` are NOT touched here — rm-web still calls them until Task 4. Both live until Task 8.

**Interfaces:**
- Consumes: Task 2's `rm_db::{LineSnapshot, LineEditOp, LineApplied, LineUndoOp, LineUndone, load_polyline_line, apply_line_edit, apply_line_undo, PolylineEditRow, InvalidLine, LineNotCurrent}`; `rm_core::polyline::{encode, decode}`; `crate::map_db_err`.
- Produces (all `pub`, in `rm_edit`):
  - `struct LineEditCmd { xno: String, direction: i64, points: Vec<[f64; 2]>, expected_updated_at: Option<String> }`
  - `struct LineEditPlan { encoded: String, expected_updated_at: Option<String> }`
  - `fn plan_line_edit(snap: &rm_db::LineSnapshot, cmd: LineEditCmd) -> Result<LineEditPlan, EditError>`
  - `struct LineOutcome { edit_id: i64, updated_at: String, created: bool }`
  - `fn edit_polyline_line(conn: &Connection, cmd: LineEditCmd, editor: &str, note: &str) -> Result<LineOutcome>`
  - `struct LineUndoPlan { edit_id: i64, route_id: i64, xno: String, direction: i64, undone_ts: String, restored: Option<String> }`
  - `fn plan_line_undo(edit: &rm_db::PolylineEditRow) -> Result<LineUndoPlan, EditError>`
  - `struct LineUndoOutcome { edit_id: i64, deleted: bool }`
  - `fn undo_polyline_line(conn: &Connection, edit_id: i64, editor: &str, note: &str) -> Result<LineUndoOutcome>`
  - New `EditError` variants (rm-edit's enum in rename.rs): `InvalidLine(String)`, `LineNotCurrent(String, i64)`

- [ ] **Step 1: Write the failing tests**

`crates/rm-edit/tests/line.rs`:

```rust
//! Manual whole-line polyline override: load → plan → apply roundtrips
//! through the service, plus every plan guard.

use rm_db::open_in_memory;
use rm_edit::{edit_polyline_line, EditError, LineEditCmd};

/// route 1 (ext_uid 'tcg:1') + batch 1 + a 2-pt src polyline.
fn fixture(conn: &rusqlite::Connection) {
    conn.execute_batch(
        "INSERT INTO route (route_id, route_code, ext_uid, created_at, updated_at)
             VALUES (1, '1', 'tcg:1', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
         INSERT INTO import_batch (batch_id, source_dir, file_count, captured_at, valid_from, status, created_at)
             VALUES (1, 'assets/x', 1, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'CLOSED', '2026-01-01T00:00:00Z');
         INSERT INTO src_route_point (batch_id, xno, direction, encoded)
             VALUES (1, '1', 1, '_p~iF~ps|U_ulLnnqC');",
    )
    .unwrap();
}

fn cmd() -> LineEditCmd {
    LineEditCmd {
        xno: "1".into(),
        direction: 1,
        // exactly two points — the legal minimum
        points: vec![[38.5, -120.2], [40.7, -120.95]],
        expected_updated_at: None,
    }
}

#[test]
fn line_create_end_to_end() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let out = edit_polyline_line(&conn, cmd(), "herry", "fix").unwrap();
    assert!(out.created);
    let encoded: String = conn
        .query_row(
            "SELECT encoded FROM polyline_override WHERE xno = '1' AND direction = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(encoded, "_p~iF~ps|U_ulLnnqC");
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 4, "all four derived datasets marked stale");
}

#[test]
fn line_rejects_single_point_and_unknown_route() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let mut c = cmd();
    c.points = vec![[38.5, -120.2]];
    let err = edit_polyline_line(&conn, c, "h", "").unwrap_err();
    assert!(err.to_string().contains("≥2 points"), "{err}");
    let mut c = cmd();
    c.xno = "999".into();
    let err = edit_polyline_line(&conn, c, "h", "").unwrap_err();
    assert!(err.to_string().contains("does not exist"), "{err}");
}

#[test]
fn line_stale_expectation_refused() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let first = edit_polyline_line(&conn, cmd(), "herry", "a").unwrap();
    let mut second = cmd();
    second.expected_updated_at = Some("2020-01-01T00:00:00Z".into());
    let err = edit_polyline_line(&conn, second, "herry", "b").unwrap_err();
    assert!(err.to_string().contains("edited by someone else"), "{err}");
    let _ = first; // token proven stale above
}

#[test]
fn line_expectation_without_override_refused() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    // client carries a token but the override was undone away in between
    let mut c = cmd();
    c.expected_updated_at = Some("2026-01-01T00:00:00Z".into());
    let err = edit_polyline_line(&conn, c, "h", "").unwrap_err();
    assert!(err.to_string().contains("edited by someone else"), "{err}");
}

#[test]
fn line_identical_resave_is_noop() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let first = edit_polyline_line(&conn, cmd(), "herry", "a").unwrap();
    let mut again = cmd();
    again.expected_updated_at = Some(first.updated_at.clone());
    let err = edit_polyline_line(&conn, again, "herry", "b").unwrap_err();
    assert!(err.to_string().contains("sets no field"), "{err}");
}

#[test]
fn line_create_ok_without_src_polyline() {
    let conn = open_in_memory().unwrap();
    // route exists but no src_route_point row (capture missing this direction)
    conn.execute_batch(
        "INSERT INTO route (route_id, route_code, ext_uid, created_at, updated_at)
             VALUES (1, '1', 'tcg:1', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');",
    )
    .unwrap();
    let out = edit_polyline_line(&conn, cmd(), "herry", "draw").unwrap();
    assert!(out.created);
}

// ---------- undo planning ----------

use rm_edit::{plan_line_undo, undo_polyline_line};

fn line_row(action: &str, changes: serde_json::Value) -> rm_db::PolylineEditRow {
    rm_db::PolylineEditRow {
        edit_id: 7,
        entity_id: 1,
        action: action.into(),
        ts: "2026-01-02T00:00:00Z".into(),
        changes,
    }
}

fn create_changes() -> serde_json::Value {
    serde_json::json!({
        "kind": "polyline_override", "xno": "1", "direction": 1,
        "encoded": { "new": "_p~iF~ps|U_ulLnnqC" }
    })
}

#[test]
fn plan_parses_create_into_delete() {
    let plan = plan_line_undo(&line_row("create", create_changes())).unwrap();
    assert_eq!(plan.restored.is_none(), true, "create undo deletes");
    assert_eq!((plan.xno.as_str(), plan.direction, plan.route_id), ("1", 1, 1));
    assert_eq!(plan.undone_ts, "2026-01-02T00:00:00Z");
}

#[test]
fn plan_parses_update_into_restore() {
    let mut c = create_changes();
    c["encoded"] = serde_json::json!({
        "old": "_ulLnnqC_p~iF~ps|U", "new": "_p~iF~ps|U_ulLnnqC"
    });
    let plan = plan_line_undo(&line_row("update", c)).unwrap();
    assert_eq!(plan.restored.as_deref(), Some("_ulLnnqC_p~iF~ps|U"));
}

#[test]
fn plan_rejects_undo_rows_foreign_kinds_and_broken_payloads() {
    // undo-of-undo
    let err = plan_line_undo(&line_row("undo", create_changes())).unwrap_err();
    assert!(matches!(err, EditError::NotUndoable(7)), "{err}");
    // foreign kind
    let mut c = create_changes();
    c["kind"] = serde_json::json!("station_move");
    let err = plan_line_undo(&line_row("update", c)).unwrap_err();
    assert!(matches!(err, EditError::NotUndoable(7)), "{err}");
    // REVIEW-FOCUS pin: legacy phase-1 piece rows refuse cleanly — history
    // stays, undo moves on
    let mut c = create_changes();
    c["kind"] = serde_json::json!("polyline_piece");
    let err = plan_line_undo(&line_row("update", c)).unwrap_err();
    assert!(matches!(err, EditError::NotUndoable(7)), "{err}");
    // missing xno / direction
    let mut c = create_changes();
    c["xno"] = serde_json::Value::Null;
    let err = plan_line_undo(&line_row("create", c)).unwrap_err();
    assert!(matches!(err, EditError::NotUndoable(7)), "{err}");
    // corrupt payload shapes: no encoded, half payload
    for bad in [
        serde_json::json!({ "kind": "polyline_override", "xno": "1", "direction": 1 }),
        serde_json::json!({ "kind": "polyline_override", "xno": "1", "direction": 1,
                            "encoded": { "old": "x" } }),
        serde_json::json!({ "kind": "polyline_override", "xno": "1", "direction": 1,
                            "encoded": { "old": null, "new": null } }),
    ] {
        let err = plan_line_undo(&line_row("update", bad)).unwrap_err();
        assert!(matches!(err, EditError::NotUndoable(7)), "{err}");
    }
}

#[test]
fn line_undo_end_to_end_create_then_undo_deletes() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let first = edit_polyline_line(&conn, cmd(), "herry", "a").unwrap();
    let out = undo_polyline_line(&conn, first.edit_id, "h", "rev").unwrap();
    assert!(out.deleted);
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM polyline_override", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    // undo of the undo → NotUndoable
    let err = undo_polyline_line(&conn, out.edit_id, "h", "again").unwrap_err();
    assert!(err.to_string().contains("cannot be undone"), "{err}");
}

#[test]
fn line_undo_end_to_end_update_then_undo_restores() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let first = edit_polyline_line(&conn, cmd(), "herry", "a").unwrap();
    let mut second = cmd();
    second.expected_updated_at = Some(first.updated_at.clone());
    second.points = vec![[38.5, -120.2], [39.6, -120.575], [40.7, -120.95]];
    edit_polyline_line(&conn, second, "herry", "b").unwrap();
    // undo the UPDATE (edit 2): newest non-undo polyline row for the route
    let newest: i64 = conn
        .query_row(
            "SELECT edit_id FROM edit_log
             WHERE entity = 'polyline' AND entity_id = 1 AND action != 'undo'
             ORDER BY edit_id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let out = undo_polyline_line(&conn, newest, "h", "rev").unwrap();
    assert!(!out.deleted);
    let enc: String = conn
        .query_row(
            "SELECT encoded FROM polyline_override WHERE xno = '1' AND direction = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(enc, "_p~iF~ps|U_ulLnnqC");
}

#[test]
fn line_undo_superseded_refused() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let first = edit_polyline_line(&conn, cmd(), "herry", "a").unwrap();
    let mut second = cmd();
    second.expected_updated_at = Some(first.updated_at.clone());
    second.points = vec![[38.5, -120.2], [39.6, -120.575], [40.7, -120.95]];
    edit_polyline_line(&conn, second, "herry", "b").unwrap();
    // now_rfc3339 is second-granular, so both edits usually land in the same
    // wall-clock second. Diverge the row directly to make "the line moved
    // on" deterministic.
    conn.execute(
        "UPDATE polyline_override SET updated_at = '2030-01-01T00:00:00Z'
         WHERE xno = '1' AND direction = 1",
        [],
    )
    .unwrap();
    // undo of the FIRST edit: line moved on → 409-shaped refusal
    let err = undo_polyline_line(&conn, first.edit_id, "h", "rev").unwrap_err();
    assert!(err.to_string().contains("edited by someone else"), "{err}");
}

#[test]
fn line_undo_unknown_or_foreign_edit_refused() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let err = undo_polyline_line(&conn, 999, "h", "").unwrap_err();
    assert!(err.to_string().contains("cannot be undone"), "{err}");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rm-edit --test line 2>&1 | tail -10`
Expected: COMPILE FAIL — `LineEditCmd` / `plan_line_undo` not found.

- [ ] **Step 3: Add the EditError variants**

In `crates/rm-edit/src/rename.rs`, inside `pub enum EditError` (after `PieceScope`), add:

```rust
    #[error("invalid polyline: {0}")]
    InvalidLine(String),
    #[error("line {0}/{1} was edited by someone else — reload and retry")]
    LineNotCurrent(String, i64),
```

- [ ] **Step 4: Write `line.rs`**

`crates/rm-edit/src/line.rs`:

```rust
//! Manual whole-line polyline override (phase 2). Pure planning over the
//! loaded snapshot; persistence is rm-db's. An edit replaces the entire
//! direction polyline: effective = override ?? src. Validation = decode
//! round-trip + ≥2 points — no anchors, no seams, nothing to splice.

use anyhow::Result;
use rm_core::LatLng;
use rusqlite::Connection;

use crate::{map_db_err, EditError};

#[derive(Debug, Clone)]
pub struct LineEditCmd {
    pub xno: String,
    pub direction: i64,
    /// `[lat, lon]` pairs, draw order.
    pub points: Vec<[f64; 2]>,
    pub expected_updated_at: Option<String>,
}

#[derive(Debug)]
pub struct LineEditPlan {
    pub encoded: String,
    pub expected_updated_at: Option<String>,
}

fn latlng(pt: [f64; 2]) -> Result<LatLng, EditError> {
    let (lat, lon) = (pt[0], pt[1]);
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Err(EditError::InvalidLine("coordinate out of range".into()));
    }
    Ok(LatLng::new(lat, lon))
}

/// Pure planning policy. Guards, in order: coordinates valid; ≥2 points
/// (consecutive duplicates dropped, decode's rule); currency token matches
/// the current override — or is refused when the override is gone; an
/// identical re-save is a no-op. No I/O.
pub fn plan_line_edit(
    snap: &rm_db::LineSnapshot,
    cmd: LineEditCmd,
) -> Result<LineEditPlan, EditError> {
    let mut pts: Vec<LatLng> = Vec::with_capacity(cmd.points.len());
    for p in &cmd.points {
        let ll = latlng(*p)?;
        if pts.last() != Some(&ll) {
            pts.push(ll);
        }
    }
    if pts.len() < 2 {
        return Err(EditError::InvalidLine("line needs ≥2 points".into()));
    }
    let encoded = rm_core::polyline::encode(&pts);

    match &snap.override_row {
        Some(row) => {
            if Some(row.updated_at.as_str()) != cmd.expected_updated_at.as_deref() {
                return Err(EditError::LineNotCurrent(cmd.xno.clone(), cmd.direction));
            }
            if row.encoded == encoded {
                return Err(EditError::NoOpEdit);
            }
        }
        // token carried but the override was undone away in between
        None if cmd.expected_updated_at.is_some() => {
            return Err(EditError::LineNotCurrent(cmd.xno.clone(), cmd.direction));
        }
        None => {}
    }

    Ok(LineEditPlan {
        encoded,
        expected_updated_at: cmd.expected_updated_at,
    })
}

#[derive(Debug)]
pub struct LineOutcome {
    pub edit_id: i64,
    pub updated_at: String,
    pub created: bool,
}

/// Application service: load → plan (pure) → apply (one tx).
pub fn edit_polyline_line(
    conn: &Connection,
    cmd: LineEditCmd,
    editor: &str,
    note: &str,
) -> Result<LineOutcome> {
    let snap = rm_db::load_polyline_line(conn, &cmd.xno, cmd.direction)?
        .ok_or_else(|| EditError::UnknownRoute(cmd.xno.clone()))?;
    let plan = plan_line_edit(&snap, cmd.clone())?;
    let applied = rm_db::apply_line_edit(
        conn,
        &rm_db::LineEditOp {
            xno: cmd.xno,
            direction: cmd.direction,
            encoded: plan.encoded,
            expected_updated_at: plan.expected_updated_at,
        },
        editor,
        note,
    )
    .map_err(map_db_err)?;
    Ok(LineOutcome {
        edit_id: applied.edit_id,
        updated_at: applied.updated_at,
        created: applied.created,
    })
}

#[derive(Debug)]
pub struct LineUndoPlan {
    pub edit_id: i64,
    pub route_id: i64,
    pub xno: String,
    pub direction: i64,
    pub undone_ts: String,
    pub restored: Option<String>,
}

/// Pure guard + inversion from the logged payload: only a non-undo
/// polyline row with `kind = 'polyline_override'` and a parseable xno +
/// direction is invertible. The `encoded.old` branch decides
/// restore-vs-delete: `old` present → restore that state; `old` absent (a
/// create row) → delete the override. Missing/half payloads and legacy
/// `polyline_piece` rows are corrupt-for-this-purpose → NotUndoable.
pub fn plan_line_undo(edit: &rm_db::PolylineEditRow) -> Result<LineUndoPlan, EditError> {
    if edit.action == "undo"
        || edit.changes.get("kind").and_then(|k| k.as_str()) != Some("polyline_override")
    {
        return Err(EditError::NotUndoable(edit.edit_id));
    }
    let xno = edit
        .changes
        .get("xno")
        .and_then(|v| v.as_str())
        .ok_or(EditError::NotUndoable(edit.edit_id))?
        .to_string();
    let direction = edit
        .changes
        .get("direction")
        .and_then(|v| v.as_i64())
        .ok_or(EditError::NotUndoable(edit.edit_id))?;
    let encoded = edit
        .changes
        .get("encoded")
        .ok_or(EditError::NotUndoable(edit.edit_id))?;
    let old = encoded.get("old").and_then(|v| v.as_str());
    let new = encoded.get("new").and_then(|v| v.as_str());
    let restored = match (old, new) {
        // every undoable row has a `new` (the state it put in place)
        (None, Some(_)) => None,             // create → undo deletes the override
        (Some(encoded), Some(_)) => Some(encoded.to_string()), // update → restore prior
        _ => return Err(EditError::NotUndoable(edit.edit_id)),
    };

    Ok(LineUndoPlan {
        edit_id: edit.edit_id,
        route_id: edit.entity_id,
        xno,
        direction,
        undone_ts: edit.ts.clone(),
        restored,
    })
}

#[derive(Debug)]
pub struct LineUndoOutcome {
    pub edit_id: i64,
    pub deleted: bool,
}

/// Application service: load → plan (pure) → apply (one tx). Forward-write
/// undo — no version chain; the apply's ts currency guard is the authority.
pub fn undo_polyline_line(
    conn: &Connection,
    edit_id: i64,
    editor: &str,
    note: &str,
) -> Result<LineUndoOutcome> {
    let row = rm_db::load_polyline_edit(conn, edit_id)?.ok_or(EditError::NotUndoable(edit_id))?;
    let plan = plan_line_undo(&row)?;
    let undone = rm_db::apply_line_undo(
        conn,
        &rm_db::LineUndoOp {
            edit_id: plan.edit_id,
            route_id: plan.route_id,
            xno: plan.xno,
            direction: plan.direction,
            undone_ts: plan.undone_ts,
            restored: plan.restored,
        },
        editor,
        note,
    )
    .map_err(map_db_err)?;
    Ok(LineUndoOutcome {
        edit_id: undone.edit_id,
        deleted: undone.deleted,
    })
}
```

- [ ] **Step 5: Wire the module + `map_db_err`**

In `crates/rm-edit/src/lib.rs`:

- In the module list, add after `mod piece;`:

```rust
mod line;
```

- In the `pub use` list, add after `pub use piece::*;`:

```rust
pub use line::*;
```

- In `map_db_err`, add after the `PieceScope` arm:

```rust
        Some(rm_db::EditError::InvalidLine(m)) => EditError::InvalidLine(m.clone()).into(),
        Some(rm_db::EditError::LineNotCurrent(x, d)) => {
            EditError::LineNotCurrent(x.clone(), *d).into()
        }
```

- [ ] **Step 6: Run the tests**

Run: `cargo test -p rm-edit`
Expected: PASS — new line tests green; existing piece tests still green (piece.rs untouched).

- [ ] **Step 7: Commit**

```bash
git add crates/rm-edit/src/line.rs crates/rm-edit/src/lib.rs crates/rm-edit/src/rename.rs crates/rm-edit/tests/line.rs
git commit -m "rm-edit: whole-line planning layer — plan_line_edit/plan_line_undo + services

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 4: rm-web — `/line` endpoint, undo dispatch, raw `effective` payload

**Files:**
- Modify: `crates/rm-web/src/handlers/edit.rs` (body struct, handler, undo dispatch, `edit_conflict` arms; DELETE the piece handler + body)
- Modify: `crates/rm-web/src/lib.rs:70-71` (route registration)
- Modify: `crates/rm-web/src/queries/routes.rs` (`raw()` — `manual_pieces` → `effective`)
- Test: `crates/rm-web/tests/edit.rs` (replace the `piece_*` tests)

**Interfaces:**
- Consumes: Task 3's `rm_edit::{LineEditCmd, edit_polyline_line, undo_polyline_line, LineOutcome, LineUndoOutcome, InvalidLine, LineNotCurrent}`; Task 2's `rm_db::effective_encoded` + `polyline_override` table.
- Produces (HTTP contract, consumed by Task 7's UI):
  - `POST /api/edit/polyline/{xno}/{direction}/line` body `{points: [[lat,lon],…], expected_updated_at?, note?}` → `{edit_id, updated_at, created}`; 400 validation, 409 currency. The `/piece` route is REMOVED.
  - `POST /api/edit/undo/{edit_id}` polyline branch → `{edit_id, restored: {deleted: bool}}` (no piece_id).
  - `GET /api/routes/{id}/directions/{dir}/raw` payload: `manual_pieces` removed; gains `effective: {points: [[lat,lon],…], source: "override"|"src", updated_at: string|null}`. (`updated_at` is a plan ruling — the spec's payload omits it, but the UI needs the override's token to echo as `expected_updated_at`; it is additive and spec-consistent.)

- [ ] **Step 1: Rewrite the failing handler tests**

In `crates/rm-web/tests/edit.rs`, DELETE these items (they test the retiring piece endpoint): `fn piece_body`, `piece_post_happy_path_and_raw_readback`, `piece_post_requires_auth_and_valid_payload`, `piece_post_stale_expectation_409`, `piece_post_overlapping_span_409`, `piece_undo_via_route_deletes_piece` (they sit between line ~720 and the file end; keep every non-piece test).

Add in their place (uses the file's existing `edit_state`, `body_json`, `get_json`, `auth` helpers):

```rust
fn line_body() -> serde_json::Value {
    serde_json::json!({
        "points": [[38.5, -120.2], [40.7, -120.95]],
        "note": "web test"
    })
}

fn line_post_req(app: &axum::Router, body: serde_json::Value, token: Option<&str>) -> std::pin::Pin<Box<dyn std::future::Future<Output = axum::response::Response> + Send>> {
    let mut b = Request::builder().method("POST").uri("/api/edit/polyline/1/1/line");
    if let Some(t) = token {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    Box::pin(app.clone().oneshot(b.header("content-type", "application/json").body(Body::from(body.to_string())).unwrap()))
}

#[tokio::test]
async fn line_post_happy_path_and_raw_readback() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    let app = rm_web::router_with(state.clone());
    let res = line_post_req(&app, line_body(), Some("sekrit")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["created"], serde_json::json!(true));
    assert!(v["edit_id"].as_i64().is_some(), "{v}");
    assert!(v["updated_at"].as_str().is_some(), "{v}");
    // raw endpoint: effective source = override with the points; the piece
    // field is gone
    let raw = get_json(&app, "/api/routes/1/directions/1/raw").await;
    assert_eq!(raw["effective"]["source"], "override");
    assert_eq!(raw["effective"]["points"][0], serde_json::json!([38.5, -120.2]));
    assert_eq!(raw["effective"]["updated_at"], v["updated_at"]);
    assert!(raw.get("manual_pieces").is_none());
}

#[tokio::test]
async fn raw_effective_defaults_to_src() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    let app = rm_web::router_with(state);
    let raw = get_json(&app, "/api/routes/1/directions/1/raw").await;
    assert_eq!(raw["effective"]["source"], "src");
    assert_eq!(raw["effective"]["updated_at"], serde_json::Value::Null);
    assert_eq!(raw["effective"]["points"][0], serde_json::json!([38.5, -120.2]));
}

#[tokio::test]
async fn line_post_requires_auth_and_valid_payload() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    let app = rm_web::router_with(state);
    let res = line_post_req(&app, line_body(), None).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let res = line_post_req(&app, line_body(), Some("nope")).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    // single point → 400
    let mut bad = line_body();
    bad["points"] = serde_json::json!([[38.5, -120.2]]);
    let res = line_post_req(&app, bad, Some("sekrit")).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    // out-of-range coordinate → 400
    let mut bad = line_body();
    bad["points"] = serde_json::json!([[95.0, -120.2], [40.7, -120.95]]);
    let res = line_post_req(&app, bad, Some("sekrit")).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    // unknown route → 400
    let mut b = Request::builder().method("POST").uri("/api/edit/polyline/999/1/line").header("authorization", "Bearer sekrit").header("content-type", "application/json");
    let res = app.oneshot(b.body(Body::from(line_body().to_string())).unwrap()).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn line_post_stale_expectation_409() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    let app = rm_web::router_with(state);
    let res = line_post_req(&app, line_body(), Some("sekrit")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let first = body_json(res).await;
    // replay with a stale token → 409, not 400
    let mut body = line_body();
    body["expected_updated_at"] = first["updated_at"].clone();
    let res = line_post_req(&app, body, Some("sekrit")).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST, "identical resave is a 400 no-op");
    let mut body = line_body();
    body["expected_updated_at"] = serde_json::json!("2020-01-01T00:00:00Z");
    let res = line_post_req(&app, body, Some("sekrit")).await.unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn line_undo_via_log_deletes_override() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    let app = rm_web::router_with(state.clone());
    let res = line_post_req(&app, line_body(), Some("sekrit")).await.unwrap();
    let first = body_json(res).await;
    let edit_id = first["edit_id"].as_i64().unwrap();
    // undo through the generic endpoint (entity = polyline via the log)
    let res = app.clone().oneshot(
        Request::builder().method("POST").uri(format!("/api/edit/undo/{edit_id}"))
            .header("authorization", "Bearer sekrit").header("content-type", "application/json")
            .body(Body::from(r#"{"note": "rev"}"#.to_string())).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["restored"]["deleted"], serde_json::json!(true));
    // row gone, raw back to src
    let conn = rm_db::open_readonly(&dir.path().join("t.db")).unwrap();
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM polyline_override", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0);
    drop(conn);
    let raw = get_json(&app, "/api/routes/1/directions/1/raw").await;
    assert_eq!(raw["effective"]["source"], "src");
    // the undo row itself refuses a second undo (undo-of-undo → 409)
    let undo_edit_id = v["edit_id"].as_i64().unwrap();
    let res = app.oneshot(
        Request::builder().method("POST").uri(format!("/api/edit/undo/{undo_edit_id}"))
            .header("authorization", "Bearer sekrit").header("content-type", "application/json")
            .body(Body::from(r#"{"note": "again"}"#.to_string())).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
}
```

NOTE: the identical-resave case (same points + fresh token) surfaces as 400 `NoOpEdit` — phase-1 behavior, kept. If the exact helpers (`auth`, `post`) in the file make `line_post_req` redundant, use the existing helpers instead; the assertions above are the contract.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rm-web --test edit 2>&1 | tail -10`
Expected: FAIL/COMPILE FAIL — `/line` route does not exist (404s), `manual_pieces` still in payload.

- [ ] **Step 3: Rewrite the handler**

In `crates/rm-web/src/handlers/edit.rs`:

1. In `edit_conflict`, add the new arms to the two match lists — `InvalidLine` goes in the 400 list, `LineNotCurrent` in the 409 list:

```rust
            | rm_edit::EditError::InvalidLine(_)
            | rm_edit::EditError::UnknownRoute(_) => {
                ApiError::bad_request(&e.to_string())
            }
```

```rust
            | rm_edit::EditError::LineNotCurrent(_, _)
            | rm_edit::EditError::PieceOverlap(_) => {
                ApiError::conflict(&e.to_string())
            }
```

(Leave all four existing piece arms in place — `InvalidPiece`, `PieceNotCurrent`, `PieceOverlap`, `PieceScope` die in Task 8, not here.)

2. DELETE `struct PolylinePieceBody` and `async fn edit_polyline_piece` (the whole block at the end of the file).

3. ADD in their place:

```rust
#[derive(Deserialize)]
pub struct PolylineBody {
    points: Vec<[f64; 2]>,
    expected_updated_at: Option<String>,
    note: Option<String>,
}

fn coord_ok(pt: [f64; 2]) -> bool {
    (-90.0..=90.0).contains(&pt[0]) && (-180.0..=180.0).contains(&pt[1])
}

/// POST /api/edit/polyline/{xno}/{direction}/line — store a whole-line
/// override (effective = override ?? src). Server encodes via rm-core; the
/// client sends plain coordinates.
pub async fn edit_polyline_line(
    Path((xno, direction)): Path<(String, i64)>,
    State(state): State<AppState>,
    Editor(editor): Editor,
    ApiJson(b): ApiJson<PolylineBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if b.points.iter().any(|p| !coord_ok(*p)) {
        return Err(ApiError::bad_request("coordinate out of range"));
    }
    let out = run_db_write(state, move |conn| {
        rm_edit::edit_polyline_line(
            conn,
            rm_edit::LineEditCmd {
                xno,
                direction,
                points: b.points,
                expected_updated_at: b.expected_updated_at,
            },
            &editor,
            b.note.as_deref().unwrap_or(""),
        )
    })
    .await
    .map_err(edit_conflict)?;
    Ok(Json(json!({
        "edit_id": out.edit_id,
        "updated_at": out.updated_at,
        "created": out.created,
    })))
}
```

4. In `undo`, swap the polyline branch (three-way dispatch stays):

```rust
        if is_polyline {
            rm_edit::undo_polyline_line(conn, edit_id, &editor, b.note.as_deref().unwrap_or(""))
                .map(|o| (o.edit_id, json!({ "deleted": o.deleted })))
        } else if is_station {
```

- [ ] **Step 4: Swap the route registration**

In `crates/rm-web/src/lib.rs`, replace the `/piece` route block:

```rust
        .route(
            "/api/edit/polyline/{xno}/{direction}/line",
            axum::routing::post(handlers::edit::edit_polyline_line),
        );
```

(the `.route(...)` call currently naming `edit_polyline_piece` at path `/piece` is deleted; keep the trailing router structure intact).

- [ ] **Step 5: Rewrite the raw payload**

In `crates/rm-web/src/queries/routes.rs` `raw()`, DELETE the whole `manual_pieces` block (the `let mut stmt = conn.prepare("SELECT piece_id, encoded, anchor_start_lat, …` through its `.collect();`) and replace the final `Ok(Some(json!(…)))` with:

```rust
    // effective = override ?? src — the line match/segments/registry
    // consume. `updated_at` is the override's concurrency token (null when
    // the effective line is plain src).
    let eff_encoded = rm_db::effective_encoded(conn, batch_id, &xno, dir)?;
    let source: &str = if conn.query_row(
        "SELECT COUNT(*) FROM polyline_override WHERE xno = ?1 AND direction = ?2",
        rusqlite::params![xno, dir],
        |r| r.get::<_, i64>(0),
    )? > 0 {
        "override"
    } else {
        "src"
    };
    let eff_updated_at: Option<String> = conn
        .query_row(
            "SELECT updated_at FROM polyline_override WHERE xno = ?1 AND direction = ?2",
            rusqlite::params![xno, dir],
            |r| r.get(0),
        )
        .ok();
    let eff_points: Vec<Vec<f64>> = polyline::decode(&eff_encoded)
        .map(|pts| pts.iter().map(|p| vec![p.lat, p.lon]).collect())
        .unwrap_or_default();

    Ok(Some(json!({
        "batch_id": batch_id,
        "path": path,
        "stops": stops,
        "effective": {
            "points": eff_points,
            "source": source,
            "updated_at": eff_updated_at,
        },
    })))
```

NOTE: `raw()` lives in `queries/routes.rs` and needs `rm_db` in scope — check the file's `use` block; add `use rm_db;`-style access only if the crate isn't already referenced (it is a dependency of rm-web already).

- [ ] **Step 6: Run the tests**

Run: `cargo test -p rm-web`
Expected: PASS — new line tests green, all other edit tests (`stop_edit_*`, `log_and_undo_roundtrip`, station tests) untouched and green.

- [ ] **Step 7: Commit**

```bash
git add crates/rm-web/src/handlers/edit.rs crates/rm-web/src/lib.rs crates/rm-web/src/queries/routes.rs crates/rm-web/tests/edit.rs
git commit -m "rm-web: POST /polyline/{xno}/{dir}/line + raw effective payload; /piece endpoint removed

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 5: Readers consume `effective_encoded` (match, segments, registry)

**Files:**
- Modify: `crates/rm-osm-match/src/lib.rs` (`Job` enumeration → `collect_jobs` with effective fill)
- Modify: `crates/rm-segments/src/lib.rs:163-180` (per-direction encoded map)
- Modify: `crates/rm-registry/src/bearing.rs` (~line 123-135, polyline fallback)
- Test: `crates/rm-osm-match/tests/match.rs` (one new test)

**Interfaces:**
- Consumes: Task 2's `rm_db::effective_encoded(conn, batch_id, xno, direction) -> Result<String>` (all three crates already depend on `rm-db`).
- Produces: behavioral contract — every reader sees the override when one exists. No signature changes.

- [ ] **Step 1: Write the failing test**

Append to `crates/rm-osm-match/tests/match.rs`:

```rust
#[test]
fn full_run_consumes_override_over_src() {
    let (_dir, conn, batch) = setup();
    insert_straight_road(&conn);
    // kill the src polyline; the override is now the only geometry — if the
    // run still read src, route2/dir0 would land in `unmatched`
    conn.execute(
        "UPDATE src_route_point SET encoded = '' WHERE batch_id = ?1 AND xno = '2' AND direction = 0",
        rusqlite::params![batch],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO polyline_override (xno, direction, encoded, updated_at)
         VALUES ('2', 0, ?1, '2026-01-01T00:00:00Z')",
        rusqlite::params![straight_polyline()],
    )
    .unwrap();
    let s = rm_osm_match::run(&conn, batch).unwrap();
    assert_eq!(s.matched, 1, "override matched where empty src would not: {s:?}");
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -r -p rm-osm-match --test match full_run_consumes_override 2>&1 | tail -5`
Expected: FAIL — `matched == 0` (run still reads empty src).

- [ ] **Step 3: Swap the match job enumeration**

In `crates/rm-osm-match/src/lib.rs`, restructure `run()`'s opening. REPLACE the `let jobs: Vec<Job> = { … };` block (the SQL selecting `srp.encoded`) with a `collect_jobs` helper that drops `encoded` from the SELECT and fills it from `effective_encoded` afterwards:

```rust
/// One unit of matching work: a published direction with its polyline.
struct Job {
    route_version_id: i64,
    subroute_version_id: i64,
    direction: i64,
    xno: String,
    stops: i64,
    encoded: String,
}

/// Enumerate the published directions of `batch` and resolve each job's
/// effective polyline (override ?? src — phase 2: the run matches the line
/// consumers see, not the raw capture). The `src_route_point` JOIN stays in
/// the query: it decides which directions exist as jobs at all.
fn collect_jobs(conn: &Connection, batch_id: i64) -> anyhow::Result<Vec<Job>> {
    let mut jobs: Vec<Job> = {
        let mut stmt = conn.prepare(
            "SELECT rv.route_version_id, sv.subroute_version_id, srp.direction,
                    substr(r.ext_uid, 5),
                    (SELECT COUNT(*) FROM route_stop_version o
                      WHERE o.subroute_version_id = sv.subroute_version_id
                        AND o.direction = srp.direction)
             FROM route r
             JOIN route_version rv ON rv.route_id = r.route_id
                  AND rv.route_version_id = r.current_route_version_id
             JOIN subroute_version sv ON sv.route_version_id = rv.route_version_id
             JOIN src_route_point srp
                  ON srp.batch_id = ?1 AND srp.xno = substr(r.ext_uid, 5)
             ORDER BY rv.route_version_id, srp.direction",
        )?;
        let rows = stmt.query_map([batch_id], |r| {
            Ok(Job {
                route_version_id: r.get(0)?,
                subroute_version_id: r.get(1)?,
                direction: r.get(2)?,
                xno: r.get(3)?,
                stops: r.get(4)?,
                encoded: String::new(),
            })
        })?;
        rows.collect::<Result<_, _>>()?
    };
    for job in &mut jobs {
        job.encoded = rm_db::effective_encoded(conn, batch_id, &job.xno, job.direction)?;
    }
    Ok(jobs)
}
```

And in `run()`, replace the inlined block with:

```rust
    let jobs = collect_jobs(conn, batch_id)?;
```

(Keep `struct Outcome` and everything from `let outcomes` onward exactly as-is in this task.)

- [ ] **Step 4: Swap the segments polyline source**

In `crates/rm-segments/src/lib.rs`, replace the per-direction encoded map block:

```rust
        // polylines for this route, by direction (effective = override ?? src)
        let mut encoded: std::collections::HashMap<i64, String> = std::collections::HashMap::new();
        {
            let dirs: std::collections::BTreeSet<i64> =
                stops.iter().map(|s| s.direction).collect();
            for dir in dirs {
                encoded.insert(dir, rm_db::effective_encoded(&tx, batch_id, xno, dir)?);
            }
        }
```

This replaces the old `SELECT direction, encoded FROM src_route_point WHERE batch_id = ?1 AND xno = ?2` block. Behavior note: directions whose only polyline was a src row with zero stops lose an unused map entry — no observable change; directions with stops but no src row now get `""` exactly as before.

- [ ] **Step 5: Swap the registry bearing fallback**

In `crates/rm-registry/src/bearing.rs`, in the per-direction fallback block (`// direction polyline for the fallback source`), replace the `match &xno { Some(x) => tx.query_row("SELECT encoded FROM src_route_point …")… }` body with:

```rust
            let path: Vec<LatLng> = match &xno {
                Some(x) => rm_db::effective_encoded(&tx, batch_id, x, dir)
                    .ok()
                    .and_then(|e| rm_core::polyline::decode(&e).ok())
                    .unwrap_or_default(),
                None => Vec::new(),
            };
```

(`&tx` derefs to `&Connection`; `batch_id`, `xno`, `dir` are the block's existing bindings.)

- [ ] **Step 6: Run the tests**

Run: `cargo test -r -p rm-osm-match -p rm-segments -p rm-registry 2>&1 | tail -15`
Expected: PASS — new override test green; all existing match/segments/registry tests green.

- [ ] **Step 7: Commit**

```bash
git add crates/rm-osm-match/src/lib.rs crates/rm-segments/src/lib.rs crates/rm-registry/src/bearing.rs crates/rm-osm-match/tests/match.rs
git commit -m "readers consume effective polyline (override ?? src) — match jobs, segments, bearing fallback

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 6: rm-osm-match `--stale` (per-direction delete → match → insert → clear)

**Files:**
- Modify: `crates/rm-osm-match/src/lib.rs` (refactor `run` into `collect_jobs`/`match_jobs`/`write_outcomes`; add `run_stale` + `stale_jobs`)
- Modify: `crates/rm-osm-match/src/main.rs` (`--stale` flag + dispatch)
- Test: `crates/rm-osm-match/tests/match.rs` (three new tests)

**Interfaces:**
- Consumes: Task 2's `seg_match` mark convention (`key = '{route_id}:{direction}'`, written by `apply_line_edit`/`apply_line_undo`); Task 5's `collect_jobs`/`Job`.
- Produces:
  - `pub struct StaleOutcome { pub summary: Summary, pub cleared: usize, pub kept: usize }`
  - `pub fn run_stale(conn: &Connection, batch_id: i64) -> anyhow::Result<Option<StaleOutcome>>` — `None` when nothing is marked.
  - CLI: `rm-osm-match --db <path> --stale [--batch N]`.

- [ ] **Step 1: Write the failing tests**

Append to `crates/rm-osm-match/tests/match.rs`:

```rust
fn route2_rid(conn: &rusqlite::Connection) -> i64 {
    conn.query_row(
        "SELECT route_id FROM route WHERE ext_uid = 'tcg:2'",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

fn seg_mark(conn: &rusqlite::Connection, rid: i64, dir: i64, key: Option<String>) {
    let key = key.unwrap_or(format!("{rid}:{dir}"));
    // cause_edit_id carries an FK into edit_log (foreign_keys=ON in rm-db) —
    // seed the cause row first
    conn.execute(
        "INSERT INTO edit_log (ts, editor, entity, entity_id, action, changes)
         VALUES ('2026-01-01T00:00:00Z', 'test', 'polyline', ?1, 'update', '{}')",
        rusqlite::params![rid],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO stale_mark (table_name, key, cause_edit_id, marked_at)
         VALUES ('seg_match', ?1, 1, '2026-01-01T00:00:00Z')",
        rusqlite::params![key],
    )
    .unwrap();
}

#[test]
fn stale_run_recomputes_marked_direction_and_clears() {
    let (_dir, conn, batch) = setup();
    insert_straight_road(&conn);
    conn.execute(
        "UPDATE src_route_point SET encoded = '' WHERE batch_id = ?1 AND xno = '2' AND direction = 0",
        rusqlite::params![batch],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO polyline_override (xno, direction, encoded, updated_at)
         VALUES ('2', 0, ?1, '2026-01-01T00:00:00Z')",
        rusqlite::params![straight_polyline()],
    )
    .unwrap();
    let s = rm_osm_match::run(&conn, batch).unwrap();
    assert_eq!(s.matched, 1, "{s:?}");
    let (ways_before, stats_before): (i64, i64) = conn
        .query_row(
            "SELECT (SELECT COUNT(*) FROM seg_match_way), (SELECT COUNT(*) FROM seg_match_stat)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(ways_before >= 1 && stats_before >= 1);

    seg_mark(&conn, route2_rid(&conn), 0, None);
    // the line moved on: garbage override → recompute drops route2's rows
    conn.execute(
        "UPDATE polyline_override SET encoded = 'zzzz' WHERE xno = '2' AND direction = 0",
        [],
    )
    .unwrap();
    let out = rm_osm_match::run_stale(&conn, batch).unwrap().unwrap();
    assert_eq!(out.cleared, 1);
    assert_eq!(out.kept, 0);
    assert_eq!(out.summary.directions, 1, "{:?}", out.summary);
    assert_eq!(out.summary.failed, 1, "garbage polyline fails decode: {:?}", out.summary);
    // route2's rows are gone (deleted, not replaced), nothing else touched
    let ways_after: i64 = conn
        .query_row("SELECT COUNT(*) FROM seg_match_way", [], |r| r.get(0))
        .unwrap();
    let stats_after: i64 = conn
        .query_row("SELECT COUNT(*) FROM seg_match_stat", [], |r| r.get(0))
        .unwrap();
    assert_eq!((ways_after, stats_after), (0, 0));
    let marks: i64 = conn
        .query_row("SELECT COUNT(*) FROM stale_mark WHERE table_name = 'seg_match'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(marks, 0, "cleared");
}

#[test]
fn stale_run_keeps_unknown_and_unmatched_marks() {
    let (_dir, conn, batch) = setup();
    insert_straight_road(&conn);
    rm_osm_match::run(&conn, batch).unwrap();
    // unknown route id + unparseable key: warn + keep, no deletes
    seg_mark(&conn, 999, 0, Some("999:0".into()));
    seg_mark(&conn, 0, 0, Some("bogus".into()));
    let out = rm_osm_match::run_stale(&conn, batch).unwrap().unwrap();
    assert_eq!((out.cleared, out.kept), (0, 2), "{out:?}");
    assert_eq!(out.summary.directions, 0);
    let marks: i64 = conn
        .query_row("SELECT COUNT(*) FROM stale_mark WHERE table_name = 'seg_match'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(marks, 2, "kept");
    // REVIEW-FOCUS pin: unknown marks never delete matched rows
    let ways: i64 = conn
        .query_row("SELECT COUNT(*) FROM seg_match_way", [], |r| r.get(0))
        .unwrap();
    assert!(ways >= 1, "route2's runs survived");
}

#[test]
fn stale_run_without_marks_is_none() {
    let (_dir, conn, batch) = setup();
    assert!(rm_osm_match::run_stale(&conn, batch).unwrap().is_none());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -r -p rm-osm-match --test match stale 2>&1 | tail -5`
Expected: COMPILE FAIL — `run_stale` not found.

- [ ] **Step 3: Refactor `run` into three pieces + add `run_stale`**

In `crates/rm-osm-match/src/lib.rs`:

1. `collect_jobs` already exists (Task 5). Extract the outcomes loop into a free function (place it between `collect_jobs` and `run`):

```rust
struct Outcome {
    job: Job,
    result: anyhow::Result<MatchOutput>,
}

fn match_jobs(graph: &Arc<RoadGraph>, jobs: Vec<Job>) -> Vec<Outcome> {
    jobs.into_par_iter()
        .map_init(|| viterbi::Ctx::new(graph), |ctx, job| {
            let result = (|| {
                let pts = rm_core::polyline::decode(&job.encoded)?;
                Ok(viterbi::match_path(
                    ctx,
                    &pts,
                    &NewsonKrummEmission::default(),
                    &BasicTransition::default(),
                ))
            })();
            Outcome { job, result }
        })
        .collect()
}
```

2. Extract the write section into (replacing the inlined `let tx = …` block inside `run`; the insert loop body is byte-identical to today's — only the DELETE part gains the scoped branch):

```rust
/// Write outcomes. `None` = full-run wipe-all; `Some(pairs)` = stale run:
/// per-(route_id, direction) scoped deletes ONLY for the pairs recomputed
/// (rows joined through subroute/route on route_id + direction — EVERY
/// route_version, so rows written under an older version cannot survive
/// the recompute). An empty pair list deletes NOTHING — a stale run whose
/// marks were all kept must never wipe matched rows.
fn write_outcomes(
    conn: &Connection,
    batch_id: i64,
    outcomes: Vec<Outcome>,
    scope: Option<&[(i64, i64)]>,
) -> anyhow::Result<Summary> {
    let mut summary = Summary {
        batch_id,
        directions: outcomes.len(),
        ..Default::default()
    };
    let tx = conn.unchecked_transaction()?;
    match scope {
        None => {
            tx.execute("DELETE FROM seg_match_way", [])?;
            tx.execute("DELETE FROM seg_match_stat", [])?;
        }
        Some(pairs) => {
            for (rid, dir) in pairs {
                tx.execute(
                    "DELETE FROM seg_match_way
                     WHERE direction = ?2 AND subroute_version_id IN (
                         SELECT sv.subroute_version_id FROM subroute_version sv
                         JOIN route_version rv ON rv.route_version_id = sv.route_version_id
                         WHERE rv.route_id = ?1)",
                    rusqlite::params![rid, dir],
                )?;
                tx.execute(
                    "DELETE FROM seg_match_stat
                     WHERE direction = ?2 AND route_version_id IN (
                         SELECT route_version_id FROM route_version WHERE route_id = ?1)",
                    rusqlite::params![rid, dir],
                )?;
            }
        }
    }
    {
        let mut ins_way = tx.prepare(
            "INSERT INTO seg_match_way
                (subroute_version_id, direction, ordinal, osm_way_id,
                 entry_node_id, exit_node_id, cum_start_m, cum_end_m)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        let mut ins_stat = tx.prepare(
            "INSERT INTO seg_match_stat
                (route_version_id, direction, stops, path_length_m, matched_length_m,
                 snap_dist_p50, snap_dist_p95, snap_dist_max, outliers, gap_repairs,
                 dev_p50_m, dev_p95_m, dev_max_m)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        )?;
        for oc in outcomes {
            let m = match oc.result {
                Ok(m) => m,
                Err(e) => {
                    summary.failed += 1;
                    tracing::warn!(xno = %oc.job.xno, direction = oc.job.direction, error = %e,
                        "match failed; direction skipped");
                    continue;
                }
            };
            if m.runs.is_empty() {
                summary.unmatched += 1;
                tracing::info!(xno = %oc.job.xno, direction = oc.job.direction,
                    "no way runs for direction");
                continue;
            }
            summary.matched += 1;
            summary.total_runs += m.runs.len();
            summary.gap_repairs += m.gap_repairs;
            for (i, r) in m.runs.iter().enumerate() {
                ins_way.execute(rusqlite::params![
                    oc.job.subroute_version_id,
                    oc.job.direction,
                    (i + 1) as i64,
                    r.way_id,
                    r.entry_node,
                    r.exit_node,
                    r.cum_start_m,
                    r.cum_end_m,
                ])?;
            }
            ins_stat.execute(rusqlite::params![
                oc.job.route_version_id,
                oc.job.direction,
                oc.job.stops,
                m.path_length_m,
                m.matched_length_m,
                m.snap_p50_m,
                m.snap_p95_m,
                m.snap_max_m,
                m.outliers as i64,
                m.gap_repairs as i64,
                m.dev_p50_m,
                m.dev_p95_m,
                m.dev_max_m,
            ])?;
        }
    }
    tx.commit()?;
    Ok(summary)
}
```

3. `run` collapses to:

```rust
pub fn run(conn: &Connection, batch_id: i64) -> anyhow::Result<Summary> {
    let graph = Arc::new(RoadGraph::from_conn(conn)?);
    let jobs = collect_jobs(conn, batch_id)?;
    let outcomes = match_jobs(&graph, jobs);
    write_outcomes(conn, batch_id, outcomes, None)
}
```

4. Add `run_stale` + its job resolver after `run`:

```rust
#[derive(Debug)]
pub struct StaleOutcome {
    pub summary: Summary, // counts over processed directions only
    pub cleared: usize,   // mark rowids deleted
    pub kept: usize,      // marks left (unknown key / unpublished route / no staging row)
}

/// One marked (route_id, direction): CURRENT route_version, its stops, its
/// effective polyline. Usually one job; a route_version with several
/// subroute_versions yields several (the full run joins the same way).
fn stale_jobs(
    conn: &Connection,
    batch_id: i64,
    route_id: i64,
    direction: i64,
) -> anyhow::Result<Vec<Job>> {
    let mut jobs: Vec<Job> = {
        let mut stmt = conn.prepare(
            "SELECT rv.route_version_id, sv.subroute_version_id, substr(r.ext_uid, 5),
                    (SELECT COUNT(*) FROM route_stop_version o
                      WHERE o.subroute_version_id = sv.subroute_version_id
                        AND o.direction = ?2)
             FROM route r
             JOIN route_version rv ON rv.route_id = r.route_id
                  AND rv.route_version_id = r.current_route_version_id
             JOIN subroute_version sv ON sv.route_version_id = rv.route_version_id
             JOIN src_route_point srp
                  ON srp.batch_id = ?1 AND srp.xno = substr(r.ext_uid, 5)
                    AND srp.direction = ?2
             WHERE r.route_id = ?3",
        )?;
        let rows = stmt.query_map(rusqlite::params![batch_id, direction, route_id], |r| {
            Ok(Job {
                route_version_id: r.get(0)?,
                subroute_version_id: r.get(1)?,
                direction,
                xno: r.get(2)?,
                stops: r.get(3)?,
                encoded: String::new(),
            })
        })?;
        rows.collect::<Result<_, _>>()?
    };
    for job in &mut jobs {
        job.encoded = rm_db::effective_encoded(conn, batch_id, &job.xno, job.direction)?;
    }
    Ok(jobs)
}

/// Refresh exactly the (route_id, direction) pairs marked stale in
/// `seg_match` (key `'{route_id}:{direction}'`; route_id stable across
/// route versions — the job resolves the CURRENT route_version here).
/// Snapshot (rowid, cause) pairs first; the clear matches both, so a mark
/// re-inserted onto a reused rowid mid-run carries a different cause and
/// survives. Unknown keys / unpublished routes / directions whose staging
/// row the latest batch dropped: warn, keep — never delete rows that were
/// not recomputed.
pub fn run_stale(conn: &Connection, batch_id: i64) -> anyhow::Result<Option<StaleOutcome>> {
    let snapshot: Vec<(i64, i64, String)> = {
        let mut stmt = conn.prepare(
            "SELECT rowid, cause_edit_id, key FROM stale_mark
             WHERE table_name = 'seg_match'",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect::<Result<_, _>>()?
    };
    if snapshot.is_empty() {
        return Ok(None);
    }
    let graph = Arc::new(RoadGraph::from_conn(conn)?);

    let mut jobs: Vec<Job> = Vec::new();
    let mut scope: Vec<(i64, i64)> = Vec::new();
    let mut clear: Vec<(i64, i64)> = Vec::new();
    let mut kept = 0usize;
    for (rowid, cause, key) in snapshot {
        let Some((rid, dir)) = key.split_once(':').and_then(|(r, d)| {
            Some((r.parse::<i64>().ok()?, d.parse::<i64>().ok()?))
        }) else {
            tracing::warn!(key = %key, "unparseable seg_match key; keeping mark");
            kept += 1;
            continue;
        };
        let published: Option<Option<i64>> = conn
            .query_row(
                "SELECT current_route_version_id FROM route WHERE route_id = ?1",
                [rid],
                |r| r.get::<_, Option<i64>>(0),
            )
            .optional()?;
        if published.flatten().is_none() {
            tracing::warn!(route = rid, "stale route not published; keeping mark");
            kept += 1;
            continue;
        }
        let mark_jobs = stale_jobs(conn, batch_id, rid, dir)?;
        if mark_jobs.is_empty() {
            // latest batch dropped this direction's staging row: nothing to
            // recompute — keep the mark (a later ingest may restore it)
            tracing::warn!(route = rid, direction = dir,
                "no staging row for stale direction; keeping mark");
            kept += 1;
            continue;
        }
        jobs.extend(mark_jobs);
        scope.push((rid, dir));
        clear.push((rowid, cause));
    }

    let outcomes = match_jobs(&graph, jobs);
    let summary = write_outcomes(conn, batch_id, outcomes, Some(&scope))?;

    // clear exactly the snapshot (rowid, cause) pairs that were processed —
    // a mark re-written since the snapshot (fresh rowid or cause) survives
    let tx = conn.unchecked_transaction()?;
    for (rowid, cause) in &clear {
        tx.execute(
            "DELETE FROM stale_mark WHERE rowid = ?1 AND cause_edit_id = ?2",
            rusqlite::params![rowid, cause],
        )?;
    }
    tx.commit()?;

    Ok(Some(StaleOutcome {
        summary,
        cleared: clear.len(),
        kept,
    }))
}
```

- [ ] **Step 4: Add the CLI flag**

In `crates/rm-osm-match/src/main.rs`:

1. In `struct Args`, after `batch`:

```rust
    /// Refresh only directions marked stale, then clear their marks
    #[arg(long)]
    stale: bool,
```

2. In `main`, after `let batch = …;`, before `let s = rm_osm_match::run(…)`:

```rust
    if args.stale {
        match rm_osm_match::run_stale(&conn, batch)? {
            Some(out) => println!(
                "stale: {} directions, {} matched, {} failed, {} marks cleared, {} kept",
                out.summary.directions, out.summary.matched, out.summary.failed, out.cleared, out.kept
            ),
            None => println!("nothing stale"),
        }
        return Ok(());
    }
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -r -p rm-osm-match 2>&1 | tail -10`
Expected: PASS — three new stale tests green; `matches_direction_to_way_runs_and_stats`, `rerun_is_idempotent`, `empty_osm_tables_is_an_error`, `full_run_consumes_override_over_src` still green (the refactor must be behavior-preserving for the full run).

- [ ] **Step 6: Commit**

```bash
git add crates/rm-osm-match/src/lib.rs crates/rm-osm-match/src/main.rs crates/rm-osm-match/tests/match.rs
git commit -m "rm-osm-match: --stale per-direction recompute (delete+match+insert+clear marks)

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 7: UI — whole-line editing (lineEdit.js, effective layer, panel text)

**Files:**
- Create: `crates/rm-web/assets/js/lineEdit.js`
- Delete: `crates/rm-web/assets/js/pieceEdit.js`
- Modify: `crates/rm-web/assets/js/api.js:55-56`
- Modify: `crates/rm-web/assets/js/layers.js:129-159` (drawRaw)
- Modify: `crates/rm-web/assets/js/panels.js:355-357` (changeText polyline branch)
- Modify: `crates/rm-web/assets/js/app.js` (imports, syncLineBtn, onLineSaved, loadRaw, openLineLog, undoLineEdit, boot, layer-effective listener)
- Modify: `crates/rm-web/assets/index.html:42` (new layer checkbox after it)
- Modify: `crates/rm-web/src/handlers/assets.rs:51-54` + `crates/rm-web/src/handlers/mod.rs:23` (asset route rename)
- Test: manual — compile + serve (assets are `include_str!`-baked; `cargo build -r -p rm-web` then restart the server to see changes)

**Interfaces:**
- Consumes: Task 4's `POST /api/edit/polyline/{xno}/{direction}/line` (`{points, expected_updated_at?, note}` → `{edit_id, updated_at, created}`), the undo response `{edit_id, restored: {deleted}}`, and the raw payload `effective: {points, source: 'override'|'src', updated_at}` (points are `[lat,lon]`).
- Produces: `api.editPolylineLine(xno, dir, body)`; `lineEdit.js` exports `startLineEdit(routeId, dir, xno, raw, onSaved)`, `stopLineEdit()`, `lineEditActive()`; `layers.js` exports `setEffectiveOn(bool)` and `drawRaw(raw, { onEditLine })`.

- [ ] **Step 1: api.js — swap the edit call**

In `crates/rm-web/assets/js/api.js`, replace the last two lines:

```js
export const editPolylinePiece = (xno, dir, body) =>
  editFetch(`/api/edit/polyline/${encodeURIComponent(xno)}/${dir}/piece`, body);
```

with:

```js
export const editPolylineLine = (xno, dir, body) =>
  editFetch(`/api/edit/polyline/${encodeURIComponent(xno)}/${dir}/line`, body);
```

- [ ] **Step 2: Create lineEdit.js, delete pieceEdit.js**

Create `crates/rm-web/assets/js/lineEdit.js`:

```js
// Whole-line override editing (phase 2). One armed session at a time: the
// effective line (override ?? src) renders fully editable — drag any vertex,
// dblclick to delete, click a midpoint to insert; save = POST .../line
// replacing the entire direction polyline. The server owns polyline math;
// this file only collects [lat,lon] pairs. Esc / edit-off tear down.
// src coords arrive GeoJSON lon-first and are normalized to [lat,lon]
// exactly once, here; effective.points are already [lat,lon].
import { editPolylineLine } from './api.js';

const VTX = (color) =>
  L.divIcon({
    className: '',
    html: `<div style="width:10px;height:10px;border-radius:50%;background:${color};border:2px solid #fff;box-shadow:0 0 2px #0008;"></div>`,
    iconSize: [10, 10],
    iconAnchor: [5, 5],
  });

let session = null;
// { map, group, line, src, routeId, dir, xno, pts, expectedUpdatedAt, btn, onSaved }

export function lineEditActive() {
  return session != null;
}

export function stopLineEdit() {
  if (!session) return;
  session.map.doubleClickZoom.enable();
  session.group.remove();
  session.btn?.remove();
  session = null;
}

// Full rebuild of the session layer. NEVER called while a vertex marker is
// mid-drag: drag updates the line in place via setLatLngs; dragend (or any
// discrete op) triggers the rebuild. clearLayers() during a drag destroys
// the dragged marker and kills the gesture.
function render(s) {
  s.group.clearLayers();
  s.line = null;
  // dimmed raw capture line (already [lat,lon]) — reference, not editable
  L.polyline(s.src, { color: '#7c3aed', weight: 2, dashArray: '2 7', opacity: 0.4 })
    .bindTooltip('src (raw)').addTo(s.group);
  // the line being edited (the effective line, possibly already moved off src)
  s.line = L.polyline(s.pts, { color: '#f59e0b', weight: 4, opacity: 0.95 }).addTo(s.group);
  s.pts.forEach((p, i) => {
    const m = L.marker(p, { icon: VTX('#f59e0b'), draggable: true }).addTo(s.group);
    m.on('drag', (e) => {
      s.pts[i] = [e.latlng.lat, e.latlng.lng];
      s.line.setLatLngs(s.pts);
    });
    m.on('dragend', () => render(s));
    m.on('dblclick', () => {
      if (s.pts.length > 2) { s.pts.splice(i, 1); render(s); }
    });
  });
  // midpoint insert handles
  for (let i = 0; i < s.pts.length - 1; i++) {
    const mid = [(s.pts[i][0] + s.pts[i + 1][0]) / 2, (s.pts[i][1] + s.pts[i + 1][1]) / 2];
    L.marker(mid, {
      icon: L.divIcon({
        className: '',
        html: '<div style="width:8px;height:8px;background:#fff;border:2px solid #f59e0b;border-radius:50%;"></div>',
        iconSize: [8, 8], iconAnchor: [4, 4],
      }),
    }).addTo(s.group).on('click', () => {
      s.pts.splice(i + 1, 0, mid);
      render(s);
    });
  }
}

export function startLineEdit(routeId, dir, xno, raw, onSaved) {
  const coords = raw?.path?.geometry?.coordinates ?? [];
  const eff = raw?.effective;
  if (coords.length < 2 || (eff?.points?.length ?? 0) < 2) return;
  stopLineEdit();
  const map = window.__rmMap;
  const src = coords.map(([lon, lat]) => [lat, lon]); // GeoJSON lon-first → bus [lat,lon], once
  const group = L.layerGroup().addTo(map);
  map.doubleClickZoom.disable();
  session = {
    map, group, line: null, src, routeId, dir, xno,
    // edit the effective line; echo its token only when it IS an override
    // (editing a src line creates one — there is no row to race yet)
    pts: eff.points.map((p) => [p[0], p[1]]),
    expectedUpdatedAt: eff.source === 'override' ? eff.updated_at : null,
    btn: null, onSaved,
  };
  const btn = document.createElement('div');
  btn.style.cssText = 'position:absolute;bottom:56px;right:12px;z-index:1000;display:flex;gap:6px;';
  btn.innerHTML = `
    <button id="line-save" style="padding:4px 10px;cursor:pointer;">✓ 存線</button>
    <button id="line-cancel" style="padding:4px 10px;cursor:pointer;">✕</button>`;
  document.body.appendChild(btn);
  session.btn = btn;
  btn.querySelector('#line-cancel').onclick = () => stopLineEdit();
  btn.querySelector('#line-save').onclick = () => save(session);
  render(session);
}

async function save(s) {
  if (s.pts.length < 2) return;
  const note = prompt('line note:');
  if (note == null) return; // cancelled
  try {
    const out = await editPolylineLine(s.xno, s.dir, {
      points: s.pts,
      expected_updated_at: s.expectedUpdatedAt ?? undefined,
      note,
    });
    const done = s.onSaved;
    stopLineEdit();
    done?.(out);
  } catch (e) {
    s.onSaved?.(null, e);
  }
}
```

Delete `crates/rm-web/assets/js/pieceEdit.js`.

- [ ] **Step 3: layers.js — drawRaw effective + setEffectiveOn**

In `crates/rm-web/assets/js/layers.js`, replace the `drawRaw` block (the doc comment through the end of the function, lines ~129-159):

```js
/// Raw staging compare: dashed blue src_route_point + square raw stops.
/// Manual pieces draw orange; clicking one hands it to onEditPiece.
export function drawRaw(raw, { onEditPiece } = {}) {
  layers.raw.clearLayers();
  if (raw?.path?.geometry) {
    L.geoJSON(raw.path, {
      style: { color: '#7c3aed', weight: 3, dashArray: '2 7', opacity: 0.8 },
    })
      .bindTooltip('src_route_point (raw)')
      .addTo(layers.raw);
  }
  for (const s of raw?.stops ?? []) {
    L.rectangle(
      [
        [s.lat - 0.00012, s.lon - 0.00012],
        [s.lat + 0.00012, s.lon + 0.00012],
      ],
      { color: '#7c3aed', weight: 1.5, fillOpacity: 0.35 }
    )
      .bindTooltip(`${s.source_order_no}. ${s.station_name} (node ${s.node_id})`, { direction: 'top' })
      .addTo(layers.raw);
  }
  for (const p of raw?.manual_pieces ?? []) {
    const line = L.polyline(p.points, {
      color: '#f59e0b', weight: 4, opacity: 0.95,
    })
      .bindTooltip(`manual piece #${p.piece_id}`, { direction: 'top' })
      .addTo(layers.raw);
    if (onEditPiece) line.on('click', () => onEditPiece(p));
  }
}
```

with:

```js
/// Effective-line visibility (layer-effective checkbox; app.js calls this).
let effectiveOn = true;

export function setEffectiveOn(v) {
  effectiveOn = v;
}

/// Raw staging compare: dashed purple src_route_point + square raw stops.
/// The effective line (override ?? src — what match consumes) draws solid
/// orange; clicking it hands off to onEditLine. Gated by layer-effective.
export function drawRaw(raw, { onEditLine } = {}) {
  layers.raw.clearLayers();
  if (raw?.path?.geometry) {
    L.geoJSON(raw.path, {
      style: { color: '#7c3aed', weight: 3, dashArray: '2 7', opacity: 0.8 },
    })
      .bindTooltip('src_route_point (raw)')
      .addTo(layers.raw);
  }
  for (const s of raw?.stops ?? []) {
    L.rectangle(
      [
        [s.lat - 0.00012, s.lon - 0.00012],
        [s.lat + 0.00012, s.lon + 0.00012],
      ],
      { color: '#7c3aed', weight: 1.5, fillOpacity: 0.35 }
    )
      .bindTooltip(`${s.source_order_no}. ${s.station_name} (node ${s.node_id})`, { direction: 'top' })
      .addTo(layers.raw);
  }
  const eff = raw?.effective;
  if (effectiveOn && (eff?.points?.length ?? 0) >= 2) {
    const label = eff.source === 'override'
      ? `effective (override ${eff.updated_at})`
      : 'effective (= src)';
    const line = L.polyline(eff.points, {
      color: '#f59e0b', weight: 4, opacity: 0.95,
    })
      .bindTooltip(label, { direction: 'top' })
      .addTo(layers.raw);
    if (onEditLine) line.on('click', () => onEditLine());
  }
}
```

(`drawPieceHighlight`/`clearPieceHighlight` — the matched-way highlight — are a different "piece" concept; do not touch them.)

- [ ] **Step 4: panels.js — changeText override branch**

In `crates/rm-web/assets/js/panels.js`, in `changeText`, replace:

```js
  if (c.kind === 'polyline_piece') {
    parts.push(`線 #${c.piece_id} ${c.xno}/${c.direction}${c.encoded?.old != null ? ' 改' : ' 新增'}`);
  }
```

with:

```js
  if (c.kind === 'polyline_override') {
    parts.push(`線 ${c.xno}/${c.direction}${c.encoded?.new == null ? ' 刪' : ' 改'}`);
  } else if (c.kind === 'polyline_piece') {
    // legacy phase-1 history rows (append-only) keep rendering
    parts.push(`線 #${c.piece_id} ${c.xno}/${c.direction}${c.encoded?.old != null ? ' 改' : ' 新增'}`);
  }
```

- [ ] **Step 5: index.html — layer-effective checkbox**

In `crates/rm-web/assets/index.html`, after the layer-raw label (line 42), add:

```html
        <label><input type="checkbox" id="layer-effective" checked> Effective line <small>(what match consumes)</small></label>
```

- [ ] **Step 6: app.js — rewire**

In `crates/rm-web/assets/js/app.js`:

1. Line 5 import:

```js
import { startLineEdit, stopLineEdit, lineEditActive } from './lineEdit.js';
```

and line 4 gains `setEffectiveOn` in the layers import (after `setBearingsOn`).

2. Lines 45-49 state block:

```js
let lastRaw = null; // { routeId, dir, v } — for line-edit entry
let editMode = false;
let pendingPlace = null; // armed move-place map handler — replaced/cancelled, never stacked
let lineBtn = null; // 修線 + 線史 — created in boot() when edit is enabled
let lineLogBtn = null;
```

3. Lines 53-64 (sync + save handler), replaced:

```js
/// Shared visibility gate for the line-edit entries: edit mode + raw layer
/// + a raw payload on screen. Also tears an armed session down with edit-off.
function syncLineBtn() {
  const show = editMode && document.getElementById('layer-raw').checked && lastRaw ? '' : 'none';
  if (lineBtn) lineBtn.style.display = show;
  if (lineLogBtn) lineLogBtn.style.display = show;
  if (!editMode) stopLineEdit();
}

function onLineSaved(out, err) {
  if (err) { status(`line save failed: ${err.message}`); return; }
  status(out.created ? 'line override created' : 'line override updated');
  if (lastRaw && document.getElementById('layer-raw').checked) loadRaw(lastRaw.routeId, lastRaw.dir);
}

/// Re-draw the raw layer from the cached payload (raw/effective toggle
/// redraws share this closure).
function redrawRaw() {
  drawRaw(lastRaw.v, {
    onEditLine: () => {
      if (!editMode || lineEditActive()) return;
      startLineEdit(lastRaw.routeId, lastRaw.dir, selectedRoute.xno, lastRaw.v, onLineSaved);
    },
  });
}
```

4. `loadRaw` body (lines ~226-237) becomes:

```js
    const v = await api.routeRaw(routeId, dir);
    if (selectedRoute?.route_id !== routeId || selectedRoute.dir !== dir) return; // stale
    lastRaw = { routeId, dir, v };
    redrawRaw();
    syncLineBtn(); // entries may have been waiting for this payload
```

5. `openPieceEditLog` → rename `openLineLog` with pane key and status message updated:

```js
/// Edit-history panel for the selected route's line overrides (線史):
/// polyline edit rows key on route_id (entity='polyline', entity_id=route).
/// ↩ deletes the override (undo of its create) or restores the prior line.
async function openLineLog(routeId) {
  busy(true);
  try {
    const v = await api.editLog('polyline', routeId);
    paneOpen('context', `linelog:${routeId}`, () => openLineLog(routeId));
    editLogPanel(v.rows ?? [], {
      canUndo: editMode,
      onUndo: (editId) => undoLineEdit(routeId, editId),
    });
  } catch (e) {
    status(`line edit log load failed: ${e.message}`);
  } finally {
    busy(false);
  }
}
```

6. `undoPieceEdit` → rename `undoLineEdit`:

```js
/// Undo one logged line edit. The polyline branch of /api/edit/undo replies
/// {edit_id, restored: {deleted}} — deleted means the override's create was
/// reverted (the row is gone, back to pure src), otherwise the prior line
/// is back. Refresh = raw re-fetch (the effective line may have moved),
/// then the log for the fresh rows.
async function undoLineEdit(routeId, editId) {
  if (!editMode) return;
  if (!confirm(`undo edit #${editId} on route #${routeId} line?`)) return;
  const note = prompt('note (required):', '') ?? '';
  if (!note) return;
  try {
    const r = await api.undoEdit(editId, note);
    status(r.restored?.deleted ? 'line override deleted (back to src)' : 'line override restored');
    if (lastRaw && document.getElementById('layer-raw').checked) loadRaw(lastRaw.routeId, lastRaw.dir);
    await openLineLog(routeId); // fresh rows — the undo row is now newest
  } catch (e) {
    editFail('undo', e);
  }
}
```

7. Boot block (lines ~786-812), replaced:

```js
      // 修線 entry + 線史 (line edit log): visible only with edit mode +
      // raw layer + a raw payload up (syncLineBtn).
      lineBtn = document.createElement('button');
      lineBtn.id = 'line-edit-btn';
      lineBtn.textContent = '修線';
      lineBtn.style.cssText = 'display:none;margin-left:6px;';
      lineBtn.addEventListener('click', () => {
        if (!lastRaw || lineEditActive()) return;
        startLineEdit(lastRaw.routeId, lastRaw.dir, selectedRoute.xno, lastRaw.v, onLineSaved);
      });
      btn.parentElement.appendChild(lineBtn);
      lineLogBtn = document.createElement('button');
      lineLogBtn.id = 'line-log-btn';
      lineLogBtn.textContent = '線史';
      lineLogBtn.title = 'line edit history';
      lineLogBtn.style.cssText = 'display:none;margin-left:6px;';
      lineLogBtn.addEventListener('click', () => {
        if (!lastRaw) return;
        openLineLog(lastRaw.routeId);
      });
      btn.parentElement.appendChild(lineLogBtn);
      document.getElementById('layer-raw').addEventListener('change', syncLineBtn);
      const effToggle = document.getElementById('layer-effective');
      effToggle.addEventListener('change', () => {
        setEffectiveOn(effToggle.checked);
        if (lastRaw && document.getElementById('layer-raw').checked) redrawRaw();
      });
      btn.addEventListener('click', () => {
        if (editMode) disableEditMode();
        else enableEditMode();
        syncLineBtn();
      });
```

- [ ] **Step 7: assets.rs + mod.rs — bake the renamed file**

In `crates/rm-web/src/handlers/assets.rs`, replace the pieceEdit arm:

```rust
        "/js/lineEdit.js" => asset(
            "application/javascript",
            include_str!("../../assets/js/lineEdit.js"),
        ),
```

In `crates/rm-web/src/handlers/mod.rs`, replace the route:

```rust
        .route("/js/lineEdit.js", get(assets::serve))
```

- [ ] **Step 8: Build and smoke-test**

Run: `cargo build -r -p rm-web 2>&1 | tail -3` — expected: compiles (assets are `include_str!`, so renames fail the build if a path is wrong).

Run: `grep -rn "pieceEdit\|startPieceEdit\|editPolylinePiece\|onEditPiece\|manual_pieces" crates/rm-web/assets/` — expected: no matches (pathPiecePanel/drawPieceHighlight are different "piece" concepts and stay).

- [ ] **Step 9: Commit**

```bash
git add crates/rm-web/assets crates/rm-web/src/handlers
git commit -m "rm-web UI: whole-line override editing — lineEdit.js, effective layer + toggle, 修線/線史 rewired

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 8: Retire the piece model (migration DROP + code deletion)

**Files:**
- Modify: `crates/rm-db/migrations/0013_polyline_override.sql` (add the DROP)
- Modify: `crates/rm-db/src/edit.rs` (piece region, EditError variants, piece tests)
- Modify: `crates/rm-edit/src/lib.rs`, `crates/rm-edit/src/rename.rs:52` (EditError)
- Delete: `crates/rm-edit/src/piece.rs`, `crates/rm-edit/tests/piece.rs`
- Modify: `crates/rm-web/src/handlers/edit.rs` (edit_conflict piece arms, PolylinePieceBody, edit_polyline_piece, stale doc comments)

**Interfaces:**
- Consumes: Tasks 1-7 all shipped; every piece consumer swapped. This task deletes the old model — the workspace must be green at each step's end.
- Produces: zero `polyline_piece` code references in Rust; migration 0013 drops the table.

- [ ] **Step 1: Migration gains the DROP**

In `crates/rm-db/migrations/0013_polyline_override.sql`, append (it ran WITHOUT this line in Task 1 — phase-1 code was still alive then; this branch is the only place it ships):

```sql

DROP TABLE polyline_piece;
```

Also append to the `migration_0013_creates_override_table_and_unique_index` test in `crates/rm-db/src/migrate.rs` (or wherever Task 1 put it):

```rust
    let gone: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'polyline_piece'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(gone, 0, "0013 drops the piece table");
```

- [ ] **Step 2: rm-db — delete the piece region and its errors**

In `crates/rm-db/src/edit.rs`:

1. Delete the `EditError` variants (lines ~21-24):

```rust
    #[error("piece {0} was edited by someone else — reload and retry")]
    PieceNotCurrent(i64),
    #[error("piece {0} exists under a different route/direction")]
    PieceScope(i64),
```

2. Delete the piece region: `PieceRow`, `load_polyline_snapshot` (+ `PolylineSnapshot`), `PieceEditOp`, `PieceApplied`, `apply_piece_edit`, `PieceRestoreState`, `PieceUndoOp`, `PieceUndone`, `apply_piece_undo` (lines ~940-1363).

   **KEEP**: `PolylineEditRow` + `load_polyline_edit` (the edit-log read the 線史 panel and undo planning use) and `src_fixture`/`fixture` in the tests mod (Task 2's line tests reuse them).

3. Delete from the tests mod: the `piece_op` helper (~1644), the `piece_undo_op` helper (~2762), and the ten piece tests: `apply_piece_edit_creates_row_and_log`, `apply_piece_edit_updates_existing_and_logs_old`, `apply_piece_edit_refuses_stale_expectation_zero_partial`, `apply_piece_edit_refuses_foreign_scope_piece_id`, `apply_piece_edit_recreates_deleted_piece_with_matching_expectation_absent`, `apply_piece_edit_explicit_id_advances_id_sequence`, `load_polyline_snapshot_resolves_route_src_and_pieces`, `apply_piece_undo_of_create_deletes_row`, `apply_piece_undo_of_update_restores_old_state`, `apply_piece_undo_refuses_after_second_edit_zero_partial`.

- [ ] **Step 3: rm-edit — delete piece.rs and its enum arms**

1. `git rm crates/rm-edit/src/piece.rs crates/rm-edit/tests/piece.rs`
2. In `crates/rm-edit/src/lib.rs`: delete `mod piece;` (line 4), `pub use piece::*;` (line 10), and the two `map_db_err` arms:

```rust
        Some(rm_db::EditError::PieceNotCurrent(id)) => EditError::PieceNotCurrent(*id).into(),
        Some(rm_db::EditError::PieceScope(id)) => EditError::PieceScope(*id).into(),
```

3. In `crates/rm-edit/src/rename.rs` (the rm-edit `EditError`, ~line 52): delete the variants `InvalidPiece(String)`, `PieceNotCurrent(i64)`, `PieceOverlap(i64)`, `PieceScope(i64)` with their `#[error(...)]` attributes.

- [ ] **Step 4: rm-web — delete the piece handler surface**

In `crates/rm-web/src/handlers/edit.rs`:

1. `edit_conflict`: delete the four piece arms — `rm_edit::EditError::InvalidPiece(_) | rm_edit::EditError::PieceScope(_)` from the 400 list, `rm_edit::EditError::PieceNotCurrent(_) | rm_edit::EditError::PieceOverlap(_)` from the 409 list — and drop "invalid piece"/"piece overlap" from the doc comment.
2. `PolylinePieceBody` + `edit_polyline_piece` are ALREADY gone — Task 4 deleted them when it swapped the route (nothing may reference the piece handler). Verify via the Step 5 grep; do not re-delete here.
3. Sweep the doc comments that still say "piece" for the polyline flow (the undo handler's doc, the module-level 400/409 comment) to say "line override".

- [ ] **Step 5: Verify the retirement is total, workspace green**

Run: `grep -rn "polyline_piece\|PolylinePiece\|pieceEdit\|PieceEditCmd\|plan_piece_edit\|edit_polyline_piece\|undo_polyline_piece" crates/ --include='*.rs' --include='*.js' --include='*.html'`
Expected: no matches. (`pathPiecePanel`/`drawPieceHighlight` in JS are matched-way highlight pieces — different concept, stays; `grep -rn "pathPiecePanel" crates/` confirms they are the only surviving "piece" tokens.)

Run: `cargo test -r --workspace 2>&1 | tail -8`
Expected: PASS — all crates green with the piece model gone.

- [ ] **Step 6: Commit**

```bash
git add -A crates/
git commit -m "retire polyline piece model: 0013 drops table, piece code deleted across rm-db/rm-edit/rm-web

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 9: README runbook + e2e drive on a taipei.db copy

**Files:**
- Modify: `README.md:57-59` (post-edit refresh block)

**Interfaces:**
- Consumes: everything shipped in Tasks 1-8. No code changes here.

- [ ] **Step 1: README runbook gains the match line**

In `README.md`, replace:

```sh
# Post-edit refresh (after web edits — see docs/web-edit-mode.md)
cargo run -r -p rm-segments -- --stale  # reproject routes marked stale by web edits
cargo run -r -p rm-registry -- --stale  # rebuild bearings + split for marked-stale, clear marks
```

with:

```sh
# Post-edit refresh (after web edits — see docs/web-edit-mode.md; marks
# accumulate across edits, one pass processes all)
cargo run -r -p rm-osm-match -- --stale # re-match line overrides marked stale
cargo run -r -p rm-segments -- --stale  # reproject routes marked stale by web edits
cargo run -r -p rm-registry -- --stale  # rebuild bearings + split for marked-stale, clear marks
```

- [ ] **Step 2: E2E drive on a COPY (never the real DB)**

Run each command; record the observed result in the task report.

1. `cp data/taipei.db /tmp/taipei-e2e.db`
2. `cargo run -r -p rm-web -- --db /tmp/taipei-e2e.db --edit --edit-tokens herry:devtoken` — expected: serves on 127.0.0.1:8787, edit toggle visible. **Release build** (house rule).
3. Browser: pick a route with ≥2 directions → raw layer ON → edit mode ON → 修線 → drag 2-3 vertices → 存線 → note. Expected: status "line override created"; orange line moved; effective tooltip shows `override <ts>`; layer-effective OFF hides it, ON restores.
4. `curl -s -H "Authorization: Bearer devtoken" http://127.0.0.1:8787/api/edit/log?entity=polyline\&entity_id=<route_id>` — expected: newest row `action=create`, `kind=polyline_override`, changes carry `encoded.new`.
5. `cargo run -r -p rm-osm-match -- --db /tmp/taipei-e2e.db --stale` — expected: `stale: 1 directions, 1 matched, 0 failed, 1 marks cleared, 0 kept`.
6. `cargo run -r -p rm-segments -- --db /tmp/taipei-e2e.db --stale` — expected: only the edited route's direction recomputed.
7. `cargo run -r -p rm-registry -- --db /tmp/taipei-e2e.db --stale` — expected: bearings rebuilt, marks cleared.
8. In the browser: compare raw (dashed purple) vs effective (solid orange) — only the edited direction's matched geometry changed; QA chips unchanged otherwise.
9. 線史 → ↩ on the create row → note. Expected: "line override deleted (back to src)"; orange line returns to src geometry.
10. Re-run step 5 — expected: `stale: 1 directions …` again (undo re-marks), then marks cleared.
11. `sqlite3 /tmp/taipei-e2e.db "SELECT COUNT(*) FROM polyline_override;"` — expected: `0` (undo of the create deleted the row).

- [ ] **Step 3: Full workspace gate**

Run: `cargo test -r --workspace 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add README.md
git commit -m "README: post-edit refresh gains rm-osm-match --stale

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```
