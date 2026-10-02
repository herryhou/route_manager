# Pipeline Guide

A plain-language tour of what this project builds and how.

## What the pipeline does

It turns two raw inputs — the Taichung bus authority's JSON captures (392 routes) and a local OpenStreetMap extract — into a single SQLite database that knows: every bus stop, every route, and exactly which streets each route takes between consecutive stops. The last part is the point: two routes running down the same road get the same "segment" ID, so you can later detect shared corridors and build arrival-time predictions on top.

## The seven stages, in order

1. **Load** (`rm-ingest`) — copy the raw JSON into simple staging tables, one row per file record. Nothing is interpreted yet. Everything downstream works from one "batch" of imported data.

2. **Identify** (`rm-identity`) — decide what counts as the same thing across routes. A *station* is one physical pole (identified by the API's node ID), so the same pole served by ten routes is one station. A *stop* is "this pole as visited by this route in this direction." Poles sharing a name within 80 m get grouped.

3. **Publish** (`rm-publish`) — for each route, write an immutable "version 1" snapshot: its two directions and their stops in order. One database transaction per route, so a bad route can't leave half-written data.

4. **Project** (`rm-segments`) — take each route's drawn path (a Google polyline) and calculate where along that path each stop sits (its "chainage" — distance from the start). This tells us the order and spacing of stops along the road.

5. **Load the map** (`rm-osm-prepare`) — read the OSM file into road tables: ways, nodes, how they connect.

6. **Match** (`rm-osm-match`) — for each route direction, snap its drawn path onto real OSM streets. Uses a hidden-Markov-style matcher (the standard map-matching technique): it finds the most likely sequence of streets given the GPS-like trace, tolerates gaps by searching for short connecting paths, and records the result as a list of "way runs" with cumulative distances.

7. **Register segments** (`rm-registry`) — the payoff. For each pair of consecutive stops, take the street ways between them and hash them together with the two stations:

   `segment_id = hash(station A, station B, way 1, way 2, …)`

   Same corridor + same stations = same ID, whichever route you came from. Reverse direction hashes differently (streets are directed). Routes sharing a road segment now visibly share a row.

Finally **verify** (`rm-verify`) re-checks ~26 invariants and counts against known-good numbers and writes QA files (`qa/report.json`, a GeoJSON map of all segments, a CSV of badly-placed stops). It exits non-zero if anything is off.

## What you get out

- `data/taichung.db` — the whole model: routes, stops, stations, groups, OSM graph, match stats, segment registry
- 19,612 stop-to-stop traversals, 6,111 unique segments, 3,326 shared by more than one route (routes 1 and 9221 share 10 — the built-in sanity pair)

## Known limits

The local OSM extract has holes at the edges of the city: 204 of 761 directions couldn't be matched to streets and are reported (not hidden) as skipped pairs. Fix is a bigger extract, not code. Timetables, GTFS export, and a second-import diff are deliberately out of scope for this version — the schema leaves room for them.

See `README.md` for build/run instructions and command flags.
