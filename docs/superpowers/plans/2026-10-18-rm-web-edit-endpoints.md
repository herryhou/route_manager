# rm-web Edit Endpoints + rm-job Removal Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port add-stop / remove-stop / extend-path to rm-web edit endpoints with dry-run plans, make all eight write operations plan-capable, then delete the rm-job crate and re-point the agent skill/docs at the HTTP API.

**Architecture:** rm-db keeps owning edit primitives; rm-osm-match gains the extend graph-walk module (rm-web already depends on it); rm-web handlers gain three POST routes plus an `apply` field on the five existing write handlers. Plans are read-only (`run_db`), applies go through `run_db_write`. rm_db plan structs stay non-`Serialize`; handlers carry serde mirror structs.

**Tech Stack:** Rust, axum 0.8, rusqlite, serde, tower `oneshot` tests.

**Spec:** `docs/superpowers/specs/2026-10-18-rm-web-edit-endpoints-design.md`

## Global Constraints

- All new routes sit behind the `Editor` bearer extractor (401 missing / 403 unknown token); edit routes only exist when `--edit` + `--edit-tokens` are set (existing `router_with` gating — no change).
- Plan legs are read-only (`run_db`); apply legs use `run_db_write`. Never write in a plan leg.
- rm_db plan structs stay non-`Serialize`. Handlers define mirrors named `<Type>Data` (e.g. `AddStopPlanData`) with the rm_db struct's fields **verbatim, snake_case**. Nested types (`StationVerdict`, `PlannedAttach`, `RowCounts`, `DetachOccurrence`) get matching mirrors.
- Plan/apply response field names = the rm_db struct's field names verbatim (detach apply: `edit_id`, `old_route_version_id`, `new_route_version_id`, `pole_deleted`, `stale_routes`; add apply: `edit_id`, `stop_id`, `stop_version_ids`, `station_id`, `station_created`, `stale_routes`). `warnings` / `logs` included whenever the primitive yields them.
- `apply` semantics: new three endpoints — absent ⇒ `false` (plan); existing five handlers — absent ⇒ apply (backward compat), explicit `false` ⇒ plan.
- Plan-time `EditError`s map through the existing `edit_conflict` mapper (400 request problems / 409 races), same as apply paths.
- Determinism: no HashMap-iteration inserts anywhere new (project invariant).
- Every task: `cargo test -p <crate>` green + commit before next task. Release mode for any manual pipeline runs (`-r`).

## Review Focus

1. **Existing UI callers must keep working**: `POST /api/edit/stop/{id}` etc. without an `apply` field must still apply. Test: Task 6 backward-compat test per handler (body without `apply` → row changes).
2. **DB missing migration-0013 table**: extend plan must degrade to "no override", not 500 (the /raw incident rule). Test: Task 1 degrade test (drop `polyline_override`, `plan_extension` still returns src-based plan).
3. **Orphan pole without `delete_pole` must be refused, never silently deleted** (one-way teardown guard). Test: Task 4 orphan-refusal test.
4. **Warnings must reach the agent** (it surfaces them to the human before applying). Test: Task 3/4 assert `warnings` present in plan JSON.
5. **Extend endpoint far from any road** must 400 naming the 50 m snap limit, not 500. Test: Task 5.

---

### Task 1: Move the extend walk into rm-osm-match

