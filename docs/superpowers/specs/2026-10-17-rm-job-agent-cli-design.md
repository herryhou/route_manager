# rm-job agent CLI — expose the rm-db edit/read surface to LLM agents

Date: 2026-10-17
Status: draft (pre-plan)
Supersedes scope-wise: extends `2026-10-17-ai-assist-rm-job-design.md` (add-stop, shipped 0248c01)

## Intent and why

rm-job currently exposes exactly two operations (`add-stop`, `undo`). The
rm-db edit layer underneath is much wider — stop move/rename, regroup,
station ops, polyline override, undo dispatch, stale marks — and the read
side is effectively unreachable from a CLI (an agent cannot even ask
"which stop on route 15 matches name A?").

Goal: an LLM agent, driven by a skill/runbook, can carry out per-route
correction jobs end-to-end over the CLI: look up the target, dry-run the
edit, read the plan, apply, report the outcome (and undo when wrong).

Three concrete jobs pulled from real correction work:

1. **Terminal path extension** — a route's first/last stop should have its
   path extend ~10 m outward along the road (the matched path currently
   ends at the terminal stop).
2. **Move a stop** — "route 15's stop X should be at (lat, lon)".
3. **Remove a stop from a route** — "route 15's stop A should be deleted"
   (detach from the direction's stop sequence; the pole itself may serve
   other directions and must survive).

## Goals

- Every rm-db edit primitive reachable from `rm-job` with the
  dry-run-first safety model.
- Read/lookup commands so an agent can resolve human input (route number
  + stop name) into ids (`stop_version_id`, direction) without SQL.
- Full agent-friendly CLI contract: `schema` introspection, JSON on
  stdout, machine-checkable exit codes, no interactive prompts.
- A skill/runbook (`docs/agents/rm-jobs.md` extension + Claude skill) so
  an agent discovers the workflow unaided: schema → dry-run → apply →
  verify → (undo).

## Non-goals

- Batch/pipeline automation: each correction is per-route, human-prompted,
  one at a time (explicit decision 2026-10-17). No "fix all 392 routes".
- No new rm-web endpoints and no UI changes. rm-web stays the human
  surface; rm-job is the agent surface over the same `rm_db::edit`
  primitives.
- No provider re-import, no matching-pipeline changes. `extend-path` only
  writes `polyline_override`; it never re-runs the matcher.

## Anti-goals (failure even if it works)

- Any command that writes without an explicit opt-in flag on that same
  invocation. (Dry-run is the default; `--apply` opts in — per command,
  visible in schema.)
- JSON and human prose mixed on stdout.
- An undo that cannot restore a removed stop to its exact prior sequence
  position.

## Constraints (repo invariants that bind the design)

- Release builds for any command touching the OSM graph (extend-path
  loads it; debug is 10–30x slower).
- Deterministic writes: no HashMap-iteration-ordered inserts anywhere new.
- Undo/apply guards check before writing; every new edit kind gets an
  undo op and an `undo_edit` dispatch arm with tests.
- Read commands must probe `sqlite_master` for newer tables and degrade
  (old DBs must not 500/crash — /raw precedent).
- JSON envelope on stdout; human-facing progress (if any) on stderr.

## Decisions already made (with the user, 2026-10-17)

1. **Output contract = option (c), full agent-friendly CLI**: `schema`
   subcommand (JSON Schema per command), JSON-only stdout envelope,
   documented exit codes, `--help` with agent examples. Follows the
   `agent-friendly-cli` skill.
2. **Dry-run stays the default; `--apply` opts in.** This deliberately
   overrides the agent-friendly skill's "execute by default + `--dry-run`
   flag": rm-job's shipped safety model (0248c01) wins. The schema spells
   this out so agents never guess.
3. **Exit codes**: keep rm-job's existing 0 ok / 1 rejected / 2 no-op
   (2=no-op is informative, not an execution error). Envelope carries
   `status: "ok" | "rejected" | "noop"` and, for rejected, the reason.
   Documented in `schema`.
4. **extend-path writes through `apply_line_edit`** (polyline_override),
   so undo and the concurrent-edit guard (`expected_updated_at`) come for
   free.
5. **remove-stop refuses to orphan a pole.** Detaching the pole's last
   direction is rejected unless `--delete-pole` is also given. Dry-run
   always shows the orphan verdict.
6. **New commands are `--json`-payload-first** (schema-driven); the two
   existing commands (`add-stop`, `undo`) keep their flags and gain
   `--json` as an alternative input.
7. **Detach is version-chain, not in-place delete** (builder-check
   finding): mint a new `route_version` + `subroute_version` +
   `route_stop_version` rows minus the detached stop, move
   `route.current_route_version_id`; undo = pointer restore, mirroring
   `apply_stop_add_undo` (crates/rm-db/src/edit/add_stop.rs). No forward
   edit destroys versioned rows. Subroute `LIMIT 1` precedent is safe
   (392/392 single-subroute in taichung.db).
8. **`--delete-pole` is one-way** (user decision 2026-10-17): an edit
   carrying it is `NotUndoable` as a whole; the dry-run prints a loud
   irreversible warning. Detach-only stays fully undoable (pointer
   restore).
9. **Duplicate occurrences refuse** (user decision 2026-10-17): when a
   stop appears more than once in a direction (loop variant; real case:
   208 東勢 at seq 29 and 42), apply refuses unless `--sequence` picks one
   occurrence or `--all` detaches every occurrence. Dry-run lists all
   occurrences.
10. **extend-path reads the graph from `--db`** (builder-check finding):
    `RoadGraph::from_conn` over the same SQLite connection — no separate
    prepared graph file exists. Empty `osm_node` → rejected with "run
    rm-osm-prepare".

