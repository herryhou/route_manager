# rm-osm-prepare

Stage 5 of the pipeline: read a local OSM PBF extract into the `osm_*`
road graph tables — the map that `rm-osm-match` later snaps bus paths onto.
No bus data is touched; this stage only decides which OSM ways a bus could
drive on, and stores them once for every route direction matched after it.

## Input → output

Input is the PBF extract (`--pbf`, default
`assets/Taichung/data/taichung.osm.pbf`); `--db` selects the database
(default `data/taichung.db`). There is no `--batch` — nothing here is
batch-scoped. Writes, in one transaction:

| Output | Meaning |
|---|---|
| `osm_way` | one kept way: `highway` class, folded `oneway`, `name` |
| `osm_way_node` | a way's node refs in travel order (1-based `ordinal`) |
| `osm_node` | coordinates, only for nodes referenced by a kept way |
| `osm_meta` | provenance: pbf file name, sha256, import time, whitelist, counts |

## What counts as a road

`src/filter.rs` (pure, unit-tested) is the whole policy:

- **Whitelist** — the way must carry a `highway` tag in
  `HIGHWAY_WHITELIST`: motorway, trunk, primary, secondary, tertiary and
  their `_link` variants, unclassified, residential, service, and
  bus_guideway. Footways, cycleways, paths, pedestrian streets, tracks,
  steps and proposed roads are out; ways with no `highway` tag are out.
- **Shape** — fewer than 2 node refs → dropped.
- **`oneway` folding** — `yes`/`true`/`1` → 1, `-1`/`reverse` → -1,
  anything else (missing, `no`, `alternating`) → 0.
- **`name`** — kept when tagged, `''` otherwise.

## How it runs

1. **Ways pass** — stream the PBF once; ways passing the filter are held
   in memory and every node id they reference goes into a want-set.
2. **Nodes pass** — stream the PBF again; only nodes in the want-set are
   kept, coordinates only (OSM node tags are not stored).
3. **Atomic rewrite** — one transaction deletes all four `osm_*` tables
   and re-inserts everything. Idempotency here is *replace*, not lookup:
   rerunning on the same PBF yields the same tables, and rerunning on a
   new extract swaps the graph in whole — no half-old rows survive.
4. **Provenance** — `osm_meta` records the pbf file name, its sha256,
   import time, the whitelist in force, and way/node counts, so a later
   reader can tell which extract produced the graph.

`main` prints one summary line: ways, way_node refs, nodes.

## How the graph is shaped for the matcher

`rm-osm-match`'s `RoadGraph::from_conn` reads exactly these tables:
`osm_node` for coordinates, and `osm_way` joined to `osm_way_node`
ordered by `(osm_way_id, ordinal)` to rebuild each way as an ordered node
sequence. It cuts every consecutive node pair into a segment (haversine
length) and builds directed adjacency honoring `oneway` — 1 travels
forward only, -1 reverse only, 0 both ways — which is why the tag is
folded to that integer here, once. Ordinals are contiguous from 1, as the
rebuild expects; travel order is what `rm-registry` later hashes into
`segment_id`. The schema also indexes `osm_way_node(osm_node_id)` for
node → way lookups. An empty `osm_node` makes the matcher refuse to start
with "run rm-osm-prepare".

## Why this stage exists

The matcher offers the graph's ways as candidates, so the whitelist is
the vocabulary of matching: a way that never gets stored can never be
snapped to, and a corridor two routes share is shared because both
matched the same stored ways — those way ids, in travel order, are half
of `segment_id`. Centralizing the filter also means one shared graph for
every direction instead of each match re-reading the PBF, and swapping
in a larger extract is a rerun of this one bin (the fix for the coverage
holes noted in the root README's known limits).
