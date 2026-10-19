#!/usr/bin/env python3
"""Route schematic diagram generator — HTML + inline SVG from the rm-web API.

One entry point for the whole pipeline the rm-web-jobs skill uses to make
print-ready route diagrams (路線簡圖):

  1. fetch   — stops (in sequence, with inline route_codes) + matched OSM
               way runs, GET-only, from the running rm-web server. Data
               rule: rm-web API only, never direct sqlite.
  2. organize — monotonic DP assignment of stops onto way-run chords
               (metres-scaled point↔segment distance; never the
               path_stop_projection ↔ seg_match_way chainage join), then
               consecutive same-road consolidation.
  3. draw    — auto layout: both directions ≤50 stops -> single racetrack
               line (A4 landscape); >50 -> serpentine, 2 rows per direction,
               50 stops per row (pitch ≈19.6px is what keeps the 3-tier
               name scale legible).

Typography: stop names are typeset with CSS vertical writing
(writing-mode: vertical-rl + text-orientation: upright) so fullwidth （）
render their true vertical forms via the OpenType `vert` feature. Measured
vertical metrics (1em advance per char, ink bottom = em bottom) let columns
hang flush from the line; PAREN_END_GAP/PAREN_START_GAP compensate the
paren arcs' inset ink so arc and neighbouring CJK bottoms align. Explicit
per-char tspans pin identical positions for engines without vertical SVG
text.

Usage:
  python3 route_diagram.py --query 5 [--route-id 3] [--out route5-diagram.html]
                           [--num 5] [--start 臺中車站] [--end 僑光科技大學]
                           [--operator 全航客運] [--url http://127.0.0.1:8791]

Outputs the HTML plus a work dir (<out>.d/) with the cached API dumps and
check.txt (per-stop road assignment + snap distances) for review.
"""
import argparse
import json
import math
import os
import re
import urllib.parse
import urllib.request
from datetime import date
from pathlib import Path

W = 1123.0
X0, X1 = 80.0, 1040.0

INK = "#1C1917"
MUTED = "#78716C"
ROAD = "#57534E"
ACCENT = "#9A3412"
PAPER = "#FFFFFF"

# Paren arcs hug the bracketed content: ）ink bottom sits 0.49em above its
# cell bottom, （ink top 0.73em below its cell top (measured at 56px). The
# chosen offsets (user-tuned: exact-flush values are 0.49/0.73) shift whole
# columns so arcs visually align with CJK neighbours.
PAREN_END_GAP = 0.7
PAREN_START_GAP = 0.73


# ---------------------------------------------------------------- fetch ----

def api_get(base, path):
    with urllib.request.urlopen(f"{base}{path}", timeout=60) as r:
        return json.load(r)


def fetch(base, route_id, workdir):
    """GET both directions' stops + way runs; cache raw dumps in workdir."""
    data = {}
    for d in (0, 1):
        stops = api_get(base, f"/api/routes/{route_id}/directions/{d}/stops")
        ways = api_get(base, f"/api/routes/{route_id}/directions/{d}/ways")
        workdir.joinpath(f"stops{d}.json").write_text(json.dumps(stops, ensure_ascii=False))
        workdir.joinpath(f"ways{d}.json").write_text(json.dumps(ways, ensure_ascii=False))
        data[d] = (stops["stops"], ways["features"])
    return data


# -------------------------------------------------------------- organize ----

SUFFIX_RE = re.compile(r"[一二三四五六七八九十]+[段街巷]$")


def normalize_road(name):
    """Strip ordinal suffixes (一段/二段/三段/一街/一巷…) so spans merge
    across them; only roads covering ≥3 stops are drawn (spec 2026-10-19)."""
    if not name:
        return name
    return SUFFIX_RE.sub("", name) or name


def dist_point_seg(px, py, ax, ay, bx, by):
    """Distance from point P to segment AB (planar degrees scaled to metres)."""
    LAT_M = 111_320.0
    LON_M = 101_600.0  # Taichung latitude
    (px, py) = (px * LON_M, py * LAT_M)
    (ax, ay) = (ax * LON_M, ay * LAT_M)
    (bx, by) = (bx * LON_M, by * LAT_M)
    dx, dy = bx - ax, by - ay
    ll = dx * dx + dy * dy
    if ll == 0:
        return math.hypot(px - ax, py - ay)
    t = max(0.0, min(1.0, ((px - ax) * dx + (py - ay) * dy) / ll))
    return math.hypot(px - (ax + t * dx), py - (ay + t * dy))


