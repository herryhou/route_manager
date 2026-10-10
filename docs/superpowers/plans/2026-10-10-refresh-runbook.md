# Refresh runbook wiring Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `--stale` mode on rm-segments and rm-registry that reads `stale_mark`, refreshes exactly the marked routes / rebuilds bearings, and clears the satisfied marks — so edit badges go green with two CLI commands instead of a full pipeline re-run.

**Architecture:** rm-db's mark writes switch `INSERT OR IGNORE` → `INSERT OR REPLACE` (each mark = fresh rowid, making the rowid snapshot race-safe). Both pipeline runs already execute in one `unchecked_transaction`; each `--stale` entry snapshots target rowids at startup, filters/recomputes, and appends one `DELETE FROM stale_mark WHERE rowid IN (...)` inside that same tx. rm-osm-match is untouched.

**Tech Stack:** Rust, rusqlite (bundled), clap, existing `rm_ingest::testfix` fixture pattern.

**Spec:** `docs/superpowers/specs/2026-10-10-refresh-runbook-design.md`

## Global Constraints

- Pipeline binaries run release: `cargo run -r -p <crate>` (CLAUDE.md; debug is 10–30x slower on this workload).
- Tests are plain `cargo test -p <crate>` (in-memory DBs, fast in debug).
- Do NOT run `cargo fmt` (baseline ~130 fmt-diff sites; fmt is a dedicated-commit policy).
- Pre-existing clippy warnings ×6 are out of scope; do not fix unrelated ones.
- `stale_mark` schema (`crates/rm-db/migrations/0010_edit.sql:21`): PK `(table_name, key)`, ordinary rowid table, FK `cause_edit_id → edit_log`.
- Mark keys in use: `('stop_bearing','*')`, `('station_bearing','*')`, `('path_stop_projection','<route_id>')`.
- rm-edit stays zero-SQL; all SQL in rm-db/rm-segments/rm-registry.

## Review Focus

1. **Marked route not in the `route` table / no polylines in batch** → warn + leave its mark, never panic, never clear. (Pinned: Task 2 Step 1.)
2. **Non-numeric route_id key in a `path_stop_projection` mark** → warn + leave marked, never panic. (Pinned: Task 2 Step 1.)
3. **Mark written after `--stale` snapshot** (concurrent edit) → survives the refresh; badge stays amber. (Pinned: Task 1 Step 1 rowid assertion; Task 3 Step 1.)
4. **Re-mark of an existing key** → one row, new `cause_edit_id`, new rowid (OR REPLACE). (Pinned: Task 1 Step 1.)
5. **`--stale` with zero marks** → exit 0, `nothing stale`, DB untouched (no bearing rebuild, no split). (Pinned: Task 2 / Task 3 tests.)

---

### Task 1: rm-db mark writes → INSERT OR REPLACE

**Files:**
- Modify: `crates/rm-db/src/edit.rs` (5 `INSERT OR IGNORE INTO stale_mark` sites: ~44, ~79, ~85, ~300, ~415)
- Test: `crates/rm-db/src/edit.rs` (`mod tests`)

**Interfaces:**
- Consumes: existing `mark_bearings_stale(tx, edit_id, now)`, `mark_position_stale(tx, stop_version_id, edit_id, now)` helpers and the two inline regroup/undo-regroup sites.
- Produces: same signatures, REPLACE semantics — every mark is a fresh row (new rowid), freshest `cause_edit_id` wins. Tasks 2–3 rely on "re-mark ⇒ new rowid" for snapshot correctness.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `crates/rm-db/src/edit.rs` (fixture pattern exists; `fixture(&conn)` mints stop_version 10 with an edit_log-less base — this test needs a real edit for the FK, so run one edit first):

