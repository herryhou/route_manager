# rm-registry

Stage 7 of the pipeline: station finalize. Takes the stop chainages from
`rm-segments` and the matched way runs from `rm-osm-match`, computes
stop and station bearings, then splits mis-clustered dual-carriageway
stations.

(The crate previously also built the cross-route segment registry —
`seg_segment` / `seg_segment_way` / `seg_traversal`, removed 2026-10-09
in migration 0011. The station pair is the identity; each route's
ordered way list derives from its pieces × way runs. History:
`docs/segment-handoff.md`.)

## Input → output

Reads `path_stop_projection` (chainages), `seg_match_way` (way runs
with cumulative metre ranges), the identity tables (`station`,
`stop_version`, `route_stop_version`, `subroute_version`/`route_version`),
and — for the bearing fallback — `src_route_point`, `osm_way_node`,
`osm_node`. Writes, in one transaction each pass:

| Output | Meaning |
|---|---|
| `stop_bearing` | departure heading per stop version, plus its `source` |
| `station_bearing` | station heading or axis + `max_spread_deg` |
| `station` / `stop_version` (splits) | per-side child stations for dual-carriageway poles |

Idempotency is delete-and-rewrite for the bearing tables: each run
wipes and rebuilds them. Rerunning on the same batch reproduces
identical rows — unlike `rm-identity`'s lookup-first minting, nothing is
carried forward, so a changed match upstream simply produces new
bearings. The split pass is separately idempotent: with unchanged
bearings it moves nothing.

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
station bearings are rebuilt from the per-side members.

## How it runs

1. **Bearing pass** (`src/bearing.rs`) — per direction, resolve each
   stop's heading (OSM, else polyline, else skip), then aggregate
   stations in one sweep over `stop_bearing`.
2. **Split pass** (`src/split.rs`) — split dual-carriageway stations as
   above; if any stop moved, rebuild `station_bearing`.
3. **Commit** — each pass runs inside one transaction; the summary
   lines print stops by source and stations, then split counts.

CLI: `--db` (default `data/taichung.db`) and `--batch` (default: latest
OPEN batch; the run errors if none is open and no `--batch` is passed).

## Why this stage exists

`rm-identity` made "same pole" expressible; this stage adds the
direction of travel — which way the corridor runs through each pole,
and how consistently a station's routes agree on it — and corrects the
poles that grouping put across both sides of a divided road.