def runs_from_features(feats):
    runs = []
    for f in sorted(feats, key=lambda f: f["properties"]["ordinal"]):
        p = f["properties"]
        (elon, elat) = p["entry_coords"]  # GeoJSON order [lon, lat]
        (xlon, xlat) = p["exit_coords"]
        runs.append({"ordinal": p["ordinal"], "name": normalize_road(p.get("name") or ""),
                     "elat": elat, "elon": elon, "xlat": xlat, "xlon": xlon})
    return runs


def assign_stops_to_runs(stops, runs):
    """Global monotonic assignment (DP): each stop -> a run chord, run index
    non-decreasing along the stop sequence, total distance minimal."""
    n, m = len(stops), len(runs)
    cost = [[dist_point_seg(s["lon"], s["lat"], r["elon"], r["elat"], r["xlon"], r["xlat"])
             for r in runs] for s in stops]
    INF = float("inf")
    dp = [[INF] * m for _ in range(n)]
    prev = [[None] * m for _ in range(n)]
    dp[0] = cost[0][:]
    for i in range(1, n):
        best, best_j = INF, None
        for j in range(m):
            if dp[i - 1][j] < best:
                best, best_j = dp[i - 1][j], j
            dp[i][j] = best + cost[i][j]
            prev[i][j] = best_j
    j = min(range(m), key=lambda x: dp[n - 1][x])
    idx = [0] * n
    for i in range(n - 1, -1, -1):
        idx[i] = j
        j = prev[i][j]
    return [{"seq": s["stop_sequence"], "name": s["stop_name"],
             "road": runs[k]["name"], "snap_m": round(cost[i][k], 1),
             "station_id": s["station_id"],
             "n_routes": len(set(s.get("route_codes") or []))}
            for i, (s, k) in enumerate(zip(stops, idx))]


def consolidate(rows):
    """Merge consecutive same-road stretches into labelled spans."""
    spans = []
    for r in rows:
        if spans and spans[-1]["road"] == r["road"]:
            spans[-1]["to_seq"] = r["seq"]
            spans[-1]["count"] += 1
        else:
            spans.append({"road": r["road"], "from_seq": r["seq"], "to_seq": r["seq"], "count": 1})
    return spans


# ----------------------------------------------------------------- draw ----

def tier_of(n):
    return 1 if n >= 15 else (2 if n >= 4 else 3)


def vname(x, y0, name, size, marker, down, unique=False):
    """Vertical name column (CSS vertical-rl + upright): fullwidth （） get
    true vertical forms, every char advances exactly 1em, ink bottom lands on
    the em bottom. down=False hangs UP from flush edge y0 (start = y0-n*size);
    down=True drops DOWN with first-char ink tops at y0 (start 0.14em higher —
    CJK ink fills the lower 86% of the cell). Paren-ending/starting columns
    shift by the measured arc gaps. Explicit per-char tspans pin the same
    positions in engines without vertical SVG text."""
    name = name.replace("(", "（").replace(")", "）")  # fullwidth: vert forms + 1em advance
    chars = (marker if unique else "") + name
    n = len(chars)
    fill = f' fill="{ACCENT}"' if unique else ""
    start = (y0 - n * size) if not down else (y0 - 0.14 * size)
    if not down and name.endswith("）"):
        start += PAREN_END_GAP * size     # push down: arc bottom lands on edge
    if down and name.startswith("（"):
        start -= PAREN_START_GAP * size   # pull up: arc top lands on edge
    body = "".join(f'<tspan x="{x:.1f}" y="{start + i * size:.1f}">{c}</tspan>' for i, c in enumerate(chars))
    return (f'<text x="{x:.1f}" y="{start:.1f}" class="vn v" '
            f'font-size="{size:.1f}"{fill}>{body}</text>')


