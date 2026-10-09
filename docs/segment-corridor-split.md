# Segment corridor split — 設計 handoff

日期：2026-10-07
狀態：**歷史決策記錄**。segment registry 已於 2026-10-09 移除（migration 0011）；其長度聚類閾值語意由 rm-verify 的 `pair:spread` 檢查延續（gap > max(100m, 3% × 較短)）。本文保留為設計史。

原本狀態：**已實作**（commit bd1aaab）。三城結果：taichung 9954 segments / 22 split pairs、taipei 7317 / 25、tainan 5019 / 18。
前置：docs/segment-canonical-cut.md（pair-only identity，commit 0d9e69f）

## 背景

Pair-only identity（站對 = segment）消除了重複 segment，但把「同站對、
真正不同的走廊」合併成一個 segment。典型案例：星動銀河旅站 → 干城站，
6 條路線走雙十路 oneway 環繞（~600m）、2 條路線走公園路捷徑（~390m），
兩條都是真實街道路徑，應為 2 個 segment。

## 案例分類學（驗證數據，Taichung）

| 案例 | spread | 中間街道 | 期望 |
|---|---|---|---|
| 干城 雙十路 vs 公園路 | 219m | 不同名 | 拆 |
| 臺灣大道港埠→大庄（車道對） | 0m | 同名 | 合 |
| 東大庄→中港高中（車道對） | 0m | 同名 | 合 |
| 大庄橋→大庄（車道對） | 32m | 同名 | 合 |
| route 70 src polyline spike（1 路線） | 902m | 同名 | 拆（單路線 segment 可接受） |

關鍵觀察：**長度差就是分界線**。Taichung 597 個 multi-variant pairs 的
spread 分佈：雜訊帶（車道/邊界）≤ 96m，真走廊帶 ≥ 148m，中間空白。
純度量即可分類，不需要街道名或幾何距離。

## 決議

**長度聚類切分**：站對群組內按 draft 長度聚類，相鄰 gap 超過閾值即分簇，
每簇一個 segment。無最小群組數限制（單路線簇獨立成 segment）。

閾值：`gap > max(100m, 3% × 較短側長度)`

- 絕對 100m：分開雜訊帶（≤96m）與真走廊帶（≥148m），取安全中值。
- 相對 3%：保護長走廊 — 31km 中橫便道段（舊佳陽↔上台電，兩路線差
  345m = 1.1%）不誤拆；對 148m+ 真走廊案例無影響。

否決的替代方案：

- 街道名序列簽名：679副 經中央路二段 connector 名序不同會誤拆（使用者
  明示該案 = 同一 segment）；OSM 改名造成 identity 漂移。
- 幾何 polyline 距離聚類：route 70 spike 凸出 450m 會誤拆、成本最高，
  一樣需要長度護欄。

## 設計

### 1. Identity（rm-registry/src/hash.rs）

```
segment_id = blake3_128("{A_ext_uid} | {B_ext_uid} | {corridor_key}")
corridor_key = 簇內長度中位數量化到 10m 的十進位字串（如 "600"；
              偶數簇取較低中位數）
pair_key     = blake3_128("{A_ext_uid} | {B_ext_uid}")   // 現行 segment_id 公式，QA 分組用
```

簇間 gap > 100m 保證 corridor_key 不碰撞。corridor_key 不含 way_sig →
OSM way 改名/切段不影響 segment ID 穩定性。

### 2. Registry（rm-registry/src/registry.rs）

現行兩階段 run()（收集 cuts → 按 segment id 分組）中，分組 key 改為
pair_key；組內新增聚類步驟：

1. cuts 按 length 排序，相鄰 gap > max(100m, 3% × 較短側) 即分簇。
2. 每簇各自走現有 `representative()`（多數決 way_sig、簇內
   variant_count、簇內 length_spread_m）。
3. 每簇寫一個 seg_segment（segment_id 含 corridor_key，另存 pair_key
   與 corridor_key 欄位）、seg_segment_way 取簇代表、seg_traversal
   連到所屬簇的 segment。

常數：`CORRIDOR_GAP_ABS_M: f64 = 100.0`、`CORRIDOR_GAP_REL: f64 = 0.03`。

### 3. Schema（rm-db/migrations/0009_corridor_split.sql）

```sql
ALTER TABLE seg_segment ADD COLUMN pair_key TEXT NOT NULL DEFAULT '';
ALTER TABLE seg_segment ADD COLUMN corridor_key TEXT NOT NULL DEFAULT '';
```

Registry 每次全量重建，無需回填。

### 4. rm-verify / rm-web

- `seg:variants` 檢查改為 group by `pair_key`：列出 pair_key 對應多個
  segment 的站對（= 走廊切分清單，供人工審閱）。檢查統計鍵改為
  split pairs 數。
- segment_variant_report.json：pair → 各 segment（長度、way_sig、路線）。
- rm-web `/api/qa/variants`：回傳 pair 分組，每對含多個 segment；
  panel 每個 segment 各自可點開 segment detail。chip = split pairs 數。
- 簇內 variant_count/length_spread_m 保留原語義（簇內車道雜訊量）。

### 5. 測試（TDD）

- hash.rs：corridor_key 參與 ID 凍結向量；pair_key 凍結向量（= 舊
  segment_id 公式）。
- registry 整合測試：
  - 同站對 2 direct + 2 detour（gap > 閾值）→ 2 個 segment（pair_key
    相同、segment_id 不同），traversal 各連所屬 segment。
  - 同站對等長變體 → 1 個 segment。
  - 單路線長變體 → 獨立 segment。
- rm-verify：split pair 進 report + warn 計數。
- rm-web：variants API pair 分組、兩個 segment 各自帶路線清單。

## 模擬結果（規則 = max(100m, 3%)，三城現有 DB）

| 城 | split pairs | segments | 單路線簇 |
|---|---|---|---|
| Taichung | 22 | 9932 → 9954 | ~28 |
| Tainan | 18 | 5001 → 5019 | ~24 |
| Taipei | 25 | 7291 → 7317 | ~22 |

人工抽查（全部 65 個 split pairs 的詳細清單）：

- 不同街名案例：全為真走廊差異（干城雙十/公園路、翁子萬年路、潭子
  391巷、輔大站 loop、景美、舊莊…）。
- 同街名案例：多為真 one-way couplet / 圓環 loop（長度差 39–54%），
  如民權吉林路口 450 vs 323m、捷運輔大站 352 vs 229m。
- 無車道雜訊誤拆：車道對 spread ≤ 96m 全數低於閾值。
- route 70 src spike（902m、1 路線）→ 自成單路線 segment（src 問題
  本身不處理，資料如實呈現）。
- 31km 中橫便道 jitter（345m = 1.1%）被 3% 相對護欄擋下，不拆。

## 已知取捨

- 同街名、長度差 100–150m 的小 loop（如中營興業里 126m、南港機廠
  131m）會被切分 — 屬真實路徑差異，切分後仍在 QA 報告可人工複核。
- 等長不同街的走廊（罕見；現有數據中此形狀皆為車道對）gap 不到閾值
  拆不開 → 維持同一 segment，靠 way_sig 呈現差異。
- corridor_key 基於長度：走廊長度隨 OSM 資料微調時 bucket 可能跳檔
  （機率低，10m 量化 + 100m 分離），registry 全量重建語義下可接受。
