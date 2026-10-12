# rm-osm-match — how the step works

STE explanation for new engineers: the step-level view. The Viterbi matching
core has its own section at the end.

## The task

The step `rm-osm-match` puts the polyline of each route direction onto the OSM road network. The result is a list of way runs per direction. Other steps use these runs as the road geometry of the route.

## What must exist first

- `rm-osm-prepare` must fill the `osm_node` and `osm_way` tables. The graph build stops with an error when `osm_node` is empty.
- The routes must be published. A job exists only for a direction with a `current_route_version_id`.

## The jobs

The function `collect_jobs` makes one `Job` per published direction of the batch. Each job carries the route version id, the subroute version id, the direction number, the route `xno`, the stop count, and an encoded polyline. The effective polyline of a job is the override polyline, or else the captured polyline in `src_route_point`. The step matches the line that consumers see, not the raw capture (the phase 2 rule).

## The road graph

The function `RoadGraph::from_conn` builds one graph for the whole run:

- Read the nodes from `osm_node` and the node order from `osm_way_node`.
- Make one directed segment per adjacent node pair. The `oneway` tag decides which travel directions the adjacency allows.
- Put the segments into a flat cell grid (CSR). The function `segments_near` reads the cells around a point, so the candidate search stays fast.
- Convert the node positions to planar metre coords (`xy`). The hot loops then use cheap planar math instead of haversine.

The run shares the graph over all worker threads as an `Arc`.

## The parallel match

The function `match_jobs` processes the directions in parallel with rayon. Each worker owns one `Ctx`. The `Ctx` holds an A* result cache and epoch-stamped search arrays. The worker reuses them across its directions, so the reset cost is constant, not a sweep over all nodes. Per-direction failures are non-fatal; the run continues with the other directions.

## The Viterbi core

Each direction goes through `match_path`. In short:

- Collect candidate states near each point (radius 200.0 m, oneway honored).
- Score candidates with the emission cost, and steps with the transition cost.
- Connect states across points with network distances (direct, U-turn, or A* with a repair bound).
- Pick the cheapest state path, and merge it into `WayRun` records.

### The states

A candidate state is a directed position on one road segment. The `State` struct holds the segment id, the travel direction (`forward`), the position along the segment (`t`, between 0 and 1), and the emission distance (`dist_m`).

The function `candidates` collects the states for one point:

- Find all segments within the search radius. The default radius is 200.0 m.
- One segment normally gives two states, one for each travel direction.
- A one-way way gives one state only. The `oneway` tag controls this.

### The costs

Two cost models live in `crates/rm-osm-match/src/models.rs`. Only relative costs matter.

- The emission cost punishes distance from the point to the candidate. `NewsonKrummEmission` uses `0.5 * (dist_m / sigma_m)^2` with `sigma_m` 10.0. The cost grows with the square of the distance.
- The transition cost punishes an implausible step between two consecutive points. `BasicTransition` uses `|gps_m - net_m| / beta_m` with `beta_m` 5.0. It adds a U-turn penalty of 10.0 and a turn angle penalty.

Here `gps_m` is the straight-line step between the two points. `net_m` is the travel distance on the road network.

### The network distance

The function `evaluate` computes the step between two states:

- Same segment, same direction: the distance is the motion along the segment.
- Same segment, opposite directions: a U-turn through the segment end. The step pays the U-turn penalty.
- Shared end node: the positions connect directly at that node.
- Different roads: the step runs an A* search over the directed graph. The oneway rules apply here too.

A planar pre-check rejects a pair before A* when its straight-line span exceeds the repair bound `500.0 + 2.0 * gps_m`. The same bound rejects a found path that is too long. A* also stops after 4000 expansions (`ASTAR_EXPANSION_CAP`). A rejected step means the two states are disconnected. The A* results go into a cache that the job reuses across directions.

### The dynamic program

- Walk the points in order.
- Give every candidate state a best total cost.
- The best total cost is the emission cost, plus the cheapest transition from a state of the previous point.
- Record the winning predecessor of each state.
- Prune states in ascending order, so the work stays small (the 2026-10-06 optimization).

At the last point, select the state with the lowest total cost. Walk the recorded predecessors back to the start. This gives one state per point.

### The output of the core

The step merges the state sequence into way runs. A `WayRun` is a contiguous run of one way in travel order. Each run records the `way_id`, the entry and exit OSM nodes, the travel direction (`forward`), and the chainage span from `cum_start_m` to `cum_end_m`.

## The write

The function `write_outcomes` rebuilds the `seg_match_way` and `seg_match_stat` tables from scratch. Each `seg_match_way` row gives one way run of one direction: the way id, the entry and exit OSM nodes, and the chainage span. The rebuild makes the step idempotent.

## The --stale refresh

The function `run_stale` re-matches only the directions that the edit flow marked stale:

1. Read the `stale_mark` rows with the table name `seg_match`. Each key is `route_id:direction`.
2. Keep the mark, and skip the job, when the key is unparseable, the route is not published, or the latest batch dropped the staging row.
3. Re-match exactly the other marked directions.
4. Delete exactly the snapshot marks. A mark that an editor re-wrote during the run survives, because the delete matches the `rowid` and the `cause_edit_id` together.

## Who uses the result

- The bearing step reads `seg_match_way` for the OSM source of stop bearings.
- `rm-segments` projects the stops onto the runs (`path_stop_projection`).
- The web QA endpoint `/api/qa/matchdev` reports large deviations between the stops and the match.

## How to run it

Build in release mode: `cargo run -r -p rm-osm-match -- --db data/taipei.db`. A debug build is 10 to 30 times slower on this workload. The release run takes about 11 minutes for the 812 directions of taipei.db. Use the `--stale` mode of the refresh runbook for edits.
