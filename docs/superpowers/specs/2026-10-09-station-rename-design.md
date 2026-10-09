# Scenario 1 — Station Rename (`rm-edit` crate) — Design

Date: 2026-10-09
Status: approved in chat; spec for review
Builds on: phase-1 web edit mode (merged to master at `05f5d64`; migrations 0010+,
`edit_log`, `stale_mark`, `rm_db::edit::{edit_stop, undo}`)

## Context

Web edit mode phase 1 shipped a single-stop edit primitive inside `rm-db`.
Scenarios 2–3 (move⇒membership, multi-row undo) need multi-entity,
whole-transaction edits with domain policy that does not belong next to SQL.
We move to a DDD-style layering **incrementally**: new scenarios get a new
domain crate; the existing `edit_stop`/`undo` stay in `rm-db` and migrate
later. One scenario at a time, each a whole-tx primitive, no UI/HTTP until
the primitive is proven. Scenario order: 1 station rename → 3 multi-row
undo → 2 move⇒membership.

## Layering

```
rm-edit   domain + application service — ZERO SQL
            owns: RenameLevel, StationRename, StationRenamePlan,
                  plan_station_rename (pure fn), EditError, outcome
rm-db     repository — SQL only
            new: edit::load_station_snapshot (read)
                 edit::apply_station_rename (write, one tx)
            op DTOs defined here; rm-edit maps Plan → ops
rm-web    interface — unchanged this scenario (no new endpoints)

dependency: rm-edit → rm-db (one-way)
No repository trait until a second implementation exists.
```

## Domain model (`rm-edit`)

```rust
pub enum RenameLevel { StationOnly, MatchingPoles, AllPoles }

pub struct StationRename {          // command
    pub station_version_id: i64,    // must be the current version
    pub new_name: String,
    pub level: RenameLevel,
}

pub struct StationRenamePlan {
    pub station_id: i64,
    pub station_version_id: i64,    // old (current) station version
    pub old_name: String,
    pub new_name: String,
    pub level: RenameLevel,
    pub poles: Vec<PoleChange>,     // empty for StationOnly
}

pub struct PoleChange {
    pub stop_id: i64,
    pub old_stop_version_id: i64,   // current pole version
    pub new_stop_name: String,      // = plan.new_name for churned poles
}

#[derive(Debug, thiserror::Error)]
pub enum EditError {
    #[error("station version {0} is not the current version (or does not exist)")]
    StationNotCurrent(i64),
    // (future variants migrate here as scenarios demand)
}
```

`RenameLevel` is a **parameter, not a policy**: all three levels are legitimate
user choices.

- `StationOnly` — churn `station_version` only; no poles touched.
- `MatchingPoles` — churn exactly the poles whose `stop_name` equals the
  station's **old** name (exact string match, decided at plan time from the
  snapshot).
- `AllPoles` — churn every pole (current `stop_version`) of the station.

## Application service

```rust
pub fn rename_station(
    conn: &Connection,
    cmd: StationRename,
    editor: &str,
    note: &str,
) -> Result<RenameOutcome>   // rm-edit::EditError or anyhow
```

Orchestration, three steps, no SQL in rm-edit:

1. `rm_db::edit::load_station_snapshot(conn, station_version_id)`
   → current station row (name) + its current poles (`stop_version_id`,
   `stop_id`, `stop_name`); `None` when `station_version_id` is not current.
2. `plan_station_rename(snapshot, cmd)` → `StationRenamePlan` (pure,
   unit-tested; rejects empty `new_name` and no-op renames
   `new_name == old_name`).
3. `rm_db::edit::apply_station_rename(conn, plan, editor, note)`
   → `RenameOutcome { edit_id, new_station_version_id, poles: Vec<{stop_id, old_svi, new_svi}> }`.

`StationNotCurrent` surfaces when the snapshot read returns `None`.

## Repository contract (`rm-db::edit`)

