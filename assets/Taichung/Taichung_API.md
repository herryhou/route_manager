# Taichung City Bus eBus API

Taichung city bus open data (臺中市公車動態). Three endpoints, six operations.

## Endpoints

| # | URL | Type |
|---|-----|------|
| 1 | `https://citybus.taichung.gov.tw/ebus/graphql` | GraphQL — core data (routes, stops, providers, timetables) |
| 2 | `https://citybus.taichung.gov.tw/ebus/strapi/graphql` | GraphQL — Strapi-managed data (joint operations) |
| 3 | `https://citybus.taichung.gov.tw/ebus/strapi/main-side-routes` | REST — main/side route mapping |

All calls are plain POSTs with `content-type: application/json`. No auth observed.

## Operations at a glance

| Operation | Endpoint | Args | Returns | Sample file |
|---|---|---|---|---|
| `providers` | 1 | `lang` | bus operators | `providers.json` (39 rows) |
| `Routes` | 1 | `lang` | all route headers | `routes.json` (392 routes) |
| `route` | 1 | `xno`, `lang` | one route: stops + polylines + operators | `route_3.json` (132 stops), `route_1010.json` (13 stops) |
| `dailyTimeTable` | 1 | `xno`, `date` | departure entries for one route/day | `dailyTimeTable_1010.json` (12 entries) |
| `jointOperationsBuses` | 2 | — | routes run by multiple operators | `jointOperationsBuses.json` (73 rows) |
| main-side-routes | 3 | — | main route → side routes mapping | `main-side-routes.json` (100 rows) |

Sample files live next to this doc in `assets/Taichung/`. Numbers above are counts in those samples, not API guarantees.

## Shared conventions

**`lang`** — `"zh"` observed; presumably `"en"` also works (unverified).

**`goBack`** — direction flag used by `route` and `dailyTimeTable`:

| Value | Meaning |
|---|---|
| `1` | 去程 outbound (departure → destination) |
| `2` | 返程 return |

**`opType`** — operator/route category. Observed values (names in samples):

| Value | Meaning (inferred from data) |
|---|---|
| `0` | city bus / general operator (364 of 392 routes) |
| `1` | highway operator (公路客運 — names carry `(公總)` suffix, e.g. 台中客運(公總)) |
| `9` | unknown category (28 of 392 routes) |
| `11` | Taichung MRT corp (臺中捷運公司) |
| `8101`–`8103` | taxi fleets (飛狗/怡美/台灣大車隊) — demand-responsive service |

**Relay-style wrapping** — endpoint 1 returns everything as `data.<field>.edges[].node` (GraphQL relay connection) except `dailyTimeTable` which is also `edges[].node`. Endpoint 2/3 return plain objects/arrays.

---

## 1. `providers` — operator directory

```bash
curl --url 'https://citybus.taichung.gov.tw/ebus/graphql' \
  -H 'content-type: application/json' \
  --data-raw '{"query":"{ providers(lang: \"zh\") { edges { node { id name telephone opType } } } }"}'
```

Response shape:

```text
data.providers
└── edges[]
    └── node                       # one operator
        ├── id        string       # operator id, referenced as providerId elsewhere
        ├── name      string       # operator name (zh)
        ├── telephone string       # contact, may be empty
        └── opType    int          # see conventions
```

Sample: `providers.json` — 39 operators, e.g. `1` 台中客運, `3` 統聯客運, `15` 中台灣客運, `6` 豐原客運.

## 2. `Routes` — all route headers

```bash
curl --url 'https://citybus.taichung.gov.tw/ebus/graphql' \
  -H 'content-type: application/json' \
  --data-raw '{"query":"query Routes($lang: String!) { routes(lang: $lang) { edges { node { isCycled id name seq departure description destination opType providers { edges { node { id name telephone } } } } } } }","variables":{"lang":"zh"}}'
```

Response shape:

```text
data.routes
└── edges[]
    └── node                          # one route header
        ├── id          string        # route id (string form of xno)
        ├── name        string        # route number shown to riders, e.g. "3"
        ├── seq         int           # sort order
        ├── departure   string        # end stop name (zh)
        ├── destination string        # other end stop name (zh)
        ├── description string        # "{departure} - {destination}"
        ├── isCycled    bool          # true = circular route (循環線); 44/392 in sample
        ├── opType      int           # see conventions
        └── providers
            └── edges[]               # operators running the route (can be several)
                └── node
                    ├── id        string   # operator id → providers.id
                    ├── name      string   # operator name (zh)
                    └── telephone string   # contact
```

Sample: `routes.json` — 392 routes. This is the entry point: list route → take `id` → query `route` and `dailyTimeTable` with it.

## 3. `route` — stop list + geometry for one route

```bash
curl --url 'https://citybus.taichung.gov.tw/ebus/graphql' \
  -H 'content-type: application/json' \
  --data-raw '{"query":"{ route(xno: 3, lang: \"zh\") { stations { edges { goBack ivrNo orderNo sid node { id lat lon name } } } routePoint { go back } providers { edges { node { id name opType } } } } }"}'
```

