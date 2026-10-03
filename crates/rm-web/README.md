# rm-web

The viewer half of the pipeline: a read-only web explorer over the SQLite
database the other crates produce. One binary serving a Leaflet map and a
small JSON API — browsing changes no pipeline state, and the server can
stay running while the pipeline writes.

## How to run

```sh
cargo run -r -p rm-web -- --db data/taichung.db --addr 127.0.0.1:8787
```

| Flag | Default | Meaning |
|---|---|---|
| `--db` | `data/taichung.db` | pipeline database to view |
| `--addr` | `127.0.0.1:8787` | listen address |
| `--tiles` | OpenStreetMap raster tiles | basemap URL template override |

The default basemap needs internet; every other layer is offline. Leaflet
is vendored (`assets/vendor/`) and all static assets are `include_str!`-embedded
(`src/handlers/assets.rs`), so the binary has no runtime files.

## Read-only by construction

- Every request opens a fresh connection via `rm_db::open_readonly` —
  `SQLITE_OPEN_READ_ONLY`, no pragmas that write, no migrations — so WAL
  readers coexist with a pipeline writer (`src/state.rs`).
- `serve` probes the schema once before binding (`ensure_schema` in
  `src/db.rs`): a missing DB, or one that predates the segment tables,
  fails with a "run the pipeline stages first" message instead of
  per-request 500s.
- Sync SQL runs on tokio's blocking pool (`state::run_db`); the query
  functions in `src/queries/` are pure `rusqlite` code with no axum
  types, and handlers are thin wrappers over them.

## What you can do

- **Route browsing** — the sidebar lists routes (numeric codes first);
  the search box filters client-side by code prefix or name/operator
  substring (`filterRoutes` in `assets/js/panels.js`). Selecting a route
  draws its canonical path (solid blue; `no_path` pieces dashed red) and
  stop markers colored by projection method, with a direction toggle
  去程/返程 and an ordered stop list.
- **Stop drill-down** — clicking a stop opens a compact popup (name,
  bearing, station, serving routes); *more details* opens the side panel:
  stop → station → station group, ring highlights over the group's member
  stations, and serving routes at stop / station / group level — every
  route chip jumps to that route. Selecting a stop pans the map to it.
- **Viewport layer** — *Stops / stations* draws the current viewport from
  bbox queries, debounced on pan: stations at zoom 12–14, individual
  stops at zoom ≥ 15 (`refreshStopsLayer` in `assets/js/app.js`). Station
  markers open their own popup: group, member stop codes, and the
  station's bearing axis with spread.
- **QA review** — topbar chips show dataset counts (routes, stops,
  stations, segments, matched directions, outliers) from `/api/stats`.
  The *outliers* chip lists the worst stop placements (snap distance over
  50 m or `OUTLIER`/`NO_PATH` method); clicking one pans to it, its route
  code opens the route. Each route panel has a *QA profile* tab: an SVG
  scatter of snap distance along the chainage. Direction dots in the
  route list are colored by p50 snap distance (green < 15 m, amber
  < 40 m, per `assets/js/colors.js`); their tooltips carry stop counts
  and matched-length ratio.
- **Segment registry** — *Segments* draws canonical corridors for the
  viewport (zoom ≥ 13), colored by how many routes share them; clicking
  one shows its ways (name, highway class, length) and every traversing
  route. On truncation the most-shared corridors survive —
  `queries/segments.rs` sorts by route count before applying the limit.
- **OSM overlay** — with a route selected, *OSM ways* draws the matched
  street ways (dashed green), each sliced to its entry/exit node window.
- **Raw compare** — *Raw compare* overlays the latest batch's raw
  staging data — the `src_route_point` polyline (dashed) and
  `src_stop_edge` squares — against the canonical view.
- **Bearings** — dot-and-arrow markers rotated to each stop's departure
  heading (or a station's axis); on by default, toggled in the sidebar,
  redrawn live on both route stops and the viewport layer.

The detail panel is tabbed (`showDetail` in `assets/js/panels.js`): route,
stop, segment, and outlier panes stay open side by side, so a stop can be
inspected next to the segment that contains it.

## URL hash sharing

Every selection writes the URL hash, and route / stop / segment hashes
are replayed at boot (`parseHash` in `assets/js/app.js`): paste someone
`#route=1&dir=0`, `#stop=123`, or `#segment=<id>` and they land in the
same view. The outliers panel sets `#outliers` too (not replayed at
boot).

## HTTP API

All GET, all JSON; errors come back non-2xx as `{"error": "..."}`.

| Endpoint | Returns |
|---|---|
| `/api/stats` | counts, match summary, outlier total, latest batch |
| `/api/routes?q=&limit=` | route list with per-direction QA |
| `/api/routes/{id}` | route detail (providers, main/side links, directions) |
| `/api/routes/{id}/directions/{dir}/path` | canonical geometry (GeoJSON) |
| `/api/routes/{id}/directions/{dir}/stops` | ordered stops + projection QA |
| `/api/routes/{id}/directions/{dir}/ways` | matched way runs (GeoJSON) |
| `/api/routes/{id}/directions/{dir}/raw` | latest-batch staging compare |
| `/api/stops?bbox=&kind=stop\|station` | viewport layer; `?q=` searches names/codes |
| `/api/stops/{id}` | stop → station → group drill-down |
| `/api/stations/{id}` | station + group + serving routes |
| `/api/qa/outliers?min_snap=&limit=` | worst projections, snap-descending |
| `/api/segments?bbox=&min_routes=&limit=` | corridors in bbox (GeoJSON) |
| `/api/segments/{id}` | ways composition + traversing routes |

## Tests

```sh
cargo test -p rm-web
```

`tests/api.rs` builds fixture databases by running the real upstream
crates (rm-ingest through rm-registry are dev-dependencies) and asserts
each endpoint through the axum router — including ways trimming,
raw-compare fallback for missing staging rows, and limit-truncation
ordering.

## Why this crate exists

The pipeline's output is rows; the questions you actually ask of it —
did this direction match cleanly, do the shared corridors line up, is an
outlier bad source data or a projection artifact — are map questions.
rm-web puts one SELECT per layer behind HTTP so eyeballing QA doesn't
mean pasting SQL into a shell, and because it opens the database
read-only and never migrates, it can sit open in a browser across
pipeline re-runs without perturbing them.
