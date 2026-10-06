#!/usr/bin/env python3
"""Convert Taipei ebus captures into the capture tree rm-ingest expects.

Inputs (assets/Taipei/):
  routes.json   — 406 ct=tpc routes scraped from /ebus?ct=tpc (id/name/cats)
  shapes.json   — GetBusShape.gz filtered to tpc (UniRouteId + wkt + GoBack)
  raw/route_{id}_stops.json — /Route/StopsOfRoute scrapes:
      {go_head, back_head, go/back: [{seq, name, stopId, lat, lng}]}

Taipei has pole-level global ids (UniStopId) but no station (pole-group) id,
so station identity is derived the organize_tainan way: same trimmed stop
name within 80 m, transitive union-find; ids minted in first-appearance
order as P%06d. Each edge's node keeps the pole's own name/coords.
sid = int(UniStopId) (per-occurrence pole id, shared across routes is fine).
GoDirectionRoute/BackDirectionRoute -> goBack 1/2. WKT LINESTRING (lon lat)
from GetBusShape -> Google polyline 1e-5. data.taipei shapes may lag the
live site; stops come from the live site.

No provider / main-side / joint data -> empty per spec.

Output: ../capture/{routes,providers,main-side-routes,jointOperationsBuses}.json
        ../capture/raw/route_{id}.json
"""
import json, glob, os, math, re
from collections import defaultdict

BASE = os.path.dirname(os.path.abspath(__file__)) + "/.."
OUT = f"{BASE}/capture"

def haversine_m(a, b):
    r = 6371000.0
    p1, p2 = math.radians(a[0]), math.radians(b[0])
    dp, dl = math.radians(b[0]-a[0]), math.radians(b[1]-a[1])
    h = math.sin(dp/2)**2 + math.cos(p1)*math.cos(p2)*math.sin(dl/2)**2
    return 2*r*math.asin(math.sqrt(h))

def encode_polyline(pts):
    """Google encoded polyline, precision 1e-5 (rm_core::polyline mirror)."""
    def varint(v):
        out = []
        while v >= 0x20:
            out.append(chr(((v & 0x1f) | 0x20) + 63)); v >>= 5
        out.append(chr(v + 63)); return ''.join(out)
    def zigzag(v): return ((v << 1) ^ (v >> 63)) & 0xFFFFFFFFFFFFFFFF
    s, plat, plon = [], 0, 0
    for lat, lon in pts:
        la, lo = round(lat*1e5), round(lon*1e5)
        s.append(varint(zigzag(la - plat))); s.append(varint(zigzag(lo - plon)))
        plat, plon = la, lo
    return ''.join(s)

WKT_COORD = re.compile(r'(-?\d+(?:\.\d+)?) (-?\d+(?:\.\d+)?)')

def wkt_to_polyline(wkt):
    pts = [(float(la), float(lo)) for lo, la in WKT_COORD.findall(wkt)]
    return encode_polyline(pts) if pts else None

def route_key(rid): return (0, int(rid)) if rid.isdigit() else (1, 0)

# ---- load ----
routes = json.load(open(f"{BASE}/routes.json"))["routes"]
shapes = {}
for d in json.load(open(f"{BASE}/shapes.json"))["shapes"]:
    shapes.setdefault((d["UniRouteId"], int(d["GoBack"])), d["wkt"])

stops_by_route = {}
for path in sorted(glob.glob(f"{BASE}/raw/route_*_stops.json"),
                   key=lambda p: route_key(os.path.basename(p)[6:-11])):
    rid = os.path.basename(path)[len("route_"):-len("_stops.json")]
    stops_by_route[rid] = json.load(open(path))

# ---- pass 1: stop occurrences in deterministic order (id, direction, seq) ----
occurrences = []            # (rid, dir_idx, seq, stopId, name, lat, lon)
occ_idx = {}                # (rid, dir_idx, seq) -> index
for rid in sorted(stops_by_route, key=route_key):
    d = stops_by_route[rid]
    for dir_idx, leg in ((0, d["go"]), (1, d["back"])):
        for i, s in enumerate(leg, start=1):
            occ_idx[(rid, dir_idx, i)] = len(occurrences)
            occurrences.append((rid, dir_idx, i, int(s["stopId"]),
                                s["name"].strip(), s["lat"], s["lng"]))

