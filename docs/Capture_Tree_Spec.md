# Capture Tree 契約規格(rm-ingest 輸入)

**Status:** Implementation Specification
**Normative source:** `crates/rm-ingest/src/parse.rs` — 本文件與該檔必須同步修改;兩者不一致時以程式碼為準並回補文件。
**Scope:** 「城市轉換器」(`organize_*.py`)與 `rm-ingest` 之間的中介資料契約。任何城市,只要產出這棵樹,管線(Rust)零修改即可匯入。
**相關文件:** `Architecture_design_spec.md`(身分概念)、`DB_Schema_Spec.md`(staging 表)、`Tainan_ingest_eval.md`(城市轉換器完整範例)、`assets/{Taichung,Tainan}/*_API.md`(原生 API)。
**History:** 2026-10-05 v1 — 自 parse.rs 萃取,併入台南轉換器(`organize_tainan.py`)實作經驗。

---

## 1. 概念與身分語義

整條管線的站-杆-路線三層身分,由 capture 樹直接決定:

| 概念 | 欄位 | 定義 |
|---|---|---|
| **Route(路線)** | `routes.json` 的 `node.id`(= xno) | 路線唯一識別。同時是 `raw/route_{xno}.json` 檔名的一部分,兩處必須**字串完全一致**。 |
| **Station(站)** | `raw` stations 的 `node.id` | 實體站位(桿群)。**必須全域**:同一 `node.id` 出現在任何路線、任何方向 = 同一站。城市 API 若無此欄位,轉換器必須自行推導(見 §7.3)。 |
| **Stop(杆/站牌)** | `sid` | 「某路線某方向經過的這根桿」— per-occurrence,路線+方向範圍內即可,跨路線重複無妨(台中 6,107 個 sid 跨路線共用;台南 stopid 零重複,兩者皆合法)。 |
| **stop_sequence** | `orderNo` | 方向內行進順序,**1 起算**,在 `(route, direction)` 內唯一。同一站可在同方向重訪(循環/繞駛) — 重訪時 `node.id` 相同、`sid` 不同。 |
| **Direction(方向)** | `goBack` | `1` = 去程(入庫為 direction 0)、`2` = 返程(direction 1)。解析器負責轉換。 |

## 2. 目錄配置

```
<source>/
├── routes.json               # 必須
├── providers.json            # 必須(城市無業者資料 → 空 edges)
├── main-side-routes.json     # 必須(無資料 → [])
├── jointOperationsBuses.json # 可省略(不存在即略過;無資料給空物件)
└── raw/
    └── route_{xno}.json      # 每路線一檔;{xno} 與 routes.json node.id 字串完全一致
```

- 編碼一律 UTF-8。JSON 建議 `ensure_ascii=False`(中文站名直接可讀、可 diff)。
- **稽核鏈說明**:`src_file` 表記的 sha256 是**轉換後**這棵樹的檔案,不是原生 API capture。原生 capture 保存在城市目錄的 `raw/` 下,是另一層保存,不進這個契約。

## 3. 檔案規格

以下 `?` = 可 null 或整個欄位省略。型別為 JSON 型別;`i64` = JSON 整數。

### 3.1 `routes.json` — 路線表頭

```json
{"data": {"routes": {"edges": [
  {"node": {
    "id": "1", "name": "1", "seq": 1,
    "departure": "A", "destination": "B", "description": "A - B",
    "isCycled": false, "opType": 0,
    "providers": {"edges": [{"node": {"id": "5"}}]}
  }}
]}}}
```

| 欄位 | 型別 | 必要 | 語義 |
|---|---|---|---|
| `id` | string | ✅ | 路線 xno。建議數字字串(排序:數值優先,非數字字串殿後)。 |
| `name` | string | ✅ | 對乘客顯示的路線號(route_code)。城市原樣保留(如 `101路(原1路)`)。 |
| `seq` | i64 | ✅ | 官方顯示順序。 |
| `departure` / `destination` | string | ✅ | 起訖站名。 |
| `description` | string | ✅ | 描述(慣例 `起 - 訖`)。 |
| `isCycled` | bool | ✅ | 循環路線。無此資訊的城市以「起訖相同」推導。 |
| `opType` | i64 | ✅ | 城市路線類型代碼,1:1 數值透傳,語義由城市自訂(過濾策略見 §7.9)。 |
| `providers` | object? | ⬜ | 業者代碼列表(`edges[].node.id`)。省略 = 無。 |

### 3.2 `providers.json` — 業者主檔

