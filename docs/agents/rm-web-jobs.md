# Agent data jobs through the rm-web edit API

Natural-language data jobs ("move the 大坑橋 stop", "add a stop X between A
and B on route 12", "extend route 1's path past its end") run through
rm-web's edit HTTP API — thin JSON handlers over `rm_db`'s manual-edit
primitives. Every write is planned before it happens (the newer endpoints
dry-run by default), audited in `edit_log`, and (barring the one-way
exception below) undoable — `refresh` is the exception: it re-runs the
pipeline chain rather than making an edit. Always release mode:

```sh
cargo run -r -p rm-web -- ...
```

## Server and auth

Start the server against the city DB and enable the edit endpoints:

```sh
cargo run -r -p rm-web -- --db data/taichung.db --edit --edit-tokens agent:<token>
```

- `--edit` layers in the POST `/api/edit/...` routes — without it they 404.
- `--edit-tokens` takes comma-separated `editor:token` pairs. The KEY is
  the editor name recorded in `edit_log` ("who made this edit"); the VALUE
  is the bearer token. The server also accepts them via the
  `RM_EDIT_TOKENS` env.
- Another city than Taichung: point `--db` at it (`--db data/taipei.db`).
  A pre-edit DB needs its migrations first:
  `cargo run -r -p rm-web -- --db <path> --migrate` (applies them, exits).
- Default listen address `0.0.0.0:8787` (`--addr` moves it).

The agent configures two env vars (shell profile or per session):

- `RM_WEB_URL` — the server's base URL, default `http://localhost:8787`.
- `RM_EDIT_TOKEN` — the agent's bearer token (the VALUE half of one
  `editor:token` pair).

Every POST to `/api/edit/...` carries
`Authorization: Bearer $RM_EDIT_TOKEN`: missing ⇒ 401, unknown token ⇒ 403.
GETs — the reads and `GET /api/edit/log` — need no auth (the log is not
secret; notes are for the team). The one GET exception:
`GET /api/edit/refresh` (the refresh poll) takes the bearer like the
POSTs.

## Endpoints

Read (GET, no auth):

| Endpoint | Returns |
|---|---|
| `GET /api/routes` | Route list (id, xno, names) — resolve an `xno` to the `{id}` the other reads want |
| `GET /api/routes/{id}/directions/{dir}/stops` | One direction's stops in sequence, each with its `stop_version_id` |
| `GET /api/routes/{id}/directions/{dir}/raw` | The direction's effective polyline: source, encoded, endpoints |
| `GET /api/stations?q=<name>` | Station search — finds stations by name |
| `GET /api/stations/{id}` | One station (by stable `station_id`): members + routes, and its current `station_version_id` |
| `GET /api/stops/{stop_version_id}` | One stop's detail: station, group, bearings, serving routes |
| `GET /api/edit/log?entity=stop&entity_id=N` | One entity's audit rows from `edit_log`, newest first (`entity` = stop/station/polyline/stop_add/stop_detach) |

Write (POST, bearer auth, JSON body):

| Endpoint | Body | Does |
|---|---|---|
| `POST /api/edit/stop` | `{name, lat, lon, attach, station?, note?, apply?}` | New pole + station, attached to one or more route directions |
| `POST /api/edit/stop/detach` | `{xno, direction, code, sequence?, all?, delete_pole?, note?, apply?}` | Detach a stop occurrence from one route direction |
| `POST /api/edit/stop/{stop_version_id}` | `{lat?, lon?, stop_name?, note?, apply?}` | Move/rename one pole (writes a new stop version) |
| `POST /api/edit/stop/{stop_version_id}/regroup` | `{new_station_id, note?, apply?}` | Move one pole to a different station |
| `POST /api/edit/station/{station_version_id}` | `{new_name, level, note?, apply?}` | Rename a station, optionally dragging matching poles (`level` = `station\|matching\|all`) |
| `POST /api/edit/station/{station_version_id}/move` | `{lat, lon, note?, apply?}` | Move a station's coordinates |
| `POST /api/edit/polyline/{xno}/{direction}/extend` | `{meters, end, note?, apply?}` | Walk the OSM road graph outward from the path ends; writes a polyline override |
| `POST /api/edit/polyline/{xno}/{direction}/line` | `{points, expected_updated_at?, note?, apply?}` | Replace one direction's whole polyline; writes a polyline override |
| `POST /api/edit/undo/{edit_id}` | `{note?}` | Revert one applied edit by its `edit_log` id |
| `POST /api/edit/refresh` | — | Run the stale-mark pipeline chain (match → segments → registry); not an edit — no audit row, not undoable |

