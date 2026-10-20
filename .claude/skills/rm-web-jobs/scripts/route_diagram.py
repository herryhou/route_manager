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
  3. draw    — layout by density: a direction with <30 stops draws as one
               L->R line; otherwise a ⊃ hairpin (一律拆開) — first half of
               the stops runs left->right on the top line, one smooth turn
               at the right, second half returns right->left — first stop
               top-left, last stop bottom-left. 去程's sits above 返程's.
               Name columns scale with row pitch (25 stops/side -> 1.8x),
               so the canvas height follows the tallest column.

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
                           [--operator 全航客運] [--url http://127.0.0.1:8787]

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
PAPER = "#FFFFFF"
GREEN = "#047857"                   # route backbone: line, dots, arrowhead, direction labels
HEADER = "#134E4A"                  # header band (BAND below is hairpin gap — don't collide)
HALF = "#10B981"                    # route-number half of the header band
PALE_YELLOW = "#FDE68A"             # eyebrow on the band
PALE_MINT = "#D1FAE5"               # operator/station-count line on the band

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


def vname(x, y0, name, size, down):
    """Vertical name column (CSS vertical-rl + upright): fullwidth （） get
    true vertical forms, every char advances exactly 1em, ink bottom lands on
    the em bottom. down=False hangs UP from flush edge y0 (start = y0-n*size);
    down=True drops DOWN with first-char ink tops at y0 (start 0.14em higher —
    CJK ink fills the lower 86% of the cell). Paren-ending/starting columns
    shift by the measured arc gaps. Explicit per-char tspans pin the same
    positions in engines without vertical SVG text."""
    name = name.replace("(", "（").replace(")", "）")  # fullwidth: vert forms + 1em advance
    chars = name
    n = len(chars)
    cls = "vn v"
    start = (y0 - n * size) if not down else (y0 - 0.14 * size)
    if not down and name.endswith("）"):
        start += PAREN_END_GAP * size     # push down: arc bottom lands on edge
    if down and name.startswith("（"):
        start -= PAREN_START_GAP * size   # pull up: arc top lands on edge
    body = "".join(f'<tspan x="{x:.1f}" y="{start + i * size:.1f}">{c}</tspan>' for i, c in enumerate(chars))
    return (f'<text x="{x:.1f}" y="{start:.1f}" class="{cls}" '
            f'font-size="{size:.1f}">{body}</text>')


# Road names: strip-merge leaves only ≥3-stop spans — sparse enough that
# one lane holds them all; each is drawn as a faded guide line between the
# stop dots and the name, name centred on its span.
ROAD_FONT = 9.5
GUIDE = "#A8A29E"
ROAD_BELOW = (8.0, 20.0)    # (guide_dy, text_baseline_dy), gap below the line
ROAD_ABOVE = (-8.0, -14.0)  # gap above the line


def road_marks(spans, x_of, below, y):
    dy, ty = ROAD_BELOW if below else ROAD_ABOVE
    out = []
    for s in spans:
        if not s["road"] or s["count"] < 3:
            continue
        xa, xb = x_of(s["from_seq"]), x_of(s["to_seq"])
        if xa >= xb:
            continue
        # the guide must fully contain the end dots: extend past their
        # outer edge (terminus r4 + half stroke = 5.5) by a small margin
        # (2026-10-20 ruling, reversing the earlier inset). Text stays
        # centred on the true span. Adjacent spans sit ≥1 dot apart
        # (≥3-stop spans only), so ±8 never bridges two guides.
        la, lb = xa - 4, xb + 4
        half = len(s["road"]) * ROAD_FONT / 2 + 4
        tx = min(max((xa + xb) / 2, 37.5 + half), W - 37.5 - half)
        out.append(f'<line x1="{la:.1f}" y1="{y + dy:.1f}" x2="{lb:.1f}" y2="{y + dy:.1f}" stroke="{GUIDE}" stroke-width="1"/>'
                   f'<text x="{tx:.1f}" y="{y + ty:.1f}" text-anchor="middle" class="road" font-size="{ROAD_FONT}">{esc(s["road"])}</text>')
    return "".join(out)


def esc(s):
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def css_block():
    return """
:root {
  --bg: #E7F3EC; --fg: #1C2B22; --muted: #5C6B62; --edge: #CFE0D6;
  --mint: #E3F2E9; --mint-edge: #C4DFD0;
  color-scheme: light dark;
}
@media (prefers-color-scheme: dark) { :root:not([data-theme="light"]) {
  --bg: #16221C; --fg: #E7F0EA; --muted: #9AAB9F; --edge: #2C3B32;
  --mint: #1B2C22; --mint-edge: #2C4237;
} }
:root[data-theme="dark"] {
  --bg: #16221C; --fg: #E7F0EA; --muted: #9AAB9F; --edge: #2C3B32;
  --mint: #1B2C22; --mint-edge: #2C4237;
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
figcaption { margin-top: 12px; background: var(--mint); border: 1px solid var(--mint-edge);
  border-radius: 8px; padding: 10px 14px; color: var(--muted); font-size: 13px; line-height: 1.7; }
figcaption b { color: var(--fg); font-weight: 500; }
.num { font-size: %%NUM%%px; font-weight: 900; fill: %s; letter-spacing: -2px; }
.eyebrow { font-size: 15px; font-weight: 500; fill: %s; letter-spacing: 6px; }
.title { font-size: %s; font-weight: 700; fill: %s; letter-spacing: 1px; }
.sub { font-size: 11.5px; fill: %s; }
.dir { font-size: 14px; font-weight: 700; fill: %s; letter-spacing: 2px; }
.vn { font-weight: 500; fill: %s; }
.v { writing-mode: vertical-rl; text-orientation: upright; }
.road { letter-spacing: 0.5px; fill: %s; }
""" % (PAPER, "#FFFFFF", PALE_YELLOW, "%TITLE%", "#FFFFFF", PALE_MINT, GREEN, INK, ROAD)



def title_svg(num, title_name, operator, n0, n1, city):
    num_x = 150 if len(num) == 1 else 210
    split_x = num_x - 16          # route-number half, band-colored rest
    svg = []
    svg.append(f'<rect x="0" y="0" width="{W}" height="112" fill="{HEADER}"/>')
    svg.append(f'<rect x="0" y="0" width="{split_x}" height="112" fill="{HALF}"/>')
    svg.append(f'<text x="{split_x / 2:.1f}" y="92" text-anchor="middle" class="num">{esc(num)}</text>')
    svg.append(f'<text x="{num_x}" y="46" class="eyebrow">{esc(city)}公車</text>')
    svg.append(f'<text x="{num_x}" y="88" class="title">{esc(title_name)}</text>')
    svg.append(f'<text x="{W - 48}" y="46" text-anchor="end" class="sub">{esc(operator)}｜去程 {n0} 站 · 返程 {n1} 站</text>')
    return svg


def wrap_html(svg, num, start, end, operator, W_H, caption):
    w, h = W_H
    css = css_block().replace("%NUM%", "84" if len(num) == 1 else "64").replace("%TITLE%", "28px" if h > 900 else "30px")
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


# -------------------------------------------------- layout by density ----

BAND = 70.0                         # gap between a hairpin's two lines
BASE_SIZE = 10.0                    # one size for every stop name (2026-10-20
                                    # ruling: no tiers, no bold)
REF_PITCH = 22.0                    # row pitch (px per stop) at scale 1.0
MAX_SCALE = 2.2                     # sparse-row cap (2026-10-19: 25/side -> 1.8x, 23/side -> ~2.1x)


def row_scale(k):
    """Type scale for a row of k stops: pitch/REF_PITCH clamped to
    [1.0, MAX_SCALE] — sparser rows get proportionally bigger type
    (25 stops/side = 40px pitch -> 1.8x, 23 stops -> ~2.1x)."""
    if k <= 1:
        return MAX_SCALE
    return max(1.0, min(MAX_SCALE, ((X1 - X0) / (k - 1)) / REF_PITCH))


def col_max(rows, scale):
    """Tallest name column in a row at this scale (chars x size)."""
    return max(len(r["name"]) * BASE_SIZE * scale for r in rows)


def draw_route(dirs, meta):
    """Layout by direction size: <30 stops draws as one L->R line with the
    names hanging above it; otherwise a ⊃ hairpin — first half of the stops
    runs left->right on the top line, one smooth turn at the right, the
    second half returns right->left on the bottom line — first stop
    top-left, last stop bottom-left (首尾站靠左). 去程 sits above 返程. Rows
    always span the full canvas width; name columns scale with row pitch,
    so the canvas height follows the tallest column."""
    cursor = 122.0  # below the header rule (y=112)
    geom = {}
    for d in (0, 1):
        rows = dirs[d]["rows"]
        cut = (len(rows) + 1) // 2
        parts = [rows[:cut], rows[cut:]] if len(rows) >= 30 else [rows]
        scales = [row_scale(len(p)) for p in parts]
        up = 12.0 + (col_max(parts[0], scales[0]) if parts[0] else 0.0)
        if len(parts) == 2:
            ya = cursor + 24.0 + up
            yb = ya + BAND
            cursor = yb + 16.0 + (col_max(parts[1], scales[1]) if parts[1] else 0.0)
            geom[d] = {"parts": parts, "scales": scales, "ya": ya, "yb": yb, "hairpin": True}
        else:
            y = cursor + 24.0 + up
            cursor = y + 30.0  # line, then road marks below it
            geom[d] = {"parts": parts, "scales": scales, "ya": y, "yb": None, "hairpin": False}
    H = int(cursor + 26.0)

    def row_svg(rows, spans, y, first_seq, rtl, names_above, scale, end_seqs=()):
        k = len(rows)
        pitch = (X1 - X0) / (k - 1) if k > 1 else 0.0

        def x_of(seq):
            return (X1 - (seq - first_seq) * pitch) if rtl else (X0 + (seq - first_seq) * pitch)

        lo, hi = rows[0]["seq"], rows[-1]["seq"]
        shown = []
        for s in spans:
            a2, b2 = max(s["from_seq"], lo), min(s["to_seq"], hi)
            if a2 <= b2:
                shown.append({"road": s["road"], "from_seq": a2, "to_seq": b2, "count": s["count"]})
        # road marks live in the gap band, opposite the stop names
        out = [road_marks(shown, x_of, below=names_above, y=y)]
        names = []
        for r in rows:
            x = x_of(r["seq"])
            # termini (direction first/last) get a heavier dot than the
            # intermediate stops (2026-10-20 ruling)
            end = r["seq"] in end_seqs
            out.append(f'<circle cx="{x:.1f}" cy="{y}" r="{4 if end else 3}" fill="{PAPER}" stroke="{GREEN}" '
                       f'stroke-width="{3 if end else 2.5}"/>')
            names.append(vname(x, y - 12 if names_above else y + 16, r["name"],
                               BASE_SIZE * scale, down=not names_above))
        return "".join(out), "".join(names)

    svg = []
    svg.append(f'<svg viewBox="0 0 {W} {H}" role="img" aria-label="{esc(meta["city"])}公車{esc(meta["num"])}路線簡圖：{esc(meta["title_name"])}，去回程站序與道路名">')
    svg.append(f'<rect width="{W}" height="{H}" fill="{PAPER}"/>')
    svg += title_svg(meta["num"], meta["title_name"], meta["operator"],
                     len(dirs[0]["rows"]), len(dirs[1]["rows"]), meta["city"])

    svg.append('<defs><marker id="arw" viewBox="0 0 10 10" refX="8" refY="5" markerUnits="userSpaceOnUse" markerWidth="10" markerHeight="10" orient="auto-start-reverse">'
               f'<path d="M 0 0 L 10 5 L 0 10 z" fill="{GREEN}"/></marker></defs>')

    bodies, names_all, loops, modes = [], [], [], []
    for d in (0, 1):
        g = geom[d]
        ya, yb = g["ya"], g["yb"]
        if g["hairpin"]:
            modes.append("⊃ hairpin")
            loops.append(bool(g["parts"][1]) and g["parts"][0][0]["name"] == g["parts"][1][-1]["name"])
            # top line L->R (continues into the turn), squared right turn
            # with rounded corners (⊐, 2026-10-20 ruling),
            # bottom line R->L ending in the direction arrow at the
            # terminus — one arrowhead per direction
            svg.append(f'<line x1="{X0}" y1="{ya:.1f}" x2="{X1}" y2="{ya:.1f}" stroke="{GREEN}" stroke-width="3"/>')
            # the bend sits half a stop-pitch right of the last dot
            # (2026-10-20 ruling) so the turn doesn't crowd the corner dots
            _k0 = len(g["parts"][0]) if g["parts"] else 2
            _half = (X1 - X0) / (_k0 - 1) / 2 if _k0 > 1 else 13.0
            _off, _r = (26 + _half) / 2, 14.0
            svg.append(f'<path d="M {X1} {ya:.1f} H {X1 + _off - _r:.1f} A {_r} {_r} 0 0 1 {X1 + _off:.1f} {ya + _r:.1f} V {yb - _r:.1f} A {_r} {_r} 0 0 1 {X1 + _off - _r:.1f} {yb:.1f} H {X1}" fill="none" stroke="{GREEN}" stroke-width="3"/>')
            svg.append(f'<line x1="{X1}" y1="{yb:.1f}" x2="{X0 + 4}" y2="{yb:.1f}" stroke="{GREEN}" stroke-width="3" marker-end="url(#arw)"/>')
        else:
            modes.append("single line")
            loops.append(False)
            # one continuous L->R line ending in the direction arrow
            svg.append(f'<line x1="{X0}" y1="{ya:.1f}" x2="{X1}" y2="{ya:.1f}" stroke="{GREEN}" stroke-width="3" marker-end="url(#arw)"/>')
        for i, part in enumerate(g["parts"]):
            if not part:
                continue
            y_row = yb if g["hairpin"] and i == 1 else ya
            body, names = row_svg(part, dirs[d]["spans"], y_row, part[0]["seq"],
                                  rtl=(g["hairpin"] and i == 1),
                                  names_above=(not g["hairpin"]) or i == 0,
                                  scale=g["scales"][i],
                                  end_seqs=(dirs[d]["rows"][0]["seq"], dirs[d]["rows"][-1]["seq"]))
            bodies.append(body)
            names_all.append(names)
        if g["hairpin"] and loops[-1]:
            # dashed link when the direction's own first and last stops are
            # the same stop (loop service)
            svg.append(f'<path d="M {X0} {yb:.1f} C {X0 - 24} {yb:.1f} {X0 - 24} {ya:.1f} {X0} {ya:.1f}" fill="none" stroke="{MUTED}" stroke-width="1.6" stroke-dasharray="5 4"/>')
            svg.append(f'<rect x="{X0 - 32}" y="{(ya + yb) / 2 - 8:.1f}" width="36" height="16" fill="{PAPER}"/>')
            svg.append(f'<text x="{X0 - 14}" y="{(ya + yb) / 2 + 3.4:.1f}" text-anchor="middle" class="road">迴轉</text>')
    for d in (0, 1):
        g = geom[d]
        if g["hairpin"]:
            yl, arrow = (g["ya"] + g["yb"]) / 2 + 5, "↓"
        else:
            # below the road-name row (ty = y+20) — at line height it
            # overlapped the road guide line (2026-10-20 ruling: +30)
            yl, arrow = g["ya"] + 30, "→"
        svg.append(f'<text x="38" y="{yl:.1f}" class="dir">{arrow} {"去程" if d == 0 else "返程"}</text>')
    svg.append("".join(bodies))
    svg.append("".join(names_all))
    svg.append("</svg>")

    loop_note = '；向內首尾同站者以左端虛線迴轉表示' if any(loops) else ''
    caption = (f'<b>{esc(meta["city"])}公車 {esc(meta["num"])}</b>（{esc(meta["operator"])}）：去程 {len(dirs[0]["rows"])} 站（{esc(meta["start"])} → {esc(meta["end"])}）、'
               f'返程 {len(dirs[1]["rows"])} 站（{esc(meta["end"])} → {esc(meta["start"])}）{loop_note}。'
               f'資料：rm-web（{esc(meta["db"])}）{date.today():%Y-%m-%d}。')
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
    ap.add_argument("--city", default="臺中市", help="city for the eyebrow/caption (e.g. 臺中市, 臺北市)")
    ap.add_argument("--db", help="city db name for the caption (default: rm-web)")
    ap.add_argument("--out", help="output HTML path (default: route<num>-diagram.html in cwd)")
    ap.add_argument("--url", default=None, help="rm-web base URL (default $RM_WEB_URL or http://127.0.0.1:8787)")
    ap.add_argument("--workdir", help="cache dir (default: <out>.d)")
    a = ap.parse_args()
    if not a.query and not a.route_id:
        ap.error("--query or --route-id required")

    base = a.url or os.environ.get("RM_WEB_URL") or "http://127.0.0.1:8787"
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
        "city": a.city,
        "start": a.start, "end": a.end,
        "operator": a.operator or provider or "市區客運",
        "shared": len(shared_names),
        "u0": sum(r["unique"] for r in dirs[0]["rows"]),
        "u1": sum(r["unique"] for r in dirs[1]["rows"]),
        "db": a.db or "rm-web",
    }
    meta["start"] = a.start or dirs[0]["rows"][0]["name"]
    meta["end"] = a.end or dirs[0]["rows"][-1]["name"]
    # Title (the line beside the route number) carries the official
    # subroute_name verbatim — its span may include a via terminus (270:
    # 豐富公園 — 大南 — 東勢). Captions keep the drawn first/last stops.
    if a.start and a.end:
        meta["title_name"] = f"{a.start} — {a.end}"
    elif subroute:
        meta["title_name"] = subroute.replace(" - ", " — ")
    else:
        meta["title_name"] = f'{meta["start"]} — {meta["end"]}'
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

    if max(len(dirs[0]["rows"]), len(dirs[1]["rows"])) > 100:
        raise SystemExit("direction > 100 stops: each direction draws as two half-rows of <=50")
    html, H = draw_route(dirs, meta)
    out.write_text(html)
    workdir.joinpath("check.txt").write_text("\n".join(check), encoding="utf-8")

    print("\n".join(v))
    shape = "⊃ hairpin" if len(dirs[0]["rows"]) >= 30 else "single line"
    print(f"layout: per-direction density ({shape} shown)  canvas {W}x{H}")
    print(f"shared={meta['shared']} unique={meta['u0']}/{meta['u1']}  tiers t1={tcount[1]} t2={tcount[2]} t3={tcount[3]}")
    print(f"written {out} ({out.stat().st_size} bytes)")
    print(f"check file: {workdir / 'check.txt'}")


if __name__ == "__main__":
    main()
