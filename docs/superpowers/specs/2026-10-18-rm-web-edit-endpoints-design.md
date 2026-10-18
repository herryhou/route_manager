# rm-web edit endpoints for add-stop / remove-stop / extend-path, then rm-job removal

Date: 2026-10-18
Status: approved in chat (herry), pending builder check

## Intent and why

`rm-job` is a local-only CLI: every command opens the SQLite file directly,
so an agent working remotely (no repo, no DB file) cannot use it. The
agent-facing surface becomes the rm-web HTTP API instead. Three write
commands have no web counterpart — add-stop, remove-stop, extend-path.
This design ports them as edit endpoints, then deletes the rm-job crate
and re-points the agent skill/docs at the API.

## Goals

- Agent can, over HTTP with a bearer token: add a stop (with attachments),
  detach a stop from a route (with the orphan/delete-pole guards), and
  extend a route's line along the road graph.
- Dry-run discipline survives the port: every new endpoint defaults to
  returning a plan and only writes when `apply: true`.
- After the endpoints land: `crates/rm-job` deleted; `.claude/skills/rm-jobs`
  replaced by an rm-web-API skill; `docs/agents/rm-jobs.md`, `CLAUDE.md`
  and `README.md` re-pointed.

## Non-goals

- No UI work: no buttons, panels, or map flows for the three operations.
- No changes to the eight existing edit endpoints.
- No new auth model: the existing `Editor` bearer-token extractor and
  `write_tokens` gating apply as-is.
- No rework of rm_db primitives — `plan_add_stop` / `add_stop`,
  `plan_detach_stop` / `detach_stop`, and the polyline edit path are
  already correct and tested at the rm-db layer.

## Anti-goals (failure even if it works)

- Losing the dry-run-first rule (an endpoint that only applies).
- Losing the `delete_pole` one-way guard (it must stay an explicit
  request field, never a default).
- A dependency cycle or layering violation between rm-db and
  rm-osm-match.

## Constraints

- Crate layering: rm-osm-match depends on rm-db and rm-core; rm-web
  already depends on rm-osm-match (refresh chain). Therefore the extend
  walk — which needs `rm_osm_match::graph::RoadGraph` — moves into the
  **rm-osm-match** crate, not rm-db (that would cycle).
- rm-db plan structs stay non-`Serialize` (existing deliberate choice,
  see `DetachPlan`); web handlers keep the mirror-struct pattern
  (`PlanData` in rm-job today).
- Coordinates in payloads follow the existing edit-endpoint conventions:
  lat/lon as plain numbers, range-checked (400 on out-of-range).
- Write endpoints run through `run_db_write` (single write lock);
  read-only plan legs may run through `run_db` (read-only conn).

## Decided (by herry)

- Dry-run via `apply: false` in the payload; plans are the default response.
- API endpoints only, no UI.
- After endpoints ship, rm-job is deleted in full (crate, skill, docs
  re-point). History under `docs/superpowers/plans|specs` stays.

## Decided (builder's calls, flagged for review)

- **Paths**, following the existing router idioms:
  - `POST /api/edit/stop` — add-stop (create; the existing
    `POST /api/edit/stop/{id}` is the update path, no conflict).
  - `POST /api/edit/stop/detach` — remove-stop (route-scoped, so the
    target rides in the body, not the path).
  - `POST /api/edit/polyline/{xno}/{direction}/extend` — extend-path
    (sibling of the existing `.../line` route).
- **Payloads** mirror the rm-job params minus `db`/`editor` (bearer
  supplies the editor):
  - add: `{ name, lat, lon, attach: ["XNO:DIR[@CODE]"...], station?,
    note?, apply? }` — `attach` strings parsed by `parse_attach`, which
    moves from rm-job into rm-db (next to `AttachCmd`).
  - detach: `{ xno, direction, code, sequence?, all?, delete_pole?,
    note?, apply? }`.
  - extend: `{ meters, end: "both"|"first"|"last", note?, apply? }`.
- **Plan responses**: serde mirrors of `AddStopPlan` / `DetachPlan`
  (warnings array included — the agent skill must surface them); extend
  plan returns `{ source, candidate: [[lat,lon]...], extensions }`.
  Apply responses mirror the existing edit endpoints
  (`edit_id`, new version ids, `created`, ...).
- **Extend module move**: `crates/rm-job/src/extend.rs` moves to
  `crates/rm-osm-match/src/extend.rs`; its `queries::line_snapshot`
  dependency is replaced by `rm_db`'s canonical line snapshot loader
  (`load_polyline_line`). `Extension` and helpers move unchanged. The
  graph-walk tests in `crates/rm-job/tests/extend.rs` that exercise
  `plan_extension` directly (midseg snap, far endpoint, empty osm
  tables) move with it as rm-osm-match unit/integration tests; the
  CLI-flag tests die with the CLI.
- **Web tests**: rm-web integration tests in the `tests/api.rs` style
  (tower `oneshot` against `router_with`): plan-vs-apply for each of the
  three, bearer 401/403, warnings surfaced in plan output, extend
  apply-writes-override + undo round-trip through
  `POST /api/edit/undo/{edit_id}`.

## Left to the builder

- Exact response field names for the add/detach mirrors (keep them equal
  to rm-job's envelope `data` keys so the skill rewrite is a transport
  change only).
- Whether the extend plan leg reuses one DB connection for
  snapshot + graph + walk (it should; read-only).
- Error mapping: reuse `edit_conflict` for `EditError` races; plan-time
  rejections are 400 with the rm-db error text, matching existing
  endpoints.
- README/CLAUDE.md wording for the removal phase.
