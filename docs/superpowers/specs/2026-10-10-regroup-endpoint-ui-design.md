# Scenario 4 — Regroup endpoint + UI (map-click target pick) — Design

Date: 2026-10-10
Status: approved in chat; spec for review
Builds on: scenario 2 (`0fad928`) — `rm_edit::regroup_stop` + undo proven, no
HTTP surface; phase-1 web edit mode (`05f5d64`) — token, edit toggle, stop
panel rename/move

## Context

The regroup primitive shipped in scenario 2 (`RegroupStop` →
`regroup_stop`, edit_log `action='move_station'`, undo covered by the
generic stop undo) but has no endpoint and no UI. This scenario wires both,
following the established edit-UI patterns: bearer-token edit mode,
prompt-to-confirm dialogs, one-shot map interaction, status-bar errors.

Decisions made in chat:

1. **Target pick is map-click** — the editor clicks the destination
   station's location on the map (same interaction family as move).
2. **Click resolves via a nearest-station endpoint** — one SQL query,
   works at any zoom, no marker/layer dependency.
3. **Scope is regroup only** — undo button / edit-log view and the
   bearing-refresh runbook stay out (later scenarios).

## Backend

**Read.** `GET /api/stations/nearest?lat=&lon=&max_m=` — new query
`queries::stops::station_nearest` + handler in `handlers/stops.rs`.

- Unauthenticated (reads stay open, like all query endpoints).
- `max_m` required, capped at 500 (larger → 400); lat/lon range-checked
  as in the edit handler.
- SQL: bbox prefilter on current `station_version` rows (index-friendly),
  haversine distance in the SELECT, `ORDER BY distance LIMIT 1`.
- Hit → `{station_id, station_name, latitude, longitude, distance_m}`;
  miss → 404 `no station within {max_m} m`.

**Write.** `POST /api/edit/stop/{stop_version_id}/regroup` — new handler
in `handlers/edit.rs`, body `{new_station_id, note?}`.

- `Editor` + `ApiJson` + `run_db_write`, mirroring `edit_stop`.
- Calls `rm_edit::regroup_stop(conn, RegroupStop { stop_version_id,
  new_station_id }, &editor, &note)`.
- Errors route through the existing `edit_conflict` map, already
  exhaustive: 409 `UnknownStation` / `NotCurrent`, 400 `NoOpEdit`
  (picked station equals current). `note` optional server-side, required
  by the UI (same split as rename/move).
- Response `{edit_id, new_stop_version_id}` — same keys as the other
  stop edits. The regroup primitive already writes
  `action='move_station'` (reserved by `0010_edit`); no migration.

No rm-db or rm-edit changes — both layers are complete and tested.

## UI (`assets/js`)

Stop panel (edit mode) gains a third button beside ✎ rename and 移動位置
move: **換站** (regroup). Flow, reusing the `pendingPlace` one-shot
pattern:

1. Click 換站 → status `click the target station`.
2. Next map click (self-deregistering; a new pending handler replaces
   any old one) → `GET /api/stations/nearest?lat&lon&max_m=100`.
3. Miss (404) → status `no station within 100 m`, flow ends.
4. Hit → confirm dialog: `regroup stop #{stop_code}\n{old_name} →
   {new_name} ({distance_m} m)`. Decline → status cleared, flow ends.
5. Picked `station_id` equals the stop's current `station_id` (available
   in the stop-detail payload) → status `already at {name}`, no POST.
6. Accept → required note prompt (empty note cancels, as move does) →
   `POST /api/edit/stop/{svi}/regroup`.
7. Success → status `regrouped`, `refreshAfterEdit()`,
   `selectStop(new_stop_version_id)` — new version id from the response.
8. Failure → `editFail('regroup', e)`; 401/403 drops the stored token as
   today.

`api.js` gains `stationNearest(lat, lon, maxM)` and
`regroupStop(svi, body)` alongside `editStop`/`undoEdit`.

## Testing

- `rm-web` handler/query tests (Rust only — no JS test infra exists):
  - `station_nearest`: hit returns nearest of several seeds with
    distance; miss → 404; `max_m` missing/over-cap/range → 400.
  - regroup endpoint: happy path mints a new svi and an
    `action='move_station'` log row; unknown target → 409; same
    station → 400; malformed body → 400; stale svi → 409.
- rm-edit/rm-db regroup behavior is already pinned (scenario 2) — no
  duplication here.
- Workspace `cargo test` green (226 tests today).

## Out of scope

Undo button + edit-log view in the UI, bearing-refresh runbook wiring,
station move (`station_version` lat/lon), any rm-edit/rm-db change.
