# Scenario 2 — Stop Move ⇒ Membership (migrate `edit_stop`/stop-undo into `rm-edit`) — Design

Date: 2026-10-10
Status: approved in chat; spec for review
Builds on: scenario 1 station rename (`1443d48`), scenario 3 multi-row rename
undo (`74ec657`); phase-1 web edit mode (`05f5d64`, migration `0010_edit`)

## Context

Two stop-level edits remain outside the rm-edit domain crate:

- **move** — change a pole's `lat`/`lon` (drag-to-fix). `rm_db::edit_stop`
  owns it, plus the stop-name path. Known carried debt: the name-only path
  deletes `stop_bearing` rows and marks `('stop_bearing','*')` —
  over-conservative by the scenario-1 reasoning (bearing value = f(position);
  a name change invalidates nothing).
- **regroup** — move a pole to another station: `stop_version.station_id`
  changes. Not implemented anywhere yet; `0010_edit` reserved
  `action='move_station'` for it. Membership lives in the child version
  (INV-17), so regroup is single-row churn like move.

This scenario migrates both (and the generic stop undo) into `rm-edit`,
mirroring the scenario-1/3 pattern: load → pure plan → apply, one tx per
edit, no UI beyond rewiring the existing endpoints.

Decisions made in chat:

1. **Two separate primitives** — move/edit-stop and regroup. Not merged into
   one op, not folded into station move.
2. **Undo included** — the generic single-stop undo migrates to rm-edit;
   scenario-3 deferred minors fold in.
3. **rm-web: rewire existing endpoints only** — `POST /api/edit/stop/{svi}`
   and `POST /api/edit/undo/{edit_id}` swap to the rm-edit services; no new
   endpoints (regroup endpoint comes later, after the primitive is proven).
4. **One generic stop-undo** — kind-agnostic predecessor restore, not
   per-kind undo plans.

## Layering

```
rm-edit   domain + application service — ZERO SQL
            owns: StopEdit, RegroupStop (commands), StopEditPlan,
                  RegroupPlan, StopUndoOp, plan_* (pure fns),
                  edit_stop / regroup_stop / undo_stop (services),
                  EditError (public error type), outcomes
rm-db     repository — SQL only
            new:  edit::load_stop_snapshot   (read)
                  edit::apply_stop_edit      (write, one tx)
                  edit::apply_stop_regroup   (write, one tx)
                  edit::load_stop_edit       (read)
                  edit::apply_stop_undo      (write, one tx)
            removed: edit_stop, undo, StopEdit (public API removal)
rm-web    interface — existing handlers rewired; no new endpoints

dependency: rm-edit → rm-db (one-way)
No repository trait until a second implementation exists.
```

## Domain model (`rm-edit`)

```rust
pub struct StopEdit {                 // command (migrates from rm-db)
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub stop_name: Option<String>,
}

pub struct RegroupStop {              // command
    pub stop_version_id: i64,         // must be the current version
    pub new_station_id: i64,
}

pub struct StopEditPlan {             // edit_stop output
    pub stop_id: i64,
    pub station_id: i64,              // unchanged by this primitive
    pub stop_version_id: i64,         // old (current) version
    pub stop_name: Option<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
}

pub struct RegroupPlan {
    pub stop_id: i64,
    pub stop_version_id: i64,
    pub old_station_id: i64,
    pub new_station_id: i64,
}

pub struct StopUndoOp {               // plan_stop_undo output
    pub edit_id: i64,
    pub stop_id: i64,
    pub old_svi: i64,                 // edit_log.version_before (predecessor)
    pub cur_svi: i64,                 // edit_log.version_after (must be current)
}

#[derive(Debug, thiserror::Error)]
pub enum EditError {
    // existing
    #[error("station version {0} is not the current version (or does not exist)")]
    StationNotCurrent(i64),
    #[error("new station name is empty")]
    EmptyNewName,
    #[error("new station name equals the current name")]
    NoOpRename,
    #[error("edit {0} cannot be undone (not current, or is itself an undo)")]
    NotUndoable(i64),
    // new this scenario
    #[error("stop version {0} is not the current version (or does not exist)")]
    NotCurrent(i64),
    #[error("station {0} does not exist or is not current")]
    UnknownStation(i64),
    #[error("edit changes nothing")]
    EmptyEdit,
    #[error("edit changes no field to a new value")]
    NoOpEdit,
}
```

