# Polyline piece 手動編修 (phase 1) — design spec

Date: 2026-10-10
Status: approved in chat, pending spec review

## Context

`src_route_point.encoded` (Google polyline 1e-5, per batch/xno/direction) is the
capture-truth geometry every downstream step consumes: rm-osm-match (Viterbi
input), rm-segments (stop chainage), rm-registry (bearing), rm-web raw compare.
Raw captures carry loops, detours and noise; QA needs to fix them by hand.

Agreed with herry 2026-10-10: **manual editing UI, vertex-level ops
(drag/add/delete), phased**. Phase 1 = UI + storage only — no stale marks, no
downstream recompute, readers unchanged. Phase 2 (separate future spec):
readers splice manual pieces over src, stale_mark wiring, refresh runbook
update.

## Decisions

1. **Piece-level, anchored on src.** A manual piece replaces a span of the
   src polyline, not the whole line, and not the matched
   `seg_geometry_draft` pieces (those are derived-from-match; editing them
   would break the recompute story). Piece granularity = small undo blast
   radius, no whole-line clobber, and phase-2 recompute can be per-window.
2. **Unversioned current-row table.** One row per piece = current, schema
   native. No `valid_from`/`valid_to`, no close+mint, no chain walk. History
   lives in `edit_log.changes` (old/new encoded per edit), which already
   duplicates anything a version table would store. Precedent: auxiliary
   tables (`stale_mark`) are unversioned with edit_log as the audit trail.
3. **Coordinate anchors, not vertex indices.** A piece stores its own full
   geometry plus the start/end anchor coordinates where it splices into the
   src line. Vertex indices shift under edits; coordinates are stable.
   Phase-2 assembly = find nearest anchor coords on src, cut, replace — no
   extra index needed.
4. **Optimistic concurrency, not versions.** `expected_updated_at` echoed by
   the client; apply tx compares before writing, mismatch → 409. Writes are
   already serialized behind the app-level tokio Mutex; the check just turns
   silent last-write-wins into a visible 409 (the race behavior scenario 7's
   e2e pinned for station edits).
5. **Undo is a forward write.** Undo reads `old_encoded` from the edit_log
   row and re-applies it as a new edit (`action='undo'`, `undo_of` set).
   Undo of a piece's first edit = DELETE the row (back to pure src) — no
   null-predecessor semantics needed, unlike version-chain entities.

## Schema (migration `0012_polyline_piece`)

```sql
CREATE TABLE polyline_piece (
  piece_id         INTEGER PRIMARY KEY,   -- minted via id_sequence
  xno              TEXT NOT NULL,
  direction        INTEGER NOT NULL,
  encoded          TEXT NOT NULL,         -- piece geometry, ≥2 points
  anchor_start_lat REAL NOT NULL,
  anchor_start_lon REAL NOT NULL,
  anchor_end_lat   REAL NOT NULL,
  anchor_end_lon   REAL NOT NULL,
  updated_at       TEXT NOT NULL          -- = producing edit's ts (optimistic-concurrency token)
);
```

No `updated_by` column — house rule (`web-edit-mode.md`): edit_log is the
audit trail, no per-table editor columns.

`xno` is stable route identity (`route.ext_uid = 'tcg:{xno}'`), and one-shot
import means a future capture starts a fresh DB file, so no cross-batch
semantics are needed. Src tables stay untouched capture truth.

`edit_log` reuse (no schema change):
- `entity = 'polyline'`, `entity_id = route_id` (resolved via
  `ext_uid = 'tcg:' || xno`), `stable_id` convention same as other entities.
- `action`: `'create'` (first write of a piece_id) | `'update'` | `'undo'`.
- `changes` JSON:
  `{kind:'polyline_piece', xno, direction, piece_id,
    encoded:{old?, new}, anchors:{old?, new}}` — `old` absent on create;
  undo rows carry the same shape with the restored state as `new`
  (`changes` on undo rows elsewhere is `'{}'`, but piece undo needs the
  payload to know delete-vs-restore; pinned here).

