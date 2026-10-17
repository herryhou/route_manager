# rm-job AI-assist add-stop Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A natural-language job path into published data: `rm_db::edit::add_stop` (insert/append a stop onto route-directions, station qualify-or-create, full re-versioning, one tagged undo) exposed through a `rm-job` CLI that is dry-run by default.

**Architecture:** New `add_stop` primitive in `rm_db::edit` following the load → plan (pure) → apply (one tx) shape of `edit_stop`/`regroup_stop`. Re-versioning mirrors `rm-publish`'s `publish_tx` (new route_version, one subroute_version, full route_stop_version re-mint for all directions, pointer move). A new `stop_add` entity kind in `edit_log` plugs into the existing `undo_edit` dispatcher. `crates/rm-job` is a thin clap CLI over `plan_add_stop` / `add_stop` / `undo_edit`.

**Tech Stack:** Rust 2021 workspace, rusqlite 0.37 (bundled), clap 4 derive (workspace dep), rm-core geo helpers (`LatLng`, `haversine_m`, `bearing_deg`, `angular_diff_deg`, `blake3_128_hex`).

**Spec:** `docs/superpowers/specs/2026-10-17-ai-assist-rm-job-design.md` — the plan argues from the spec; executors read both. Where the spec's API sketch says `route_id: i64` in `Attach`, the builder-check correction at the end of the spec supersedes it: attachments key routes by **xno string** (line-edit precedent).

## Global Constraints

- Pipeline binaries run release: `cargo run -r -p <crate>` (CLAUDE.md). Tests are `cargo test -p <crate> <filter>`.
- INSERT-only re-versioning: never UPDATE a published row. The only UPDATE is the `route` current-pointer move (`current_route_version_id`, `revision`, `updated_at`) — publish_tx precedent.
- Deterministic DB writes: no HashMap-iteration insert order; same input on a fresh DB → byte-identical rows.
- rm-job touches SQLite only through `rm_db` (`rm_db::open`). No second connection layer.
- Dry-run is the default; `--apply` performs exactly the shown plan.
- `crates/rm-job` depends on `rm_db` + clap only (workspace `members = ["crates/*"]` picks it up automatically — no root Cargo.toml edit).
- Timestamps: `rm_db::now_rfc3339()`. Ids: `rm_db::mint(&tx, "<table>")`.
- rm-web: no programmatic zoom/pan; read `crates/rm-web/DESIGN.md` before UI work; JS is `include_str!`-compiled — rebuild + hard refresh or nothing changes.
- rm-web matches `rm_db::EditError` and `UndoOutcome` exhaustively — every new variant forces an arm in `crates/rm-web` in the same commit that adds the variant.
- Published rows are never hard-deleted — except rows the job itself minted during `undo` (spec sanctions delete-own-rows for INSERT-shaped edits; the B2==B0 round-trip test is the gate).

## Review Focus

Failure modes the spec implies but no test covered by default — each is pinned by a named test below.

1. **Stop just past the off-path threshold** (e.g. 501 m from the matched path) must be rejected with both numbers in the message, not silently attached. Pinned: `plan_rejects_501m_off_path_but_499m_passes` (Task 3).
2. **Two-way axial station, approach from the opposite pole** (approach bearing ≈ stored + 180°) must qualify when the station's `max_spread_deg ≥ 45` — the axial fold, not a directional reject. Pinned: `qualify_axial_station_matches_opposite_heading` (Task 2).
3. **Another current stop with the same normalized name nearby** must warn, not reject — the plan still succeeds. Pinned: `plan_warns_but_succeeds_on_same_name_nearby` (Task 3).
4. **Route row exists but `current_route_version_id` IS NULL** (ingested, never published) must reject as unknown-or-unpublished, not panic or 500. Pinned: `plan_rejects_unpublished_route` (Task 3).
5. **Undo after a later edit moved the pointer** must refuse (`NotUndoable`) and delete nothing. Pinned: `undo_refuses_after_route_moved_on` (Task 5).

---

### Task 1: Matched-path loader + projection helpers

**Files:**
- Create: `crates/rm-db/src/edit/matched_path.rs`
- Modify: `crates/rm-db/src/edit/mod.rs` (module declaration)

