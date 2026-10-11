# Backlog

Small known debts, oldest last. Not bugs — each shipped with a ruling that
the cost was acceptable for now. Source noted per item.

## From polyline-override review (2026-10-11, branch merge ee6b415)

- **`run_stale` scope dedup for hand-edited keys** — stale_mark keys are
  written by code as `{route_id}:{direction}`, but the table is editable, and
  a hand-inserted duplicate pair (e.g. `5:0` and `05:0` parsing to the same
  (rid, dir)) would put the pair in the delete scope twice. Harmless today
  (idempotent DELETEs), just wasteful. Fix: dedup `scope: Vec<(i64,i64)>`
  before `write_outcomes` in `crates/rm-osm-match/src/lib.rs`.

- **`raw()` payload runs 3 queries for one override row** —
  `crates/rm-web/src/queries/routes.rs` fetches the polyline_override row in
  separate existence + field queries. One
  `SELECT encoded, updated_at` + `optional()` would do. Plan-verbatim code,
  style only.

- **No fixture test that segments/registry consume overrides** — the
  override reader swap is pinned by tests only in rm-osm-match
  (`full_run_consumes_override_over_src`, stale tests); rm-segments and
  rm-registry read `effective_encoded` too but are covered only by the
  2026-10-11 e2e drive. One small test per crate would pin the reader swap
  against future regressions.

- **Garbage override blocks UI re-edit** — an override that fails decode
  makes `effective.points` empty, so the ≥2-point guard in
  `crates/rm-web/assets/js/lineEdit.js` (`startLineEdit`) won't arm; the line
  can't be re-edited from the map. Recovery works: undo via 線史 deletes the
  override row, src takes over. Worth one line in `docs/web-edit-mode.md`
  (and, if it bites in practice, a server-side "allow save over undecodable
  override" escape hatch).
