# PRODUCT.md — rm-web

Date: 2026-10-13 · 為前端重新設計而寫的現況盤點。資料來源：`crates/rm-web/README.md`、`docs/web-edit-mode.md`、`docs/refresh-button.md`、`assets/` 原始碼。

---

## 這是什麼

rm-web 是公車路網資料 pipeline 的**Web 前端**：一個 axum binary 同時伺服 Leaflet 地圖前端與小型 JSON API，直接讀 pipeline 產出的 SQLite 資料庫（`data/taipei.db`、`taichung.db` 等，一城一檔）。預設是唯讀瀏覽器；加 `--edit` 旗標後開放編輯模式，讓人直接在地图上修資料。

上游 pipeline：`rm-ingest → rm-identity → rm-osm-match → rm-registry / rm-segments → rm-verify`，把台中/台北等城市的 GTFS-ish 捕捉資料加工成 stops、stations、routes、matched OSM ways、segments。rm-web 是這條 pipeline 的「眼睛與手」。

## 為誰設計

- **主要使用者：資料維護小組（含開發者本人），1–5 人的可信內部團隊。** 不是公開網站，沒有訪客、註冊、多租戶。
- 兩種使用情境：
  1. **QA 檢視**（唯讀）— pipeline 跑完後，用眼睛驗證匹配品質：這條方向配得乾不乾淨、outlier 是來源資料爛還是投影錯位、共用走廊有沒有對齊。
  2. **編輯**（auth）— 修 QA 找出的壞資料（站牌位置、名稱）、整理結構（stop↔station 歸組）、修正線 polyline。
- 使用裝置：**桌機瀏覽器**。所有核心互動（拖曳、map-click place、hover tooltip）都以滑鼠為前提。

## 解決什麼問題

Pipeline 的產出是資料庫列（rows），但使用者真正要問的問題是**地圖問題**。沒有 rm-web，這些問題得貼 SQL 進 shell。rm-web 把每個圖層一個 SELECT 包成 HTTP，讓：

1. **QA 不用寫 SQL** — 打開瀏覽器就看得到匹配結果、偏差值、outlier 清單。
2. **修資料不用下 CLI** — 看到錯的站牌直接拖到對的位置；編輯以「新版本列」寫入（version-append），資料庫本身就是 working master，留完整 audit trail（edit_log）與 undo。
3. **檢視與 pipeline 共存** — 所有讀取走 `SQLITE_OPEN_READ_ONLY`，伺服器可以一直開著，pipeline 重跑不互相干擾（WAL）。

## 功能清單

### 瀏覽（唯讀）

| 功能 | 說明 |
|---|---|
| 路線搜尋 | 側欄搜尋框：路線代碼 prefix / 名称 / 營運商子字串（client-side `filterRoutes`），同時 server-side 搜站名（debounce，臺/台互通） |
| 路線繪製 | 選路線畫出方向路徑（實心藍；`no_path` 段紅虛線）+ 依投影方式著色的站牌點；去程/返程切換；有序站牌清單 |
| 路徑幾何來源 | `route path` 下拉：provider polyline 或 matched OSM ways，即時重繪 |
| 點擊 path piece | 開 piece 面板，該段紅色高亮 |
| 站牌 drill-down | 點站牌 → popup（bearing 羅盤、名稱、經過路線 chips）→ 面板逐層：stop → station → station group，各層可服路線；所有交叉引用可點 |
| 視窗圖層 | Stops/stations 圖層按 bbox 載入：zoom 12–14 畫 station、≥15 畫個別 stop（pan debounce） |
| QA chips | 頂欄顯示 dataset 統計（routes/stops/stations/matched）；outliers 與 path dev 兩顆可點、非零轉紅 |
| QA outlier 面板 | 最差站牌投影（snap > 50 m 或 `OUTLIER`/`NO_PATH`），列點位可 pan |
| QA path dev 面板 | matched OSM 路徑偏離來源 polyline 的方向（p95 > 50 m），同 rm-verify 報表 |
| QA profile | 每路線面板一個 tab：snap distance 沿里程的 SVG scatter；路線列表圓點依 p50 snap 著色（綠 <15 m / 黃 <40 m） |
| OSM ways overlay | 選路線後疊 matched street ways（綠虛線，切到進出 node 窗口） |
| Raw compare | 疊最新 batch 的原始 staging 資料（`src_route_point` 虛線 + `src_stop_edge` 方塊）對照 canonical |
| Effective line | 顯示 match 實際吃的線（override ?? src） |
| Bearings | dot+arrow 依出站朝向旋轉的 marker，預設開，側欄可關 |
| 雙面板歷史 | 詳細面板兩層堆疊（context / point），各自獨立 ᐸ/ᐳ 歷史，不重抓 |
| URL hash 分享 | `#route=1&dir=0`、`#stop=123`、`#station=45` 開機時重放 |
| 快捷鍵 | `Esc` 關閉鏈（popup → point pane → context pane）、`/` 聚焦搜尋 |

