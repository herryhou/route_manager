# route_manger — Taichung bus pipeline

All-Rust pipeline turning raw Taichung ebus captures (392 routes) into one
versioned SQLite database plus a directed OSM-aligned segment registry,
implementing `docs/Architecture_design_spec.md` and `docs/DB_Schema_Spec.md`.

## Layout

```
crates/
├── rm-core         domain types, polyline codec, geo, blake3 helpers
├── rm-db           migrations, id allocator, import_batch
├── rm-ingest       raw JSON -> src_* staging tables
├── rm-identity     staging -> identities + Stop/Station/StationGroup V1
├── rm-publish      Route/SubRoute/StopOfRoute V1 atomic publish
├── rm-segments     polyline decode, stop projection (chainage), geometry cuts
├── rm-osm-prepare  PBF -> road graph tables
├── rm-osm-match    Viterbi way-run matcher (Newson-Krumm + A* gap repair)
├── rm-registry     directed segment registry (blake3 identity)
└── rm-verify       invariant checks, count cross-checks, QA exports
```

## Run

```sh
# 0. one-time: place captures under assets/Taichung/raw/ and the OSM extract
#    at assets/Taichung/data/taichung.osm.pbf (untracked; see .gitignore)

# 1. build
cargo build --release

# 2. pipeline (each step idempotent per batch; each prints a summary line)
cargo run -r -p rm-ingest        # raw -> staging, opens batch 1
cargo run -r -p rm-identity      # identities + V1 versions
cargo run -r -p rm-publish       # 392 routes, atomic per route
cargo run -r -p rm-segments      # projections + geometry drafts
cargo run -r -p rm-osm-prepare   # PBF -> osm_* tables
cargo run -r -p rm-osm-match     # way runs + match stats
cargo run -r -p rm-registry      # seg_segment / seg_segment_way / seg_traversal

# 3. verify + QA exports (exits non-zero on any failed check)
cargo run -r -p rm-verify
```

All bins accept `--db` (default `data/taichung.db`) and `--batch`
(default: latest OPEN batch). `rm-verify` writes `qa/report.json`,
`qa/segments.geojson`, `qa/outlier_stops.csv`.

## Data model notes

- Station identity = API `node.id` (`station.ext_uid = tcg:node:{id}`);
  stop identity = `(route, direction, sid)` (`tcg:{xno}:{dir}:{sid}`).
- Directed segments: `segment_id = blake3_128("tcg:node:{A} | tcg:node:{B} | w1|…|wn")`
  over station ext_uids and travel-order OSM way ids; the reverse corridor
  hashes differently (two ids per physical corridor).
- Shared-segment surface: `seg_traversal` grouped by `segment_id` with
  `COUNT(DISTINCT route_version_id) > 1`.

## Known limits (V1)

- The local OSM extract has coverage holes: ~200 of 761 directions match no
  way runs (surfaced as skipped pairs in the registry summary and
  `qa/report.json` stats). Fix is a larger extract, not code.
- Timetables/TRIP, GTFS export, re-import diffing (V2+ transitions) and the
  API server are out of scope; their tables exist but stay empty.

## Tests

```sh
cargo test --workspace
```
