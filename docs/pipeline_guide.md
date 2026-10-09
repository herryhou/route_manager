# Pipeline Guide

A plain-language tour of what this project builds and how.

## What the pipeline does

It turns two raw inputs — a city bus authority's JSON captures (Taichung 392 routes; Taipei and Tainan via the same capture-tree contract) and a local OpenStreetMap extract — into a single SQLite database per city that knows: every bus stop, every route, and exactly which streets each route takes between consecutive stops. The last part is the point: for any stop pair you can derive the ordered OSM way list directly from the pieces and way runs, so you can later detect shared corridors and build arrival-time predictions on top.

## The seven stages, in order

1. **Load** (`rm-ingest`) — copy the raw JSON into simple staging tables, one row per file record. Nothing is interpreted yet. Everything downstream works from one "batch" of imported data.

2. **Identify** (`rm-identity`) — decide what counts as the same thing across routes. A *station* is one physical pole (identified by the API's node ID), so the same pole served by ten routes is one station. A *stop* is "this pole as visited by this route in this direction." Poles sharing a name within 80 m get grouped.

3. **Publish** (`rm-publish`) — for each route, write an immutable "version 1" snapshot: its two directions and their stops in order. One database transaction per route, so a bad route can't leave half-written data.

4. **Project** (`rm-segments`) — take each route's drawn path (a Google polyline) and calculate where along that path each stop sits (its "chainage" — distance from the start). This tells us the order and spacing of stops along the road.

5. **Load the map** (`rm-osm-prepare`) — read the OSM file into road tables: ways, nodes, how they connect.

6. **Match** (`rm-osm-match`) — for each route direction, snap its drawn path onto real OSM streets. Uses a hidden-Markov-style matcher (the standard map-matching technique): it finds the most likely sequence of streets given the GPS-like trace, tolerates gaps by searching for short connecting paths, and records the result as a list of "way runs" with cumulative distances.

7. **Finalize stations** (`rm-registry`) — the payoff. It runs two passes:

   - **Bearings** — the departure heading of the bus at each stop (from the matched OSM way when one covers it, otherwise from the drawn polyline) and, per station, the travel axis of its stops' headings (a one-way pole keeps its signed heading; two-way poles fold headings 180° apart onto one axis) plus the largest deviation from it.
   - **Station split** — a station whose member headings form 2+ clusters over 90° apart (poles across a dual carriageway) is split into per-side stations; the biggest cluster keeps the parent, and station bearings are rebuilt afterwards.

   (An earlier design also built a cross-route "segment registry" here. It was removed on 2026-10-09 — the station pair is the identity, and each route's ordered way list derives directly from its pieces × way runs; see `docs/segment-handoff.md`.)

Finally **verify** (`rm-verify`) re-checks the invariants and counts against per-city known-good numbers (`--preset taichung|taipei|tainan`) and writes the QA files (`qa/report.json`, a CSV of badly-placed stops, bearing / match-deviation / pair-spread / grouping reports). It exits non-zero if anything is off. Warnings never fail the run: stations whose member stops sit more than 45° off the axis, directions whose matched path strays > 50 m p95 from the source polyline, station pairs whose piece lengths spread beyond `max(100 m, 3%)`, and groups that chain-drifted too far.

## What you get out

- One SQLite DB per city (`data/taichung.db`, `data/taipei.db`, `data/tainan.db`) — the whole model: routes, stops, stations, groups, OSM graph, match stats, per-route path pieces with their ordered OSM way runs
- Taichung: 36,531 path pieces; ~22 station pairs flagged by the pair-spread check. Taipei: 27,381 / ~25. Tainan: 8,330 / ~18.

## Known limits

With the full-Taiwan extract (clipped to the routes' bbox + 10 km; `taiwan-261004.osm.pbf`), 767 of 784 directions match; the remaining 17 are routes with no drawn polyline at all, reported (not hidden) as skipped pairs. One matched direction (352延) strays > 50 m p95 from its polyline — warn-only, in `qa/match_dev_report.json` and the rm-web matchdev chip. Timetables, GTFS export, and a second-import diff are deliberately out of scope for this version — the schema leaves room for them.

See `README.md` for build/run instructions and command flags.

## City extract clips (2026-10-06)

Each city DB prepares from the same Taiwan-wide PBF with a route-bbox +
10 km clip (derivation: decode all `src_route_point` polylines, take the
min/max, pad 0.09° lat and 0.09°/cos(mid-lat) lon). Reproduce with:

| City | `--db` | `--clip south,west,north,east` |
|---|---|---|
| Taichung | `data/taichung.db` | `23.89,120.41,24.53,121.42` |
| Taipei | `data/taipei.db` | `24.85,121.30,25.30,121.77` |
| Tainan | `data/tainan.db` | `22.47,119.99,23.56,120.69` |

Each DB also carries its clip in `osm_meta.clip_bbox`. Re-derive the bbox
when a city's route coverage changes (new routes may leave the old box).

## Browsing the result: rm-web

`rm-web` is a read-only web viewer over the database the pipeline produces — no pipeline step is changed by using it.

```sh
cargo run -r -p rm-web -- --db data/taichung.db --addr 127.0.0.1:8787
```

Then open the address in a browser. Flags: `--db` (default `data/taichung.db`), `--addr` (default `127.0.0.1:8787`), `--tiles URL` (override the basemap tile server; default is OpenStreetMap's, which needs internet — every other layer is offline).

What you can do:

- **Route browsing** — search the sidebar (route code/name or station name — station search hits the server); click a route: its path (provider polyline or matched OSM ways — a geometry selector) and stops draw on the map, with a stop list and direction toggle (去程/返程) in the panel. Clicking a path piece opens its pane with the piece highlighted.
- **Stop drill-down** — click any stop: its station, station group (rings on the map), and every route serving it at stop / station / group level; every cross-reference is clickable.
- **QA review** — two clickable chips (red on non-zero): *outliers* (worst stop placements — OUTLIER/NO_PATH or snap > 50 m), *path dev* (directions whose matched path strays > 50 m p95 from the source polyline — the detours snap stats can't see). Each route has a snap-distance profile chart; direction dots in the route list are colored by match quality.
- **OSM overlay** — with a route selected, the OSM-ways geometry setting draws the matched street ways (entry/exit-trimmed) under the route.
- **Raw compare** — *Raw compare* draws the raw staging polyline and raw stop squares against the canonical view.
- **Bearings** — dot-and-arrow markers per stop (and a station's axis), on by default.

The detail panel is two stacked panes (context + point), each with its own back/forward history; `Esc` closes popup → point → context, `/` focuses search, the sidebar hides behind a topbar toggle. The server opens the database with SQLite read-only flags and never runs migrations, so it can run while the pipeline writes. Views are shareable via URL hash (`#route=1&dir=0`, `#stop=123`, `#station=45`).
