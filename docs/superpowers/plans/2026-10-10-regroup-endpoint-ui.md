# Scenario 4 — Regroup endpoint + UI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (inline) to implement this plan task-by-task. Steps use RED-GREEN order.

**Goal:** Wire the scenario-2 regroup primitive (`rm_edit::regroup_stop`) to an HTTP endpoint and a map-click UI flow, plus the nearest-station lookup the click needs.

**Spec:** `docs/superpowers/specs/2026-10-10-regroup-endpoint-ui-design.md` (binding authority)

**Tech Stack:** Rust workspace (axum 0.8, rusqlite 0.37 bundled — SQLite math functions available in SQL), vanilla JS in `crates/rm-web/assets/js` (no JS test infra).

## Global Constraints

- No rm-db / rm-edit changes — both layers complete and tested (scenario 2, `0fad928`).
- `edit_conflict` in `handlers/edit.rs` is already exhaustive — do not modify it.
- Nearest endpoint is unauthenticated and lives in `handlers::router()` (reads stay open); regroup is edit-gated and layered in `lib.rs::router_with` like the other POST edits.
- Route order safe: axum 0.8 / matchit gives static segments priority, so `/api/stations/nearest` wins over `/api/stations/{id}`.
- Match existing style: `Query(HashMap)` parsing in `handlers/stops.rs`, `ApiJson` + `run_db_write` + `edit_conflict` in `handlers/edit.rs`, light SQL fixtures in `tests/edit.rs`.
- Workspace `cargo test` green at every task boundary (226 tests today).

## Review Focus

1. **Bbox prefilter correctness at |lat| high / max_m near cap** — the lon delta must widen with cos(lat); a too-narrow bbox silently misses stations the haversine would accept.
2. **acos domain safety** — floating point can push the haversine cos term infinitesimally above 1.0 → SQL NULL/domain error. Must clamp.
3. **UI station-id source** — `d.stop` carries no `station_id`; the current station id must come from `d.station.station_id` (passed through `stopPanel`).
4. **404 vs error routing in the UI flow** — nearest-miss (404) ends the flow with a status line; 401/403 must still drop the stored token (via `editFail`).

---

### Task 1: `GET /api/stations/nearest` — query + handler + route

**Files:**
- Modify: `crates/rm-web/src/queries/stops.rs` (add `station_nearest` after `station_search`)
- Modify: `crates/rm-web/src/handlers/stops.rs` (add `station_nearest` handler after `station_detail`)
- Modify: `crates/rm-web/src/handlers/mod.rs` (route `/api/stations/nearest`)
- Test: `crates/rm-web/tests/nearest.rs` (new file)

**Interfaces:**
- Produces (Task 3 consumes):
  - `GET /api/stations/nearest?lat=&lon=&max_m=` → hit `{"station_id","station_name","latitude","longitude","distance_m"}` (distance_m = f64 meters, full precision); miss → 404 `no station within {max_m} m`; `max_m` missing or outside 0.0..=500.0 → 400; lat/lon missing/out-of-range → 400.
  - `pub fn station_nearest(conn: &Connection, lat: f64, lon: f64, max_m: f64) -> Result<Option<Value>>`

- [ ] **Step 1 (RED):** write `crates/rm-web/tests/nearest.rs`: two-station fixture (station 1 'A' at 24.0,120.0; station 2 'B' at 24.0,120.002 — ≈203 m apart), tests:
  - `nearest_returns_closest_station_with_distance` — query at 24.0,120.0005 with max_m=100 → 200, station_id 1, station_name "A", distance_m within 45.0..=56.0.
  - `nearest_misses_when_nothing_within_max_m` — same point, max_m=40 → 404, error contains "no station within 40 m".
  - `nearest_validates_params` — missing lat; missing lon; lat 91; lon 181; missing max_m; max_m 501; max_m -1 → all 400.
- [ ] **Step 2 (RED):** `cargo test -p rm-web --test nearest` → compile error (no `station_nearest`) or failures. **Expected:** fails because endpoint absent.
- [ ] **Step 3 (GREEN):** implement query + handler + route. SQL: bbox prefilter on current `station_version` rows (lat_delta = max_m/111320.0, lon_delta = lat_delta / cos(lat) with |cos| clamped ≥ 0.01, computed in Rust, passed as params); haversine in SELECT with `MIN(1.0, …)` clamp; `ORDER BY distance LIMIT 1`.
- [ ] **Step 4 (GREEN):** `cargo test -p rm-web --test nearest` → 3/3 pass. **Expected:** all pass.
- [ ] **Step 5:** `cargo test -p rm-web` whole crate green (no route shadowing of `/api/stations/{id}` — `station_detail` test still passes).

