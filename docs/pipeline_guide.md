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

   The same stage then computes **bearings**: the departure heading of the bus at each stop (from the matched OSM way when one covers it, otherwise from the drawn polyline) and, per station, the mean travel axis of its stops' headings (headings 180° apart fold onto one axis — a pole serves both directions of a road) plus the largest deviation from that axis.

Finally **verify** (`rm-verify`) re-checks ~26 invariants and counts against known-good numbers and writes QA files (`qa/report.json`, a GeoJSON map of all segments, a CSV of badly-placed stops). It exits non-zero if anything is off. Stations whose member stops sit more than 45° off the station's axis are warnings, not failures — they land in `report.json` warnings and `qa/bearing_report.json` (reverse travel never flags: one axis ± 180° is legitimate).

## What you get out

- `data/taichung.db` — the whole model: routes, stops, stations, groups, OSM graph, match stats, segment registry
- 35,876 stop-to-stop traversals, 10,631 unique segments, 5,897 shared by more than one route (routes 1 and 9221 share 22 — the built-in sanity pair)

## Known limits

With the 2026-10 citywide extract (`TW-TXG-202610020300.osm.pbf`), 767 of 784 directions match; the remaining 17 are routes with no drawn polyline at all, reported (not hidden) as skipped pairs. Timetables, GTFS export, and a second-import diff are deliberately out of scope for this version — the schema leaves room for them.

See `README.md` for build/run instructions and command flags.

## Browsing the result: rm-web

`rm-web` is a read-only web viewer over the database the pipeline produces — no pipeline step is changed by using it.

```sh
cargo run -p rm-web -- --db data/taichung.db --addr 127.0.0.1:8787
```

Then open the address in a browser. Flags: `--db` (default `data/taichung.db`), `--addr` (default `127.0.0.1:8787`), `--tiles URL` (override the basemap tile server; default is OpenStreetMap's, which needs internet — every other layer is offline).

What you can do:

- **Route browsing** — search the sidebar, click a route: its path (from the segment geometry) and stops draw on the map, with a stop list and direction toggle (去程/返程) in the panel.
- **Stop drill-down** — click any stop: its station, station group (rings on the map), and every route serving it at stop / station / group level.
- **QA review** — the *outliers* chip lists the worst stop placements (OUTLIER/NO_PATH or snap > 50 m); each route has a snap-distance profile chart; direction dots in the route list are colored by match quality.
- **Segment registry** — enable *segments*: canonical corridors colored by how many routes share them (zoom ≥ 13); click one for its ways and traversing routes.
- **OSM overlay** — with a route selected, *OSM ways* draws the matched street ways (entry/exit-trimmed) under the route.
- **Raw compare** — *Raw compare* draws the raw staging polyline and raw stop squares against the canonical view.

The server opens the database with SQLite read-only flags and never runs migrations, so it can run while the pipeline writes. Views are shareable via URL hash (`#route=1&dir=0`, `#stop=123`, `#segment=<id>`).
