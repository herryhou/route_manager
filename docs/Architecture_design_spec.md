# Bus Route Versioning & Historical Reporting

## Architecture & Implementation Guide

**Status:** Proposed Standard
**Audience:** Backend / Database / Architecture / BI / Data Engineering
**Scope:** Route、SubRoute、StopOfRoute、Stop、Station、StationGroup、Trip、Report 的歷史版本與一致性管理

---

# 1. Purpose

本規格定義公車管理系統如何處理：

* Route 頻繁新增、刪除、調整 Stop
* Stop 名稱、位置、屬性變更
* Stop business code 被重新使用
* 歷史交易必須保留當時的 Route 狀態
* 報表必須能產生歷史快照
* 同一份報表需要可重現
* 多人同時修改 Route
* Report 執行期間 Route 發生變更
* 大量歷史資料與 Reporting workload

核心要求：

> **歷史資料不得依賴目前的 Route / Stop 狀態來重新推導。**

所有需要歷史一致性的資料，必須在適當的時間點固定對應到 immutable version。

---

# 2. Core Principles

系統必須遵守以下原則。

## 2.1 Identity 與 Version 分離

以下概念必須分開：

```text
Identity
Version
Current State
Business Code
```

例如：

```text
route_id
route_version_id

subroute_id
subroute_version_id

stop_id
stop_version_id

station_id
station_version_id

station_group_id
station_group_version_id

stop_code
```

其中：

* `route_id`：Route 的穩定 identity
* `route_version_id`：Route aggregate 在某個版本的完整狀態（含 SubRoute、StopOfRoute）
* `subroute_id`：SubRoute 的穩定 identity，隸屬於 Route
* `stop_id`：Stop（站牌）的永久 identity
* `stop_version_id`：Stop 在某個版本的狀態
* `station_id`：Station（站位）的永久 identity
* `station_version_id`：Station 在某個版本的狀態
* `station_group_id`：StationGroup（組站位）的永久 identity
* `station_group_version_id`：StationGroup 在某個版本的狀態
* `stop_code`：業務使用的 Stop 編號，可依業務規則重新分配

Identity 一律不版本化；被歷史事實引用的「狀態」才版本化。

命名規則：

```text
*_id          immutable identity
*_version_id  某版本的狀態
```

---

# 3. Immutable History Rule

### MUST

Published Version 一旦建立，不得修改其歷史內容。

包括：

```text
RouteVersion
SubRouteVersion
StopOfRouteVersion
StopVersion
StationVersion
StationGroupVersion
```

### MUST NOT

禁止透過 UPDATE 修改已發布 Version 的業務內容，例如：

```text
RouteVersion 的 SubRoute 組成
SubRouteVersion 的名稱
StopOfRouteVersion 的 direction / sequence
StopVersion 的名稱
StopVersion 的座標
StationVersion 的名稱
StationGroupVersion 的內容
```

如果需要修改：

```text
Create New Version
```

而不是：

```text
UPDATE Existing Version
```

---

# 4. Route Model

## 4.1 Route

`route` 表示 Route Identity。

概念欄位：

```text
route_id
route_code
current_route_version_id
revision
created_at
```

其中：

```text
route_id
```

是 immutable identity。

```text
current_route_version_id
```

只表示目前使用中的 Version。

因此：

> Current State 是一個 pointer，不是歷史資料本身。

## 4.2 SubRoute

`subroute` 表示 SubRoute（子路線）的 identity，例如本線、區間車、延駛、繞駛版本。

概念欄位：

```text
subroute_id
route_id
subroute_code
created_at
```

SubRoute 只有 identity，沒有獨立的版本表。

SubRoute 在某個版本的名稱與狀態，保存於 `SubRouteVersion`，作為 `RouteVersion` 的一部分（見第 5 章）。

---

# 5. Route Version

`route_version` 保存某一時點 Route aggregate 的完整版本。

RouteVersion 涵蓋整個 aggregate：

