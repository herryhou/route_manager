# Polyline override 手動修線 (phase 2) — design spec

Date: 2026-10-11
Status: approved in chat (pivot from piece model), pending spec review
Supersedes: the piece-splice design track within
`2026-10-10-polyline-piece-edit-design.md` (phase 1 storage/UI shipped; this
spec retires the piece model and replaces it with whole-line override)

## Context

Phase 1 shipped manual piece editing (span + coordinate anchors + guards) with
readers untouched. Phase 2 was planned as "readers splice pieces over src" —
an assembly module resolving piece anchors onto the src line, fail-loud on
unresolvable anchors, plus stale-mark wiring and an `osm-match --stale`
per-direction recompute.

Pivot rationale (agreed with herry 2026-10-11):

- Recompute after a geometry edit is unavoidable — derived data must
  regenerate. The real cost lever is blast radius + trigger cost, and
  per-direction `--stale` recompute (≈1–2 s Viterbi + seconds of
  segments/registry) already achieves that. The 11-minute number is the full
  812-direction run; stale runs never pay it.
- Piece granularity existed for a per-window recompute future that is now
  ruled out (Viterbi boundary effects; 0.1 s saving not worth seam-correctness
  risk) and for concurrent span edits that cannot happen (single-editor tool,
  house model: `docs/web-edit-mode.md`).
- Anchor/seam/overlap constraints existed only to make pieces safely
  spliceable in the assembly module. With the whole-line model there is no
  assembly — effective line = override if present, else src — so the
  constraints and the assembly module both disappear. Four of six phase-2
  BACKLOG items (degenerate-src expect, overlap normalization, no-src regime,
  anchor directionality) die with them.

## Decisions

1. **Whole-line override per (xno, direction).** A manual edit replaces the
   entire direction polyline. `effective = manual_line.encoded ?? src.encoded`
   — a COALESCE at every read site, no splicing math anywhere.
2. **Unversioned current-row table, optimistic concurrency token.** Same
   pattern as phase 1: one row per (xno, direction), `updated_at` doubles as
   the token, `expected_updated_at` echoed by the client, mismatch → 409.
   Writes serialized behind the app-level tokio Mutex.
3. **Validation = decode round-trip + ≥2 points.** No anchors, no seam
   tolerance, no overlap checks — nothing to splice, nothing to overlap.
4. **Undo is a forward write.** Reuses the phase-1 mechanism unchanged:
   edit_log `changes` carries old+new encoded; `undo_of` set; delete-vs-restore
   from `old` presence (undo of the first override edit deletes the row, back
   to pure src). Undo granularity = one save action.
5. **Stale marks in the same tx as apply/undo.** Phase-1-style whole-line
   marks (conventions from `mark_position_stale`, `crates/rm-db/src/edit.rs`):
   - `('seg_match', '{route_id}:{direction}')` — new consumer (see 6)
   - `('path_stop_projection', '{route_id}')` — existing segments stale run
   - `('stop_bearing', '*')` + `('station_bearing', '*')` — existing registry
     convention (whole-table; bearings depend on geometry via matched runs
     and the src-polyline fallback)
   Keys use route_id (stable across route versions); the match stale run
   resolves the current route_version at recompute time.
6. **osm-match gains `--stale`.** New per-direction run: read `seg_match`
   marks → for each `{route_id}:{direction}`: delete that direction's
   `seg_match_way` + `seg_match_stat` rows for the route (joined through
   subroute/route on route_id + direction — every route_version, so rows
   written under an older version cannot survive) → re-run Viterbi on the
   effective polyline → insert → clear marks (rowid+cause paired clear, the
   segments/registry pattern). The full run switches its input to the
   effective polyline too (all readers consume effective, unconditionally).
7. **UI: whole-line editing.** Raw compare loads the effective line; edit
   mode exposes all vertices for drag/insert/delete; save = whole-line POST.
   No span picking, no anchors, no piece overlay. The compare view toggles
   raw (capture) vs effective (what match consumes); unresolved-override
   errors cannot exist, so no error surface is needed.
8. **Phase-1 piece model retired.** Migration drops `polyline_piece`
   (edit_log history rows are append-only and stay). Deleted: span-pick UI,
   anchor/overlap/seam guards, piece planning layer, `manual_pieces` payload.
   Survives: edit_log forward-write undo, three-way undo dispatch, 線史
   panel + polyline rows, optimistic-token plumbing, vertex-editing UI,
   edit-gated route mounting.

## Schema (migration `0013_polyline_override`)

```sql
CREATE TABLE polyline_override (
  xno         TEXT NOT NULL,
  direction   INTEGER NOT NULL,
  encoded     TEXT NOT NULL,   -- full replacement geometry, ≥2 points
  updated_at  TEXT NOT NULL    -- = producing edit's ts (concurrency token)
);
-- exactly one current override per direction
CREATE UNIQUE INDEX polyline_override_dir ON polyline_override (xno, direction);

DROP TABLE polyline_piece;
```

