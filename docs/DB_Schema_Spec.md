# Bus Route Versioning

## DB Schema & Constraints Specification

**Status:** Implementation Specification
**Scope:** Route / SubRoute / StopOfRoute / Stop / Station / StationGroup / Trip / Report Snapshot
**Database:** DB-neutral
**Primary Goal:** Preserve immutable historical truth under frequent Route / Stop changes.

---

# 1. Design Rules

以下規則是 DB schema 的最高優先級：

1. `route_id` 是 Route identity，不隨 Version 改變。
2. `route_version_id` 是 immutable Route aggregate state（含 SubRoute 與 StopOfRoute）。
3. `stop_id`、`station_id`、`station_group_id` 永不重用。
4. `stop_version_id` / `station_version_id` / `station_group_version_id` 是該實體在特定有效期間的狀態。
5. `subroute` 只有 identity；其版本狀態保存於 `subroute_version`，隸屬某個 `route_version`。
6. `stop_of_route_version` 描述某個 SubRoute Version 在某個 Direction 的站序。
7. Published Version 不得 UPDATE 業務內容。
8. `trip` 必須保存發生時的 `route_version_id`、`subroute_id`、`direction`。
9. Report Snapshot 必須保存當時選定的 `route_version_id`。
10. `current_route_version_id` 只是 current pointer，不是歷史依據。
11. Route Publish 必須 atomic。

---

# 2. Entity Relationship

```text
ROUTE
  ├──< SUBROUTE
  ├──  current_route_version_id
  └──< ROUTE_VERSION
         └──< SUBROUTE_VERSION >──── SUBROUTE
                └──< STOP_OF_ROUTE_VERSION >──── STOP_VERSION
                                                │
                                                └──── STOP
                                                      │
                                                      └─ station_id → STATION

STATION
  └──< STATION_VERSION
          └─ station_group_id → STATION_GROUP
                                  └──< STATION_GROUP_VERSION


TRIP
 └──── route_version_id + subroute_id + direction


REPORT_SNAPSHOT
 ├──< REPORT_ROUTE_SNAPSHOT
 │         └──── route_version_id
 └──< REPORT_STATION_GROUP_SNAPSHOT
           └──── station_group_version_id


OUTBOX_EVENT
 └──── aggregate_id
```

---

# 3. ROUTE

概念 schema：

```sql
CREATE TABLE route (
    route_id                  BIGINT PRIMARY KEY,
    route_code                VARCHAR(...),
    current_route_version_id  BIGINT NULL,
    revision                  BIGINT NOT NULL,
    created_at                TIMESTAMP NOT NULL,
    updated_at                TIMESTAMP NOT NULL
);
```

### Rules

`revision`：

* MUST 用於 optimistic concurrency。
* 每次成功 Publish 必須遞增。
* Client 提交修改時必須帶 expected revision。

`current_route_version_id`：

* MUST 指向已成功 Publish 的 Route Version。
* MUST NOT 用於解釋歷史 Transaction。
* 更新必須與 Publish 位於同一 transaction。

---

# 3.1 SUBROUTE

```sql
CREATE TABLE subroute (
    subroute_id    BIGINT PRIMARY KEY,
    route_id       BIGINT NOT NULL,
    subroute_code  VARCHAR(...) NOT NULL,
    created_at     TIMESTAMP NOT NULL,

    FOREIGN KEY (route_id)
        REFERENCES route(route_id)
);
```

`subroute` 只有 identity，沒有獨立版本表；
其版本狀態保存於 `subroute_version`，隸屬某個 `route_version`。

---

# 4. ROUTE_VERSION

```sql
CREATE TABLE route_version (
    route_version_id  BIGINT PRIMARY KEY,
    route_id          BIGINT NOT NULL,
    version_no        BIGINT NOT NULL,

    valid_from        TIMESTAMP NOT NULL,
    valid_to          TIMESTAMP NULL,

    status            VARCHAR(...) NOT NULL,

    created_at        TIMESTAMP NOT NULL,
    published_at      TIMESTAMP NULL,

    created_by        BIGINT NULL,

    FOREIGN KEY (route_id)
        REFERENCES route(route_id)
);
```

### Required constraints

同一 Route：

```text
(route_id, version_no)
```

必須 UNIQUE。

Published Version：

```text
status = PUBLISHED
```

後不得修改：

```text
route_id
version_no
valid_from
valid_to
```

以及任何構成 Route business state 的欄位。

---

# 5. ROUTE VERSION STATUS

建議：

