# rm_db function usage by consumer crate

Which `rm_db` functions each crate actually calls. Survey of qualified
`rm_db::…` call sites plus `use rm_db` imports, `src/` + `tests/`,
across all workspace crates (2026-10-17). Schema/migration background
lives in `crates/rm-db/README.md`; this doc is only the usage map.

## Connection layer — the only door

Every crate gets its connection from `rm_db`; nobody else calls
`rusqlite::Connection::open*` directly.

| Function | rm-web | rm-ingest | rm-identity | rm-registry | rm-publish | rm-osm-prepare | rm-osm-match | rm-segments | rm-verify |
|---|---|---|---|---|---|---|---|---|---|
| `open` (write) | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| `open_readonly` | ✅ | — | — | — | — | — | — | — | — |
| `open_in_memory` | tests | tests | tests | tests | tests | — | tests | tests | tests |

`open_readonly` is rm-web-only: the viewer is a WAL reader beside the
pipeline writer and never migrates. `open_in_memory` is the workspace
test fixture (production use: none).

## Function → consumers matrix

| Function / item | Callers | Role |
|---|---|---|
| `now_rfc3339` | rm-web, rm-ingest, rm-identity, rm-registry, rm-publish, rm-osm-prepare | `created_at` / `valid_from` timestamps |
| `mint` | rm-identity (8 sites), rm-publish (4), rm-registry (2) | per-table id space, never reused |
| `create_batch` | rm-ingest | register import batch (`OPEN`) |
| `set_batch_status` | rm-ingest, rm-identity | move `OPEN` → `CLOSED` / `FAILED` |
| `latest_open_batch` | rm-identity | default `--batch` target |
| `latest_batch` | rm-web, rm-registry, rm-publish, rm-osm-match, rm-segments, rm-verify | most recent batch (any status) |
| `effective_encoded` | rm-web, rm-registry, rm-osm-match, rm-segments | read current version — the shared "what is current" reader |
| `entity_history` | rm-web | edit audit / undo panel |
| `edit_stop` + `StopEdit` | rm-web | stop edits (position, poles) |
| `rename_station` + `StationRename`, `RenameLevel` | rm-web | station rename (3 levels) |
| `move_station` + `StationMove` | rm-web | stop ↔ station regrouping by move |
| `regroup_stop` + `RegroupStop` | rm-web | stop ↔ station regrouping |
| `edit_polyline_line` + `LineEditCmd` | rm-web | polyline line edits |
| `undo_edit` + `UndoOutcome` | rm-web | undo dispatcher over the above |
| `EditError` (+ 11 variants matched) | rm-web | edit semantics → HTTP status mapping |
| `stale::snapshot` | rm-osm-match, rm-segments, rm-registry | snapshot marks before a pipeline run |
| `stale::clear_all_if_unchanged` | rm-osm-match, rm-segments, rm-registry | clear marks after an identical rerun |
| `stale::StaleTable::SegMatch` + `parse_seg_match_key` | rm-osm-match | own stale scope |
| `stale::StaleTable::PathStopProjection` | rm-segments | own stale scope |
| `stale::StaleTable::StopBearing` / `StationBearing` | rm-registry | own stale scope |
| `stale::bearings_stale` | rm-web | stale badge |
| `stale::pending_count` | rm-web | refresh button badge |
| `stale::projection_routes_for_stop` | rm-web | per-stop staleness detail |

`rm-core` touches no DB (pure domain types); `rm-osm-prepare` touches
the OSM graph tables via its own connection from `rm_db::open` only.

## Producer / consumer split on stale marks

Pipelines **clear** stale marks for the tables they rewrite; rm-web
**reads** them for UI badges. Nobody crosses the line:

- `rm-osm-match` → `SegMatch`
- `rm-segments` → `PathStopProjection`
- `rm-registry` → `StopBearing`, `StationBearing`
- `rm-web` ← `bearings_stale`, `pending_count`, `projection_routes_for_stop`

`snapshot` + `clear_all_if_unchanged` is the idempotence hook: a rerun
that produces identical rows leaves no stale mark (pairs with the
byte-diff verification discipline).

## Edit writes: rm-web only

All mutation of published data through the `edit` module happens in
rm-web handlers (token-authenticated edit mode) via
`run_db_write` → `rm_db::open`. Pipeline binaries INSERT new versions;
they never call `edit_*` / `undo_edit`.

## Pub but no external caller

`pub` items whose callers live inside rm-db itself (or arrive through
a wrapper): `migrate` (runs inside `open`/`open_in_memory`),
`stale::mark`, `stale::clear_if_unchanged`, `stale::seg_match_key`,
the `stale::mark_*_stale` family (driven by the edit primitives),
`undo_stop` / `undo_station` / `undo_polyline_line` (dispatched via
`undo_edit`), `as_str` helpers.

## Read-model ownership

`rm_db` functions above are the *policy* layer (connections, ids,
batches, current-version reads, edit semantics, stale protocol).
Per-endpoint presentation SQL for rm-web lives in rm-web's own
`queries/` + `handlers/` against the connection rm_db handed over —
so new-migration read queries added there must probe `sqlite_master`
and degrade (see CLAUDE.md, `/raw` incident).