```text
RouteVersion
├── SubRoute 組成（此版本包含哪些 SubRoute）
├── 每個 SubRoute 的 StopOfRoute 站序（依 Direction 分開）
└── 每個站序點引用的 StopVersion
```

概念欄位：

```text
route_version_id
route_id
version_no
valid_from
valid_to
created_at
published_at
```

SubRoute 在此版本的狀態：

```text
subroute_version_id
route_version_id
subroute_id
subroute_name
```

StopOfRoute 站序在此版本的狀態：

```text
stop_of_route_version_id
subroute_version_id
stop_version_id
direction
stop_sequence
```

其中：

* `direction`：0 = 去程，1 = 返程
* `stop_sequence`：該方向內的站序

例如：

```text
Route 307 / SubRoute 307A（本線）/ Direction 0

Version 1
A → B → C → D

Version 2
A → B' → C → D

Version 3
A → B' → D

Version 4
A → B' → E → D
```

所有 Version 都必須保留。

### MUST

SubRoute 與 StopOfRoute 只能作為 RouteVersion 的一部分建立新版本；
不得脫離 RouteVersion 單獨版本化。

---

# 6. Stop / Station / StationGroup Identity

站體分為三層：

```text
Stop（站牌）
  一個站牌點位、特定路線、特定方向；
  同一點位有多條路線各自的站牌桿時，即有多筆 Stop。

Station（站位）
  道路同側、附近同一集群站牌聚合而成的抽象點位。

StationGroup（組站位）
  不分路線、不分方向，供票證 OD 旅次分析使用。
```

## 6.1 Stop Identity

`stop` 表示站牌的永久 identity。

概念欄位：

```text
stop_id
created_at
```

## 6.2 Station Identity

`station` 表示站位的永久 identity。

概念欄位：

```text
station_id
created_at
```

Stop 與 Station 的隸屬關係記錄在 `StopVersion.station_id`。

## 6.3 StationGroup Identity

`station_group` 表示組站位的永久 identity。

概念欄位：

```text
station_group_id
created_at
```

Station 與 StationGroup 的隸屬關係記錄在 `StationVersion.station_group_id`。

### MUST

三層 identity 皆不得重用：

```text
stop_id
station_id
station_group_id
```

如果：

```text
Stop C
stop_id = 1003
```

被撤除，`1003` 永遠代表 C。

未來即使：

```text
stop_code = 103
```

被另一個 Stop E 使用，也必須建立新的 identity：

```text
Stop E
stop_id = 1005
stop_code = 103
```

---

# 7. Stop / Station / StationGroup Version

三層站體各自有 version，記錄該時間區間內的狀態。

## 7.1 StopVersion

`stop_version` 描述站牌在某個時間區間內的狀態。

概念欄位：

```text
stop_version_id
stop_id
station_id
stop_code
stop_name
direction
latitude
longitude
valid_from
valid_to
created_at
```

其中：

* `station_id`：此站牌隸屬的站位
* `direction`：站牌方向（去程 / 返程）

Stop 改隸屬 Station 時，建立新的 StopVersion，不得 UPDATE 舊版本。

例如：

```text
stop_id = 1002

Version 1
stop_code = 102
stop_name = B站
valid_from = 2026-01-01
valid_to   = 2026-03-01

Version 2
stop_code = 102
stop_name = B中央站
valid_from = 2026-03-01
valid_to   = NULL
```

注意：

> B 的 identity 沒有改變；只是 Stop Version 改變。

## 7.2 StationVersion

`station_version` 描述站位在某個時間區間內的狀態。

概念欄位：

```text
station_version_id
station_id
station_group_id
station_name
latitude
longitude
valid_from
valid_to
created_at
```

Station 改隸屬 StationGroup 時，建立新的 StationVersion。

## 7.3 StationGroupVersion

`station_group_version` 描述組站位在某個時間區間內的狀態。

概念欄位：

```text
station_group_version_id
station_group_id
station_group_name
valid_from
valid_to
created_at
```

