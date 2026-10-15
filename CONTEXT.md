# route_manger — Taiwan bus pipeline

Language for the transit-data pipeline that turns raw city ebus captures
(Taichung, Taipei, Tainan) into one versioned, OSM-aligned SQLite database
per city, browsed and edited in rm-web. zh-Hant terms are canonical where
the docs and UI use them.

## The route

**Route（路線）**:
The passenger-visible main line ("307", "紅幹線") — the stable thing that
persists across re-captures.
_Avoid_: xno (that is its id), route version (a snapshot, not the route)

**xno**:
A route's stable identifier, equal to the capture's `node.id`; also the raw
capture filename key (`route_{xno}.json`).

**route_code**:
The rider-facing number/name ("101路(原1路)"), kept verbatim from the city.
_Avoid_: using route_code as identity — codes recycle; ids never.

**SubRoute（子路線）**:
A named variant branch of one Route — 區間車, 延駛, 繞駛, 尖峰班次; it
carries its own stop order per direction.
_Avoid_: calling a SubRoute a Route.

**Main / side route（主線/支線）**:
The declared pairing between a Route and its side Routes, captured from
`main-side-routes.json`.

**Direction（方向）**:
One direction of travel of a SubRoute: 0 = 去程, 1 = 返程 (capture `goBack`
1→0, 2→1). Direction is route-relative — it labels the route's own two
headings, never an absolute compass heading; two routes' direction-0 stops
at one pole can head opposite ways.
_Avoid_: "northbound/southbound" readings of 0/1; grouping by direction.

**Route version（route_version）**:
An immutable full-state snapshot of a Route — its SubRoutes and stop
orders at a point in time. Corrections create V(n+1); V(n) is never edited.
_Avoid_: relying on the current pointer for history.

**Version status**:
DRAFT / PUBLISHED / SUPERSEDED / CANCELLED. PUBLISHED content is immutable
forever — SUPERSEDED is bookkeeping, not permission to edit.

**Current pointer（current_route_version_id）**:
The one version consumers read; it moves only at publish and is never
historical evidence.

**valid_from / valid_to**:
A version's validity interval. "Current" means `valid_to IS NULL` — the
single definition of current, everywhere.

**Publish (pipeline)**:
The stage writing each route's first immutable version from captured data,
one atomic transaction per route.

**Draft / Publish (edit workflow)**:
The API editing lifecycle: mutations hit a mutable Draft copied from a base
version; publishing atomically mints the next version and moves the current
pointer. Same word as pipeline Publish, different lifecycle — specify which
when ambiguous.

**isCycled（循環路線）**:
Departure equals destination; the route may visit the same stop twice
within one direction.

**Provider（業者）**:
The operating bus company; a route may list several (聯營 joint operation).

**ext_uid**:
The external identity string with city prefix (`tcg:node:1270`). City
prefixes share one numeric space — parameterize before cross-city merging.

## Stops and stations

**Stop（站牌）**:
One pole-visit: a specific pole as served by one Route in one Direction,
identified per (route, direction, sid). The same physical pole on another
route is a different Stop.
_Avoid_: pole (ambiguous), "the physical stop" (that is the Station)

**sid**:
The capture's per-occurrence stop id; unique only within (route, direction).
Cross-route sid reuse is normal, not a collision.

**stop_code**:
The rider-facing stop number ("103"); may be reused over time and never
implies identity.

**Station（站位）**:
The aggregated physical stop point — nearby same-side poles clustered into
one place, globally identified by the capture's `node.id`; the same id
anywhere is the same Station.
_Avoid_: stop; node (collides with OSM node)

**StationGroup（組站位）**:
Stations grouped by the grouping rule for OD/ticketing purposes; no
direction split — one group serves both 去返程.

**Grouping rule**:
Same trimmed station name within 80 m groups, chained transitively
(A–B, B–C ⇒ one group). Different names never group, even at the same point.

**Station split**:
The registry step giving a dual-carriageway station per-direction child
stations when member headings cluster into 2+ ways (>90° gap); the parent
keeps the largest cluster. One pole can legitimately serve both directions
of a road.

**Stop bearing**:
The departure heading at a stop, degrees from north; the last stop of a
direction reports its arrival heading.

**Station axis**:
A station's mean travel axis in [0,180) — headings 180° apart fold onto one
axis, so it has no direction. **Spread** is the largest member deviation
from the axis; past 45° is a warn-only flag.
_Avoid_: drawing station axes as directional arrows.

**Regroup**:
Moving a Stop to another Station — the same version-append edit primitive
as any other edit.

## Versions, edits, staleness

**Edit-as-new-versions**:
Every edit appends a new version row (source = manual) and closes the
predecessor's valid_to; versioned rows are never UPDATEd.

**edit_log**:
The append-only audit trail, addressed by stable id; each row carries
old+new, a note, and `undo_of` for undos.

