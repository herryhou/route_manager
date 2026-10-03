# rm-osm-match

Stage 6 of the pipeline: snap every published direction polyline onto
the OSM road graph. A hidden-Markov matcher (Viterbi over candidate
states, Newson–Krumm costs, A* gap repair) turns each drawn path into an
ordered list of way runs — which OSM ways the direction travels, in
travel order, with cumulative chainage — plus per-direction quality
stats.

## Input → output

Reads the batch's current published route versions (`route` +
`route_version.current_route_version_id` + `subroute_version`), the
direction polylines from `src_route_point` (joined back to routes by
route number), and the `osm_node` / `osm_way` / `osm_way_node` graph
written by `rm-osm-prepare`. Writes, in one transaction:

| Output | Meaning |
|---|---|
| `seg_match_way` | ordered way runs per (subroute_version_id, direction): `osm_way_id`, entry/exit OSM node, chainage span `cum_start_m`..`cum_end_m` |
| `seg_match_stat` | per (route_version_id, direction): stop count, path vs matched length, snap distance p50/p95/max, outliers, gap repairs |

Both tables are `DELETE`d first — the stage is a full rebuild, so
rerunning on the same inputs writes the same rows.

## How the matcher runs (`src/viterbi.rs`)

1. **Candidates** — each polyline point projects onto nearby way
   segments (100 m grid cells, 200 m search radius). A bidirectional
   segment yields two directed states; a oneway street only its legal
   direction. Points with no candidates drop out; the DP runs over the
   rest.
2. **Emission** (`NewsonKrummEmission`) — the distance-dependent term
   of the Newson–Krumm snap cost: `0.5 · (d / sigma)²`. Only relative
   costs matter.
3. **Transition** (`BasicTransition`) — `|gps − net| / beta` between
   consecutive states (great-circle step vs network distance), plus a
   U-turn penalty and a turn-angle penalty at shared nodes. The gps step
   is the direct point-to-point displacement, not accumulated chainage —
   unmatched points in between must not inflate it. Reverse motion under
   5 m on the same state is treated as jitter, not a U-turn.
4. **Viterbi DP** over the lattice. If no state at a point is reachable
   from the previous one (unbridgeable jump), the DP restarts there
   instead of zeroing the direction; backtrace then walks each DP
   segment separately.
5. **A\* gap repair** — when consecutive states share no node, A* over
   directed adjacency (oneway honored, per-pair cached, 4000-expansion
   cap) bridges the gap, and its intermediate segments join the run
   list. A repair is rejected unless the network path stays within
   `500 m + 2 × gps step`, so A* cannot wander km-scale loops through
   extract holes.
6. **Extraction** — chosen states become per-transition pieces with
   chainage spread proportionally to network length, then merge into
   contiguous runs of the same way in the same direction (interior
   same-segment spans chain without a shared boundary node).

Cost constants live in `src/models.rs` behind `EmissionModel` /
`TransitionModel` traits (defaults: sigma 10 m, radius 200 m, beta 5 m,
U-turn 10, turn scale 5) — swap the model, not the matcher, to tune.
Matched points snapping worse than 50 m (`OUTLIER_M`, matching
rm-segments' default) count as `outliers` in `seg_match_stat`.

## Per-direction failure handling

Nothing here fails the run. A direction that errors (e.g. an
undecodable polyline) is warned and counted `failed`; one that yields no
runs — degenerate polyline, or nothing near any road — is counted
`unmatched`; both leave no rows and show up in the summary line. Known
context: the local OSM extract has holes at the city edges, so ~204 of
761 directions match nothing (root README, *Known limits*) — the fix is
a bigger extract, not code. The one hard error is empty `osm_*` tables,
which means `rm-osm-prepare` hasn't run.

## Running it

```sh
cargo run -r -p rm-osm-match   # --db (default data/taichung.db),
                               # --batch (default: latest OPEN)
```

Directions match in parallel (rayon) against one shared read-only
`RoadGraph`; writes are serialized in the single closing transaction.
Prints one summary line: directions / matched / unmatched / failed /
runs / gap repairs. To dissect one direction,
`cargo run -p rm-osm-match --example dbg_match -- <xno> <dir>` prints
per-point candidate counts and match stats.

## Why this stage exists

`rm-registry` hashes `segment_id` over station ext_uids plus the
travel-order way ids between each stop pair — those way ids come from
`seg_match_way`, clipped to the pair's chainage window. Bearings work
the same way: a stop's departure heading is read off the way run
covering it. Without this stage, "which streets does this direction
use" has no answer, and shared-corridor detection — the point of the
registry — has nothing to key on.