組站位成員組成由各 `StationVersion.station_group_id` 還原；
不得 UPDATE 已發布的 StationGroupVersion。

---

# 8. StopOfRoute Version

SubRoute 與 Stop 的關係（實際停靠站序）也必須版本化，
作為 RouteVersion 的一部分。

概念：

```text
stop_of_route_version
---------------------
stop_of_route_version_id
subroute_version_id
stop_version_id
direction
stop_sequence
```

同一 SubRoute 的去程與返程，是不同的 StopOfRoute 站序。

例如：

```text
Route 307 / SubRoute 307A / V1 / Direction 0

sequence | stop
---------+-----
1        | A
2        | B
3        | C
4        | D

Route 307 / SubRoute 307A / V1 / Direction 1

sequence | stop
---------+-----
1        | D
2        | C
3        | B
4        | A
```

V3：

```text
Route 307 / SubRoute 307A / V3 / Direction 0

sequence | stop
---------+-----
1        | A
2        | B'
3        | D
```

C 並沒有被「刪除歷史資料」。

只是：

> V3 不再 reference C。

---

# 9. Complete Example

假設（以 307 本線 SubRoute 307A、Direction 0 為例）：

```text
T1
307 = A → B → C → D

T2
B 改名為 B'

307 = A → B' → C → D

T3
C 從 Route 307 移除

307 = A → B' → D

T4
原 C 使用過的 stop_code = 103 被 E 重用

307 = A → B' → E → D
```

最後：

```text
V1 = A → B  → C → D
V2 = A → B' → C → D
V3 = A → B'      → D
V4 = A → B' → E → D
```

這四個 Version 必須全部保留。
（其他 SubRoute 與 Direction 1 的站序，同樣保存在各 RouteVersion 內。）

---

# 10. Stop Code Reuse

`stop_code` 不等於 Stop Identity。

例如：

```text
Old C

stop_id = 1003
stop_code = 103
```

C 被撤除後：

```text
New E

stop_id = 1005
stop_code = 103
```

允許：

```text
1003 / 103 / C
1005 / 103 / E
```

同時存在於歷史資料中。

同一原則適用於三層站體 identity：

```text
stop_id
station_id
station_group_id
```

任一層 identity「撤除後再出現」，都必須建立新的 identity。

### MUST NOT

禁止透過：

```text
UPDATE stop
SET stop_id = ...
```

讓舊 Stop Identity 轉變成新 Stop。

---

# 11. Valid Time

Version 必須有有效時間：

```text
valid_from
valid_to
```

例如：

```text
V10
2026-01-01 → 2026-10-01

V11
2026-10-01 → NULL
```

同一 Route 的 Published Version：

### MUST NOT

存在 overlapping valid intervals。

例如：

```text
V10: 01/01 → 10/15
V11: 10/01 → NULL
```

是不合法的。

---

# 12. System Time vs Valid Time

系統至少需要區分：

### System Time

```text
created_at
published_at
```

回答：

> 系統什麼時候知道／發布這個 Version？

### Valid Time

```text
valid_from
valid_to
```

回答：

> 這個 Version 在業務世界什麼時候生效？

例如：

```text
created_at  = 2026-09-29 15:00
published_at = 2026-09-29 15:01
valid_from = 2026-10-01 00:00
```

代表：

> 9/29 已經發布，但 10/1 才生效。

兩種時間不得混用。

---

# 13. Transaction Version Binding

任何具有歷史 Route 語意的交易，必須保存：

```text
route_id
route_version_id
subroute_id
direction
occurred_at
```

其中 `subroute_id` 與 `direction` 標明交易發生在哪個分支、哪個方向；
其當時狀態由 `route_version_id` 內的 SubRouteVersion / StopOfRouteVersion 還原。

例如：

```text
Trip T001
route_id = 307
route_version_id = V1
subroute_id = 307A
direction = 0
occurred_at = 2026-02-15
```

即使現在：

```text
307 = V20
```

T001 仍然指向 V1。

