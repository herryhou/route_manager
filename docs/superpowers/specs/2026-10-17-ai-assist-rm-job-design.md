# AI-assist job interface (`rm-job`) — design

Date: 2026-10-17
Status: approved in chat; builder check done — findings folded in,
calls listed under "Settled during builder check"
Brainstorm: superpowers:brainstorming, resumed from handoff
(`/tmp/route-manager-ai-assist-handoff.md`)

## Intent and why

The user wants to ask Claude Code, in natural language, to do jobs against
route-manager data. Driving example:

> 「Add a new stop named "1st market" at 23.12,120.14 for (route 1, go)
> and (23, return).」

Today the only write path into published data is rm-web's
token-authenticated edit UI, one primitive at a time. An agent has no way
to act. This design adds an agent-driven path: a runbook the Claude Code
session reads, plus a CLI it drives, backed by one new `rm_db` edit
primitive. This is the already-scoped "phase 4 authoring" domain work
(new stops, route-stop links), surfaced through a CLI.

## Goals

- Natural-language jobs become deterministic, inspectable CLI invocations.
- `rm_db::edit::add_stop`: insert or append a stop onto one or more
  route-directions in one transaction, with full re-versioning.
- Station qualification: reuse an existing station when name, distance and
  bearing all qualify; create a standalone station only when none does.
- Every job is dry-run by default with an exact plan; `--apply` writes;
  `undo_edit` reverts the whole job.
- The human stays in the loop for refresh: the job ends at stale marks and
  a report; a human presses rm-web's existing refresh chain.

## Non-goals