**Files:**
- Create: `crates/rm-osm-match/src/extend.rs` (content from `crates/rm-job/src/extend.rs`, plus private `LineSnapshot` struct + probing `line_snapshot` copied from `crates/rm-job/src/queries.rs:54-66,318-365` shape)
- Create: `crates/rm-osm-match/tests/extend_walk.rs` (direct-call tests + fixture helpers copied verbatim from `crates/rm-job/tests/extend.rs`: `line_db`, `midseg_db`, `graphless_db`, and the tests `midseg_snap_starts_on_the_outward_side`, `endpoint_far_from_graph_rejected`, `empty_osm_tables_rejected_with_prepare_hint` — the latter two rewritten to call `plan_extension` directly and assert the error text contains `"50"` / `"rm-osm-prepare"`)
- Modify: `crates/rm-osm-match/src/lib.rs` (add `pub mod extend;`)
- Modify: `crates/rm-job/src/extend.rs` (gut to a re-export shim: `pub use rm_osm_match::extend::{plan_extension, Extension};` — rm-job's CLI tests keep passing against the moved code)
- Delete nothing yet.

**Interfaces:**
- Produces: `rm_osm_match::extend::plan_extension(conn: &Connection, xno: &str, direction: i64, meters: f64, ends: &str, logs: &mut Vec<String>) -> anyhow::Result<(Vec<rm_core::LatLng>, Vec<Extension>, String)>`; `Extension { end: String, way_ids: Vec<i64>, new_vertices: Vec<[f64; 2]>, snap_dist_m: f64 }` (both `pub`).
- The copied `line_snapshot` keeps the sqlite_master probe: missing `polyline_override` table ⇒ treated as no override (degrade, never error).
- Import-path deltas from the rm-job original: `queries::line_snapshot` ⇒ local fn; `crate::graph::RoadGraph` (rm-osm-match's own graph); `rm_core::{geo, polyline, LatLng}` unchanged.

- [ ] **Step 1: Write the failing tests** — create `crates/rm-osm-match/tests/extend_walk.rs` with the three named tests + helpers copied per above; top: `use rm_osm_match::extend::plan_extension;`
- [ ] **Step 2: Run** `cargo test -p rm-osm-match --test extend_walk` — Expected: FAIL (module missing)
- [ ] **Step 3: Create `src/extend.rs`** — copy rm-job's extend.rs body; adjust imports per deltas; add the private snapshot type + probing wrapper (probe `sqlite_master` for `polyline_override`, resolve route by `ext_uid = 'tcg:' || xno`, latest `src_batch` encoded, else matched path — copy the SQL from `crates/rm-job/src/queries.rs` `line_snapshot` verbatim)
- [ ] **Step 4: Run** `cargo test -p rm-osm-match` — Expected: PASS (all, incl. new walk tests)
- [ ] **Step 5: Shim rm-job** — replace `crates/rm-job/src/extend.rs` body with the re-export; run `cargo test -p rm-job --test extend` — Expected: PASS
- [ ] **Step 6: Commit** `refactor(rm-osm-match): move extend graph-walk from rm-job (probed snapshot keeps 0013 degrade)`

### Task 2: Move `parse_attach` into rm-db

**Files:**
- Modify: `crates/rm-db/src/edit/add_stop.rs` (add `pub fn parse_attach(s: &str) -> Result<AttachCmd, String>` — body copied verbatim from `crates/rm-job/src/lib.rs:448`; plus unit tests)
- Modify: `crates/rm-db/src/edit/mod.rs` + `crates/rm-db/src/lib.rs` re-exports (match how `add_stop`/`AttachCmd` are already re-exported — same path style)
- Modify: `crates/rm-job/src/lib.rs` — `parse_attach` becomes `pub use rm_db::parse_attach;`

**Interfaces:**
- Produces: `rm_db::parse_attach(s: &str) -> Result<rm_db::AttachCmd, String>` (accepts `"XNO:DIR"` and `"XNO:DIR@CODE"`; rejects garbage with a human message).

- [ ] **Step 1: Write failing tests** in `add_stop.rs` test module: parses `"12:0"` (after_stop_code None), `"12:0@2249"` (Some("2249")), rejects `"12"`, `"12:x"`, `"12:0@"`, `":0"` with non-empty message
- [ ] **Step 2: Run** `cargo test -p rm-db parse_attach` — Expected: FAIL
- [ ] **Step 3: Implement** per signature above
- [ ] **Step 4: Run** `cargo test -p rm-db` — PASS; then shim rm-job, `cargo test -p rm-job` — PASS
- [ ] **Step 5: Commit** `refactor(rm-db): absorb parse_attach from rm-job`

### Task 3: POST /api/edit/stop (add-stop)

**Files:**
- Modify: `crates/rm-web/src/handlers/edit.rs` (handler + bodies + mirrors)
- Modify: `crates/rm-web/src/lib.rs` (route: `.route("/api/edit/stop", post(handlers::edit::add_stop))` inside the `edit_enabled` arm)
- Test: `crates/rm-web/tests/api.rs` (fixture_db provides routes "1"/"2")

**Interfaces:**
- Consumes: `rm_db::plan_add_stop(&Connection, &AddStopCmd) -> AddStopPlan`, `rm_db::add_stop(Connection, AddStopCmd, editor, note) -> AddStopOutcome`, `rm_db::parse_attach`.
- Produces: `POST /api/edit/stop` body `{ name: String, lat: f64, lon: f64, station?: i64, attach: Vec<String>, note?: String, apply?: Bool }`.
  - Plan (default): `AddStopPlanData` mirror → 200 JSON (fields verbatim incl. `warnings`, `row_counts`, `stale_routes`).
  - Apply: 200 `{ edit_id, stop_id, stop_version_ids, station_id, station_created, stale_routes }`.
  - Empty `attach` ⇒ 400 `"nothing to attach — give at least one attach XNO:DIR[@CODE]"`; bad attach string ⇒ 400 with parse message; lat/lon out of range ⇒ 400 (existing convention).

- [ ] **Step 1: Failing tests** in `tests/api.rs` (bearer header helper exists in edit tests — reuse): (a) plan default: POST without `apply` → 200, JSON has `attachments` array + `row_counts`, and a follow-up `GET /api/routes` shows unchanged stop counts; (b) plan with `apply:false` behaves the same; (c) apply:true → 200 `edit_id` present + `station_created` bool, and `POST /api/edit/undo/{edit_id}` then restore is reflected in `GET /api/routes/{id}/directions/0/stops`; (d) empty attach → 400
- [ ] **Step 2: Run** `cargo test -p rm-web --test api add_stop` — Expected: FAIL (404)
- [ ] **Step 3: Implement** handler: parse attach strings first (400 on error, before opening any conn), build `AddStopCmd`, plan via `run_db` / apply via `run_db_write`
- [ ] **Step 4: Run** `cargo test -p rm-web` — PASS
- [ ] **Step 5: Commit** `feat(rm-web): POST /api/edit/stop — add-stop with dry-run plan`

### Task 4: POST /api/edit/stop/detach (remove-stop)

**Files:**
- Modify: `crates/rm-web/src/handlers/edit.rs` (+ `DetachPlanData` mirror with `PlanOccurrence` — copy field-for-field from rm-job's `PlanData`, `crates/rm-job/src/lib.rs:1440-1492`)
- Modify: `crates/rm-web/src/lib.rs` (route — static sibling of `/api/edit/stop/{stop_version_id}`, axum 0.8 resolves static before `{param}`)
- Test: `crates/rm-web/tests/api.rs`

**Interfaces:**
- Consumes: `rm_db::plan_detach_stop(&Connection, &DetachCmd) -> DetachPlan`, `rm_db::detach_stop(Connection, DetachCmd, editor, note) -> DetachOutcome`.
- Produces: body `{ xno: String, direction: i64, code: String, sequence?: i64, all?: bool, delete_pole?: bool, note?: String, apply?: Bool }`.
  - Plan: `DetachPlanData` → 200 (fields incl. `occurrences`, `pole_orphan`, `serving_directions`, `warnings`).
  - Apply: 200 `{ edit_id, old_route_version_id, new_route_version_id, pole_deleted, stale_routes }`.

- [ ] **Step 1: Failing tests**: (a) plan default → 200 with `occurrences` + `pole_orphan` bool; (b) `delete_pole:true` plan on the orphaning fixture case → `warnings` non-empty; (c) apply without `delete_pole` when `pole_orphan` → error status (via `edit_conflict`), and `GET /api/routes/{id}/directions/{dir}/stops` still lists the stop; (d) apply happy path → 200 `edit_id`, stop gone from the direction's stops
- [ ] **Step 2: Run** `cargo test -p rm-web --test api detach` — Expected: FAIL (404)
- [ ] **Step 3: Implement** handler per signatures above
- [ ] **Step 4: Run** `cargo test -p rm-web` — PASS
- [ ] **Step 5: Commit** `feat(rm-web): POST /api/edit/stop/detach — remove-stop with orphan guard`

### Task 5: POST /api/edit/polyline/{xno}/{direction}/extend

**Files:**
- Modify: `crates/rm-web/src/handlers/edit.rs` (+ `ExtensionData` mirror: `end`, `way_ids`, `new_vertices`, `snap_dist_m`)
- Modify: `crates/rm-web/src/lib.rs` (route)
- Test: `crates/rm-web/tests/extend_api.rs` (new file; copy `line_db` / `graphless_db` helpers verbatim from `crates/rm-job/tests/extend.rs:81-170`; add `tempfile` to rm-web dev-deps if absent)

**Interfaces:**
- Consumes: `rm_osm_match::extend::plan_extension`; `rm_db::LineEditCmd` / `rm_db::edit_polyline_line` (existing).
- Produces: body `{ meters: f64, end: String("both"|"first"|"last"), note?: String, apply?: Bool }`.
  - Plan: 200 `{ source: String, candidate: [[f64;2]...], extensions: [ExtensionData...], logs: [String...] }`.
  - Apply: inside ONE `run_db_write` closure — `plan_extension` → `rm_db::load_polyline_line` for `expected_updated_at` (override's `updated_at`, else None) → `rm_db::edit_polyline_line(LineEditCmd{ xno, direction, points: candidate as [lat,lon], expected_updated_at }, editor, note)` → 200 `{ edit_id, updated_at, created, source, extensions }`. ("both" with either end failing ⇒ whole thing errors, no partial write.)
  - Invalid `end` / `meters <= 0` ⇒ 400; endpoint >50 m from graph ⇒ 400 containing `"50"`; empty osm tables ⇒ 400 containing `"rm-osm-prepare"`.

- [ ] **Step 1: Failing tests** in `extend_api.rs`: (a) plan default → 200, `extensions[0].new_vertices` non-empty, `source` non-empty; (b) apply → 200 `edit_id`, then `GET /api/routes/{id}/directions/{d}/raw` effective polyline endpoint == candidate's last point; `POST /api/edit/undo/{edit_id}` restores; (c) far endpoint → 400 with `"50"`; (d) graphless → 400 with `"rm-osm-prepare"`
- [ ] **Step 2: Run** `cargo test -p rm-web --test extend_api` — Expected: FAIL (404)
- [ ] **Step 3: Implement** handler per signatures above
- [ ] **Step 4: Run** `cargo test -p rm-web` — PASS
- [ ] **Step 5: Commit** `feat(rm-web): POST /api/edit/polyline/{xno}/{direction}/extend — graph-walk line extension`

### Task 6: `apply` field on the five existing write handlers

**Files:**
- Modify: `crates/rm-web/src/handlers/edit.rs` — bodies gain `apply: Option<bool>`: `StopEditBody`, regroup body, `StationRenameBody`, `StationMoveBody`, `PolylineBody`. Mirrors: `StopEditPreviewData`, `RegroupStopPlanData`, `StationRenamePlanData`, `StationMovePlanData`, `LineEditPlanData` (fields verbatim from `rm_db::{StopEditPreview, RegroupStopPlan, StationRenamePlan, StationMovePlan, LineEditPlan}`).
- Test: `crates/rm-web/tests/api.rs`

**Interfaces:**
- Consumes: `rm_db::preview_stop_edit(conn, i64, StopEdit)`, `rm_db::plan_regroup_stop(conn, RegroupStop)`, `rm_db::plan_rename_station(conn, StationRename)`, `rm_db::plan_move_station(conn, StationMove)`, `rm_db::plan_polyline_line(conn, LineEditCmd)`.
- Produces: `apply` absent ⇒ unchanged apply behavior (existing response JSON, existing tests keep passing); `apply: false` ⇒ 200 plan mirror, **zero writes**; `apply: true` ⇒ apply.

- [ ] **Step 1: Failing tests** per handler (5×): (a) explicit `apply:false` → 200 plan JSON, and the target row/counts read unchanged afterwards; (b) body **without** `apply` → still applies (assert row changed) — the backward-compat pin
- [ ] **Step 2: Run** `cargo test -p rm-web --test api apply_field` — Expected: FAIL (`apply:false` currently applies or 400s on unknown field)
- [ ] **Step 3: Implement** the five handlers
- [ ] **Step 4: Run** `cargo test -p rm-web` — PASS (existing edit tests green = compat proven)
- [ ] **Step 5: Commit** `feat(rm-web): apply:false plan legs on the five existing edit handlers`

### Task 7: Delete rm-job; skill + docs re-point to the HTTP API

**Files:**
- Delete: `crates/rm-job/`
- Rewrite: `.claude/skills/rm-jobs/SKILL.md` → HTTP-API skill (same loop: schema-free — endpoints above; plan first via `apply:false`; quote plan; apply with bearer; `POST /api/edit/undo/{edit_id}` for wrong; refresh via `POST /api/edit/refresh` then `GET` status; `delete_pole` needs explicit human yes; warnings must be shown; base URL + token from env `RM_WEB_URL` / `RM_EDIT_TOKEN` — names are this plan's call)
- Rewrite: `docs/agents/rm-jobs.md` (same content as the skill, fuller)
- Modify: `CLAUDE.md` (2 refs), `README.md` (7 refs), `crates/rm-web/README.md` (endpoint list if present), the 2 comment refs in `crates/rm-web/src/`
- Leave: `docs/superpowers/plans|specs/2026-10-17-*` (history)

- [ ] **Step 1: `git rm -r crates/rm-job`**
- [ ] **Step 2: Fix refs** — `grep -rn "rm-job\|rm_job" --include="*.toml" --include="*.md" --include="*.rs" . | grep -v target | grep -v docs/superpowers` clean (history docs exempt)
- [ ] **Step 3: Rewrite skill + docs** per above
- [ ] **Step 4: Run** `cargo test --workspace` — Expected: PASS (no crate referenced rm-job; verified in builder check)
- [ ] **Step 5: Manual smoke**: `cargo run -r -p rm-web -- --db data/taichung.db --edit --edit-tokens agent:test123` then curl plan+apply on a throwaway DB copy — NOT taichung.db (write smoke goes on a copied file)
- [ ] **Step 6: Commit** `refactor!: remove rm-job CLI — agent edits go through rm-web API`