`rm-db::edit::EditError` keeps its own internal enum and maps 1:1 to
`rm_edit::EditError`, as in scenario 1. rm-edit owns the public type.

### Plan guards (pure, unit-tested without DB)

- **plan_stop_edit** — reject empty edit (all fields `None`,
  `EmptyEdit`); reject no-op (every `Some` field equals the snapshot value,
  `NoOpEdit`). Lat/lon range stays rm-web's job (→ 400 as today), matching
  the current split — `edit_stop` in rm-db never range-checked either.
- **plan_regroup** — target station must exist and be current (a SELECT on
  `station_version WHERE station_id = ? AND valid_to IS NULL` runs during
  load; `None` → `UnknownStation`); `old == new` rejected (`NoOpEdit`).
- **plan_stop_undo** — row loaded, `entity == 'stop'`, `action != 'undo'`,
  both version ids present (`NotUndoable` otherwise). Kind-agnostic:
  `changes` shape is not inspected, so legacy kind-less rows (all
  pre-existing stop edits) stay undoable. `version_before == version_after`
  is impossible by construction (edit always mints) but guarded anyway.

## Repository contract (`rm-db::edit`)

**Read.** `load_stop_snapshot(conn, stop_version_id)` — one SELECT on
`stop_version` matched by id **and** `valid_to IS NULL` (current-row
discipline); `None` → `NotCurrent`. For regroup, a second SELECT on
`station_version WHERE station_id = ? AND valid_to IS NULL` asserts the
target station exists and is current.

**Read.** `load_stop_edit(conn, edit_id)` — one `edit_log` row with
`changes` parsed as JSON, any entity (the plan's entity guard is the
discriminator, same as scenario 3); `None` when the id does not exist.

**Write.** `apply_stop_edit(conn, plan, editor, note)` — single tx,
re-validates currency inside the tx (guard against concurrent edits; failure
→ `EditError::NotCurrent`, zero partial state):

- close old `stop_version` (`valid_to = now`), mint new row,
  `source = 'manual'`, applying the plan's changed fields
- repoint `route_stop_version.stop_version_id` old → new
- bearing by change type (table below): position present ⇒
  `DELETE FROM stop_bearing WHERE stop_version_id = old`; name-only ⇒
  `INSERT INTO stop_bearing SELECT new_svi, … FROM stop_bearing WHERE
  stop_version_id = old` (carry-over, keeping `computed_at` — fixes the
  carried debt)