## rm-edit / rm-db split (house pattern)

- `rm-edit` (zero SQL):
  - `PieceEditOp { xno, direction, piece_id: Option<i64>, points,
      anchor_start, anchor_end }`, `plan_piece_edit` (pure): decode
      round-trip, ≥2 points, anchors ≠ equal, no-overlap against existing
      piece spans (chainage interval comparison on the src polyline),
      currency check input.
  - `plan_piece_undo` (pure): kind guard `'polyline_piece'`, refuse
      `action='undo'` (no undo-of-undo), refuse when the piece has moved on
      (currency via `updated_at`), decide restore-vs-delete from `old`
      presence.
  - `edit_polyline_piece` / `undo_polyline_piece` services (load→plan→apply).
- `rm-db`:
  - `load_polyline_edit(xno, direction)` → src encoded (latest batch),
      existing pieces, resolved route_id.
  - `apply_piece_edit`: one tx — currency guard (`expected_updated_at`
      vs row) before any write, id mint if `piece_id` absent, UPSERT,
      edit_log row (`updated_at` = edit ts). Edge pinned: `piece_id`
      given but row absent (deleted by a first-edit undo) — treated as
      create, requires `expected_updated_at` absent too, else 409.
  - `apply_piece_undo`: one tx — all guards before any write; UPSERT old
      state or DELETE; new edit_log row `action='undo'`, `undo_of`.
  - Zero stale marks (phase 1).

## HTTP (edit-gated, mounted only with `--edit`)

- `POST /api/edit/polyline/{xno}/{dir}/piece`
  body `{points: [[lat,lon],…], anchor_start: [lat,lon],
  anchor_end: [lat,lon], piece_id?, expected_updated_at?, note}`
  → `{edit_id, piece_id, updated_at}`. Server encodes via rm-core — the JS
  stays free of polyline math. 400 validation (decode/points/anchors),
  409 currency or overlap.
- `POST /api/edit/undo/{edit_id}` — dispatch gains a polyline branch →
  `{edit_id, restored: {piece_id, deleted}}`.
- `GET /api/routes/{id}/directions/{dir}/raw` — payload gains
  `manual_pieces: [{piece_id, points, anchor_start, anchor_end,
  updated_at}]` so the compare view renders the overlay.

## UI

Edit mode, raw layer:
- Click two existing raw vertices = cut a span (anchors). Vertices inside
  the span become editable: drag to move, button/keypress to delete,
  insert by clicking a segment midpoint affordance.
- Existing manual pieces render in a distinct style; clicking one re-opens
  it for editing (`piece_id` + `expected_updated_at` carried).
- Save = confirm + note → POST → raw refetch. Undo via the existing
  editLogPanel (entity polyline rows humanize: new `changeText` branch).
- Gotcha carried from scenario 5: rm-web assets are `include_str!`-baked —
  frontend changes need `cargo build -r -p rm-web` + server restart.

## Non-goals (phase 1)

- Readers never consult `polyline_piece` (match/segments/registry unchanged).
- No stale marks, no refresh-runbook wiring.
- No automatic overlap split/merge — overlapping span = 409, editor cuts
  again.
- No batch editing, no whole-line replace op (piece with span = full line
  is legal but not special-cased).

## Testing

- rm-edit unit: plan guards (decode, points, anchors, overlap, currency,
  undo kind/undo-of-undo/restore-vs-delete).
- rm-db tx tests (in-memory, house fixture style): upsert create/update,
  currency 409 path, undo restore, undo-first-edit deletes, overlap reject.
- Handler tests: 401 no token / 403 bad token / routes absent without
  `--edit` / happy path readback / 409 race / overlap 409.
- E2E drive on a taipei.db copy (playwright, house pattern: sessionStorage
  token before boot, fresh port, verify /api/stats batch timestamp).
