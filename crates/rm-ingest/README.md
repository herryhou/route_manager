# rm-ingest

Stage 1 of the pipeline: copy the raw Taichung ebus JSON captures into
`src_*` staging tables — one row per file record, nothing interpreted —
under one import batch. Every later stage reads these tables keyed by
`batch_id` instead of touching the captures again.

## Input → output

Reads the capture tree under `--source` (serde shapes mirror
`assets/Taichung/Taichung_API.md`): `routes.json` (route headers),
`providers.json` (operator directory), `raw/route_{xno}.json` (stop edges
and polylines per route), `main-side-routes.json`, and — only if the file
exists — `jointOperationsBuses.json`.

Writes, in one transaction:

| Output | Meaning |
|---|---|
| `src_route_header` | one row per route: xno, route_code, departure/destination, description, seq, is_cycled, op_type |
| `src_route_provider` | route → provider code |
| `src_stop_edge` | one row per station edge: direction, source_order_no, ivr_no (nullable), sid, node_id, station_name (surrounding whitespace trimmed — grouping compares trimmed names), lat/lon as doubles plus rounded ×1e6 ints |
| `src_route_point` | encoded polyline per direction — always two rows per route; a missing side stores `''` |
| `src_provider` | one row per operator: code, name, telephone, op_type |
| `src_main_side_route` | main ↔ side xno pairs, stored as-is even when dangling |
| `src_joint_operation` | joint-operation rows |
| `src_file` | per-file audit: kind, name, sha256, row_count |

Direction comes from the raw `goBack` field: 1 → 0 (去程), 2 → 1 (返程);
any other value aborts the run, as does a raw file whose `data.route` is
null. Provider, main-side, and joint rows use `INSERT OR IGNORE`, so
repeated codes within one file collapse. Side-route lists prefer the
`children` array and fall back to the `sideRoutes` CSV string; `xno`
values arrive as JSON numbers or strings and are normalized to text.

## How it runs

1. **Parse everything first** (`src/parse.rs`) — every file is read and
   deserialized before any DB write. Headers and raw files are processed
   in numeric-xno order (`xno_sort_key`: "2" before "10"); main-side
   pairs and warnings are sorted and deduped, so two runs see the data
   in the same order.
2. **Open or reuse the batch** — the newest `OPEN` `import_batch` whose
   `source_dir` equals the `--source` path is reused; otherwise
   `rm_db::create_batch` mints one: `status = 'OPEN'`, the run time
   recorded as both `captured_at` and `valid_from`, the audit list's
   length as `file_count`. That `valid_from` is the one `rm-identity`
   reads from the batch.
3. **Wipe, then insert** — existing rows for this batch are deleted from
   all eight `src_*` tables, everything is re-inserted, and the single
   transaction commits. A crash mid-run cannot leave half a batch
   behind.

## Re-runs and failures

- **Same source dir** — the same OPEN batch is reused and its staging
  replaced; row counts do not grow. That is the idempotency mechanism:
  run `rm-ingest` twice, nothing duplicates.
- **Parse error** — nothing is written (parsing precedes the
  transaction), and a pre-existing reused batch is flipped to `FAILED`;
  the last good run's rows survive untouched.
- **Different source dir** — a fresh batch is minted and both batches'
  rows coexist, keyed apart by `batch_id`.

Warnings never fail the run; they are sorted, deduped, and printed after
the summary line:

- route header without a matching raw detail, and vice versa;
- a main-side reference to an xno absent from `routes.json` (stored
  anyway);
- provider sets differing between `routes.json` and the raw route file.

## CLI

```sh
cargo run -r -p rm-ingest
```

| Flag | Default | Meaning |
|---|---|---|
| `--db` | `data/taichung.db` | SQLite path; parent dirs created, DB opened with pragmas + migrations |
| `--source` | `assets/Taichung` | capture root to ingest; also the batch's `source_dir` |

There is no `--batch` here — unlike every later stage, `rm-ingest` is the
tool that *creates* batches, and which batch it writes is decided by the
source dir (reuse-or-mint above). Later tools default to the latest OPEN
batch. The binary prints one summary line (batch id plus routes,
stop_edges, route_points, providers, route_providers, main_side_links,
joint_operations) and then any warnings. The `testfix` cargo feature
exposes `src/testfix.rs`, a capture-fixture builder shared with other
crates' tests.

## Why this stage exists

Everything downstream reads `src_*`, never the captures: `rm-identity`
mints identities from `src_stop_edge` and friends, `rm-segments` decodes
`src_route_point` polylines, `rm-osm-match` joins `src_route_point` for
its match jobs. Staging keeps the API's answer verbatim and queryable,
and `src_file` pins its provenance — kind, name, row count, and sha256
per file — so "which exact files made this batch" is answerable from the
DB itself. Batch-keyed staging is also what makes a future second import
a comparable new batch rather than a destructive overwrite.