```json
{"data": {"providers": {"edges": [
  {"node": {"id": "5", "name": "台中客運", "telephone": "04-2222", "opType": 0}}
]}}}
```

| 欄位 | 型別 | 必要 | 語義 |
|---|---|---|---|
| `id` | string | ✅ | 業者代碼;`routes.json` / `raw` 的 provider 引用即指此。 |
| `name` / `telephone` | string | ✅ | 業者名稱/電話(無資料給 `"-"` 類佔位亦可)。 |
| `opType` | i64 | ✅ | 透傳。 |

城市無業者資料(台南)→ `{"data": {"providers": {"edges": []}}}`。

### 3.3 `main-side-routes.json` — 主線/支線對應

```json
[{"xno": 1, "sideRoutes": "2,999", "children": [2, 999]}]
```

- 頂層是**陣列**(非 GraphQL 包裝)。
- `xno`: number 或 string 皆可;`children` 優先,無 children 才用 `sideRoutes` CSV 字串(逗號切割、trim、去空)。
- 解析後 (main, side) 配對排序 + 去重;懸空引用(xno 不在 routes.json)是**警告**不是錯誤。
- 無資料 → `[]`。

### 3.4 `jointOperationsBuses.json` — 聯營(可省略)

```json
{"data": {"jointOperationsBuses": [
  {"busProvider": "四方公司", "providerId": 5, "routeId": 3}
]}}
```

- 檔案**不存在即整段略過**(台南轉換器輸出空物件,亦可)。
- `providerId` / `routeId` 可為 number 或 string(內部統一轉字串)。

### 3.5 `raw/route_{xno}.json` — 路線明細(站序 + 幾何)

```json
{"data": {"route": {
  "stations": {"edges": [
    {"goBack": 1, "orderNo": 1, "ivrNo": 101, "sid": 11,
     "node": {"id": "44", "name": "干城站", "lat": 24.1409, "lon": 120.6857}}
  ]},
  "routePoint": {"go": "_p~iF~ps|U", "back": null},
  "providers": {"edges": [{"node": {"id": "5"}}]}
}}}
```

`stations.edges[]` 每筆(注意:edge 本身直接攜帶 goBack/orderNo/sid/node,**沒有** relay 包裝):

| 欄位 | 型別 | 必要 | 語義 |
|---|---|---|---|
| `goBack` | i64 | ✅ | 只能 `1`(去程)或 `2`(返程);其他值 = 硬錯誤。 |
| `orderNo` | i64 | ✅ | 方向內順序,1 起算,`(xno, goBack)` 內唯一。 |
| `ivrNo` | i64? | ⬜ | 電話查詢碼。無 → null 或省略。 |
| `sid` | i64 | ✅ | Stop(pole occurrence)id,見 §1。 |
| `node.id` | string | ✅ | Station id,**全域**(§1、§7.3)。 |
| `node.name` | string | ✅ | 站名。解析器會 trim;**站名比對用 trim 後值**,轉換器輸出前最好先 strip。 |
| `node.lat` / `node.lon` | number | ✅ | **該 occurrence 自己的座標**(不是叢集中心)— 台南雙向桿各自保持真實位置即靠此。 |

`routePoint`:
- `go` / `back`: Google encoded polyline,**精度 1e-5**(與 `rm_core::polyline` 互逆)。無 → null、省略欄位、或整個 `routePoint` 省略,staging 一律存空字串。
- 單方向路線:存在的方向給值,另一方向 null。全無幾何的路線可進入(配對/驗證階段會回報 skipped)。

`providers`: 該路線業者;與 routes.json 表頭的業者集合不一致 → 警告。

`data.route` 為 `null` = **硬錯誤**(整批失敗);城市 API 明細抓不到時,轉換器應直接不輸出該檔(產生「header 無 raw」警告)。

## 4. 解析器做的正規化(轉換器不必做,但輸出前做亦無害)

- `goBack` 1/2 → `direction` 0/1;站名 trim;polyline null → `""`。
- 路線排序:xno 數值優先、非數字殿後(`xno_sort_key`)。
- 每檔記 sha256 + 列數進 `src_file`(kind:`providers`/`route_header`/`main_side_route`/`joint_operation`/`route_detail`)。
- 每路線**恆寫兩筆** `src_route_point`(direction 0 與 1,無幾何者 `encoded=''`)。
- `lat`/`lon` 另存 e6 整數(`lat_e6`/`lon_e6`)。

## 5. 硬錯誤 vs 警告

**硬錯誤**(parse 失敗 → 整批不寫入;若重用到既有 OPEN batch,該 batch 標 `FAILED`):

