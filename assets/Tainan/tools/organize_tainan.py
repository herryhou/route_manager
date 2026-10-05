#!/usr/bin/env python3
"""Convert raw Tainan captures into the Taichung-shape capture tree rm-ingest expects.

Tainan has no global station id (stopid is per-occurrence — see
Tainan_ingest_eval.md), so station identity is derived here: same stop name
within 80 m, chained transitively (the rm-identity/grouping.rs rule), first
occurrence wins name/coords. Station ids are minted in first-appearance order
as T%06d.

Filters out 小黃巴士 (type name; their pathList typeid is 71-76, not the 7
that pathTypeList implies). Direction flag maps Tainan goback 0/1 -> Taichung
goBack 1/2. ReadText point lists are encoded to Google polylines (1e-5),
matching rm_core::polyline.

Output: ../capture/{routes,providers,main-side-routes,jointOperationsBuses}.json
        ../capture/raw/route_{id}.json
"""
import json, glob, os, math
from collections import defaultdict

BASE = os.path.dirname(os.path.abspath(__file__)) + "/.."
OUT = f"{BASE}/capture"
EXCLUDE_TYPE = "小黃巴士"

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
            out.append(chr((v & 0x1f) | 0x20)); v >>= 5
        out.append(chr(v)); return ''.join(out)
    def zigzag(v): return ((v << 1) ^ (v >> 63)) & 0xFFFFFFFFFFFFFFFF
    s, plat, plon = [], 0, 0
    for lat, lon in pts:
        la, lo = round(lat*1e5), round(lon*1e5)
        s.append(varint(zigzag(la - plat))); s.append(varint(zigzag(lo - plon)))
        plat, plon = la, lo
    return ''.join(s)

# ---- load + filter headers ----
paths = json.load(open(f"{BASE}/pathList.json"))
kept = [p for p in paths if p["type"] != EXCLUDE_TYPE]
excluded = [p for p in paths if p["type"] == EXCLUDE_TYPE]
by_id = {p["id"]: p for p in kept}

# ---- pass 1: stop occurrences in deterministic order ----
# (route numeric id, direction, sequence); station attrs = first occurrence.
def route_key(rid): return (0, int(rid)) if rid.isdigit() else (1, 0)

occurrences = []            # (rid, goback, seq, stopid, name, lat, lon)
occ_idx = {}                # (rid, goback, seq) -> index into occurrences
for path in sorted(glob.glob(f"{BASE}/raw/route_*.json"),
                   key=lambda p: route_key(os.path.basename(p)[6:-5])):
    rid = os.path.basename(path)[len("route_"):-len(".json")]
    if rid not in by_id:
        continue
    dirs = sorted(json.load(open(path)), key=lambda d: int(d["goback"]))
    for d in dirs:
        for i, s in enumerate(d["stop"], start=1):
            occ_idx[(rid, int(d["goback"]), i)] = len(occurrences)
            occurrences.append((rid, int(d["goback"]), i, int(s["stopid"]),
                                s["stopname"].strip(), float(s["lat"]), float(s["lon"])))

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
station_attrs = {}          # root -> (name, lat, lon) first occurrence
for idx, o in enumerate(occurrences):
    root = find(idx)
    if root not in station_attrs:
        station_attrs[root] = (o[4], o[5], o[6])
        cluster_of[root] = f"T{len(station_attrs):06d}"

# ---- emit capture tree ----
os.makedirs(f"{OUT}/raw", exist_ok=True)

json.dump({"data": {"providers": {"edges": []}}}, open(f"{OUT}/providers.json", "w"))
json.dump([], open(f"{OUT}/main-side-routes.json", "w"))
json.dump({"data": {"jointOperationsBuses": []}}, open(f"{OUT}/jointOperationsBuses.json", "w"))

headers = []
for p in kept:
    headers.append({"node": {
        "id": p["id"], "name": p["name"], "seq": p["sequence"],
        "departure": p["Dept"], "destination": p["Dest"],
        "description": f"{p['Dept']} - {p['Dest']}",
        "isCycled": p["Dept"] == p["Dest"],
        "opType": int(p["typeid"]),
        "providers": {"edges": []},
    }})
json.dump({"data": {"routes": {"edges": headers}}}, open(f"{OUT}/routes.json", "w"),
          ensure_ascii=False)

def load_geom(rid, leg):
    try:
        pts = json.load(open(f"{BASE}/raw/geom_{rid}_{leg}.json"))
        if not (isinstance(pts, list) and pts): return None
        return encode_polyline([(p["Y"], p["X"]) for p in pts])
    except Exception:
        return None

rows = 0
no_geom = []
for p in kept:
    rid = p["id"]
    dirs = sorted(json.load(open(f"{BASE}/raw/route_{rid}.json")),
                  key=lambda d: int(d["goback"]))
    edges = []
    for d in dirs:
        for i, s in enumerate(d["stop"], start=1):
            root = find(occ_idx[(rid, int(d["goback"]), i)])
            name, lat, lon = station_attrs[root]
            edges.append({
                "goBack": 1 if int(d["goback"]) == 0 else 2,
                "orderNo": i, "ivrNo": None, "sid": int(s["stopid"]),
                "node": {"id": cluster_of[root], "name": name, "lat": lat, "lon": lon},
            })
    rows += len(edges)
    go, back = load_geom(rid, "go"), load_geom(rid, "back")
    if not go and not back:
        no_geom.append(rid)
    doc = {"data": {"route": {
        "stations": {"edges": edges},
        "routePoint": {"go": go, "back": back},
        "providers": {"edges": []},
    }}}
    json.dump(doc, open(f"{OUT}/raw/route_{rid}.json", "w"), ensure_ascii=False)

print(json.dumps({
    "routes_kept": len(kept), "routes_excluded": len(excluded),
    "stop_rows": rows, "stations": len(station_attrs),
    "routes_without_geometry": no_geom,
}, ensure_ascii=False, indent=2))
