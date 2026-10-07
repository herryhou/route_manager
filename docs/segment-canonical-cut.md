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

## 模擬結果（2026-10-07）

以 Python 重製 registry 切段（`cut_window`/`slice_ways` 逐行對照 Rust 實作，36,531
traversals 全數重現、way 清單 0 mismatch），對 taichung.db 只讀模擬 v2 投票 + canonical
重切。投票者 = 全部 37,270 個 PROJECTED 停靠（24 個 OUTLIER 排除）；候選 = 該方向
`seg_match_way` 所經 way 上、haversine 距投影點 R 米內的全域 junction 節點
（osm_way_node 中 ≥2 條相異 way；node 在 route 路徑上重複出現時取離該停靠 chainage
最近的 occurrence —— 模擬假設，見文末邊界情況）。

### 邊界 boundary-only 重新嚴格分類

嚴格規則：兩清單剝除「兩端各 ≤2 way、每端剝除米數 ≤50m」後核心（有序 way 清單，
非空）相同。結果：**196 pairs → 177 clusters、320 個端點站**（597 dup pairs 中）。

| 分類規則 | pairs |
|---|---|
| 嚴格規則（≤2 way/端、≤50m/端、核心非空） | **196** |
| 敏感度 K=1、M=80、K=3 | 151 / 282 / 199 |
| 舊「各去首尾再比核心」literal 規則 | 18 |
| 單 way 延伸（一清單 = 另一清單 ± 1 way） | 142 |

原「≤38」估計偏低的原因：literal 去首尾規則連大庄 bbc/0da 都抓不到
（[W1,W2,八前] vs [W1,W2,八前,中央] 去首尾後 [W2] vs [W2,八前] 不相等）。
「94% 核心不同」的舊分類裡其實混有為數不少的 boundary-only pairs。

### 各組態收斂數字

12 組態全跑（R × 純計數/1(1+d) 權重 × 過半分母 cast/voters）。指標：收斂 clusters
（該 cluster 全部 traversals 重切後清單一致）、修復 dup pairs、**誤傷 splits**
（原單一 segment 的站對被切成多清單）、淨 dup pairs（baseline 597）：

| 組態 | 收斂 clusters | 修復 | splits | 淨 dup |
|---|---|---|---|---|
| R=20 count cast / voters | 56 / 63 | 45 / 53 | 36 / 38 | 588 / 582 |
| R=25 count cast / voters | 60 / **80** | 46 / 66 | 44 / 47 | 595 / **578** |
| R=30 count cast / voters | 65 / **87** | 50 / 70 | 49 / 55 | 596 / 582 |
| R=25 inv cast / voters | 79 / 77 | 65 / 65 | 52 / 48 | 584 / 580 |
| R=30 inv cast / voters | 81 / 84 | 67 / 69 | 60 / 56 | 590 / 584 |

- 收斂上限 87/177（49%）；最佳淨 dup 578（−19，約 3%）。所有組態 skipped pairs 維持 2。
- R=30 count voters：segments 10613 → 10584（收斂合併掉 90 個，splits 卻新增 61 個）。
- 大庄案例**確實收斂**：3 個 segment 全部重切為 [359380170, 212731490]，B 端切點回退
  −26.5~−31.9m 至 1378323901；對側 carriageway `d3bf82a8` 保持獨立（其清單 [359193613,
  764235578] 仍不同）。但注意：cast 分母下該站 12/24 = 0.50 恰不過半（反方向 25.6m 有
  競爭 junction 2406685037 拿 9 票、1378323909 拿 3 票，部分 route 同時投多個），
  **voters 分母（12/18 = 0.667）才通過**；1/(1+d) 權重也能救。

### 未收斂原因分類（R=30 count voters，90 個未收斂 clusters，原因可複選）

| 原因 | clusters | 說明 |
|---|---|---|
| 切點不改變清單 / 切了仍分歧 | 73 | 最大宗，三種機制見下 |
| fallback_mixed（節點不在少數 route 路徑上） | 17 | 對側廊道 route fallback → 仍分歧 |
| no_canonical（兩端站皆無法決議） | 10 | |

站端決議失敗結構：320 個端點站中 **93 個（29%）完全無票** —— 30m 內、該 route 自己
匹配 way 上根本沒有 junction 節點（候選限定在投票者路徑所經 way 上）；no_majority
僅 6 站、n_top<2 僅 3 站。票分散不是主要障礙，**無票與「投出的節點不是差異點」才是**。

「切了仍分歧」73 clusters 的機制（實例）：

