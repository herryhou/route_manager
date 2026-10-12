# rm-registry — how the step works

STE explanation for new engineers. The step runs after `rm-osm-match` and
`rm-segments`. Its own help line describes it: station finalize — bearing
derivation, plus the dual-carriageway station split.

## The task

The step `rm-registry` gives every stop a travel-direction bearing, and then splits the stations that cover two or more travel headings. A station on a dual carriageway holds poles of both directions. The split gives each direction its own station.

## What must exist first

- `rm-osm-match` must fill `seg_match_way`. The OSM bearing source reads the way runs there.
- `rm-segments` must fill `path_stop_projection`. The bearing step reads the stop chainages there.
- `rm-identity` must have minted the stations. Every stop version already carries a `station_id`.

## Part 1 — the stop bearings

The function `bearing::run` writes the `stop_bearing` table:

1. Read every distinct direction from `path_stop_projection`.
2. Read the OSM nodes once.
3. For each direction, read the stops in `stop_sequence` order with their `chainage_m`, the way runs from `seg_match_way`, and the effective polyline (the override, or else the capture in `src_route_point`).
4. For each stop, compute the bearing, and write one row with its source value.

The bearing rules:

- `osm_bearing` finds the way run that covers the stop chainage, or the nearest run within 200.0 m (`RUN_GAP_MAX_M`). The bearing is the travel direction of that run. The row gets the source `OSM`.
- `polyline_bearing` takes the forward tangent of the direction polyline at the chainage. The row gets the source `POLYLINE`.
- A stop with no geometry gets no row. The step counts it in `stops_skipped`.

A cycled route can visit the same stop twice in one direction. The insert uses `OR REPLACE`, so the table holds one row per stop version, and the last visit wins.

## Part 2 — the station axis

The function `bearing::stations` rebuilds the `station_bearing` table from scratch. It groups the member bearings of each station, and calls `station_axis`:

- The default result is the circular mean, with the maximum spread from that mean.
- When the spread reaches 45.0° (`SIGNED_SPREAD_MAX_DEG`), the members disagree on the travel direction. The function then folds the result onto an axis without direction: the headings 0° and 180° share one axis.

The 45° threshold aligns with the `BEARING_SPREAD_WARN_DEG` check of `rm-verify`.

## Part 3 — the split

The function `split::run` walks the current stations:

1. Collect the members of the station with their bearings.
2. `bearing_clusters` sorts the headings on the circle, and cuts at every gap wider than 90.0° (`SPLIT_GAP_DEG`). A station whose members agree makes one cluster.
3. A station with fewer than two clusters stays as it is.
4. The parent station keeps the largest cluster. A tie keeps the smaller heading. The child stations follow in ascending heading order.
5. Each child gets an idempotent identity: the parent `ext_uid`, a colon, and the rounded cluster mean. An existing `ext_uid` reuses its station.
6. Each child copies the group id, the name, and the valid-from date of the parent. Its coordinates are the arithmetic mean of its member coordinates, so the child sits on its own carriageway.
7. The step moves the cluster members to the child with a guarded update: only stops that still point at the parent move. The guard keeps re-runs honest.
8. The parent gets the coordinates of its own kept cluster.

The split is idempotent: re-running with unchanged bearings moves nothing.

## The run order

The binary runs the parts in this order, because the split changes the station membership:

1. `bearing::run` — the per-stop bearings, then the station axes.
2. `split::run` — the heading-cluster split.
3. When the split moved stops: `bearing::stations` again — the moved stops changed the station groups.

## The --stale refresh

The function `run_stale` reacts to the edit flow:

1. Read the `stale_mark` rows for the table names `stop_bearing` and `station_bearing`.
2. No marks: print `nothing stale`, and touch nothing.
3. Any mark: rebuild the whole bearing tables, run the split, re-run the station axes when needed.
4. Clear exactly the snapshot marks. The clear matches the `rowid` and the `cause_edit_id` together, so a mark that an editor re-wrote during the run survives.

## How to run it

Build in release mode and obey the project rule about `-r`:

```bash
cargo run -r -p rm-registry -- --db data/taichung.db
```

Options: `--batch N` picks a batch (default: the latest), and `--stale` refreshes the marked bearings instead of a full run. The run prints one line for the bearings and one line for the split counts.

## Who uses the result

- `rm-verify` warns about stations with a wide bearing spread.
- The web UI shows the split stations in the station panel and the nearest-station lookup.
- The station split shapes what a station means for consumers: one station per travel heading, not one per road name.