### 編輯模式（`--edit` + bearer token）

| 功能 | 說明 |
|---|---|
| Edit toggle | 頂欄 ✎（server 回報 `edit_enabled` 才顯示）；token 進 sessionStorage |
| 搬站牌 | 編輯模式下 stop marker 可拖；放下 → confirm（移動距離 + note）→ POST 新版本 |
| 改名 | stop / station 面板 pencil → `prompt()` 輸入新名 |
| Regroup | 把 stop 改歸另一個 station：arm one-shot place mode（十字游標）→ 點地圖目標 station → 距離 confirm |
| 搬 station | 同 place mode 改 station 座標 |
| 線編輯 | `lineEdit.js`：effective line 全線可編 — 拖頂點、dblclick 刪點、 midpoint 插入；存檔整條取代該方向 polyline；帶 `expected_updated_at` 樂觀鎖 |
| 歷史 + undo | 每實體 edit_log 面板，可 revert（undo of undo = 再編輯） |
| Stale badge | 編輯使衍生資料過期 → 受影響面板 amber badge（`stale_mark` 表驅動） |
| Apply updates | 頂欄 ⟳：單一 flight 的 in-server refresh 鏈（match → segments → registry 的 `--stale`），顯示目前步驟與剩餘 mark 數 |
| 無聲編輯禁止 | 每次寫入都要 explicit confirm + note |

## 目前畫面元素

```
┌────────────────────────────────────────────────────────────┐
│ topbar  « | rm-web | routes·stops·stations·matched 統計chips │
│         | outliers* pathdev* | ✎edit | ⟳apply-updates      │   * 可點、非零紅
├──────────┬─────────────────────────────────┬───────────────┤
│ sidebar  │                                 │ detail panel  │
│ 搜尋框    │                                 │ (選取才出現)    │
│ 路線列表   │          Leaflet 地圖            │ ┌─context pane┐│
│ (或站名列)│   路徑線/站牌/bearing/疊圖        │ │ ᐸ ᐳ X 標題  ││
│          │   編輯 handle(編輯模式)           │ └─內容────────┘│
│ Layers   │                                 │ ┌─point pane──┐│
│ ☐stops   │                                 │ │ ᐸ ᐳ X 標題  ││
│ ☑bearings│                                 │ └─內容────────┘│
│ path▾    │                                 │               │
│ ☐raw     │   [存線][✕] ← 線編輯浮動鈕(右下)   │               │
│ ☑eff     │                                 │               │
│ legend   │                                 │               │
├──────────┴─────────────────────────────────┴───────────────┤
│ statusbar  spinner·訊息          | basemap 需網路提示         │
└────────────────────────────────────────────────────────────┘
```

元素清單：

