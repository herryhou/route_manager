# Scenario 3 — Multi-Row Undo of Station Rename — Design

Date: 2026-10-09
Status: approved in chat; spec for review
Builds on: scenario 1 station rename (merged at `1443d48`; `rm-edit` crate,
`rm_db::edit::{load_station_snapshot, apply_station_rename}`, edit_log
`changes` JSON shaped for undo)

## Context

Scenario 1 made station rename a whole-tx primitive whose `edit_log` row
carries everything an undo needs:

```json
{
  "kind": "station_rename",
  "level": "AllPoles",
  "old_name": "舊名",
  "new_name": "新名",
  "stops": [{"stop_id": 1, "old_svi": 10, "new_svi": 11}]
}
```

The existing `rm_db::edit::undo` handles only `entity='stop'` single-row
edits and rejects `entity='station'`. This scenario adds the multi-row
undo: the exact inverse of `apply_station_rename`, one transaction.
Scenario order agreed 2026-10-09: 1 rename → **3 multi-row undo** → 2
move⇒membership. No UI/HTTP until the primitive is proven.

## Layering

Mirrors scenario 1 (option B agreed in chat):

```
rm-edit   domain + application service — ZERO SQL
            new: plan_rename_undo (pure), undo_station_rename,
                  RenameUndoOp, EditError::NotUndoable
rm-db     repository — SQL only
            new: edit::load_station_rename_edit (read)
                 edit::apply_station_rename_undo (write, one tx)
rm-web    interface — unchanged this scenario (no new endpoints)

dependency: rm-edit → rm-db (one-way, unchanged)
edit_stop / undo (stop) stay in rm-db, migrate later — unchanged.
```

Rationale vs extending `rm_db::edit::undo` with a station branch: new
scenario logic follows the agreed DDD drift (domain in rm-edit, SQL in
rm-db); the stop undo migrates in a later scenario, not this one.

## Semantics — exact inverse of apply, one tx

Undo targets an `edit_log` row by `edit_id`.

### Guards (all inside the apply tx; any failure = `NotUndoable(edit_id)`, nothing written)

- the row exists, `entity = 'station'`, `action != 'undo'`,
  `changes.kind = 'station_rename'` — unknown kinds refuse rather than
  guess (forward-compat: a future kind gets its own undo)
- the new station version (`version_after`) is still the current
  `station_version` (`valid_to IS NULL`)
- **every** pole `new_svi` in `stops[]` is still the current
  `stop_version` of its stop — a post-rename `edit_stop` on any pole
  makes the whole rename non-undoable