# Road names: strip-merge leaves only ≥3-stop spans; each is drawn as a
# faded guide line between the stop dots and the name, name centred on it.
# Two lanes alternate so neighbouring names can't collide (same-lane
# neighbours are ≥6 pitches apart, wider than any name).
ROAD_FONT = 9.5
GUIDE = "#A8A29E"
LANE_BELOW = [(6.0, 17.0), (22.0, 33.0)]    # (guide_dy, text_baseline_dy), gap below the line
LANE_ABOVE = [(-6.0, -17.0), (-22.0, -33.0)]  # gap above the line


def road_marks(spans, x_of, below, y):
    raw = [{"label": s["road"], "xa": x_of(s["from_seq"]), "xb": x_of(s["to_seq"])}
           for s in spans if s["road"] and s["count"] >= 3]
    lanes = [raw[0::2], raw[1::2]] if len(raw) > 1 else [raw]
    out = []
    for g, (dy, ty) in zip(lanes, LANE_BELOW if below else LANE_ABOVE):
        for c in g:
            half = len(c["label"]) * ROAD_FONT / 2 + 4
            tx = min(max((c["xa"] + c["xb"]) / 2, 37.5 + half), W - 37.5 - half)
            out.append(f'<line x1="{c["xa"]:.1f}" y1="{y + dy:.1f}" x2="{c["xb"]:.1f}" y2="{y + dy:.1f}" stroke="{GUIDE}" stroke-width="1"/>'
                       f'<text x="{tx:.1f}" y="{y + ty:.1f}" text-anchor="middle" class="road" font-size="{ROAD_FONT}">{esc(c["label"])}</text>')
    return "".join(out)


def esc(s):
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def css_block():
    return """
:root {
  --bg: #EDEAE4; --fg: #292524; --muted: #79716B; --edge: #D6D0C6;
  color-scheme: light dark;
}
@media (prefers-color-scheme: dark) { :root:not([data-theme="light"]) {
  --bg: #201D1A; --fg: #E7E2DB; --muted: #A39B92; --edge: #3B362F;
} }
:root[data-theme="dark"] {
  --bg: #201D1A; --fg: #E7E2DB; --muted: #A39B92; --edge: #3B362F;
}
* { box-sizing: border-box; }
body {
  margin: 0; background: var(--bg); color: var(--fg);
  font-family: "Noto Sans TC", "PingFang TC", "Microsoft JhengHei", sans-serif;
  padding-left: max(16px, env(safe-area-inset-left, 0px));
  padding-right: max(16px, env(safe-area-inset-right, 0px));
  padding-block: 20px 28px;
}
.wrap { max-width: 1180px; margin: 0 auto; }
.scroller { overflow-x: auto; border: 1px solid var(--edge); border-radius: 10px;
  background: %s; box-shadow: 0 10px 30px rgba(0,0,0,.10); }
svg { display: block; width: 1123px; max-width: none; height: auto; }
figure { margin: 0; }
figcaption { margin-top: 12px; color: var(--muted); font-size: 13px; line-height: 1.7; }
figcaption b { color: var(--fg); font-weight: 500; }
.num { font-size: %%NUM%%px; font-weight: 900; fill: %s; letter-spacing: -2px; }
.eyebrow { font-size: 15px; font-weight: 500; fill: %s; letter-spacing: 6px; }
.title { font-size: %s; font-weight: 700; fill: %s; letter-spacing: 1px; }
.sub { font-size: 11.5px; fill: %s; }
.dir { font-size: 14px; font-weight: 700; fill: %s; letter-spacing: 2px; }
.vn { font-weight: 500; fill: %s; }
.vn.t1 { font-weight: 700; }
.v { writing-mode: vertical-rl; text-orientation: upright; }
.road { letter-spacing: 0.5px; fill: %s; }
""" % (PAPER, INK, ROAD, "%%TITLE%%", INK, ROAD, ACCENT, INK, ROAD)