```text
DRAFT
PUBLISHED
SUPERSEDED
CANCELLED
```

但要注意：

> `SUPERSEDED` 是 metadata status，不代表原 Version 的內容可以被修改。

例如：

```text
V10 → SUPERSEDED
V11 → PUBLISHED
```

V10 的 Route Stops 仍然完全不變。

---

# 6. STOP / STATION / STATION_GROUP

```sql
CREATE TABLE stop (
    stop_id     BIGINT PRIMARY KEY,
    created_at  TIMESTAMP NOT NULL
);

CREATE TABLE station (
    station_id  BIGINT PRIMARY KEY,
    created_at  TIMESTAMP NOT NULL
);

CREATE TABLE station_group (
    station_group_id  BIGINT PRIMARY KEY,
    created_at        TIMESTAMP NOT NULL
);
```

這三張表刻意非常簡單。

原因是：

> identity 表只保存 identity，不保存會隨時間變化的 business state。

隸屬關係（Stop → Station、Station → StationGroup）不單獨建表，
保存於子端 version 表的 FK 欄位。

---

# 7. STOP_VERSION / STATION_VERSION / STATION_GROUP_VERSION

```sql
CREATE TABLE stop_version (
    stop_version_id  BIGINT PRIMARY KEY,
    stop_id          BIGINT NOT NULL,
    station_id       BIGINT NOT NULL,

    stop_code        VARCHAR(...) NOT NULL,
    stop_name        VARCHAR(...) NOT NULL,
    direction        SMALLINT NOT NULL,

    latitude         DECIMAL(...),
    longitude        DECIMAL(...),

    valid_from       TIMESTAMP NOT NULL,
    valid_to         TIMESTAMP NULL,

    created_at       TIMESTAMP NOT NULL,

    FOREIGN KEY (stop_id)
        REFERENCES stop(stop_id),

    FOREIGN KEY (station_id)
        REFERENCES station(station_id)
);

CREATE TABLE station_version (
    station_version_id  BIGINT PRIMARY KEY,
    station_id          BIGINT NOT NULL,
    station_group_id    BIGINT NOT NULL,

    station_name        VARCHAR(...) NOT NULL,

    latitude            DECIMAL(...),
    longitude           DECIMAL(...),

    valid_from          TIMESTAMP NOT NULL,
    valid_to            TIMESTAMP NULL,

    created_at          TIMESTAMP NOT NULL,

    FOREIGN KEY (station_id)
        REFERENCES station(station_id),

    FOREIGN KEY (station_group_id)
        REFERENCES station_group(station_group_id)
);

CREATE TABLE station_group_version (
    station_group_version_id  BIGINT PRIMARY KEY,
    station_group_id          BIGINT NOT NULL,

    station_group_name        VARCHAR(...) NOT NULL,

    valid_from                TIMESTAMP NOT NULL,
    valid_to                  TIMESTAMP NULL,

    created_at                TIMESTAMP NOT NULL,

    FOREIGN KEY (station_group_id)
        REFERENCES station_group(station_group_id)
);
```

### Important

`stop_code` 不得作為 Primary Key。

也不得假設：

```text
stop_code → stop_id
```

是一對一永久 mapping。

Stop 改隸屬 Station → new stop_version；
Station 改隸屬 StationGroup → new station_version。
不得 UPDATE 舊 version。

---

# 8. STOP CODE REUSE

允許：

```text
Old Stop:
stop_id = 1003
code = 103

New Stop:
stop_id = 1005
code = 103
```

但必須確保在同一個有效時間點，不會出現不符合業務規則的 duplicate。

這個 constraint 必須依實際 DB 能力實作。

例如：

```text
UNIQUE(stop_code, active_time_range)
```

如果 DB 不支援 temporal/exclusion constraint，必須由 Publish transaction + locking 保證。

---

# 9. SUBROUTE_VERSION 與 STOP_OF_ROUTE_VERSION

```sql
CREATE TABLE subroute_version (
    subroute_version_id  BIGINT PRIMARY KEY,

    route_version_id     BIGINT NOT NULL,
    subroute_id          BIGINT NOT NULL,

    subroute_name        VARCHAR(...) NOT NULL,

    created_at           TIMESTAMP NOT NULL,

    FOREIGN KEY (route_version_id)
        REFERENCES route_version(route_version_id),

    FOREIGN KEY (subroute_id)
        REFERENCES subroute(subroute_id)
);

CREATE TABLE stop_of_route_version (
    stop_of_route_version_id  BIGINT PRIMARY KEY,

    subroute_version_id       BIGINT NOT NULL,
    stop_version_id           BIGINT NOT NULL,

    direction                 SMALLINT NOT NULL,
    stop_sequence             INTEGER NOT NULL,

    created_at                TIMESTAMP NOT NULL,

    FOREIGN KEY (subroute_version_id)
        REFERENCES subroute_version(subroute_version_id),

    FOREIGN KEY (stop_version_id)
        REFERENCES stop_version(stop_version_id)
);
```