**Read.** `load_station_snapshot` — one SELECT for the current
`station_version` (matched by id **and** `valid_to IS NULL`) plus its poles'
current versions. No locks held; apply re-validates inside its tx.

**Write.** `apply_station_rename` — single tx:

- close old `station_version` (`valid_to = now`), mint new row with
  `station_name = new_name`, `source = 'manual'`
- per `PoleChange`: close old `stop_version`, mint new with
  `stop_name = new_stop_name`, `source = 'manual'`, repoint
  `route_stop_version.stop_version_id` old → new, **carry the pole's
  `stop_bearing` row over**: `INSERT INTO stop_bearing SELECT <new_svi>,
  bearing_deg, source, computed_at FROM stop_bearing WHERE stop_version_id =
  <old_svi>` (skip when no row exists — bearing not computed yet). Bearing
  value = f(position, path geometry); a rename changes neither, so the value
  stays valid — only the version key moves. Keep the original `computed_at`.
- re-validate the old station version is still current inside the tx
  (guard against concurrent edits). On failure rm-db returns its own
  `rm_db::edit::EditError::StationNotCurrent(i64)` (new variant next to the
  existing `NotCurrent`/`NotUndoable`); `rm-edit` maps it 1:1 to
  `rm_edit::EditError::StationNotCurrent` — rm-edit owns the public error
  type callers see, rm-db stays SQL-side with its own internal enum
- **zero stale marks**: no `stop_bearing` mark (rows carried, not deleted —
  see above), no `station_bearing` mark (keyed by `station_id`, name not an
  input to the circular mean), no `path_stop_projection` marks (no lat/lon
  changes). A rename invalidates no derived data.
- one `edit_log` row:
  - `entity = 'station'`, `entity_id = station_id`, `action = 'update'`
  - `version_before/after` = old/new `station_version_id`
  - `changes` JSON shaped for scenario-3 multi-row undo:

```json
{
  "kind": "station_rename",
  "level": "AllPoles",
  "old_name": "舊名",
  "new_name": "新名",
  "stops": [{"stop_id": 1, "old_svi": 10, "new_svi": 11}]
}
```

`stops` is empty for `StationOnly`.

## What stays put

- `edit_stop` / `undo` / `entity_history` remain in `rm-db`, unchanged.
  Their migration into `rm-edit` is a later scenario, not this one. Known
  inconsistency carried until then: `edit_stop`'s name-only path still
  deletes `stop_bearing` rows + marks `('stop_bearing','*')` — over-
  conservative by this spec's reasoning (bearing value survives a rename);
  the later migration scenario switches it to carry-over too.
- Scenario-3 undo of a rename must mirror the carry-over: copy `stop_bearing`
  rows new_svi → old_svi from the `stops` array in the edit_log `changes`
  JSON. The shape recorded here already carries everything undo needs.
- `rm-web` gets no new endpoint and no UI change.
- No repository trait, no domain events, no aggregates — plan/apply with a
  pure policy function is the whole pattern here.

## Testing

- `rm-edit` unit tests (no DB): `plan_station_rename` level selection
  (MatchingPoles boundary: exact match only, no substring/trim), empty-name
  rejection, no-op rejection, StationNotCurrent mapping from `None` snapshot.
- `rm-db` tests (in-memory DB, existing fixture style): apply semantics —
  version append + source, FK repoint, `route_stop_version` integrity,
  **zero stale marks after any rename level**, bearing carry-over
  (churned pole's `stop_bearing` row lands on the new svi with identical
  values + `computed_at`; pole without a bearing row stays without one),
  edit_log row shape including `stops` array, concurrent-edit guard (stale
  station_version_id refused, no partial state).
- Workspace `cargo test` green.

## Out of scope

HTTP/UI for station rename (wired in a later phase), scenario 2 (move⇒
membership), scenario 3 (multi-row undo — consumes this edit_log shape),
migrating `edit_stop`/`undo` into `rm-edit`.