def legend_svg(dirs, sizes, y, unique_marker_size):
    """Tier legend + one-way markers. Samples come from this route's data —
    shortest name per tier (a sample only has to show the type size); width
    math follows the drawn sample so long names can't overlap."""
    tier_names = {}
    for d in (0, 1):
        for r in dirs[d]["rows"]:
            tier_names.setdefault(r["tier"], []).append(r["name"])
    lx = 48.0
    leg = []
    for t, label in [(1, "≥15 路"), (2, "4–14 路"), (3, "≤3 路")]:
        names = tier_names.get(t)
        sample = min(names, key=len) if names else "站名"
        w = len(sample) * sizes[t]
        leg.append(f'<text x="{lx}" y="{y + 4}" class="vn t{t}" font-size="{sizes[t]}">{esc(sample)}</text>')
        leg.append(f'<text x="{lx + w + 8}" y="{y + 3}" class="sub">{label}</text>')
        lx += w + 8 + len(label) * 11 + 26
    for d, marker, label in [(0, "▲", "僅去程"), (1, "▽", "僅返程")]:
        u = next((r["name"] for r in sorted(dirs[d]["rows"], key=lambda r: len(r["name"])) if r["unique"]), None)
        if u is None:
            continue
        w = (1 + len(u)) * unique_marker_size
        leg.append(f'<text x="{lx}" y="{y + 4}" class="vn" font-size="{unique_marker_size}" fill="{ACCENT}">{marker}{esc(u)}</text>')
        leg.append(f'<text x="{lx + w + 8}" y="{y + 3}" class="sub">{label}</text>')
        lx += w + 8 + len(label) * 11 + 30
    leg.append(f'<text x="{lx}" y="{y + 3}" class="sub">字級＝該站服務路線數</text>')
    return "".join(leg)


def title_svg(num, start, end, operator, n0, n1, shared, u0, u1, serp):
    num_size = 84 if len(num) == 1 else 64
    num_x = 150 if len(num) == 1 else 210
    title_size = "28px" if serp else "30px"
    svg = []
    svg.append(f'<text x="48" y="{92 if serp else 96}" class="num">{esc(num)}</text>')
    svg.append(f'<text x="{num_x}" y="{46 if serp else 52}" class="eyebrow">臺中市公車</text>')
    svg.append(f'<text x="{num_x}" y="{88 if serp else 94}" class="title">{esc(start)} — {esc(end)}</text>')
    svg.append(f'<text x="{W - 48}" y="{46 if serp else 52}" text-anchor="end" class="sub">{esc(operator)}｜去程 {n0} 站 · 返程 {n1} 站</text>')
    svg.append(f'<text x="{W - 48}" y="{64 if serp else 70}" text-anchor="end" class="sub">共用站名 {shared} · 僅去程 {u0} · 僅返程 {u1}</text>')
    return svg, num_size, title_size


def wrap_html(svg, num, start, end, operator, W_H, caption):
    w, h = W_H
    css = css_block().replace("%%NUM%%", "84" if len(num) == 1 else "64").replace("%%TITLE%%", "28px" if h > 900 else "30px")
    return f"""<meta charset="utf-8">
<title>台中公車{esc(num)}路線圖</title>
<meta name="claude:viewport-fit-sides" content="cover">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Noto+Sans+TC:wght@400;500;700;900&display=swap">
<style>{css}</style>
<div class="wrap">
<figure>
<div class="scroller">
{''.join(svg)}
</div>
<figcaption>{caption}</figcaption>
</figure>
</div>"""


# --------------------------------------------------- layout: single line ----

