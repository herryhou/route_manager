# Segment 重複問題 handoff

日期：2026-10-07
狀態：方案 A 已實作（taichung 驗證通過）；規則為 **snap-down** 變體，見下文「實作結果」

> 實作筆記：最初採「最近 junction」雙向 snap，天冷案例殘留 3 個 segment（短 run
> 17m、站點近中間，最近邊界方向逐 route 翻轉）。定案改 **單向 snap-down**：
> 切點只貼「≤ c 的最近 junction」，lo/hi 皆然；相鄰 segment 在同一 junction
> 無縫相接。SLIVER_M 由 3 提至 6 吸收殘餘短 run 案例。

## 問題

同一對「前站→後站」存在多個 segment（`seg_segment` 多列）。直觀上同一對站、走同一條路，應該只有一個 segment。

各城市現況（taichung.db / tainan.db / taipei.db）：

| 城市 | segments | 相異站對 | 站對有多個 segment |
|---|---|---|---|
| taichung | 10883 | 9932 | 806 |
| tainan | 5194 | 5001 | 171 |
| taipei | 8128 | 7291 | 637 |

## 根本原因

### segment_id 的定義

`crates/rm-registry/src/hash.rs:8`

```
segment_id = blake3_128("{A站ext_uid} | {B站ext_uid} | w1|w2|…|wn")
```

way 清單（travel order）已納入 hash。問題不在 hash，在於**各 route 切出的 way 清單不一致**。

### 切段邏輯

`crates/rm-registry/src/registry.rs:114-131`（`run()` 內）：

- 每個 route direction：stops（含投影 chainage，來自 `path_stop_projection`）+ way runs（`seg_match_way`，Viterbi 匹配結果，各有 `cum_start_m`/`cum_end_m`）
- 對相鄰站對 (lo, hi)：收「與 (lo,hi) 視窗重疊 > 1e-6 的 runs」成 way 清單 → hash

### 不一致的來源（以臺中 天冷→大同橋 為例，5 個 segment）

全部走台8東關路四段，核心主幹（1149579011→1149579016→1149579015，約 500m）完全相同。差異全在邊界：

1. **切點在 way junction 附近的進出翻轉**（主因）
   - 天冷站離 609374024 run 起點 6.2 / 6.5 / 7.0 / 9.1 / 16.1m（各 route 投影不同）
   - 大同橋站離 1149579012（大同橋）run 起點 0.3 / 0.4 / 7.7m
   - 切點落在 junction 內外差幾公尺 → 邊界 way 有無被涵蓋翻轉 → hash 不同
   - 例：`015c78f8` 有 0.4m 的 1149579012 尾、`6f244197` 沒有 → 兩個 id
2. **匹配到不同進場路徑**
   - `153`/`207` 經無名 service road（711269513、238995112）接回主線，其他 route 直走東關路（609374025）→ way 清單本質不同
3. **同一 way 重複出現**
   - `153` 的 1149579011 分兩段進出（141m + 163m）→ 清單內重複 → hash 不同

5 個 segment 明細（taichung.db）：

| segment_id | len_m | way 數 | 進場 | 尾端 |
|---|---|---|---|---|
| `015c78f8…` | 535.4 | 6 | 直走東關路 | +1149579012 (0.4m) |
| `6f244197…` | 537.6 | 5 | 直走東關路 | 無 |
| `924d71d4…` | 543.7 | 7 | +豐埔公路 386971585 (2.5m) | +1149579012 (0.3m) |
| `3648e5e8…` | 539.2 | 8 | service 側線 | +1149579012；1149579011×2 |
| `eacd66c0…` | 540.4 | 7 | service 側線 | +1149579012 (7.7m) |

使用 routes：153、153副、153延、207、207繞、266、266繞1、266繞2、267、821、850、865、865區、90延、黃9（皆 PROJECTED、無 outlier，span−seg_len 差 0~6m 純投影誤差）。

## 目標（使用者定義）

前站、後站相同，且經過一樣的 OSM ways → 視為同一個 segment。

注意：進場路徑本質不同（service 側線 vs 主線）**不**合併——way 不同。

## 方案

### A. 切點正規化（snap to junction）— 已選定，未實作

改 `registry.rs` 切段處（`run()` 內，約 L114-131）：

