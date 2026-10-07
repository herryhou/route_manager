# Segment canonical cut（跨 route 統一切點）handoff

日期：2026-10-07
狀態：設計討論中，**尚未實作**。前期工作（方案 A：snap-down + sliver + dedup）已完成，見 `segment-handoff.md`。

## 脈絡

方案 A 上線後三城市殘留 dup pairs：taichung 597、tainan 131、taipei 508。分類（taichung）：

- **559（94%）核心 ways 不同** = 真實路徑差異（雙 carriageway、繞路、不同進場、out-and-back、專用道），不應合併
- **≤38（6%）只差邊界一個 way** = 正規化殘留，切點抖動/拓撲表達差異所致 —— canonical cut 的目標

## 大庄案例解剖（設計依據）

站對：臺灣大道港埠路口 → 大庄(浩天宮)，~621m，15 traversals。切成 4 個 segment：

| segment | ways | routes | 說明 |
|---|---|---|---|
| `bbc483b0` | 九段+八段前段 | 8 | 多數 |
| `0da0e45e` | …+中央路二段 11.2m | 679副2 | 多一條 |
| `fd1174c6` | …+中央路+八段後 21.5m | 679副 | 多兩條 |
| `d3bf82a8` | 對側 carriageway | 4 | 真差異 ✓ |

前 3 個是同一實體路，差異機制（已用 node 證據定案）：

1. **站的實體位置無歧義**：15 條 route 的 大庄 投影點（`path_stop_projection.projected_lat/lon`）全在 15m 內；bbc 組與 679副2 只差 1~2m
2. 兩組 route 從**同一節點** `1307103282` 進入 212731490（臺灣大道八段前段）
3. 差別在轉出點：
   - 306（bbc 組）：直行通過中央路口（內部節點 `1378323901`），續走到 1273m 處才轉出
   - 679副2：在 `1378323901`（= `764235572` 中央路二段的 entry node）轉出，經 11.2m 中央路接回八段後段（212731492）
4. **關鍵認知：run 邊界 = 轉彎點，不是路口**。直行通過的路口不會切開 run，所以 306 的 run 看起來「路口在 963m 外」——那是 run 終點（它自己的轉出點），不是中央路口
5. 結論：同一個物理直行動作，兩種 OSM 拓撲表達（way 中段通過 vs 轉出-經過-轉回）→ way 清單差中央路一條 → hash 不同

統一切點在 `1378323901` 可合併：306 窗口縮 26m（清單不變）、679副2 窗口止於路口（中央路排除）→ 兩邊清單一致。

### 已修正的錯誤假設（避免重蹈）

- ~~「8 票 J0、2 票 J1，投票可決」~~：錯。8 條 route 的站投影在 run 中段（距任何 run 邊界 300m+），在 v1 設計下根本不投票
- ~~「679副/679副2 路徑分歧 ~1km」~~：錯。run 長度誤導（runs 止於轉彎點）；投影實體座標證明兩組貼合同一點。分歧的是「轉出點位置」，不是走廊

## 設計演進

### v1（已否決）：run-boundary 投票

每站對「最近的 run 邊界節點」投票，多數決。**缺口**：直行通過路口的 route，
該路口不是它的 run 邊界 → 投不了票。大庄案例直接暴露此缺口（多數組無票可投）。

### v2（現行設計）：junction-node 投票

```
Phase 1（投票）：對每個 (route, direction) 的每次停靠 S：
  候選 = S 投影點物理距離 R 米內、屬於該 route 匹配路徑所經 way 的
         junction 節點（osm_way_node 中屬於 ≥2 條 way 的節點）
  每個候選節點得一票（可加權 1/(1+d)）
  → 不論 route 有沒有在此轉彎，路過就能投

Phase 2（決議）：group by (station_id, node_id)
  接受條件：n_top ≥ 2 且過半；平手比平均距離；再平手 → 無 canonical
  樞紐站（多走廊票分散）自然 fallback

Phase 3（切段）：pair (A,B) 在 route R 上：
  找 R 的路徑中 entry/exit == canonical_node(A) 的位置 → lo（B 同理 hi）
  找不到（R 未經該節點）→ fallback snap_down
  lo ≥ hi → fallback raw；沿 stop 序單調 clamp
```

Fallback 鏈不變：canonical node → snap-down（現行）→ raw projection。

### 參數待定（需實測分佈）

| 參數 | 初值 | 依據 |
|---|---|---|
| R（投票半徑，物理距離） | 30m | 大庄案例 306 的站-路口距離 26.5m；原 20m 會漏票 |
| 權重 | 1/(1+d) vs 純計數 | 待模擬比較 |
| 過半門檻 | >50% | 樞紐站 fallback 的保險 |

## 下一步

1. **模擬**（不做實作前先算收益）：掃 38 個 boundary-only pairs，對每端站套 v2 投票規則，算出多少 pair 會收斂（預期大庄案例收斂）；同時掃 R=20/25/30 的票源分佈
2. **TDD 實作**：registry.rs 增加 `collect_votes()` / `resolve_canonical()` 純函式 + 兩階段 `run()`；現有 `cut_window`/`slice_ways` 保留為 fallback
3. **驗證**：rerun 三城 → dup pairs 降幅、skipped pairs 不變、rm-verify all pass、boundary-only 分類重算

## 相關檔案

- `crates/rm-registry/src/registry.rs` — 現行切段（A 已上線：`cut_window`/`slice_ways`）
- `crates/rm-osm-match` — `seg_match_way`（entry/exit node 來源）、`path_stop_projection`
- `osm_way` / `osm_way_node` / `osm_node` — junction 拓撲（v2 投票候選來源）
- `docs/segment-handoff.md` — 前期：問題定義 + 方案 A 實作結果