def draw_single(dirs, meta):
    """Both directions on one racetrack: outbound L->R on top, return R->L on
    bottom, one smooth pipeline turn at the right, dashed same-stop 迴轉 at
    the left. Road names fully off the line, two lanes on the centre-strip
    side; names hang flush (top row: bottom edges; bottom row: top edges)."""
    H = 794
    Y0, Y1 = 420.0, 520.0
    n = max(len(dirs[0]["rows"]), len(dirs[1]["rows"]))
    STEP = (X1 - X0) / (n - 1)
    SIZES = {1: 14.0, 2: 11.5, 3: 9.5}

    def line_svg(rows, spans, line_y, rtl, names_above, marker):
        x_of = (lambda seq: X1 - (seq - 1) * STEP) if rtl else (lambda seq: X0 + (seq - 1) * STEP)
        # road lanes sit on the centre-strip side, opposite the stop names
        out = [road_marks(spans, x_of, below=names_above, y=line_y)]
        names = []
        for r in rows:
            x = x_of(r["seq"])
            t = r["tier"]
            out.append(f'<circle cx="{x:.1f}" cy="{line_y}" r="3" fill="{PAPER}" stroke="{ACCENT}" stroke-width="1.6"/>')
            names.append(vname(x, line_y - 14 if names_above else line_y + 14, r["name"],
                               SIZES[t], marker, down=not names_above, unique=r["unique"]))
        return "".join(out), "".join(names)

    body0, names0 = line_svg(dirs[0]["rows"], dirs[0]["spans"], Y0, rtl=False, names_above=True, marker="▲")
    body1, names1 = line_svg(dirs[1]["rows"], dirs[1]["spans"], Y1, rtl=True, names_above=False, marker="▽")

    svg = []
    svg.append(f'<svg viewBox="0 0 {W} {H}" role="img" aria-label="台中市公車{esc(meta["num"])}路線簡圖：{esc(meta["start"])}—{esc(meta["end"])}，去回程站序與道路名">')
    svg.append(f'<rect width="{W}" height="{H}" fill="{PAPER}"/>')
    t, num_size, _ = title_svg(meta["num"], meta["start"], meta["end"], meta["operator"],
                               len(dirs[0]["rows"]), len(dirs[1]["rows"]),
                               meta["shared"], meta["u0"], meta["u1"], serp=False)
    svg += t
    ly = 140
    svg.append(legend_svg(dirs, SIZES, ly, unique_marker_size=11.5))
    svg.append(f'<line x1="48" y1="{ly + 22}" x2="{W - 48}" y2="{ly + 22}" stroke="#E7E5E4" stroke-width="1"/>')

    svg.append('<defs><marker id="arw" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">'
               f'<path d="M 0 0 L 10 5 L 0 10 z" fill="{ACCENT}"/></marker></defs>')
    svg.append(f'<path d="M {X0} {Y0} L {X1} {Y0} C {X1 + 42} {Y0} {X1 + 42} {Y1} {X1} {Y1} L {X0 + 6} {Y1}" '
               f'fill="none" stroke="{ACCENT}" stroke-width="3" marker-end="url(#arw)"/>')
    svg.append(f'<path d="M {X0} {Y1} C {X0 - 24} {Y1} {X0 - 24} {Y0} {X0} {Y0}" fill="none" stroke="{MUTED}" stroke-width="1.6" stroke-dasharray="5 4"/>')
    svg.append(f'<rect x="{X0 - 32}" y="{(Y0 + Y1) / 2 - 8}" width="36" height="16" fill="{PAPER}"/>')
    svg.append(f'<text x="{X0 - 14}" y="{(Y0 + Y1) / 2 + 3.4}" text-anchor="middle" class="road">迴轉</text>')
    svg.append(f'<text x="1113" y="{Y0 - 8}" text-anchor="end" class="dir">去程 →</text>')
    svg.append(f'<text x="14" y="{Y1 + 24}" text-anchor="start" class="dir">← 返程</text>')

    svg.append(body0)
    svg.append(body1)
    svg.append(names0)
    svg.append(names1)
    svg.append("</svg>")

    caption = (f'<b>臺中市公車 {esc(meta["num"])}</b>（{esc(meta["operator"])}）：上線為去程（{esc(meta["start"])} → {esc(meta["end"])}，左至右），'
               f'下線為返程（右至左），首尾站靠左，右側為同一管線的平滑迴轉'
               + ('，左端虛線為端點同站迴轉' if meta["ends_shared"] else '') + '。'
               f'站名字級依該站服務路線數分三級，上排站名下緣切齊、下排站名上緣切齊；紅字＋▲／▽ 為僅單向停靠之站名。'
               f'道路名去除「段／街／巷」後綴合併，僅標示跨三站以上之路名，淡化直線連起其對應站點、路名置中。資料：rm-web（{esc(meta["db"])}）{date.today():%Y-%m-%d}。'
               f'列印：Cmd+P、A4 橫向、邊界「無」。')
    return wrap_html(svg, meta["num"], meta["start"], meta["end"], meta["operator"], (W, H), caption), H


# ------------------------------------------------ layout: serpentine 2x ----

