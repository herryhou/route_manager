# rm-verify

Final stage of the pipeline: re-check the database every earlier stage
built. It runs 28 checks — count cross-checks against known-good ledger
numbers plus schema invariants — writes the QA exports, and exits
non-zero if any check fails. Read-only against the database; its only
writes are the QA files.

## Input → output

Verifies one batch over every layer, staging `src_*` tables through
`seg_*`. Flags: `--db` (default `data/taichung.db`), `--batch`
(default: latest OPEN batch; an error if none is OPEN and unset),
`--qa-dir` (default `qa`). Writes five files there:

| File | Contents |
|---|---|
| `report.json` | the whole report: each check (id, pass, detail), warnings, counts, stats |
| `segments.geojson` | one `LineString` per `seg_segment` with `segment_id`, `length_m`, `way_count`, `routes` |
| `outlier_stops.csv` | `path_stop_projection` rows snapped > 50 m or method `OUTLIER`, worst first |
| `bearing_report.json` | stations over the 45° spread threshold with member bearings, plus the OSM/POLYLINE source mix |
| `match_dev_report.json` | directions whose matched OSM path strays > 50 m (p95) from the src polyline, worst first, with matched ratio / gap repairs |
| `grouping_report.json` | groups over the chain-drift thresholds with their member stations |

## What gets checked

With `Expectations::REAL` (all 11 count targets set) `run()` emits
28 checks:

- **Count cross-checks (11)** — routes, stations, `stop_rows`, `sids`,
  stops, groups, providers, `cycled`, `single_dir`, `no_polyline`,
  `main_side_groups` against values verified from the real captures
  (392 routes, 7,424 stations, …; the full ledger lives in the
  `Expectations::REAL` constant). Tests inject their own `Expectations`
  over a small fixture; a `None` field skips that check.
- **Versioning invariants (INV-01…18 from the spec, some checks covering
  several)** — closed version intervals never referenced as current;
  `id_sequence` ahead of every table max so ids are never reused; route
  `ext_uid` unique; no duplicate `(route_id, valid_from)`; at most one
  PUBLISHED `route_version` per route; every route's current pointer
  resolves to a PUBLISHED version with subroutes and stops (publish
  atomicity); subroute/stop/station versions never dangle outside their
  parent; trip/report tables present and empty (out of V1 scope).
- **Scope statements as always-pass checks** — single-writer pipeline
  (INV-12), `stop_code` is not identity (INV-15), GTFS export out of V1
  scope (INV-18). They record the decision, they can't fail.
- **Sequence contiguity** — per `(subroute_version, direction)`,
  `stop_sequence` runs 1..N with no gaps or repeats.
- **Station grouping (station/stop 整合)** — rerunning the grouping
  algorithm (`rm-identity::group_stations`) over the current
  `station_version` rows must reproduce the stored `station_group`
  membership exactly; every station and group has exactly one current
  version. Both fail the run when tampered with.
- **Segment layer** — every `seg_segment` has at least one way and
  positive length; routes `tcg:1` and `tcg:9221` share at least one
  segment (the built-in corridor sanity pair). If no traversals exist
  yet — matcher/registry not run — both pass with a note instead.

Matcher numbers (matched/unmatched directions, snap-distance p50,
directions with p95 over 100 m, gap repairs, bottom-decile match ratio)
land in `report.json` stats: informational, never checks.

## Fail vs warn

A failed check makes `main` print `FAIL <id> <detail>` per check and
exit non-zero with the failure count; warnings print as `WARN` lines.
Some classes are deliberately exempt:

- **Bearing spread** — a station whose member stops deviate more than
  `BEARING_SPREAD_WARN_DEG` (45°) from its axis becomes a warning in
  `report.json` and a row in `bearing_report.json` — the exit status is
  unchanged, because reverse travel on the same road (one axis ± 180°) is
  legitimate and headings fold onto the axis before comparing.
- **Matched-path deviation** — a direction whose matched OSM path strays
  more than `DEV_P95_WARN_M` (50 m) p95 from the src polyline warns and
  lands in `match_dev_report.json` (`match:dev-p95`). The snap stats
  measure polyline→path and stay clean when the source polyline is
  heavily simplified, so this path→polyline side catches detours through
  side streets; source polyline quality is a data issue, so it never
  fails the run. rm-web's "path dev" chip lists the same rows live for
  eyeballing ways-vs-raw on the map.
- **Chain drift** — grouping unions same-name stations at < 80 m hops,
  so a long chain can drift past any real interchange. A group over
  `GROUP_DIAMETER_WARN_M` (300 m) diameter or `GROUP_MEMBERS_WARN`
  (10) members warns and lands in `grouping_report.json`; the partition
  itself is still legal, so it never fails the run.
- **sid ambiguity / name hygiene** — a `sid` mapping to more than one
  station node (identity resolves it first-occurrence-wins and warns at
  mint time) and current station names carrying surrounding whitespace
  are surfaced as warnings for manual review.

## Library shape and tests

The engine is one public function, `run()` in `src/lib.rs`, over a
`rusqlite` connection plus an `Expectations` struct — so
`tests/verify.rs` drives ingest → identity → publish over an in-memory
fixture and asserts: a healthy fixture passes and writes all exports; a
wrong count expectation fails the right check; a sequence gap is
caught; the shared-pair check joins through `route_version` (misaligned
ids must not misattribute routes); bearing spread warns without
failing; tight spread does not warn; tampered group membership fails
the recompute check; a station without a current version fails the
current-unique check; chain drift, sid ambiguity and untrimmed names
warn without failing.

## Why this stage exists

Every stage prints its own summary and moves on; nothing else notices
when one quietly under-imports or leaves a dangling reference. This is
the one place the whole chain's output is compared against the ledger
and against itself in a single pass, and its exports are what a human
opens to eyeball where the geometry went wrong — the map of segments,
the list of badly-placed stops, the stations whose headings disagree.
