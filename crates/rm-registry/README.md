# rm-registry

Stage 7 of the pipeline: the payoff. Takes the stop chainages from
`rm-segments` and the matched way runs from `rm-osm-match`, computes
stop and station bearings, splits mis-clustered dual-carriageway
stations, then cuts each direction into consecutive-stop pairs and gives
every directed station pair a content address — so two routes running
the same corridor land on the same row.

## Input → output

Reads `path_stop_projection` (chainages), `seg_match_way` (way runs
with cumulative metre ranges), `seg_geometry_draft` (per-pair geometry),
the identity tables (`station`, `stop_version`, `route_stop_version`,
`subroute_version`/`route_version`), and — for the bearing fallback —
`osm_way_node`, `osm_node`, `src_route_point`. Writes, in one
transaction each pass:

| Output | Meaning |
|---|---|
| `stop_bearing` | departure heading per stop version, plus its `source` |
| `station_bearing` | station heading or axis + `max_spread_deg` |
| `station` / `stop_version` (splits) | per-side child stations for dual-carriageway poles |
| `seg_segment` | one directed corridor: id, pair/corridor keys, endpoints, polyline, length, way count, variant count, length spread |
| `seg_segment_way` | a segment's OSM ways in travel order, with overlap lengths |
| `seg_traversal` | one route direction visiting one segment, with its `way_sig` |

Idempotency is delete-and-rewrite for the `seg_*` and bearing tables:
each run wipes and rebuilds them. Rerunning on the same batch reproduces
identical rows — unlike `rm-identity`'s lookup-first minting, nothing is
carried forward, so a changed match upstream simply produces a new
registry. The split pass is separately idempotent: with unchanged
bearings it moves nothing.

## Segment identity

Identity is the directed station pair:
`pair_key = blake3_128("tcg:node:{A} | tcg:node:{B}")` (`src/hash.rs`).
The reverse corridor hashes differently — stations swapped — so one
physical road is two directed pair keys.

A pair's cuts (one per traversing route direction) are clustered by
draft length into **corridors**: sorted ascending, a new cluster starts
where the gap to the previous length exceeds
`max(CORRIDOR_GAP_ABS_M 100 m, 3% × the shorter adjacent length)` — so
sub-100 m jitter merges while a dual carriageway vs its service road
splits. Each cluster becomes its own segment with
`segment_id = blake3_128("tcg:node:{A} | tcg:node:{B} | {corridor_key}")`,
where `corridor_key` is the cluster's lower-median length quantized
down to 10 m — a length bucket, deliberately carrying no way ids, so
OSM way splits/renames cannot drift identity. The segment stores
`pair_key` + `corridor_key` (QA grouping), `variant_count` (distinct
travel-order way lists seen in this corridor) and `length_spread_m`
(min..max cut length); each traversal keeps its own `way_sig`, so
per-route path variants stay queryable as data instead of fragmenting
identity.

Cut windows are normalized before slicing so routes covering the same
road hash the same corridor: each boundary snaps *down* to the latest
way-run junction at or below it (within `SNAP_EPS_M` 20 m, never
forward — adjacent segments then tile at shared junctions), and
boundary-way overlaps under `SLIVER_M` 6 m are dropped as matching
noise. A degenerate snap (lo ≥ hi, dense stops) keeps the raw
projections. The way list is the covering set, not the whole run:
`seg_segment_way.length_m` records the overlap, not the way's full
length, and consecutive repeats merge. The stored segment geometry is
the representative variant — the most-traveled way list in the cluster,
ties to the earliest-processed route — with its draft polyline; a
missing draft (e.g. NO_PATH) stores an empty polyline with the summed
way overlap as length. A pair no way covers is a skipped pair, never
registered — zero-way segments don't exist.

The shared-segment surface is `seg_traversal` grouped by `segment_id`
with `COUNT(DISTINCT route_version_id) > 1`. Current outputs —
Taichung: 36,531 traversals, 9,954 segments, 6,040 shared, 22 pairs
split into multiple corridors; Taipei: 27,381 / 7,317 / 4,307 / 25;
Tainan: 8,330 / 5,019 / 1,770 / 18.

## Bearings

`stop_bearing` is the departure heading (degrees clockwise from north)
of the bus at each stop, with a `source` column saying where it came
from:

