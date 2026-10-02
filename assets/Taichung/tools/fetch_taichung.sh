#!/bin/bash
# Fetch route(xno) detail for every route id. 4 parallel, 2 retries each.
set -u
OUT=/Users/herry/project/rust/route_manger/assets/Taichung/raw
URL='https://citybus.taichung.gov.tw/ebus/graphql'

fetch() {
  local xno="$1"
  local f="$OUT/route_${xno}.json"
  [ -s "$f" ] && jq -e '.data.route' "$f" >/dev/null 2>&1 && return 0
  local body='{"query":"{ route(xno: '"$xno"', lang: \"zh\") { stations { edges { goBack ivrNo orderNo sid node { id lat lon name } } } routePoint { go back } providers { edges { node { id name opType } } } } }"}'
  for attempt in 1 2 3; do
    curl -s -m 30 --url "$URL" -H 'content-type: application/json' --data-raw "$body" -o "$f"
    if jq -e '.data.route' "$f" >/dev/null 2>&1; then
      echo "OK $xno"
      return 0
    fi
    sleep $((attempt * 2))
  done
  echo "FAIL $xno"
  return 1
}
export -f fetch
export OUT URL

cat /tmp/xnos.txt | xargs -P 4 -I{} bash -c 'fetch "$@"' _ {}
echo "DONE ok=$(ls "$OUT" | wc -l | tr -d ' ') fail=$(grep -c FAIL /dev/null 2>/dev/null || true)"
