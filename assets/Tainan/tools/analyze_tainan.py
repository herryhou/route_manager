#!/usr/bin/env python3
"""Analyze raw Tainan captures (assets/Tainan/raw) for ingest feasibility.

Mirrors the Taichung stats (assets/Taichung/data/_stats.json) so the two
datasets can be compared side by side.
"""
import json, glob, os, math
from collections import defaultdict

BASE = os.path.dirname(os.path.abspath(__file__)) + "/.."
RAW = f"{BASE}/raw"

paths = json.load(open(f"{BASE}/pathList.json"))

stations = {}                    # stopid -> {name, lat, lon}
station_alt_names = defaultdict(set)
station_alt_coords = defaultdict(set)
rs_rows = []                     # route_stop rows
dirs_per_route = defaultdict(set)
sid_like = defaultdict(list)     # (stopid, dir) -> [routes]  (would-be pole key)
geom_ok, geom_pts, geom_bad = 0, {}, []
first_stop_gap = []              # gap between geometry start and first stop (m)

def haversine(a, b):
    r = 6371000.0
    p1, p2 = math.radians(a[0]), math.radians(b[0])
    dp, dl = math.radians(b[0]-a[0]), math.radians(b[1]-a[1])
    h = math.sin(dp/2)**2 + math.cos(p1)*math.cos(p2)*math.sin(dl/2)**2
    return 2*r*math.asin(math.sqrt(h))

bad_coord_rows = 0
for path in sorted(glob.glob(f"{RAW}/route_*.json")):
    rid = os.path.basename(path)[len("route_"):-len(".json")]
    for d in json.load(open(path)):
        goback = int(d["goback"])
        direction = 0 if goback == 0 else 1
        dirs_per_route[rid].add(direction)
        for i, s in enumerate(d["stop"], start=1):
            sid = s["stopid"]
            try:
                lat, lon = float(s["lat"]), float(s["lon"])
                ok = 22.8 < lat < 23.5 and 119.9 < lon < 120.75  # Greater Tainan bbox
            except (ValueError, TypeError):
                lat, lon, ok = 0.0, 0.0, False
            if not ok:
                bad_coord_rows += 1
            stations.setdefault(sid, {"name": s["stopname"], "lat": lat, "lon": lon, "routes": [], "n": 0})
            station_alt_names[sid].add(s["stopname"])
            station_alt_coords[sid].add((lat, lon))
            if rid not in stations[sid]["routes"]:
                stations[sid]["routes"].append(rid)
            stations[sid]["n"] += 1
            sid_like[(sid, direction)].append(rid)
            rs_rows.append({"route_id": rid, "direction": direction,
                            "stop_sequence": i, "stop_id": sid,
                            "stop_name": s["stopname"], "lat": lat, "lon": lon})

for path in sorted(glob.glob(f"{RAW}/geom_*.json")):
    key = os.path.basename(path)[len("geom_"):-len(".json")]  # e.g. 2101_go
    try:
        pts = json.load(open(path))
        assert isinstance(pts, list) and pts
        geom_ok += 1
        geom_pts[key] = len(pts)
        rid, leg = key.rsplit("_", 1)
        g = (pts[0]["Y"], pts[0]["X"])
        rows = [r for r in rs_rows if r["route_id"] == rid and r["direction"] == (0 if leg == "go" else 1)]
        if rows:
            first = min(rows, key=lambda r: r["stop_sequence"])
            first_stop_gap.append((key, haversine(g, (first["lat"], first["lon"]))))
    except Exception:
        geom_bad.append(key)

single_dir = [r for r, ds in dirs_per_route.items() if len(ds) == 1]
multi_name = {k: sorted(v) for k, v in station_alt_names.items() if len(v) > 1}
multi_coord = {k: [list(c) for c in v] for k, v in station_alt_coords.items() if len(v) > 1}
gaps_far = [(k, round(m)) for k, m in first_stop_gap if m > 150]
pole_reuse = {k: v for k, v in sid_like.items() if len(v) > 1}

stats = {
    "capture_date": "2026-10-05",
    "routes": len(paths),
    "route_types": {t: sum(1 for p in paths if p["type"] == t) for t in sorted({p["type"] for p in paths})},
    "single_direction_routes": len(single_dir),
    "direction_stop_rows": len(rs_rows),
    "stations_unique_stopid": len(stations),
    "bad_or_out_of_bbox_coords": bad_coord_rows,
    "stopid_with_alt_names": len(multi_name),
    "stopid_with_alt_coords": len(multi_coord),
    "stopid_on_multiple_routes": sum(1 for s in stations.values() if len(s["routes"]) > 1),
    "wouldbe_pole_key_reuse": len(pole_reuse),   # (stopid, dir) serving >1 route
    "geom_files_ok": geom_ok,
    "geom_files_bad": geom_bad,
    "geom_median_pts": sorted(geom_pts.values())[len(geom_pts)//2] if geom_pts else 0,
    "geom_first_pt_vs_first_stop_over_150m": len(gaps_far),
    "circular_DeptEqDest": sum(1 for p in paths if p["Dept"] == p["Dest"]),
}
print(json.dumps(stats, ensure_ascii=False, indent=2))
print("\n-- stopid with >1 name (first 5):", dict(list(multi_name.items())[:5]))
print("-- stopid with >1 coord (first 5):", dict(list(multi_coord.items())[:5]))
print("-- geom far-start (first 10):", gaps_far[:10])
print("-- top shared stations:",
      sorted(((len(s["routes"]), k, s["name"]) for k, s in stations.items()), reverse=True)[:5])
