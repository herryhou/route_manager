# rm-web edit mode — design spec

Date: 2026-10-08
Status: approved design, pre-implementation

## Context

rm-web is a read-only viewer over the pipeline DB: axum server, all-GET routes,
every connection opened via `rm_db::open_readonly`. The pipeline (rm-ingest →
rm-identity → rm-osm-match → rm-registry/rm-segments → rm-verify) builds the
DB from captured Taichung feeds.

We need editing: fix bad data that QA surfaces, curate structure (station
grouping, stop↔station assignment), and author new data. Editors are a small
trusted team. Light auth, no concurrent-edit conflicts expected on the same
row.

## Decisions

1. **Approach A — edit mode inside rm-web.** Same server, same UI, new
   POST-family endpoints behind an `--edit` flag. No separate service.
2. **Edit as new versions.** Manual edits append new rows to the versioned
   tables (`stop_version`, `station_version`) and close the previous row's
   `valid_to`. The DB becomes the working master.
3. **One-shot import.** After rm-identity finishes a batch, the import is
   closed; re-import into the same DB is structurally refused. No replay
   machinery, no upstream-vs-manual conflict model. A future capture starts a
   fresh DB file. Incremental import into a working DB is an explicit
   non-goal (possible future spec).
4. **Small-team auth.** Per-editor tokens resolve to an editor name; shared
   read access stays open.

Why version-append rather than in-place UPDATE (the split.rs precedent):
schema-native current-row discipline (`valid_to IS NULL` remains the single
definition of "current"), history lives in the schema not just in logs, and
undo is a chain walk. Cost is FK repointing (below), accepted.

## Edit primitive

One transactional helper (new `rm-edit` crate or a module in rm-db; decide at
plan time) used by every edit endpoint:

```
edit(conn, entity, stable_id, changes, editor, note, undo_of = None):
  BEGIN TX
    old = current version row (valid_to IS NULL) for stable_id
    UPDATE old SET valid_to = now
    new_id = mint(next version id)
    INSERT new version row: old fields + changes applied,
           valid_from = now, source = 'manual'
    -- FK graph maintenance (stop edits):
    UPDATE route_stop_version SET stop_version_id = new_id
     WHERE stop_version_id = old_id
    DELETE FROM stop_bearing WHERE stop_version_id = old_id
    INSERT edit_log (ts, editor, entity, stable_id, action, changes JSON, note, undo_of)
    INSERT stale_mark rows for affected derived data (see map)
  COMMIT
```

- `stable_id` is the entity id (`stop_id`, `station_id`), not the version id —
  edit_log survives version churn.
- Endpoints and the client address entities by the **current version id**
  (`stop_version_id`, `station_version_id`) — that is what every panel, popup
  and URL hash already carries, and the detail JSON does not expose the
  stable id. The server resolves version id → stable id, asserts the version
  is current (`valid_to IS NULL`), then edits.
- `changes` JSON: `{field: {old, new}}`.
- Undo: same primitive in reverse — closes the current (manual) version,
  re-validates the predecessor, logs `action = 'undo'` referencing
  `undo_of = edit_id`. Undo of an undo is a forward edit again.
- Station regroup (move a stop to another station) is the same primitive with
  `changes = {station_id: {old, new}}`; FK maintenance repoints nothing extra
  (`stop_version.station_id` is the link).

## Schema (migration `0010_edit`)

```sql
CREATE TABLE edit_log (
  edit_id   INTEGER PRIMARY KEY,
  ts        TEXT NOT NULL,
  editor    TEXT NOT NULL,
  entity    TEXT NOT NULL,          -- 'stop' | 'station' | ...
  entity_id INTEGER NOT NULL,       -- stop_id / station_id (stable)
  action    TEXT NOT NULL,          -- 'update' | 'move_station' | 'undo' | 'create'
  changes   TEXT NOT NULL,          -- JSON {field: {old, new}}
  note      TEXT,
  undo_of   INTEGER REFERENCES edit_log(edit_id)
);

CREATE TABLE stale_mark (
  table_name    TEXT NOT NULL,      -- derived table that is stale
  key           TEXT NOT NULL,      -- unit the refresh step consumes
  cause_edit_id INTEGER NOT NULL REFERENCES edit_log(edit_id),
  marked_at     TEXT NOT NULL,
  PRIMARY KEY (table_name, key)
);
```

`source = 'manual'` on version rows marks provenance; no per-table editor
column (edit_log is the audit trail).

