# rm-job: agent-driven data jobs

Natural-language data jobs ("move the 大坑橋 stop", "add a stop X between A
and B on route 12", "extend route 1's path past its end") run through the
`rm-job` CLI — a thin dry-run-first wrapper over `rm_db`'s manual-edit
primitives. Every run prints exactly one JSON envelope to stdout; every
write is planned before it happens, audited in `edit_log`, and (barring the
one-way exception below) undoable. Always release mode:
`cargo run -r -p rm-job -- ...`.

The default DB is `data/taichung.db` — for another city pass
`--db data/<city>.db` (e.g. `--db data/taipei.db`) on every command; in a
`--json` payload the `"db"` field is required — always state it. A typo'd
DB path never errors: `rm_db::open` creates a new empty DB there. The
read commands and extend-path dry runs open the DB read-only;
add-stop/move-stop/remove-stop open read-write even for a dry run, so a
dry run against an older DB applies pending migrations.

## Commands

Read (no side effects, open the DB read-only):

| Command | Returns |
|---|---|
| `find-stop` | Current poles on one route matching a name substring / exact code |
| `show-route` | One route's current stops per direction + each direction's path source |
| `show-path` | One direction's effective polyline: source, encoded, endpoints |

Write (dry run by default; `--apply` / `"apply": true` writes):

| Command | Does |
|---|---|
| `add-stop` | New pole + station, attached to one or more route directions |
| `move-stop` | Move/rename one existing pole (writes a new stop version) |
| `remove-stop` | Detach a stop occurrence from one route direction |
| `extend-path` | Walk the OSM road graph outward from a direction's path ends; writes a polyline override |
| `undo` | Revert one applied job edit by its `edit_log` id |

Field lists live in the binary, not in this page: `rm-job schema` lists
every command, `rm-job schema <cmd>` prints that command's `--json` payload
schema — names, types, defaults, required fields. That output is the
authoritative parameter reference; this page deliberately does not
duplicate it.

## Invocation

Every command accepts either classic flags or one `--json` payload; when
`--json` is given the flags are ignored. Build the payload from
`rm-job schema <cmd>`:

```sh
rm-job schema extend-path
rm-job extend-path --json '{"db":"data/taichung.db","xno":"1","direction":0,"meters":30}'
```

## Envelope and exit codes

Stdout is exactly one JSON object on every run — parse it first:

```json
{"status":"ok|rejected|noop","command":"<cmd>","data":{...},"logs":[...],"duration_ms":N}
```

`data` carries the command's payload (rejection and no-op reasons land in
`data.reason`) and `logs` human progress lines; both are omitted when
empty. Exit codes: `0` ok, `1` rejected (the reason also goes to stderr),
`2` no-op. One caveat: when clap itself rejects the invocation (unknown or
missing flag) it exits `2` with a usage line on stderr and **nothing on
stdout** — treat "exit 2 without an envelope" as a usage error, not a
no-op.

## The canonical job loop

1. `rm-job schema` — discover the commands; `rm-job schema <cmd>` for the
   payload you are about to send.
2. `find-stop` / `show-route` / `show-path` — ground truth first: which
   pole, which direction, what the path looks like now.
3. Dry run — the same command without `--apply`.
4. Show the human the dry-run output and wait for a yes. **The dry-run
   output is the approval artifact** — quote the plan, never apply from
   memory or a paraphrase of it.
5. Re-run the identical command with `--apply` (or `"apply": true`) and
   report the `edit_id` it prints to the user.
6. Wrong? `rm-job undo <edit_id>`.

## When to reach for rm-job

- The user asks in natural language for a route/stop correction on the
  published data: a **new stop**, a stop **moved or renamed**, a stop
  **removed from a direction**, a path **extended** past its ends, or an
  **undo** of a previous job. `rm-job` is the only write path — never
  hand-write SQL against the published tables.
- Free-form station surgery (regrouping, splitting, merging stations) is
  not an rm-job job: those go through rm-web's edit UI (backed by the same
  `rm_db::edit` primitives).

## Warnings that matter

- **Release mode.** Debug builds are 10–30x slower — `cargo run -r -p
  rm-job -- ...` always.
- **`--delete-pole` is one-way.** `remove-stop --delete-pole` tears the
  orphaned pole down and makes the whole edit `NotUndoable`. Every plan
  that would delete the pole carries a loud warning line. Never pass it
  without an explicit human yes.
- **rm-web's derived layers stay stale** after any apply until the refresh
  chain runs — see the refresh rule below.
- **The dry-run output is the approval artifact** — always show it before
  applying.

## add-stop specifics

**The dry-run-first rule.** **Never pass `--apply` before the plan has
been shown to the user.** Every job runs dry by default and prints the
exact plan — station verdict, insert position and neighbours per
attachment, per-table row counts, stale routes, warnings.

By default the stop joins an existing station when name, distance and
bearing all qualify, else creates a standalone one; `--station <ID>` pins
the station and skips the search.

**Attach syntax.** `--attach XNO:DIR[@CODE]`, repeatable (`XNO` = route
number, `DIR` = direction; in the payload the `"attach"` array holds the
same strings):

- `--attach 12:1` — append the stop to the end of route 12 direction 1.
- `--attach 12:1@0045` — insert after the stop whose `stop_code` is `0045`
  on that direction; downstream stops re-version.
- Repeat the flag to land one stop on several directions in a single job.

Coordinates come from the user: right-clicking the rm-web map opens a small
menu — 「複製座標 <lat>, <lon>」 — and clicking the menu item copies
`<lat>, <lon>` (six decimals) to the clipboard. Ask the user to copy the
spot and paste the coordinates — never invent or look them up.

## move-stop and remove-stop specifics

`move-stop` resolves its target either from an explicit
`stop_version_id` (`find-stop` prints it) or from `xno` + `code`; a code
that matches more than one pole on the route is rejected — resolve the
ambiguity with `find-stop` and pass the id. Give at least one of lat/lon/
`stop_name`; the dry run shows exactly the fields that would change.

`remove-stop` detaches `xno` + `direction` + `code`. When the code occurs
more than once in the direction the plan says so — disambiguate with
`sequence` (the `stop_sequence` to take) or `all`. The plan reports
`pole_orphan` / `serving_directions`: without `--delete-pole` an orphaned
pole survives, detached but present.

## extend-path specifics

`extend-path` snaps the direction's path ends to the OSM road graph and
walks outward `meters` (default 10) on `end` = `both` (default), `first`
or `last`. The apply writes the whole extended polyline as a polyline
override, so it needs the OSM graph tables in the DB (`rm-osm-prepare`) —
without them the dry run rejects with a "run rm-osm-prepare" message. A
dry run that returns empty `new_vertices` means the graph offers no
extension within the requested distance.

## Undo

`--apply` prints `edit_id <N>`; keep it and report it to the user.
`rm-job undo <N>` reverts the whole job — every row the apply wrote — in
one transaction. Undo in LIFO order — newest job first: edits interleaved
after a job (rm-web pole edits included) make its undo refuse with
`NotUndoable` instead of rewinding over them — and so does any edit that
used `--delete-pole`.

## The refresh rule

**The job ends at stale marks and a report — a human presses refresh.**
Apply writes the edit plus `stale_mark` rows and reports the stale routes;
it never runs the pipeline. Tell the user to press rm-web's refresh
(chain: match → segments → registry — registry is the step that rebuilds
the edited stops' bearings), then check the result on the map.
