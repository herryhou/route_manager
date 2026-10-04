# Taichung City Bus — normalized dataset

Built from `assets/Taichung/raw/route_*.json` (392 routes, captured 2026-10-02) plus
`routes.json`, `providers.json`, `main-side-routes.json`. Regenerate with
`tools/fetch_taichung.sh` then `tools/organize_taichung.py`.

## Files

| File | Content |
|---|---|
| `routes.json` | route headers + provider links (from `Routes` + `route` queries) |
| `providers.json` | operator directory |
| `stations.json` | unique physical stations (`node.id` key), with serving routes |
| `route_stop.jsonl` | one row per stop-in-direction occurrence — the RouteStop table |
| `polylines.json` | encoded path per route per direction |
| `subroute_links.json` | main route → side routes (from Strapi `main-side-routes`) |
| `station_groups.json` | **generated** StationGroup candidates (see below) |
| `_stats.json` | capture statistics |

## Entity mapping (API → Architecture_design_spec)

| Spec entity | Source field | Notes |
|---|---|---|
| Route | `routes.id` | `route_code` = rider-facing `name` |
| SubRoute | `subroute_links.children` | side/detour routes are **separate route ids** in this API; grouping is a soft link, not an aggregate |
| RouteStop row | one `stations.edges[]` entry | keyed by (route, direction, stop_sequence) |
| direction | `goBack`: 1→`0` 去程, 2→`1` 返程 | spec numbering |
| stop_sequence | `orderNo` | gaps exist (non-contiguous) — keep raw values |
| Stop (站牌) | `sid` | ⚠ NOT route-scoped: 6,107 sids are shared by >1 route, some by both directions — it is a physical pole/sign id |
| Station (站位) | `node.id` | 7,424 unique; name+coords stable per id (no alt names found) |
| StationGroup | generated `G####` | derived, not from API (rule below) |

## Key findings

1. **`sid` is a pole id, not a route+direction id.** 37,294 stop-of-route rows collapse
   to 14,045 unique sids. In spec terms one physical pole hosts several routes'
   RouteStop rows; a spec-conformant `stop_id` would be per (route, direction,
   sequence) with `sid` kept as an external pole reference.
2. **`node.id` over-splits stations.** 3,542 names map to >1 `node.id`;
   3,437 of those pairs sit <80 m apart (e.g. 干城站 ids `44`+`46`, 28 m apart).
   Only 337 same-name pairs are genuinely different places — same name is common
   across districts (e.g. two 不同 行政區 的「中山路口」).
3. **90% of stations cluster by (name, ≤80 m).** 6,703 of 7,424 stations fall into
   3,326 same-name clusters — good first-pass StationGroups.

## station_groups.json derivation rule

Union-find over stations sharing identical `name`, chained when pairwise <80 m.
Ids `G####` are generated at build time, not stable identities — treat as
candidates; merging decisions belong to the data pipeline (spec §6.3).

## Capture stats

```json
{
  "routes": 392,
  "stations_unique": 7424,
  "route_stop_rows": 37294,
  "stops_unique_sid": 14045,
  "routes_without_polyline": 0,
  "providers": 39,
  "subroute_link_groups": 100
}
```

Busiest stations: 干城站 (`44`, 68 routes), 臺中車站(民族路口) (`6226`, 57),
彰化銀行(臺灣大道) (`464`, 57), 干城站 (`46`, 57), 中友百貨 (`340`, 57).

## Not captured

Timetables (`dailyTimeTable` per route per date), joint-operation table already in
parent dir as `jointOperationsBuses.json`. Timetable bulk download is ~392 more
queries per day of interest — run `tools/fetch_taichung.sh` variant if needed.
