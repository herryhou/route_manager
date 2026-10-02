# Tainan City Bus API (大台南公車 2384)

Reverse-engineered from https://2384.tainan.gov.tw/NewTNBusWeb/ front-end JS. Plain HTTP JSON — no auth, no GraphQL. Unlike Taichung there is no single clean API; calls are split across three servers.

## Endpoints

| # | Base URL | Type |
|---|----------|------|
| 1 | `https://2384.tainan.gov.tw/NewTNBusAPI_V2/API/` | legacy `.ashx` JSON |
| 2 | `https://2384.tainan.gov.tw/IMP/jsp/rwd_api/` | newer `.jsp` JSON (live data) |
| 3 | `https://2384.tainan.gov.tw/NewTNBusWeb/APIs/` | vehicle positions |
| 4 | `https://busmap.tainan.gov.tw/ebus/` | server-rendered HTML (timetable) |

All probed endpoints accept plain GET with query params. The site's own AJAX helper always adds `prj=tn` — include it to be safe.

## Operations at a glance

| Operation | Endpoint | Args | Returns | Sample file |
|---|---|---|---|---|
| `GetPathTypeListV2.ashx` | 1 | `Lang` | route categories | `pathTypeList.json` (13 rows) |
| `PathListV2.ashx` | 1 | `Lang` | all route headers | `pathList.json` (165 routes) |
| `CrossRoutesByPathName.ashx` | 1 | `id` | route stops, both directions + first/last bus | `route_2101_stops.json` (72+73 stops) |
| `ReadText.ashx` | 3 | `Path={id}_go/_back` | route path point list per direction | `route_2101_go.json` (216 pts), `route_2101_back.json` (192 pts) |
| `ajax_routeinfo.jsp` | 2 | `prj`, `id` | per-stop live ETA + which bus is coming | `route_2101_live.json` (72 stops) |
| `ajax_routeinfo_pathattr.jsp` | 2 | `prj`, `id` | sub-route (variant) pattern per stop | `route_2101_pathattr.json` |
| `CarInfo.ashx` | 3 | `pathId` | live vehicle positions | `carInfo_2101.json` |
| `location.ashx` | 1 | `id`, `goback`, `Lang` | stop positions for one direction | `location_2101.json` (73 stops) |
| `keyword.ashx` | 1 | `keyword`, `Lang` | stop/route search results | `keyword_train.json` (20 stops) |
| `pathInfo.jsp` | 4 | `pathId` | full timetable as HTML | `timetable_2101.html` (39 KB) |
| `Marquee2.ashx` | 1 | — | news ticker | `marquee2.json` |

Samples live next to this doc in `assets/Tainan/`. Counts are from those captures.

## Shared conventions

**`Lang`** — `cht` / `en` / `jp` (front page offers all three).

**`goback`** — direction flag. ⚠ opposite of Taichung:

| Value | Meaning |
|---|---|
| `0` | 去程 outbound (dept → dest) |
| `1` | 返程 return |

**Route id (`id` / `pathId` / `routeid`)** — string like `"2101"` (101路). Same value across endpoints 1–4.

**`subRouteId`** — route variant, = route id + 2-digit suffix, e.g. `2101` → `210101`, `210103`, `210105`, `210107`. Each variant is one timetable pattern (e.g. different terminus or a detour 繞南寧高中). Analogous to Taichung's main/side routes but formalized in the id.

**Id spaces** (do not mix):

| Field | Space |
|---|---|
| `stopid` / `id` (in stop lists) | global stop id, e.g. `33316` |
| `locationId` (endpoint 2, timetable HTML) | per-route stop occurrence, e.g. `2675` |
| `locationid` (endpoint 1 `location.ashx`) | its own numbering, e.g. `9193` — different from the one above despite the same name |

**No operator directory** — unlike Taichung `providers`, there is no operator list endpoint. `Dept` in `PathListV2` is the departure terminus name, not the operator.

---

## 1. `GetPathTypeListV2.ashx` — route categories

```
GET .../API/GetPathTypeListV2.ashx?Lang=cht&prj=tn
```

```text
[]                        # one row per category
├── type     string       # 市區公車, 觀光公車, 高鐵接駁車, 雙層巴士, 小黃巴士, 綠/藍/棕/橘/黃/紅線...
└── typeid   string       # filter value; colored-line entries are CSV, e.g. "11,71" (綠線 = types 11+71)
```

