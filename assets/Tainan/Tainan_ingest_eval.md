# Tainan ingest evaluation (2026-10-05 capture)

Verdict: **可以轉入** — via a Tainan→Taichung-shape converter script plus three
small changes, none structural. The pipeline's staging schema needs no new
tables; every Tainan field maps onto an existing one.

> **Executed 2026-10-05** (this plan, verbatim): `tools/organize_tainan.py`
> builds `capture/` (113 routes — 小黃巴士 52 excluded by type name; their
> pathList typeid is 71-76, **not** the 7 that `pathTypeList` implies),
> pipeline ran against `data/tainan.db` (independent DB, `tcg:` prefix kept)
> with `data/TW-TNN-202610040000.osm.pbf`, and `rm-verify --preset tainan`
> passes all checks. Results: 2,295 stations (name+80 m clusters, `T%06d`
> synthetic ids), 8,707 stops, 225/226 directions matched (miss =
> 1805 dir 1, no return geometry), 4,990 segments, 1,696 shared by >1 route,
> snap p50 3.1 m. One code change was needed beyond the converter:
> `rm-verify` expectations and the `seg:shared-pair` check were Taichung-
> hardcoded — now `Expectations::TAINAN` + `--preset tainan` (101/102 share
> 28 segments as the sanity pair). 74 stations warn on bearing spread >45°
> (warn-only; 3.2% vs Taichung's 0.5%).

## What was captured

`tools/fetch_tainan.sh` (165 routes, 3.6 MB, ~500 requests):

| File | Source op | Rows |
|---|---|---|
| `pathList.json` | `PathListV2.ashx` | 165 route headers |
| `raw/route_{id}.json` | `CrossRoutesByPathName.ashx` | 165 files, stop lists per direction |
| `raw/geom_{id}_{go,back}.json` | `ReadText.ashx` | 329 valid + 1 expected miss (`1805_back`, single-direction route) |

Route types: 市區公車 27, 小黃巴士 52, 綠 20 / 黃 16 / 橘 14 / 藍 12 / 棕 8 / 紅 7, 觀光 7, 雙層 1, 高鐵接駁 1.

## Data quality findings

1. **`stopid` is route-scoped per-occurrence, NOT a global station id** —
   corrects `Tainan_API.md`. Across all 11,379 stop rows, zero reuse: the same
   physical pole in two directions of one route gets two ids (route 1129 煙子林:
   dir0=24941, dir1=25013, identical coords). `ajax_routeinfo.jsp`'s `id` is
   the same space. Consequence: `stopid` is exactly the spec's **Stop** (pole
   occurrence) — a better fit than Taichung's `sid` (6,107 shared across
   routes) — but a **Station key must be derived**; there is no `node.id`
   equivalent anywhere in the API.
2. **No bad coordinates.** The only 2 rows outside the Tainan bbox are route
   9111 (高鐵接駁) terminating at 高雄國際航空站 — real points.
3. **Station clustering must be name + ≤80 m proximity, not coords-equal.**
   1,305 stop names serve >1 route; only 76 of those have byte-identical
   coords, 1,024 stay within 50 m, and names like 公館 span 51 km (different
   villages, same name). Same-name-<80 m transitive chaining — the rule
   `rm-identity/grouping.rs` already applies — is the right derivation.
4. **Geometry**: median 144 points/direction. 29 of 330 direction geometries
   start >150 m from the direction's first stop (worst `1032_back` 14.9 km) —
   same class of quirk as Taichung's route 258 tail; snap-distance QA will
   surface them, not break ingestion.
5. 6 single-direction routes, 4 circular (Dept==Dest). Loop patterns exist:
   route 1129 revisits 煙子林/平溪二號橋 within one direction (seq 44/48) —
   legal with per-occurrence stopids; `stop_sequence` stays unique.

## Field mapping (Tainan → Taichung capture shape → staging)

