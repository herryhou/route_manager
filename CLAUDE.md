# Project notes for LLM agents

## Always use release builds for pipeline binaries

Heavy pipeline binaries (`rm-osm-match`, `rm-registry`, `rm-segments`, and
similar) must run in release mode:

```bash
cargo run -r -p <crate> -- <args>      # -r == --release
```

Debug builds run 10–30x slower on this numeric workload (no inlining, no
vectorization, bounds checks everywhere). `README.md` documents the correct
invocations — obey it.

Incident (2026-10-06): a debug `rm-osm-match` run on `data/taipei.db` used
870+ CPU-minutes; release finishes in minutes. Reference timings (taipei.db,
812 directions, M-series, all cores): debug 1hr+ (killed); release 23 min;
11 min after the 2026-10-06 optimization pass. Expect minutes, not hours.
If a release run far exceeds this, profile before assuming more hardware is
needed.

To diagnose a slow run: `ps aux | grep rm-` — the binary path shows
`target/debug/` vs `target/release/` immediately.

## rm-web: no programmatic zoom or pan (whitelist only)

Application code must not change the map zoom. Single exception: the
initial view at map creation (`L.map(...).fitBounds(...)` in
`assets/js/app.js`). Never call `setZoom`, `setView` with a zoom argument,
`flyTo`, `fitBounds`, or `zoomIn`/`zoomOut` from app logic. User-driven
zoom (scroll wheel, +/- controls) stays fully allowed — the ban is on the
program moving the zoom, not on the user.

Pan is allowed only in these four flows (2026-10-13 decision — all other
selection is display-only: markers, popups, panels, hash, layers change;
the viewport never moves):

1. Route info panel stop click (`popupStop`) — pans to the clicked stop.
2. Stop drill-down (`selectStop`) — pans to the stop (covers hash `#stop=`
   restore, pane ←/→ nav, post-edit jumps to new stops).
3. Station select (`selectStation`) — pans to the station (covers hash
   `#station=` restore, search list, popup/panel chips, edit flows).
4. QA outlier panel pick (`openOutliers` `onPick`) — pans to the outlier
   (edit workflow needs to locate off-map stops).

Route selection (`selectRoute`/`selectRouteById`) never pans — no `center`
option exists; do not reintroduce one.

## Performance work discipline

Two rules, both paid for:

- **Measure before designing.** Spike first; numbers pick the design, not
  intuition. (2026-10-14: the cold-start cache proposal died on measurement —
  graph load+build is ≤0.5s; the real cost is per-direction matching.)
- **Verify numeric refactors against a byte-identical baseline.** Save the
  current release binary, rerun after the change, byte-diff the dumped
  tables. Empty diff ships on its own; output drift needs explicit sign-off
  with quality metrics. (+0.6% for 13m chainage drift → reverted; 2.06x
  accepted only after a snap-quality comparison.)

## Correctness invariants — each one already broke once

- Join only on explicit FK chains. `route_id = route_version_id` once matched
  by coincidence (both counters start at 1) and broke on re-import.
- DB writes must be deterministic: never order inserts by HashMap iteration;
  a rerun must produce identical rows.
- Undo/apply guards check before writing; pin the rollback order with a test.
- `path_stop_projection.chainage_m` and `seg_match_way.cum_*_m` are on
  different scales (provider polyline vs matched OSM path) — never join them.

## Migrations vs read-only viewers

`rm_db::open_readonly` never migrates. Any read query touching a table added
by a new migration must probe `sqlite_master` and degrade gracefully, or old
DBs get 500s (the `/raw` incident, 2026-10-13).

## OSM graph: clip, never delete

Never slim the road graph by deleting un-referenced ways — A* explores far
beyond the finally matched ways, and match quality collapses. Slim only with
`rm-osm-prepare --clip "south,west,north,east"` (keeps any way with a node in
the bbox, so boundary connectivity survives).

## rm-web frontend loop

- Assets are `include_str!`-compiled into the binary: after editing JS/CSS,
  rebuild + restart + browser hard refresh, or nothing changes — check this
  before debugging a "my change did nothing" report.
- A new JS file must be registered twice (`handlers/assets.rs` match arm and
  the `handlers/mod.rs` route); missing one silently kills the module graph.
- No JS test infra: `node --check` + release build + manual browser pass.
- Vendored Leaflet files stay pristine (upgrade path); overrides go in our CSS.

## rm-web UX rules

Visual and interaction rules live in `crates/rm-web/DESIGN.md` (Do's and
Don'ts) — read it before any rm-web UI work. The viewport rule above applies
on top of it.
