# Refresh-runbook review minors — report

Branch `worktree-refresh-runbook-minors`, base da4d93b. Four commits, one per fix.
Final state: `cargo test --workspace` → **264 passed (50 suites)**.

## Commit 1 — e60c443 `rm-registry: --stale reports bearing + split counts`

- `crates/rm-registry/src/lib.rs`: new `pub struct StaleRun { bearings: bearing::Summary, split: split::Summary }` defined at the crate root next to `run_stale` (same place `run_stale` lives, so both are crate-root reachable). Signature changed to `run_stale(conn, batch_id) -> anyhow::Result<Option<StaleRun>>`: `None` on empty snapshot (same short-circuit, DB untouched), `Some(..)` carries both summaries. Internal `_b` binding became `b` and feeds the result.
- `crates/rm-registry/src/main.rs`: `--stale` now matches on the Option and prints
  `stale: {osm}+{polyline} bearings ({skipped} skipped); split: {split} split, {created} created, {moved} moved; marks cleared`; `nothing stale` kept for `None`.
- `crates/rm-registry/tests/stale.rs`: `stale_rebuilds_and_clears_marks` now `.expect()`s the run and asserts `bearings.stops_osm >= 2` and `split.stops_moved == 0` (both stops share one northbound heading → one cluster, no split) in addition to the pre-existing DB-row assertions; `stale_with_no_marks_is_false_and_touches_nothing` asserts `.is_none()`.
- Only caller was registry main.rs:35 (grep-verified); rm-segments main's `run_stale` is the segments one, untouched.

## Commit 2 — 780a386 `rm-registry: test — rebuild failure leaves marks present (zero-partial)`

- New test `rebuild_failure_leaves_marks_present` in tests/stale.rs: seeds edit_log + a `stop_bearing` mark (same inline seeding style as the existing test), then `DROP TABLE stop_bearing` so `bearing::run`'s first statement (`DELETE FROM stop_bearing`) errors. Asserts `run_stale(..).is_err()` AND `stale_mark` count still 1 — the snapshot delete never ran, so the badge stays amber over unrefreshed data instead of flashing green.
- Green immediately: current code already propagates the error via `?` before the delete. Pins the zero-partial contract per the spec's Testing bullet.

## Commit 3 — 40c99a6 `rm-registry: test — station_bearing-only mark covers gate + station rebear`

- Helper refactor: extracted `insert_run(conn, sv, entry, exit)` (parameterized entry/exit node ids) from `insert_northbound_runs`, which now delegates unchanged. Doc comment notes nodes run south → north, so (2, 4) bears ~0° and (4, 2) ~180°.
- New test `station_bearing_mark_alone_triggers_rebuild_split_and_rebear`:
  - Seeds split support inline (station_group, station_group_version, station_version for station 1, `id_sequence` station/station_version at 2) — required because `split::run` copies the parent's current station_version into children and `rm_db::mint` would otherwise mint a colliding id 1. Kept out of the shared `setup()` so the existing tests' fixture is untouched.
  - Runs: sv 11 northbound (stop 1 ~0°), sv 12 southbound (stop 2 ~180°). Seeds ONLY a `station_bearing` mark (no `stop_bearing` mark) — the new coverage; snapshot query already matches that table.
  - Asserts: summary `stops_osm >= 2` and `stops_moved == 1`; stop 2's `stop_version.station_id` (station_of-style query) moved off station 1; `station_bearing` count >= 1; and — the load-bearing gate pin — a `station_bearing` row exists for the child station with `member_count == 1`. That row can only come from `bearing::stations()` running *after* `split::run` moved the stop, because `bearing::run`'s own station pass predates the split; with the gate broken/skipped the child query returns no row and the test fails. The prompt's `>= 1` assert alone could not distinguish this (bearing::run always writes station_bearing), so the child-row assert is what actually covers the gate.
  - Marks cleared asserted.
- Green immediately, as the task anticipated (gate works; the new coverage is the mark-type and the gate pin).

## Commit 4 — 2e39528 `rm-segments: --stale propagates DB errors in the published check`