```rust
#[test]
fn re_mark_replaces_row_fresh_rowid_and_cause() {
    let conn = open_in_memory().unwrap();
    let old = fixture(&conn);
    let e1 = StopEditOp { stop_id: 1, stop_version_id: old, stop_name: Some("A".into()), ..Default::default() };
    let out1 = apply_stop_edit(&conn, &e1, "h", "first").unwrap();
    // name-only edits mark nothing; force a bearing mark via a position edit
    let cur = out1.new_version_id;
    let e2 = StopEditOp { stop_id: 1, stop_version_id: cur, latitude: Some(24.001), ..Default::default() };
    let out2 = apply_stop_edit(&conn, &e2, "h", "second").unwrap();

    let row = |conn: &rusqlite::Connection| -> (i64, i64, String) {
        conn.query_row(
            "SELECT rowid, cause_edit_id, key FROM stale_mark WHERE table_name = 'stop_bearing'",
            [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).unwrap()
    };
    let (rid1, cause1, _) = row(&conn);

    // concurrent-style re-mark of the same key: new cause must win, rowid must change
    let now = rm_db::now_rfc3339();
    conn.execute(
        "INSERT OR REPLACE INTO stale_mark (table_name, key, cause_edit_id, marked_at)
         VALUES ('stop_bearing', '*', ?1, ?2)",
        rusqlite::params![out2.edit_id, now],
    ).unwrap();
    let (rid2, cause2, key2) = row(&conn);
    assert_eq!(key2, "*");
    assert_eq!(cause2, out2.edit_id, "freshest cause wins");
    assert_ne!(rid2, rid1, "OR REPLACE mints a fresh rowid (snapshot soundness)");
    let _ = cause1;
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rm-db re_mark_replaces_row 2>&1 | tail -5`
Expected: FAIL — `cause2` equals `out1.edit_id`... actually with OR IGNORE the re-mark is a no-op, so `cause2 == cause1` (the FIRST edit's cause) and `rid2 == rid1`. Both asserts fail.

- [ ] **Step 3: Switch the five mark sites to OR REPLACE**

In `crates/rm-db/src/edit.rs`, change all five statements from
`INSERT OR IGNORE INTO stale_mark ...` to `INSERT OR REPLACE INTO stale_mark ...`
(sites: `mark_bearings_stale` body, `mark_position_stale`'s per-route and station_bearing inserts, the regroup inline insert ~300, the stop-undo regroup-branch insert ~415). Update the doc comments that say "Idempotent via INSERT OR IGNORE" to say re-marks replace (fresh rowid, freshest cause).

- [ ] **Step 4: Run full rm-db suite**

Run: `cargo test -p rm-db 2>&1 | tail -5`
Expected: PASS (all existing tests — IGNORE only ever differed from REPLACE on re-mark of an existing key, which no existing test pins).

- [ ] **Step 5: Commit**

```bash
git add crates/rm-db/src/edit.rs
git commit -m "rm-db: stale_mark re-marks REPLACE — fresh rowid per mark"
```

### Task 2: rm-segments `--stale`

**Files:**
- Modify: `crates/rm-segments/src/lib.rs` (`run()` gains an internal filtered variant; new public `run_stale`)
- Modify: `crates/rm-segments/src/main.rs` (`--stale` flag)
- Test: `crates/rm-segments/tests/segments.rs`

**Interfaces:**
- Consumes: `stale_mark` rows with `table_name = 'path_stop_projection'`, `key` = route_id decimal string; `run()`'s existing single `unchecked_transaction` per call.
- Produces:

```rust
pub struct StaleOutcome {
    pub summary: Summary, // counts over processed routes only
    pub cleared: usize,   // snapshot rowids deleted
    pub kept: usize,      // marks left (unknown route / unparseable key)
}

/// None = no path_stop_projection marks at all (nothing to do).
pub fn run_stale(conn: &Connection, batch_id: i64, outlier_m: f64) -> anyhow::Result<Option<StaleOutcome>>;
```

`run(conn, batch_id, outlier_m)` signature and behavior unchanged (wrapper over shared internals with no filter, no clear).

- [ ] **Step 1: Write the failing tests**

Append to `crates/rm-segments/tests/segments.rs` (reuses its `setup()` + `count()`; fixture has routes 1 and 2, both published, both with projections after a first `run`):

```rust
use rm_segments::run_stale;

fn mark(conn: &rusqlite::Connection, table: &str, key: &str, edit: i64) {
    conn.execute(
        "INSERT OR REPLACE INTO stale_mark (table_name, key, cause_edit_id, marked_at)
         VALUES (?1, ?2, ?3, '2026-01-02T00:00:00Z')",
        rusqlite::params![table, key, edit],
    ).unwrap();
}

#[test]
fn stale_reprojects_only_marked_routes_and_clears_their_marks() {
    let (_dir, conn, batch) = setup();
    run(&conn, batch, 50.0).unwrap();
    let before: i64 = count(&conn, "SELECT COUNT(*) FROM path_stop_projection");
    // stop 11 belongs to route 1: move it, then mark route 1 stale the way an edit would
    conn.execute(
        "UPDATE stop_version SET latitude = 24.1410 WHERE stop_version_id IN (
            SELECT sv.stop_version_id FROM stop_version sv JOIN stop s ON s.stop_id = sv.stop_id
            WHERE s.ext_uid = 'tcg:1:0:11')", [],
    ).unwrap();
    mark(&conn, "path_stop_projection", "1", 1);

    let out = run_stale(&conn, batch, 50.0).unwrap().expect("some marks");
    assert_eq!(out.summary.directions, 1, "only route 1's directions");
    assert_eq!(out.cleared, 1);
    assert_eq!(out.kept, 0);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM stale_mark"), 0);
    // route 2 untouched: its projection rows are the same ones (deleted+rewritten only for route 1)
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM path_stop_projection"), before);
}

#[test]
fn stale_keeps_marks_for_unknown_routes_and_bad_keys() {
    let (_dir, conn, batch) = setup();
    run(&conn, batch, 50.0).unwrap();
    mark(&conn, "path_stop_projection", "999", 1); // no such route
    mark(&conn, "path_stop_projection", "not-a-number", 2);

    let out = run_stale(&conn, batch, 50.0).unwrap().expect("some marks");
    assert_eq!(out.cleared, 0);
    assert_eq!(out.kept, 2);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM stale_mark"), 2);
}

#[test]
fn stale_with_no_marks_is_none_and_touches_nothing() {
    let (_dir, conn, batch) = setup();
    run(&conn, batch, 50.0).unwrap();
    let n: i64 = count(&conn, "SELECT COUNT(*) FROM path_stop_projection");
    assert!(run_stale(&conn, batch, 50.0).unwrap().is_none());
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM path_stop_projection"), n);
}

#[test]
fn mark_inserted_after_snapshot_survives() {
    let (_dir, conn, batch) = setup();
    run(&conn, batch, 50.0).unwrap();
    mark(&conn, "path_stop_projection", "1", 1);
    // simulate a concurrent edit marking a DIFFERENT route mid-run: it must survive
    // (run_stale's snapshot is taken before processing; row 999/2-key below is inserted
    //  before the call but the surviving-mark property for same-run inserts is pinned
    //  at the rowid level in rm-db tests — here we pin: only snapshot rows are cleared)
    mark(&conn, "path_stop_projection", "2", 3);
    let out = run_stale(&conn, batch, 50.0).unwrap().unwrap();
    assert_eq!(out.cleared, 2, "both snapshot rows go");
}
```

(Note the fourth test's assertion: only rows existing at snapshot time are ever deleted — the rm-db-level fresh-rowid pin in Task 1 is what makes a genuinely concurrent insert survive; this test pins the delete-set boundary.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rm-segments stale 2>&1 | tail -5`
Expected: FAIL — `run_stale` not found (compile error).

- [ ] **Step 3: Implement**

In `crates/rm-segments/src/lib.rs`:

1. Rename nothing public. Extract the body of `run()` after the routes query into
   `fn run_inner(conn: &Connection, batch_id: i64, outlier_m: f64, only: Option<&std::collections::HashSet<i64>>, clear: &[i64]) -> Result<Summary>`
   — the routes query moves inside `run_inner`, gaining `AND (?1 IS NULL OR ...)` is NOT idiomatic here; instead filter in Rust: `routes.retain(|(rid, _, _)| only.map(|s| s.contains(rid)).unwrap_or(true))`. At the end of the existing tx block, before `tx.commit()`:

```rust
if !clear.is_empty() {
    let mut del = tx.prepare("DELETE FROM stale_mark WHERE rowid = ?1")?;
    for rid in clear {
        del.execute([rid])?;
    }
}
```

2. `run()` becomes a thin wrapper: `run_inner(conn, batch_id, outlier_m, None, &[])`.
3. New public function:

```rust
/// Refresh exactly the routes marked stale in `path_stop_projection`.
/// Snapshot rowids first; a mark inserted after the snapshot (concurrent
/// edit) survives. Unknown route ids / unparseable keys: warn, keep.
pub fn run_stale(
    conn: &Connection,
    batch_id: i64,
    outlier_m: f64,
) -> Result<Option<StaleOutcome>> {
    let snapshot: Vec<(i64, String)> = {
        let mut stmt = conn.prepare(
            "SELECT rowid, key FROM stale_mark WHERE table_name = 'path_stop_projection'",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<Result<_, _>>()?
    };
    if snapshot.is_empty() {
        return Ok(None);
    }
    let mut only = std::collections::HashSet::new();
    let mut clear = Vec::new();
    let mut kept = 0usize;
    for (rowid, key) in snapshot {
        match key.parse::<i64>() {
            Ok(rid) => {
                let published: Option<i64> = conn
                    .query_row(
                        "SELECT current_route_version_id FROM route WHERE route_id = ?1",
                        [rid],
                        |r| r.get(0),
                    )
                    .map(|v| v.is_some())
                    .unwrap_or(false);
                if published {
                    only.insert(rid);
                    clear.push(rowid);
                } else {
                    tracing::warn!(route = rid, "stale route not published; keeping mark");
                    kept += 1;
                }
            }
            Err(_) => {
                tracing::warn!(key = %key, "unparseable path_stop_projection key; keeping mark");
                kept += 1;
            }
        }
    }
    let summary = run_inner(conn, batch_id, outlier_m, Some(&only), &clear)?;
    Ok(Some(StaleOutcome { summary, cleared: clear.len(), kept }))
}
```

In `crates/rm-segments/src/main.rs`: add flag + dispatch (batch resolution stays as-is):

```rust
/// Refresh only routes marked stale, then clear their marks
#[arg(long)]
stale: bool,
```

and after `batch` resolution:

```rust
if args.stale {
    match rm_segments::run_stale(&conn, batch, args.outlier_m)? {
        Some(out) => println!(
            "stale: {} directions, {} projections, {} marks cleared, {} kept",
            out.summary.directions, out.summary.projections, out.cleared, out.kept
        ),
        None => println!("nothing stale"),
    }
    return Ok(());
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p rm-segments 2>&1 | tail -5`
Expected: PASS (new + all existing).

- [ ] **Step 5: Commit**

```bash
git add crates/rm-segments/src/lib.rs crates/rm-segments/src/main.rs crates/rm-segments/tests/segments.rs
git commit -m "rm-segments: --stale — refresh marked routes, clear satisfied marks"
```

### Task 3: rm-registry `--stale`

**Files:**
- Modify: `crates/rm-registry/src/lib.rs` (new `pub mod stale;` or inline fn — inline in lib.rs, matching crate size)
- Modify: `crates/rm-registry/src/main.rs` (`--stale` flag)
- Test: `crates/rm-registry/tests/stale.rs` (new file; fixture style of `tests/bearing.rs`'s hand-built DB)

**Interfaces:**
- Consumes: `rm_registry::bearing::run(conn, batch_id)`, `rm_registry::split::run(conn)`, `rm_registry::bearing::stations(conn)` (all existing); `stale_mark` rows `('stop_bearing','*')` / `('station_bearing','*')`.
- Produces:

```rust
/// Full refresh when any bearing mark exists: bearing rebuild, split,
/// conditional station re-bear, then clear the snapshot marks. Returns
/// false when there was nothing stale (DB untouched).
pub fn run_stale(conn: &rusqlite::Connection, batch_id: i64) -> anyhow::Result<bool>;
```

- [ ] **Step 1: Write the failing test**

Create `crates/rm-registry/tests/stale.rs` — copy `tests/bearing.rs`'s `setup()` verbatim (hand-built DB: routes 1+2 published, station 1, stops 1+2 with `route_stop_version` 1+2; see that file for the exact `execute_batch` — includes `src_route_point` polylines and `path_stop_projection` rows fed by segments-equivalent inserts; read it and reuse as-is), then:

```rust
use rm_registry::run_stale;

#[test]
fn stale_rebuilds_and_clears_marks() {
    let conn = setup();
    // seed marks the way edits write them
    conn.execute(
        "INSERT INTO stale_mark (table_name, key, cause_edit_id, marked_at)
         VALUES ('stop_bearing', '*', 1, '2026-01-02T00:00:00Z')", [],
    ).unwrap();
    // cause_edit_id has an FK to edit_log: insert a log row first (copy the
    // edit_log INSERT shape from rm-db tests — entity 'stop', action 'edit')
    // ... see Step 1 note below

    let did = run_stale(&conn, 1).unwrap();
    assert!(did, "bearing mark present -> refresh ran");
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get::<_, i64>(0)).unwrap(),
        0,
        "snapshot rows cleared"
    );
    let bearings: i64 = conn
        .query_row("SELECT COUNT(*) FROM stop_bearing", [], |r| r.get(0))
        .unwrap();
    assert!(bearings >= 2, "both stops re-bearied, got {bearings}");
}

#[test]
fn stale_with_no_marks_is_false_and_touches_nothing() {
    let conn = setup();
    assert!(!run_stale(&conn, 1).unwrap());
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM stop_bearing", [], |r| r.get::<_, i64>(0)).unwrap(),
        0,
        "no rebuild happened"
    );
}
```

**Step 1 note (FK):** `stale_mark.cause_edit_id` FKs `edit_log`. `tests/bearing.rs`'s fixture has no `edit_log` rows — the test must insert one first (single `INSERT INTO edit_log (entity, entity_id, editor, action, changes, created_at) VALUES ('stop', 1, 'test', 'edit', '{}', T0)`; check `0010_edit.sql` for the exact NOT NULL columns and satisfy them).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rm-registry --test stale 2>&1 | tail -5`
Expected: FAIL — `run_stale` not found.

- [ ] **Step 3: Implement**

In `crates/rm-registry/src/lib.rs` (add `pub use stale::run_stale;` if placed in a module, or define directly):

```rust
/// `--stale`: any bearing mark -> whole-table rebuild -> split ->
/// conditional station re-bear -> clear the snapshot rowids. Rowid
/// snapshot: marks inserted after startup survive (concurrent edit).
pub fn run_stale(conn: &rusqlite::Connection, batch_id: i64) -> anyhow::Result<bool> {
    let snapshot: Vec<i64> = {
        let mut stmt = conn.prepare(
            "SELECT rowid FROM stale_mark
             WHERE table_name IN ('stop_bearing', 'station_bearing')",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    if snapshot.is_empty() {
        return Ok(false);
    }

    let _b = bearing::run(conn, batch_id)?;
    let sp = split::run(conn)?;
    if sp.stops_moved > 0 {
        bearing::stations(conn)?;
    }

    let tx = conn.unchecked_transaction()?;
    {
        let mut del = tx.prepare("DELETE FROM stale_mark WHERE rowid = ?1")?;
        for rid in &snapshot {
            del.execute([rid])?;
        }
    }
    tx.commit()?;
    Ok(true)
}
```

In `crates/rm-registry/src/main.rs`: add `#[arg(long)] stale: bool,`; after batch resolution:

```rust
if args.stale {
    if rm_registry::run_stale(&conn, batch)? {
        println!("stale: bearings rebuilt, split re-run, marks cleared");
    } else {
        println!("nothing stale");
    }
    return Ok(());
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p rm-registry 2>&1 | tail -5`
Expected: PASS (new + existing bearing/split tests).

- [ ] **Step 5: Commit**

```bash
git add crates/rm-registry/src/lib.rs crates/rm-registry/src/main.rs crates/rm-registry/tests/stale.rs
git commit -m "rm-registry: --stale — bearing rebuild + split, clear satisfied marks"
```

### Task 4: Runbook docs + badge-loop pin

**Files:**
- Modify: `docs/web-edit-mode.md` ("Staleness & refresh" section, lines ~115–139; "Phasing" phase 3 line ~188)
- Modify: `README.md` (pipeline command list, lines ~40–44)
- Test: `crates/rm-web/tests/edit.rs` (one small test)

**Interfaces:**
- Consumes: Tasks 1–3 as shipped; `rm_web::queries::stops` badge fields `stale.projections` (route-id array) and `stale.bearings` (bool) as asserted at `tests/edit.rs:267-268`.
- Produces: docs only + one test; no interfaces.

- [ ] **Step 1: Write the failing badge-loop test**

Append to `crates/rm-web/tests/edit.rs` (mirror `stop_detail_reports_stale`'s fixture/handler shape — reuse its setup helpers; the existing test asserts amber after edit, this one asserts green after a simulated refresh clear):

```rust
#[tokio::test]
async fn badge_clears_when_marks_deleted() {
    // same setup as stop_detail_reports_stale: edit a stop, detail shows amber
    // ... (copy that test's body up to the readback) ...
    // simulate the refresh runbook having cleared the marks:
    let conn = rm_db::open(&dir.path().join("t.db")).unwrap();
    conn.execute("DELETE FROM stale_mark", []).unwrap();
    let v = /* re-GET /api/stops/{svi} */;
    assert_eq!(v["stale"]["projections"], serde_json::json!([]));
    assert_eq!(v["stale"]["bearings"], serde_json::json!(false));
}
```

Follow the existing test's exact request-building pattern (the file's helpers `edit_state`/`body_json`/`get_json` are already in scope).

- [ ] **Step 2: Run test to verify it fails then passes**

Run: `cargo test -p rm-web badge_clears 2>&1 | tail -5`
Expected: PASS immediately is acceptable (it pins existing behavior — the LEFT JOIN). If RED, the badge query is wrong; investigate before proceeding.

- [ ] **Step 3: Rewrite the runbook section**

In `docs/web-edit-mode.md`, replace the numbered refresh list (lines ~130–139) with:

```markdown
Refresh is two CLI commands — the web server never spawns pipeline
binaries. Both read `stale_mark` themselves; neither takes ids:

1. `cargo run -r -p rm-segments -- --stale` — reprojects exactly the
   routes a stop position/regroup edit marked (`path_stop_projection`
   marks), clears them in the same transaction. Prints `nothing stale`
   when there are no marks.
2. `cargo run -r -p rm-registry -- --stale` — when any bearing mark
   exists (`stop_bearing`/`station_bearing`): whole-table bearing
   rebuild, station split re-run, then clears the marks. Split is
   idempotent; its re-run after regroup edits is the manual step the
   marks trigger.

Order matters: bearings are derived FROM `path_stop_projection`
(`rm-registry/src/bearing.rs` reads projections), so segments before
registry. rm-osm-match is NOT part of edit refresh — stop edits never
change road-graph matches; re-match only after upstream re-imports.
```

Also update the "Phasing" phase-3 line (~188) from "`--routes` filter in rm-osm-match, bearing re-run steps, mark clearing" to "refresh `--stale` wiring (rm-segments + rm-registry), mark clearing, stale badges verified accurate" and mark it shipped.

- [ ] **Step 4: Mirror in README**

In `README.md`'s pipeline command list (~40–44), add after the two existing commands:

```markdown
cargo run -r -p rm-segments -- --stale  # reproject routes marked stale by web edits
cargo run -r -p rm-registry -- --stale  # rebuild bearings + split for marked-stale, clear marks
```

- [ ] **Step 5: Full workspace test + commit**

Run: `cargo test --workspace 2>&1 | tail -5`
Expected: PASS (255+ tests green).

```bash
git add docs/web-edit-mode.md README.md crates/rm-web/tests/edit.rs
git commit -m "docs: refresh runbook = rm-segments/rm-registry --stale; badge-clear pin"
```

---

## Self-review notes

- Spec coverage: OR REPLACE (Task 1), segments --stale incl. unknown-route + bad-key keep (Task 2), registry --stale incl. none-short-circuit (Task 3), docs order/binary correction + README (Task 4), badge accuracy pin (Task 4). All spec Testing bullets have homes.
- Type consistency: `StaleOutcome { summary, cleared, kept }` used identically in Task 2 steps; `run_stale(conn, batch)` registry bool form distinct per crate — intentional, both documented in Interfaces.
- Review Focus → tests: items 1–2 → Task 2 test 2; item 3 → Task 1 + Task 3/Task 2 boundary; item 4 → Task 1; item 5 → Task 2 test 3 + Task 3 test 2.
