#!/bin/bash
# Fetch stop lists (CrossRoutesByPathName) + path geometry (ReadText) for every
# route id in pathList.json. 4 parallel, 3 retries each. No auth needed.
set -u
BASE=/Users/herry/project/rust/route_manger/assets/Tainan
OUT=$BASE/raw
API='https://2384.tainan.gov.tw/NewTNBusAPI_V2/API'
WEB='https://2384.tainan.gov.tw/NewTNBusWeb/APIs'
mkdir -p "$OUT"

# route headers (also the id source for everything below)
curl -s -m 30 "$API/PathListV2.ashx?Lang=cht&prj=tn" -o "$BASE/pathList.json"
jq -e 'type == "array" and length > 100' "$BASE/pathList.json" >/dev/null \
  || { echo "FAIL pathList"; exit 1; }
jq -r '.[].id' "$BASE/pathList.json" > /tmp/tn_ids.txt
echo "routes: $(wc -l < /tmp/tn_ids.txt | tr -d ' ')"

fetch_route() {
  local id="$1"
  local f="$OUT/route_${id}.json"
  [ -s "$f" ] && jq -e 'type == "array" and length >= 1' "$f" >/dev/null 2>&1 && return 0
  for attempt in 1 2 3; do
    curl -s -m 30 "$API/CrossRoutesByPathName.ashx?id=${id}&prj=tn" -o "$f"
    if jq -e 'type == "array" and length >= 1 and .[0].stop' "$f" >/dev/null 2>&1; then
      return 0
    fi
    sleep $((attempt * 2))
  done
  echo "FAIL route $id"
  return 1
}

fetch_geom() {
  local id="$1" dir="$2"   # dir = go | back
  local f="$OUT/geom_${id}_${dir}.json"
  [ -s "$f" ] && jq -e 'type == "array"' "$f" >/dev/null 2>&1 && return 0
  for attempt in 1 2 3; do
    curl -s -m 30 --url "$WEB/ReadText.ashx" \
      -H 'X-Requested-With: XMLHttpRequest' \
      --data "Path=${id}_${dir}&prj=tn" -o "$f"
    if jq -e 'type == "array"' "$f" >/dev/null 2>&1; then
      return 0
    fi
    sleep $((attempt * 2))
  done
  echo "FAIL geom ${id}_${dir}"
  return 1
}

fetch_all() {
  local id="$1"
  fetch_route "$id" || return 1
  fetch_geom "$id" go || true   # single-direction routes legitimately miss one
  fetch_geom "$id" back || true
  echo "OK $id"
}
export -f fetch_route fetch_geom fetch_all
export OUT API WEB

cat /tmp/tn_ids.txt | xargs -P 4 -I{} bash -c 'fetch_all "$@"' _ {}
echo "DONE routes=$(ls "$OUT" | grep -c '^route_' | tr -d ' ') geoms=$(ls "$OUT" | grep -c '^geom_' | tr -d ' ')"