- `crates/rm-segments/src/lib.rs`: per-route published check now `.optional()?` (rusqlite::OptionalExtension, import added to the existing `use rusqlite::Connection;` line) instead of `.map(..).unwrap_or(false)`. `QueryReturnedNoRows` → `None` → not published (warn + keep mark, unchanged); real DB errors now propagate with `?` and abort `run_stale` with Err instead of silently keeping one route's mark. No existing test hits the error path (18 rm-segments tests green).

## Test runs

| Command | Result |
|---|---|
| `cargo test -p rm-registry` (after fix 1) | 26 passed (6 suites) |
| `cargo test -p rm-registry` (after fix 2) | 27 passed |
| `cargo test -p rm-registry --test stale` (after fix 3) | 4 passed (1 suite) |
| `cargo test -p rm-segments` (after fix 4) | 18 passed (4 suites) |
| `cargo test --workspace` (final) | 264 passed (50 suites, 3.17s) |

Plain (no --release), no cargo fmt, per constraints.

## Self-review of `git diff da4d93b`

- 4 files, +158/−22; every changed line traces to one of the four fixes. No unrelated edits, no formatting churn.
- `StaleRun` derives nothing — only consumed by main's println and test `{:?}` (Debug needed → Summary already derives Debug; verified compile). No Clone/Default speculation.
- main.rs format string is a long single line (>100 cols); no cargo fmt allowed, left as is — consistent with the neighboring batch println which is also long.
- Fix 2's failure mode is exercised at the earliest possible statement (`DELETE FROM stop_bearing`), so partial-write inside `bearing::run`'s transaction is not exercised — but the pin target (marks survive a failed refresh) is exactly what the delete-guard contract needs.
- Fix 3: the split fixture seeds were verified against schema (station_version.station_group_id NOT NULL + FK; mint/id_sequence collision) by mirroring tests/split.rs's setup. The southbound geometry needed no special handling — tests/bearing.rs already proves entry/exit swap resolves ~180° on this exact way.
- Nits left alone deliberately: test name `stale_with_no_marks_is_false_and_touches_nothing` still says "is_false" while asserting `is_none()` (None is the new false; rename was not requested and the doc meaning holds).

## Fix round 1 — reviewer verdict: fixes 1-3 approved; fix 4 regression repaired

**Finding (correct):** in the 2e39528 version of the published check, the row mapper `|r| r.get::<_, Option<i64>>(0)` returns `Ok(None)` for a row whose `current_route_version_id IS NULL` (the normal unpublished lifecycle state — NULL until rm-publish sets it, migrations/0001_init.sql). `.optional()` wrapped that to `Ok(Some(None))` and `.is_some()` read it as PUBLISHED. Consequence: a stale mark on an unpublished route was queued for clear while `run_inner`'s `current_route_version_id IS NOT NULL` filter never recomputes it — stale signal erased with no recompute.

**Commit b1b77d9 `rm-segments: --stale — NULL current_route_version_id is not published (flatten)`:**

- Repair (crates/rm-segments/src/lib.rs): `.optional()?` now flattens before the check — `.optional()?.flatten().is_some()`. Row absent → `None` → false; row with NULL → `Some(None)` → flatten → `None` → false; row with id → `Some(Some(id))` → true. The `?` error propagation from the original fix is kept.
- Pin test (crates/rm-segments/tests/segments.rs): `stale_keeps_marks_for_unpublished_routes` — setup, `run()`, `UPDATE route SET current_route_version_id = NULL WHERE route_id = 2`, `mark(.., "2", 1)`; asserts `out.cleared == 0`, `out.kept == 1`, `stale_mark` count still 1.

**RED/GREEN evidence:**

- RED: `cargo test -p rm-segments --test segments stale_keeps_marks_for_unpublished` against 2e39528 → FAILED: `assertion left == right failed: left: 1, right: 0` at segments.rs:263 — `out.cleared` was 1 (mark cleared for the unpublished route), exactly the reported regression. Full log: `$HOME/Library/Application Support/rtk/tee/1791629243_cargo_test.log`.
- GREEN after repair: `cargo test -p rm-segments` → 19 passed (4 suites); `cargo test --workspace` → 265 passed (50 suites, 3.09s).

**Final branch state:** 5 commits on da4d93b (e60c443, 780a386, 40c99a6, 2e39528, b1b77d9); `git diff da4d93b` = 4 source/test files + this report, all lines traceable to the four fixes plus this repair.