| Staging (rm-ingest) | Taichung source | Tainan source | Transform |
|---|---|---|---|
| `src_route_header.xno` | `routes.edges[].node.id` | `pathList[].id` | as-is (both numeric strings) |
| `route_code` | `node.name` | `pathList[].name` | optional strip `路(原…)` → `101` |
| `departure`/`destination` | `node.departure/destination` | `Dept`/`Dest` | as-is |
| `seq` | `node.seq` | `sequence` | as-is |
| `is_cycled` | `node.isCycled` | — | derive `Dept == Dest` (4 routes) |
| `op_type` | `node.opType` | `typeid` | 1:1 numeric pass-through |
| `src_provider` / `src_route_provider` | `providers.json` | — | **empty** (no operator API) |
| `src_stop_edge.node_id` | `node.id` (global) | — | **synthesize**: cluster stops by name+80 m (grouping.rs rule) offline, mint `T00001…` per cluster |
| `station_name/lat/lon` | `node.*` | first occurrence in cluster | — |
| `src_stop_edge.sid` | `sid` (shared pole) | `stopid` | as-is — already per-occurrence |
| `direction` | `goBack` 1→0, 2→1 | `goback` 0→0, 1→1 | flag values differ, both normalize |
| `source_order_no` | `orderNo` | array index (1-based) | no explicit seq in payload |
| `ivr_no` | `ivrNo` | — | null (fixture already exercises null) |
| `src_route_point.encoded` | `routePoint.go/back` (Google polyline) | `geom_{id}_{go,back}` point list | encode points → Google polyline (`rm_core::polyline` inverse) |
| `src_main_side_route` | Strapi `main-side-routes` | — | **empty**; variants exist only as `subRouteId` in the live ETA endpoint, not in static data |
| `src_joint_operation` | Strapi `jointOperationsBuses` | — | empty |

Timetables: JSON endpoint is dead (HTML scrape only). Out of scope anyway —
the pipeline ingests no timetables today.

## Required changes (3, none structural)

1. **Converter script** `tools/organize_tainan.py` (python, mirrors
   `organize_taichung.py`): builds a Taichung-shape tree
   (`routes.json`, `providers.json` with empty edges, `main-side-routes.json`
   `[]`, `jointOperationsBuses.json` empty, `raw/route_{id}.json` with relay
   wrappers, synthetic `node.id`, encoded polylines) → `rm-ingest` runs
   unmodified. The offline name+80 m clustering + synthetic station ids live
   here, keeping Rust untouched.
2. **ext_uid city prefix** — `rm-identity` hardcodes `tcg:{xno}` /
   `tcg:node:{id}` (`lib.rs:395-418`). Tainan ids (2101…) and Taichung ids
   share one numeric space → same-DB collision. Either parametrize the prefix
   per import source (`tn:`) — small, touches `route_ext_uid`,
   `station_ext_uid`, `stop_ext_uid` + call sites — **or** run Tainan as its
   own DB file (the pipeline already produces one SQLite per run; `rm-web`
   serves one DB). Separate DB = zero Rust change today; prefix parametrization
   is the price of a shared DB later.
3. **OSM extract `TW-TNN`** for `rm-osm-prepare`/`rm-osm-match` (Taichung used
   `TW-TXG-202610020300.osm.pbf`). Standard osmium polygon extract; without it,
   stops fall back to polyline bearings only (bearing.rs handles this, QA
   degrades).

## Decisions to make

- **小黃巴士 (52 routes, demand-responsive)**: flexible/virtual stops may
  pollute the segment registry. Recommend ingesting but filtering by
  `typeid == 7` in the converter, or a first pass excluding them.
- **route_code display**: keep `101路(原1路)` verbatim or strip to `101`.
- Same-DB (prefix change) vs per-city DB.

## Effort estimate

Converter + prefix parametrization + one ingest/identity/verify run: roughly a
day including QA review of the far-start geometries and 小黃巴士 stops. OSM
extract + match/registry runtime is compute, not code.