---

# 14. Transaction Snapshot Rule

交易流程：

```text
Transaction Start
       ↓
Resolve Route Version
       ↓
Persist route_version_id
       ↓
Business Processing
       ↓
Commit
```

### MUST

一旦 Transaction 綁定 `route_version_id`，後續 Route Publish 不得改變該 Transaction 的歷史對應。

### MUST NOT

在 Transaction commit 後才重新根據 `route_id` 推導 Route Version。

---

# 15. Report Snapshot

正式報表必須有自己的 Snapshot。

概念：

```text
report_snapshot
----------------
report_id
snapshot_at
snapshot_type
status
created_at
```

Route mapping：

```text
report_route_snapshot
---------------------
report_id
route_id
route_version_id
```

OD 類報表若依賴組站位，也必須 snapshot：

```text
report_station_group_snapshot
-----------------------------
report_id
station_group_id
station_group_version_id
```

例如：

```text
Report R001

snapshot_at = 2026-09-29

307 → V2
308 → V15
309 → V8
```

---

# 16. Report Snapshot Rule

### MUST

正式 Report 在建立 Snapshot 後，必須使用 Snapshot 中的 Version Reference。

### MUST NOT

報表執行期間重新查詢：

```text
current_route_version_id
```

來改變已經確定的歷史語意。

---

# 17. Two Report Semantics

系統必須明確區分：

## 17.1 TRANSACTION_TIME

問題：

> 交易發生時 Route 是什麼？

來源：

```text
trip.route_version_id
```

---

## 17.2 REPORT_TIME

問題：

> 在報表 Snapshot 時間點，Route 是什麼？

流程：

```text
snapshot_at
    ↓
resolve Route Version
    ↓
persist report_route_snapshot
```

---

# 18. Reproducibility

正式報表必須具備：

> Same Report Snapshot + Same Source Facts = Same Result

例如：

```text
R001
307 → V2
```

即使之後：

```text
307 → V3
307 → V4
307 → V5
```

重新產生 R001：

```text
307 → V2
```

不得變成 V5。

---

# 19. Concurrency Control

Route 修改必須具有 concurrency control。

推薦：

```text
route.revision
```

例如：

```text
route_id = 307
revision = 10
```

修改時：

```text
UPDATE route
SET revision = revision + 1
WHERE route_id = 307
AND revision = 10
```

如果：

```text
affected_rows = 0
```

代表：

> 另一個修改已經先完成。

目前操作必須重新讀取最新 Version。

---

# 20. Draft / Publish

複雜 Route 修改 SHOULD 使用：

```text
Draft
 ↓
Validate
 ↓
Publish
 ↓
Immutable Version
```

Draft 可以自由修改。

Published Version 不可修改。

例如：

```text
Current V10
      │
      ▼
    Draft
      │
 ┌────┼─────┐
 │    │     │
Add  Remove Rename
 │    │     │
 └────┼─────┘
      ▼
   Validate
      ▼
   Publish
      ▼
     V11
```

---

# 21. Atomic Publish

Publish 必須是一個 DB transaction。

概念：

```text
BEGIN

lock Route aggregate

check revision

validate Version

create RouteVersion

create SubRouteVersion(s)

create StopOfRouteVersion(s)
（站序引用既有 StopVersion；站體版本另有自己的版本化流程）

close previous validity interval

update current_route_version_id

create Outbox Event

COMMIT
```

### MUST

上述資料要嘛全部成功，要嘛全部 rollback。

不得出現：

```text
RouteVersion 已建立
但 StopOfRouteVersion 尚未完成
```

的半成品 Published Version。

---

# 22. Route-level Serialization

Concurrency boundary SHOULD 以 Route 為單位。

概念：

```text
Route 307 → lock 307
Route 308 → lock 308
```

因此：

```text
User A → 修改 307
User B → 修改 307
```

必須 serialize。

但：

```text
User A → 修改 307
User B → 修改 308
```

可以平行。

---