## Command surface

Read (no `--apply`, no side effects, work on `open_readonly`):

| Command | Input | Output (data) |
|---|---|---|
| `find-stop` | `xno` + `name` or `code` (fuzzy match allowed) | candidate poles: `stop_version_id`, `stop_id`, `stop_code`, `stop_name`, `station_id`, `station_name`, lat/lon, directions served |
| `show-route` | `xno` (route number as shown in rm-web) | route_id, per-direction: stop sequence (code/name/`stop_version_id`/chainage), path state (`src` / `override` / none), stale marks |
| `show-path` | `xno` + `direction` | effective encoded polyline (override-preferred), endpoint coordinates, which source won |

Write (dry-run default; `--apply` executes; single tx via `rm_db::edit`):

| Command | Wraps | Notes |
|---|---|---|
| `add-stop` (exists) | `plan_add_stop` / `add_stop` | gains `--json` |
| `move-stop` | `plan_stop_edit` / `edit_stop` | lat and/or lon and/or rename; identifies pole by `stop_version_id` (from `find-stop`) or `--xno` + `--code` |
| `remove-stop` | **new detach primitive** | see below |
| `extend-path` | graph walk + `apply_line_edit` | see below |
| `undo` (exists) | `undo_edit` | gains `--json` |

Envelope (every command, stdout, exit code alongside):

```json
{"status": "ok", "command": "move-stop", "data": { "...": "..." }, "logs": [], "duration_ms": 42}
```

## New rm-db primitive: detach stop from direction

`rm_db::edit` gains one primitive, one plan/apply pair, mirroring the
add-stop discipline:

- Plan (read-only): resolve (xno, direction, stop_code/name) → the
  current `route_stop_version` row(s); verdicts: found / unknown route /
  unknown direction / unknown stop / **duplicate occurrence** (N>1 in the
  direction — apply refuses; `--sequence` picks one, `--all` takes them
  all) / **pole becomes orphan** (no other direction references it) /
  terminal stop (sequence shift affects neighbors' ordering), plus which
  tables go stale.
- Apply (one tx): mint the new route-version chain minus the detached
  occurrence(s) (decision 7), move the current pointer, write `edit_log`
  (`entity`, action `detach`, changes carrying xno/direction/stop_code/
  old sequence(s)), mark stale like add-stop does for its route.
- Undo: pointer restore to the previous route_version, dispatched from
  `undo_edit` via a new arm (guard: `NotUndoable` when the current
  pointer has moved on — same discipline as the other undo ops).
- Orphan pole: plan reports it; apply refuses unless `--delete-pole`,
  which additionally removes the pole version chain + station link and
  makes the whole edit one-way (decision 8). No forward pole-removal
  teardown exists today; the builder writes it modeled on the rows
  `apply_stop_add_undo` deletes (it removes exactly what add-stop
  minted).

## New command internals: extend-path

Input: `xno`, `direction`, `meters` (default 10), `end`
(`first|last|both`, default `both`).

Algorithm (dry-run shows everything; apply writes once):

1. Load the effective path for the direction: polyline_override if
   present, else src polyline (`effective_encoded` semantics), else
   `matched_path` from the match tables when no src exists — decoded via
   `rm-core::polyline`. No path at all → rejected with a clear reason.
2. Load the OSM graph from the same `--db` connection via
   `RoadGraph::from_conn` (the matcher's own path — decision 10; no
   separate graph file exists). Clip rule respected: this only reads the
   graph, never slims it.
3. For each requested end: find the graph node nearest the path's
   endpoint; walk outward (Dijkstra from that node, excluding the edge
   segment leading back into the path) until cumulative length ≥
   `meters`. When several outward branches exist, pick the straightest
   continuation relative to the path's incoming bearing (default chosen
   by the builder; the dry-run prints the chosen branch's way name(s) so
   the human can veto before `--apply`).
4. Candidate path = original + extension vertices. Dry-run output:
   extension length, way names, first/last new vertex coordinates,
   endpoint snap distance. Apply: encode and write via `apply_line_edit`
   (override created or updated; `expected_updated_at` guard when an
   override already exists).

Failure modes to surface as rejections, not panics: endpoint farther than
a sane snap radius (builder: reuse add-stop's QUALIFY_MAX_DIST_M or
matcher's snap threshold) from any graph node; `osm_node`/`osm_way`
tables empty (error says to run `rm-osm-prepare`).

## Skill / runbook

`docs/agents/rm-jobs.md` gains the read/write command catalogue and the
canonical job loop (schema → find-stop → dry-run → show human → --apply →
report → optionally undo). A Claude skill wraps the same loop. Both must
state: heavy commands run release-mode; rm-web may show stale derived
layers until `rm-registry --stale` runs (same as manual edits today).

## What's left to the builder

- schemars/clap mechanics for `schema` and `--json` payload validation,
  and the exact envelope struct (one serde type, golden tests per
  candidate-2 discipline).
- EditError → envelope `status`/message mapping table (400-ish vs
  conflict-ish is already sketched in rm-web's `edit_conflict`).
- The detach primitive's exact edit_log `entity`/`changes` shape and its
  undo op struct (pointer-restore flavor, decision 7).
- The `--delete-pole` teardown SQL (new; modeled on the rows
  `apply_stop_add_undo` deletes).
- Which stale tables a detach marks (mirror add-stop; verify with the
  stale module's table list).
- Straightest-continuation tie-break for extend-path; snap radius.
- Read-command SQL: reuse rm-web `queries/routes.rs` shapes where they
  fit (they already carry the projection-QA joins); keep the
  sqlite_master degrade for tables newer than old DBs.