def draw_serp(dirs, meta):
    """Serpentine (boustrophedon): 2 rows per direction, 50 stops per row,
    pitch ≈19.6px. dir0 starts top-right, drops at the LEFT connector;
    dir1 starts mid-left, drops at the RIGHT connector. Road chips straddle
    the line on the gap side (white plates), names own the outer side."""
    H = 1010
    P = (X1 - X0) / 49.0
    SIZES = {1: 12.0, 2: 10.0, 3: 8.5}
    # slot -> (y, rtl, names_above); dir0 uses slots 0-1, dir1 slots 2-3
    SLOTS = [(310.0, True, True), (450.0, False, False), (720.0, False, True), (860.0, True, False)]
    DIR_SLOTS = {0: (0, 1), 1: (2, 3)}

    def row_svg(rows, spans, slot, first_seq, marker):
        y, rtl, names_above = SLOTS[slot]
        lo, hi = rows[0]["seq"], rows[-1]["seq"]

        def x_of(seq):
            return (X1 - (seq - first_seq) * P) if rtl else (X0 + (seq - first_seq) * P)

        shown = []
        for s in spans:
            a, b = max(s["from_seq"], lo), min(s["to_seq"], hi)
            if a <= b:
                shown.append({"road": s["road"], "from_seq": a, "to_seq": b, "count": s["count"]})
        lane_below = slot in (0, 2)  # row A/C: gap below; row B/D: gap above
        out = [road_marks(shown, x_of, below=lane_below, y=y)]
        names = []
        for r in rows:
            x = x_of(r["seq"])
            t = r["tier"]
            out.append(f'<circle cx="{x:.1f}" cy="{y}" r="3" fill="{PAPER}" stroke="{ACCENT}" stroke-width="1.6"/>')
            names.append(vname(x, y - 12 if names_above else y + 16, r["name"],
                               SIZES[t], marker, down=not names_above, unique=r["unique"]))
        return "".join(out), "".join(names)

    svg = []
    svg.append(f'<svg viewBox="0 0 {W} {H}" role="img" aria-label="台中市公車{esc(meta["num"])}路線簡圖：{esc(meta["start"])}—{esc(meta["end"])}，去回程站序與道路名">')
    svg.append(f'<rect width="{W}" height="{H}" fill="{PAPER}"/>')
    t, _, _ = title_svg(meta["num"], meta["start"], meta["end"], meta["operator"],
                        len(dirs[0]["rows"]), len(dirs[1]["rows"]),
                        meta["shared"], meta["u0"], meta["u1"], serp=True)
    svg += t
    ly = 124
    svg.append(legend_svg(dirs, SIZES, ly, unique_marker_size=10))
    svg.append(f'<line x1="48" y1="{ly + 20}" x2="{W - 48}" y2="{ly + 20}" stroke="#E7E5E4" stroke-width="1"/>')

    svg.append('<defs><marker id="arw" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">'
               f'<path d="M 0 0 L 10 5 L 0 10 z" fill="{ACCENT}"/></marker></defs>')
    all_chips, all_names = [], []
    for d in (0, 1):
        s0, s1 = DIR_SLOTS[d]
        n = len(dirs[d]["rows"])
        # row split: seq 1..50 then 51..n (each row ≤ 50 stops)
        cut = min(50, n)
        parts = [dirs[d]["rows"][:cut]]
        if n > cut:
            parts.append(dirs[d]["rows"][cut:])
        # connector between the direction's own two rows (drop at the side
        # where the first row ends reading)
        if len(parts) == 2:
            ya, yb = SLOTS[s0][0], SLOTS[s1][0]
            bx = X0 if SLOTS[s0][1] else X1
            bulge = -26 if bx == X0 else 26
            svg.append(f'<path d="M {bx} {ya} C {bx + bulge} {ya} {bx + bulge} {yb} {bx} {yb}" fill="none" stroke="{ACCENT}" stroke-width="3"/>')
        # row lines with travel-direction arrows (each row's own reading direction)
        for k, part in enumerate(parts):
            slot = (s0, s1)[k]
            y, rtl, _ = SLOTS[slot]
            sx = X1 if rtl else X0
            ex = (X0 + 4) if rtl else ((1020.0 - 4) if slot == 1 else (X1 - 4))
            svg.append(f'<line x1="{sx:.1f}" y1="{y}" x2="{ex:.1f}" y2="{y}" stroke="{ACCENT}" stroke-width="3" marker-end="url(#arw)"/>')
            marker = "▲" if d == 0 else "▽"
            body, names = row_svg(part, dirs[d]["spans"], slot, part[0]["seq"], marker)
            all_chips.append(body)
            all_names.append(names)
    svg.append(f'<text x="{W - 38}" y="{(SLOTS[0][0] + SLOTS[1][0]) / 2}" text-anchor="end" class="dir">↓ 去程</text>')
    svg.append(f'<text x="38" y="{(SLOTS[2][0] + SLOTS[3][0]) / 2}" text-anchor="start" class="dir">返程</text>')
    svg.append("".join(all_chips))
    svg.append("".join(all_names))
    svg.append("</svg>")

    caption = (f'<b>臺中市公車 {esc(meta["num"])}</b>（{esc(meta["operator"])}）：去程 {len(dirs[0]["rows"])} 站（{esc(meta["start"])} → {esc(meta["end"])}）、'
               f'返程 {len(dirs[1]["rows"])} 站（{esc(meta["end"])} → {esc(meta["start"])}）'
               + ('，去回皆兩端共用' if meta["ends_shared"] else '') + '；'
               f'路徑蛇行折行，實線弧為折行續行（非迴轉）。上組兩行為去程（右→左接左→右），下組為返程（左→右接右→左）。'
               f'站名字級依該站服務路線數分三級；紅字＋▲／▽ 為僅單向停靠之站名。'
               f'道路名去除「段／街／巷」後綴合併，僅標示跨三站以上之路名，淡化直線連起其對應站點、路名置中。資料：rm-web（{esc(meta["db"])}）{date.today():%Y-%m-%d}。'
               f'畫布 1123×{H}（百站直排名稱超出 A4 橫單頁高度），列印請選 A4 橫向＋「縮放至符合紙張」。')
    return wrap_html(svg, meta["num"], meta["start"], meta["end"], meta["operator"], (W, H), caption), H


