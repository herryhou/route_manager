# rm-registry

Stage 7 of the pipeline: the payoff. Takes the stop chainages from
`rm-segments` and the matched way runs from `rm-osm-match`, cuts each
direction into consecutive-stop pairs, and gives every pair a content
address — so two routes running the same corridor land on the same row.
The same run then computes stop and station bearings.

## Input → output

Reads `path_stop_projection` (chainages), `seg_match_way` (way runs
with cumulative metre ranges), `seg_geometry_draft` (per-pair geometry),
the identity tables (`station`, `stop_version`, `stop_of_route_version`,
`subroute_version`/`route_version`), and — for the bearing fallback —
`osm_way_node`, `osm_node`, `src_route_point`. Writes, in one
transaction:

| Output | Meaning |
|---|---|
| `seg_segment` | one directed corridor: id, endpoints, polyline, length, way count |
| `seg_segment_way` | a segment's OSM ways in travel order, with overlap lengths |
| `seg_traversal` | one route direction visiting one segment |
| `stop_bearing` | departure heading per stop version, plus its `source` |
| `station_bearing` | axial mean heading per station + `max_spread_deg` |

Idempotency is delete-and-rewrite: the run wipes the `seg_*` tables (and
both bearing tables) first, then rebuilds them from scratch. Rerunning
on the same batch reproduces identical rows — unlike `rm-identity`'s
lookup-first minting, nothing is carried forward, so a changed match
upstream simply produces a new registry.

## Segment identity

`segment_id = blake3_128("tcg:node:{A} | tcg:node:{B} | w1|…|wn")`
(`segment_id` in `src/hash.rs`) over the two station ext_uids and the
OSM way ids between them in travel order. Same stations + same ways in
the same order = same id, whichever route you came from. The reverse
corridor hashes differently — stations swapped, ways reversed — so one
physical road is two directed segments.

The way list is the covering set, not the whole run: a way belongs to
the pair when its cumulative range overlaps the pair's chainage
interval (overlap over 1e-6 m), and `seg_segment_way.length_m` records
that overlap, not the way's full length. A pair no way covers is
counted as a skipped pair and never registered — zero-way segments
don't exist. Geometry comes from `seg_geometry_draft` when one exists
for the pair; a missing draft (e.g. NO_PATH) stores an empty polyline
with the summed way overlap as length.

The shared-segment surface is `seg_traversal` grouped by `segment_id`
with `COUNT(DISTINCT route_version_id) > 1`. Known outputs on the
current batch: 19,612 traversals, 6,111 unique segments, 3,326 shared
by more than one route.

## Bearings

`stop_bearing` is the departure heading (degrees clockwise from north)
of the bus at each stop, with a `source` column saying where it came
from:

- **OSM** — the way run covering the stop's chainage; between runs or
  past the ends, the nearest run, so the final stop of a direction
  reports its arrival heading on the last way. The bearing is the
  heading of the run's entry→exit sub-path segment nearest the pole,
  walked in travel order — the same way matched southbound reports
  180°, regardless of how the way was drawn.
- **POLYLINE** — no run resolves: the forward tangent of the
  direction's decoded `src_route_point` polyline at that chainage; at
  or past the end, the final leg.
- neither → skipped, counted in the summary line.

A cycled route can visit the same stop twice in one direction; the
insert is `OR REPLACE` — one bearing row per stop version, last visit
wins.

`station_bearing` aggregates member stops onto the travel *axis*:
`bearing_deg` is the axial mean (`rm_core::geo::axial_mean_deg`, the
doubled-angle mean in [0, 180) — headings 180° apart fold onto one
axis, because one pole legitimately serves both directions of a road)
and `max_spread_deg` the largest member deviation from that axis,
measured modulo the reverse direction. Route `direction` is
route-relative — two routes' 去程 at one pole can head opposite ways —
so it is deliberately not used for grouping; the axis is. The 45°
warn-only threshold lives downstream in `rm-verify`, which flags
stations with `max_spread_deg` over 45° into `report.json` warnings and
`qa/bearing_report.json` without failing the run.

## How it runs

1. **Enumerate directions** — every `(route_version, subroute_version,
   direction)` with stop projections (`subroute_version` is 1:1 with
   `route_version` in this pipeline), ordered deterministically.
2. **Registry pass** (`src/registry.rs`) — per direction, load stops by
   `stop_sequence` and runs by ordinal, walk consecutive pairs, compute
   the covering ways and the hash, insert segment + ways + traversal.
3. **Bearing pass** (`src/bearing.rs`) — per direction, resolve each
   stop's heading (OSM, else polyline, else skip), then aggregate
   stations in one sweep over `stop_bearing`.
4. **Commit** — both passes run inside one transaction each; the
   summary lines print directions / traversals / segments / skipped
   pairs, then stops by source and stations.

CLI: `--db` (default `data/taichung.db`) and `--batch` (default: latest
OPEN batch; the run errors if none is open and no `--batch` is passed).
Directions the matcher couldn't match show up as skipped pairs here and
in `qa/report.json` — the current extract leaves 204 of 761 directions
unmatched (coverage holes, not code).

## Why this stage exists

`rm-identity` made "same pole" expressible; this stage makes "same
corridor" expressible. The content-addressed `segment_id` is what turns
per-route stop sequences into a shared network: `rm-verify` cross-checks
it, `rm-web` colors canonical corridors by how many routes share them,
and anything built on shared-corridor arrival times starts from
`seg_traversal`. Bearings add the direction of travel to that picture —
which way the corridor runs, and how consistently a station's routes
agree on it.