`POST /api/edit/refresh` answers 202 and runs the chain in the background;
poll `GET /api/edit/refresh` for `{state, step?, pending_marks, ...}` until
`state` is `done` or `failed`. A second POST while one runs is 409.

### The `apply` field — always send it

**Every write request states `apply` explicitly. Never omit it — the two
halves of the API disagree on the default.**

- `POST /api/edit/stop`, `POST /api/edit/stop/detach` and
  `POST /api/edit/polyline/{xno}/{direction}/extend`: `apply` absent or
  false ⇒ read-only plan; only `apply: true` writes.
- The five older handlers (`.../stop/{stop_version_id}`, `.../regroup`,
  `.../station/{station_version_id}`, `.../move`,
  `.../polyline/{xno}/{direction}/line`): `apply` absent or true ⇒ they
  WRITE; `apply: false` is the only way to get their plan.

`apply: false` answers the underlying `rm_db` plan struct with its field
names verbatim — add-stop: station verdict, attachments with sequence
context, row_counts, stale_routes, warnings; detach: occurrences,
pole_orphan, serving_directions, warnings; extend: source, candidate,
extensions, logs; the others: the preview op with before/after values.
`apply: true` performs the write and returns the outcome — every apply
response carries `edit_id` (extend/line include the override's
`updated_at`).

## Status codes

JSON in, JSON out; errors are `{"error": "..."}`:

- `400` — request problems: malformed body, empty attach list, extend
  rejections (bad meters/end, endpoint beyond the 50 m snap limit, empty
  osm tables), detach's orphan guard (an apply that would orphan the pole,
  naming `delete_pole`).