- **OSM** — the way run covering the stop's chainage; between runs or
  past the ends, the nearest run, so the final stop of a direction
  reports its arrival heading on the last way. A stop further than
  `RUN_GAP_MAX_M` (200 m) from every run has none — past the matched
  coverage the nearest run describes a different road entirely (e.g. a
  tail running past the city extract's edge), and the fallback takes
  over. The bearing is the heading of the run's entry→exit sub-path
  segment nearest the pole, walked in travel order — the same way
  matched southbound reports 180°, regardless of how the way was drawn.
- **POLYLINE** — no run resolves: the forward tangent of the
  direction's decoded `src_route_point` polyline at that chainage; at
  or past the end, the final leg.
- neither → skipped, counted in the summary line.

A cycled route can visit the same stop twice in one direction; the
insert is `OR REPLACE` — one bearing row per stop version, last visit
wins.

`station_bearing` aggregates member stops (`station_axis` in
`src/bearing.rs`): members that all sit within 45° of their circular
mean keep that **signed** heading — a one-way roadside pole departs one
way only, and folding it onto an axis would hide which way. Everything
else takes the **axis**: the axial mean (`rm_core::geo::axial_mean_deg`,
doubled-angle mean in [0, 180) — headings 180° apart fold onto one
axis, because one pole legitimately serves both directions of a road)
with `max_spread_deg` the largest member deviation modulo the reverse
direction. Near-antipodal pairs must take this branch — their circular
mean is degenerate and lands perpendicular to the true axis — so the
branch threshold matches the warn threshold. Route `direction` is
route-relative — two routes' 去程 at one pole can head opposite ways —
so it is deliberately not used for grouping; the spread is. The 45°
warn-only threshold lives downstream in `rm-verify`, which flags
stations with `max_spread_deg` over 45° into `report.json` warnings and
`qa/bearing_report.json` without failing the run.

## Station split

`src/split.rs` runs after the first bearing pass: a current station
whose *beared* member stops form 2+ heading clusters — circular gaps
over `SPLIT_GAP_DEG` (90°) between sorted headings — is split into
per-side stations, because grouping put both carriageways of a divided
road under one pole. The biggest cluster (ties to the lower heading)
keeps the parent station, with its coordinates moved to its own
cluster's member mean; each other cluster mints a child station
(`ext_uid = {parent}:{rounded cluster mean}`, its V1 version inheriting
the parent's name, group, and `valid_from`) at its member mean and
re-points the member `stop_version.station_id`s directly — the update
is guarded on the old station so re-runs count nothing twice. Members
without bearings never move. The pass is idempotent — with unchanged
bearings, re-running moves nothing — and when it does move stops,
station bearings are rebuilt from the per-side members before the
registry hashes segments over the new station identity.

## How it runs

1. **Bearing pass** (`src/bearing.rs`) — per direction, resolve each
   stop's heading (OSM, else polyline, else skip), then aggregate
   stations in one sweep over `stop_bearing`.
2. **Split pass** (`src/split.rs`) — split dual-carriageway stations as
   above; if any stop moved, rebuild `station_bearing`.
3. **Registry pass** (`src/registry.rs`) — per direction, load stops by
   `stop_sequence` and runs by ordinal, walk consecutive pairs, snap
   the cut windows, collect one cut per traversal; then group cuts by
   pair key, length-cluster each group into corridors, and write
   segment + ways + traversals with the representative variant.
4. **Commit** — each pass runs inside one transaction; the summary
   lines print stops by source and stations, then split counts, then
   directions / traversals / segments / skipped pairs.

CLI: `--db` (default `data/taichung.db`) and `--batch` (default: latest
OPEN batch; the run errors if none is open and no `--batch` is passed).
Directions the matcher couldn't match show up as skipped pairs here and
in `qa/report.json` — 17 of 784 directions have no drawn polyline at
all and stay unmatched (source data, not code).

## Why this stage exists

`rm-identity` made "same pole" expressible; this stage makes "same
corridor" expressible. The content-addressed `segment_id` is what turns
per-route stop sequences into a shared network: `rm-verify` cross-checks
it, `rm-web` colors canonical corridors by how many routes share them,
and anything built on shared-corridor arrival times starts from
`seg_traversal`. Bearings add the direction of travel to that picture —
which way the corridor runs, and how consistently a station's routes
agree on it.