# 23. Reporting Layer

當資料量變大，不應讓所有歷史報表直接查 OLTP。

推薦：

```text
OLTP
 ↓
CDC / Transactional Outbox
 ↓
Reporting Layer
```

OLTP 負責：

* Source of Truth
* Version
* Transaction
* Concurrency
* Audit

Reporting Layer 負責：

* Historical Query
* BI
* Aggregation
* Large-scale Reporting

---

# 24. Reporting Data Model

推薦 Star Schema / SCD Type 2 思路。

例如：

```text
DIM_ROUTE_VERSION
DIM_SUBROUTE
DIM_STOP
DIM_STATION
DIM_STATION_GROUP

FACT_TRIP
```

Fact 可以直接保存：

```text
route_version_key
subroute_key
stop_key
station_key
station_group_key
```

而不是每次報表都重新執行 temporal resolution。

---

# 25. Historical Dimension

例如：

```text
DIM_STOP

stop_key | stop_id | code | name
---------+---------------+------+---------
501      | 1002          | 102  | B站
502      | 1002          | 102  | B中央站
503      | 1003          | 103  | C站
504      | 1005          | 103  | E站
```

因此：

```text
C ≠ E
```

即使：

```text
C.stop_code = E.stop_code = 103
```

站體三層各有 historical dimension：

```text
DIM_STOP           站牌（含 direction、所屬 station）
DIM_STATION        站位（含所屬 station group）
DIM_STATION_GROUP  組站位（OD 分析用）
```

---

# 26. Partitioning

大量 `FACT_TRIP` SHOULD 依：

```text
service_date
```

或：

```text
service_month
```

進行 partition。

例如：

```text
FACT_TRIP
├── 2026-01
├── 2026-02
├── 2026-03
└── ...
```

具體 partition strategy 必須依實際 DB 與 workload 決定。

---

# 27. Transactional Outbox

Route Publish 與 Event 必須具有一致性。

推薦：

```text
BEGIN

create RouteVersion
create SubRouteVersion / StopOfRouteVersion
update current pointer

insert Outbox Event

COMMIT
```

然後：

```text
Outbox
   ↓
CDC / Message Broker
   ↓
Reporting Layer
```

### MUST NOT

先送 Event，再寫 DB。

否則可能出現：

```text
Event：V11 exists
DB：V11 不存在
```

---

# 28. Database Snapshot Isolation

DB 的 Snapshot Isolation 可以用來保證：

> 單次 Report Query 執行期間看到一致的 DB view。

但它不能取代：

```text
route_version_id
report_snapshot
```

兩者責任不同：

```text
Snapshot Isolation
= Query consistency

Version / Report Snapshot
= Business historical semantics
```

兩者 SHOULD 同時使用。

---

# 29. Anti-patterns

以下設計禁止或強烈不建議。

## 29.1 直接 UPDATE 歷史 Route

```text
UPDATE route_stop
SET ...
```

❌ 不可用於 Published History。

---

## 29.2 報表只 JOIN Current Route

```text
Trip
 → route_id
 → current route
```

❌ 會造成歷史污染。

---

## 29.3 只保存 stop_code

```text
trip.stop_code = 103
```

❌ 不足以識別歷史 Stop。

應保存：

```text
stop_id
stop_version_id / stop_key
```

---

## 29.4 重用 Stop Identity

```text
C identity 1003
↓
C deleted
↓
E becomes identity 1003
```

❌ 禁止。

---

## 29.5 報表每次重新 resolve Version

```text
Report Query
 → route_id
 → current/temporal lookup
```

❌ 正式可重現報表不應依賴此模式。

---

## 29.6 Publish 非 Atomic

```text
Create RouteVersion
COMMIT

Create RouteStops
COMMIT
```

❌ 可能產生半完成版本。

---

## 29.7 站牌與站位混用

```text
Stop（站牌）與 Station（站位）互相取代，或只保存其中一層
```

❌ 站牌、站位、組站位是不同 granularity，歷史資料必須分層保存。