# ---- cluster stations: same name, <80 m, transitive (union-find) ----
by_name = defaultdict(list)
for idx, o in enumerate(occurrences):
    by_name[o[4]].append(idx)
parent = list(range(len(occurrences)))
def find(x):
    while parent[x] != x:
        parent[x] = parent[parent[x]]; x = parent[x]
    return x
def union(a, b):
    ra, rb = find(a), find(b)
    if ra != rb: parent[max(ra, rb)] = min(ra, rb)
for name, idxs in by_name.items():
    for i in range(len(idxs)):
        for j in range(i+1, len(idxs)):
            a, b = occurrences[idxs[i]], occurrences[idxs[j]]
            if haversine_m((a[5], a[6]), (b[5], b[6])) < 80:
                union(idxs[i], idxs[j])
cluster_of = {}
for idx in range(len(occurrences)):
    root = find(idx)
    if root not in cluster_of:
        cluster_of[root] = f"P{len(cluster_of)+1:06d}"

# ---- emit capture tree ----
os.makedirs(f"{OUT}/raw", exist_ok=True)
json.dump({"data": {"providers": {"edges": []}}}, open(f"{OUT}/providers.json", "w"))
json.dump([], open(f"{OUT}/main-side-routes.json", "w"))
json.dump({"data": {"jointOperationsBuses": []}}, open(f"{OUT}/jointOperationsBuses.json", "w"))

headers = []
for i, r in enumerate(sorted(routes, key=lambda r: route_key(r["id"])), start=1):
    rid = r["id"]
    d = stops_by_route.get(rid)
    if d and d["go"]:
        dep, dest = d["go"][0]["name"], d["go"][-1]["name"]
    elif d and d["back"]:
        dep, dest = d["back"][-1]["name"], d["back"][0]["name"]
    else:
        dep = dest = r["name"]
    headers.append({"node": {
        "id": rid, "name": r["name"], "seq": i,
        "departure": dep, "destination": dest,
        "description": f"{dep} - {dest}",
        "isCycled": dep == dest,
        "opType": 0,   # tpc categories are tags, not a 1:1 code (kept in routes.json)
        "providers": {"edges": []},
    }})
json.dump({"data": {"routes": {"edges": headers}}}, open(f"{OUT}/routes.json", "w"),
          ensure_ascii=False)

rows = 0
no_back, no_geom = [], []
for r in sorted(routes, key=lambda r: route_key(r["id"])):
    rid = r["id"]
    d = stops_by_route.get(rid)
    if not d:
        continue
    edges = []
    for dir_idx, leg in ((0, d["go"]), (1, d["back"])):
        for i, s in enumerate(leg, start=1):
            occ = occ_idx[(rid, dir_idx, i)]
            _, _, _, sid, name, lat, lon = occurrences[occ]
            root = find(occ)
            edges.append({
                "goBack": 1 if dir_idx == 0 else 2,
                "orderNo": i, "ivrNo": None, "sid": sid,
                "node": {"id": cluster_of[root], "name": name, "lat": lat, "lon": lon},
            })
    rows += len(edges)
    if not d["back"]:
        no_back.append(rid)
    go = wkt_to_polyline(shapes.get((rid, 0), "")) if d["go"] else None
    back = wkt_to_polyline(shapes.get((rid, 1), "")) if d["back"] else None
    if not go and not back:
        no_geom.append(rid)
    doc = {"data": {"route": {
        "stations": {"edges": edges},
        "routePoint": {"go": go, "back": back},
        "providers": {"edges": []},
    }}}
    json.dump(doc, open(f"{OUT}/raw/route_{rid}.json", "w"), ensure_ascii=False)

print(json.dumps({
    "routes_kept": len(headers),
    "routes_without_raw": len(routes) - len(stops_by_route),
    "stop_rows": rows, "stations": len(set(cluster_of.values())),
    "unique_pole_ids": len({o[3] for o in occurrences}),
    "routes_without_back": no_back,
    "routes_without_geometry": no_geom,
}, ensure_ascii=False, indent=2))
