#!/usr/bin/env python3
"""Build normalized dataset from raw Taichung ebus captures.

Mapping to docs/Architecture_design_spec.md:
  API node.id  -> Station identity (clustered stop point)
  API sid      -> Stop identity (pole: route + direction occurrence)
  API orderNo  -> stop_sequence
  API goBack   -> direction (1 -> 0 去程, 2 -> 1 返程)
  routes.id    -> route identity
"""
import json
import glob
import os
from collections import defaultdict

BASE = os.path.dirname(os.path.abspath(__file__))
A = "/Users/herry/project/rust/route_manger/assets/Taichung"
DATA = os.path.join(A, "data")
os.makedirs(DATA, exist_ok=True)

# --- load route headers ---
with open(f"{A}/routes.json") as f:
    routes_raw = json.load(f)["data"]["routes"]["edges"]

routes = {}
for e in routes_raw:
    n = e["node"]
    routes[n["id"]] = {
        "route_id": n["id"],
        "route_code": n["name"],          # rider-facing number
        "departure": n["departure"],
        "destination": n["destination"],
        "description": n["description"],
        "seq": n["seq"],
        "is_cycled": n["isCycled"],
        "op_type": n["opType"],
        "provider_ids": [p["node"]["id"] for p in n["providers"]["edges"]],
    }

# --- providers ---
with open(f"{A}/providers.json") as f:
    providers = {}
    for e in json.load(f)["data"]["providers"]["edges"]:
        n = e["node"]
        providers[n["id"]] = {"provider_id": n["id"], "name": n["name"],
                              "telephone": n["telephone"], "op_type": n["opType"]}

# --- main-side-routes ---
subroute_links = []
with open(f"{A}/main-side-routes.json") as f:
    for doc in json.load(f):
        subroute_links.append({"main_route_id": str(doc["xno"]),
                               "sub_route_ids": [str(c) for c in doc["children"]]})

# --- raw route details ---
stations = {}            # node.id -> station record
station_names = defaultdict(set)
rs_rows = []            # route_stop rows
polylines = {}
sid_seen = defaultdict(list)

for path in sorted(glob.glob(f"{A}/raw/route_*.json")):
    xno = os.path.basename(path)[len("route_"):-len(".json")]
    with open(path) as f:
        r = json.load(f)["data"]["route"]
    if r is None:
        continue
    if xno in routes:
        routes[xno]["providers_detail"] = [
            {"provider_id": p["node"]["id"], "name": p["node"]["name"]}
            for p in r["providers"]["edges"]]
    rp = r.get("routePoint") or {}
    polylines[xno] = {"go": rp.get("go", ""), "back": rp.get("back", "")}
    for e in r["stations"]["edges"]:
        node = e["node"]
        direction = 0 if e["goBack"] == 1 else 1
        st = stations.setdefault(node["id"], {
            "station_id": node["id"],
            "name": node["name"],
            "lat": node["lat"],
            "lon": node["lon"],
            "routes": [],
            "stop_count": 0,
        })
        station_names[node["id"]].add(node["name"])
        if xno not in st["routes"]:
            st["routes"].append(xno)
        st["stop_count"] += 1
        sid_seen[e["sid"]].append(f"{xno}:{direction}")
        rs_rows.append({
            "route_id": xno,
            "direction": direction,
            "stop_sequence": e["orderNo"],
            "stop_id": e["sid"],           # per route+direction pole id
            "ivr_no": e["ivrNo"],
            "station_id": node["id"],
        })

# attach alt names
for sid, names in station_names.items():
    alt = sorted(names - {stations[sid]["name"]})
    if alt:
        stations[sid]["alt_names"] = alt

# --- write outputs ---
def dump(name, obj):
    with open(f"{DATA}/{name}", "w") as f:
        json.dump(obj, f, ensure_ascii=False, indent=1 if isinstance(obj, dict) else None)

dump("stations.json", sorted(stations.values(), key=lambda s: int(s["station_id"])))
dump("routes.json", [routes[k] for k in sorted(routes, key=lambda x: int(x))])
dump("polylines.json", {k: polylines[k] for k in sorted(polylines, key=lambda x: int(x))})
dump("subroute_links.json", subroute_links)
dump("providers.json", [providers[k] for k in sorted(providers, key=lambda x: int(x))])
with open(f"{DATA}/route_stop.jsonl", "w") as f:
    for row in sorted(rs_rows, key=lambda r: (int(r["route_id"]), r["direction"], r["stop_sequence"])):
        f.write(json.dumps(row, ensure_ascii=False) + "\n")

# --- stats + sanity ---
sid_dupes = {k: v for k, v in sid_seen.items() if len(v) > 1}
no_poly = [k for k, v in polylines.items() if not v["go"] and not v["back"]]
stats = {
    "routes": len(routes),
    "stations_unique": len(stations),
    "route_stop_rows": len(rs_rows),
    "stops_unique_sid": len(sid_seen),
    "sid_reused_across_route_dir": len(sid_dupes),
    "stations_with_alt_names": sum(1 for s in stations.values() if "alt_names" in s),
    "stations_on_most_routes": sorted(((len(s["routes"]), s["station_id"], s["name"]) for s in stations.values()), reverse=True)[:5],
    "routes_without_polyline": len(no_poly),
    "providers": len(providers),
    "subroute_link_groups": len(subroute_links),
}
print(json.dumps(stats, ensure_ascii=False, indent=2))
with open(f"{DATA}/_stats.json", "w") as f:
    json.dump(stats, f, ensure_ascii=False, indent=2)