---

## 29.8 丟失 SubRoute / Direction

```text
trip 只存 route_id + route_version_id
```

❌ 無法區分本線 / 區間車、去程 / 返程的歷史事實。

---

# 30. Recommended End-to-End Flow

完整 Route Change：

```text
                 User Edit
                     │
                     ▼
                  Draft
                     │
                     ▼
                 Validate
                     │
                     ▼
             Acquire Route Lock
                     │
                     ▼
             Check Revision
                     │
                     ▼
          Create Immutable Version
                     │
                     ▼
              Route Version
                     │
                     ▼
           SubRoute Version(s)
                     │
                     ▼
        StopOfRoute Version(s)
        （引用既有 StopVersion）
                     │
                     ▼
       Update Current Pointer
             │
             ▼
        Insert Outbox Event
             │
             ▼
            COMMIT
             │
             ▼
        CDC / Event Bus
             │
             ▼
       Reporting Layer
```

---

# 31. End-to-End Report Flow

```text
Report Request
      │
      ▼
Determine Snapshot Type
      │
      ├── TRANSACTION_TIME
      │
      └── REPORT_TIME
              │
              ▼
        Resolve Versions
              │
              ▼
      Create Report Snapshot
              │
              ▼
        Persist Version IDs
              │
              ▼
       Execute Report Query
              │
              ▼
       Materialize if needed
              │
              ▼
             READY
```

---

# 32. GTFS Export

## 32.1 基本原則

GTFS feed 本身就是 snapshot：一份 feed 代表某時間點的發布狀態。

> External ID 綁 identity；版本綁在 feed 匯出時刻。

```text
ID    ← identity（跨 feed 穩定）
屬性  ← 匯出當下 valid 的 Version
時間  ← feed 本身釘住
```

## 32.2 Mapping

欄位對照（GTFS ←→ 內部模型 ←→ TDX）。

### stops.txt — 站牌（location_type = 0）

```text
GTFS 欄位       內部來源                              TDX 對應
--------------  ------------------------------------  ---------------------------
stop_id         stop_id（identity）                   StopUID
stop_code       StopVersion.stop_code                 StopID
stop_name       StopVersion.stop_name                 StopName（Zh / En）
stop_lat        StopVersion.latitude                  StopPosition.PositionLat
stop_lon        StopVersion.longitude                 StopPosition.PositionLon
location_type   0（站牌）                             Stop 實體
parent_station  station_id                            Station.StationUID
```

### stops.txt — 站位（location_type = 1）

```text
GTFS 欄位       內部來源                              TDX 對應
--------------  ------------------------------------  ---------------------------
stop_id         station_id（identity）                StationUID
stop_name       StationVersion.station_name           StationName
stop_lat        StationVersion.latitude               StationPosition.PositionLat
stop_lon        StationVersion.longitude              StationPosition.PositionLon
location_type   1（站位）                             Station 實體
```

### routes.txt

```text
GTFS 欄位          內部來源                          TDX 對應
-----------------  --------------------------------  ------------------------------------
route_id           route_id（或 subroute_id）         RouteUID（或 SubRouteUID）
agency_id          operator                           Operators[].OperatorID
route_short_name   route_code                         RouteName.Zh
route_long_name    起訖站                              DepartureStopName → DestinationStopName
route_type         3（公車）                           BusRouteType
```

### trips.txt

```text
GTFS 欄位        內部來源                            TDX 對應
---------------  ----------------------------------  ------------------------------
route_id         同 routes.txt                       RouteUID / SubRouteUID
service_id       班表服務日                          Timetables[].ServiceDay
trip_id          trip_id                             Timetables[].Trips[]
trip_headsign    SubRouteName / 起訖站               SubRouteName
direction_id     direction                           Direction（0 去程、1 返程）
```

### stop_times.txt

