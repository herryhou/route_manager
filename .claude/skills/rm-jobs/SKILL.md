---
name: rm-jobs
description: Bus route/stop correction jobs through the rm-job CLI — add a stop, move or rename a stop, remove a stop from a direction, extend or replace a route path, regroup/rename/move a station, run the stale-mark refresh, undo a previous job edit. Use when the user asks to fix bus data in a route_manger city database (a pole is misplaced, a stop was deleted from the source, a route's drawn path is too short or drawn wrong, a station moved or got a new name, the map is stale after an edit).
---

# rm-jobs

Correct route/stop data through `rm-job` — dry-run first, human-approved,
undoable. Always release mode, from the repo root:
`cargo run -r -p rm-job -- ...`. Another city than Taichung: pass
`--db data/<city>.db` (or `"db"` in the payload) on every command.

Full runbook: `docs/agents/rm-jobs.md`. Parameter reference:
`rm-job schema <cmd>` — never guess field names.

## The loop

1. `rm-job schema <cmd>` — the command's payload schema (names, types,
   defaults, required fields).
2. `find-stop` / `show-route` / `show-path` — confirm the target pole,
   direction and current path state.
3. Dry run — the same command without `--apply` (`"apply": false`).
4. Quote the plan to the human; wait for a yes.
5. Re-run identically with `--apply` (`"apply": true`); report the
   `edit_id` it returns.
6. Tell the human the map stays stale until the refresh chain runs
   (match → segments → registry). Run it yourself with `rm-job refresh`
   (dry run first — pending-mark report — then `--apply`), or let the
   human press rm-web's refresh button. Pick one; don't interleave.
7. Wrong? `rm-job undo <edit_id>` — newest edit first (LIFO).

## Hard rules

- Dry-run before EVERY apply. No exceptions — even when the human
  described the edit exactly.
- Quote the plan to the human: the dry-run output is the approval
  artifact. Never apply from memory or a paraphrase of it.
- Never `--delete-pole` (remove-stop) without an explicit human yes — it
  tears the pole down one-way and makes the whole edit NotUndoable.
- Never apply while the plan shows warnings the human hasn't seen. Show
  the warning lines and get a yes that covers them.
- Never hand-write SQL against the published tables — `rm-job` is the
  only write path. Coordinates always come from the user (rm-web map
  right-click 「複製座標」), never invented.

Envelope: stdout is exactly one JSON object
`{"status":"ok|rejected|noop","command":"<cmd>","data":{...},"logs":[...],"duration_ms":N}`;
exit codes 0 ok / 1 rejected / 2 no-op. Exit 2 with no envelope means clap
rejected the invocation itself (usage error) — fix the flags, don't treat
it as a no-op.
