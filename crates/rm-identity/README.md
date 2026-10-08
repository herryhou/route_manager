# rm-identity

Stage 2 of the pipeline: decide what counts as "the same thing" across the
whole batch. Reads the `src_*` staging tables written by `rm-ingest` and
mints permanent identity rows plus first (V1) version snapshots — the join
layer every later stage keys on.

## Input → output

Reads `src_route_header`, `src_provider`, `src_route_provider`,
`src_main_side_route`, `src_stop_edge`, and the batch's `valid_from`.
Writes, in one transaction:

| Output | Meaning |
|---|---|
| `route` + `subroute` | one per route number |
| `route_link` | main ↔ side route pairs |
| `route_provider` | route → operator code + name |
| `station` + `station_version` V1 | one physical pole |
| `station_group` + `station_group_version` V1 | poles grouped by name + proximity |
| `stop` + `stop_version` V1 | a pole as visited by one route in one direction |

## Three identity concepts

The crate distinguishes a "thing" at three zoom levels (see the
`*_ext_uid` constructors in `src/lib.rs`):

- **Station** — one physical pole. `ext_uid = tcg:node:{API node.id}`.
  The same pole served by ten routes is one row.
- **Stop** — a pole *as visited*: `ext_uid = tcg:{xno}:{direction}:{sid}`.
  Route 3 去程 at pole X and route 3 返程 at pole X are different stops
  sharing one station.
- **Station group** — derived: same exact name, chained within 80 m.
  `ext_uid = tcg:grp:{blake3_128 over sorted member uids joined by "|"}`
  — order-insensitive, membership-sensitive.

## How it runs

1. **Load and sort everything deterministically** — routes by numeric xno
   (`xno_key`: "2" < "10" < "9221"), stop edges by xno → direction →
   source order. Sorting before any minting means two runs produce the
   same IDs.
2. **First pass over stop edges** — the first occurrence of a `node_id`
   fixes station name/lat/lon; the first `(xno, dir, sid)` mints a stop.
   Each stop version then carries its *own* edge's name/lat/lon, not the
   station's first-seen values — poles across a dual carriageway share a
   station but sit metres apart, and the stop keeps its per-pole position
   (the registry's split pass later moves mis-clustered poles onto
   per-side stations). If the same stop key later shows a different
   node, a warning (`ambiguous stop ... first wins`) goes into the
   summary, never fails.
3. **Cluster stations** (`src/grouping.rs`) — union-find: bucket by exact
   name, union pairs under 80 m haversine, transitively — a—b 28 m plus
   b—c 28 m chains into one group. Different names never group; same name
   33 km apart never groups. Singletons become their own group with
   `source = "singleton"` (vs `"derived:same-name<80m"`).
4. **Mint with ext_uid lookup first** — every entity checks
   `SELECT id WHERE ext_uid = ?` before insert; existing row is reused,
   otherwise `rm_db::mint` allocates the ID. That lookup is the
   idempotency mechanism: rerunning on the same batch mints nothing.
   Version rows have a separate `has_version` guard so a later batch can
   mint V2 without duplicating V1.
5. **Wire the graph** — stations get `station_group_id`, stops get
   `station_id`, routes get providers and main-side links.

## Why this stage exists

Everything downstream keys on these IDs: `rm-osm-match` keys way runs by
`subroute_version_id`, `rm-registry` hashes `segment_id` over station
ext_uids. Identity is what makes "two routes share a corridor" expressible
at all — without it, every later stage would re-invent its own
"same pole?" rule.
