# Scenario 6 — Station Move (primitive + kind-agnostic station undo) — Design

Date: 2026-10-10
Status: approved in chat; spec for review
Builds on: scenario 1 station rename (`1443d48`), scenario 3 rename undo
(`74ec657`), scenario 2 stop move/regroup/undo (`0fad928`), scenario 4
regroup endpoint/UI (`49fac47`), scenario 5 undo button/log UI (`c4a25af`),
move-stop UX pass (`7690344`, `b531fd9`)

## Context

Station-level edits so far: rename (scenario 1) and its undo (scenario 3),
both rm-edit primitives with no HTTP surface yet. Station move — reposition
the station's reference point (`station_version.latitude/longitude`) — is
the next whole-tx primitive. The station panel has no edit buttons yet; its
UI (move + rename) is a later scenario.

Why move is the cheapest write primitive in this codebase:

- The versioning scheme makes a move append-only: close the current
  `station_version`, mint a new one. No UPDATE of published content.
- **Nothing references `station_version_id`** — unlike stop move, which had
  to repoint `route_stop_version`, station move has no repoint step.
- **No derived table reads station coords**: `stop_bearing` computes from
  stop position + road runs; `station_bearing` is a pure axis over member
  bearings (rm-registry `bearing.rs`). Nearest-station and map markers read
  the row directly and are always fresh. So **zero stale marks** — same as
  rename, unlike stop move (3 marks).

Decisions made in chat:

1. **Primitive only** — rm-edit + rm-db + tests. No HTTP endpoint, no UI,
   no rm-web change (the `POST /api/edit/undo` handler dispatches to
   `undo_stop` only; station undo has no endpoint to rewire).
2. **Undo included** — whole-tx pair, like stop move was.
3. **Kind-agnostic station undo** — one `plan_station_undo` +
   `apply_station_undo` handling rename AND move (kind guard inside),
   mirroring how `plan_stop_undo` went kind-agnostic in scenario 2. The
   rename-undo callers migrate; no parallel second station-undo stack.
4. **Both coords required** — a station position is a point; a partial
   lat-only edit would create a mixed old/new point. (Stop move differs
   because its UI nudges one axis; station move is a map click.)
5. Fold in the ledgered error-mapping consolidation: undo.rs's inline
   rm-db→EditError match moves to the shared `map_db_err` used by stop.rs.

Accepted semantics (documented, not guarded):

- Manual coords survive `split.rs` re-runs — it repositions a station only
  when a split event moves stops (`moved_here > 0`); otherwise coords are
  untouched.
- A large move can flip rm-verify `station:recompute` clustering (verify
  re-groups stations from coords and compares against stored
  `station_group` membership). That is a verify-time warning, not data
  corruption; grouping is repaired by the grouping pipeline, not by edit.
- A full re-ingest rebuilds stations from source data and loses manual
  coords — same accepted risk as manual renames; out of scope.

## Layering

```
rm-edit   domain + application service — ZERO SQL
            owns: StationMove (command), StationMovePlan, StationMoveUndoOp,
                  plan_station_move / plan_station_undo (pure fns),
                  move_station / undo_station (services),
                  EditError (public error type), outcomes
rm-db     repository — SQL only
            new:     edit::apply_station_move    (write, one tx)
            renamed: load_station_rename_edit → load_station_edit
                     (read; DTO StationRenameEdit → StationEdit)
                     plan fns + apply fn renamed with it (see Undo)
            extended: StationSnapshot gains latitude/longitude
                      (rename plans ignore them; move plan needs old coords)
rm-web    interface — UNTOUCHED this scenario

dependency: rm-edit → rm-db (one-way), unchanged
```

## Domain model (`rm-edit`)

```rust
pub struct StationMove {              // command
    pub station_version_id: i64,      // must be the current version
    pub latitude: f64,
    pub longitude: f64,
}

pub struct StationMovePlan {          // plan_station_move output
    pub station_id: i64,
    pub station_version_id: i64,      // old (current) version
    pub old_latitude: f64,
    pub old_longitude: f64,
    pub new_latitude: f64,
    pub new_longitude: f64,
}

pub struct MoveOutcome {
    pub edit_id: i64,
    pub new_station_version_id: i64,
}

pub struct StationMoveUndoOp {        // plan_station_undo output (move branch)
    pub edit_id: i64,
    pub station_id: i64,
    pub old_station_version_id: i64,  // edit_log.version_before (predecessor)
    pub new_station_version_id: i64,  // edit_log.version_after (must be current)
}
// RenameUndoOp keeps its name and shape (poles vec); it becomes one
// branch's output of the generalized planner.
```

No new `EditError` variants: move guards reuse `StationNotCurrent` (stale
svi, same as rename) and `NotUndoable` (undo branch). rm-db's internal
enum keeps mapping 1:1, as in scenarios 1–3.

### Plan guards (pure, unit-tested without DB)

- **plan_station_move** — no no-op guard (moving to identical coords mints
  a version; stop move has no equality guard either — YAGNI). Coords are
  stored verbatim, no range check here; range stays the future endpoint's
  job (→ 400), matching the stop-move split. The only failure mode is
  upstream: snapshot load returns `None` for a stale/missing svi →
  `StationNotCurrent`.