- 必要檔缺失、JSON 壞、非 UTF-8;
- `data.route` 為 null;
- `goBack` ∉ {1, 2};
- `main-side-routes` 的 `xno` 非 number/string。

**警告**(寫入 `Summary.warnings`,排序去重,不擋匯入):

- route header 無對應 raw 明細;raw 明細無對應 header;
- main-side 引用不存在的路線;
- 同路線 header 與 raw 的業者代碼集合不一致。

**冪等**:同一 `source_dir` 存在 OPEN batch 時重用之;該 batch 既有 staging rows 先全刪再寫。重跑轉換器後直接重跑 rm-ingest 即可覆蓋。

## 6. staging 對應總表

| capture 來源 | staging 表 |
|---|---|
| routes.json node | `src_route_header`(xno, route_code, departure, destination, description, seq, is_cycled, op_type) |
| routes.json providers | `src_route_provider`(xno, provider_code) |
| providers.json node | `src_provider`(provider_code, name, telephone, op_type) |
| raw stations edges | `src_stop_edge`(xno, direction, source_order_no, ivr_no, sid, node_id, station_name, lat, lon, lat_e6, lon_e6) |
| raw routePoint | `src_route_point`(xno, direction 0/1 各一筆, encoded) |
| main-side-routes.json | `src_main_side_route`(main_xno, side_xno) |
| jointOperationsBuses.json | `src_joint_operation`(bus_provider, provider_code, route_xno) |
| 全部檔案 | `src_file`(kind, name, sha256, row_count) |

## 7. 城市轉換器責任清單(接新城市時逐項打勾)

1. **路線表頭**:逐欄位填 §3.1;`isCycled` 無來源就以「起=訖」推導;`opType` 透傳城市類型代碼。
2. **業者**:無 API → 空 edges,勿虛造。
3. **Station id 推導**(城市無全域站位 id 時的核心工作)。台南先例(`organize_tainan.py`,與 `rm-identity/grouping.rs` 同規則):
   - 站名 trim 後完全相同者為候選;兩兩 haversine **< 80 m** 則 union(union-find 傳遞閉包);
   - 決定論順序掃描(路線數字 id → 方向 → 站序), 叢集 id 按首見順序鑄造(台南格式 `T%06d`),叢集屬性(名稱/座標)取首見 occurrence;
   - **每個 edge 的 `node.lat/lon/name` 仍填該 occurrence 自己的值** — 叢集 id 只負責身分,座標負責位置。
4. **sid**:城市的 per-occurrence 站牌 id 原樣放入(整數)。
5. **方向旗標**:城市旗標 → `goBack` 1/2(台南 `goback` 0→1、1→2)。
6. **站序**:方向內 1 起算、唯一;城市 payload 無序號就用陣列索引(1-based)。
7. **幾何**:城市點列 → Google polyline 1e-5 編碼後放入 `routePoint.go/back`(台南 `ReadText` 點列 `{X, Y}` → 編碼)。
8. **main-side / joint**:城市無此概念 → `[]` / 空物件 / 省略 joint 檔。
9. **類型過濾政策**:非固定路線型(如台南小黃巴士 DRTS)在轉換器排除,且排除規則寫進該城市的 eval 文件(台南教訓:官方 typeid 與檔案類型名不一致,以**類型名**過濾才對)。
10. **自檢輸出**:轉換器印統計(routes_kept、stop_rows、stations、無幾何路線清單),供人工核對。
11. **驗收**:`rm-ingest` → `rm-verify --preset <城市>`;形狀疑慮時對照 `crates/rm-ingest/src/testfix.rs`(最小合法樹,含雙方向路線、單方向路線、ivrNo null、懸空 side ref)。

## 8. 兩城市現況與已知怪癖

| | 台中 | 台南 |
|---|---|---|
| 來源 | 原生 capture 即本格式,`rm-ingest` 直接吃 `assets/Taichung/` | `organize_tainan.py` 產出 `assets/Tainan/capture/` 再匯入 |
| Station id | API `node.id` 原生全域 | 轉換器合成(`T%06d`,名+80 m 叢集) |
| DB | `data/taichung.db` | `data/tainan.db`(城市各自 DB;`tcg:` 前綴兩城共用數字空間,共庫前必須先參數化前綴) |

已知資料怪癖(可匯入,QA 會浮現,不需轉換器修):同方向重訪站(台南 1129 煙子林)、單方向路線(兩城皆有)、全無幾何路線、幾何起點離首站 >150 m(台南 29 條,最遠 `1032_back` 14.9 km;台中 258 尾巴同類)。