Args: `xno` = route number (int), `lang`.

Response shape (one entry per stop, both directions merged — filter by `goBack`, sort by `orderNo`):

```text
data.route
├── stations
│   └── edges[]                        # one entry per stop-in-direction
│       ├── goBack        int          # 1 = outbound, 2 = return (see conventions)
│       ├── orderNo       int          # stop sequence within that direction
│       ├── ivrNo         int          # stop number for phone/IVR announcements
│       ├── sid           int          # physical pole/sign id — shared across routes (and sometimes directions); NOT route-scoped
│       └── node          object       # the physical station
│           ├── id        string       # GLOBAL station id — shared across routes
│           ├── name      string       # stop name (zh)
│           ├── lat       float        # WGS84
│           └── lon       float        # WGS84
├── routePoint
│   ├── go               string        # encoded polyline (Google algorithm, appears to be), outbound path
│   └── back             string        # encoded polyline, return path
└── providers
    └── edges[]
        └── node
            ├── id        string       # operator id → providers.id
            ├── name      string       # operator name (zh)
            └── opType    int          # see conventions
```

Id-space warning: `sid` ≠ `node.id` — `sid` is per route+direction, `node.id` is the global stop key (dedupe on it).

Samples: `route_3.json` (route 3: 132 stops, both directions), `route_1010.json` (route 1010 shuttle: 13 stops).

## 4. `dailyTimeTable` — departures for one route/day

```bash
curl --url 'https://citybus.taichung.gov.tw/ebus/graphql' \
  -H 'content-type: application/json' \
  --data-raw '{"query":"{ dailyTimeTable(xno: 1010, date: \"2026-10-01\") { edges { node { xno id goBack scheduleTime carType carId providerId } } } }"}'
```

Args: `xno` = route number (int), `date` = `YYYY-MM-DD`.

Response shape (one entry per scheduled departure):

```text
data.dailyTimeTable
└── edges[]
    └── node                        # one scheduled departure
        ├── id            string    # departure entry id
        ├── xno           int       # route number (echo)
        ├── goBack        int       # direction (see conventions)
        ├── scheduleTime  string    # "HH:MM" departure time
        ├── carType       string    # vehicle class, e.g. "lfv_2" (lfv ≈ low-floor, inferred)
        ├── carId         string    # plate number, e.g. "FAB-337"
        └── providerId    string    # operator id → providers.id
```

Sample: `dailyTimeTable_1010.json` — 12 entries, all `goBack: 1` (route 1010 sample has no return entries that day).

## 5. `jointOperationsBuses` — jointly operated routes

Endpoint 2 (Strapi GraphQL):

```bash
curl --url 'https://citybus.taichung.gov.tw/ebus/strapi/graphql' \
  -H 'content-type: application/json' \
  --data-raw '{"query":"{ jointOperationsBuses { busProvider providerId routeId } }"}'
```

Response shape (plain array, no relay wrapper):

```text
data.jointOperationsBuses[]         # one entry per jointly-run route
├── busProvider   string            # slug: "Ubus" = 統聯客運 (id 3), "CTbus" = 中台灣客運 (id 15)
├── providerId    int               # operator id → providers.id
└── routeId       int               # route number → Routes.id / xno
```

Sample: `jointOperationsBuses.json` — 73 rows (Ubus 34, CTbus 39).

## 6. main-side-routes — main route → side routes

Endpoint 3 (REST, GET):

```bash
curl --url 'https://citybus.taichung.gov.tw/ebus/strapi/main-side-routes' \
  -H 'content-type: application/json'
```

Response shape (bare JSON array, Mongo/Strapi documents):

```text
[]                                 # one document per main route
├── xno          int               # main route number
├── sideRoutes   string            # comma-separated side route numbers, e.g. "932,933,1051,1052"
├── children     int[]             # same as sideRoutes, already parsed
├── _id / id     string            # Mongo document id
├── createdAt    string            # ISO timestamp
├── updatedAt    string            # ISO timestamp
└── __v          int               # Mongo version key, ignore
```

Sample: `main-side-routes.json` — 100 mappings, e.g. `105 → [932, 933, 1051, 1052]`.

---

## Data quirks observed in samples

- `routes.json` ids are strings (`"3"`); `xno`/`routeId` args are ints — convert at the boundary.
- Same physical stop appears twice in `route` output (once per direction), same `node.id` — dedupe on `node.id` when building a stop master list.
- `node.id` and `sid` are different id spaces; `providers.id`/`providerId` is a third. Bulk capture shows `sid` is a pole id reused across routes (6,107 of 14,045 sids serve >1 route) — see `data/README.md`.
- `main-side-routes` has no GraphQL layer and its own Mongo ids.
- Polylines in `routePoint` are opaque encoded strings; decode only if map rendering needed.
