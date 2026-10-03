# rm-segments

Stage 4 of the pipeline: give every stop a position *along* its route.
Reads the per-direction polylines captured in `src_route_point`, projects
each published stop onto the decoded path — its chainage, metres from the
path start — and cuts a geometry draft between each consecutive stop pair.

## Input → output

Reads `route` (published rows only, i.e. with a `current_route_version_id`),
`subroute_version`, `stop_of_route_version` joined to `stop_version` for
coordinates, and the batch's `src_route_point`. Writes, in one transaction:

| Output | Meaning |
|---|---|
| `path_stop_projection` | one row per stop: chainage, snap distance, method, clamp flag, projected point |
| `seg_geometry_draft` | one row per consecutive stop pair: encoded sub-polyline, length, geometry method |

## Four projection methods

`path_stop_projection.method` says how each stop was placed:

- **PROJECTED** — snapped onto the path within `--outlier-m`.
- **OUTLIER** — snapped beyond the threshold. Still projected and still
  used downstream; the flag is advisory.
- **ENDPOINT** — the polyline degenerates to a single point. Chainage 0,
  projected onto that point, snap = distance to it.
- **NO_PATH** — no polyline for the direction (`''` in the capture).
  Chainage 0, snap 0, the stop keeps its own coordinates. A polyline that
  fails to decode is warned about and falls back the same way, but keeps
  the ENDPOINT label because an encoded string was present.

## How it runs

1. **Collect per route** — published routes ordered by `route_id`; the
   route number for the polyline lookup is the ext_uid minus its `tcg:`
   prefix. Stops come ordered by direction then `stop_sequence`.
2. **Decode the polyline** (`rm_core::polyline`; the decoder drops
   consecutive duplicate points). A path with fewer than two points takes
   the ENDPOINT / NO_PATH branch above.
3. **Project each stop** (`geometry::project_point`) — nearest
   point-to-segment projection over *all* path segments, never
   endpoint-only. Chainage = cumulative arc length up to that segment
   plus distance along it (`geometry::cumulative_m`, haversine);
   snap = perpendicular distance.
4. **Enforce monotonic chainage** (`geometry::clamp_monotonic`) — a
   running maximum over stop order. A stop that lands strictly behind its
   predecessor is pinned to the running max and flagged
   `monotonic_clamped` (equal chainages do not flag).
5. **Cut drafts** (`geometry::cut_subpath`) between consecutive stops'
   chainages: endpoints interpolated on the path, interior vertices kept
   when strictly inside, length = chainage difference. A zero-width cut
   (two stops at one chainage) is a degenerate two-point polyline of
   length 0. Without a usable path the draft is the straight line between
   the two stops' coordinates — `geometry_method` is `path` or `straight`
   (the 0003 migration comment says `path|no_path`; the code writes
   `straight`).
6. **Idempotent per route** — the route's `path_stop_projection` and
   `seg_geometry_draft` rows are deleted before re-inserting, and the
   whole run commits as a single transaction; rerunning the same batch
   reproduces identical rows (asserted by `rerun_is_idempotent`).

## CLI

| Flag | Default | Meaning |
|---|---|---|
| `--db` | `data/taichung.db` | SQLite database path |
| `--batch` | latest OPEN batch | whose polylines to project |
| `--outlier-m` | `50.0` | snap distance beyond which a stop is flagged OUTLIER |

Prints one summary line: directions, projections (no_path, endpoint,
outliers, clamped), draft rows.

## Why this stage exists

Chainage is the shared ruler of the pipeline's back half. `rm-osm-match`
decodes the same polylines and records each way run with cumulative
start/end metres along them; `rm-registry` walks consecutive stop pairs
by chainage, keeps the way runs whose cumulative range overlaps the pair,
and hashes them together with the two stations into `segment_id`. The
draft row supplies the segment's polyline, falling back to summed way
overlap when no draft exists. Without this stage, "which streets lie
between which two stops" would have no answer — and `rm-verify`'s outlier
CSV (snap > 50 m or method OUTLIER; 50 m is hardcoded there, matching
this crate's default) would have nothing to read.