# ----------------------------------------------------------------- main ----

def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--query", help="route number to search (e.g. 5, 270)")
    ap.add_argument("--route-id", type=int, help="skip search, use this route_id")
    ap.add_argument("--num", help="route number for the title (default: matched route_code)")
    ap.add_argument("--start", help="outbound terminus (default: from subroute_name)")
    ap.add_argument("--end", help="outbound terminus (default: from subroute_name)")
    ap.add_argument("--operator", help="operator name (default: first provider)")
    ap.add_argument("--db", help="city db name for the caption (default: rm-web)")
    ap.add_argument("--out", help="output HTML path (default: route<num>-diagram.html in cwd)")
    ap.add_argument("--url", default=None, help="rm-web base URL (default $RM_WEB_URL or http://127.0.0.1:8791)")
    ap.add_argument("--workdir", help="cache dir (default: <out>.d)")
    a = ap.parse_args()
    if not a.query and not a.route_id:
        ap.error("--query or --route-id required")

    base = a.url or os.environ.get("RM_WEB_URL") or "http://127.0.0.1:8791"
    base = base.rstrip("/")

    # resolve route_id + metadata from the routes index
    num = a.num
    subroute = None
    provider = None
    route_id = a.route_id
    if a.query and not route_id:
        hits = api_get(base, f"/api/routes?q={urllib.parse.quote(a.query)}")["routes"]
        if not hits:
            raise SystemExit(f"no route matches {a.query!r}")
        exact = [r for r in hits if r["route_code"] == a.query]
        r = (exact or hits)[0]
        route_id = r["route_id"]
        num = num or r["route_code"]
        subroute = r.get("subroute_name")
        provider = (r.get("providers") or [None])[0]
    elif not num:
        hits = api_get(base, f"/api/routes?q={route_id}")["routes"]
        r = next((x for x in hits if x["route_id"] == route_id), None)
        if r:
            num = num or r["route_code"]
            subroute = r.get("subroute_name")
            provider = (r.get("providers") or [None])[0]
    num = num or str(route_id)

    out = Path(a.out) if a.out else Path(f"route{num}-diagram.html")
    workdir = Path(a.workdir) if a.workdir else out.with_suffix(out.suffix + ".d") / str(route_id)
    workdir.mkdir(parents=True, exist_ok=True)

    print(f"route_id={route_id} num={num} url={base}")
    raw = fetch(base, route_id, workdir)

    dirs = {}
    check = []
    for d in (0, 1):
        stops, feats = raw[d]
        runs = runs_from_features(feats)
        rows = assign_stops_to_runs(stops, runs)
        spans = consolidate(rows)
        dirs[d] = {"rows": rows, "spans": spans}
        check.append(f"== direction {d}: {len(stops)} stops, {len(runs)} way runs, {len(spans)} road spans")
        for r in rows:
            check.append(f"  {r['seq']:>3} {r['name']}  [{r['road']}]  snap={r['snap_m']}m  routes={r['n_routes']}")
        worst = max(rows, key=lambda r: r["snap_m"])
        check.append(f"  worst snap: seq {worst['seq']} {worst['name']} {worst['snap_m']}m")
        if worst["snap_m"] > 150:
            check.append(f"  !! worst snap > 150m — check road assignment for this direction")

    shared_names = {r["name"] for r in dirs[0]["rows"]} & {r["name"] for r in dirs[1]["rows"]}
    for d in (0, 1):
        for r in dirs[d]["rows"]:
            r["tier"] = tier_of(r["n_routes"])
            r["unique"] = r["name"] not in shared_names

    meta = {
        "num": num,
        "start": a.start, "end": a.end,
        "operator": a.operator or provider or "市區客運",
        "shared": len(shared_names),
        "u0": sum(r["unique"] for r in dirs[0]["rows"]),
        "u1": sum(r["unique"] for r in dirs[1]["rows"]),
        "db": a.db or "rm-web",
    }
    # Title termini: explicit args win; else subroute_name when it is a plain
    # two-part span ("A - B" — 3+ parts means a via-terminus, e.g. 270's
    # "豐富公園 - 大南 - 東勢", whose ends are not what the diagram draws);
    # else the direction's own first/last stop names.
    if subroute and " - " in subroute:
        parts = [p.strip() for p in subroute.split(" - ")]
        if len(parts) == 2:
            meta["start"] = meta["start"] or parts[0]
            meta["end"] = meta["end"] or parts[1]
    meta["start"] = meta["start"] or dirs[0]["rows"][0]["name"]
    meta["end"] = meta["end"] or dirs[0]["rows"][-1]["name"]
    ends_shared = (dirs[0]["rows"][0]["name"] in shared_names
                   and dirs[0]["rows"][-1]["name"] in shared_names
                   and dirs[1]["rows"][0]["name"] in shared_names
                   and dirs[1]["rows"][-1]["name"] in shared_names)
    meta["ends_shared"] = ends_shared

    # ---- verification ----
    for d in (0, 1):
        rows = dirs[d]["rows"]
        assert all(rows[i]["seq"] < rows[i + 1]["seq"] for i in range(len(rows) - 1)), f"dir{d} seq not monotonic"
        assert rows[0]["seq"] == 1, f"dir{d} does not start at seq 1"
        v = [f"dir{d}: {len(rows)} rows, first={rows[0]['name']!r} last={rows[-1]['name']!r}"]
        if rows[0]["name"] in shared_names and rows[-1]["name"] in shared_names:
            v.append(f"  (both ends are shared names)")
    check.append(f"shared names: {len(shared_names)}, unique dir0={meta['u0']} dir1={meta['u1']}")
    tcount = {1: 0, 2: 0, 3: 0}
    for d in (0, 1):
        for r in dirs[d]["rows"]:
            tcount[r["tier"]] += 1
    check.append(f"tier distribution (stop rows): t1={tcount[1]} t2={tcount[2]} t3={tcount[3]}")

    serp = max(len(dirs[0]["rows"]), len(dirs[1]["rows"])) > 50
    if serp and max(len(dirs[0]["rows"]), len(dirs[1]["rows"])) > 100:
        raise SystemExit("direction > 100 stops: serpentine supports 2 rows of 50 only")
    html, H = (draw_serp if serp else draw_single)(dirs, meta)
    out.write_text(html)
    workdir.joinpath("check.txt").write_text("\n".join(check), encoding="utf-8")

    print("\n".join(v))
    print(f"layout: {'serpentine 2x2' if serp else 'single racetrack'}  canvas {W}x{H}")
    print(f"shared={meta['shared']} unique={meta['u0']}/{meta['u1']}  tiers t1={tcount[1]} t2={tcount[2]} t3={tcount[3]}")
    print(f"written {out} ({out.stat().st_size} bytes)")
    print(f"check file: {workdir / 'check.txt'}")


if __name__ == "__main__":
    main()