No `updated_by` column — house rule (`web-edit-mode.md`): edit_log is the
audit trail. `polyline_override` has no surrogate PK; (xno, direction) is the
identity, so no id_sequence involvement.

edit_log reuse (same shape as phase 1, minus piece fields):
- `entity = 'polyline'`, `entity_id = route_id` (resolved via
  `ext_uid = 'tcg:' || xno`), `action`: `'create' | 'update' | 'undo'`.
- `changes` JSON: `{kind:'polyline_override', xno, direction,
  encoded:{old?, new}}` — `old` absent on create; undo rows carry the same
  shape with the restored state as `new`.

## rm-edit / rm-db split (house pattern)

- `rm-edit` (zero SQL):
  - `LineEditOp { xno, direction, points, expected_updated_at: Option<_> }`,
    `plan_line_edit` (pure): decode round-trip, ≥2 points, currency input.
    (The piece planning layer's anchors/overlap/seam guards do not exist here.)
  - `plan_line_undo` (pure): kind guard `'polyline_override'`, refuse
    `action='undo'` (no undo-of-undo), refuse when the line has moved on
    (currency via `updated_at`), restore-vs-delete from `old` presence.
  - `edit_polyline_line` / `undo_polyline_line` services (load→plan→apply).
- `rm-db`:
  - `load_polyline_line(xno, direction)` → src encoded (latest batch),
      current override row, resolved route_id, current route_version_id.
  - `apply_line_edit`: one tx — currency guard before any write, upsert
      (INSERT ON CONFLICT(xno,direction) DO UPDATE), edit_log row, stale
      marks (decision 5).
  - `apply_line_undo`: one tx — guards before writes; upsert old state or
      DELETE; edit_log row `action='undo'`, `undo_of`; same stale marks.
  - `effective_encoded(conn, batch_id, xno, direction)` → the COALESCE read:
      override encoded if present else src encoded (used by match/segments/
      registry load sites and the raw payload).

## HTTP (edit-gated, mounted only with `--edit`)

- `POST /api/edit/polyline/{xno}/{dir}/line`
  body `{points: [[lat,lon],…], expected_updated_at?, note}`
  → `{edit_id, updated_at}`. 400 validation (decode/points/unknown route),
  409 currency. (The phase-1 `/piece` endpoint and handler are removed.)
- `POST /api/edit/undo/{edit_id}` — polyline branch kept, response
  `{edit_id, restored: {deleted: true}}` (no piece_id — nothing to identify).
- `GET /api/routes/{id}/directions/{dir}/raw` — payload gains
  `effective: {points, source: 'override'|'src'}` alongside the raw points;
  `manual_pieces` removed.

## Readers

- `rm-osm-match` jobs query: src `srp.encoded` → `effective_encoded`;
  new `--stale` flag per decision 6 (per-direction delete→match→insert).
- `rm-segments` (`lib.rs:165` reads src encoded): → `effective_encoded`.
  Existing `--stale` run and `path_stop_projection` marks unchanged.
- `rm-registry` (`bearing.rs:128` polyline fallback): → `effective_encoded`.
  Existing `--stale` runs unchanged.
- The runbook sequence after a line edit:
  `rm-osm-match --stale` → `rm-segments --stale` → `rm-registry --stale`
  (README gains the match line; marks accumulate across edits, one run
  processes all).

## UI

Edit mode, raw layer:
- Effective line renders as the editable line (distinct style from raw
  capture); all vertices draggable; insert/delete as phase 1 vertex tools.
- Save = confirm + note → POST → raw refetch. Undo via the existing
  editLogPanel (entity polyline rows; changeText shows 改/刪 by
  `encoded.new == null`).
- Compare toggle: raw capture line vs effective line (source shown).
- Gotcha carried from phase 1: assets are `include_str!`-baked — frontend
  changes need `cargo build -r -p rm-web` + server restart.

## Non-goals

- Per-window / span-local recompute (ruled out: Viterbi boundary effects).
- Multi-editor concurrency (single-editor house model; token → 409 only).
- Batch multi-direction editing; whole-route multi-direction replace ops.
- Keeping any piece-model code paths alive behind a flag.
- Migrating phase-1 piece data (taipei.db test rows are throwaway; edit_log
  history stays as history).

## Testing

- rm-edit unit: plan guards (decode, ≥2 points, currency; undo
  kind/undo-of-undo/moved-on/restore-vs-delete).
- rm-db tx tests: upsert create/update, currency 409, undo restore,
  undo-first-edit deletes, stale-mark rows for all four (table_name, key)
  pairs on apply AND undo, effective_encoded COALESCE both branches.
- rm-osm-match: `--stale` per-direction delete+insert idempotency; full run
  consumes effective input (fixture with an override changes the result);
  marks cleared, untouched directions' rows byte-identical.
- Handler tests: 401/403/absent-without-`--edit`/happy readback/409 race;
  raw payload `effective` field both sources.
- E2E drive on a taipei.db copy: edit line → run the three `--stale` steps →
  matched geometry/chainage/bearings change for that direction only; compare
  toggle shows raw vs effective; undo restores and re-marks.