The load/plan split: `edit_log` is append-only, so a plan parsed outside
the tx from the logged row is safe; apply re-validates all currency
guards inside the tx (same pattern as scenario 1's snapshot → apply).

### Inverse steps (mirror of apply, in order)

1. close the new `station_version` (`valid_to = now`), reopen the old
   one (`valid_to = NULL`)
2. per pole in `stops[]`:
   - close the new `stop_version`, reopen the old one
   - repoint `route_stop_version.stop_version_id` new_svi → old_svi
   - **move** the `stop_bearing` row back: `INSERT INTO stop_bearing
     SELECT <old_svi>, bearing_deg, source, computed_at FROM
     stop_bearing WHERE stop_version_id = <new_svi>` then `DELETE` the
     new_svi row — the row moves (PK invariant; a leftover on a closed
     version would collide on a future re-churn). Skip when no row
     exists (bearing never computed). Original `computed_at` kept: a
     rename changes neither position nor path geometry.
3. one `edit_log` row: `entity = 'station'`, `entity_id = station_id`,
   `action = 'undo'`, `changes = '{}'` (agreed: stop-undo precedent;
   the original row holds the full shape and is reachable via
   `undo_of`), `undo_of = edit_id`, `version_before`/`version_after` =
   new/old `station_version_id` (swapped)
4. **zero stale marks** — mirror of apply's zero: bearing rows are
   moved, not deleted; no position changes; station name is not an
   input to any derived datum

No `id_sequence` allocation — undo reopens existing rows, mints nothing.

## Domain model (`rm-edit`)

```rust
/// Read-side DTO for the logged rename (rm-db loads it).
pub struct StationRenameEdit {
    pub edit_id: i64,
    pub station_id: i64,
    pub action: String,          // must not be "undo"
    pub old_station_version_id: i64,   // version_before
    pub new_station_version_id: i64,   // version_after
    pub changes: serde_json::Value,    // kind, level, names, stops[]
}

pub struct RenameUndoOp {
    pub edit_id: i64,
    pub station_id: i64,
    pub old_station_version_id: i64,
    pub new_station_version_id: i64,
    pub poles: Vec<PoleUndo>,    // from stops[]; empty for StationOnly
}

pub struct PoleUndo {
    pub stop_id: i64,
    pub old_svi: i64,
    pub new_svi: i64,
}

pub enum EditError {
    StationNotCurrent(i64),      // existing
    NotUndoable(i64),            // NEW: mapped 1:1 from rm_db
    // (future variants migrate here as scenarios demand)
}
```

`plan_rename_undo(edit: &StationRenameEdit) -> Result<RenameUndoOp>` —
pure: rejects `action == "undo"`, `kind != "station_rename"`, malformed
`stops` entries; copies `stop_id`/`old_svi`/`new_svi` into the op.

`undo_station_rename(conn, edit_id, editor, note) -> Result<UndoOutcome>`
— load → plan → apply; maps `rm_db::EditError::{NotUndoable,
StationNotCurrent, NotCurrent}` onto rm-edit's public error, same 1:1
mapping pattern as scenario 1.

```rust
pub struct UndoOutcome {
    pub edit_id: i64,                  // the undo's own edit_log row
    pub restored_station_version_id: i64,
}
```

## Repository contract (`rm-db::edit`)

**Read.** `load_station_rename_edit(conn, edit_id) -> Option<StationRenameEdit>`
— one SELECT of the `edit_log` row with `changes` parsed as JSON.
`None` when the id does not exist. The row's `entity` is not re-checked
downstream: only `apply_station_rename` writes `kind =
"station_rename"`, so the plan's kind guard is the discriminator.

**Write.** `apply_station_rename_undo(conn, &op, editor, note)`
— single tx executing the guards and inverse steps above. Returns
rm-db's own outcome struct (`edit_id`, `restored_station_version_id`;
rm-edit maps it to `UndoOutcome` — dependency stays one-way). On guard
failure returns `rm_db::EditError::NotUndoable(edit_id)`; on internal
staleness (station version no longer current) `StationNotCurrent`. The
undo's `edit_log` row is written in the same tx.

## What stays put

- `edit_stop` / stop-`undo` / `entity_history` in `rm-db`, unchanged.
- `rm-web` gets no new endpoint and no UI change (undo dispatch by
  entity is a later phase).
- Redo: undo-of-undo is rejected (guard above); a redo scenario, if ever
  wanted, needs its own design.
- Scenario 2 (move⇒membership), migrating `edit_stop`/undo into rm-edit.

## Testing

- `rm-edit` pure unit tests (no DB): happy parse of the recorded shape;
  `action == "undo"` rejected; `kind` missing/unknown rejected;
  malformed `stops` entries rejected.
- `rm-db` tx tests (in-memory DB, scenario-1 fixture style):
  - **bearing mirror-test first** (PK invariant): seed rename via
    `apply_station_rename`, undo, assert the `stop_bearing` row sits on
    `old_svi` with identical `bearing_deg`/`source`/`computed_at` and no
    row remains on `new_svi`; pole that had no row still has none
  - station + poles restored: old rows current with original names and
    original `source` (reopened, not re-minted), new rows closed
  - `route_stop_version` repointed back to old_svi
  - zero `stale_mark` rows after the undo
  - undo `edit_log` row shape: entity/action/`{}`/undo_of/swapped
    versions
  - each guard refuses with zero partial state: stale station version
    (station re-renamed), stale pole (post-rename `edit_stop` on a
    pole), undo-of-undo, unknown `edit_id`, `kind != station_rename`
- `rm-edit` integration test (in-memory DB): rename → undo → names back
  for station and churned poles; second undo refused.
- Workspace `cargo test` green.