**Interfaces:**
- Consumes: `rm_db::open_in_memory()` (tests), `rm_core::LatLng`, `rm_core::geo::{haversine_m, bearing_deg}`.
- Produces (used by Task 3's plan):
  - `pub fn matched_path(conn: &Connection, route_id: i64, direction: i64) -> Result<Option<Vec<LatLng>>>` — the direction's matched OSM path in travel order; `None` when the route has no current version, no subroute_version, or no `seg_match_way` rows for that direction.
  - `pub fn project_onto_path(path: &[LatLng], p: LatLng) -> Option<(f64, f64)>` — `(chainage_m, dist_m)` of the nearest point on the path; `None` for paths shorter than 2 points.
  - `pub fn bearing_at(path: &[LatLng], chainage_m: f64) -> Option<f64>` — forward tangent; at/beyond the end, the final leg (registry `polyline_bearing` semantics).
  - `pub const OFF_PATH_MAX_M: f64 = 500.0;`

- [ ] **Step 1: Write the failing tests** in `matched_path.rs` `#[cfg(test)]`

Seed by copying the seed SQL pattern from `crates/rm-registry/tests/bearing.rs` (it already builds the `route` → `route_version` → `subroute_version` → `seg_match_way` + `osm_way` → `osm_way_node` → `osm_node` chain). `seg_match_way` columns: `(subroute_version_id, direction, ordinal, osm_way_id, entry_node_id, exit_node_id, cum_start_m, cum_end_m)`; `osm_node (osm_node_id, lat, lon)`.

Tests (each asserts exact values):
- `matched_path_returns_points_in_travel_order` — a two-way run seeded entry→exit yields the sliced node coords in order.
- `matched_path_none_without_match_rows` — route with current version but zero `seg_match_way` rows → `Ok(None)`.
- `matched_path_none_for_unpublished_route` — `current_route_version_id` NULL → `Ok(None)`.
- `project_onto_path_distance_and_chainage` — point offset perpendicular from a known two-point path: exact `(chainage_m, dist_m)` within 1e-6.
- `bearing_at_mid_and_beyond_end` — mid-path bearing equals the segment bearing; chainage past the end returns the final leg bearing.

- [ ] **Step 2: Run tests, verify fail**

Run: `cargo test -p rm-db matched_path`
Expected: FAIL (module/fns not defined).

- [ ] **Step 3: Implement**

Loader resolution chain: `route.current_route_version_id` → `subroute_version WHERE route_version_id = ?` (exactly one per route) → `seg_match_way WHERE subroute_version_id = ? AND direction = ? ORDER BY ordinal` → per way, slice `osm_way_node ORDER BY ordinal` from `entry_node_id` to `exit_node_id` (copy the entry/exit slicing pattern of `osm_bearing` in `crates/rm-registry/src/bearing.rs:220`), concatenating coords from `osm_node`. Projection: per-segment nearest point in equirectangular-scaled coordinates (scale lon by cos(lat)); chainage accumulates `haversine_m` over whole legs plus the in-leg offset.

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo test -p rm-db matched_path`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/rm-db/src/edit/matched_path.rs crates/rm-db/src/edit/mod.rs
git commit -m "feat(rm-db): matched-path loader + projection helpers for add_stop

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 2: add_stop command types + station qualification

**Files:**
- Create: `crates/rm-db/src/edit/add_stop.rs`
- Modify: `crates/rm-db/src/edit/mod.rs` (module declaration)

**Interfaces:**
- Consumes: Task 1's `matched_path` + `project_onto_path` + `bearing_at`; `rm_core::geo::{haversine_m, angular_diff_deg}`; `station_bearing (station_id, bearing_deg, member_count, max_spread_deg, computed_at)`.
- Produces (used by Tasks 3–4):
  - `pub struct AddStopCmd { pub name: String, pub lat: f64, pub lon: f64, pub station_id: Option<i64>, pub attachments: Vec<AttachCmd> }`
  - `pub struct AttachCmd { pub xno: String, pub direction: i64, pub after_stop_code: Option<String> }` — `None` = append to end.
  - `pub struct StationCandidate { pub station_id: i64, pub station_version_id: i64, pub station_name: String, pub dist_m: f64 }`
  - `pub struct Qualification { pub matched: Option<StationCandidate>, pub warnings: Vec<String> }`
  - `pub fn qualify_station(conn: &Connection, name: &str, lat: f64, lon: f64, approach_bearing: Option<f64>) -> Result<Qualification>`
  - `pub const QUALIFY_MAX_DIST_M: f64 = 500.0;` `pub const QUALIFY_BEARING_TOL_DEG: f64 = 30.0;`

- [ ] **Step 1: Write the failing tests** in `add_stop.rs` `#[cfg(test)]`

Own `fixture(&conn)` seeding the full chain (mirror the fixture style of `crates/rm-db/src/edit/station_move.rs:76`, extended with `route`/`route_version`/`subroute_version`/`route_stop_version` per the rm-publish insert shapes, plus the Task 1 match tables). Tests:

- `qualify_matches_same_name_nearby` — current station, name equal after trim+case-fold, distance < 500 m, no bearing row → matched, one warning (bearing condition dropped).
- `qualify_rejects_distant_or_renamed` — same coords but different normalized name → `matched: None`; right name, 600 m away → `None`.
- `qualify_axial_station_matches_opposite_heading` — `station_bearing` row with `max_spread_deg = 60.0`, `bearing_deg = 90.0`; approach bearing 265.0° (axial opposite) → matched. Directional diff would be 175° — this test pins the fold.
- `qualify_signed_station_rejects_opposite` — `max_spread_deg = 20.0`, `bearing_deg = 90.0`, approach 265.0° → `None`; approach 100.0° → matched.
- `qualify_thresholds_500m_30deg` — exactly 500.0 m matches; 500.1 m does not. Bearing diff exactly 30.0° matches; 30.5° does not.
- `qualify_tie_break_nearest_then_lowest_id` — two qualifiers: nearer wins; equal distance → lower `station_id`.

- [ ] **Step 2: Run tests, verify fail**

Run: `cargo test -p rm-db qualify`
Expected: FAIL (not defined).

- [ ] **Step 3: Implement**

Query current stations (`station_version.valid_to IS NULL`, joined to `station` for ids) `ORDER BY station_id`, filter in Rust: normalized name equal (trim + `to_lowercase`), `haversine_m ≤ QUALIFY_MAX_DIST_M`. Bearing: read `station_bearing` for the station; when the row exists and an approach bearing was supplied — if `max_spread_deg < 45.0` (duplicate the constant locally with a comment pointing at `rm-registry::bearing::SIGNED_SPREAD_MAX_DEG`; rm-db cannot depend on rm-registry) compare `angular_diff_deg` directionally, else fold both sides to 0–180° (`let d = (a - b).rem_euclid(180.0); d.min(180.0 - d)`) and compare axially; tolerance `QUALIFY_BEARING_TOL_DEG`. No bearing row, or no approach bearing (caller passes `None` — Task 3 computes it from the first attachment with a matched path) → drop the bearing condition, push a warning. Sort qualifiers by `(dist_m, station_id)`, take the first.

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo test -p rm-db qualify`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/rm-db/src/edit/add_stop.rs crates/rm-db/src/edit/mod.rs
git commit -m "feat(rm-db): add_stop command types + spread-discriminated station qualification

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 3: `plan_add_stop` — read-only plan, rejections, EditError variants

**Files:**
- Modify: `crates/rm-db/src/edit/add_stop.rs` (plan fn + plan types)
- Modify: `crates/rm-db/src/edit/mod.rs` (EditError variants)
- Modify: `crates/rm-web/src/handlers/edit.rs:77-94` (`edit_conflict` match arms)

**Interfaces:**
- Consumes: Task 1 (`matched_path`, `project_onto_path`, `bearing_at`, `OFF_PATH_MAX_M`), Task 2 (`qualify_station`, cmd types).
- Produces (used by Tasks 4 and 6):
  - `pub struct AddStopPlan { pub name: String, pub lat: f64, pub lon: f64, pub station: StationVerdict, pub attachments: Vec<PlannedAttach>, pub row_counts: RowCounts, pub stale_routes: Vec<i64>, pub warnings: Vec<String> }`
  - `pub enum StationVerdict { UseExisting { station_id: i64, name: String }, CreateNew, Pinned { station_id: i64, name: String } }`
  - `pub struct PlannedAttach { pub xno: String, pub direction: i64, pub route_id: i64, pub insert_after_stop_code: Option<String>, pub neighbor_before: Option<String>, pub neighbor_after: Option<String> }` — neighbor names from the current sequence.
  - `pub struct RowCounts { pub stop: u32, pub stop_version: u32, pub station: u32, pub station_version: u32, pub station_group: u32, pub station_group_version: u32, pub route_version: u32, pub subroute_version: u32, pub route_stop_version: u32, pub stale_mark: u32 }`
  - `pub fn plan_add_stop(conn: &Connection, cmd: &AddStopCmd) -> Result<AddStopPlan>`
  - New `EditError` variants: `UnknownDirection(String, i64)`, `UnknownAfterStop(String, i64, String)`, `OffPathStop { xno: String, direction: i64, dist_m: f64, max_m: f64 }`, `DuplicateAttach(String, i64)`.

- [ ] **Step 1: Write the failing tests**

- `plan_resolves_route_by_xno_and_reports_verdict` — attach `1:0` append on the fixture: verdict `CreateNew` (no qualifying station in fixture), `route_id` resolved, neighbors = last stop's name / `None`.
- `plan_mid_insert_positions_and_neighbors` — `@<code>` insert: neighbors are the referenced stop and its successor.
- `plan_rejects_unknown_xno_direction_aftercode` — three asserts: unknown xno → `EditError::UnknownRoute(_)` (existing variant — xno resolution mirrors `load_polyline_line` at `crates/rm-db/src/edit/mod.rs:1049`: `WHERE ext_uid = 'tcg:{xno}'`); known route, absent direction → `UnknownDirection`; direction ok, absent stop_code → `UnknownAfterStop`.
- `plan_rejects_duplicate_attach` — same `(xno, direction)` twice → `DuplicateAttach`.
- `plan_rejects_unpublished_route` — route row with `current_route_version_id` NULL → `UnknownRoute(_)` (Review Focus 4).
- `plan_rejects_501m_off_path_but_499m_passes` — two commands whose coords sit 499 m / 501 m (haversine) from the fixture's matched path: the 499 one plans, the 501 one returns `OffPathStop { dist_m, max_m }` with `max_m == 500.0` (Review Focus 1).
- `plan_skips_offpath_check_when_no_matched_path` — attachment on a direction with no `seg_match_way` rows: no rejection, one warning.
- `plan_warns_but_succeeds_on_same_name_nearby` — fixture stop named like the cmd within 500 m: plan succeeds, warning present (Review Focus 3).
- `plan_counts_exact_for_two_attach` — attaches on two routes: counts assert the full arithmetic — `stop: 1`, `stop_version: 2`, `station: 1`, `station_version: 1`, `station_group: 1`, `station_group_version: 1`, `route_version: 2`, `subroute_version: 2`, `route_stop_version: (existing rows of both routes) + 2`, `stale_mark: 6` (2 seg_match keys + 2 path_stop_projection keys + 1 stop_bearing `'*'` + 1 station_bearing `'*'` — the `'*'`-keyed tables INSERT OR REPLACE-collapse across directions, and `mark_position_stale` only REPLACEs keys `mark_line_stale` already wrote), `stale_routes` sorted ascending.
- `plan_leaves_db_unchanged` — dump all tables before/after a planning call; identical.

- [ ] **Step 2: Run tests, verify fail**

Run: `cargo test -p rm-db plan_`
Expected: FAIL.

- [ ] **Step 3: Implement `plan_add_stop`**

Validation order (first error wins): duplicate `(xno, direction)` → xno resolution (`UnknownRoute`) → direction exists on the current route_version's `route_stop_version` rows (`UnknownDirection`) → `after_stop_code` found on that direction's current sequence by `stop_code` (`UnknownAfterStop`) → off-path (skip + warning when Task 1 loader returns `None`) → pinned `station_id` must be current (`EditError::UnknownStation`, existing variant, mirrors `regroup_stop`'s target check). Off-path: `project_onto_path(...).dist_m > OFF_PATH_MAX_M` → `OffPathStop`. Approach bearing for qualification: `bearing_at` at the projection chainage of the first attachment that yields a matched path; `None` otherwise. Then `qualify_station` → `StationVerdict`. Neighbor names read `stop_version.stop_name` via the direction's current `route_stop_version` sequence. `stale_routes` = distinct route ids of the attachments, ascending. Warnings order: station-level first (no-bearing, same-name-nearby from Task 2 + the same-name check against current `stop_version` rows within 500 m), then per-attach (no-matched-path) in attachment order.

- [ ] **Step 4: Add the EditError variants + rm-web arms**

Variants in `crates/rm-db/src/edit/mod.rs` `EditError` (thiserror display naming xno/direction/numbers). In `crates/rm-web/src/handlers/edit.rs` `edit_conflict`, map `UnknownDirection(_) | UnknownAfterStop(..) | OffPathStop { .. } | DuplicateAttach(_, _)` → `ApiError::bad_request` (400 — request problems, same bucket as `UnknownRoute`).

- [ ] **Step 5: Run tests, verify pass**

Run: `cargo test -p rm-db plan_ && cargo test -p rm-web edit`
Expected: PASS (rm-web compiles — the exhaustive match is satisfied).

- [ ] **Step 6: Commit**

```bash
git add crates/rm-db/src/edit/add_stop.rs crates/rm-db/src/edit/mod.rs crates/rm-web/src/handlers/edit.rs
git commit -m "feat(rm-db): read-only add_stop plan with pre-write rejections; rm-web 400 arms

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 4: `add_stop` — apply in one tx, edit_log, stale marks

**Files:**
- Modify: `crates/rm-db/src/edit/add_stop.rs` (apply + service fn)
- Modify: `crates/rm-db/src/edit/mod.rs` (re-export)
- Modify: `crates/rm-db/src/lib.rs` (root re-export, mirroring how `edit_stop`/`LineEditCmd` are re-exported)

**Interfaces:**
- Consumes: Task 3's `plan_add_stop`/`AddStopPlan`; `rm_db::mint`; `mark_line_stale` (`crates/rm-db/src/stale.rs:202`), `mark_position_stale` (`crates/rm-db/src/stale.rs:166`).
- Produces (used by Tasks 5–6):
  - `pub struct AddStopOutcome { pub edit_id: i64, pub stop_id: i64, pub stop_version_ids: Vec<i64>, pub station_id: i64, pub station_created: bool, pub stale_routes: Vec<i64> }`
  - `pub fn add_stop(conn: &Connection, cmd: AddStopCmd, editor: &str, note: &str) -> Result<AddStopOutcome>` — plan, then apply in one tx.
  - `pub fn apply_add_stop(conn: &Connection, plan: &AddStopPlan, editor: &str, note: &str) -> Result<AddStopOutcome>` (pub for tests; re-validates inside the tx).

- [ ] **Step 1: Write the failing tests**

Golden exact-key-set style (candidate-2 pattern): after one two-attach apply on the fixture, assert per table the **full exact set** of inserted keys — not counts:
- `apply_mints_exact_key_sets_per_table` — for each of `stop`, `stop_version`, `station`, `station_version`, `station_group`, `station_group_version`, `route_version`, `subroute_version`, `route_stop_version`: `SELECT` the id/key set and assert equality with the expected vec. Include: minted `stop_version.direction` equals the attachment direction; `stop_code = format!("M{stop_id}")`; new `route_version.version_no = old max + 1`, `status = 'PUBLISHED'`; downstream stops shifted `stop_sequence + 1` on the insert direction; sibling direction re-minted with the **same stop_version_ids** as before (only `route_stop_version` rows are new); `source_order_no` NULL on the inserted row, carried on re-minted rows; `subroute_name` carried from the old `subroute_version`.
- `apply_moves_pointer_and_bumps_revision` — `route.current_route_version_id`, `revision + 1`, `updated_at`.
- `apply_writes_one_tagged_edit_log_row` — `entity = 'stop_add'`, `action = 'add'`, `entity_id = stop_id`, `changes` carries `kind`, `stop_id`, `station_id`, `station_created`, `group_created`, `stop_version_ids`, and per route: `route_id`, `old_rv`, `new_rv`, `old_revision`, `old_updated_at`, `new_sv_id`, `new_rsv_ids` (exact JSON key set asserted).
- `apply_marks_line_and_position_stale` — `stale_mark` rows carry `cause_edit_id = edit_id`; all four datasets per attached direction (per `mark_line_stale`'s test at `crates/rm-db/src/stale.rs:434`); `stale_routes` returned.
- `apply_rejects_when_route_moved_between_plan_and_apply` — plan, then publish-style pointer move, then `apply_add_stop` → `EditError::NotCurrent(_)` and the dump is unchanged (in-tx re-validation).
- `apply_rejection_leaves_db_byte_identical` — a rejected plan (Task 3 error) through `add_stop`: full dump identical (zero-partial-write).

- [ ] **Step 2: Run tests, verify fail**

Run: `cargo test -p rm-db apply_`
Expected: FAIL.

- [ ] **Step 3: Implement**

One `unchecked_transaction` (mirrors `apply_stop_edit`). In-tx re-validation: each route's `current_route_version_id` must still equal the planned old rv, else `EditError::NotCurrent(old_rv)`.

Mint order (pinned, deterministic — Vec-driven, never HashMap iteration):
1. Station (when `CreateNew`): `station_group` (ext_uid = `"tcg:grp:" + rm_core::blake3_128_hex(station_ext_uid)` — inline the `rm_identity::group_ext_uid` formula; rm-db cannot depend on rm-identity) → `station_group_version` (name = the stop name) → `station` (ext_uid = `format!("manual:{station_id}")`) → `station_version` (`source = 'manual'`, coords = cmd lat/lon).
2. `stop` (ext_uid = `format!("manual:{stop_id}")`).
3. Per attachment (in order): `stop_version` (`station_id` resolved, `stop_code = format!("M{stop_id}")`, `stop_name`, `direction`, coords, `source = 'manual'`).
4. Per route (attachment first-appearance order): `route_version` (`version_no = max + 1`, `valid_from = now`, `valid_to NULL`, `status = 'PUBLISHED'`, `published_at = now` — exact publish_tx column list at `crates/rm-publish/src/lib.rs:174`) → `subroute_version` (subroute_id from `subroute WHERE route_id = ?`, name carried) → `route_stop_version` re-mint for **all** directions ascending (publish_tx column list incl. `source_order_no`), inserting the new stop at the planned position on its direction, appending when `after_stop_code: None`.
5. `UPDATE route SET current_route_version_id, revision = revision + 1, updated_at` (publish_tx:213).
6. One `edit_log` row with the Task-4-test payload (insert column list mirrors `apply_stop_edit`).
7. `mark_line_stale` per attached `(route_id, direction)` + `mark_position_stale` per new `stop_version`, all with `cause_edit_id = edit_id`.

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo test -p rm-db apply_`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/rm-db/src/edit/add_stop.rs crates/rm-db/src/edit/mod.rs crates/rm-db/src/lib.rs
git commit -m "feat(rm-db): add_stop applies station+stop+re-versioning in one tagged tx

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 5: Undo — dispatch arm, net-zero round-trip, guards

**Files:**
- Modify: `crates/rm-db/src/edit/add_stop.rs` (`undo_stop_add`)
- Modify: `crates/rm-db/src/edit/mod.rs` (`undo_edit` dispatch arm + `UndoOutcome` variant)
- Modify: `crates/rm-web/src/handlers/edit.rs` (undo handler arm for the new `UndoOutcome` variant)

**Interfaces:**
- Consumes: Task 4's `edit_log` payload; `EditError::NotUndoable`.
- Produces: `UndoOutcome::StopAdd { edit_id: i64 }` (new serde-tagged variant, tag mirrors `edit_log.entity` = `"stop_add"`); `pub fn undo_stop_add(conn: &Connection, edit_id: i64, editor: &str, note: &str) -> Result<EditOutcome>` wired into `undo_edit` before the stop fallback.

- [ ] **Step 1: Write the failing tests**

Reuse the dump helper pattern from the line-undo repro tests (`crates/rm-db/src/edit/line.rs:328+`):
- `undo_roundtrips_add_to_byte_identical` — apply → undo → full dump of all published + derived tables **excluding `edit_log` and `stale_mark`** is byte-identical to the pre-apply dump (B2==B0). This test pins the rollback order — reorder the deletes and it fails.
- `undo_deletes_own_stale_marks_only` — seed a foreign stale mark (different `cause_edit_id`) on the same table/key; after undo the job's marks are gone, the foreign one remains.
- `undo_refuses_after_route_moved_on` — apply job, then a second `add_stop` on the same route (or a pointer move); undoing the first → `NotUndoable`, dump unchanged (Review Focus 5).
- `undo_of_undo_refused` — `action = 'undo'` rows are `NotUndoable` (station_undo precedent).
- `undo_edit_dispatches_stop_add` — through the generic `undo_edit(conn, edit_id, ...)`: returns `UndoOutcome::StopAdd`.
- `determinism_same_add_two_fresh_dbs` — same cmd on two `open_in_memory()` dbs → byte-identical dumps including `edit_log`/`stale_mark` (no time-of-day dependence: `now_rfc3339` is fine, both runs share the second only if the helper pins `now` — assert on dumps with `created_at`-bearing columns normalized by replacement, the line-undo repro's approach).

- [ ] **Step 2: Run tests, verify fail**

Run: `cargo test -p rm-db undo_ determinism_`
Expected: FAIL.

- [ ] **Step 3: Implement `undo_stop_add`**

Load the `stop_add` row via `load_stop_edit`; guard `action != "undo"` → `NotUndoable`. Guard: for every route in the payload, `route.current_route_version_id == new_rv` and the route's current `route_stop_version` id set equals the payload's `new_rsv_ids` — else `NotUndoable(edit_id)`. Then, in one tx, the pinned delete order: new `route_stop_version` rows → new `subroute_version` → new `route_version` → pointer restore (`current_route_version_id = old_rv`, `revision = old_revision`, `updated_at = old_updated_at`) → minted `stop_version` rows → `stop` → `station_version`/`station` (only when `station_created`) → `station_group_version`/`station_group` (only when `group_created`) → `stale_mark WHERE cause_edit_id = edit_id`. Write the undo's own `edit_log` row (`action = 'undo'`, `changes.kind = 'stop_add_undo'`). Add the `undo_edit` dispatch arm and the `UndoOutcome::StopAdd` variant; add the matching arm in rm-web's undo handler (same commit — exhaustive match).

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo test -p rm-db undo_ determinism_ && cargo test -p rm-web`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/rm-db/src/edit/add_stop.rs crates/rm-db/src/edit/mod.rs crates/rm-web/src/handlers/edit.rs
git commit -m "feat(rm-db): net-zero undo for stop_add jobs via undo_edit dispatch

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 6: `crates/rm-job` CLI

**Files:**
- Create: `crates/rm-job/Cargo.toml` (deps: `rm-db = { workspace = true }`, `clap.workspace = true`, `anyhow`)
- Create: `crates/rm-job/src/main.rs` (thin: parse + `std::process::exit(rm_job::run(cli))`)
- Create: `crates/rm-job/src/lib.rs` (`pub fn run(cli: Cli) -> i32` — testable)
- Create: `crates/rm-job/tests/cli.rs`

**Interfaces:**
- Consumes: `rm_db::{open, plan_add_stop, add_stop, undo_edit, AddStopCmd, AttachCmd, AddStopPlan, AddStopOutcome}`.
- Produces: binary `rm-job` with the spec's UX — `add-stop "name" --lat F --lon F [--attach XNO:DIR[@CODE]]... [--station ID] [--db PATH] [--editor NAME] [--note TEXT] [--apply]` and `undo EDIT_ID [--db PATH] [--editor NAME]`.

- [ ] **Step 1: Write the failing tests** in `tests/cli.rs`

Fixture: mirror `crates/rm-web/tests/edit.rs`'s `fixture()` chain (station_group → station → stop → route → route_version → subroute_version → route_stop_version) on a `tempfile` db. `run()` returns the exit code; capture stdout via `gag`-free approach — have `run` take a `report: &mut String` sink (or return the report alongside the code) so tests assert content without process capture.

- `attach_parses_xno_dir_and_code` — `"1:0"`, `"23:1@C"`, reject `"1"`, `"1:x"`, `"1:0@"` (exit 1, usage line).
- `dry_run_is_default_exit_0_prints_verdict_and_counts` — report contains `create new station` (or the matched verdict), per-attach lines with route/direction/neighbors, the `stale_mark` count, and the stale routes line; db dump unchanged after.
- `apply_writes_and_reports_ids` — `--apply`: report contains edit_id, stop_id, station_id; readback asserts the stop exists; exit 0.
- `unknown_route_exit_1_reason_on_stderr_sink` — exit 1, message names the xno.
- `empty_attachments_exit_2` — no `--attach` → exit 2.
- `undo_subcommand_roundtrips` — apply then `undo <edit_id>`: dump (excl. `edit_log`/`stale_mark`) back to pre-apply; exit 0.

- [ ] **Step 2: Run tests, verify fail**

Run: `cargo test -p rm-job`
Expected: FAIL (crate doesn't exist).

- [ ] **Step 3: Implement**

`AttachCmd` parsing: split `'@'` (insert-after code) then `':'` → `(xno, direction)`. Dry-run calls `rm_db::plan_add_stop` on `rm_db::open(&db)` and prints: station verdict line, one line per attach (`route {xno} dir {d}: insert after {code|end}, between {before} → {after}`), per-table row counts from `RowCounts`, `refresh needed for routes: ...`, warnings. `--apply` calls `add_stop` and prints ids + stale routes. `undo` wraps `rm_db::undo_edit` (prints the outcome variant; rm-web's history panel will not show job edits — the CLI is the undo surface). Exit codes: 0 ok, 1 rejected (reason to stderr), 2 no-op (no attachments).

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo test -p rm-job`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/rm-job
git commit -m "feat(rm-job): dry-run-first CLI for add-stop jobs over rm_db::edit

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 7: rm-web copy-coords context menu

**Files:**
- Modify: `crates/rm-web/assets/js/app.js` (after map creation, `app.js:9` region)

**Interfaces:**
- Consumes: Leaflet `map.contextmenu` event. Produces: clipboard text `"<lat>, <lon>"` (6 decimal places).

- [ ] **Step 1: Add the handler** (read `crates/rm-web/DESIGN.md` first)

```js
// contextmenu → clipboard coordinates for the rm-job workflow (docs/agents/rm-jobs.md).
map.on('contextmenu', (e) => {
  navigator.clipboard?.writeText(`${e.latlng.lat.toFixed(6)}, ${e.latlng.lng.toFixed(6)}`);
});
```

No viewport movement (whitelist stands), no endpoint, no state.

- [ ] **Step 2: Verify**

Run: `node --check crates/rm-web/assets/js/app.js && cargo build -r -p rm-web`
Expected: both succeed (JS is `include_str!`-compiled; the release build proves it still compiles).

- [ ] **Step 3: Commit**

```bash
git add crates/rm-web/assets/js/app.js
git commit -m "feat(rm-web): copy-coords context menu for rm-job workflows

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

Note for the human: browser verification (right-click → paste) is a manual pass — `include_str!` means rebuild + hard refresh.

---

### Task 8: Runbook + CLAUDE.md pointer

**Files:**
- Create: `docs/agents/rm-jobs.md`
- Modify: `CLAUDE.md` (Agent skills section — one pointer line, matching the issue-tracker/domain-docs pattern)

- [ ] **Step 1: Write the runbook**, matching the style of `docs/agents/issue-tracker.md`. Contents (spec §Runbook): when to reach for rm-job; **the dry-run-first rule — never `--apply` before the plan has been shown to the user**; the attach syntax (`route:direction` append, `route:direction@code` insert); `undo <edit_id>`; the refresh rule — the job ends at stale marks and a report, a human presses rm-web's refresh (chain: match → segments → registry); the copy-coords map gesture as the source of coordinates.

- [ ] **Step 2: Add the CLAUDE.md pointer** under "### Agent skills":

```markdown
### AI-assist jobs

rm-job CLI for natural-language data jobs — dry-run first, never `--apply`
without showing the plan. See `docs/agents/rm-jobs.md`.
```

- [ ] **Step 3: Verify + commit**

Check the doc renders sensibly (paths/flags match Task 6's actual CLI — read `--help` output: `cargo run -r -p rm-job -- add-stop --help`).

```bash
git add docs/agents/rm-jobs.md CLAUDE.md
git commit -m "docs: rm-jobs runbook for agent-driven data jobs

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 9: E2E on a copy of a city DB

**Files:** none (verification only). Never touches a live DB.

- [ ] **Step 1: Copy the DB** (safe while rm-web is idle; use `.backup` for consistency)

```bash
sqlite3 data/taipei.db ".backup /tmp/rm-job-e2e.db"
sqlite3 /tmp/rm-job-e2e.db "SELECT route_id, ext_uid FROM route WHERE current_route_version_id IS NOT NULL LIMIT 3"
```

Pick an xno and a direction that exists (`SELECT DISTINCT direction FROM route_stop_version rsv JOIN subroute_version sv ON ... JOIN route_version rv ON ... WHERE rv.route_id = ?`).

- [ ] **Step 2: Dry-run → show plan → apply**

```bash
cargo run -r -p rm-job -- add-stop "E2E測試站" --lat <picked> --lon <picked> --attach <xno>:<dir> --db /tmp/rm-job-e2e.db --editor herry
cargo run -r -p rm-job -- add-stop "E2E測試站" --lat <picked> --lon <picked> --attach <xno>:<dir> --db /tmp/rm-job-e2e.db --editor herry --apply
```

Verify: the plan line names create-or-match; after apply, `sqlite3 /tmp/rm-job-e2e.db "SELECT stop_code, stop_name, direction FROM stop_version WHERE stop_name='E2E測試站'"` shows the minted rows; the route's version_no incremented.

- [ ] **Step 3: Undo → byte-diff**

```bash
cargo run -r -p rm-job -- undo <edit_id> --db /tmp/rm-job-e2e.db
sqlite3 /tmp/rm-job-e2e.db ".dump" | grep -v -E "edit_log|stale_mark" > /tmp/after.sql
```

Diff `/tmp/after.sql` against the same dump taken **before apply** (snapshot it in step 2). Expected: empty diff.

- [ ] **Step 4: Clean up + report**

`rm -f /tmp/rm-job-e2e.db* /tmp/before.sql /tmp/after.sql`. Report the E2E result (plan output, undo diff empty or not) to the user.

---

## Left to the builder (from the spec, unchanged)

- Exact `AddStopOutcome`/plan field spelling may adjust as long as the Interfaces blocks here stay consistent.
- The off-path projection's equirectangular approximation details (any sane local-flat approximation within the 500 m threshold is fine — the boundary test pins behavior, not math).
- `--db` default (cwd `route.db` vs required flag) — follow how other pipeline binaries handle it.
