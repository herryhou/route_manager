# rm-segments — how the step works

STE explanation for new engineers. Name note: the crate and binary are
`rm-segments` (not rm-osm-segment). It runs after `rm-osm-match`.

## The task

The step `rm-segments` projects every stop of every published route direction onto the effective direction polyline. It then cuts the geometry between each pair of consecutive stops. Its own help line says it well: project stops onto direction polylines, and cut per-stop-pair geometry.

## What must exist first

- `rm-ingest` must have loaded a batch. Without a batch, the run stops with the message `no import batch; run rm-ingest first`.
- `rm-osm-match` must have filled `seg_match_way`. The downstream way lists need it.
- The routes must be published. The step reads only routes with a `current_route_version_id`.

## The effective polyline

For each direction the step resolves one polyline: the override polyline, or else the captured polyline in `src_route_point`. The helper `rm_db::effective_encoded` does this. The step uses the same effective line as `rm-osm-match` (the phase 2 rule).

## The per-route work

The function `run_inner` walks the published routes of the batch:

1. Read the stops of the route, ordered by direction and then `stop_sequence`.
2. Decode the effective polyline of each direction.
3. Delete the old output of this route: the `path_stop_projection` rows and the `seg_geometry_draft` rows. The rewrite makes the step idempotent.
4. Process each direction group.

## The projection

The function `projection_group` places each stop on the polyline:

- Collect the projection candidates of each stop within a small slack (`CAND_SLACK_M`).
- `select_sequence` picks one candidate per stop in stop order. A loop closure picks the side that the stop order implies.
- `clamp_monotonic` makes the chainages increase along the direction. A clamped stop gets the flag `monotonic_clamped`.
- A stop with `snap_dist_m` over the outlier limit gets the method `OUTLIER`. The default limit is 50.0 m (`--outlier-m`).
- A normal stop gets the method `PROJECTED`.

The step writes one `path_stop_projection` row per stop: the chainage, the snap distance, the method, the clamped flag, and the projected position.

### The fallbacks

A direction without a usable polyline gets no real projection:

- No polyline at all: every stop gets the method `NO_PATH`, chainage 0, and the first path point as its position.
- A polyline too short to project: every stop gets the method `ENDPOINT`, with the same rules.

## The draft cuts

For each pair of consecutive stops in a direction, the step writes one `seg_geometry_draft` row:

- With a path: `cut_subpath` cuts the polyline between the two chainages. The cut interpolates the two endpoints. The length is the difference of the chainages. The method is `path`.
- Without a path: the draft is the straight line between the two poles, and the length is the haversine distance. The method is `straight`.

Each row holds the direction, the ordinal (1-based), the two `route_stop_version_id` values, the encoded sub-polyline, the length, and the geometry method.

## The removed segment registry

The old step also built a segment registry: one `seg_segment` per station pair, with an identity hash and corridor splits. Migration `0011_drop_segment.sql` (2026-10-09) removed the `seg_segment`, `seg_segment_way`, and `seg_traversal` tables. The piece-level data stayed: `path_stop_projection`, `seg_geometry_draft`, `seg_match_way`, and `seg_match_stat`. Today the system derives the station-pair way lists by intersecting the stop chainage window with the `seg_match_way` runs. The design history lives in `docs/segment-corridor-split.md`; its length-cluster idea (gap over max(100 m, 3% of the shorter side)) now lives on as the `pair:spread` check of `rm-verify`.

## The --stale refresh

The function `run_stale` re-processes only the marked routes:

1. Read the `stale_mark` rows with the table name `path_stop_projection`. Each key is one route id.
2. Keep the mark when the key is unparseable or the route is not published.
3. Re-run the normal per-route work for exactly the other marked routes.
4. Delete exactly the snapshot marks. The delete matches the `rowid` and the `cause_edit_id` together, so a mark that an editor re-wrote during the run survives.

## How to run it

Build in release mode and obey the project rule about `-r`:

```bash
cargo run -r -p rm-segments -- --db data/taichung.db
```

Options: `--batch N` picks a batch (default: the latest), `--outlier-m` sets the outlier limit (default 50.0), and `--stale` refreshes only the marked routes. The run prints one summary line with the direction, projection, `no_path`, `endpoint`, outlier, clamped, and draft-row counts.

## Who uses the result

- The bearing step reads `path_stop_projection` for the stop chainages.
- The web stops endpoint shows the chainage, snap distance, method, and clamped flag of each stop.
- The QA endpoint `/api/qa/outliers` lists the stops with a large snap distance.
- `rm-verify` checks the draft lengths and the pair spreads.