## One-shot import guard

Today rm-identity is INSERT-only; re-running it on a working DB would
duplicate rows silently. The guard:

- rm-identity refuses to run unless the batch status is `OPEN`
  (`rm_db::latest_open_batch` / explicit batch lookup).
- On success, identity itself sets the batch `CLOSED`
  (`rm_db::set_batch_status`) inside the same transaction.

rm-ingest can still stage a new batch; identity will never consume a closed
one. Policy becomes mechanism.

## Staleness & refresh

Dependency map — what each edit invalidates:

| Edit | Stale |
|---|---|
| stop lat/lon | `path_stop_projection` (per serving route-direction), `stop_bearing`, `station_bearing` |
| stop name | nothing |
| stop → other station | `station_bearing` (both sides), split clusters |
| station name | nothing |

The edit primitive writes `stale_mark` rows in the same tx. Read queries LEFT
JOIN `stale_mark` to render an amber badge on affected panels. Refresh steps
recompute, then delete the marks they satisfied, in the same tx.

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

## Server write path & auth

- `rm-web --edit --edit-tokens name1:tok1,name2:tok2` (or `RM_EDIT_TOKENS`).
  Without `--edit`, no write routes are mounted at all.
- Auth middleware on `/api/edit/*`: `Authorization: Bearer <token>`; token
  resolves to editor name (written to edit_log); unknown token → 403; absent
  → 401. Reads stay unauthenticated.
- `run_db_write`: `rm_db::open()` (mutable, pragmas, migrations) per request
  on `spawn_blocking`, serialized by an app-level `tokio::Mutex`; WAL keeps
  viewers working during writes; busy_timeout is the backstop.
- Endpoints (phase 1):
  - `POST /api/edit/stop/{stop_version_id}` — body
    `{lat?, lon?, stop_name?, note}`; position edits include the
    moved-distance sanity check client-side.
  - `GET  /api/edit/log?entity=&entity_id=` — history for a panel.
  - `POST /api/edit/undo/{edit_id}`.
- `/api/stats` response gains `edit_enabled: bool`; the client hides the edit
  toggle when false.

## UI

- **Edit toggle** in the topbar (visible only when `edit_enabled`). Click →
  token prompt → token held in sessionStorage; all edit fetches carry it.
- **Drag-to-fix stop position**: in edit mode, stop markers become draggable.
  Drop → confirm dialog (distance moved in meters + note) → POST → panel
  re-renders from the new version row.
- **Rename**: pencil icon → inline input on name fields in stop/station
  panels.
- **Stale badge**: amber chip on stop/station panels whose derived data is
  marked stale.
- **History + undo** (phase 2 UI): per-entity edit_log list with revert.
- Every write requires an explicit confirm with a note. No silent edits.

## Non-goals

- Incremental re-import / upstream-vs-manual conflict resolution.
- Multi-writer concurrency beyond the app-level mutex (small team).
- UI buttons that trigger refresh/pipeline runs.
- Per-field permissions, user management UI.

## Phasing

1. **Foundation + stop vertical slice**: edit primitive, `0010_edit`
   migration, auth + `--edit`, drag-to-fix + rename stop, stale marks
   written, CLOSED-batch guard, history endpoint.
2. **Station curation**: regroup stop → station, station rename, history +
   undo UI.
3. **Refresh runbook** (shipped): refresh `--stale` wiring (rm-segments +
   rm-registry), mark clearing, stale badges verified accurate.
4. **Authoring**: new stops/stations, route-stop links.

## Testing

- Edit primitive on in-memory DB (per-crate `tests/` fixture pattern):
  version chain (old closed, new current, `source='manual'`), FK repointing,
  edit_log row, stale marks per dependency map, undo restores predecessor.
- Handler tests via the axum router: 401 (no token), 403 (bad token), write
  routes absent without `--edit`, happy-path write + readback.
- One-shot guard: identity refuses a CLOSED batch.
- JS: no test infra exists; keep `api.js` edit helpers thin and verify
  manually (drag, rename, badge).

## Open details (resolve at plan time)

- `rm-edit` crate vs module in rm-db.
- Exact `stale_mark` key shapes per table (`route:<id>` vs raw id) — pick
  what the refresh CLI filters consume.
- `ensure_schema` behavior on read-only viewer connections vs the new
  migration (viewer must not require write access to see the tables).
- Whether `--edit-tokens` also accepts a file to keep tokens out of shell
  history.
