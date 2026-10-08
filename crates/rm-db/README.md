# rm-db

The database layer the rest of the pipeline stands on: owns the schema,
the id allocator, and the import-batch bookkeeping for the one SQLite
file every stage shares. Runs no pipeline step itself — each stage
binary and `rm-web` open the DB through it.

## What it exports

| Function | Role |
|---|---|
| `open` | open (create if needed) the pipeline DB: pragmas + migrations applied |
| `open_readonly` | viewer mode — no pragmas that write, never migrates |
| `open_in_memory` | same pragmas + migrations; the test fixture across the workspace |
| `migrate` | apply pending migrations (see below) |
| `mint` | next id in a table's id space |
| `create_batch` / `set_batch_status` / `latest_open_batch` | import-batch lifecycle |
| `now_rfc3339` | RFC3339 UTC string for `created_at` / `valid_from` columns |

Write convention (crate doc): published-table writes are INSERT-only;
UPDATE is reserved for pointers/revision inside a publish transaction.

## Migrations

Nine SQL files under `migrations/`, embedded into the binary with
`include_str!` — no external migration tool, no migration command to
forget. `migrate` creates `schema_migrations` if missing, applies each
unrecorded file in order as one batch, and records version, name, and
`applied_at`. Recorded versions are skipped, so calling it repeatedly
is safe — and since it runs inside every `open` / `open_in_memory`,
any binary opening the DB brings the schema along. `open_readonly`
deliberately never migrates.

| Migration | Creates |
|---|---|
| `0001_init` | the versioned model per `docs/DB_Schema_Spec.md`: identity tables (`route`, `subroute`, `stop`, `station`, `station_group`), their `*_version` tables, `route_stop_version`, empty-by-design trip/report/outbox tables, indexes — plus pipeline tables: `id_sequence`, `import_batch`, `src_file` audit, batch-scoped `src_*` staging (no FKs by design), `route_link`, `route_provider` |
| `0002_identity` | `ext_uid` + unique index on `stop` and `station_group`; group-provenance `source` column on `station_group_version` |
| `0003_segment` | `path_stop_projection`, `seg_geometry_draft`, `seg_match_way`, `seg_match_stat`, and the registry: `seg_segment` / `seg_segment_way` / `seg_traversal` |
| `0004_osm` | road graph from the PBF extract: `osm_meta`, `osm_way`, `osm_way_node`, `osm_node` |
| `0005_bearing` | `stop_bearing`, `station_bearing` — derived data rebuilt per run, never stored inside the immutable `*_version` tables |
| `0006_match_dev` | path→polyline deviation percentiles (`dev_p50_m`, `dev_p95_m`, `dev_max_m`) on `seg_match_stat` |
| `0007_current_views` | the `*_current` read views (see below) |
| `0008_pair_only` | segment identity becomes the directed station pair: `variant_count` + `length_spread_m` on `seg_segment`, `way_sig` on `seg_traversal` (per-route path variants as data) |
| `0009_corridor_split` | one segment per corridor within a station pair: `pair_key` + `corridor_key` on `seg_segment` |

Connections from `open` set `foreign_keys = ON`, WAL,
`synchronous = NORMAL`, and a 30 s busy timeout (WAL is a no-op on
`:memory:`).

## Current-version views

Reading the versioned model means joining: every stop/station/group
lookup filters `valid_to IS NULL`, every route lookup follows
`current_route_version_id`, and route stops need the four-table
`route_stop_version → subroute_version → route_version → route` chain.
Migration `0007_current_views` defines one view per concept so
"current data" reads like a plain table:

| View | Current rows from |
|---|---|
| `stop_current` | `stop_version` with `valid_to IS NULL` |
| `station_current` | `station_version` with `valid_to IS NULL` |
| `station_group_current` | `station_group_version` with `valid_to IS NULL` |
| `route_current` | `route` joined to its `current_route_version_id` version |
| `route_stop_current` | the route→version→subroute→stop chain, current version only |

```sql
-- ordered stops of a route's current version, with names
SELECT stop_sequence, stop_name
FROM route_stop_current rsc
JOIN stop_current sc ON sc.stop_version_id = rsc.stop_version_id
WHERE rsc.route_id = 1 AND rsc.direction = 0
ORDER BY stop_sequence;
```

The views rely on the pipeline's at-most-one-open-version invariant and
are read-only convenience — writers keep using the base tables, and
existing queries were not rewritten to use them.

## ID minting

`mint(conn, table)` is how published rows get ids: one id space per
table name, seeded at 1 on first use. It is a single
`UPDATE id_sequence ... RETURNING`, so it must run inside the caller's
transaction — the row lock serializes concurrent minters; a missing
row is seeded with `INSERT OR IGNORE` and the update retried. Ids are
never reused (schema-spec invariant I02: stop/station/group ids).
`rm-identity` mints route, subroute, station, station-group, and
version ids this way; `rm-publish` mints `route_version`,
`subroute_version`, `route_stop_version`, and `outbox_event`.

## Import batches

`create_batch` registers a row in `import_batch` (status `OPEN`, its id
minted like any other) carrying `source_dir`, `file_count`,
`captured_at`, and `valid_from` — the validity start later stamped onto
the versions the batch produces. `rm-ingest` reuses an OPEN batch for
the same `source_dir`, writes the per-file sha256 audit into `src_file`,
and marks the batch `FAILED` on error. `set_batch_status` only moves
the flag. `latest_open_batch` answers "most recent OPEN batch", which
is the default `--batch` target for every stage binary.

## Why this crate exists

The schema is the contract between stages: `rm-ingest` writes staging
tables `rm-identity` reads, `rm-registry` writes `seg_*` tables
`rm-web` renders. Keeping migrations, id allocation, and batch state in
one crate means every binary carries the same schema, ids stay unique
and never reused across stages, and every tool agrees on "which batch
are we working on" — while `rm-web` opens the same file read-only
(a WAL reader beside the pipeline writer) and never mutates a thing.
