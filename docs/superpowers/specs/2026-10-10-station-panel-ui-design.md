# Scenario 7 — Station panel UI (rename · move · edit-log undo) — Design

Date: 2026-10-10
Status: approved in chat; spec for review
Builds on: scenario 1 (`1443d48`) — `rename_station` + rename levels;
scenario 6 (`358b1e2`) — `move_station`, `undo_station`, kind-dispatched
`plan_station_undo`; scenarios 4–5 — endpoint/UI patterns (`armPlace`,
`editLogPanel`, mini-btns, `edit_conflict`)

## Context

Every station-side primitive is shipped and tested, but `rm-web` is
untouched by them: the station panel has no edit buttons (audit note from
the move-stop UX pass) and no station undo endpoint exists (scenario 6
note). This scenario closes both gaps by replaying the established
stop-side patterns against station primitives.

Decisions made in chat:

1. **One scenario** for rename + move + station undo/log — all backends
   exist; splitting adds ceremony without de-risking.
2. **Rename level is prompted** — name, then level (1 station-only /
   2 matching poles / 3 all poles, default 3), with a client-side match
   preview computed from the payload already in hand.
3. **One undo route with entity dispatch** — `POST /api/edit/undo/{edit_id}`
   peeks the row's entity and dispatches. This also kills a latent trap:
   today a station `edit_id` posted there feeds a station row into the
   stop undo path (`load_stop_edit` is any-entity by design).

## Backend

**Read.** `GET /api/stations/{id}` gains `station_version_id` on the
`station` object (`queries::stops::station_detail`). Rename and move key
on the svi — `load_station_snapshot` refuses non-current versions — and
the panel only has `station_id` today. Same one-line gap scenario 5
fixed for `stop_id`; TDD'd the same way.

**Rename.** `POST /api/edit/station/{station_version_id}` — new handler
in `handlers/edit.rs`, body `{new_name, level, note?}`.

- `level` wire values `"station" | "matching" | "all"`, mapped to
  `rm_edit::RenameLevel::{StationOnly, MatchingPoles, AllPoles}`;
  anything else → 400.
- Empty/whitespace name needs no client short-circuit — the backend
  already maps `EmptyNewName` → 400 and `NoOpRename` (new == old) →
  400 via `edit_conflict`.
- Calls `rm_edit::rename_station`; response
  `{edit_id, new_station_version_id}` (`poles` in `RenameOutcome` is
  not surfaced — the UI re-fetches detail).

**Move.** `POST /api/edit/station/{station_version_id}/move` — body
`{lat, lon, note?}`; lat/lon range checks mirror `edit_stop` (400).
Calls `rm_edit::move_station`; response `{edit_id,
new_station_version_id}`. Moving a station does NOT move its poles —
`apply_station_move` touches only `station_version`; the confirm dialog
says so.

**Undo dispatch.** `POST /api/edit/undo/{edit_id}` — existing handler
gains a peek: new `rm_db::edit_entity(conn, edit_id) -> Option<String>`
(one SELECT of `entity` from `edit_log`; rm-db's only change in this
scenario). `Some("station")` → `rm_edit::undo_station`, response
`{edit_id, restored_station_version_id}` (matches `UndoOutcome`);
anything else (`"stop"`, `None`) → the existing `undo_stop` path with
its response byte-unchanged, so unknown ids still 409 `NotUndoable`
today's way. `undo_station` refuses stop rows on its own
(`undo_station_rename_refuses_stop_edit_row` pins this) — dispatch is
routing, not a guard.

All three handlers reuse `Editor` + `ApiJson` + `run_db_write` +
`edit_conflict` — the map is already exhaustive on station variants
(`EmptyNewName`, `NoOpRename`, `StationNotCurrent`). `note` optional
server-side, required by the UI (same split as every edit so far).

No rm-edit changes. rm-db gains only `edit_entity`.

## UI (`assets/js`)

`api.js` gains `renameStation(svi, body)` and `moveStation(svi, body)`
beside `editStop`; `undoEdit` is reused as-is (dispatch is server-side).

`panels.js`:

- `stationPanel` gains an `onEditStation` callback param and, under the
  same `editable` gate as `stopPanel`, three header mini-btns:
  ✎ (rename) · 移動 (move) · ↩ 歷史 (edit log).
- `changeText` gains station branches keyed on `changes.kind`
  (station rows always carry one): `kind: 'station_rename'` →
  `名: {old_name} → {new_name} · {level}`; `kind: 'station_move'` →
  `移動: {lat},{lon} → {lat},{lon}`. Existing badges cover
  `update`/`undo`.

`app.js` — `onEditStation(act, d)` (receives the full station-detail
payload so previews need no extra fetch):

1. **rename** → `prompt` new name (cancel or unchanged aborts) → match
   preview: `d.stops.filter(s => s.stop_name === oldName).length` of
   `d.stops.length` poles byte-match (exact bytes, matching the
   backend's no-trim rule; 0 is legitimate) → `prompt` level `1/2/3`
   default 3 → confirm summary (`{old} → {new} · level · N/M poles
   renamed`) → required note → `renameStation` → status `renamed` →
   `refreshAfterEdit()` + `selectStation(station_id)`.
2. **move** → status `click the new station position` →
   `armPlace([st.lat, st.lon], ...)` (crosshair + elastic guide + Esc
   teardown come free) → confirm `{dist} m — stops stay put` →
   required note → `moveStation` → status `moved` → `refreshAfterEdit()`
   + `selectStation(station_id)`.
3. **log** → `api.editLog('station', station_id)` → `editLogPanel`
   (exact-undo-hint and `canUndo = editMode` logic are already
   entity-agnostic) → per-row ↩ → confirm + required note → `undoEdit`
   → `refreshAfterEdit()` + `selectStation(station_id)` + log re-fetch.
   The query filters `entity='station'`, so only station rows render.

`selectStation` and `openStationPopup` both pass `onEditStation`;
`editFail` semantics (401/403 drops the stored token) carry over
untouched.

## Testing

- `rm-web/tests/edit.rs` (Rust only — no JS test infra exists; the
  hand-rolled edit fixture lives here, so endpoint tests belong beside
  it, not in `api.rs` whose pipeline fixture is heavier than needed):
  - station payload exposes `station_version_id`.
  - rename: happy path mints a new station version + a
    `kind: 'station_rename'` log row; each level maps (pole names in
    response DB state); `"matching"` with zero byte-matches succeeds
    (station-only effect); bad level → 400; empty name → 400;
    new == old → 400; stale svi → 409.
  - move: happy path mints + `kind: 'station_move'` log row; lat/lon
    out of range → 400; stale svi → 409.
  - undo dispatch: a station rename row undone via the generic route
    restores the predecessor (`restored_station_version_id`); a station
    move row ditto; an undo row re-undone → 409; a stop row still
    responds in the stop shape (existing tests already pin the stop
    path — one explicit dual-entity case added).
  - `edit_entity`: known id → entity, unknown → `None`.
- rm-edit / rm-db edit behavior is already pinned (scenarios 1, 3, 6) —
  no duplication here.
- Workspace `cargo test` green (255 tests today + new).
- Frontend verified by e2e drive checklist in the plan (sessionStorage
  token before boot, edit toggle after boot, `cargo build -r -p rm-web`
  + restart before driving — assets are `include_str!`-baked).

## Out of scope

Parked minors (nearest radius-cut branch test, haversine domain-clamp
note, `openEditLog` stale-seq guard), stop-panel behavior changes beyond
the shared undo route, any rm-edit change, pole-coords editing (poles
move via the stop panel), station group editing.