### Constraints

同一 Route Version：

```text
(route_version_id, subroute_id)
```

必須 UNIQUE。

同一 SubRoute Version 同一 Direction：

```text
(subroute_version_id, direction, stop_sequence)
```

必須 UNIQUE。

如果業務規則禁止同一 Stop 在同一（SubRoute、Direction）出現兩次：

```text
(subroute_version_id, direction, stop_version_id)
```

也應 UNIQUE。

如果允許循環路線／同站多次停靠，則不要建立後者 constraint。

---

# 10. ROUTE VERSION 完整性

一個 Published Route Version 必須具備完整的 SubRoute 與站序集合：
每個 SubRoute 的每個 Direction 都有連續 stop_sequence。

例如：

```text
V10 / SubRoute 307A / Direction 0
1 → A
2 → B
3 → C
4 → D
```

不能 Published 成：

```text
V10 / SubRoute 307A / Direction 0
1 → A
2 → B
4 → D
```

除非業務明確允許 sequence gap。

建議 Publish 前由 application validation 檢查：

```text
- sequence valid（per SubRoute + Direction）
- no invalid Stop reference
- required fields complete
- duplicate sequence absent
- Route-specific business rules satisfied
```

---

# 11. TRIP

```sql
CREATE TABLE trip (
    trip_id            BIGINT PRIMARY KEY,

    route_id           BIGINT NOT NULL,
    route_version_id   BIGINT NOT NULL,
    subroute_id        BIGINT NOT NULL,
    direction          SMALLINT NOT NULL,

    occurred_at        TIMESTAMP NOT NULL,

    created_at         TIMESTAMP NOT NULL

    -- other business fields
);
```

### Critical rule

`trip.route_version_id` 是歷史真相。

不要在報表執行時重新：

```text
trip.route_id
    ↓
current_route_version_id
```

---

# 12. REPORT_SNAPSHOT

```sql
CREATE TABLE report_snapshot (
    report_id       BIGINT PRIMARY KEY,

    snapshot_at     TIMESTAMP NOT NULL,

    snapshot_type   VARCHAR(...) NOT NULL,

    status          VARCHAR(...) NOT NULL,

    created_at      TIMESTAMP NOT NULL,

    completed_at    TIMESTAMP NULL
);
```

建議 status：

```text
CREATING
READY
FAILED
EXPIRED
```

---

# 13. REPORT_ROUTE_SNAPSHOT

```sql
CREATE TABLE report_route_snapshot (
    report_id          BIGINT NOT NULL,
    route_id           BIGINT NOT NULL,
    route_version_id   BIGINT NOT NULL,

    PRIMARY KEY (
        report_id,
        route_id
    ),

    FOREIGN KEY (report_id)
        REFERENCES report_snapshot(report_id),

    FOREIGN KEY (route_id)
        REFERENCES route(route_id),

    FOREIGN KEY (route_version_id)
        REFERENCES route_version(route_version_id)
);
```

### Rule

Report READY 後：

```text
report_route_snapshot
```

不得修改。

---

## REPORT_STATION_GROUP_SNAPSHOT

OD 類報表依賴組站位時：

```sql
CREATE TABLE report_station_group_snapshot (
    report_id                BIGINT NOT NULL,
    station_group_id         BIGINT NOT NULL,
    station_group_version_id BIGINT NOT NULL,

    PRIMARY KEY (
        report_id,
        station_group_id
    ),

    FOREIGN KEY (report_id)
        REFERENCES report_snapshot(report_id),

    FOREIGN KEY (station_group_id)
        REFERENCES station_group(station_group_id),

    FOREIGN KEY (station_group_version_id)
        REFERENCES station_group_version(station_group_version_id)
);
```

Report READY 後同樣不得修改。

---

# 14. OUTBOX_EVENT

```sql
CREATE TABLE outbox_event (
    event_id         BIGINT PRIMARY KEY,

    aggregate_type   VARCHAR(...) NOT NULL,
    aggregate_id     BIGINT NOT NULL,

    event_type       VARCHAR(...) NOT NULL,

    payload          TEXT NOT NULL,

    created_at       TIMESTAMP NOT NULL,
    published_at     TIMESTAMP NULL
);
```

