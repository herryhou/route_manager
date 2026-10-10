# Refresh runbook wiring — design

Date: 2026-10-10
Status: approved in chat, pending spec review
Prior scenarios: station move + kind-agnostic undo (`358b1e2`); see memory + `2026-10-10-station-move-design.md`

## Problem

Every edit writes `stale_mark` rows; rm-web renders amber badges from them.
Nothing ever deletes the marks, so badges stay amber forever and the only
refresh is a full pipeline re-run. `docs/web-edit-mode.md` phase 3 planned
this as "`--routes` filter in rm-osm-match, bearing re-run steps, mark
clearing", but two of its premises are wrong:

1. **Wrong binary.** `path_stop_projection` is written by **rm-segments**
   (per-route_version DELETE+INSERT, `rm-segments/src/lib.rs:107,177`), not
   rm-osm-match. Stop edits never change road-graph matches (runs); they
   invalidate the projection of the moved stop onto the path. rm-osm-match
   re-runs stay a whole-city, re-import-only step.
2. **Wrong order.** `rm-registry bearing::run` reads
   `path_stop_projection` (`bearing.rs:44,79`): bearings derive from
   projections. The doc's runbook (bearings step 1, projections step 2)
   would rebuild bearings from stale projections.

## Decisions made in chat

- **`--stale` mode, not explicit ids.** rm-segments and rm-registry each
  gain a `--stale` flag: the CLI reads `stale_mark` itself, refreshes
  exactly what is marked, clears the satisfied marks. Runbook = two fixed
  commands; no ids to copy from badges.
- **Rowid snapshot against the concurrent-edit race.** Someone edits from
  the browser while a refresh runs. Naive "recompute, then DELETE by key"
  would sweep away a mark written during the run. Instead `--stale`
  snapshots the target rows' rowids at startup and the satisfying
  transaction deletes exactly those rowids; rows inserted after the
  snapshot survive.
- **Mark writes become `INSERT OR REPLACE`.** Schema check during design:
  `stale_mark` PK is `(table_name, key)` and mark writes use
  `INSERT OR IGNORE` — re-marking an existing key keeps the original
  rowid, so a concurrent edit re-marking `(stop_bearing,'*')` mid-run
  would still be lost. With OR REPLACE each mark is a fresh row (new
  rowid), making the rowid snapshot sound. Freshest `cause_edit_id` wins,
  which is also the more truthful audit. rm-db `edit.rs` mark helpers +
  inline sites switch IGNORE → REPLACE; existing tests unchanged in
  intent (idempotency now means "single row after re-mark", not
  "first write kept").

## rm-segments `--stale`

- Startup: `SELECT rowid, key FROM stale_mark WHERE table_name =
  'path_stop_projection'` → snapshot rowids, parse `key` as route_id.
- Intersect with the routes present in the latest batch (existing
  route_version iteration). A marked route absent from the batch is left
  marked with a warning (cannot reproject what has no polylines).
- Per marked route_version: DELETE projections for that route_version,
  reproject — all inside the run's existing single whole-run
  `unchecked_transaction` (`run()` already works this way), with the
  snapshot rowid delete appended at the end of that same tx. (Plan-time
  correction: the per-route-tx wording above was wrong about current
  code; no tx refactor.)
- No marks → print `nothing stale`, exit 0. Behavior without the flag is
  byte-identical to today.

## rm-registry `--stale`

- Startup: snapshot rowids of all `stop_bearing` and `station_bearing`
  marks (both are whole-table `'*'` keys; one rebuild satisfies both).
- Any snapshot rows → run the existing `main.rs` flow: full bearing
  rebuild, split (idempotent, cheap), conditional station re-bear — then
  one tx deleting all snapshot rowids.
- No bearing marks → `nothing stale`, exit 0, split skipped (split
  without fresh bearings is pointless work).
- A regroup edit marks `station_bearing` only; the same whole-table
  rebuild covers it, so no separate gating.

## Mark-clearing contract

`--stale` = snapshot rowids → recompute → delete exactly those rowids.
Both CLIs already do their whole run in one `unchecked_transaction`, so
the delete rides the same tx as the recompute it satisfies (rm-registry's
split/stations steps keep their own txs; the mark delete is a final
statement in the main rebuild tx). Crash ordering is safe: a crash before
the delete leaves marks present over fresh data
(badge amber until the next refresh — never green over stale data). No
clock comparisons; correct under any interleaving with web edits (web
writes stay behind the app mutex; CLIs take the DB directly — the rowid
rule, not locking, is what makes the interleaving safe). `stale_mark` is
an ordinary rowid table (`0010_edit.sql:21`, no WITHOUT ROWID).

## Docs

- `web-edit-mode.md` "Staleness & refresh": correct the runbook order
  (segments → bearings → split), replace the three-step prose with the two
  `--stale` commands, note the re-import-only role of rm-osm-match.
- README runbook section mirrors the two commands if it documents refresh.
- Dependency map table in the doc gains the projection row already
  implied by `mark_position_stale` (no semantic change; text alignment).

## Testing

- rm-db: OR REPLACE re-mark leaves one row with the new `cause_edit_id`
  and a fresh rowid (pins the race fix).
- rm-segments: `--stale` reprojects only marked routes; per-route tx
  clears exactly that route's snapshot rowids; unmarked routes untouched;
  a mark inserted after snapshot (rowid > snapshot) survives; absent
  route warned + left marked; `nothing stale` short-circuit.
- rm-registry: marks cleared after rebuild; `nothing stale` skips split;
  zero-partial (rebuild fails → marks still present).
- Badge accuracy end-to-end (rm-db level): edit → mark present → simulate
  refresh clear → badge queries return clean (rm-web badge queries are
  already LEFT JOIN stale_mark and tested).

## Non-goals

- Single `rm-refresh` wrapper binary (two commands are fine).
- UI buttons that trigger refresh (docs non-goal: web server never spawns
  pipelines).
- `--routes` explicit filter on rm-segments (YAGNI; `--stale` covers the
  runbook; can be added later if surgical re-runs are ever needed).
- rm-osm-match changes.
- split-specific stale marks.

## Accepted semantics

- Refresh operates on the **latest batch**. Edits made after an upstream
  re-import but before its rm-segments/rm-registry run are refreshed by
  that run like any other (marks cleared by the full run).
- A refresh CLI and the web server may run concurrently; correctness is
  the rowid snapshot rule, not exclusion. A mark surviving a refresh is
  always safe (badge stays amber); a mark never silently disappears.
- Manual station coords: station move writes no marks, so `--stale` never
  runs *because of* a station move; refresh after other edits re-runs
  split, whose interaction with manual coords is the accepted scenario-6
  semantics (manual coords survive split.rs).
