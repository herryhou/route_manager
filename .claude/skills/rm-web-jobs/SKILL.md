---
name: rm-web-jobs
description: Bus route/stop correction jobs through the rm-web edit HTTP API — add a stop, move or rename a stop, remove a stop from a direction, extend or replace a route path, regroup/rename/move a station, run the stale-mark refresh, undo a previous edit. This skill should be used when the user asks to fix bus data in a route_manger city database (a pole is misplaced, a stop was deleted from the source, a route's drawn path is too short or drawn wrong, a station moved or got a new name, the map is stale after an edit).
---

# rm-web-jobs

Correct route/stop data through rm-web's edit HTTP API — dry-run first,
human-approved, undoable. Server (always release mode; `--migrate` first
for a pre-edit DB):
`cargo run -r -p rm-web -- --db data/<city>.db --edit --edit-tokens agent:<token>`.
Client env: `RM_WEB_URL` (default `http://localhost:8787`) and
`RM_EDIT_TOKEN` (the bearer token). Full runbook:
`docs/agents/rm-web-jobs.md`.

Write endpoints — POST, JSON body, bearer
`Authorization: Bearer $RM_EDIT_TOKEN`:

- `POST /api/edit/stop` — add-stop `{name, lat, lon,
  attach:["XNO:DIR[@CODE]"...], station?, note?, apply}`. Plans by default.
- `POST /api/edit/stop/detach` — remove-stop `{xno, direction, code,
  sequence?, all?, delete_pole?, note?, apply}`. Plans by default.
- `POST /api/edit/stop/{stop_version_id}` — move/rename pole `{lat?,
  lon?, stop_name?, note?, apply}`. **Applies by default.**
- `POST /api/edit/stop/{stop_version_id}/regroup` — `{new_station_id,
  note?, apply}`. **Applies by default.**
- `POST /api/edit/station/{station_version_id}` — rename `{new_name,
  level:"station|matching|all", note?, apply}`. **Applies by default.**
- `POST /api/edit/station/{station_version_id}/move` — `{lat, lon,
  note?, apply}`. **Applies by default.**
- `POST /api/edit/polyline/{xno}/{direction}/extend` — extend path
  `{meters, end:"both|first|last", note?, apply}`. Plans by default.
- `POST /api/edit/polyline/{xno}/{direction}/line` — set-line
  `{points:[[lat,lon]...], expected_updated_at?, note?, apply}`.
  **Applies by default.**
- `POST /api/edit/undo/{edit_id}` — `{note?}`.
- `POST /api/edit/refresh` — start the stale-mark chain (409 if one is
  already running); `GET /api/edit/refresh` polls `{state, step?,
  pending_marks}` until `state` is `done`/`failed`. Bearer required on
  both (the poll is the one authed GET).

Reads (GET, no auth): `/api/stations?q=` + `/api/stations/{id}` (find a
station/pole), `/api/routes` + `/api/routes/{id}/directions/{dir}/stops`
(poles in sequence, with `stop_version_id`s),
`/api/routes/{id}/directions/{dir}/raw` (effective polyline),
`/api/edit/log?entity=stop&entity_id=N` (audit trail).

## The loop

1. Reads — confirm the target pole, direction, current path state, prior
   edits; collect the ids the write paths want (`stop_version_id`,
   `station_version_id` from the reads above).
2. Dry run — the same request with `"apply": false`.
3. Quote the plan to the human; wait for a yes. Show every warning line.
4. Apply — identical request with `"apply": true`; report the `edit_id`.
   Verify with a re-read (direction stops / raw polyline) and
   `/api/edit/log` — the new edit row should be on top.
   `set-line` token loop: pass the plan's `expected_updated_at` back on
   the apply; a 409 "edited by someone else" means re-plan and take the
   fresh token.
5. Tell the human the map stays stale until the refresh chain runs (match
   → segments → registry). Run it yourself (`POST /api/edit/refresh`,
   then poll the GET; never re-POST while running), or let the human
   press rm-web's refresh button. Pick one; don't interleave.
6. Wrong? `POST /api/edit/undo/{edit_id}` — newest edit first (LIFO);
   undo refuses (409 NotUndoable) when later edits interleave or the
   edit used `delete_pole`.

## Hard rules

- ALWAYS send `apply` explicitly. The three newer endpoints (`/stop`,
  `/stop/detach`, `/polyline/.../extend`) plan when it is absent; the
  five older ones APPLY when it is absent. Never omit the field.
- Dry-run before EVERY apply. No exceptions — even when the human
  described the edit exactly.
- Quote the plan to the human: the plan response is the approval
  artifact. Never apply from memory or a paraphrase of it.
- Never `delete_pole` (detach) without an explicit human yes — it tears
  the pole down one-way and makes the whole edit NotUndoable; the orphan
  guard 400s without it.
- Never apply while the plan shows warnings the human hasn't seen. Show
  the warning lines and get a yes that covers them.
- Never hand-write SQL against the published tables — the rm-web edit
  API is the only write path. Coordinates always come from the user
  (rm-web map right-click 「複製座標」), never invented.
