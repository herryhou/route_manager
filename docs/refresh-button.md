# rm-web refresh button — design spec

Date: 2026-10-12
Status: shipped

## Context

After web edits, derived data (way matches, stop projections, bearings) is
stale until someone runs the manual three-command chain
(`rm-osm-match --stale` → `rm-segments --stale` → `rm-registry --stale`).
For daily curation that is friction: three commands, in order, on the right
DB, not forgotten.

Spike measurements behind this design (2026-10-12, release build):

- One-direction stale rematch: Taichung route 391:0 (shortest path) 0.49 s
  end-to-end; Taipei 353:0 (shortest path) 4.11 s; Taipei 1:0 / 2:0
  11.0 s / 8.1 s.
- Graph load + build is ≤ 0.5 s in both cities (the Taichung total proves
  it). Profiler shows the Taipei cost inside `viterbi::evaluate` + A*
  `BinaryHeap` — per-direction match cost scales with street density
  around the path, not with path length or graph size.
- Consequence: a polyline-edit batch costs seconds to a few minutes of
  background compute; storage changes (Parquet, graph cache) would target
  the wrong 0.5 s.

`stale_mark` rows already accumulate across edits and one pass consumes
all of them; per-mark clears are `cause_edit_id`-guarded against the
rowid-reuse race (da4d93b).

## Decisions

1. **Explicit button, not a background loop.** An **Apply updates**
   button in the rm-web topbar triggers the refresh. The user decides
   when — after one edit or after a curation session. This deliberately
   revises the web-edit-mode non-goal "UI buttons that trigger
   refresh/pipeline runs": that non-goal guarded against ad-hoc,
   unsupervised long runs; the button is explicit, authed, and
   single-flight.
2. **In-process library calls, no child binaries.** The server calls
   `run_stale` entry points of rm-osm-match / rm-segments / rm-registry
   directly. No sibling-binary lookup problem; the original "server never
   spawns pipeline binaries" decision gets stronger. The OSM road graph
   (100–200 MB) is resident only while a polyline refresh runs.
3. **Background job + status endpoint, not synchronous HTTP.** A batch
   can run for minutes; the POST returns immediately. UI polls status.
4. **Single-flight.** One refresh at a time per server process; a second
   POST gets `409`. Matches the app-level write mutex's single-writer
   assumption.
5. **Crash safety is inherited, not built.** The job holds no durable
   state: server restart resets status to `idle`, leftover `stale_mark`
   rows survive, the badge stays amber, the next click (or CLI) heals.
   A step that dies mid-run leaves its marks — never green over stale
   data.
6. **No new locks.** The refresh connection is `rm_db::open` (mutable) on
   `spawn_blocking`, like `run_db_write`. It does not take the app write
   mutex (a step can run for minutes; edits must not block that long).
   Correctness under concurrent edits rests on WAL + `busy_timeout` and
   the `cause_edit_id` guard: an edit landing mid-run re-marks, and the
   guard stops the running pass from clearing the fresh mark.

Out of scope (revisit only if the manual clicking annoys): an
auto-refresh debounce loop, per-direction progress, a cancel button,
multi-instance single-flight (a second rm-web on the same DB is already
unsupported by the edit mutex).

## API

Both routes sit behind the existing `/api/edit/*` token middleware.

- `POST /api/edit/refresh`
  - `202` — job started: `{}` (body reserved for future use)
  - `409` — refresh already running
  - `401` / `403` — as every edit endpoint
- `GET /api/edit/refresh` — status snapshot (read, but grouped under
  `/api/edit/*` so the UI logic stays in one place):

```json
{
  "state": "idle | running | done | failed",
  "step": "match | segments | registry",     // when running
  "started_at": "...",                        // running / done / failed
  "finished_at": "...",                       // done / failed
  "error": "message",                         // failed only
  "pending_marks": 7                          // always: COUNT(*) FROM stale_mark
}
```

Status lives in `AppState` behind a `std::sync::Mutex<RefreshStatus>`.
`done` / `failed` stay visible until the next POST (so a failed run's
error does not vanish).

## Refresh chain

Run in dependency order (matches feed bearings; projections feed
bearings); stop at the first error and record it:

1. `rm_osm_match::run_stale` — re-match `seg_match` marks from the
   effective polyline (override ?? src). No marks → `nothing stale`.
2. `rm_segments::run_stale` — reproject `path_stop_projection` marks.
3. rm-registry stale entry — bearing rebuild + station split when
   bearing marks exist. If the logic lives only in the CLI `main`
   today, expose it as a `pub fn` (expected small refactor).

Each step's existing summary line goes to the server log (tracing
info).

## UI

- **Apply updates** button (「套用更新」) in the topbar, visible only in
  edit mode.
- Pending count badge (from `pending_marks`); disabled at 0.
- Click → confirm dialog (the "every write requires an explicit confirm"
  rule) → POST → button shows a spinner with the current step
  ("matching… / projecting… / bearings…"), polling the status endpoint
  every 2 s.
- `done` → spinner stops; badges turn green as panels re-read.
  `failed` → red note with `error`; button re-enabled.
- Without `--edit`, no button (same gate as the edit toggle).

## Documentation edits (at implementation time)

- `docs/web-edit-mode.md`: § *Staleness & refresh* — button flow is the
  primary path, the manual three-command chain stays as the ops
  fallback; § *Server write path & auth* — the two endpoints; § *UI* —
  the button; § *Non-goals* — replace the anti-button line with the
  explicit-button decision; § *Phasing* — this feature as phase 5.
- `README.md`: one line in the rm-web section — edit mode has an
  Apply-updates button; the `--stale` commands remain the CLI
  equivalent.

## Testing

- Handler tests via the axum router: 401 (no token), 403 (bad token),
  409 (second POST while running), 202 then status transitions
  `running → done`.
- Server test (tempfile DB): POST a polyline edit → marks appear → POST
  refresh → poll status to `done` → `stale_mark` empty,
  `seg_match_way` / `path_stop_projection` / `stop_bearing` updated,
  `edit_log` untouched by the refresh.
- Crash property: simulate a failure between steps (e.g. registry
  error) → marks of later steps remain, status `failed`, next POST
  succeeds.
- Unit: status state machine transitions (idle→running→done/failed,
  409 on running).
- UI: manual verification (no JS test infra, existing pattern) — button
  states, spinner, error note, badge clearing.

## Phasing

Phase 5 of edit mode; independent of phase 4 (authoring) — can land
before or after it.
