# Onboarding notes — stops, stations, bearings, matching

STE explanations for new engineers: how stops group into stations, how stop
bearings are computed, and how the rm-osm-match Viterbi works.

---

## How stops group into stations

**Stops and stations — how the grouping works**

This project has two entity levels: the stop and the station. A stop is one pole at one place. Several stops can share one station. Example: the poles on the two sides of a dual carriageway share one station, but each pole has its own coordinates.

### Where the stop data comes from

The import step reads rows from the `src_stop_edge` table. Each row gives one visit of a route at a stop. Each row has a `node_id`, a `station_name`, and coordinates. The system makes one stop record for each unique (`xno`, `direction`, `sid`) combination. It makes one station candidate for each unique `node_id`. The first occurrence of a `node_id` wins. Its name and coordinates become the candidate attributes.

### The grouping rule

The function `group_stations` in `crates/rm-identity/src/grouping.rs` does the grouping. It uses a union-find algorithm. Apply these rules:

- Group candidates only when their names are the same.
- Pair the candidates, and keep a pair only when the haversine distance is less than 80 m.
- Chain the pairs. If A–B is 28 m and B–C is 28 m, then A, B, and C make one station.
- Never group candidates with different names, even at the same point.
- Keep candidates with the same name separate when they are far apart.
- Give a candidate with no partner its own single-member station.

Every candidate lands in exactly one station. The system mints a `station_id` and writes it to all member stops. The cluster also becomes a `station_group`, and the group name is the member station name.

### The later split pass

The `rm-registry` step `crates/rm-registry/src/split.rs` runs after the grouping. It reads a bearing for each stop. The bearing comes from the OSM way at the stop position, or from the route polyline as a fallback. If the bearings of one station make two or more heading clusters, the system splits the station into child stations. Each child keeps its members and gets a position on its own carriageway. The split is idempotent: re-running with unchanged bearings moves nothing.

### Manual override

Edit mode gives the regroup endpoint `POST /api/edit/stop/{svi}/regroup`. Use it to move one stop to a different station. The system records each change, and you can undo it.

---

## How stop bearings are computed

**Stop bearings — how the system computes them**

The bearing is the travel direction of the road at a stop. The unit is degrees: 0° is north, 90° is east. The step in `crates/rm-registry/src/bearing.rs` computes one bearing for each stop version that has geometry, and writes it to the `stop_bearing` table.

### The inputs

The step reads every direction of every route. For each stop it reads the coordinates and the `chainage_m` from `path_stop_projection`. The chainage is the distance along the route. It also reads the matched way runs from `seg_match_way`. Each run gives one OSM way, its entry and exit nodes, and its start and end chainage.

### The steps

- Sort the stops of the direction by `stop_sequence`.
- Find the run that covers the stop chainage. If no run covers it, find the nearest run.
- Give up the OSM source when the nearest run is more than 200 m away (`RUN_GAP_MAX_M`).
- Use the travel direction of the run as the bearing.
- Write the row with the source value `"OSM"`.
- Use the polyline fallback when no run qualifies. Write the row with the source value `"POLYLINE"`.

The test `osm_bearing_follows_run_travel_direction` shows the direction rule: a northbound run gives about 0°, and a reversed run gives about 180°. A stop between two runs reports the nearest way heading, for example a departure ramp.

### The polyline fallback

The function `polyline_bearing` gives the forward tangent of the direction polyline at the chainage. At or after the route end, it reports the final leg. This is the arrival heading of the last stop. The step uses this fallback when the stop sits on a road that the OSM extract cannot match.

### The limits

A stop with no geometry gets no bearing row. The step counts it in `stops_skipped`. A cycled route can visit the same stop twice in one direction. The table then holds one row only, and the last visit wins.

### The station axis

After the per-stop rows, the step rebuilds the `station_bearing` table. It groups the member bearings of each station. The function `station_axis` computes the circular mean and the maximum spread. If the spread reaches 45° (`SIGNED_SPREAD_MAX_DEG`), the station has mixed headings. The function then uses the axial mean instead: the headings 0° and 180° fold onto the same axis. A one-way station keeps its signed mean.

### Who uses the result

The split step `crates/rm-registry/src/split.rs` reads the bearings. It splits a station into child stations when its members make two or more heading clusters.

---

## The Viterbi matcher in rm-osm-match

### The problem

The step `rm-osm-match` puts each point of a route direction polyline onto the OSM road network. A simple rule picks the nearest road for each point. That rule fails at junctions and parallel roads. The step instead uses a Viterbi dynamic program. It picks the one state sequence with the lowest total cost across the whole direction.

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

### The output

The step merges the state sequence into way runs. A `WayRun` is a contiguous run of one way in travel order. Each run records the `way_id`, the entry and exit OSM nodes, the travel direction (`forward`), and the chainage span from `cum_start_m` to `cum_end_m`. The step writes these runs to `seg_match_way`. The bearing step reads them there.

### How to run it

Build in release mode: `cargo run -r -p rm-osm-match -- --db data/taipei.db`. A debug build is 10 to 30 times slower on this workload. The release run takes about 11 minutes for the 812 directions of taipei.db.