### Task 2: `POST /api/edit/stop/{svi}/regroup` — handler + route

**Files:**
- Modify: `crates/rm-web/src/handlers/edit.rs` (`RegroupBody` + `regroup` after `edit_stop`)
- Modify: `crates/rm-web/src/lib.rs` (route in the edit-gated block)
- Test: `crates/rm-web/tests/edit.rs` (append; local helper inserts station 2 'T' at 24.0,120.0, station_version 2, group 1)

**Interfaces:**
- Consumes: `rm_edit::regroup_stop(conn, RegroupStop { stop_version_id, new_station_id }, &editor, &note)` (exists).
- Produces (Task 3 consumes): body `{new_station_id, note?}` → `{edit_id, new_stop_version_id}`; errors via `edit_conflict`: same station → 400 `NoOpEdit`, unknown target → 409 `UnknownStation`, stale svi → 409 `NotCurrent`, malformed body → 400.

- [ ] **Step 1 (RED):** append tests to `crates/rm-web/tests/edit.rs`:
  - `regroup_happy_path_mints_version_and_move_station_log` — POST `/api/edit/stop/10/regroup` `{"new_station_id":2,"note":"fix"}` → 200; `new_stop_version_id > 10`; readback: stop_version row has `station_id = 2`, `source = 'manual'`; `edit_log.action = 'move_station'`.
  - `regroup_rejects_unknown_target` — new_station_id 999 → 409.
  - `regroup_rejects_same_station` — new_station_id 1 → 400.
  - `regroup_validates_body` — `{}`, `{"new_station_id":"x"}` → 400.
  - `regroup_stale_svi_conflicts` — regroup, then repeat with svi 10 → 409.
- [ ] **Step 2 (RED):** `cargo test -p rm-web --test edit` → new tests fail (404: route absent). **Expected:** exactly that failure.
- [ ] **Step 3 (GREEN):** implement `RegroupBody` + `regroup` handler (mirror `undo`: `Editor` + `ApiJson` + `run_db_write` + `edit_conflict`, response `{edit_id, new_stop_version_id}`); register route in `lib.rs`.
- [ ] **Step 4 (GREEN):** `cargo test -p rm-web --test edit` → all pass. **Expected:** all pass.
- [ ] **Step 5:** `cargo test -p rm-web` green.

### Task 3: UI — 換站 button + map-click flow

**Files:**
- Modify: `crates/rm-web/assets/js/api.js` (add `stationNearest`, `regroupStop`)
- Modify: `crates/rm-web/assets/js/panels.js` (third mini-btn 換站; pass station id to `onEditStop`)
- Modify: `crates/rm-web/assets/js/app.js` (`regroup` branch in `onEditStop`, signature gains `stationId`)

**Interfaces:**
- Consumes: Task 1 endpoint (`hit.station_id`, `hit.station_name`, `hit.distance_m`), Task 2 endpoint (`r.new_stop_version_id`).

- [ ] **Step 1:** api.js — `stationNearest = (lat, lon, maxM) => fetchJson('/api/stations/nearest?lat=${lat}&lon=${lon}&max_m=${maxM}')`; `regroupStop = (svi, body) => editFetch('/api/edit/stop/${svi}/regroup', body)`.
- [ ] **Step 2:** panels.js — add `${editable ? '<button class="mini-btn" data-act="regroup" title="regroup stop to another station">換站</button>' : ''}` after the move button; mini-btn wiring becomes `onEditStop(el.dataset.act, d.stop, d.station.station_id)`.
- [ ] **Step 3:** app.js — `onEditStop(act, stop, stationId)`; `regroup` branch = pendingPlace one-shot: status `click the target station` → click → `stationNearest(lat, lng, 100)`; 404 → status `no station within 100 m`; `hit.station_id === stationId` → status `already at ${hit.station_name}`; confirm `regroup stop #{stop_code}\n{old} → {new} ({Math.round(distance_m)} m)`; decline → status ''; empty note → status ''; POST → `regrouped` + `refreshAfterEdit()` + `selectStop(r.new_stop_version_id)`; failure → `editFail('regroup', e)` (401/403 drops token).
- [ ] **Step 4:** `node --check` on the three JS files; `cargo test -p rm-web` green (assets routes still serve).

### Task 4: Workspace suite + review

- [ ] `cargo test` (workspace) green — expected 226 + new.
- [ ] `cargo clippy -p rm-web` no new warnings.
- [ ] Final whole-branch review per executing-plans.
