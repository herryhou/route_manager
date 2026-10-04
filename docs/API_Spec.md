# Bus Route Versioning

## Backend API & Publish Workflow Specification

**Status:** Implementation Specification
**Scope:** Route / SubRoute Draft / Validation / Publish / Historical Query / Report Snapshot

---

# 1. API Design Principles

Backend API 必須區分：

```text
Draft mutation
Publish
Read current
Read historical
Create report snapshot
```

不要讓一般 CRUD API 直接修改 Published Version。

---

# 2. Route Read API

## Get Current Route

```http
GET /routes/{routeId}
```

Response 概念：

```json
{
  "routeId": 307,
  "routeCode": "307",
  "currentVersion": {
    "routeVersionId": 11,
    "versionNo": 11
  },
  "revision": 25
}
```

這個 API 回傳的是：

> Current State

不是歷史資料。

---

# 3. Get Specific Version

```http
GET /routes/{routeId}/versions/{versionId}
```

Response：

```json
{
  "routeId": 307,
  "routeVersionId": 11,
  "versionNo": 11,
  "validFrom": "...",
  "validTo": null,
  "subRoutes": [
    {
      "subRouteId": 30701,
      "subRouteName": "307",
      "routeStops": [
        {
          "direction": 0,
          "stops": [
            {
              "stopSequence": 1,
              "stopId": 1001,
              "stopVersionId": 2001,
              "stopCode": "101",
              "stopName": "A",
              "stationId": 501
            }
          ]
        }
      ]
    }
  ]
}
```

這個 API 必須直接讀指定 Version。

不得 silently fallback 到 current version。

---

# 4. Draft API

建立 Draft：

```http
POST /routes/{routeId}/drafts
```

Request：

```json
{
  "baseVersionId": 10
}
```

Response：

```json
{
  "draftId": 9001,
  "routeId": 307,
  "baseVersionId": 10,
  "status": "DRAFT"
}
```

---

# 5. Draft Mutation

Draft 的站序操作以 SubRoute + Direction 為單位。

例如新增 Stop：

```http
POST /routes/{routeId}/drafts/{draftId}/subroutes/{subRouteId}/stop-of-routes
```

```json
{
  "direction": 0,
  "stopSequence": 3,
  "stopVersionId": 2005
}
```

刪除 Stop：

```http
DELETE /routes/{routeId}/drafts/{draftId}/subroutes/{subRouteId}/stop-of-routes/{entryId}
```

修改 Stop：

```http
PATCH /routes/{routeId}/drafts/{draftId}/subroutes/{subRouteId}/stop-of-routes/{entryId}
```

### Important

這些 API 修改的是：

```text
Draft
```

不是：

```text
Published Route Version
```

---

# 6. Draft Revision

Draft SHOULD 有自己的 revision：

```text
draft_id
revision
```

例如：

```text
Draft 9001
revision = 8
```

Update 時：

```json
{
  "expectedRevision": 8,
  "name": "B中央站"
}
```

如果目前已經變成：

```text
revision = 9
```

API 應回：

```http
409 Conflict
```

而不是 silently overwrite。

---

# 7. Validate Draft

```http
POST /routes/{routeId}/drafts/{draftId}/validate
```

Response：

```json
{
  "valid": true,
  "errors": [],
  "warnings": []
}
```

可能 errors：

```text
STOP_NOT_FOUND
SUBROUTE_NOT_FOUND
INVALID_DIRECTION
DUPLICATE_SEQUENCE
INVALID_SEQUENCE
INVALID_EFFECTIVE_TIME
OVERLAPPING_VERSION
INVALID_STOP_STATE
```

---

# 8. Publish API

```http
POST /routes/{routeId}/drafts/{draftId}/publish
```

Request：

```json
{
  "expectedRouteRevision": 25,
  "validFrom": "2026-10-01T00:00:00Z"
}
```

---

# 9. Publish Response

成功：

```http
200 OK
```

```json
{
  "routeId": 307,
  "routeVersionId": 26,
  "versionNo": 26,
  "status": "PUBLISHED",
  "validFrom": "2026-10-01T00:00:00Z"
}
```

如果 concurrency conflict：

```http
409 Conflict
```

例如：

```json
{
  "code": "ROUTE_REVISION_CONFLICT",
  "currentRevision": 26
}
```

---

