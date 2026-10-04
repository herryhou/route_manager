# Bearing validation — check report

**Date:** 2026-10-04 · **Batch:** 1 · **DB:** `data/taichung.db` · **Rule commit:** `4707b98`
**Run:** all verify checks PASS; bearing check is warn-only and never fails the run.

## What is checked

Each stop gets a departure heading (bearing, degrees from north); each
station gets one mean travel axis. The check flags stations whose member
stops disagree with the station axis by more than 45°
(`BEARING_SPREAD_WARN_DEG`), because all stops boarding at one physical
pole should leave along the same road (either direction of it).

Two uses:

1. **Map UI arrows** — `rm-web` draws stop/station direction arrows from
   `stop_bearing` / `station_bearing`.
2. **QA rule** — a stop far off its station's axis usually means a bad
   stop placement or a bad way match, not a real road.

### Data model (migration `0005_bearing.sql`)

| Table | Grain | Key columns |
|---|---|---|
| `stop_bearing` | one row per stop version | `bearing_deg`, `source` (`OSM`/`POLYLINE`) |
| `station_bearing` | one row per station | `bearing_deg` (axis, [0, 180)), `member_count`, `max_spread_deg` |

Both are derived tables, rebuilt (DELETE + INSERT) on every
`rm-registry` run — immutability rules (INV-02) keep derived data out of
the version tables.

### How bearings are computed (`rm-registry::bearing`)

- **OSM first:** take the matched way run (`seg_match_way`) covering (or
  nearest to) the stop's chainage, slice the way nodes entry→exit in
  travel order, and take the heading of the segment nearest the pole.
  The final stop of a direction reports its arrival heading on the last
  way.
- **Polyline fallback:** when no run matches, the forward tangent of the
  route's drawn polyline (`src_route_point`) at the stop's chainage.
- **Station axis:** axial mean of member bearings — the circular mean of
  doubled angles, halved (stable for bidirectional sets where a plain
  circular mean flips perpendicular). `max_spread_deg` = the largest
  member deviation from that axis, each deviation measured modulo the
  reverse direction: `min(adiff(b, mean), adiff(b, mean + 180°))`.

Coverage this run: **37,294 stop visits → 37,292 stop-bearing rows
(28,933 OSM + 8,359 POLYLINE), 0 skipped; 7,424 stations.**

## Rule evolution

| Version | Rule | Flagged | Problem |
|---|---|---|---|
| v1 | all pairwise member spread > 45° | 1,808 | opposite-direction pairs at the same pole are legitimate, not violations |
| v1 analysis | of 1,249 non-opposite-pair: 559 opposite pairs, 522 multi-cluster intersections, 727 single-outlier candidates | — | two distinct false-positive classes |
| v2 | same-direction groups only (cross-direction exempt) | 1,553 | `direction` (0/1) is **route-relative** (each route's own 去程/返程), not an absolute heading — 517 of 1,553 were ~180° *same-direction* pairs |
| **v3 (current)** | **axis rule**: axial mean + deviation modulo 180°, warn > 45° | **931** | intersection poles still flag (accepted, see below) |

Why v2 failed: two routes' direction-0 stops at one pole can head
opposite ways (route A runs north on its 去程, route B south on its
去程). The field labels the route's own outbound/return, so it cannot
group physical headings.

Why the axis rule is right: the legitimate variation at one pole is "one
road, two directions" — headings 180° apart. Folding bearings modulo
180° compares what should be compared: is every member on the same road
axis?

## Current results (v3)

**931 of 7,424 stations flagged (12.5%).** Warn check id:
`bearing:station-spread` (`report.json` warnings; detail in
`qa/bearing_report.json`).

### By spread magnitude

| max_spread_deg | stations |
|---|---|
| 45–50° | 89 |
| 50–60° | 182 |
| 60–90° | 656 |
| ≈90° | 4 |
| >90° | 0 (impossible — folded deviation caps at 90°) |

### By structure (cluster analysis, 22.5° tolerance)

| Class | Stations | Meaning |
|---|---|---|
| Single outlier | 445 | one lone member off an axis corroborated by ≥2 others — the genuine divergence signal (bad stop placement / bad match) |
| True multi-axis intersection | 318 | every axis has ≥2 members (crossroads: e.g. `{85°, 86°}` + `{175°}`) — geometrically real, not a data error |
| Wide / scatter | 168 | no dominant axis at all |

Flagged stations have 3–57 members (median 8), so these are busy poles
where a single wrong member matters most.

### Examples

- `tcg:node:2623` — 14 members on one axis (109.8–115.8° plus one
  reverse at 295.8°, correctly folded) and **one member at 24.5°**, ~90°
  off. Classic single outlier: likely a misplaced stop or a bad match.
- `tcg:node:1270` — 6 members at ~180° (N-S road) + one at 270.2°
  (westbound). Either one E-W route genuinely uses this pole or one bad
  member.
- `tcg:node:1603` — `{84.9°, 86.1°}` + `{175.5°}`: two corroborated
  axes 90° apart — a corner pole, not an error.

## Interpretation

- The 445 single-outlier stations are the actionable set. Spot-check
  their lone member's `snap_dist_m` / match quality before trusting its
  bearing.
- The 318 intersection stations are expected noise under a one-axis
  rule. A pole legitimately can sit at a crossroads where routes diverge
  90°; folding modulo 180° handles reverse travel but not perpendicular
  travel.
- The 168 wide/scatter stations deserve a manual look — some may be
  poles whose members come almost entirely from polyline fallback
  (unmatched directions), where the drawn polyline quality sets the
  bearing.

## Known limits / possible next refinement

1. **No multi-axis exemption.** Exempting axis clusters with ≥2
   corroborated members each would drop flags from 931 to roughly the
   445 + 168 set. Cost: two new heuristic knobs (cluster tolerance,
   minimum cluster size). Not implemented.
2. **Route-relative `direction`** in `bearing_report.json` members is
   kept for eyeballing only; it is not used by the rule.
3. **`station_bearing.bearing_deg` is now an axis in [0, 180°)** (was a
   circular mean in [0, 360°)). Consumers drawing station arrows —
   `rm-web` — should re-merge master: for bidirectional poles the arrow
   now shows the road axis instead of a meaningless perpendicular mean.

## Reproduce

```sh
cargo run -r -p rm-registry   # rebuilds stop_bearing / station_bearing
cargo run -r -p rm-verify     # warn + writes qa/bearing_report.json
```

`qa/bearing_report.json` schema:
`{threshold_deg, source_mix: {OSM, POLYLINE}, violating_stations:
[{station_id, ext_uid, bearing_deg (axis), max_spread_deg, members:
[{stop, direction, bearing_deg, source}]}]}` — stations ordered by
spread descending, members ordered by direction then bearing.