- No MCP server, no chat panel in rm-web.
- No automatic pipeline refresh from rm-job.
- No station-group assignment (group membership stays the registry
  pipeline's job).
- No new rm-web write path — rm-web's changes are the copy-coords context
  menu plus trivial `EditError` match arms for the new variants (rm-web
  matches the enum exhaustively, so new variants force arms; that is
  mapping, not a write path).
- v1 does not edit existing stops (`edit_stop`, `rename_station`,
  `move_station`, `regroup_stop` already cover that).

## Anti-goals (what would make this a failure even if it works)

- Any write without a plan shown first (dry-run default is the guard).
- Non-deterministic output — e.g. HashMap-iteration insert order breaking
  rerun equality (CLAUDE.md invariant, already broke once).
- rm-job touching SQLite except through `rm_db` — no second connection
  layer.

## Locked decisions (user picked each explicitly)

1. **Interface: Skill + CLI only.** The Claude Code session reads a
   runbook (`docs/agents/rm-jobs.md`) and drives a CLI. No MCP, no chat
   panel.
2. **Map pick: copy-coords context menu** in rm-web (~15 lines JS,
   clipboard). The user pastes coordinates to Claude. No endpoint, no
   state.
3. **Attach mode: insert-between v1**, not append-only. A new stop can go
   mid-sequence with downstream re-versioning.
4. **Refresh: human presses the button.** The job ends at stale marks plus
   a report pointing at rm-web's refresh chain (match → segments →
   registry, already built — registry is the step that rebuilds the new
   stop's bearings).
5. **Job layer: new crate `crates/rm-job`**, clap subcommands, wrapping
   `rm_db::edit` primitives. Depends only on `rm_db` + clap.
6. **Station default: qualify-or-create** (user's answer). `add_stop`
   first searches for a qualified existing station — same name, near
   location, close bearing — and attaches the new stop to it; it creates a
   new standalone station only if none qualifies. An explicit station id
   pins the choice and skips the search.
7. **One stop_version per (stop, direction)** (user confirmed). Each
   occurrence of a stop on a route direction carries its own
   `stop_version` row keyed by direction; the go and return occurrences
   are separate rows. Matches the 台南 stopid-per-occurrence semantics and
   the existing `stop_version.direction` column.

## Architecture

```
user ──natural language──> Claude Code session
                               │ reads docs/agents/rm-jobs.md
                               ▼
                          rm-job CLI (crates/rm-job, clap)
                               │ dry-run plan → user → --apply
                               ▼
                          rm_db::edit::add_stop   (one tx)
                               ▼
                            SQLite
```

- rm-web's write path is unchanged; its only change is the copy-coords
  context menu (read-only JS).
- Writers of published data become rm-web (existing) and rm-job (new),
  both via `rm_db::open`. rm-web stays the only `open_readonly` consumer.
  See `docs/rm-db-usage.md` for the full function→consumer matrix.

## The primitive: `rm_db::edit::add_stop`

API sketch (final shapes are the builder's, constrained by the tests):

```rust
pub struct AddStop {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub station_id: Option<i64>,   // Some = pin, skip qualification
    pub attachments: Vec<Attach>,
}

pub struct Attach {
    pub route_id: i64,
    pub direction: i64,
    pub after_stop_code: Option<String>, // None = append to the end
}

pub fn add_stop(conn: &Connection, cmd: AddStop, editor: &str, note: &str)
    -> Result<AddStopOutcome>;
```

Follows the established edit-module shape: load → plan (pure) → apply
(one tx). The plan is fully computable read-only, which is what makes
dry-run free.

### Station resolution

- `station_id: Some(id)` → verify the station is current; error otherwise
  (mirrors `regroup_stop`'s target check). The search is skipped.
- `station_id: None` → qualify-or-create over current `station_version`
  rows:
  - normalized name equal (trim + case-fold),
  - distance ≤ 500 m (the codebase's nearest-query convention),
  - bearing within ±30° — the new stop's approach bearing (matched route
    path bearing at the projection of the given lat/lon) vs the station's
    `station_bearing`, compared in the mode the row itself declares via
    `max_spread_deg`: spread < 45° (`SIGNED_SPREAD_MAX_DEG`) means the
    stored value is a signed heading → compare directionally; spread
    ≥ 45° means it is an axial mean folded to 0–180° → fold the candidate
    bearing the same way and compare axially. Two-way stations (one
    station holding 去程/返程 poles) are the common case, and a
    directional test against an axial value would reject half the valid
    matches. No `station_bearing` row → the bearing condition is dropped
    and the plan carries a warning line; name + distance decide.
  - Deterministic tie-break: nearest distance first, then lowest
    `station_id`.
  - No qualifier → create station + station_version for this stop alone
    (group membership stays registry's job). `stop_version.station_id` is
    NOT NULL, so a stationless stop is not an option.
- The dry-run plan names the verdict — matched `station_id`/name, or
  "create new station" — before `--apply`.

### Versioning invariants (each one already broke once somewhere)

- **INSERT-only re-versioning.** Never UPDATE a published row. Any stop
  sequence change mints a new `route_version` (`version_no + 1`), new
  `subroute_version` rows, and re-minted `route_stop_version` rows with
  the full contiguous sequence; stops after the insertion point shift
  `stop_sequence + 1` as new rows. `route.current_route_version_id`
  moves in the same tx. Old versions remain untouched (schema spec:
  never mutate history).
- **One stop_version per (stop, direction).** The new stop gets one
  `stop_version` row per direction it serves — (route 1, go) and
  (route 23, return) attachments mint two rows sharing one `stop_id`.
  `stop_code` follows the per-occurrence convention (台南); minted
  deterministically inside the tx.
- **Re-versioning shape** (builder-check correction): there is exactly
  **one subroute per route**, and both directions' `route_stop_version`
  rows hang off that one `subroute_version`, keyed by `direction`. A stop
  insertion therefore re-versions: a new `route_version` (`version_no` =
  max + 1, the old one left untouched), one new `subroute_version`, and a
  full re-mint of `route_stop_version` rows for **all** directions — the
  insert-point direction gains the new stop with downstream stops shifted
  `stop_sequence + 1`; the sibling direction re-mints unchanged. Same
  shape publish itself uses (rm-publish precedent).
- **Deterministic mint order inside the tx** — never HashMap iteration;
  a rerun on identical input must produce identical rows (byte-diff
  test).
- **One tagged undo for the whole job** (candidate-5 pattern): a single
  `edit_log` row covers every table the job touched; existing
  `undo_edit(edit_id)` reverts all of it; the rollback order is pinned
  by a test.
- **Stale marks**: `mark_line_stale` for each attached (route, direction)
  and `mark_position_stale` for the new stop_version rows, all with the
  job's `edit_id` as cause. The outcome reports `stale_routes` so the CLI
  can print "refresh needed for routes …".
- **Pre-write rejection of bad plans** — the entire plan validates before
  any row is written; a rejection leaves the DB byte-identical:
  - unknown route / unknown direction,
  - unknown `after_stop_code`, or one not on that direction,
  - off-path stop: the given lat/lon projects beyond a threshold from the
    direction's matched path. No matched path for that direction → the
    check is skipped with a warning (nothing geometric exists to validate
    against; no provider-polyline fallback — the check validates against
    the matched path only),
  - warn-only: another current stop_version with the same normalized
    name nearby (notice in the plan, not a rejection).

### Outcome

`AddStopOutcome` must serve three consumers: the CLI report (edit_id,
minted stop/station ids, stale routes), `undo_edit` (one edit_id), and
tests (exact key sets). Exact fields are the builder's call.

## rm-job CLI UX

```
rm-job add-stop "1st market" \
    --lat 23.12 --lon 120.14 \
    --attach 1:0 \
    --attach 23:1@C \
    [--station <station_id>] \
    [--db <path>] [--editor <name>] [--note <text>] \
    [--apply]
```

- `--attach <route>:<direction>` appends; `@<stop_code>` inserts after
  that stop. Repeatable; all attachments commit in one transaction.
- **Dry-run is the default.** It prints:
  - the station verdict (matched station id+name, or "create new"),
  - per-attach: route, direction, insert position, neighboring stops
    before/after,
  - per-table row counts to be inserted (stop, stop_version, station,
    station_version, route_version, subroute_version,
    route_stop_version, edit_log, stale_mark),
  - the stale routes needing refresh.
  Literal ids are minted at apply time; the plan shows counts and keys,
  not future ids.
- `--apply` performs exactly the shown plan.
- `rm-job undo <edit_id>` — thin wrapper over `rm_db::undo_edit`, so the
  agent can revert a job without leaving the CLI. rm-web's history panel
  is per-entity and will not surface job edits, so the CLI is the undo
  surface for jobs.
- Exit codes: `0` plan/apply ok, `1` rejected (reason on stderr),
  `2` no-op (e.g. nothing to attach).
- Dependencies: `rm_db` + clap only. Run under `cargo run -r` like every
  pipeline binary (CLAUDE.md).

## rm-web copy-coords

Map `contextmenu` handler → `navigator.clipboard.writeText("lat, lon")`,
~15 lines in the existing assets JS. No endpoint, no state, **no viewport
movement** (the no-programmatic-zoom/pan whitelist stands). Follow
`crates/rm-web/DESIGN.md`; remember the `include_str!` gotcha — rebuild +
hard refresh or the change silently does nothing.

## Runbook

`docs/agents/rm-jobs.md`, matching the existing `docs/agents/` pattern,
plus a CLAUDE.md pointer. Contents: when to reach for rm-job, the
dry-run-first rule (never `--apply` before the plan has been shown to the
user), the attach syntax, undo instructions, and the refresh rule — the
job ends at stale marks and a report; a human presses rm-web's refresh.

## Testing

- **Golden exact-key-set** per written table (candidate-2 pattern):
  the full set of inserted keys is asserted exactly, per table.
- **Zero-partial-write**: every rejection path leaves a table dump
  byte-identical to before (zero-partial DROP TABLE probe style).
- **Undo round-trip**: apply → undo → byte-identical dump of all
  published + derived tables, excluding `edit_log` and `stale_mark` (the
  undo itself writes its own `edit_log` row by design) —
  line-undo-incident repro style; the test pins the rollback order. Undo
  of a job also deletes the job's own `stale_mark` rows (those carrying
  the job's `edit_id` as cause): a fully reverted job changed nothing, so
  no refresh badge may nag. Marks from other causes are untouched.
- **Determinism**: the same add twice on two fresh in-memory dbs →
  byte-identical dumps (no HashMap-order drift).
- **Station qualification**: match / no-match-create / pinned
  `--station` / threshold boundaries (500 m, ±30°) / missing-bearing
  fallback warning.
- **Attach shapes**: append vs mid-insert; multi-attach in one tx;
  stale-mark assertions per attached route+direction.

## Rollout order

1. `rm_db::edit::add_stop` + tests.
2. `crates/rm-job` CLI.
3. rm-web copy-coords.
4. Runbook + CLAUDE.md pointer.
5. E2E on a copy of a city DB (not a live one).

Each step lands as its own commit.

## Constraints in force (project)

- CLAUDE.md: release builds for pipeline binaries; INSERT-only publish
  writes; deterministic DB writes; edit undo guards checked before
  write; rm-web DESIGN.md before UI work; no programmatic zoom/pan.
- `docs/DB_Schema_Spec.md`: never mutate history — create a new version
  and move the current pointer; published rows never hard-deleted.
- `docs/rm-db-usage.md`: rm_db is the only connection layer; stale marks
  are produced by pipelines/edits and consumed by rm-web badges.
- No schema migration is anticipated by this design; if the builder finds
  one needed, that is a change to flag back, not to absorb silently.

## Left to the builder

- Exact `EditError` variants and `AddStopOutcome` fields.
- `stop_code` minting convention — align with how ingest mints per-
  occurrence codes.
- `station_group_id` handling when creating a standalone station (the
  schema spec has it NOT NULL on `station_version`) — follow however
  existing station creation does it.
- The off-path projection threshold value (consistent with the ≤500 m
  convention).
- Undo mechanics for an INSERT-shaped edit (delete-own-rows vs supersede)
  — either is acceptable if the B2==B0 round-trip test passes and no
  pre-existing published row is mutated.
- CLI flag spelling details.

## Settled during builder check (my calls — veto any)

The builder check came back with five questions and three verified
corrections. All settled as follows; none went to the user as blocking.

1. **Bearing compare is spread-discriminated.** `station_bearing` stores
   a signed heading only when member stops agree within 45°
   (`max_spread_deg`); otherwise an axial mean folded to 0–180°. The
   qualification compares directionally in the first case, axially (both
   sides folded) in the second — folded into the spec above.
2. **Off-path check degrades on unmatched directions** — skipped with a
   warning, no provider-polyline fallback. Folded in.
3. **Undo surface is `rm-job undo <edit_id>`** — rm-web's per-entity
   panels won't show job edits, and `undo_edit` is already an
   `rm_db::edit` primitive, so the wrapper stays within locked decision
   5. Folded in.
4. **Undo is true net-zero**: the undone job's own `stale_mark` rows are
   deleted (no badge nag for data that is back to unchanged), and the
   round-trip dump excludes `edit_log`/`stale_mark`. Folded in.
5. **rm-web gains trivial `EditError` match arms** — the enum is matched
   exhaustively there, so new variants force a build change; the
   non-goals wording is amended to say so. The arms belong in step 1's
   commit.

Corrections folded in: one subroute per route (re-versioning shape
bullet rewritten); refresh chain is match → segments → **registry**;
`--attach` keys routes by xno string (line-edit precedent), so the
"route identifier" builder note is resolved.