**Stable id vs version id**:
Edits and history address the stable entity (stop_id, station_id); panels,
endpoints, and URLs address the current version id (stop_version_id,
station_version_id). Never join them by coincidence of counters.

**Undo**:
The same edit primitive in reverse — closes the current manual version and
revalidates the predecessor. Undo of an undo is a forward edit again.

**Stale mark**:
A row written inside the edit transaction naming derived data that must
regenerate. A refresh clears a mark only if the data was rebuilt after that
mark's edit — never green over stale data.

**Refresh chain（--stale）**:
The dependency-ordered rebuild of marked data: match → segments →
registry. Marks accumulate across edits; one pass clears them all.

**Apply updates（套用更新）**:
The single-flight in-server refresh button — explicit, authed, one run at
a time; a second concurrent request conflicts.

**Edit mode**:
rm-web with write endpoints mounted (`--edit` + tokens). Without it the
database opens read-only and no write route exists.

## Geometry: capture, match, projection

**Capture tree**:
The directory contract a city converter must emit (providers, routes,
main-side-routes, joint operations, `raw/route_{xno}.json`). Any city
producing it imports with zero pipeline changes.

**Batch**:
One import run's staged data; everything downstream processes per batch.
OPEN accepts staging, CLOSED is consumed, FAILED aborted on parse errors.

**Staging（`src_*` tables）**:
Raw capture facts copied verbatim — never interpreted, never modified.

**Polyline**:
The drawn route geometry per direction, Google-encoded at 1e-5 precision.

**Raw line vs effective line（有效線）**:
Raw = the captured polyline. Effective = the manual override if present,
else raw — the one line every consumer (match, segments, registry, UI)
reads. Compare views show both; never mix them.

**Polyline override**:
A manual whole-direction replacement of the polyline. Whole-line only —
the piece-splice model it replaced is retired.

**Piece（path piece）**:
The cut geometry between two consecutive stops of one direction:
path-interpolated between projections, or a straight-line fallback. The
unit of pair-geometry QA.

**Chainage（chainage_m）**:
Distance along a line from its start. Two scales exist — stop chainage on
the drawn polyline and way-run chainage on the matched path — same word,
different lines; never join them.

**Projection（path_stop_projection）**:
Placing a stop on its line: chainage, snap distance, method. Rewritten per
run, so it is idempotent.

**Projection methods**:
PROJECTED (normal) / OUTLIER (snap > 50 m) / NO_PATH (no drawn line) /
ENDPOINT (line too short). Clamped = monotonicity was repaired.

**Snap（snap_dist_m）**:
Distance from a stop to its projected/matched position — the primary
stop-placement QA signal.

**Map match（Viterbi）**:
Aligning the effective line to the OSM road graph as one whole-direction
state sequence, not nearest-road-per-point.

**Candidate state**:
One directed position on one road-graph edge; two per edge, one on a oneway.

**gps_m / net_m**:
Straight-line step between two polyline points (gps_m) vs travel distance
along the network (net_m). The matcher costs their difference; the two are
never interchangeable.

**Way run**:
A contiguous travel-order run of ONE OSM way, ending where the route turns
off it — straight-through intersections do not split a run. The unit the
matcher writes.

**OSM way / node**:
The road graph's edges and intersections from the clipped extract. A node
here is road topology, not a station's `node.id`.

**Road segment**:
Matcher vocabulary for one directed node-pair edge of the graph.
_Avoid_: bare "segment" for this — it is reserved for station pairs.

**Station pair（站對）**:
A directed adjacent-station pair (A→B) served by some direction — the
segment identity today, derived solely from the two ext_uids.
_Avoid_: the retired seg_segment registry (dropped 2026-10-09).

**Corridor（走廊）**:
One true path variant among the pieces of a station pair. Siblings split
into corridors when the length gap exceeds max(100 m, 3% × shorter side);
a corridor is labeled by its median length quantized to 10 m.

**Pair spread**:
The length spread among same-pair pieces; beyond the corridor threshold =
multi-corridor pair, listed for human review.

**Matchdev**:
Directions whose matched path strays > 50 m p95 from the drawn line —
detours that snap stats cannot see.

**Out-and-back（折返）**:
A route doubling back over the same way — legitimately different way
lists, not a cut error.

**Skipped pairs**:
Directions with no drawn or unmatchable geometry — reported, never
silently dropped.

## Pipeline stages

**rm-identity**:
Decides what counts as the same thing across captures; mints Stops,
Stations, StationGroups and their V1 versions.

**rm-segments**:
Decodes polylines and projects stops (chainage, snap, pieces).

**rm-osm-prepare / rm-osm-match**:
Loads the clipped PBF into road tables; matches effective lines to way
runs (Viterbi + gap repair). Clip, never delete, to slim the graph.

**rm-registry**:
Stop/station bearings plus the dual-carriageway station split.

**rm-verify**:
Re-checks invariants and counts against per-city known-good presets;
warnings never fail the run, failed checks exit non-zero.