- **plan_station_undo** — generalizes `plan_rename_undo`. Guards
  (inherited): `action == 'undo'` → `NotUndoable`; missing
  `version_before`/`version_after` → `NotUndoable`. Kind dispatch on
  `changes.kind`:
  - `"station_rename"` → existing behavior verbatim (poles extracted from
    `changes.stops`, corrupt/missing pairs → `NotUndoable`)
  - `"station_move"` → `StationMoveUndoOp` (coords not read — restore is
    version reopen, the old row already holds them)
  - anything else / missing `kind` → `NotUndoable` (station-side undo is
    kind-strict, unlike stop-side: station rows are all post-scenario-1,
    every one carries a `kind`)

## Repository contract (`rm-db::edit`)

**Read (renamed).** `load_station_edit(conn, edit_id) -> Option<StationEdit>`
— byte-identical behavior to `load_station_rename_edit`, new name: still no
entity filter (the plan's kind guard discriminates), `changes` parsed as
JSON, `None` on unknown id. `StationSnapshot` gains `latitude: f64,
longitude: f64` (SELECT extends by two columns; rename callers unaffected).

**Write (new).** `apply_station_move(conn, op, editor, note)` — single tx,
re-validates currency inside the tx (concurrent-edit guard; failure →
`StationNotCurrent`, zero partial state):

- guards first: old svi current (`valid_to IS NULL`), `== 1` rows-affected
  check on the close UPDATE (new code does not repeat the scenario-3 debt)
- close old `station_version` (`valid_to = now`), mint new row:
  same `station_id`/`station_group_id`/`station_name`, new
  latitude/longitude, `source = 'manual'`, `valid_from = now`
- no repointing, no bearing moves, no pole churn, **zero stale marks**
- one `edit_log` row: `entity='station'`, `entity_id=station_id`,
  `action='update'`,
  `changes={"kind":"station_move","latitude":{"old":…,"new":…},
  "longitude":{"old":…,"new":…}}` — `{field:{old,new}}` matches the
  stop-move shape the scenario-5 log humanizer already parses; `kind`
  feeds the undo dispatcher; `version_before/after` = old/new station
  version ids

**Write (renamed + extended).** `apply_station_rename_undo` →
`apply_station_undo(conn, op, editor, note)` — one entry point, branch on
which undo op arrived (rename's `RenameUndoOp` vs move's
`StationMoveUndoOp`; both already carry their kind's full instruction set,
so no re-parsing in SQL-land):

- rename branch: today's steps verbatim (close/reopen station + pole
  versions, rsv repoint, bearing rows move back, `==1` checks — all as
  merged in scenario 3 / folded in scenario 2)
- move branch: ALL guards before ANY write — minted svi still current,
  predecessor exists; every UPDATE `== 1` checked; then close the minted
  row (`valid_to = now`), reopen the predecessor (`valid_to = NULL`).
  Undo mints nothing; touches no poles, no bearings, no rsv (a station
  move wrote none of them)
- both branches: one `edit_log` row `action='undo'`, `changes='{}'`,
  `undo_of = edit_id`, `version_before/after` swapped — scenario-3 shape

## Application services (`rm-edit`)

```
move_station(conn, cmd: StationMove, editor, note)
    load_station_snapshot → plan_station_move → apply_station_move
undo_station(conn, edit_id, editor, note)
    load_station_edit → plan_station_undo → apply_station_undo
    (renames undo_station_rename; behavior superset: now also undoes moves)
```

Error mapping: the inline `match e.downcast_ref::<rm_db::EditError>()`
blocks in undo.rs and the analogous one in rename.rs fold into the shared
`map_db_err` helper (move from stop.rs to a common spot; stop.rs, rename.rs,
undo.rs, move.rs all call it). Ledger item "consolidate undo.rs inline
error-mapping vs stop.rs map_db_err" closes with this scenario.

## What stays put

- rm-web: no endpoint, no handler change, no asset change. The undo
  endpoint stays stop-only until the station UI scenario.
- Stop-side primitives untouched.
- No repository trait, no domain events, no aggregates.
- Station move + rename UI, refresh runbook wiring — later scenarios.

## Testing

- `rm-edit` unit (no DB): `plan_station_move` happy path + stale svi
  (via service load, since the plan itself is total); `plan_station_undo`
  — undo-of-undo, wrong kind, kind-less row, corrupt poles payload →
  `NotUndoable`; move branch extracts version pair; rename branch
  regression (existing plan tests migrate to the new fn name).
- `rm-db` (in-memory, existing fixture style): `apply_station_move` —
  coords applied, old closed, minted `source='manual'`, name/group
  carried; zero stale marks; zero pole/bearing/rsv touches (row counts
  unchanged); in-tx currency guard (stale svi refused, zero partial);
  `==1` check (bogus id ⇒ error, no silent 0-row); edit_log shape
  (entity/action/changes JSON round-trip). `apply_station_undo` move
  branch — restores (old current with original coords, minted closed,
  `COUNT(station_version)` unchanged — undo mints nothing), guards
  before writes, zero partial on every guard, `==1` on bogus ids;
  rename branch byte-identical behavior (existing tests migrate).
- `rm-edit` integration (existing `tests/` pattern): move roundtrip;
  undo-move roundtrip (coords back, log rows correct); move → move again
  → first undo refused (`StationNotCurrent`-family guard); undo-of-undo
  refused; rename undo through `undo_station` still green.
- rm-web: untouched — existing handler tests keep passing unchanged.
- Workspace `cargo test` green (235 tests today; expect +15–25).

## Out of scope

Station move HTTP endpoint, station panel UI (move + rename buttons),
refresh runbook wiring, rm-verify grouping repair, migrating
`entity_history`, any further rm-edit generalization.