- stale marks by change type (table below)
- one `edit_log` row: `entity='stop'`, `entity_id=stop_id`,
  `action='update'`, `changes={field:{old,new}}` (shape unchanged from
  today's `edit_stop`), `version_before/after` = old/new svi

**Write.** `apply_stop_regroup(conn, plan, editor, note)` — single tx, same
discipline:

- churn `stop_version` with `station_id = new_station_id`, everything else
  carried; `source = 'manual'`; repoint rsv
- bearing carried over (position unchanged ⇒ value survives)
- stale: `station_bearing` mark only (whole-table rebuild covers both
  stations). No `path_stop_projection` marks — scenario-1 precedent: an rsv
  repoint alone invalidates nothing
- one `edit_log` row: `action='move_station'` (the value `0010_edit`
  reserved), `changes={"station_id":{"old":…,"new":…}}`

**Write.** `apply_stop_undo(conn, op, editor, note)` — single tx, ALL
guards before ANY write (scenario-3 discipline):

- guards: `op.cur_svi` is current with `valid_to IS NULL` and its
  predecessor `op.old_svi` exists; failure → `NotUndoable`, zero partial
  state. Every UPDATE carries a `== 1` rows-affected check (bogus id ⇒
  error, no silent 0-row)
- close the minted row (`valid_to = now`), reopen the predecessor
  (`valid_to = NULL`) — undo mints nothing
- repoint rsv back to `old_svi`
- bearing: `DELETE` @ `cur_svi` (a refresh may have recomputed a row there;
  the row belongs to a closed version); re-insert @ `old_svi` keeping
  `computed_at` **iff** the undone edit's `changes` lack latitude/longitude
  (name-only or regroup — the value survived)
- stale marks re-derived from the recorded `changes` fields, not table
  state: latitude/longitude present ⇒ `stop_bearing` + per-route
  `path_stop_projection` (via the repointed rsv) + `station_bearing`;
  `station_id` present ⇒ `station_bearing` only; name-only ⇒ zero marks
- one `edit_log` row: `action='undo'`, `changes='{}'`, `undo_of = edit_id`,
  `version_before/after` swapped — scenario-3 shape

### Stale-mark semantics (one table)

| Change | stop_bearing | path_stop_projection | station_bearing |
|---|---|---|---|
| edit w/ lat/lon | DELETE + mark | per serving route | mark |
| edit, name-only | carry-over, **zero marks** | — | — |
| regroup | carry-over | — | mark |
| undo of lat/lon edit | mark (row gone at old_svi) | per serving route | mark |
| undo of regroup | carry back | — | mark |
| undo of name-only | carry back | — | — |

## Application services (`rm-edit`)

```
edit_stop(conn, stop_version_id, cmd: StopEdit, editor, note)
    load_stop_snapshot → plan_stop_edit → apply_stop_edit
regroup_stop(conn, cmd: RegroupStop, editor, note)
    load_stop_snapshot + target check → plan_regroup → apply_stop_regroup
undo_stop(conn, edit_id, editor, note)
    load_stop_edit → plan_stop_undo → apply_stop_undo
```

Error mapping mirrors scenario 1: rm-db internal errors map 1:1 onto
`rm_edit::EditError`; callers see only the rm-edit type.

## rm-web rewire

- `handlers/edit.rs`: `edit_stop` builds `rm_edit::StopEdit` and calls
  `rm_edit::edit_stop`; `undo` calls `rm_edit::undo_stop`. No new endpoints,
  no UI change. Regroup has no endpoint this scenario.
- `edit_conflict` downcasts `rm_edit::EditError`:
  `NotCurrent` / `StationNotCurrent` / `NotUndoable` / `UnknownStation` →
  409 conflict (lost a race or bad target);
  `EmptyEdit` / `NoOpEdit` → 400. Handler keeps its own range checks → 400.
- `rm_db::StopEdit` import dropped from the handler.

## Folded in (scenario-3 deferred minors)

- `== 1` rows-affected checks on the reopen UPDATEs in
  `apply_station_rename_undo` (bogus old id currently = silent 0-row;
  unreachable via the service today)
- explicit `source` assertion on rows reopened by the rename undo
- e2e test: station-undo refuses a stop-edit row (wrong entity)

## What stays put

- `entity_history` stays in rm-db (read-only, no policy).
- Station rename/undo paths unchanged except the folded minors above.
- No repository trait, no domain events, no aggregates.
- rm-web regroup endpoint + UI, refresh runbook changes — later scenarios.

## Testing

- `rm-edit` unit (no DB): plan guards — empty/no-op edit, regroup
  old==new + unknown target, undo entity/action guards, error mapping.
- `rm-db` (in-memory, existing fixture style): apply semantics per
  primitive — version churn + `source='manual'`, rsv repoint, bearing
  DELETE vs carry-over per change type, zero marks after name-only edit,
  mark sets after move/regroup, in-tx currency guard (stale svi refused,
  zero partial state), edit_log shapes, undo: guards-before-writes,
  zero-partial on every guard, bearing move-back rules, mark rules per
  changes fields, `==1` rows-affected (bogus id ⇒ error, no partial).
- `rm-edit` integration (existing `tests/` pattern): load→plan→apply
  roundtrips for edit/regroup, undo roundtrips for all three edit kinds,
  undo-of-undo refused.
- `rm-web`: existing handler tests keep passing through the rewire;
  409-vs-400 mapping covered.
- Workspace `cargo test` green (210 tests today).

## Out of scope

Regroup HTTP endpoint and UI, refresh runbook wiring, station move
(`station_version` lat/lon), migrating `entity_history`, any further
`rm-edit` generalization.
