# DB schema relationships — route side & stop/station side

ASCII map of the versioned model in `crates/rm-db/migrations/0001_init.sql`
(plus 0003_segment, 0005_bearing, 0010_edit). Two versioning idioms coexist:

- **Route side** — workflow versioning: `route.current_route_version_id`
  back-pointer + `route_version.status` chain
  (DRAFT → PUBLISHED → SUPERSEDED / CANCELLED).
- **Stop/station side** — temporal versioning: identity tables carry **no**
  current-version pointer; the current version is simply the row with
  `valid_to IS NULL`.

---

## Route side

```
             IDENTITY LAYER (stable — one row per real-world thing)

 ┌──────────────────────────────┐              ┌──────────────────────────────┐
 │ route                        │ 1        *   │ subroute                     │
 ├──────────────────────────────┤──────────────┤──────────────────────────────┤
 │ route_id            (PK)     │              │ subroute_id         (PK)     │
 │ route_code                   │              │ route_id (FK) ──▶ route      │
 │ ext_uid   UNIQUE  tcg:{xno}  │              │ subroute_code                │
 │ current_route_version_id ────┼──┐           │ UNIQUE (route_id,            │
 │ revision                     │  │           │          subroute_code)      │
 └──────────────────────────────┘  │           └──────────────────────────────┘
               ▲                   │
               │                   │
     route_id  │          back-pointer edge — an explicit FK column.
     (FK)      │          NEVER join route_id = route_version_id by value
               │          (both counters start at 1 — coincidence join).
 ┌─────────────┴────────────────┐             ┌──────────────────────────────┐
 │ route_version                │             │ subroute_version             │
 ├──────────────────────────────┤ 1        *  ├──────────────────────────────┤
 │ route_version_id    (PK)     │─────────────│                              │
 │ route_id (FK) ──▶ route      │             │ subroute_version_id (PK)     │
 │ version_no                   │◄────────────┤ route_version_id (FK)        |
 │ UNIQUE (route_id, version_no)│             │ subroute_id (FK)      ───────┼──▶ subroute
 │ valid_from / valid_to        │             │ subroute_name                │
 │ status  DRAFT | PUBLISHED |  │             │ UNIQUE (route_version_id,    │
 │         SUPERSEDED|CANCELLED │             │          subroute_id)        │
 └──────────────────────────────┘             └───────────────┬──────────────┘
                                                              │ 1
                                                              │
                                              subroute_version_id (FK)
                                                              │
 ┌──────────────────────────────┐             ┌───────────────┴──────────────┐
 │ stop_version (stop side,     │             │ route_stop_version           │
 │ see next section)            │    1      * │                              │
 │                              │◄────────────┤ route_stop_version_id (PK)   │
 └──────────────────────────────┘             │ subroute_version_id (FK) ────┼──▶ subroute_version (up)
                                              │ stop_version_id (FK)  ───────┼──▶ stop_version (left)
                                              │ direction  0=去程 1=返程      |
                                              │ stop_sequence                |
                                              │ source_order_no              |
                                              │ UNIQUE (subroute_version_id, |
                                              │    direction, stop_sequence) |
                                              └──────────────────────────────┘
```

- `direction` is an **integer column** (0/1), not a table — API "directions"
  are a GROUP BY over it.
- Cyclic routes may visit the same stop twice: there is deliberately **no**
  `UNIQUE (subroute_version_id, direction, stop_version_id)`.
- `route_stop_version` hangs off **`subroute_version`** — a `route_version`
  has no direct stop link.

### Route-level link tables (identity level, version-independent)

```
 ┌──────────────────────────────┐      ┌──────────────────────────────┐
 │ route_provider               │      │ route_link                   │
 ├──────────────────────────────┤      ├──────────────────────────────┤
 │ route_id    (PK, FK ─▶ route)│      │ main_route_id (PK, FK─▶route)│
 │ provider_code (PK)           │      │ side_route_id (PK, FK─▶route)│
 │ provider_name                │      │ PRIMARY KEY (main, side)     │
 └──────────────────────────────┘      └──────────────────────────────┘
```

### How route_version rows appear

`route_version` / `subroute_version` / `route_stop_version` rows are written
by the **import pipeline** (`rm-registry`). There is no edit or re-publish
flow on the route side today — `status = 'SUPERSEDED'` appears only in test
fixtures. Resolution of "the" version is always through
`route.current_route_version_id`.

---

## Stop / station side