Publish Route 時：

```text
Route Version
SubRoute Version
StopOfRoute Version
Current Pointer
Outbox Event
```

必須同一 DB transaction commit。

---

# 15. Recommended Indexes

最低限：

```text
route_version
    (route_id, version_no)

route_version
    (route_id, valid_from)

subroute_version
    (route_version_id)

stop_of_route_version
    (subroute_version_id, direction, stop_sequence)

stop_of_route_version
    (subroute_version_id, stop_version_id)

stop_version
    (stop_id, valid_from)

station_version
    (station_id, valid_from)

station_group_version
    (station_group_id, valid_from)

trip
    (route_version_id)

trip
    (occurred_at)

report_route_snapshot
    (report_id)

report_station_group_snapshot
    (report_id)

outbox_event
    (published_at, created_at)
```

實際 index 必須依 production query plan 調整。

---

# 16. Referential Integrity

以下 FK 必須存在：

```text
route_version.route_id
        → route.route_id

subroute.route_id
        → route.route_id

subroute_version.route_version_id
        → route_version.route_version_id

subroute_version.subroute_id
        → subroute.subroute_id

stop_of_route_version.subroute_version_id
        → subroute_version.subroute_version_id

stop_of_route_version.stop_version_id
        → stop_version.stop_version_id

stop_version.stop_id
        → stop.stop_id

stop_version.station_id
        → station.station_id

station_version.station_id
        → station.station_id

station_version.station_group_id
        → station_group.station_group_id

station_group_version.station_group_id
        → station_group.station_group_id

trip.route_version_id
        → route_version.route_version_id

report_route_snapshot.route_version_id
        → route_version.route_version_id

report_station_group_snapshot.station_group_version_id
        → station_group_version.station_group_version_id
```

歷史資料不得依賴 application convention 才能維持 reference integrity。

---

# 17. Delete Policy

Published historical records：

### MUST NOT hard delete

包括：

```text
route_version
subroute_version
stop_of_route_version
stop_version
station_version
station_group_version
historical trip
report snapshot
```

應使用：

```text
retention / archival
```

而不是直接 DELETE。

如果法規要求刪除，必須另外定義：

```text
legal deletion policy
```

不能把一般 CRUD delete 當成歷史資料治理。

---

# 18. Version Creation

新增 Version 時：

```text
route_version_id = new ID
version_no = previous + 1
```

不要：

```text
UPDATE old route_version
```

---

# 19. Current Pointer

Publish V11：

```text
route.current_route_version_id
        ↓
V11
```

V10 仍存在。

因此：

```text
current state
```

與：

```text
historical state
```

可以同時存在。

---

# 20. Database Invariants

DB + application 必須共同保證：

```text
I01  Published Version immutable
I02  stop_id / station_id / station_group_id never reused
I03  Route Version belongs to exactly one Route
I04  SubRoute Version belongs to exactly one Route Version;
     StopOfRoute Version belongs to exactly one SubRoute Version
I05  Trip preserves Route Version + SubRoute + Direction
I06  Report Snapshot preserves Route Version
I07  Current pointer references a published Version
I08  Publish is atomic
I09  Route valid intervals do not overlap
I10  Published Report Snapshot is immutable
I11  Membership（Stop→Station、Station→StationGroup）
     lives in child version, immutable once published
```

---

# 21. DB-neutral Temporal Validation

如果 DB 不支援 temporal constraint：

Publish 時：

```text
BEGIN

lock route 307

query existing published intervals

verify:
    new.valid_from >= previous.valid_from
    no overlap
    valid_to > valid_from

create version

COMMIT
```

重點不是 SQL 語法，而是：

> **Validation 必須發生在 concurrency-controlled transaction 裡。**

---

# 22. Example: 307

Initial:

```text
307 / V1（SubRoute 307A / Direction 0）

1 A
2 B
3 C
4 D
```

B 改名：

```text
307 / V2

1 A
2 B'
3 C
4 D
```

刪 C：

```text
307 / V3

1 A
2 B'
3 D
```

103 重用為 E：

```text
307 / V4

1 A
2 B'
3 E
4 D
```

歷史永遠存在：

```text
V1
V2
V3
V4
```

---

# 23. Final DB Rule

> **Never mutate history to represent the present. Create a new Version and move the current pointer.**