## 2. `PathListV2.ashx` — all route headers

```
GET .../API/PathListV2.ashx?Lang=cht&prj=tn
```

```text
[]                        # one row per route (165 in sample)
├── id         string     # route id, e.g. "2101" — the key for all other calls
├── name       string     # display name, e.g. "101路(原1路)"
├── Dept       string     # departure terminus name (zh)
├── Dest       string     # destination terminus name (zh)
├── type       string     # category name (see GetPathTypeList)
├── typeid     string     # category id
├── sequence   int        # sort order
├── Beacon     string     # "1" = beacon-equipped stop group? (flag, meaning inferred)
└── isReserve  int        # 1 = reservation required (小黃公車-style), inferred
```

## 3. `CrossRoutesByPathName.ashx` — stop list for one route

```
GET .../API/CrossRoutesByPathName.ashx?id=2101&prj=tn
```

```text
[]                        # one entry per direction (2 in sample)
├── routeid    string     # "2101"
├── routename  string     # "101路(原1路)"
├── dept       string     # departure terminus
├── dest       string     # destination terminus
├── goback     string     # "0" = outbound, "1" = return (see conventions)
├── startTime  string     # first departure "HHMM", e.g. "0520"  ← headway info lives here, not in a timetable JSON
├── endTime    string     # last departure "HHMM", e.g. "2200"
└── stop[]                # ordered stops of this direction
    ├── stopname string   # stop name (zh)
    ├── stopid   string   # global stop id, e.g. "33316"
    ├── lat      string   # WGS84 (string, not float!)
    └── lon      string
```