```
            IDENTITY LAYER (stable — one row per real-world thing)
            ⚠ NO current-version pointers anywhere in this layer

 ┌──────────────────────────┐       ┌──────────────────────────┐       ┌────────────────────┐
 │ stop                     │       │ station                  │       │ station_group      │
 ├──────────────────────────┤       ├──────────────────────────┤       ├────────────────────┤
 │ stop_id         (PK)     │       │ station_id      (PK)     │ 1   * │ station_group_id   │
 │ (bare surrogate —        │       │ ext_uid     UNIQUE       │       │ (PK)               │
 │  no natural key)         │       │           tcg:node:{n}   │       │ (no payload —      │
 │ created_at               │       │ created_at               │       │  name lives in     │
 └──────────────────────────┘       └──────────────────────────┘       │  group_version)    │
                                                                      └────────────────────┘
            │                                    │                             │
            │                          station_id (FK → identity)      station_group_id (FK)
            │                                    │                             │
            ▼                                    ▼                             ▼
            VERSION LAYER (immutable snapshots; current = valid_to IS NULL)

 ┌───────────────────────────────────┐       ┌───────────────────────────────────┐
 │ stop_version                      │       │ station_version                   │
 ├───────────────────────────────────┤       ├───────────────────────────────────┤
 │ stop_version_id          (PK)     │       │ station_version_id       (PK)     │
 │ stop_id (FK) ──▶ stop             │       │ station_id (FK) ──▶ station       │
 │ station_id (FK) ──▶ station       │       │ station_group_id (FK)             │
 │             (identity table, NOT  │       │             ──▶ station_group     │
 │              station_version)     │       │             (identity table)      │
 │ stop_code / stop_name             │       │ station_name                      │
 │ direction                         │       │ latitude / longitude              │
 │ latitude / longitude              │       │ valid_from / valid_to             │
 │ valid_from / valid_to             │       │ source   NULL | 'manual' (0010)   │
 │ source   NULL | 'manual' (0010)   │       └───────────────────────────────────┘
 └───────────────────────────────────┘                    │ 1
            │ 1                                           │
            │                                             ▼
            │ *                            ┌───────────────────────────────────┐
 ┌───────────────────────────────────┐       │ station_group_version             │
 │ (consumed by route side)          │       ├───────────────────────────────────┤
 │ route_stop_version.stop_version_id│       │ station_group_version_id (PK)     │
 │ ──▶ stop_version (explicit FK)    │       │ station_group_id (FK)             │
 └───────────────────────────────────┘       │ station_group_name                │
                                             │ valid_from / valid_to             │
                                             └───────────────────────────────────┘
```

Key contrast with the route side:

| | route side | stop/station side |
|---|---|---|
| current resolution | explicit `route.current_route_version_id` | implicit — `valid_to IS NULL` |
| chain marker | `status` column | temporal close/reopen; chain in `edit_log` |
| natural key | `ext_uid` on `route` | `ext_uid` on `station`; **`stop` has none** |

---

## Derived layer (rebuilt per run — never stored inside `*_version`)

```
 ┌──────────────────────────────┐       ┌───────────────────────────────────┐
 │ stop_bearing (0005)          │       │ station_bearing (0005)            │
 ├──────────────────────────────┤       ├───────────────────────────────────┤
 │ stop_version_id (PK, FK)     │       │ station_id (PK, FK ─▶ station)    │
 │ bearing_deg  [0, 360)        │       │ bearing_deg (circular mean of     │
 │ source  OSM | POLYLINE       │       │   member stop bearings)           │
 │ computed_at                  │       │ member_count / max_spread_deg     │
 └──────────────────────────────┘       │ computed_at                       │
                                        └───────────────────────────────────┘

 ┌───────────────────────────────────────────────────────────────────────┐
 │ path_stop_projection (0003) — bridge between the two sides            │
 ├───────────────────────────────────────────────────────────────────────┤
 │ route_stop_version_id (PK, FK ─▶ route_stop_version)                  │
 │ route_version_id (FK ─▶ route_version)                                │
 │ direction, stop_sequence, chainage_m, snap_dist_m,                    │
 │ method PROJECTED|ENDPOINT|OUTLIER|NO_PATH, monotonic_clamped          │
 │ projected_lat / projected_lon                                         │
 └───────────────────────────────────────────────────────────────────────┘
```

⚠ `path_stop_projection.chainage_m` and `seg_match_way.cum_*_m` are on
different scales (provider polyline vs matched OSM path) — never join them.

---

## Edit audit layer (0010_edit)

```
 ┌───────────────────────────────────────────────┐   ┌───────────────────────────────┐
 │ edit_log                                      │   │ stale_mark                    │
 ├───────────────────────────────────────────────┤   ├───────────────────────────────┤
 │ edit_id (PK), ts, editor                      │   │ (table_name, key) PK          │
 │ entity    'stop' | 'station' | ...            │◀──┤ cause_edit_id (FK)            │
 │ entity_id (stable id — stop_id/station_id)    │   │ key: unit the refresh step    │
 │ action    update | move_station | undo | …    │   │      consumes ('*' = whole)   │
 │ changes   JSON {field: {old, new}}            │   └───────────────────────────────┘
 │ undo_of (FK ─▶ edit_log, self-reference)      │
 │ version_before / version_after                │──▶ stop_version_id /
 └───────────────────────────────────────────────┘    station_version_id
```

### Edit / undo flow (stop side; guards in `crates/rm-db/src/edit/mod.rs`)

```
 edit_stop / regroup / rename
   guard: version still current (valid_to IS NULL) — re-checked inside tx
   close old version    valid_to = now
   insert new version   valid_to NULL, source = 'manual'
   edit_log row         version_before / version_after, changes JSON
   stale_mark rows      for affected route projections (stale_routes)

 undo
   re-open version_before (re-validate), close version_after
   edit_log row with undo_of = original edit_id
   undo of an undo refused
```

---

## Cross-side links (summary)

```
 route.current_route_version_id ──▶ route_version.route_version_id
 subroute_version.route_version_id ──▶ route_version
 route_stop_version.stop_version_id ──▶ stop_version
 stop_version.station_id ──▶ station (identity!)
 station_version.station_group_id ──▶ station_group (identity!)
 path_stop_projection.route_stop_version_id ──▶ route_stop_version
 path_stop_projection.route_version_id ──▶ route_version
```

Rules already paid for (see `CLAUDE.md` correctness invariants):

- Join only on these explicit FK columns — never on coincidentally equal
  counters (`route_id` vs `route_version_id`).
- Never join `path_stop_projection.chainage_m` with `seg_match_way.cum_*_m`.
