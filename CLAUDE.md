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
