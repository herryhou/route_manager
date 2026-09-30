# Bus Route Versioning & Historical Reporting

## Architecture & Implementation Guide

**Status:** Proposed Standard
**Audience:** Backend / Database / Architecture / BI / Data Engineering
**Scope:** Route、Stop、Trip、Report 的歷史版本與一致性管理

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

stop_identity_id
stop_version_id

stop_code
```

其中：

* `route_id`：Route 的穩定 identity
* `route_version_id`：Route 在某個版本的完整狀態
* `stop_identity_id`：Stop 的永久 identity
* `stop_version_id`：Stop 在某個版本的狀態
* `stop_code`：業務使用的 Stop 編號，可依業務規則重新分配

---

# 3. Immutable History Rule

### MUST

Published Version 一旦建立，不得修改其歷史內容。

包括：

```text
RouteVersion
RouteStopVersion
StopVersion
```

### MUST NOT

禁止透過 UPDATE 修改已發布 Version 的業務內容，例如：

```text
RouteVersion 的 Stop 組成
StopVersion 的名稱
StopVersion 的座標
RouteStopVersion 的 sequence
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

---

# 5. Route Version

`route_version` 保存某一時點 Route 的完整版本。

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

例如：

```text
Route 307

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

---

# 6. Stop Identity

`stop` 表示 Stop 的永久 identity。

概念欄位：

```text
stop_identity_id
created_at
```

### MUST

`stop_identity_id` 不得重用。

如果：

```text
Stop C
stop_identity_id = 1003
```

被撤除，`1003` 永遠代表 C。

未來即使：

```text
stop_code = 103
```

被另一個 Stop E 使用，也必須建立新的 identity：

```text
Stop E
stop_identity_id = 1005
stop_code = 103
```

---

# 7. Stop Version

`stop_version` 描述 Stop 在某個時間區間內的狀態。

概念欄位：

```text
stop_version_id
stop_identity_id
stop_code
stop_name
latitude
longitude
valid_from
valid_to
created_at
```

例如：

```text
stop_identity_id = 1002

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

---

# 8. Route Stop Version

Route 與 Stop 的關係也必須版本化。

概念：

```text
route_stop_version
-------------------
route_stop_version_id
route_version_id
stop_version_id
sequence_no
```

例如：

```text
Route 307 / V1

sequence | stop
---------+-----
1        | A
2        | B
3        | C
4        | D
```

V3：

```text
Route 307 / V3

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

假設：

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

---

# 10. Stop Code Reuse

`stop_code` 不等於 Stop Identity。

例如：

```text
Old C

stop_identity_id = 1003
stop_code = 103
```

C 被撤除後：

```text
New E

stop_identity_id = 1005
stop_code = 103
```

允許：

```text
1003 / 103 / C
1005 / 103 / E
```

同時存在於歷史資料中。

### MUST NOT

禁止透過：

```text
UPDATE stop
SET stop_identity_id = ...
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
occurred_at
```

例如：

```text
Trip T001
route_id = 307
route_version_id = V1
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

create StopVersion(s)

create RouteStopVersion(s)

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
但 RouteStopVersion 尚未完成
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
DIM_STOP
DIM_ROUTE_STOP

FACT_TRIP
```

Fact 可以直接保存：

```text
route_version_key
stop_key
```

而不是每次報表都重新執行 temporal resolution。

---

# 25. Historical Dimension

例如：

```text
DIM_STOP

stop_key | stop_identity | code | name
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
create RouteStopVersion
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
stop_identity_id
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
             ┌───────┴────────┐
             ▼                ▼
       Route Version      Stop Version
             │
             ▼
      Route Stop Version
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

# 32. Production Invariants

以下規則應視為系統 invariant：

```text
INV-01
Published RouteVersion MUST be immutable.

INV-02
Published StopVersion MUST be immutable.

INV-03
RouteStopVersion MUST be immutable.

INV-04
stop_identity_id MUST NEVER be reused.

INV-05
route_id MUST represent stable Route identity.

INV-06
route_version_id MUST identify one immutable Route state.

INV-07
A Route MUST NOT have overlapping published valid intervals.

INV-08
A historical Transaction MUST persist its Route Version reference.

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
```

---

# 33. Reference Data Model

```text
ROUTE
-----
route_id PK
route_code
current_route_version_id
revision
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


STOP
----
stop_identity_id PK
created_at


STOP_VERSION
------------
stop_version_id PK
stop_identity_id FK
stop_code
stop_name
latitude
longitude
valid_from
valid_to
created_at


ROUTE_STOP_VERSION
------------------
route_stop_version_id PK
route_version_id FK
stop_version_id FK
sequence_no


TRIP
----
trip_id PK
route_id
route_version_id
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

# 34. Terminology

本文件統一使用：

```text
Route
Stop
Route Version
Stop Version
Route Stop Version
Trip
Report Snapshot
```

不要在此模型中以 `Station` 作為資料模型名稱。

如果既有系統仍使用：

```text
station_id
station_name
```

可以在 migration 階段逐步轉換，但新設計應統一使用：

```text
stop_identity_id
stop_version_id
stop_code
stop_name
```

---

# 35. One-line Architecture Principle

整個設計可以濃縮成：

> **Current state is a pointer; history is immutable; transactions bind versions when events occur; reports bind versions when snapshots are created.**

中文：

> **現在狀態只是一個指標；歷史版本不可變；交易在事件發生時綁定版本；報表在建立 Snapshot 時綁定版本。**

這是本系統所有 Route / Stop / Transaction / Report 設計的最高層原則。