```text
GTFS 欄位        內部來源                            TDX 對應
---------------  ----------------------------------  ------------------------------
trip_id          trip_id                             Trips[]
stop_id          StopOfRouteVersion → stop_id        StopTimes[].StopUID
stop_sequence    StopOfRouteVersion.stop_sequence    StopTimes[].StopSequence
arrival_time     班表時刻                            StopTimes[].ArrivalTime
departure_time   班表時刻                            StopTimes[].DepartureTime
```

### 其他檔案

```text
GTFS 檔案             內部來源                            TDX 對應
--------------------  ----------------------------------  ------------------------------
calendar.txt          班表服務日                          Timetables[].ServiceDay
calendar_dates.txt    停駛 / 加開例外                     ServiceDay 例外
feed_info.txt         gtfs_export（export_id / 時間）     VersionID / UpdateTime
agency.txt            operator                            Operators
shapes.txt            軌跡圖資（內部另存）                  Shape（Geometry，含 Direction）
```

### Route granularity

預設一個 Route 一個 GTFS route，
SubRoute 變體以 trips + stop_times patterns 表達；
若業務要求每個 SubRoute 顯示為獨立路線，
則 `routes.route_id` 改綁 `subroute_id`，規則不變（仍綁 identity）。

### MUST NOT

不得用 `stop_version_id` 作為 GTFS `stop_id`。

原因：每次 Stop 改名（新 StopVersion）外部 ID 就跟著變，
下游看到「舊站刪除 + 新站建立」，歷史連續性全斷，
違反 GTFS stable ID best practice。

## 32.3 歷史 Feed 可重現

正式匯出必須記錄版本綁定：

```text
gtfs_export
-----------
export_id
exported_at
stop_id  → stop_version_id
route_id → route_version_id
```

與 report_snapshot 同精神（INV-09 / INV-10 套用）：

> 重新產生歷史 feed 時，必須使用 export 當時的版本綁定，
> 不得以 current state 重新推導。

## 32.4 搬遷 vs 改名

```text
屬性修正（改名、座標微調）
  → 新 StopVersion
  → GTFS stop_id 不變

實體搬遷／換桿
  → 新 stop identity
  → GTFS 發新 stop_id
```

符合 GTFS「移位過大應發新 ID」慣例。

## 32.5 StationGroup

GTFS 沒有組站位等級。

StationGroup 僅供內部 OD 分析，不匯出；
不得以非標準 parent chain 硬塞進 GTFS。

TDX 同樣：靜態公車 API 只有 Stop（站牌）/ Station（站位），
組站位僅出現在票證 OD 主題式資料，無標準靜態欄位可對應。

## 32.6 TDX 匯入注意事項

自 TDX 匯入（反向 mapping）時：

```text
TDX Direction 編碼與內部一致（0 去程、1 返程），直接映射。
```

公路客運 `SubRouteID` 編碼原則：

```text
前四碼    路線代號
第五碼    正副路線（0 主線；A、B... 副線）
第六碼    去返程（1 去程；2 返程）
```

匯入時 MUST 拆解為 `subroute` + `direction`，
不得把整串 SubRouteID 直接當單一 identity。

同一站點因業者不同會有多個 `StopID`（公路客運特性）：
`stop_code` 相同不等於同一 `stop_id`，
mapping 一律以 `StopUID` 為準。

顯示用路線站序（DisplayStopOfRoute）是線性化顯示資料，
不等於實際跑法；匯入站序 MUST 使用 StopOfRoute，
不得使用 DisplayStopOfRoute。

---

# 33. Production Invariants

以下規則應視為系統 invariant：