# 10. Publish Transaction

Backend 必須實作以下 transaction boundary：

```text
BEGIN
 │
 ├─ Lock Route
 │
 ├─ Check expected revision
 │
 ├─ Load Draft
 │
 ├─ Validate Draft
 │
 ├─ Validate effective interval
 │
 ├─ Create RouteVersion
 │
 ├─ Create SubRouteVersion
 │
 ├─ Create RouteStopVersion
 │    （引用既有 StopVersion）
 │
 ├─ Update current_route_version_id
 │
 ├─ Increment route.revision
 │
 ├─ Insert Outbox Event
 │
 COMMIT
```

任何一步失敗：

```text
ROLLBACK
```

---

# 11. Stop Version Creation Rule

當 Stop 修改：

```text
B
→
B中央站
```

不要：

```text
UPDATE stop_version
```

而是：

```text
old Stop Version
        ↓
new Stop Version
```

同一：

```text
stop_id
```

可以擁有多個 Stop Versions。

---

# 12. Stop Identity Reuse Rule

C 被刪除：

```text
stop_id = 1003
```

不能拿 1003 給 E。

E 必須：

```text
stop_id = NEW_ID
```

即使：

```text
stop_code = 103
```

也完全不影響 identity。

---

# 13. Route Change Algorithm

假設 current：

```text
307 / V1

A → B → C → D
```

使用者：

```text
rename B
remove C
add E
```

Draft：

```text
A → B' → E → D
```

Publish：

```text
V2
A → B' → E → D
```

Backend 不應逐筆 UPDATE V1。

應建立完整 V2。

---

# 14. Why Full Version Is Preferred

不要只保存：

```text
V2:
remove C
rename B
add E
```

作為唯一 Route Definition。

因為 Report 要回答：

> V2 完整 Route 是什麼？

最可靠的方式是：

```text
V2
A → B' → E → D
```

直接代表完整 state。

Change Log 可以另外保存，但不能取代 immutable Version。

---

# 15. Report Snapshot API

建立 Report：

```http
POST /reports
```

Request：

```json
{
  "snapshotAt": "2026-09-29T10:00:00Z",
  "routes": [307, 308, 309],
  "stationGroups": [71, 72]
}
```

Backend：

```text
BEGIN

resolve Route Version
for each requested Route

resolve StationGroup Version
for each requested StationGroup

create report_snapshot

insert report_route_snapshot

insert report_station_group_snapshot

COMMIT
```

---

# 16. Report Snapshot Response

```json
{
  "reportId": "R001",
  "snapshotAt": "2026-09-29T10:00:00Z",
  "status": "CREATING"
}
```

Snapshot 完成後：

```text
READY
```

---

# 17. Snapshot Resolution

如果：

```text
snapshotAt = 2026-09-29
```

Backend 必須 resolve：

```text
Route 307
→ Version whose valid interval contains snapshotAt
```

概念：

```text
valid_from <= snapshotAt
AND
(valid_to IS NULL OR snapshotAt < valid_to)
```

具體 SQL 由 DB 實作。

---

# 18. Once Snapshot Exists

Snapshot：

```text
R001
307 → V2
```

之後即使：

```text
307 → V3
```

R001 不變。

### MUST NOT

重新產生：

```text
R001 → current route
```

---

# 19. Report Execution

Report Query：

```text
Report
 ↓
Report Snapshot
 ↓
Route Version
 ↓
SubRoute Version
 ↓
RouteStop Version
 ↓
Stop Version
 ↓
Fact / Trip
```

而不是：

```text
Report
 ↓
Current Route
```

---

# 20. Historical Transaction Query

如果要查：

> 2026-02-15 307 經過哪些 Stop？

優先：

```text
Trip
→ route_version_id + subroute_id + direction
→ route_stop_version
→ stop_version
```

而不是：

```text
Trip
→ route_id
→ current route
```

---

# 21. Idempotency

Publish API SHOULD 支援 idempotency。

例如：

```http
Idempotency-Key: 8f3...
```

如果 client 因 timeout retry：

```text
POST publish
POST publish
```

不能產生：

```text
V11
V12
```

兩次相同 Publish。

應回傳同一個 Publish Result。

---

# 22. Outbox Event

成功 Publish：

```json
{
  "eventType": "RouteVersionPublished",
  "routeId": 307,
  "routeVersionId": 26,
  "versionNo": 26,
  "publishedAt": "..."
}
```

