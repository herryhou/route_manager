# route_manger — Taiwan bus pipeline

All-Rust pipeline turning raw city ebus captures (Taichung 392, Taipei 406,
Tainan 113 routes) into one versioned SQLite database per city with
OSM-aligned route geometry and station bearings, implementing
`docs/Architecture_design_spec.md` and `docs/DB_Schema_Spec.md`.

## Layout

```
crates/
├── rm-core         domain types, polyline codec, geo, blake3 helpers
├── rm-db           migrations, id allocator, import_batch
├── rm-ingest       raw JSON -> src_* staging tables
├── rm-identity     staging -> identities + Stop/Station/StationGroup V1
├── rm-publish      Route/SubRoute/RouteStop V1 atomic publish
├── rm-segments     polyline decode, stop projection (chainage), geometry cuts
├── rm-osm-prepare  PBF -> road graph tables
├── rm-osm-match    Viterbi way-run matcher (Newson-Krumm + A* gap repair)
├── rm-registry     stop/station bearings + dual-carriageway station split
└── rm-verify       invariant checks, count cross-checks, QA exports
```

## Run

```sh
# 0. one-time: place captures under assets/Taichung/raw/ and a Taiwan-wide
#    OSM extract under assets/ (e.g. taiwan-261004.osm.pbf; untracked, see
#    .gitignore), then pass it via --pbf to rm-osm-prepare, with
#    --clip "south,west,north,east" (route bbox + ~10 km) to keep the DB
#    compact — clipped matching is bit-for-bit identical to unclipped

# 1. build
cargo build --release

# 2. pipeline (each step idempotent per batch; each prints a summary line)
cargo run -r -p rm-ingest        # raw -> staging, opens batch 1
cargo run -r -p rm-identity      # identities + V1 versions
cargo run -r -p rm-publish       # 392 routes, atomic per route
cargo run -r -p rm-segments      # projections + geometry drafts
cargo run -r -p rm-osm-prepare \ # PBF -> osm_* tables
    -- --pbf assets/taiwan-261004.osm.pbf --clip "23.89,120.41,24.53,121.42"
cargo run -r -p rm-osm-match     # way runs + match stats
cargo run -r -p rm-registry      # stop_bearing / station_bearing +
                                  #   dual-carriageway station split
cargo run -r -p rm-segments -- --stale  # reproject routes marked stale by web edits
cargo run -r -p rm-registry -- --stale  # rebuild bearings + split for marked-stale, clear marks

# 3. verify + QA exports (exits non-zero on any failed check)
cargo run -r -p rm-verify -- --preset taichung   # | taipei | tainan
```

All bins accept `--db` (default `data/taichung.db`) and `--batch`
(default: latest batch, any status; `rm-identity`: latest OPEN).
`rm-verify` writes `qa/report.json`,
`qa/outlier_stops.csv`, `qa/bearing_report.json`,
`qa/match_dev_report.json`, `qa/pair_spread_report.json`, and
`qa/grouping_report.json`.

## Bearings

`stop_bearing` is the departure heading (degrees from north) of the bus at
each stop: taken from the matched OSM way run when one covers the stop,
else the route polyline tangent; the final stop of a direction reports its
arrival heading. `station_bearing` is the mean travel **axis** of a
station's member stops (headings 180° apart fold onto one axis — one pole
legitimately serves both directions of a road) plus `max_spread_deg`, the
largest deviation from that axis. A station whose members sit more than
45° off its axis is flagged — warn-only (`report.json` warnings +
`qa/bearing_report.json`), never failing the run.

To browse the result in a map-centric web UI (read-only, safe alongside the
pipeline):

```sh
cargo run -r -p rm-web -- --db data/taichung.db --addr 127.0.0.1:8787
```

See `docs/pipeline_guide.md` § *Browsing the result: rm-web* for what it shows.

## Data model notes

- Station identity = API `node.id` (`station.ext_uid = tcg:node:{id}`);
  stop identity = `(route, direction, sid)` (`tcg:{xno}:{dir}:{sid}`).
- Stop-pair way lists: for any pair of consecutive stops, the ordered OSM
  way list is the `seg_match_way` runs intersecting the
  `path_stop_projection` chainage window `[chainage(from), chainage(to)]` —
  derived per route, no registry needed. (The cross-route
  `seg_segment`/`seg_segment_way`/`seg_traversal` registry was removed on
  2026-10-09, migration 0011 — see `docs/segment-handoff.md`.)

## Known limits (V1)

- 17 of 784 directions have no drawn polyline in the source data at all, so
  they match no way runs (surfaced as skipped pairs in `qa/report.json`
  stats); every direction with a polyline matches on the
  full-Taiwan extract (clipped to the routes' bbox + 10 km). 1 of the 767
  matched directions (352延) still strays > 50 m p95 from its polyline —
  warn-only, in `qa/match_dev_report.json` and the rm-web matchdev chip.
- Timetables/TRIP, GTFS export, re-import diffing (V2+ transitions) and the
  API server are out of scope; their tables exist but stay empty.

## Tests

```sh
cargo test --workspace
```