- **topbar**：側欄收合鈕 `«`、標題、4 顆統計 chip、2 顆可點 QA chip（outliers / path dev）、edit toggle、apply-updates 按鈕（編輯模式）。
- **sidebar**：搜尋框（路線與站名二合一，站名結果暫時取代路線列表）、路線列表（數字開頭優先；圓點 = 每方向 QA 色點）、Layers 區（4 個 checkbox + 1 個幾何來源下拉 + 圖例）。
- **地圖**：Leaflet，全部圖層的畫布。popup 兩種（stop / station）。編輯模式的 handle（頂點圓點、midpoint 白點、十字游標 place）。
- **detail panel**：兩層堆疊 pane。context pane 放路線詳情 / QA 清單；point pane 放 stop / station / path piece。各自 ᐸ ᐳ 歷史 + X。
- **statusbar**：busy spinner、狀態訊息、basemap 提示。
- **浮動元素**：線編輯的 存線/✕ 按鈕（地圖右下）。
- **面板內容型別**：路線詳情（providers、方向列表、QA profile tab）、有序站牌表、piece 面板、outlier 表、path-dev 表、stop 卡（羅盤 + 經過路線 + 編輯列）、station 卡（group、成員、bearing 軸、編輯列）、edit-log 表、popup。

## 設計限制（重新設計時的邊界）

### 架構不變量

1. **單 binary、零 runtime 檔案**：所有 static assets `include_str!` 內嵌，Leaflet vendored。重新設計若引入 build step（ bundler / framework ），必須回答「產物怎麼回到 `assets/` 並內嵌」。
2. **讀寫分離**：viewer 永遠 readonly connection；寫入走 `/api/edit/*` + auth middleware。前端重新設計不得打破「瀏覽不變更狀態」的保證。
3. **編輯 = 版本附加**：所有寫入是 append 新 version row + audit log + stale mark，不是 UPDATE。UI 的責任：每筆編輯 confirm + note、顯示 stale badge、提供 undo。API 不可同步等待 refresh 完成（refresh 是單一 flight 的長任務，按鈕輪詢狀態）。
4. **server 擁有地理數學**：client 只收集 `[lat,lon]`（lineEdit.js 明文約定）。前端不做 snap、投影、polyline 運算。
5. **API 形狀**：GET 全部 JSON、錯誤 non-2xx `{"error"}`；編輯以 current version id 定址。重新設計可以包新 UI，但換 API = 動 axum handlers + 測試。

### 資源限制

6. **無 JS framework、無 build step、無 JS 測試**：vanilla ES modules，約 3,000 行（app.js 900、panels.js 619、style.css 804）。JS 驗證靠手動 + 瀏覽器檢查。重新設計要自帶測試策略。
7. **單人維護**：複雜度預算極低。伺服器目前開一個 port、無部署管線；`--db` 換檔即換城。
8. **離線優先（basemap 除外）**：basemap tiles 需網路，其餘全部本地。statusbar 有固定提示文案。

### 互動限制

9. **桌機滑鼠前提**：拖曳編輯、map-click place、dblclick 刪點皆無 touch 替代路径。目前無 responsive 設計（有 viewport meta 但版面是固定三欄）。
10. **小團隊單寫入**：寫入以 app-level mutex 序列化，無樂觀併發 UI（僅線編輯帶 `expected_updated_at`）；無 per-field 權限、無使用者管理畫面。設計不需做多人在線、衝突合併 UI。
11. **原生對話框**：改名/notes 用 `prompt()`、確認用 `confirm()` — 是已知粗糙點，重新設計可替換，但「明確 confirm + note」的行為必須保留。
12. **中文為主 UI**：站名/方向（去程/返程）/存線等字串為台灣慣用語；無 i18n 機制。

### 已知痛點（重設計的動機清單）

- marker z-order 靠 `zIndexOffset` 魔術數字（1000/1001）撐住編輯 handle 不被重繪蓋掉 — `lineEdit.js:49,71`。
- 單一 `app.js` 900 行，狀態（選取、圖層、編輯 session、pane 歷史）分散在模組頂層變數。
- `prompt()`/`confirm()` 原生對話框與整體 UI 風格斷裂。
- 無任何 JS 測試；回歸靠手動瀏覽器檢查（最近 8 項 UI 檢查仍待人工驗證）。
- 編輯 arm/place 模式（十字游標 one-shot）可發現性低 — 新使用者難以知道目前處於「待點擊」狀態。