This is the closest equivalent of Taichung `route(xno)`. Missing vs Taichung: no per-stop `ivrNo`; geometry lives in a separate call (`ReadText.ashx`, see #4).

## 4. `ReadText.ashx` — route path geometry (polyline)

```
POST .../NewTNBusWeb/APIs/ReadText.ashx
X-Requested-With: XMLHttpRequest
body: Path=2101_go&prj=tn        # or Path=2101_back
```

```text
[]                        # ordered path points of that direction
├── X   float             # longitude (WGS84, real numbers — only endpoint with numeric coords)
└── Y   float             # latitude
```

Point list, not an encoded polyline — draw directly. Name is misleading (likely "read server text/shape file"). `Path` must be exactly `{routeId}_go` or `{routeId}_back`; sub-route ids (`210101_go`) and other orderings return `Err01`. First point matches the direction's first stop.

## 5. `ajax_routeinfo.jsp` — per-stop live ETA

```
GET .../IMP/jsp/rwd_api/ajax_routeinfo.jsp?prj=tn&id=2101
```

```text
{                              # top level
├── result     bool            # true on success
├── size       int             # stop count
├── data[]                    # one entry per stop, outbound order
│   ├── id         int         # global stop id
│   ├── stopSeq    int         # 1-based sequence
│   ├── name       string      # stop name (zh)
│   ├── lat / lon  string      # WGS84
│   ├── stopStatus string      # live ETA text: "28分鐘" / "即將進站" / "進站中" / "末班車已過"...
│   ├── busNo      string      # plate of approaching bus, "-1"/"" when none
│   ├── busImage   string      # bus icon ref, usually ""
│   ├── locationId int         # per-route stop occurrence (timetable HTML uses same)
│   └── stop_type  string      # stop class, e.g. "0"
└── stopData[]                # per-stop variant membership
    ├── id         int         # global stop id
    └── padata[]
        ├── paid  string      # subRouteId, e.g. "210101"
        └── img   string      # per-variant marker 0/1/2/3 (bucket index, inferred)
```

Live data — poll for countdown displays. No Taichung equivalent (Taichung samples have no ETA endpoint).

## 6. `ajax_routeinfo_pathattr.jsp` — variant pattern per stop

```
GET .../IMP/jsp/rwd_api/ajax_routeinfo_pathattr.jsp?prj=tn&id=2101
```

```text
{
├── lspatp   int               # unknown flag (8 in sample)
├── result   bool
├── size     int
└── data[]                   # one entry per stop
    ├── id          int       # global stop id
    ├── seq         string    # 0-based sequence
    ├── subRouteId  string    # variant this stop's next-bus info belongs to
    ├── stopInfo    string    # same ETA text as ajax_routeinfo
    ├── carNo       string    # plate, "" when none
    ├── infoId      string    # internal info row id
    └── imgTag      string    # "000" / "100"... status bitmap (inferred)
```

Redundant with #4 for most purposes; use #4.

## 7. `CarInfo.ashx` — live vehicle positions

```
GET .../NewTNBusWeb/APIs/CarInfo.ashx?pathId=2101&prj=tn
```

```text
[]                        # one entry per running vehicle
├── No       string       # plate, e.g. "070-U9"
├── Lat      string       # WGS84
├── Lon      string
├── Speed    string       # km/h
├── Angle    string       # heading degrees, zero-padded "034"
├── GoBack   string       # "0" outbound / "1" return
├── InStop   string       # 1 = dwelling at a stop
└── CarVoid  string       # 1 = in service? (inferred)
```

Closest Taichung equivalent would be a vehicles feed, but Taichung samples didn't include one.

## 8. `location.ashx` — stop positions for one direction

```
GET .../API/location.ashx?id=2101&goback=1&Lang=cht&prj=tn
```

```text
[]                        # one entry per stop of that direction (73 in sample)
├── routeid    string
├── goback     string
├── stopid     string     # global stop id
├── stopname   string
├── lat / lon  string
└── locationid string     # ⚠ different id space from #4's locationId
```

Despite the name this returned the full ordered stop list, not moving buses — use `CarInfo.ashx` for vehicles.

## 9. `keyword.ashx` — text search

```
GET .../API/keyword.ashx?keyword=火車站&Lang=cht&prj=tn
```

```text
[]                        # mixed result types
├── type     string       # "stop" (only type in sample; JS also handles route)
├── id       string       # stop id (for stop results)
├── name     string       # zh name
├── EName    string       # en name
├── JPName   string       # ja name
├── cross1 / cross2 string # intersecting routes (empty in sample)
├── lat / lon  string
```

## 10. `pathInfo.jsp` — timetable (HTML, not JSON)

```
GET https://busmap.tainan.gov.tw/ebus/pathInfo.jsp?pathId=2101
```

Returns a server-rendered page: one `<table>` per direction, split into one sub-table per `subRouteId` variant (101路 has 4: two terminus patterns × 繞南寧高中 detour). Columns are stops (identified by `locationId`), cells are `HHMM` departure strings.

No JSON timetable endpoint found — the legacy `GoAndBackWithTimeV2.ashx` returns `410 Gone`. To get machine-readable times you must scrape this HTML.

## 11. `Marquee2.ashx` — news ticker

```text
[]
├── url   string   # link, often ""
└── info  string   # announcement text
```

---

## Endpoints seen in JS but dead or out of scope

| Endpoint | Status |
|---|---|
| `API/GoAndBack.ashx`, `API/GoAndBackWithTimeV2.ashx` | 404 / 410 Gone (legacy stop+timetable) |
| `API/CrossRoutesV2.ashx`, `API/GetPathFromStopIdV2.ashx`, `API/News.ashx` | return `err03` (unknown error code) |
| `ajax_ubikestation.jsp`, `ajax_ubikesInfo.jsp` | YouBike data, not probed |
| `DRTS/action/api/getPathTimeTableStatus` | 小黃公車 DRTS status; GET → 405, needs POST, not probed |
| `News4PathOp.ashx?pathId=` | per-route operator news, not probed |

## Quirks vs Taichung

| | Taichung | Tainan |
|---|---|---|
| Protocol | GraphQL (+relay wrappers) | plain GET JSON + one HTML page |
| Direction flag | `goBack` 1=去程 2=返程 | `goback` **0**=去程 **1**=返程 |
| Route key | `xno` int / `id` string | string `"2101"` everywhere |
| Route variants | separate `main-side-routes` mapping | built into `subRouteId` suffix |
| Timetable | `dailyTimeTable` JSON (per departure) | HTML scrape only |
| Operators | `providers` directory | none |
| Route geometry | encoded polyline per direction | plain point list (`ReadText.ashx`, POST) |
| Live data | not sampled | stop ETA (#4) + vehicle GPS (#6) |
| Coords | JSON numbers | **strings** everywhere |
| Realtime freshness | static schedule data | #4/#6 are live snapshots |
