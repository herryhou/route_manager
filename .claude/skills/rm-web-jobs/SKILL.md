---
name: rm-web-jobs
description: Bus route/stop correction jobs through the rm-web edit HTTP API — add a stop, move or rename a stop, remove a stop from a direction, extend or replace a route path, regroup/rename/move a station, run the stale-mark refresh, undo a previous edit — and read-only route schematic diagrams (路線簡圖) generated from the same API via scripts/route_diagram.py. This skill should be used when the user asks to fix bus data in a route_manger city database (a pole is misplaced, a stop was deleted from the source, a route's drawn path is too short or drawn wrong, a station moved or got a new name, the map is stale after an edit) or to produce a route diagram / 站序圖 / 路線簡圖.
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

## Route schematic diagrams (路線簡圖)

`scripts/route_diagram.py` renders one route as a print-ready HTML/SVG
schematic: fetch via the rm-web API **only** (never direct sqlite), DP-align
stops onto matched way runs, lay out each direction by density — <30 stops
draws one left→right line, otherwise a ⊃ hairpin (first half of the stops
left→right on the top line, one smooth turn at the right, second half back
right→left; first stop top-left, last stop bottom-left); 去程 above 返程.
Name columns scale with row pitch up to 2.2× (25 stops/side ≈1.8×,
23 ≈2.1×), so the canvas height follows the content (1123 wide × dynamic).
Needs only the GET API (no `--edit`, no token):

```bash
python3 .claude/skills/rm-web-jobs/scripts/route_diagram.py \
  --query 5 --operator 全航客運 --db taichung.db
```

- `--query N` resolves the route by `route_code` (exact match preferred);
  `--route-id N` skips the search. `--num/--start/--end/--operator/--db`
  override the title block (`--city` sets 臺中市/臺北市 for the eyebrow and
  caption); the title line beside the route number is the
  API's `subroute_name` verbatim, captions use the drawn first/last stops.
- Writes `route<N>-diagram.html` in the cwd (`--out` to move it) plus
  `<out>.d/<route_id>/` — cached API dumps and `check.txt` with per-stop
  road assignment and snap distances.
- Built-in checks: monotonic stop sequence, per-direction counts; check.txt
  flags any snap >150 m — inspect those stops before trusting the road
  names shown.
- Verify visually in a browser (fonts load from Google Fonts; offline the
  names still render via fallback fonts but paren arcs may sit slightly
  off). Hand the HTML to the human; printing is A4 landscape (server-side
  print is not part of the skill).

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