1. 對每個 stop chainage c：若 c 距所在 run 的 `cum_start_m` 或 `cum_end_m` < ε（建議 20m），把切點移到該 junction
2. 相鄰同名 way 合併（重複的 1149579011 → 一筆）
3. `segment_id()` 函式（hash.rs）不動

Guards（必做）：
- 兩站 snap 到同一點 → way 清單空 → 退回原始 chainage（避免新增 skipped_pairs）
- 兩站相距 < ε 且 snap 方向相反 → lo > hi → clamp

效果（天冷→大同橋）：5 → 2 segments（主線組 `015c78f8`/`6f244197`/`924d71d4` 合一；service 組 `3648e5e8`/`eacd66c0` 合一）。

已知後果：
- `INSERT OR IGNORE`（registry.rs L58）first-writer-wins：共用 id 後 `encoded_polyline`/`length_m` 取第一個寫入 route 的 draft，各 route 差 2–8m。語義變成「way 層級相同」，長度是代表值
- segment 變多 route 共用後，`seg_geometry_draft` 查找邏輯（per subroute/direction/ordinal）不用改，但只有第一份會存進 `seg_segment`

實作 + 驗證步驟：
1. 改 `registry.rs`（約 40 行）
2. `cargo run -r -p rm-registry -- --db data/taichung.db`（冪等，seg_* 全刪全建）
3. 驗證 SQL：
   ```sql
   SELECT count(*) FROM (
     SELECT 1 FROM seg_segment GROUP BY start_station_id, end_station_id HAVING count(*) > 1);
   ```
   taichung 806 應大幅下降；天冷→大同橋應 = 2
4. `rm-verify` 跑一輪確認無新 skipped_pairs

### B. 事後分組（未選）

加 `seg_segment_group` 表，用正規化 signature（站對 + 去 sliver/de-dup 的 way 清單）分組，查詢/顯示層合併。不動 pipeline、風險零，但 id 仍有兩個，只在 view 層「視為同一個」。若 A 的 first-writer-wins 副作用不可接受，退回此案。

### C. 幾何模糊合併（Hausdorff/Frechet < ε）— 否決

會連 service 側線都合併，違反「經過一樣的 osm-way」條件。

## 實作結果（2026-10-07，taichung）

`registry.rs` 新增 `cut_window()`（snap-down，SNAP_EPS_M=20）與 `slice_ways()`
（連續同 way 合併 + 邊界 sliver < SLIVER_M=6 捨棄），8 個單元測試（TDD）。

| 指標 | 前 | 後 |
|---|---|---|
| segments | 10883 | 10613 |
| dup pairs | 806 | 597 |
| skipped pairs | 2 | 2 |
| traversals | 36531 | 36531 |
| 天冷→大同橋 | 5 | **2**（主線 13 routes / service 2 routes） |

剩餘 597 dup pairs 抽查為真實路徑差異（例：會展中心→經貿黎明路口，一條多
27m 中段 link 256936159），符合「way 不同即不同 segment」語義。
`rm-verify`（taichung）all checks pass。

三城市結果（rm-verify 皆 all checks pass；tainan/taipei 需 `--preset`）：

| 城市 | segments 前→後 | dup pairs 前→後 | skipped pairs | traversals |
|---|---|---|---|---|
| taichung | 10883→10613 | 806→597 | 2（不變） | 36531（不變） |
| tainan | 5194→5146 | 171→131 | 156（與改前 baseline 相同） | 8330（不變） |
| taipei | 8128→7925 | 637→508 | 34（與改前 baseline 相同） | 27381（不變） |

## 相關檔案

- `segment-canonical-cut.md` — 後續：殘留 dup pairs 分析 + 跨 route canonical cut 設計（討論中）
- `crates/rm-registry/src/registry.rs` — 切段、建 segment/traversal（核心修改點）
- `crates/rm-registry/src/hash.rs` — segment_id 定義（不動）
- `crates/rm-osm-match` — Viterbi 匹配、`seg_match_way` 來源、`path_stop_projection`
- `seg_segment` / `seg_segment_way` / `seg_traversal` — 資料表（見 `docs/DB_Schema_Spec.md`）
- 下游：`rm-web` segment detail 頁直接讀 `seg_segment`