Event 必須在 Route transaction 中寫入 Outbox。

---

# 23. Reporting CDC

流程：

```text
OLTP
 ↓
Outbox / CDC
 ↓
Message Broker
 ↓
Reporting Consumer
 ↓
DIM_ROUTE_VERSION
DIM_SUBROUTE
DIM_STOP
DIM_STATION
DIM_STATION_GROUP
FACT_TRIP
```

Consumer 必須是 idempotent。

---

# 24. Error Handling

API 必須區分：

```text
400 Bad Request
```

輸入格式錯誤。

```text
404 Not Found
```

Route / Draft / Version 不存在。

```text
409 Conflict
```

Revision / concurrency conflict。

```text
422 Unprocessable Entity
```

Business validation failure。

```text
500 / 503
```

Infrastructure failure。

---

# 25. Concurrency Scenario

User A：

```text
讀 V10
revision = 20
```

User B：

```text
讀 V10
revision = 20
```

A Publish：

```text
V11
revision = 21
```

B Publish：

```text
expectedRevision = 20
```

結果：

```text
409 Conflict
```

B 必須：

```text
reload
→ compare changes
→ rebase / recreate Draft
→ publish
```

---

# 26. Report Concurrency Scenario

Report R001 開始建立 Snapshot：

```text
307 → V10
```

同時另一個 User Publish：

```text
307 → V11
```

只要 R001 的 Snapshot transaction 已確定：

```text
R001 → V10
```

R001 就繼續使用 V10。

不需要 rollback。

---

# 27. Acceptance Tests

## Test 1 — Rename Stop

Given：

```text
V1 = A → B → C → D
```

When：

```text
B → B'
```

Then：

```text
V1 = A → B → C → D
V2 = A → B' → C → D
```

---

## Test 2 — Delete Stop

Given：

```text
V2 = A → B' → C → D
```

When：

```text
remove C
```

Then：

```text
V3 = A → B' → D
```

And：

```text
V2 remains unchanged
```

---

## Test 3 — Stop Code Reuse

Given：

```text
C:
stop_id=1003
code=103
```

When E gets:

```text
code=103
```

Then:

```text
E.stop_id != 1003
```

---

## Test 4 — Historical Trip

Given:

```text
T001 → V1
```

When:

```text
307 → V4
```

Then:

```text
T001 → V1
```

---

## Test 5 — Concurrent Publish

Given:

```text
revision=10
```

Two users publish simultaneously.

Expected:

```text
exactly one succeeds
one receives 409
```

---

## Test 6 — Report Reproducibility

Given:

```text
R001
307 → V2
```

When Route becomes:

```text
V3
V4
V5
```

Then rerunning R001 must still resolve:

```text
307 → V2
```

---

## Test 7 — Atomic Publish

Force failure while creating RouteStop Version.

Expected:

```text
Route Version NOT visible as published
current pointer unchanged
Outbox Event NOT committed
```

---

# 28. Backend Code Review Checklist

每個 Route mutation PR 都應回答：

```text
[ ] 是否修改 Published Version？
[ ] 是否建立 New Version？
[ ] 是否有 revision check？
[ ] 是否有 transaction boundary？
[ ] 是否可能產生 overlapping valid interval？
[ ] 是否可能重用 stop_id / station_id / station_group_id？
[ ] SubRoute / Direction 是否完整保存？
[ ] 是否更新 current pointer？
[ ] 是否建立 Outbox Event？
[ ] 是否支援 retry/idempotency？
[ ] 歷史 Transaction 是否仍然指向原 Version？
[ ] Report Snapshot 是否保持 immutable？
```

---

# 29. Golden Rule for Developers

如果需求是：

> 「把現在的 Route 改成 X」

實作思維必須是：

```text
Current Version
      ↓
Create Draft
      ↓
Modify Draft
      ↓
Validate
      ↓
Create NEW Version
      ↓
Move Current Pointer
```

而不是：

```text
UPDATE current Route
```

---

# 30. Final Backend Contract

Backend 的責任可以濃縮為四句：

1. **Never mutate published history.**
2. **Bind transactions to versions when they occur.**
3. **Bind reports to versions when snapshots are created.**
4. **Publish a new Route Version atomically and move the current pointer only after success.**
