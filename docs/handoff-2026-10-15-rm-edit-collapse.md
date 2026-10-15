# Handoff: route_manger — post rm-edit collapse

Written 2026-10-15 after the collapse branch merged; updated 2026-10-16
after candidates 3 and 2. Repo: `/Users/herry/project/rust/route_manger`,
master at `ed25484`, pushed, clean tree, 350 tests green
(335 + 15 golden shape), `cargo fmt --check` clean.

## Just finished (do not redo)

The rm-edit crate is deleted; all edit primitives live in `rm_db::edit` —
one module per kind (`stop.rs`, `line.rs`, `station_move.rs`,
`station_rename.rs`, `station_undo.rs`), each with a private plan policy
emitting apply ops directly and service entries re-exported at the crate
root. 8 commits 220cd3c→06dd320 + fast-forward merge 892b05b. Full
narrative, pattern notes, and the private-mod whitelist trap are in memory:
`.claude/projects/-Users-herry-project-rust-route-manger/memory/collapse-rm-edit-merged.md`
(see MEMORY.md index). HTTP contract was verified unchanged by the 17
HTTP-level tests in `crates/rm-web/tests/edit.rs` — same JSON keys, same
400/409 split via `edit_conflict` downcasting `rm_db::EditError`.

## Immediate loose ends (small)

1. ~~Stale `rm-edit` references~~ done 2026-10-16 (`d572e80`): living docs
   point at `rm_db::edit`; archive plan docs left as-is.
2. ~~`unused variable: runs`~~ done 2026-10-16 (`d2dfe94`): the test was
   assertion-free; completed it.
3. `docs/agents/` + `GLOSSARY.md` exist (agent-skills setup done earlier
   this session); issue tracker is GitHub via `gh`.

## Environment facts a fresh session needs

- **fmt hook is live**: `core.hooksPath=.githooks` (local config — after a
  re-clone, re-run `git config core.hooksPath .githooks`). Every commit
  runs `cargo fmt --check` over the whole tree; bypass with `--no-verify`.
  Established 2026-10-15 (commits 2812a09 sweep + acb1d4c hook).
- **rtk CLI proxy** wraps shell commands (token-optimized output); see
  `~/.claude/RTK.md`. `rtk proxy <cmd>` for raw output.
- Project rules live in `CLAUDE.md` (release builds for pipeline binaries,
  no programmatic zoom/pan whitelist in rm-web, byte-diff discipline for
  numeric refactors, migrations-vs-readonly-viewer degradation, OSM
  clip-never-delete). `crates/rm-web/DESIGN.md` gates any UI work.
- User conventions: 每步 commit, explicit triggers before new work,
  spike-first for perf, worktree finish flow = keep-exit → merge →
  rm -rf + `git worktree prune`.

## Next work: architecture candidate 4

The survey report is archived at `docs/architecture-review-2026-10-15.html`
(re-run `/mattpocock-skills:improve-codebase-architecture` only if you want
a fresh survey rather than the queued candidates).
Candidates 1 (collapse rm-edit), 5 (tagged undo outcome — master
`a54c147`), 3 (stale-mark owner — master `fbbd5ec`, 2026-10-16) and
**2 (shape authority — master `ed25484`, 2026-10-16: serde struct per
endpoint response in queries/{stops,routes,qa,pairs}.rs, rows fill by
column name, wire bytes preserved — the station_nearest latitude/longitude
drift kept and pinned; golden wire-shape contract in
`crates/rm-web/tests/shape.rs` (15 tests, exact-key-set walker) written
green pre-refactor; JSDoc typedefs in api.js mirror the structs;
6 commits 0ef9a81→ed25484; 350 tests green)** are done; queued:

- **Candidate 4**: split app.js along its seams — candidate 5 already
  unified the undo fields, so the undo skeleton is now trivial; JS has no
  test infra, so verification is manual browser pass. Last.

Process for the next pick: read the report card, then grill the user
(`/mattpocock-skills:grilling`) to a design contract before touching code,
same as candidates 1 and 2 did. Straight-to-code in a worktree is the
agreed mode (no spec/tickets); confirm per-candidate.

## Suggested skills

- `mattpocock-skills:improve-codebase-architecture` — if the temp report
  is gone, re-run the survey; otherwise just read the HTML.
- `mattpocock-skills:grilling` — frontier interview before implementing any
  candidate (candidate 1 ran Q1–Q8 + a design contract this way).
- `mattpocock-skills:codebase-design` — the module/seam/depth vocabulary
  the report and any refactor proposal speak.
- `mattpocock-skills:ask-matt` — if the user asks "what next" at a flow
  level rather than picking a candidate.

No secrets or PII in this document.