- `401` / `403` — missing / unknown bearer token.
- `404` — unknown route/stop/station, or the server was started without
  `--edit` (the edit routes don't exist at all).
- `409` — races and refusals: version no longer current, the set-line
  "edited by someone else" token mismatch, and every undo refusal
  (`NotUndoable`).

## The canonical job loop

0. Server up? `curl -s "$RM_WEB_URL/api/routes"` — a cheap liveness probe.
1. Ground truth first (all GET, no auth): which pole (`/api/stations?q=`
   → `/api/stations/{id}`), which direction
   (`/directions/{dir}/stops`), what the path looks like now
   (`/directions/{dir}/raw`), prior edits (`/api/edit/log`). Collect the
   ids the write paths want (`stop_version_id`, `station_version_id`).
2. Dry run — the same request with `"apply": false`.
3. Show the human the plan and wait for a yes. **The plan is the approval
   artifact** — quote it, never apply from memory or a paraphrase of it.
   Show every warning line and get a yes that covers them.
4. Re-send the identical request with `"apply": true`; report the
   `edit_id` it returns to the user. Verify by re-reading (the direction's
   stops / raw polyline) and by one `GET /api/edit/log?...` — the new edit
   row should be on top.
5. Wrong? `POST /api/edit/undo/{edit_id}`.

```sh
curl -s "$RM_WEB_URL/api/edit/stop" \
  -H "Authorization: Bearer $RM_EDIT_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"name":"新站","lat":24.1776,"lon":120.7395,"attach":["1:0"],"apply":false}'
```

## When to reach for the edit API

- The user asks in natural language for a route/stop correction on the
  published data: a **new stop**, a stop **moved or renamed**, a stop
  **removed from a direction**, a station **regrouped / renamed / moved**,
  a path **extended or replaced**, or an **undo** of a previous edit.
  The rm-web edit API (and rm-web's own edit UI) is the only write path —
  never hand-write SQL against the published tables.
- Station surgery beyond the endpoints — splitting and merging stations —
  goes through rm-web's edit UI (backed by the same `rm_db::edit`
  primitives); ask the human to do it there.

## Warnings that matter

- **Release mode.** Debug builds are 10–30x slower — `cargo run -r -p
  rm-web -- ...` always.
- **`delete_pole` is one-way.** `POST /api/edit/stop/detach` with
  `delete_pole: true` tears the orphaned pole down and makes the whole
  edit `NotUndoable`. The plan reports `pole_orphan` /
  `serving_directions`; an apply that would orphan the pole is refused
  with a 400 naming `delete_pole`. Never send it without an explicit
  human yes.
- **rm-web's derived layers stay stale** after any apply until the
  refresh chain runs — see the refresh rule below.
- **The plan is the approval artifact** — always show it before applying.

## add-stop specifics

**The dry-run-first rule.** **Never send `apply: true` before the plan has
been shown to the user.** The plan carries the station verdict, the insert
position and neighbours per attachment, per-table row counts, stale
routes, warnings.

By default the stop joins an existing station when name, distance and
bearing all qualify, else creates a standalone one; `"station": <id>` pins
the station and skips the search.

**Attach syntax.** `attach` is an array of `XNO:DIR[@CODE]` strings (`XNO`
= route number, `DIR` = direction); resolve `XNO` against `GET /api/routes`:

- `"12:1"` — append the stop to the end of route 12 direction 1.
- `"12:1@0045"` — insert after the stop whose `stop_code` is `0045` on
  that direction; downstream stops re-version.
- Several entries land one stop on several directions in a single job.

An empty `attach` array is a 400. Coordinates come from the user:
right-clicking the rm-web map opens a small menu — 「複製座標 <lat>,
<lon>」 — and clicking the menu item copies `<lat>, <lon>` (six decimals)
to the clipboard. Ask the user to copy the spot and paste the coordinates
— never invent or look them up.

## move-stop and detach specifics

Move/rename addresses the pole by **`stop_version_id`** in the path — the
direction-stops read (`/api/routes/{id}/directions/{dir}/stops`) and the
station detail print it; there is no xno+code resolver on the wire. Give
at least one of lat/lon/`stop_name`; the plan shows exactly the fields
that would change.

`POST /api/edit/stop/detach` identifies the occurrence by `xno` +
`direction` + `code`. When the code occurs more than once in the direction
the plan says so — disambiguate with `sequence` (the `stop_sequence` to
take) or `all`. The plan reports `pole_orphan` / `serving_directions`;
if applying would orphan the pole, the apply is refused with a 400 naming
`delete_pole` — nothing detaches and the pole keeps serving as before.
Decide `delete_pole` only then (one-way; explicit human yes required).

## extend and set-line specifics

`extend` snaps the direction's path ends to the OSM road graph and walks
outward `meters` from the chosen end. `end` is required (`both`, `first`
or `last`) — there is no default; omitting it is a 400. The apply
writes the whole extended polyline as a polyline override, so it needs the
OSM graph tables in the DB (`rm-osm-prepare`) — without them the plan
rejects with a 400 telling you to run rm-osm-prepare. A plan whose
`extensions` come back empty means the graph offers no extension within
the requested distance.

`set-line` (`/line`) replaces a direction's whole polyline; `points` is an
array of `[lat, lon]` pairs (the server encodes). It uses a
**concurrency-token loop**: the plan returns `expected_updated_at`; pass
it back as `expected_updated_at` on the apply. Once an override exists the
token is mandatory — an apply without it is refused with the "edited by
someone else" 409, and the recovery is simply re-plan and take the fresh
token. (The overwrite plan wants the token too — it guards identically.)

## Station specifics

Look stations up by the **stable `station_id`** (`GET /api/stations?q=`
finds it; the station detail prints it) — that is also what regroup's
`new_station_id` wants. The rename and move endpoints take the station's
current **`station_version_id`** in the path — the station detail response
carries it; re-read it after any edit rather than caching ids across jobs.

Regroup's plan shows both stations by id and name; it stales
`station_bearing` only. Rename's `level` picks how far the rename reaches:
`station` renames the station alone, `matching` also renames poles whose
name byte-equals the old station name, `all` renames every pole of the
station — the plan lists exactly which poles would follow. Rename and
move write no stale marks: names and coordinates are read live.

## Undo

Apply responses carry `edit_id`; keep it and report it to the user.
`POST /api/edit/undo/{edit_id}` reverts the whole edit — every row the
apply wrote — in one transaction. Undo in LIFO order — newest edit first:
edits interleaved after one (rm-web pole edits included) make its undo
refuse with 409 `NotUndoable` instead of rewinding over them — and so
does any edit that used `delete_pole`. Unknown ids surface as the same
409.

## The refresh rule

**An apply ends at stale marks; the map stays stale until the refresh
chain runs (match → segments → registry — registry rebuilds the edited
stops' bearings).** Two ways to run it:

- The API — the agent path. `POST /api/edit/refresh` starts the chain
  (202; single-flight — a second POST while running is 409) and
  `GET /api/edit/refresh` reports `{state, step, pending_marks, ...}`;
  poll until `state` is `done`, or `failed` (the error lands in the
  status). The poll GET is authed — carry the bearer on it, unlike the
  other GETs. Safe to rerun after a failure: marks carry their causing
  `edit_id`, so a rerun clears what is still stale and leaves the rest.
- rm-web's refresh button — the human path.

Either is fine; pick one, don't interleave. The chain holds a write
connection for minutes on a big DB while it works.