1. 投出的 junction 與清單分歧處無關：切窗移動了，邊界 way 照舊（[2484→495]、[2903→2586]）
2. 同一節點在各 route 的 way 拓撲位置不同（節點是某 way 內部節點 vs way 端點），切在同一
   節點上首/尾 way 仍翻轉（[648→2447]：route 6 切點在節點後 +3.5m，其餘在節點前 −10.7m）
3. 分類為 boundary-only 實為折返（out-and-back）：[218023008] vs [218023008, 241745773,
   218023008]，切完仍兩清單（[806→81]）

### 誤傷風險評估

splits 機制與投票品質無關（R=30 inv voters 下把多數門檻提高到 0.6/0.7：收斂 84→81→78、
splits 56→55→54，幾乎不動）：

1. **mixed_apply（47/55）**：全場一致的高品質投票也會 split —— 節點不在某 route 的匹配
   路徑（平行廊道/對側），該 route fallback、其餘 route 切在節點 → 同一站對清單分歧
   （例 [1673→1671]，B 站 9/9 一致投票仍 split）
2. **all_applied_diverge（8/55）**：切點移到投票節點使窗口延長、越過站前的短 way，暴露
   原 chainage 切點下看不見的 per-route 拓撲差異（例 [4120→318]：單 way segment，
   部分 route 多出一條 1050737598）

變體測試：**pair-level 共識**（同站對同端，全數 route 都能用該節點才一起切，否則全數
維持現行）—— splits 減半（55→31）但收斂也減半（87→44），淨 dup 不變（587）。瓶頸不在
fallback 語義，在上面兩個結構性缺口。

### 參數建議與結論

- 若實作：R=30（20m 會漏大庄 306 的 26.5m 票）、權重 1/(1+d) 優於純計數（站決議 217 vs
  174、收斂 81 vs 65 @cast）、**過半分母必須用投票者數而非總票數**（一人可投多候選；
  cast 分母讓大庄恰好 0.50 被拒）。
- **不建議實作 v2 原案**：最佳組態收斂 49%（87/177），淨 dup 最佳 −19（578；3%），
  代價是 ~50 個原本正確的站對被打成多 segment；修復:splits ≈ 1.3:1。
  三個結構性障礙（29% 端點站
  無票、投出節點常非差異點、per-route fallback 製造新分歧）都不是調參可解。
- 若要再推進，方向：(a) pair-level 共識 + 只處理「兩端都高票」的 cluster，接受低覆蓋率
  換零誤傷；(b) 對「切點不改變清單」類型改用邊界 way 層級的訊號（如以站為單位對
  「第 k 個 way 是誰」投票），而非只投節點。

模擬的邊界情況與假設（結果解讀時留意）：junction = ≥2 條**相異** way（way 自重複不算，
osm_way_node 有 612 筆 (way,node) 重複列）；站 id 本身已分方向（大庄 1901/1902 各自投
票）；loop 路徑上節點重複出現取最近 chainage 的 occurrence；單一 (route,direction) 多次
停靠同站 = 多個投票者；canonical 重切只影響該站對兩端，未模擬整條 route 相鄰站對的
單調 clamp 連鎖（real pipeline 需沿 stop 序 clamp，效果只會更保守）。


## 決議（2026-10-07）：不實作 canonical cut，改 pair-only identity

模擬否決 v2 後，segment identity 改為**站對本身**：

```
segment_id = blake3_128("{A站ext_uid} | {B站ext_uid}")
```

way 清單不再是身份，降級為資料：`seg_segment` 存多數決代表變體（`variant_count`、
`length_spread_m` 兩欄標記分歧），`seg_traversal.way_sig` 記每條 route 實走的
way 清單簽名。schema = migration `0008_pair_only`。

三城結果（rm-verify all pass，skipped pairs / traversals 不變）：

| 城市 | segments | dup pairs | 多變體 pairs | spread>200m |
|---|---|---|---|---|
| taichung | 9932 | **0**（原 597） | 597 | 15 |
| tainan | 5001 | **0**（原 131） | 131 | — |
| taipei | 7291 | **0**（原 508） | 508 | — |

案例：天冷→大同橋 1 segment / 2 variants（主線+service）；大庄 pair 1 segment /
4 variants（主線、+中央路、+中央路+八段後、對側 carriageway）。

已知取捨：15 對 >200m 繞路在 segment 層同 ID（per-route 真相仍在 way_sig /
seg_match_way）；segment 幾何 = 多數組代表值。
