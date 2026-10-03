# rm-core

The shared leaf library the rest of the pipeline builds on: the
`LatLng` domain type, the Google polyline codec the ebus captures
arrive in, small geo helpers, and the blake3 hasher behind segment
identity. Not a pipeline stage — no binary, no database access, no
knowledge of the schema.

## What's in it

| Module | Provides |
|---|---|
| `lib.rs` | `LatLng`, `blake3_128_hex` |
| `polyline` | `encode` / `decode` + `PolylineError` |
| `geo` | metres, projections, bearings over WGS84 |

## LatLng

A WGS84 pair in bus-domain order `(lat, lon)`, matching the source
APIs. Convert to geo-types `Coord` (x = lon, y = lat) only at the
geometry boundary — one conversion point, instead of two conventions
drifting through the pipeline.

## Polyline codec

Google's encoded polyline algorithm at the format's 1e-5 precision
(delta + zigzag + 5-bit varint). Taichung ebus `routePoint.go` /
`.back` arrive in this format. Two behaviours worth knowing:

- `decode` drops consecutive duplicate points — the raw captures
  contain 988 of them, and they break chainage monotonicity
  downstream.
- Malformed input is an error, not garbage: `TruncatedVarint` (stream
  chopped mid-varint) or `InvalidByte` (outside the format's ASCII
  63..=126).

Unit tests pin `encode` against Google's documented reference vector
and round-trip a real `route_1010.json` capture byte-identically.

## Geo helpers

No `proj` dependency: Taichung spans ~40 km, and at stop scale the
equirectangular/Haversine error is negligible — so `geo.rs` is a
handful of functions over plain WGS84:

- `haversine_m` — great-circle distance in metres.
- `point_to_segment_m` — projection of a point onto a segment, in a
  local metre frame anchored at the segment's first endpoint.
- `path_length_m` — polyline arc length (sum of haversine legs).
- `bearing_deg` — initial bearing, degrees clockwise from north.
- `angular_diff_deg` — smaller angle between two bearings.
- `circular_mean_deg` / `axial_mean_deg` — vector mean of bearings,
  and the axis variant where headings 180° apart fold onto one road
  axis (a pole legitimately serves both directions of a street).

## Hashing

`blake3_128_hex`: 128-bit blake3, hex-encoded (32 chars) — short
enough for an id column. The unit test pins that swapping the two
station uids in the input changes the hash, which is what lets the
reverse corridor hash differently (two ids per physical corridor).

## Why this crate exists

The pipeline's cross-stage agreements all live here:

| Consumer | Uses it for |
|---|---|
| `rm-segments` | polyline decode, stop projection + chainage |
| `rm-osm-match` | trace decode, `path_length_m` in the matcher |
| `rm-registry` | `segment_id` hashing, stop/station bearings |
| `rm-identity` | station-group `ext_uid` hashing |
| `rm-verify`, `rm-web` | polyline decode / `LatLng` for QA exports and map geometry |

One `LatLng` and one metre means the 80 m grouping rule in
`rm-identity` and the snap distances in `rm-osm-match` measure the
same thing; one hash function means an id minted by `rm-registry` is
reproducible anywhere. (`rm-db`, `rm-ingest`, `rm-osm-prepare` and
`rm-publish` declare the dependency but don't call into it yet.)