```text
INV-01
Published RouteVersion MUST be immutable,
including its SubRouteVersion and StopOfRouteVersion content.

INV-02
Published StopVersion, StationVersion and
StationGroupVersion MUST be immutable.

INV-03
StopOfRouteVersion MUST be immutable.

INV-04
stop_id, station_id and
station_group_id MUST NEVER be reused.

INV-05
route_id MUST represent stable Route identity.

INV-06
route_version_id MUST identify one immutable
Route aggregate state.

INV-07
A Route MUST NOT have overlapping published valid intervals.

INV-08
A historical Transaction MUST persist its Route Version,
SubRoute and Direction reference.

INV-09
A formal Report MUST persist its Snapshot.

INV-10
A completed Report Snapshot MUST NOT be re-resolved.

INV-11
Route Publish MUST be atomic.

INV-12
Concurrent Route modifications MUST use optimistic
or pessimistic concurrency control.

INV-13
Reporting Layer MUST preserve historical Version references.

INV-14
Current State MUST be treated as a pointer to a Version,
not as the historical source of truth.

INV-15
Business Code such as stop_code MUST NOT be treated
as immutable identity.

INV-16
SubRoute and StopOfRoute MUST only be versioned
as part of a RouteVersion, never independently.

INV-17
Stop-to-Station and Station-to-StationGroup membership
MUST live in the child's version
(StopVersion / StationVersion).

INV-18
GTFS export IDs MUST bind identity (*_id), never version IDs;
each exported feed MUST log its version bindings
for reproducibility.
```

---

# 34. Reference Data Model

```text
ROUTE
-----
route_id PK
route_code
current_route_version_id
revision
created_at


SUBROUTE
--------
subroute_id PK
route_id FK
subroute_code
created_at


ROUTE_VERSION
-------------
route_version_id PK
route_id FK
version_no
valid_from
valid_to
created_at
published_at


SUBROUTE_VERSION
----------------
subroute_version_id PK
route_version_id FK
subroute_id FK
subroute_name


STOP_OF_ROUTE_VERSION
---------------------
stop_of_route_version_id PK
subroute_version_id FK
stop_version_id FK
direction
stop_sequence


STOP
----
stop_id PK
created_at


STOP_VERSION
------------
stop_version_id PK
stop_id FK
station_id FK
stop_code
stop_name
direction
latitude
longitude
valid_from
valid_to
created_at


STATION
-------
station_id PK
created_at


STATION_VERSION
---------------
station_version_id PK
station_id FK
station_group_id FK
station_name
latitude
longitude
valid_from
valid_to
created_at


STATION_GROUP
-------------
station_group_id PK
created_at


STATION_GROUP_VERSION
---------------------
station_group_version_id PK
station_group_id FK
station_group_name
valid_from
valid_to
created_at


TRIP
----
trip_id PK
route_id
route_version_id
subroute_id
direction
occurred_at
...


REPORT_SNAPSHOT
---------------
report_id PK
snapshot_at
snapshot_type
status
created_at


REPORT_ROUTE_SNAPSHOT
---------------------
report_id FK
route_id
route_version_id
...


REPORT_STATION_GROUP_SNAPSHOT
-----------------------------
report_id FK
station_group_id
station_group_version_id
...


GTFS_EXPORT
-----------
export_id PK
exported_at
version bindings（stop / route → version）
status
created_at


OUTBOX_EVENT
------------
event_id PK
aggregate_type
aggregate_id
event_type
payload
created_at
published_at
```

---

# 35. Terminology

本文件統一使用：

```text
Route
SubRoute
StopOfRoute
Stop（站牌）
Station（站位）
StationGroup（組站位）
Route Version
Stop Version
Trip
Report Snapshot
```

站體三層語意固定：

```text
Stop          站牌：特定點位、特定路線、特定方向
Station       站位：同側附近站牌的聚合點位
StationGroup  組站位：不分路線方向，OD 分析用
```

`Station` 在本模型是正式實體（站位），
不得與 `Stop`（站牌）互換使用。

---

# 36. One-line Architecture Principle

整個設計可以濃縮成：

> **Current state is a pointer; history is immutable; transactions bind versions when events occur; reports bind versions when snapshots are created.**

中文：

> **現在狀態只是一個指標；歷史版本不可變；交易在事件發生時綁定版本；報表在建立 Snapshot 時綁定版本。**

這是本系統所有 Route / Stop / Transaction / Report 設計的最高層原則。
