# rm-job Agent CLI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Expose the rm-db edit/read surface as an agent-friendly CLI: `schema` introspection, JSON envelopes, read commands (`find-stop`/`show-route`/`show-path`), write commands (`move-stop`/`remove-stop`/`extend-path`), plus a new rm-db detach primitive and the agent runbook/skill.

**Architecture:** rm-job grows an envelope layer (one serde struct wrapping every command's output) and schemars-driven param structs (single source for `schema` output and `--json` payload validation). Read commands are fresh SQL in rm-job mirroring rm-web's query shapes; write commands stay thin wrappers over `rm_db::edit` services. One new rm-db primitive (`detach`, version-chain style) and one rm-job-local algorithm module (`extend`) that walks the OSM graph from `RoadGraph::from_conn`.

**Tech Stack:** clap, schemars 1.x, serde/serde_json, rusqlite, rm-db, rm-osm-match (`RoadGraph`), rm-core (polyline codec, geo).

**Spec:** `docs/superpowers/specs/2026-10-17-rm-job-agent-cli-design.md` (decisions 1–10 are binding)

## Global Constraints

- rm-job runs release-mode whenever it touches the graph: `cargo run -r -p rm-job -- …` (CLAUDE.md incident rule).
- Dry-run is the default for every write command; `--apply` (or payload `"apply": true`) opts in per invocation (spec decision 2).
- Exit codes: 0 ok, 1 rejected, 2 no-op. Envelope `status`: `"ok"` / `"rejected"` / `"noop"` (spec decision 3).
- stdout carries ONLY the envelope JSON; human text goes to stderr. Never mix.
- DB writes deterministic: mint/insert order pinned to Vec order, never HashMap iteration (repo invariant).
- Read commands must probe `sqlite_master` for tables added by newer migrations and degrade, never error (repo /raw precedent).
- extend-path reads the OSM tables only — never deletes ways or slims the graph (repo clip rule).
- Every new edit kind: guards check before write; undo dispatched from `undo_edit`; undo must restore byte-identical state (B2==B0 pattern in `crates/rm-job/tests/cli.rs`).

## Review Focus

1. **Prose leaking into stdout** breaks agent parsing: with the envelope shipped, every command's stdout must parse as JSON end-to-end. Pinned by Task 1's `stdout_is_always_json` test over all commands (ok, rejected, noop paths).
2. **Old DB without new tables**: `show-path` on a DB whose `polyline_override` table is absent must return `source: "none"`, not error. Pinned by Task 2's `show_path_degrades_without_override_table`.
3. **A dry-run that writes**: every new write command's dry-run must leave the DB byte-identical (`dump_at` before/after). Pinned per-task (`move_stop_dry_run_writes_nothing`, `remove_stop_dry_run_writes_nothing`, `extend_path_dry_run_writes_nothing`).
4. **extend-path fork choice is a guess the human can't see**: the dry-run must print the chosen way_id per extension so a wrong branch is vetoable before `--apply`. Pinned by Task 6's `dry_run_reports_way_ids`.
5. **Irreversible delete sold as undoable**: `--delete-pole` edits must refuse undo (`NotUndoable`) and the dry-run must carry a one-way warning. Pinned by Task 4's `delete_pole_undo_refused` and Task 5's `dry_run_warns_one_way_delete_pole`.

---

### Task 1: Envelope, `schema` command, `--json` input for existing commands

**Files:**
- Create: `crates/rm-job/src/envelope.rs`
- Create: `crates/rm-job/src/params.rs`
- Modify: `crates/rm-job/src/lib.rs` (dispatch + `Command::Schema`)
- Modify: `crates/rm-job/src/main.rs` (stdout=JSON, stderr=human)
- Modify: `crates/rm-job/Cargo.toml` (add `serde`, `serde_json`, `schemars` via workspace deps; add `schemars = "1"` to the workspace `[workspace.dependencies]` if absent)
- Test: `crates/rm-job/tests/cli.rs`

**Interfaces:**
- Consumes: existing `run(cli: Cli) -> (i32, Report)`, `Report { out, err }`.
- Produces:
  - `envelope::Envelope`, serialized shape (every command, stdout):
    `{"status":"ok"|"rejected"|"noop","command":"<name>","data":{...},"logs":[...],"duration_ms":N}` — `logs` and `data` may be omitted when empty/None.
  - `envelope::ok(command, data) -> Envelope`, `envelope::rejected(command, reason) -> (Envelope, i32 /*1*/)`, `envelope::noop(command, reason) -> (Envelope, i32 /*2*/)`.
  - `params::*` — one schemars+serde struct per command (exact fields below).
  - `run()` returns `(i32, Report)` where `Report.out` is now ALWAYS the envelope JSON string; `Report.err` stays the human text.
  - `schema_list() -> serde_json::Value` (`{"commands":[{name,description}…]}`) and `schema_for(name: &str) -> Option<serde_json::Value>` (`{"command":name,"schema":<JSON Schema>}`).

- [ ] **Step 1: Write failing tests in `crates/rm-job/tests/cli.rs`**

```rust
fn envelope_of(report: &Report) -> serde_json::Value {
    serde_json::from_str(&report.out).unwrap_or_else(|e| panic!("stdout not JSON: {e}: {}", report.out))
}

#[test]
fn stdout_is_always_json() {
    let (_dir, db) = fixture_db();
    for (code, report) in [
        run(add_stop_cli("X", &["1:0"], &db, false)),            // ok
        run(add_stop_cli("X", &["99:0"], &db, false)),           // rejected
        run(add_stop_cli("X", &[], &db, false)),                 // noop
    ] {
        let v = envelope_of(&report);
        assert_eq!(v["command"], "add-stop");
        assert_eq!(code, match v["status"].as_str().unwrap() { "ok" => 0, "rejected" => 1, "noop" => 2, s => panic!("{s}") });
    }
}

#[test]
fn add_stop_json_payload_matches_flags() {
    let (_dir, db) = fixture_db();
    let payload = format!(r#"{{"name":"X","lat":24.1405,"lon":120.6857,"attach":["1:0"],"db":"{}","editor":"test","apply":false}}"#, db.display());
    let cli = Cli { command: Command::AddStop { name: String::new(), lat: 0.0, lon: 0.0, attach: vec![], station: None, db: db.clone(), editor: String::new(), note: None, apply: false, json: Some(payload) } };
    let (code, report) = run(cli);
    assert_eq!(code, 0, "{}{}", report.out, report.err);
    assert!(envelope_of(&report)["data"]["report"].as_str().unwrap().contains("create new station"));
}

#[test]
fn schema_lists_all_commands_and_describes_add_stop() {
    let (_, report) = run(Cli { command: Command::Schema { command: None } });
    let cmds: Vec<&str> = envelope_of(&report)["data"]["commands"].as_array().unwrap()
        .iter().map(|c| c["name"].as_str().unwrap()).collect();
    assert!(cmds.contains(&"add-stop") && cmds.contains(&"schema"));
    let (_, report) = run(Cli { command: Command::Schema { command: Some("add-stop".into()) } });
    let schema = &envelope_of(&report)["data"]["schema"];
    assert!(schema["properties"]["name"]["type"] == "string", "{}", report.out);
    assert!(schema["properties"]["apply"]["type"] == "boolean");
}
```

Update the existing assertions that read `report.out` directly (`dry_run_is_default_exit_0_prints_verdict_and_counts`, `apply_writes_and_reports_ids`, `undo_subcommand_roundtrips`): parse the envelope first, then assert on `data["report"]` (same strings). `grab()` keeps working by running it over `data["report"].as_str().unwrap()`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p rm-job`
Expected: FAIL — `Command::Schema` and `json` field don't exist.

- [ ] **Step 3: Implement**

- `Command` gains `Schema { command: Option<String> }`; `AddStop`/`Undo` each gain `json: Option<String>` (clap `#[arg(long)]`). When `json` is `Some`, deserialize the matching `params::*` struct and build the variant's fields from it; flags are ignored in that case.
- `params.rs`: `AddStopParams { name: String, lat: f64, lon: f64, #[serde(default)] attach: Vec<String>, #[serde(default)] station: Option<i64>, db: String, #[serde(default = "default_editor")] editor: String, #[serde(default)] note: Option<String>, #[serde(default)] apply: bool }` and `UndoParams { edit_id: i64, db: String, #[serde(default = "default_editor")] editor: String }`; `default_editor() -> "agent"`.
- `run()` wraps every return path: build `Envelope` (status per exit code; `data` = `{"report": <previous out text>}` for add-stop/undo), serialize into `report.out`. Rejections keep writing the human reason to `report.err` AND `data.reason`.
- `schema_for` uses `schemars::schema_for!(AddStopParams)` etc.; unknown name → `rejected`.
- `main.rs`: print `report.out` to stdout, `report.err` to stderr, exit with the code.
- `--help` gains an `AGENT USAGE:` section (schema → --json → exit codes), per the agent-friendly-cli skill.

- [ ] **Step 4: Run tests**

Run: `cargo test -p rm-job`
Expected: PASS (all).

- [ ] **Step 5: Commit**

```bash
git add crates/rm-job Cargo.toml
git commit -m "feat(rm-job): JSON envelope, schema introspection, --json payloads"
```

---

### Task 2: Read commands — `find-stop`, `show-route`, `show-path`

**Files:**
- Create: `crates/rm-job/src/queries.rs`
- Modify: `crates/rm-job/src/lib.rs` (variants + dispatch + schema entries)
- Modify: `crates/rm-job/src/params.rs`
- Test: `crates/rm-job/tests/read_cmds.rs`

**Interfaces:**
- Consumes: `rm_db::open_readonly(path: &Path) -> Result<Connection>`; `rm_db::load_polyline_line(conn, xno: &str, direction: i64) -> Result<Option<LineSnapshot>>` (`LineSnapshot { route_id, src_encoded: Option<String>, override_row: Option<OverrideRow{encoded, updated_at}>, route_version_id: Option<i64> }`); route resolution mirrors `plan_add_stop`'s xno→route SQL (`crates/rm-db/src/edit/add_stop.rs`).
- Produces:
  - `params::FindStopParams { db: String, xno: String, #[serde(default)] name: Option<String>, #[serde(default)] code: Option<String> }`
  - `params::ShowRouteParams { db: String, xno: String }`
  - `params::ShowPathParams { db: String, xno: String, direction: i64 }`
  - `queries::find_stop(conn, xno, name, code) -> Result<Vec<StopCandidate>>` where `StopCandidate { stop_version_id, stop_id, stop_code, stop_name, station_id, station_name, lat, lon, directions: Vec<i64> }` (Serialize).
  - `queries::show_route(conn, xno) -> Result<Option<RouteOverview>>` where `RouteOverview { route_id, xno, directions: Vec<DirectionOverview> }`, `DirectionOverview { direction, stops: Vec<StopRow>, path: String /* "override"|"src"|"none" */ }`, `StopRow { sequence, stop_code, stop_name, stop_version_id, station_id }`.
  - `queries::show_path(conn, xno, direction) -> Result<Option<PathOverview>>` where `PathOverview { source: String, encoded: Option<String>, endpoints: Option<Vec<[f64; 2]>> }`.

- [ ] **Step 1: Write failing tests (`tests/read_cmds.rs`, reuse `tests/cli.rs`'s `fixture_db` pattern)**

```rust
#[test]
fn find_stop_by_name_returns_candidate_with_ids() {
    let (_dir, db) = fixture_db();
    // fixture route xno "1" has stop "Old" (assert on name + ids, not codes)
    let v = find_stop_ok(&db, "1", Some("Old"), None);
    let c = &v["candidates"][0];
    assert_eq!(c["stop_name"], "Old");
    assert!(c["stop_version_id"].as_i64().unwrap() > 0);
    assert!(c["directions"].as_array().unwrap().iter().any(|d| d.as_i64() == Some(0)));
}

#[test]
fn find_stop_no_match_is_ok_empty() {
    let (_dir, db) = fixture_db();
    let (code, report) = run_find_stop(&db, "1", Some("不存在"), None);
    assert_eq!(code, 0);
    assert_eq!(envelope_of(&report)["data"]["candidates"].as_array().unwrap().len(), 0);
}

#[test]
fn show_route_lists_stops_and_path_state() {
    let (_dir, db) = fixture_db();
    let v = show_route_ok(&db, "1");
    assert!(v["directions"].as_array().unwrap().len() >= 1);
    // fixture has a src polyline for dir 0 (same shape src_fixture seeds in rm-db tests)
    assert_eq!(v["directions"][0]["path"], "src");
}

#[test]
fn show_path_reports_source_and_endpoints() {
    let (_dir, db) = fixture_db();
    let v = show_path_ok(&db, "1", 0);
    assert_eq!(v["source"], "src");
    assert!(v["endpoints"].as_array().unwrap().len() == 2);
}

#[test]
fn show_path_degrades_without_override_table() {
    let (_dir, db) = fixture_db();
    { let c = rm_db::open(&db).unwrap(); c.execute("DROP TABLE polyline_override", []).unwrap(); }
    let (code, report) = run_show_path(&db, "1", 0);
    assert_eq!(code, 0, "{}", report.out);
    let v = envelope_of(&report);
    assert!(v["logs"].as_array().unwrap().iter().any(|l| l.as_str().unwrap().contains("polyline_override")));
}

#[test]
fn unknown_xno_rejected() {
    let (_dir, db) = fixture_db();
    let (code, _) = run_show_route(&db, "999");
    assert_eq!(code, 1);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p rm-job --test read_cmds`
Expected: FAIL — modules/variants don't exist.

- [ ] **Step 3: Implement**

- `find_stop` SQL: resolve route (same SQL as `plan_add_stop`), then join current `route_version` → `subroute_version` → `route_stop_version` → `stop_version`, station name via the same current-version join `load_station_snapshot` uses; filter `stop_name LIKE %q%` / `stop_code = q`; group directions per pole in a Vec (insert order = sequence order — deterministic).
- `show_route` stops query: mirror `crates/rm-web/src/queries/routes.rs:419` `stops()` columns (sequence/code/name/svi/station_id) minus projection QA. Path state per direction from `load_polyline_line`: `override_row.is_some()` → "override", else `src_encoded.is_some()` → "src", else "none" (with the `sqlite_master` probe for `polyline_override` — missing table → treat as None + a `logs` entry).
- `show_path`: `load_polyline_line` → override row wins (`source:"override"`), else src (`"src"`), else `"none"` with null encoded/endpoints; endpoints = first/last of the decoded polyline via `rm_core::polyline::decode`.
- Dispatch: `open_readonly`; unknown route → `rejected`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p rm-job`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/rm-job
git commit -m "feat(rm-job): find-stop/show-route/show-path read commands"
```

---

### Task 3: `move-stop` — stop-edit preview + CLI

**Files:**
- Modify: `crates/rm-db/src/edit/stop.rs` (add preview service), `crates/rm-db/src/lib.rs` (re-export)
- Modify: `crates/rm-job/src/lib.rs`, `crates/rm-job/src/params.rs`
- Test: `crates/rm-db/src/edit/stop.rs` (module tests), `crates/rm-job/tests/read_cmds.rs`

**Interfaces:**
- Consumes: `rm_db::edit_stop(conn, stop_version_id: i64, StopEdit { lat: Option<f64>, lon: Option<f64>, stop_name: Option<String> }, editor: &str, note: &str) -> Result<EditOutcome>` (`EditOutcome { edit_id, new_version_id, stale_routes }`).
- Produces:
  - `rm_db::preview_stop_edit(conn, stop_version_id: i64, cmd: StopEdit) -> Result<StopEditPreview>`; `StopEditPreview { stop_id: i64, stop_version_id: i64, current_stop_name: String, current_lat: f64, current_lon: f64, op: StopEditOp }` (Debug). Same guards as the apply path (`NotCurrent`, `EmptyEdit`, `NoOpEdit`) but zero writes.
  - `params::MoveStopParams { db: String, #[serde(default)] stop_version_id: Option<i64>, #[serde(default)] xno: Option<String>, #[serde(default)] code: Option<String>, #[serde(default)] lat: Option<f64>, #[serde(default)] lon: Option<f64>, #[serde(default)] stop_name: Option<String>, #[serde(default = "default_editor")] editor: String, #[serde(default)] note: Option<String>, #[serde(default)] apply: bool }`. Resolution rule: `stop_version_id` if given; else `xno`+`code` MUST resolve to exactly one current pole (via Task 2's `find_stop` query) else rejected.

- [ ] **Step 1: Write failing tests**

rm-db (`stop.rs` tests module, fixture per existing `snap()`/`fixture` pattern):

```rust
#[test]
fn preview_stop_edit_mirrors_guards_and_writes_nothing() {
    let conn = open_in_memory().unwrap();
    let old = fixture(&conn);
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM stop_version", [], |r| r.get(0)).unwrap();
    let p = preview_stop_edit(&conn, old, StopEdit { lat: Some(24.5), ..Default::default() }).unwrap();
    assert_eq!(p.stop_version_id, old);
    assert_eq!(p.op.lat, Some(24.5));
    assert!(matches!(preview_stop_edit(&conn, 999, StopEdit::default()), Err(EditError::NotCurrent(999))));
    assert!(matches!(preview_stop_edit(&conn, old, StopEdit::default()), Err(EditError::EmptyEdit)));
    assert_eq!(n, conn.query_row("SELECT COUNT(*) FROM stop_version", [], |r| r.get(0)).unwrap());
}
```

CLI (`tests/read_cmds.rs`):

```rust
#[test]
fn move_stop_dry_run_then_apply_and_undo() {
    let (_dir, db) = fixture_db();
    let svi = find_stop_ok(&db, "1", Some("Old"), None)["candidates"][0]["stop_version_id"].as_i64().unwrap();
    let b0 = dump_at(&db, &[]);
    let (code, report) = run_move_stop(&db, serde_json::json!({"stop_version_id": svi, "lat": 24.15, "db": db.display().to_string()}), false);
    assert_eq!(code, 0, "{}", report.out);
    assert_eq!(dump_at(&db, &[]), b0, "dry run must not write");
    let d = envelope_of(&report)["data"].clone();
    assert_eq!(d["changes"]["lat"], 24.15);

    let (code, report) = run_move_stop(&db, serde_json::json!({"stop_version_id": svi, "lat": 24.15, "db": db.display().to_string(), "apply": true}), true);
    assert_eq!(code, 0, "{}", report.out);
    let edit_id = envelope_of(&report)["data"]["edit_id"].as_i64().unwrap();
    assert!(edit_id > 0);

    let (code, report) = run_undo(edit_id, &db);
    assert_eq!(code, 0, "{}{}", report.out, report.err);
    assert_eq!(dump_at(&db, ROUNDTRIP_SKIP), dump_at(&db, ROUNDTRIP_SKIP)); // placeholder — see note
}

#[test]
fn move_stop_by_xno_code_ambiguous_rejected() {
    let (_dir, db) = fixture_db();
    let (code, report) = run_move_stop(&db, serde_json::json!({"xno": "1", "code": "不存在", "lat": 24.15, "db": db.display().to_string()}), false);
    assert_eq!(code, 1);
    assert!(report.err.contains("resolve"), "{}", report.err);
}
```

Note: the undo half of the first test replaces the placeholder line with the standard `b0`-before-dry-run comparison: capture `b0 = dump_at(&db, ROUNDTRIP_SKIP)` before the apply, and after undo assert `dump_at(&db, ROUNDTRIP_SKIP) == b0`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p rm-db preview_stop_edit && cargo test -p rm-job --test read_cmds`
Expected: FAIL — `preview_stop_edit` undefined.

- [ ] **Step 3: Implement**

- `preview_stop_edit` in `stop.rs`: `load_stop_snapshot` → same guards as `edit_stop`'s plan leg → `StopEditPreview`. Re-export from `lib.rs`.
- CLI dispatch: resolve target → dry-run: `preview_stop_edit`, data `{"stop_id", "stop_version_id", "changes": {…op fields…}}`; apply: `edit_stop`, data `{"edit_id", "new_stop_version_id", "stale_routes"}` (same keys as rm-web's response).

- [ ] **Step 4: Run tests**

Run: `cargo test -p rm-db -p rm-job`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/rm-db crates/rm-job
git commit -m "feat(rm-db,rm-job): stop-edit preview + move-stop command"
```

---

### Task 4: rm-db detach primitive (version-chain stop removal)

**Files:**
- Create: `crates/rm-db/src/edit/detach.rs`
- Modify: `crates/rm-db/src/edit/mod.rs` (`mod detach;` + `EditError` variants + `UndoOutcome::StopDetach` + `undo_edit` arm), `crates/rm-db/src/lib.rs` (re-exports)
- Test: `crates/rm-db/src/edit/detach.rs` (tests module)

**Interfaces:**
- Consumes: `crate::mint(&tx, "route_version"|"subroute_version") -> Result<i64>`, `crate::now_rfc3339()`, `crate::stale::mark_line_stale(tx, route_id, direction, cause_edit_id, &now)` (marks all four datasets — `crates/rm-db/src/stale.rs:202`); the version-chain mint/pointer pattern of `apply_add_stop` (`crates/rm-db/src/edit/add_stop.rs:545`, incl. `RouteState` re-validation: current pointer must equal the planned old rv or `NotCurrent`).
- Produces (all re-exported from `rm_db`):
  - `pub struct DetachCmd { pub xno: String, pub direction: i64, pub stop_code: String, pub sequence: Option<i64>, pub all: bool, pub delete_pole: bool }`
  - `pub struct DetachOccurrence { pub route_stop_version_id: i64, pub sequence: i64 }`
  - `pub struct DetachPlan { pub route_id: i64, pub route_version_id: i64, pub stop_id: i64, pub stop_version_id: i64, pub stop_code: String, pub stop_name: String, pub occurrences: Vec<DetachOccurrence>, pub pole_orphan: bool, pub serving_directions: Vec<i64>, pub stale_routes: Vec<i64>, pub warnings: Vec<String> }` (Debug)
  - `pub struct DetachOutcome { pub edit_id: i64, pub old_route_version_id: i64, pub new_route_version_id: i64, pub pole_deleted: bool, pub stale_routes: Vec<i64> }` (Debug)
  - `pub struct DetachUndone { pub edit_id: i64, pub restored_route_version_id: i64 }`
  - `pub fn plan_detach_stop(conn: &Connection, cmd: &DetachCmd) -> Result<DetachPlan>`
  - `pub fn apply_detach_stop(conn: &Connection, plan: &DetachPlan, editor: &str, note: &str) -> Result<DetachOutcome>`
  - `pub fn detach_stop(conn: &Connection, cmd: DetachCmd, editor: &str, note: &str) -> Result<DetachOutcome>` (plan→apply service)
  - `pub fn undo_detach_stop(conn: &Connection, edit_id: i64, editor: &str, note: &str) -> Result<DetachUndone>`
  - `EditError` gains: `DuplicateStopOccurrence { xno: String, direction: i64, stop_code: String, count: i64 }` ("occurs {count} times — give --sequence or --all") and `OrphanPoleRefused(String)` ("…last direction; pass --delete-pole (one-way, not undoable)"). "Stop not on the direction" reuses `UnknownAfterStop(xno, direction, code)`.
  - `UndoOutcome::StopDetach { edit_id: i64, restored_route_version_id: i64 }`; `undo_edit` arm on entity `"stop_detach"` (before the stop fallthrough).
  - edit_log row: entity `"stop_detach"`, entity_id = stop_id, action `"detach"`, version_before = old rv, version_after = new rv, changes JSON `{"kind":"stop_detach","xno":…,"direction":…,"stop_code":…,"sequences":[…],"delete_pole":bool,"old_route_version_id":…,"new_route_version_id":…,"pole_deleted":bool,"old_revision":…,"old_updated_at":"…"}`.

- [ ] **Step 1: Write failing tests (tests module in `detach.rs`)**

Fixture: extend the `setup()` pattern of `crates/rm-db/src/edit/matched_path.rs:162` (route 1 `'tcg:1'`, route_version 11, subroute_version 11, rsv rows, osm ways/nodes) with a second stop and a route where the same stop occurs twice in dir 1 (loop variant). All inserts in Vec order.

```rust
#[test]
fn plan_reports_found_and_orphan_verdicts() { /* stop on two directions → pole_orphan false; stop on one → true; unknown stop → UnknownAfterStop */ }

#[test]
fn plan_duplicate_occurrence_needs_sequence_or_all() {
    // loop fixture: same code at seq 29 and 42 in dir 1
    // sequence: None + all: false → Err(DuplicateStopOccurrence{count: 2})
    // sequence: Some(42) → 1 occurrence; all: true → 2 occurrences
}

#[test]
fn apply_mints_new_chain_and_moves_pointer() {
    // plan then apply; assert: route.current_route_version_id == out.new_route_version_id
    // new rv has one fewer rsv row in that direction; old rv untouched (row count unchanged)
    // detached rsv still exists under the OLD route_version (versioned history preserved)
    // stale_mark holds the four mark_line_stale datasets for (route, direction)
}

#[test]
fn undo_pointer_restore_is_byte_identical() {
    // b0 = full-table dump excluding ["edit_log","stale_mark","id_sequence"] (ROUNDTRIP_SKIP set)
    // apply → undo → dump == b0; second undo → Err(NotUndoable)
}

#[test]
fn undo_refused_after_route_moved_on() {
    // apply detach, then add-stop on the same route (pointer moves), then undo → Err(NotUndoable)
}

#[test]
fn orphan_pole_refused_without_delete_pole() {
    // pole serving only this direction: apply(delete_pole: false) → Err(OrphanPoleRefused)
    // apply(delete_pole: true) → pole_deleted true; pole version chain + station link gone
}

#[test]
fn delete_pole_undo_refused() {
    // undo of a delete_pole edit → Err(NotUndoable) regardless of pointer state
}

#[test]
fn undo_edit_dispatches_stop_detach() {
    // undo_edit(eid) matches UndoOutcome::StopDetach { restored_route_version_id == old rv }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p rm-db detach`
Expected: FAIL — module doesn't exist.

- [ ] **Step 3: Implement**

- `plan_detach_stop`: resolve route (add_stop's SQL) → current rv → the direction's rsv rows joined to stop_version; verdicts in order: unknown route (`UnknownRoute`), unknown direction (`UnknownDirection`), stop not on direction (`UnknownAfterStop`), duplicate occurrence without disambiguator (`DuplicateStopOccurrence`), orphan pole verdict (count other directions referencing the pole across ALL routes — `pole_orphan` + `serving_directions`); `stale_routes` from the same read `mark_line_stale` will mark (route itself); warnings: terminal-stop note ("first/last stop — neighbors' sequence shifts").
- `apply_detach_stop`: one tx; `RouteState` re-check (`NotCurrent(old_rv)`); mint new `route_version` (copy current row, revision+1) + `subroute_version` (copy) + rsv rows minus detached occurrence(s); `UPDATE route SET current_route_version_id = new, revision = old_revision + 1, updated_at = now`; `mark_line_stale` for each edited direction; `delete_pole` (only when `pole_orphan`): delete the pole's `stop_version` rows + `stop` row + station link, mirroring exactly the row set `apply_stop_add_undo` deletes; edit_log write per the shape above (changes carry `old_revision`/`old_updated_at` for the undo payload).
- `undo_detach_stop`: load row (`QueryReturnedNoRows` → `NotUndoable`); `changes.delete_pole == true` → `NotUndoable`; pointer must equal `new_route_version_id` else `NotUndoable`; restore pointer + revision + updated_at; own edit_log row (`action "undo"`, `undo_of`); clear this job's own stale marks (add-stop-undo precedent).
- Register in `mod.rs`: `pub mod detach;`, `UndoOutcome::StopDetach`, `undo_edit` arm, re-exports in `lib.rs`. Fix `crates/rm-job/src/lib.rs` `write_undo` with a `StopDetach` arm (compiler will point at it).

- [ ] **Step 4: Run tests**

Run: `cargo test -p rm-db -p rm-job`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/rm-db crates/rm-job
git commit -m "feat(rm-db): detach primitive — version-chain stop removal with pointer-restore undo"
```

---

### Task 5: `remove-stop` CLI command

**Files:**
- Modify: `crates/rm-job/src/lib.rs`, `crates/rm-job/src/params.rs`
- Test: `crates/rm-job/tests/read_cmds.rs`

**Interfaces:**
- Consumes: Task 4's full surface.
- Produces: `params::RemoveStopParams { db: String, xno: String, direction: i64, code: String, #[serde(default)] sequence: Option<i64>, #[serde(default)] all: bool, #[serde(default)] delete_pole: bool, #[serde(default = "default_editor")] editor: String, #[serde(default)] note: Option<String>, #[serde(default)] apply: bool }`. Envelope `data` (dry-run): the `DetachPlan` fields verbatim (serde-serialize a `PlanData` mirror struct — `DetachPlan` itself is not Serialize and stays that way). Data (apply): `{"edit_id", "old_route_version_id", "new_route_version_id", "pole_deleted", "stale_routes"}`.

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn remove_stop_dry_run_reports_plan_then_apply_roundtrips() {
    let (_dir, db) = fixture_db();
    let code_on_route = /* find-stop → (xno "1", code of stop "Old") from fixture */;
    let b0 = dump_at(&db, ROUNDTRIP_SKIP);
    let (code, report) = run_remove_stop(&db, json!({"xno":"1","direction":0,"code":code_on_route,"db":db.display().to_string()}));
    assert_eq!(code, 0, "{}", report.out);
    assert_eq!(dump_at(&db, ROUNDTRIP_SKIP), b0, "dry run must not write");
    let d = &envelope_of(&report)["data"];
    assert_eq!(d["occurrences"].as_array().unwrap().len(), 1);

    let (code, report) = run_remove_stop(&db, json!({"xno":"1","direction":0,"code":code_on_route,"db":db.display().to_string(),"apply":true}));
    assert_eq!(code, 0, "{}", report.out);
    let eid = envelope_of(&report)["data"]["edit_id"].as_i64().unwrap();
    run_undo(eid, &db);
    assert_eq!(dump_at(&db, ROUNDTRIP_SKIP), b0, "apply+undo byte-identical (B2==B0)");
}

#[test]
fn remove_stop_dry_run_warns_one_way_delete_pole() {
    // orphan-pole stop; payload delete_pole: true, apply: false
    // status ok; data.warnings contains a "one-way"/"not undoable" line
}

#[test]
fn remove_stop_duplicate_needs_sequence() {
    // loop fixture (Task 4's shape, seeded via rm_db inserts in the test file)
    // code 1 + stderr contains "occurs 2 times"
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p rm-job --test read_cmds`
Expected: FAIL — variant undefined.

- [ ] **Step 3: Implement** — dispatch: `open(db)` (write-capable handle is fine; plan is read-only), dry-run → `plan_detach_stop`, apply → `detach_stop`. Rejections map straight to exit 1 with the `EditError` text.

- [ ] **Step 4: Run tests** — `cargo test -p rm-job` → PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/rm-job
git commit -m "feat(rm-job): remove-stop command over the detach primitive"
```

---

### Task 6: `extend-path` — graph walk + polyline override

**Files:**
- Create: `crates/rm-job/src/extend.rs`
- Modify: `crates/rm-job/src/lib.rs`, `crates/rm-job/src/params.rs`, `crates/rm-job/Cargo.toml` (add `rm-osm-match`, `rm-core` workspace deps)
- Test: `crates/rm-job/tests/extend.rs`

**Interfaces:**
- Consumes: `rm_osm_match::RoadGraph::from_conn(conn) -> anyhow::Result<RoadGraph>`; `g.segments_near(p: LatLng, radius_m: f64) -> impl Iterator<Item = usize>`; `g.seg(idx) -> Seg { a: u32, b: u32, way_id: i64, length_m: f64, oneway: i32, .. }`; `g.node_pos(idx) -> NodePos { osm_id, lat, lon }`; `g.xy: Vec<(f64, f64)>`; `g.out_edges: Vec<Vec<(u32 /*node*/, u32 /*seg*/)>>` (all public, `crates/rm-osm-match/src/graph.rs`); `rm_db::load_polyline_line`, `rm_db::effective_encoded(conn, route_id, xno, direction) -> Result<String>`; `rm_db::matched_path(conn, route_id, direction) -> Result<Option<Vec<LatLng>>>`; `rm_core::polyline::{encode, decode}`; `rm_core::geo::{point_to_segment_m, bearing_deg, haversine_m}`; `rm_db::edit_polyline_line(conn, LineEditCmd { xno, direction, points: Vec<[f64; 2]>, expected_updated_at: Option<String> }, editor, note) -> Result<LineApplied { edit_id, created, updated_at }>`.
- Produces:
  - `params::ExtendPathParams { db: String, xno: String, direction: i64, #[serde(default = "default_meters")] meters: f64 /* 10.0 */, #[serde(default = "default_end")] end: String /* "both"|"first"|"last" */, #[serde(default = "default_editor")] editor: String, #[serde(default)] note: Option<String>, #[serde(default)] apply: bool }`
  - `extend::Extension { end: String, way_ids: Vec<i64>, new_vertices: Vec<[f64; 2]>, snap_dist_m: f64 }` and `extend::plan_extension(conn, xno, direction, meters, ends) -> Result<(Vec<LatLng> /*candidate points*/, Vec<Extension>, String /*source used*/)>`.
  - Const `MAX_SNAP_M: f64 = 50.0`.

- [ ] **Step 1: Write failing tests (`tests/extend.rs`)**

Fixture: a tiny db — `rm_db::open`, seed a route with `src` polyline whose last vertex sits exactly on a straight osm way (3–4 `osm_node`/`osm_way`/`osm_way_node` rows via plain INSERT, mirroring `matched_path.rs` setup's insert style), plus a fork way branching at ~45° so the tie-break is observable.

```rust
#[test]
fn extend_appends_vertices_along_the_way() {
    // plan_extension 10m "last": candidate = original + ~10m of way vertices;
    // first new vertex ≈ 10m beyond the old end along the way (haversine < 11m)
    // source == "src"; original vertices are a prefix of the candidate
}

#[test]
fn dry_run_prints_way_ids_and_writes_nothing() {
    // run CLI extend-path apply:false → data.extensions[0].way_ids non-empty;
    // dump_at unchanged
}

#[test]
fn apply_creates_override_and_undo_removes_it() {
    // apply:true → data.created == true; polyline_override row exists;
    // run undo with the edit_id → override row gone (B2==B0 on ROUNDTRIP_SKIP)
}

#[test]
fn fork_takes_straightest_continuation() {
    // straight way continues the path bearing; fork bends 45°:
    // way_ids must contain the straight way's id, not the fork's
}

#[test]
fn endpoint_far_from_graph_rejected() {
    // path end > MAX_SNAP_M from any node → exit 1, stderr names the snap limit
}

#[test]
fn empty_osm_tables_rejected_with_prepare_hint() {
    // no osm_node rows → exit 1, stderr contains "rm-osm-prepare"
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p rm-job --test extend`
Expected: FAIL — module/variant undefined.

- [ ] **Step 3: Implement `extend.rs`**

- Resolve the effective path: `load_polyline_line` → override `encoded` → src `encoded` → `matched_path(route_id, direction)`; all absent → rejected "no path for route/dir". Decode.
- `RoadGraph::from_conn` (empty `osm_node` → its error text already says "run rm-osm-prepare" — surface verbatim).
- Per end, the outward walk (the one algorithm the signatures don't determine):

```text
endpoint = first/last candidate vertex
snap: among g.segments_near(endpoint, MAX_SNAP_M) pick the seg with the
      smallest point_to_segment_m distance; none → rejection naming MAX_SNAP_M
      snap_dist_m = that distance
start_node = whichever of seg.a/seg.b is nearer the endpoint
incoming bearing = rm_core::geo::bearing_deg(second→first vertex) at "first"
                   (i.e. the travel direction pointing outward), symmetric at "last"
Dijkstra from start_node over g.out_edges with cost = seg.length_m;
      do not traverse the snap seg back toward the path;
      when a node fans out, order edges by cosine alignment of the edge's
      unit vector (from g.xy) with the incoming bearing (straightest first)
stop when the popped node's cumulative distance ≥ meters; record the node
      chain; final partial seg → interpolate the last vertex at the exact
      remaining distance (drop the duplicated junction vertex where the
      extension meets the original)
```

- Candidate points: original + extension(s) appended/prepended.
- Dry-run data: `{"source", "extensions": [Extension…]}` (way_ids included — Review Focus 4). Apply: fresh `load_polyline_line` → `expected_updated_at = override_row.updated_at` when an override exists (None on create) → `edit_polyline_line`; data `{"edit_id", "created", "updated_at", "extensions"}`.
- All graph work read-only; dry-run opens `open_readonly` (Review Focus 3).

- [ ] **Step 4: Run tests**

Run: `cargo test -p rm-job --test extend && cargo test -p rm-job`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/rm-job
git commit -m "feat(rm-job): extend-path — outward graph walk written as polyline override"
```

---

### Task 7: Runbook, agent skill, README

**Files:**
- Modify: `docs/agents/rm-jobs.md` (command catalogue + canonical job loop)
- Create: `.claude/skills/rm-jobs/SKILL.md`
- Modify: `README.md` (rm-job section: agent invocation examples)

**Interfaces:**
- Consumes: every command from Tasks 1–6; their `schema` output is the authoritative param reference — the runbook links to `rm-job schema <cmd>` instead of duplicating field lists.

- [ ] **Step 1: Extend `docs/agents/rm-jobs.md`**

Add: the command table (read: `find-stop`/`show-route`/`show-path`; write: `add-stop`/`move-stop`/`remove-stop`/`extend-path`/`undo`), the canonical loop (schema → find-stop → dry-run → show human → `apply:true` → report → undo if wrong), the envelope/exit-code contract, and the warnings that matter: run `cargo run -r -p rm-job`; `--delete-pole` is one-way; rm-web derived layers stay stale until `rm-registry --stale`; dry-run output is the approval artifact — always show it before applying.

- [ ] **Step 2: Write `.claude/skills/rm-jobs/SKILL.md`**

Frontmatter `name: rm-jobs`, description triggering on route/stop correction jobs (move/remove a stop, extend a path, add a stop, undo). Body: the same loop, condensed; hard rules — dry-run before every apply, quote the plan to the human, never `--delete-pole` without an explicit human yes, never apply when the plan shows warnings the human hasn't seen.

- [ ] **Step 3: README + manual smoke**

README rm-job section: three example invocations (`schema`, a dry-run, an apply). Manual smoke on a copy of `data/taichung.db`: `schema` → `find-stop` → `show-route` → one dry-run `extend-path`. (`cp data/taichung.db /tmp/rmjob-smoke.db` first — never the live file.)

- [ ] **Step 4: Commit**

```bash
git add docs/agents/rm-jobs.md .claude/skills/rm-jobs README.md
git commit -m "docs: rm-jobs agent runbook + skill for the rm-job CLI surface"
```

---

## Self-Review

- **Spec coverage:** envelope/schema/`--json`/exit codes (T1), read commands + degrade (T2), move-stop (T3), detach primitive incl. version-chain/one-way/duplicate decisions 7–9 (T4–T5), extend-path incl. decision 10 and graph-from-db (T6), runbook/skill (T7), Review-Focus pins distributed to owning tasks. Decision 6 (`--json`-first for new commands) is T2/T3/T5/T6's params structs. No gap found.
- **Step scan:** each step writes one thing; bodies appear only where the signature+test don't determine them (the outward-walk block, the envelope shape).
- **Type consistency:** `StopEditPreview`/`DetachCmd`/`DetachPlan`/`DetachOutcome`/`Extension` names match across tasks; `UndoOutcome::StopDetach` spelled identically in T4 and consumed by `write_undo` in T4's Step 3; `params::*` names match the schema tests.
- **Review Focus:** five failure modes each carry a named test in their owning task.
- **Proportion:** plan ≈ 1.5× spec — signatures and pinned values, no transcripts.
