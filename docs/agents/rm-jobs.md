# rm-job: agent-driven data jobs

Natural-language data jobs ("add a stop X between A and B on route 12") run
through the `rm-job` CLI — a thin dry-run-first wrapper over `rm_db`'s
manual-edit primitives. Every write is planned before it happens, undoable,
and audited in `edit_log`. Always release mode: `cargo run -r -p rm-job -- ...`.

The default DB is `data/taichung.db` — for another city pass
`--db data/<city>.db` (e.g. `--db data/taipei.db`) on every command. A
typo'd `--db` path never errors: `rm_db::open` creates a new empty DB
there.

## When to reach for rm-job

- The user asks in natural language for a **new stop** attached to one or
  more route directions — `rm-job add-stop` is the only write path. Never
  hand-write SQL against the published tables.
- Editing an *existing* stop (rename, move, regroup) is not an rm-job job:
  those go through rm-web's edit UI (backed by the same `rm_db::edit`
  primitives).

## The dry-run-first rule

**Never pass `--apply` before the plan has been shown to the user.** Every
job runs dry by default and prints the exact plan — station verdict, insert
position and neighbours per attachment, per-table row counts, stale routes,
warnings. Show that output, get the user's go-ahead, then re-run the
identical command with `--apply`. Exit codes: `0` plan/apply ok, `1`
rejected (reason on stderr), `2` no-op (no `--attach` given).

By default the stop joins an existing station when name, distance and
bearing all qualify, else creates a standalone one; `--station <ID>` pins
the station and skips the search.

## Attach syntax

`--attach XNO:DIR[@CODE]`, repeatable (`XNO` = route number, `DIR` =
direction):

- `--attach 12:1` — append the stop to the end of route 12 direction 1.
- `--attach 12:1@0045` — insert after the stop whose `stop_code` is `0045`
  on that direction; downstream stops re-version.
- Repeat the flag to land one stop on several directions in a single job.

Coordinates come from the user: right-clicking the rm-web map copies
`<lat>, <lon>` (six decimals) to the clipboard. Ask the user to copy the
spot and paste the coordinates — never invent or look them up.

## Undo

`--apply` prints `edit_id <N>`; keep it and report it to the user.
`rm-job undo <N>` reverts the whole job — every row the apply wrote — in
one transaction. Undo in LIFO order — newest job first: edits interleaved
after a job (rm-web pole edits included) make its undo refuse with
`NotUndoable` instead of rewinding over them.

## The refresh rule

**The job ends at stale marks and a report — a human presses refresh.**
Apply writes the stop plus `stale_mark` rows and prints
`refresh needed for routes: ...`; it never runs the pipeline. Tell the user
to press rm-web's refresh (chain: match → segments → registry — registry is
the step that rebuilds the new stop's bearings), then check the stop on the
map.
