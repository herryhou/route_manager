# rm-publish

Stage 3 of the pipeline: per-route atomic V1 publish. Takes the identity
rows minted by `rm-identity` plus the batch's staging rows and writes each
route's first immutable version snapshot — one database transaction per
route, so a bad route rolls back alone and cannot leave half-written data.

## Input → output

Reads `src_route_header` and `src_stop_edge` for the batch, and resolves
identities via `route.ext_uid` / `stop.ext_uid` (must already exist).
Writes, per route, in one `IMMEDIATE` transaction:

| Output | Meaning |
|---|---|
| `route_version` | version_no = max + 1, `status = 'PUBLISHED'`, batch `valid_from` |
| `subroute_version` | one per route version, name = header description |
| `stop_of_route_version` | stops per direction, renumbered 1..N, `source_order_no` kept |
| `route.current_route_version_id` | the pointer downstream stages read |
| `outbox_event` | one `route.published` JSON event per publish |

## How it runs

1. **Numeric xno order** — routes publish in `xno_key` order (same ordering
   discipline as `rm-identity`), so runs are deterministic.
2. **Skip check** — a route whose `current_route_version_id` is already set
   is skipped. This is the idempotency mechanism: rerunning the same batch
   publishes nothing and never mints a V2.
3. **Resolve before transact** — all `stop_version_id`s are looked up
   first; a missing identity (`no route identity`, `no stop_version`) or an
   empty staging set (`no stops`) fails the route before any write.
4. **One transaction per route** (`publish_tx`):
   - overlap guard: new `valid_from` must not start before the latest
     existing version's (spec §21);
   - `stop_sequence` is renumbered 1..N per direction from source order —
     sparse `orderNo` values like 5 collapse to 1, and a cyclic route
     visiting the same pole twice keeps both rows (sequences 1 and 2);
   - pointer update bumps `route.revision`;
   - the outbox event records route/version/xno/stop count for later
     consumers (V1 writes it; nothing reads it yet).

## Failure isolation

Per-route failures are collected (`failed`, `failures` list) and printed;
the remaining routes still publish. The binary exits non-zero if any route
failed, so scripts notice — but the successful routes stay committed.

## Tests

`tests/publish.rs` runs end-to-end over the shared `rm_ingest::testfix`
fixture: pointer + revision + outbox assertions, rerun-skips, a
header-without-stops route failing alone, cyclic same-stop-twice rows, and
`stop_sequence` contiguity per direction (spec §10).

## Why this stage exists

Everything downstream reads the published surface: `rm-osm-match` selects
`current_route_version_id` rows, `rm-segments` projects stops onto the
route path, `rm-registry` hashes segments over published versions. The
pointer + immutable-version pair is what will let a later batch mint a V2
without disturbing V1 readers.
