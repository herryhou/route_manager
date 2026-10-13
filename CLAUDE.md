# Project notes for LLM agents

## ALWAYS use release builds for pipeline binaries

When running any workspace binary that does heavy computation (`rm-osm-match`,
`rm-registry`, `rm-segments`, and similar pipeline steps), always build/run in
release mode:

```bash
cargo run -r -p <crate> -- <args>      # -r == --release
```

Never use plain `cargo run -p <crate>` for these. Debug builds run 10–30x
slower on this numeric workload (Viterbi/A*/haversine math: no inlining, no
vectorization, bounds checks everywhere).

Incident (2026-10-06): an agent ran `cargo run -q -p rm-osm-match -- --db
data/taipei.db` without `-r`. The debug build pinned all CPUs at 100% for over
an hour (870+ CPU-minutes) on a job the release build finishes in minutes.
`README.md` already documents the correct invocations — follow it.

Reference timings, taipei.db (812 directions, M-series, all cores): debug
1hr+ (killed); release 23 min; release after the 2026-10-06 optimization pass
(planar node coords, flat CSR grid, shared per-worker A* cache/scratch, Arc
cache entries, ascending-order DP prune) 11 min. Expect minutes, not hours;
if a release run far exceeds this, profile before assuming more hardware
is needed.

Diagnosis tip: if one of these runs seems inexplicably slow, check
`ps aux | grep rm-` — the binary path shows `target/debug/` vs
`target/release/` immediately.

## rm-web: never change the Leaflet zoom or pan programmatically (except whitelist)

In the rm-web frontend, application code is forbidden from changing the map
zoom level. The single allowed exception is the initial view at map creation
(`L.map(...).fitBounds(...)` in `assets/js/app.js`). Never call `setZoom`,
`setView` with a zoom argument, `flyTo`, `fitBounds`, or `zoomIn`/`zoomOut`
from app logic. User-driven zoom (scroll wheel, +/- controls) stays fully
allowed — the ban is on the program moving the zoom, not on the user.

Panning is likewise forbidden except for exactly four whitelisted flows
(2026-10-13 decision — all other selection is display-only: markers,
popups, panels, hash, layers change; the viewport never moves):

1. Route info panel stop click (`popupStop`) — pans to the clicked stop.
2. Stop drill-down (`selectStop`) — pans to the stop (covers hash `#stop=`
   restore, pane ←/→ nav, post-edit jumps to new stops).
3. Station select (`selectStation`) — pans to the station (covers hash
   `#station=` restore, search list, popup/panel chips, edit flows).
4. QA outlier panel pick (`openOutliers` `onPick`) — pans to the outlier
   (edit workflow needs to locate off-map stops).

Route selection (`selectRoute`/`selectRouteById`) no longer pans at all —
no `center` option exists; don't reintroduce one.

