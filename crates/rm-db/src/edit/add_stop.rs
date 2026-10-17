//! Add-stop edit primitive: the command types, the qualify-or-create
//! station search, and the read-only plan that previews the write.

use std::collections::BTreeSet;

use anyhow::Result;
use rm_core::{geo, LatLng};
use rusqlite::{Connection, OptionalExtension};

use super::matched_path::{bearing_at, matched_path, project_onto_path, OFF_PATH_MAX_M};
use super::EditError;
use crate::stale::seg_match_key;

/// Maximum distance (m) between the proposed coordinate and a current
/// station's reference point for qualification.
pub const QUALIFY_MAX_DIST_M: f64 = 500.0;

/// Bearing tolerance (deg) between the approach heading and a station's
/// bearing — directional for signed stations, axial for spread ones.
pub const QUALIFY_BEARING_TOL_DEG: f64 = 30.0;

/// Duplicate of `rm-registry::bearing::SIGNED_SPREAD_MAX_DEG` (rm-db
/// cannot depend on rm-registry): below it the stored `bearing_deg` is a
/// signed heading, at/above it an axis.
const SIGNED_SPREAD_MAX_DEG: f64 = 45.0;

/// Add-stop command: the new pole's name and position, an optional
/// existing station to join (`None` = caller still owes the
/// qualify-or-create decision), and one attachment per direction to add
/// the pole to.
#[derive(Debug, Clone)]
pub struct AddStopCmd {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub station_id: Option<i64>,
    pub attachments: Vec<AttachCmd>,
}

/// One direction attachment. `after_stop_code: None` = append to the end
/// of the direction's stop sequence.
#[derive(Debug, Clone)]
pub struct AttachCmd {
    pub xno: String,
    pub direction: i64,
    pub after_stop_code: Option<String>,
}

/// A current station version that passed qualification.
#[derive(Debug, Clone)]
pub struct StationCandidate {
    pub station_id: i64,
    pub station_version_id: i64,
    pub station_name: String,
    pub dist_m: f64,
}

/// Result of the qualify-or-create search: the best candidate (`None` =
/// no current station qualifies, create a new one) plus warnings for
/// conditions that could not be checked.
#[derive(Debug)]
pub struct Qualification {
    pub matched: Option<StationCandidate>,
    pub warnings: Vec<String>,
}

/// Axial (direction-blind) difference of two bearings: fold onto the
/// 0-180 axis, then take the smaller side — 265° vs 90° is 5°, not 175°.
/// For spread stations, whose `bearing_deg` is an axis, not a heading.
fn axial_diff_deg(a: f64, b: f64) -> f64 {
    let d = (a - b).rem_euclid(180.0);
    d.min(180.0 - d)
}

/// Qualify-or-create search for `AddStopCmd.station_id`: among current
/// station versions (`valid_to IS NULL`), those whose normalized name
/// (trim + lowercase) equals the proposed name AND whose reference point
/// sits within [`QUALIFY_MAX_DIST_M`] of the proposed coordinate.
/// Nearest wins, then lowest `station_id`. When the station has a
/// `station_bearing` row and the caller supplied an approach bearing, it
/// must agree within [`QUALIFY_BEARING_TOL_DEG`] — directionally for
/// signed stations (`max_spread_deg < 45`), axially for spread ones. A
/// missing bearing row or missing approach bearing drops the condition
/// and pushes a warning. Read-only.
pub fn qualify_station(
    conn: &Connection,
    name: &str,
    lat: f64,
    lon: f64,
    approach_bearing: Option<f64>,
) -> Result<Qualification> {
    let query = LatLng::new(lat, lon);
    let want = name.trim().to_lowercase();
    // station_version carries station_id; ORDER BY keeps the scan
    // deterministic (the final tie-break re-sorts by (dist, id))
    let mut stmt = conn.prepare(
        "SELECT station_id, station_version_id, station_name, latitude, longitude
         FROM station_version WHERE valid_to IS NULL
         ORDER BY station_id",
    )?;
    let rows: Vec<(i64, i64, String, f64, f64)> = stmt
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let mut qualifiers: Vec<(StationCandidate, bool)> = Vec::new(); // bool = bearing condition dropped
    for (station_id, station_version_id, station_name, slat, slon) in rows {
        if station_name.trim().to_lowercase() != want {
            continue;
        }
        let dist_m = geo::haversine_m(query, LatLng::new(slat, slon));
        if dist_m > QUALIFY_MAX_DIST_M {
            continue;
        }
        // Bearing gate: decidable only when the station HAS a bearing row
        // and the caller HAS an approach heading; anything else drops the
        // condition (warning on the winner below).
        let bearing: Option<(f64, f64)> = conn
            .query_row(
                "SELECT bearing_deg, max_spread_deg FROM station_bearing
                 WHERE station_id = ?1",
                [station_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let mut dropped = false;
        if let (Some((deg, spread)), Some(approach)) = (bearing, approach_bearing) {
            let diff = if spread < SIGNED_SPREAD_MAX_DEG {
                geo::angular_diff_deg(approach, deg) // signed heading: compare directions
            } else {
                axial_diff_deg(approach, deg) // spread station: bearing_deg is an axis
            };
            if diff > QUALIFY_BEARING_TOL_DEG {
                continue;
            }
        } else {
            dropped = true;
        }
        qualifiers.push((
            StationCandidate {
                station_id,
                station_version_id,
                station_name,
                dist_m,
            },
            dropped,
        ));
    }
    qualifiers.sort_by(|a, b| {
        a.0.dist_m
            .total_cmp(&b.0.dist_m)
            .then_with(|| a.0.station_id.cmp(&b.0.station_id))
    });
    let mut warnings = Vec::new();
    let matched = qualifiers.into_iter().next().map(|(c, dropped)| {
        if dropped {
            warnings.push(format!(
                "station {} '{}': bearing condition dropped — no station_bearing \
                 row or no approach bearing",
                c.station_id, c.station_name
            ));
        }
        c
    });
    Ok(Qualification { matched, warnings })
}

// ---------- read-only plan ----------

/// The station decision the apply carries out: join a station found by
/// qualification, mint a new one, or the caller's pinned `station_id`.
#[derive(Debug, Clone)]
pub enum StationVerdict {
    UseExisting { station_id: i64, name: String },
    CreateNew,
    Pinned { station_id: i64, name: String },
}

/// One attachment resolved against the current DB: the planned insert
/// position (append when `insert_after_stop_code` is `None`) and the
/// neighbor pole names the dry-run shows.
#[derive(Debug, Clone)]
pub struct PlannedAttach {
    pub xno: String,
    pub direction: i64,
    pub route_id: i64,
    pub insert_after_stop_code: Option<String>,
    pub neighbor_before: Option<String>,
    pub neighbor_after: Option<String>,
}

/// Rows each table gains when the plan is applied (the apply's mint order:
/// station chain when `CreateNew`, one `stop`, one `stop_version` per
/// attachment, one `route_version` + `subroute_version` per distinct route
/// with its whole current `route_stop_version` sequence re-minted, and the
/// stale marks' `(table, key)` set — `'*'`-keyed tables collapse).
#[derive(Debug, Clone, PartialEq)]
pub struct RowCounts {
    pub stop: u32,
    pub stop_version: u32,
    pub station: u32,
    pub station_version: u32,
    pub station_group: u32,
    pub station_group_version: u32,
    pub route_version: u32,
    pub subroute_version: u32,
    pub route_stop_version: u32,
    pub stale_mark: u32,
}

/// Read-only preview of an add-stop: what apply would write, what would go
/// stale, and every condition short of a rejection as a warning.
#[derive(Debug)]
pub struct AddStopPlan {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub station: StationVerdict,
    pub attachments: Vec<PlannedAttach>,
    pub row_counts: RowCounts,
    pub stale_routes: Vec<i64>,
    pub warnings: Vec<String>,
}

/// One attachment validated against the current DB: the direction's
/// sequence in travel order, the resolved insert position (the new stop
/// precedes `seq[insert_idx]`; append ⇒ `seq.len()`), and the matched path
/// with the proposed coordinate's projection when one exists.
struct ResolvedAttach {
    xno: String,
    direction: i64,
    route_id: i64,
    route_version_id: i64,
    seq: Vec<(String, String)>, // (stop_code, stop_name) in travel order
    insert_idx: usize,
    path: Option<Vec<LatLng>>,
    projection: Option<(f64, f64)>, // (chainage_m, dist_m)
}

/// Dry-run the add-stop against the current DB without writing a byte.
/// Validation order (first error wins): duplicate `(xno, direction)` →
/// xno resolution (missing route row or NULL `current_route_version_id`
/// both → `UnknownRoute`, mirroring `load_polyline_line`) → direction
/// missing from the current version's `route_stop_version` rows
/// (`UnknownDirection`) → `after_stop_code` absent from that direction's
/// current sequence (`UnknownAfterStop`) → off-path check (`OffPathStop`;
/// skipped with a warning when the matched-path loader returns `None`) →
/// pinned `station_id` not current (`UnknownStation`). The qualification
/// approach bearing comes from the first attachment that yields a matched
/// path. Warnings: station-level first (qualification's, then the
/// same-name-pole check against current `stop_version` rows within
/// [`QUALIFY_MAX_DIST_M`]), then per-attachment no-matched-path in
/// attachment order.
pub fn plan_add_stop(conn: &Connection, cmd: &AddStopCmd) -> Result<AddStopPlan> {
    // duplicate (xno, direction)
    for (i, a) in cmd.attachments.iter().enumerate() {
        if cmd.attachments[..i]
            .iter()
            .any(|p| p.xno == a.xno && p.direction == a.direction)
        {
            return Err(EditError::DuplicateAttach(a.xno.clone(), a.direction).into());
        }
    }

    let query = LatLng::new(cmd.lat, cmd.lon);
    let mut resolved = Vec::with_capacity(cmd.attachments.len());
    for a in &cmd.attachments {
        let (route_id, route_version_id): (i64, Option<i64>) = match conn.query_row(
            "SELECT route_id, current_route_version_id FROM route WHERE ext_uid = ?1",
            [format!("tcg:{}", a.xno)],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ) {
            Ok(v) => v,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                return Err(EditError::UnknownRoute(a.xno.clone()).into())
            }
            Err(e) => return Err(e.into()),
        };
        let Some(route_version_id) = route_version_id else {
            return Err(EditError::UnknownRoute(a.xno.clone()).into());
        };
        let subroute_version_id: Option<i64> = conn
            .query_row(
                "SELECT subroute_version_id FROM subroute_version
                 WHERE route_version_id = ?1
                 ORDER BY subroute_version_id LIMIT 1",
                [route_version_id],
                |r| r.get(0),
            )
            .optional()?;
        let sequence: Vec<(String, String)> = match subroute_version_id {
            Some(sv) => {
                let mut stmt = conn.prepare(
                    "SELECT s.stop_code, s.stop_name
                     FROM route_stop_version rsv
                     JOIN stop_version s ON s.stop_version_id = rsv.stop_version_id
                     WHERE rsv.subroute_version_id = ?1 AND rsv.direction = ?2
                     ORDER BY rsv.stop_sequence",
                )?;
                let rows = stmt
                    .query_map([sv, a.direction], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<Result<Vec<_>, _>>()?;
                rows
            }
            None => Vec::new(),
        };
        if sequence.is_empty() {
            return Err(EditError::UnknownDirection(a.xno.clone(), a.direction).into());
        }
        let insert_idx = match &a.after_stop_code {
            None => sequence.len(),
            Some(code) => sequence
                .iter()
                .position(|(c, _)| c == code)
                .map(|i| i + 1)
                .ok_or_else(|| {
                    EditError::UnknownAfterStop(a.xno.clone(), a.direction, code.clone())
                })?,
        };
        let path = matched_path(conn, route_id, a.direction)?;
        let projection = path.as_ref().and_then(|p| project_onto_path(p, query));
        resolved.push(ResolvedAttach {
            xno: a.xno.clone(),
            direction: a.direction,
            route_id,
            route_version_id,
            seq: sequence,
            insert_idx,
            path,
            projection,
        });
    }

    // off-path: reject past the bound; otherwise keep the first attachment's
    // projection chainage as the qualification approach bearing
    let mut approach_bearing: Option<f64> = None;
    for r in &resolved {
        if let Some((chainage_m, dist_m)) = r.projection {
            if dist_m > OFF_PATH_MAX_M {
                return Err(EditError::OffPathStop {
                    xno: r.xno.clone(),
                    direction: r.direction,
                    dist_m,
                    max_m: OFF_PATH_MAX_M,
                }
                .into());
            }
            if approach_bearing.is_none() {
                if let Some(path) = &r.path {
                    approach_bearing = bearing_at(path, chainage_m);
                }
            }
        }
    }

    let mut warnings = Vec::new();
    let station = match cmd.station_id {
        Some(station_id) => {
            let name: Option<String> = conn
                .query_row(
                    "SELECT station_name FROM station_version
                     WHERE station_id = ?1 AND valid_to IS NULL
                     ORDER BY station_version_id LIMIT 1",
                    [station_id],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(name) = name else {
                return Err(EditError::UnknownStation(station_id).into());
            };
            StationVerdict::Pinned { station_id, name }
        }
        None => {
            let q = qualify_station(conn, &cmd.name, cmd.lat, cmd.lon, approach_bearing)?;
            warnings.extend(q.warnings);
            match q.matched {
                Some(c) => StationVerdict::UseExisting {
                    station_id: c.station_id,
                    name: c.station_name,
                },
                None => StationVerdict::CreateNew,
            }
        }
    };

    // same-name pole check: any current stop_version with the normalized
    // name within the qualification distance
    let want = cmd.name.trim().to_lowercase();
    let mut stmt = conn.prepare(
        "SELECT stop_code, stop_name, latitude, longitude FROM stop_version
         WHERE valid_to IS NULL
         ORDER BY stop_version_id",
    )?;
    let poles: Vec<(String, String, f64, f64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    for (code, name, lat, lon) in poles {
        if name.trim().to_lowercase() != want {
            continue;
        }
        let dist_m = geo::haversine_m(query, LatLng::new(lat, lon));
        if dist_m <= QUALIFY_MAX_DIST_M {
            warnings.push(format!(
                "same-name stop {code} '{name}' is {dist_m:.0} m from the new pole — \
                 check for duplicates"
            ));
        }
    }
    // per-attachment: the loader found nothing to project onto
    for r in &resolved {
        if r.projection.is_none() {
            warnings.push(format!(
                "route {} direction {}: no matched path — off-path check skipped",
                r.xno, r.direction
            ));
        }
    }

    // distinct routes in attachment first-appearance order (counts keyed by
    // the route's current version; stale_routes sorted ascending)
    let mut routes: Vec<(i64, i64)> = Vec::new(); // (route_id, route_version_id)
    for r in &resolved {
        if !routes.iter().any(|(rid, _)| *rid == r.route_id) {
            routes.push((r.route_id, r.route_version_id));
        }
    }
    let mut existing_rsv: u32 = 0;
    for (_, rv) in &routes {
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM route_stop_version
             WHERE subroute_version_id IN (
                 SELECT subroute_version_id FROM subroute_version
                 WHERE route_version_id = ?1)",
            [rv],
            |r| r.get(0),
        )?;
        existing_rsv += n as u32;
    }

    // the stale keys apply would write: mark_line_stale per (route,
    // direction) + mark_position_stale per new stop_version; INSERT OR
    // REPLACE collapses onto the (table_name, key) primary key
    let mut stale_keys: BTreeSet<(String, String)> = BTreeSet::new();
    for r in &resolved {
        stale_keys.insert(("seg_match".into(), seg_match_key(r.route_id, r.direction)));
        stale_keys.insert(("path_stop_projection".into(), r.route_id.to_string()));
        stale_keys.insert(("stop_bearing".into(), "*".into()));
        stale_keys.insert(("station_bearing".into(), "*".into()));
    }

    let create_new = u32::from(matches!(station, StationVerdict::CreateNew));
    let n_attach = resolved.len() as u32;
    let n_routes = routes.len() as u32;
    let mut stale_routes: Vec<i64> = routes.iter().map(|(rid, _)| *rid).collect();
    stale_routes.sort_unstable();

    Ok(AddStopPlan {
        name: cmd.name.clone(),
        lat: cmd.lat,
        lon: cmd.lon,
        station,
        attachments: resolved
            .iter()
            .map(|r| {
                let (before_code, before_name) = &r.seq[r.insert_idx - 1];
                PlannedAttach {
                    xno: r.xno.clone(),
                    direction: r.direction,
                    route_id: r.route_id,
                    insert_after_stop_code: (r.insert_idx < r.seq.len())
                        .then(|| before_code.clone()),
                    neighbor_before: Some(before_name.clone()),
                    neighbor_after: r.seq.get(r.insert_idx).map(|(_, n)| n.clone()),
                }
            })
            .collect(),
        row_counts: RowCounts {
            stop: 1,
            stop_version: n_attach,
            station: create_new,
            station_version: create_new,
            station_group: create_new,
            station_group_version: create_new,
            route_version: n_routes,
            subroute_version: n_routes,
            route_stop_version: existing_rsv + n_attach,
            stale_mark: stale_keys.len() as u32,
        },
        stale_routes,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_in_memory;
    use rm_core::geo;
    use rusqlite::{params, Connection};

    const T0: &str = "2026-01-01T00:00:00Z";

    /// Degrees of latitude per metre on a meridian (inverse of rm_core
    /// geo's private DEG_TO_M, duplicated for fixture coordinates).
    const M_TO_DEG: f64 = 180.0 / (std::f64::consts::PI * 6_371_008.8);

    /// Station 'S' (station 1, svi 20) at (24, 120) with pole sv 10, the
    /// published route chain (route 1 -> rv 1 -> subroute_version 1 ->
    /// rsv 1) and a matched way run — the full chain the planner reads,
    /// in the sibling edit fixtures' shape.
    fn fixture(conn: &Connection) {
        conn.execute_batch(&format!(
            "INSERT INTO station_group (station_group_id, created_at) VALUES (1, '{T0}');
             INSERT INTO station_group_version (station_group_version_id, station_group_id, station_group_name, source, valid_from, created_at)
                 VALUES (1, 1, 'g', 'capture', '{T0}', '{T0}');
             INSERT INTO station (station_id, ext_uid, created_at) VALUES (1, 'tcg:node:1', '{T0}');
             INSERT INTO station_version (station_version_id, station_id, station_group_id, station_name, latitude, longitude, valid_from, created_at)
                 VALUES (20, 1, 1, 'S', 24.0, 120.0, '{T0}', '{T0}');
             INSERT INTO stop (stop_id, ext_uid, created_at) VALUES (1, 'tcg:stop:1', '{T0}');
             INSERT INTO stop_version (stop_version_id, stop_id, station_id, stop_code, stop_name, direction, latitude, longitude, valid_from, created_at)
                 VALUES (10, 1, 1, '001', 'S', 0, 24.0, 120.0, '{T0}', '{T0}');
             INSERT INTO route (route_id, route_code, ext_uid, revision, created_at, updated_at)
                 VALUES (1, '1', 'tcg:1', 1, '{T0}', '{T0}');
             INSERT INTO route_version (route_version_id, route_id, version_no, valid_from, status, created_at)
                 VALUES (1, 1, 1, '{T0}', 'PUBLISHED', '{T0}');
             UPDATE route SET current_route_version_id = 1 WHERE route_id = 1;
             INSERT INTO subroute (subroute_id, route_id, subroute_code, created_at)
                 VALUES (1, 1, '1-0', '{T0}');
             INSERT INTO subroute_version (subroute_version_id, route_version_id, subroute_id, subroute_name, created_at)
                 VALUES (1, 1, 1, '1', '{T0}');
             INSERT INTO route_stop_version (route_stop_version_id, subroute_version_id, stop_version_id, direction, stop_sequence, created_at)
                 VALUES (1, 1, 10, 0, 1, '{T0}');
             INSERT INTO osm_way (osm_way_id, highway, oneway, name) VALUES (100, 'residential', 0, '路');
             INSERT INTO osm_node (osm_node_id, lat, lon) VALUES
                 (1, 24.1400, 120.6857), (2, 24.1405, 120.6857), (3, 24.1410, 120.6857);
             INSERT INTO osm_way_node (osm_way_id, ordinal, osm_node_id) VALUES
                 (100, 1, 1), (100, 2, 2), (100, 3, 3);
             INSERT INTO seg_match_way
                 (subroute_version_id, direction, ordinal, osm_way_id,
                  entry_node_id, exit_node_id, cum_start_m, cum_end_m)
                 VALUES (1, 0, 1, 100, 1, 3, 0.0, 200.0);
             INSERT INTO id_sequence (table_name, next_value) VALUES ('station_version', 21);
             INSERT INTO id_sequence (table_name, next_value) VALUES ('stop_version', 11);"
        ))
        .unwrap();
    }

    /// One more current station in group 1; the fixture's chain stays put.
    fn seed_station(conn: &Connection, station_id: i64, svi: i64, name: &str, lat: f64, lon: f64) {
        conn.execute(
            "INSERT INTO station (station_id, ext_uid, created_at)
             VALUES (?1, 'tcg:node:' || ?1, '2026-01-01T00:00:00Z')",
            [station_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO station_version
                (station_version_id, station_id, station_group_id, station_name,
                 latitude, longitude, valid_from, created_at)
             VALUES (?1, ?2, 1, ?3, ?4, ?5, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            params![svi, station_id, name, lat, lon],
        )
        .unwrap();
    }

    /// A station_bearing row as rm-registry writes it (this module only
    /// reads them).
    fn seed_bearing(
        conn: &Connection,
        station_id: i64,
        bearing_deg: f64,
        members: i64,
        spread: f64,
    ) {
        conn.execute(
            "INSERT INTO station_bearing
                (station_id, bearing_deg, member_count, max_spread_deg, computed_at)
             VALUES (?1, ?2, ?3, ?4, '2026-01-02T00:00:00Z')",
            params![station_id, bearing_deg, members, spread],
        )
        .unwrap();
    }

    #[test]
    fn qualify_matches_same_name_nearby() {
        let conn = open_in_memory().unwrap();
        fixture(&conn);
        // name equal after trim + case-fold; approach supplied, but the
        // station has no bearing row: condition dropped, one warning
        let q = qualify_station(&conn, "  s ", 24.0, 120.0, Some(90.0)).unwrap();
        let m = q.matched.expect("same-name nearby station matches");
        assert_eq!(m.station_id, 1);
        assert_eq!(m.station_version_id, 20);
        assert_eq!(m.station_name, "S");
        assert!(m.dist_m < QUALIFY_MAX_DIST_M);
        assert_eq!(q.warnings.len(), 1);
        assert!(
            q.warnings[0].contains("bearing condition dropped"),
            "{:?}",
            q.warnings
        );
    }

    #[test]
    fn qualify_rejects_distant_or_renamed() {
        let conn = open_in_memory().unwrap();
        fixture(&conn);
        // same coords, different normalized name
        let q = qualify_station(&conn, "Other", 24.0, 120.0, None).unwrap();
        assert!(q.matched.is_none());
        // right name, 600 m away
        conn.execute(
            "UPDATE station_version SET latitude = ?1 WHERE station_version_id = 20",
            [24.0 + 600.0 * M_TO_DEG],
        )
        .unwrap();
        let q = qualify_station(&conn, "S", 24.0, 120.0, None).unwrap();
        assert!(q.matched.is_none());
    }

    #[test]
    fn qualify_axial_station_matches_opposite_heading() {
        let conn = open_in_memory().unwrap();
        fixture(&conn);
        seed_bearing(&conn, 1, 90.0, 2, 60.0); // spread >= 45: axial station
                                               // pin the premise: directionally the approach is 175 deg off —
                                               // only the axial fold can accept it
        assert!(geo::angular_diff_deg(265.0, 90.0) > QUALIFY_BEARING_TOL_DEG);
        let q = qualify_station(&conn, "S", 24.0, 120.0, Some(265.0)).unwrap();
        let m = q.matched.expect("axial opposite heading must match");
        assert_eq!(m.station_id, 1);
        assert!(q.warnings.is_empty());
    }

    #[test]
    fn qualify_signed_station_rejects_opposite() {
        let conn = open_in_memory().unwrap();
        fixture(&conn);
        seed_bearing(&conn, 1, 90.0, 2, 20.0); // spread < 45: signed heading
        let q = qualify_station(&conn, "S", 24.0, 120.0, Some(265.0)).unwrap();
        assert!(q.matched.is_none());
        let q = qualify_station(&conn, "S", 24.0, 120.0, Some(100.0)).unwrap();
        assert_eq!(
            q.matched.expect("10 deg off the signed heading").station_id,
            1
        );
    }

    #[test]
    fn qualify_thresholds_500m_30deg() {
        let conn = open_in_memory().unwrap();
        fixture(&conn);
        // exactly 500 m north of the query point, minus a 0.1 um nudge:
        // at the exact delta the double-rounded haversine lands a hair
        // ABOVE 500.0 and the inclusive <= boundary would ride on float noise
        seed_station(&conn, 2, 21, "TH1", 24.0 + 500.0 * M_TO_DEG - 1e-12, 120.0);
        seed_station(&conn, 3, 22, "TH2", 24.0, 120.0);
        seed_bearing(&conn, 3, 90.0, 2, 20.0);

        let q = qualify_station(&conn, "TH1", 24.0, 120.0, None).unwrap();
        assert!(q.matched.is_some(), "exactly 500 m qualifies");
        conn.execute(
            "UPDATE station_version SET latitude = ?1 WHERE station_version_id = 21",
            [24.0 + 500.1 * M_TO_DEG],
        )
        .unwrap();
        let q = qualify_station(&conn, "TH1", 24.0, 120.0, None).unwrap();
        assert!(q.matched.is_none(), "500.1 m does not qualify");

        let q = qualify_station(&conn, "TH2", 24.0, 120.0, Some(120.0)).unwrap();
        assert_eq!(
            q.matched.expect("exactly 30 deg off qualifies").station_id,
            3
        );
        let q = qualify_station(&conn, "TH2", 24.0, 120.0, Some(120.5)).unwrap();
        assert!(q.matched.is_none(), "30.5 deg off does not qualify");
    }

    #[test]
    fn qualify_tie_break_nearest_then_lowest_id() {
        let conn = open_in_memory().unwrap();
        fixture(&conn);
        seed_station(&conn, 20, 21, "Tie", 24.0 + 100.0 * M_TO_DEG, 120.0);
        seed_station(&conn, 30, 22, "Tie", 24.0, 120.0);
        // nearer wins even with the higher id
        let q = qualify_station(&conn, "Tie", 24.0, 120.0, None).unwrap();
        assert_eq!(q.matched.unwrap().station_id, 30);
        // equal distance: lower station_id wins
        conn.execute(
            "UPDATE station_version SET latitude = 24.0 WHERE station_version_id = 21",
            [],
        )
        .unwrap();
        let q = qualify_station(&conn, "Tie", 24.0, 120.0, None).unwrap();
        assert_eq!(q.matched.unwrap().station_id, 20);
    }

    // ---------- plan_add_stop ----------

    /// Two published routes over Task 1's N-S road (way 100, nodes 1-5 at
    /// lon 120.6857): route 1 (xno '1') dir 0 = 'A'(001) → 'B'(002) with a
    /// matched run, route 2 (xno '2') dir 0 = 'C'(001) with a matched run
    /// and dir 1 = 'C 往' with NO matched run. Station 'S' sits 60+ km away,
    /// so on-path commands never qualify a station (verdict CreateNew).
    fn plan_fixture(conn: &Connection) {
        conn.execute_batch(&format!(
            "INSERT INTO station_group (station_group_id, created_at) VALUES (1, '{T0}');
             INSERT INTO station_group_version (station_group_version_id, station_group_id, station_group_name, source, valid_from, created_at)
                 VALUES (1, 1, 'g', 'capture', '{T0}', '{T0}');
             INSERT INTO station (station_id, ext_uid, created_at) VALUES (1, 'tcg:node:1', '{T0}');
             INSERT INTO station_version (station_version_id, station_id, station_group_id, station_name, latitude, longitude, valid_from, created_at)
                 VALUES (20, 1, 1, 'S', 24.0, 120.0, '{T0}', '{T0}');
             INSERT INTO stop (stop_id, ext_uid, created_at) VALUES
                 (1, 'tcg:stop:1', '{T0}'), (2, 'tcg:stop:2', '{T0}'), (3, 'tcg:stop:3', '{T0}');
             INSERT INTO stop_version (stop_version_id, stop_id, station_id, stop_code, stop_name, direction, latitude, longitude, valid_from, created_at) VALUES
                 (10, 1, 1, '001', 'A', 0, 24.1405, 120.6857, '{T0}', '{T0}'),
                 (11, 2, 1, '002', 'B', 0, 24.1415, 120.6857, '{T0}', '{T0}'),
                 (12, 3, 1, '001', 'C', 0, 24.1400, 120.6857, '{T0}', '{T0}'),
                 (13, 3, 1, '001', 'C 往', 1, 24.1400, 120.6857, '{T0}', '{T0}');
             INSERT INTO route (route_id, route_code, ext_uid, revision, created_at, updated_at) VALUES
                 (1, '1', 'tcg:1', 1, '{T0}', '{T0}'),
                 (2, '2', 'tcg:2', 1, '{T0}', '{T0}');
             INSERT INTO route_version (route_version_id, route_id, version_no, valid_from, status, created_at) VALUES
                 (1, 1, 1, '{T0}', 'PUBLISHED', '{T0}'),
                 (2, 2, 1, '{T0}', 'PUBLISHED', '{T0}');
             UPDATE route SET current_route_version_id = 1 WHERE route_id = 1;
             UPDATE route SET current_route_version_id = 2 WHERE route_id = 2;
             INSERT INTO subroute (subroute_id, route_id, subroute_code, created_at) VALUES
                 (1, 1, '1-0', '{T0}'), (2, 2, '2-0', '{T0}');
             INSERT INTO subroute_version (subroute_version_id, route_version_id, subroute_id, subroute_name, created_at) VALUES
                 (1, 1, 1, '1', '{T0}'), (2, 2, 2, '2', '{T0}');
             INSERT INTO route_stop_version (route_stop_version_id, subroute_version_id, stop_version_id, direction, stop_sequence, created_at) VALUES
                 (1, 1, 10, 0, 1, '{T0}'), (2, 1, 11, 0, 2, '{T0}'),
                 (3, 2, 12, 0, 1, '{T0}'), (4, 2, 13, 1, 1, '{T0}');
             INSERT INTO osm_way (osm_way_id, highway, oneway, name)
                 VALUES (100, 'residential', 0, '路');
             INSERT INTO osm_node (osm_node_id, lat, lon) VALUES
                 (1, 24.1400, 120.6857), (2, 24.1405, 120.6857), (3, 24.1410, 120.6857),
                 (4, 24.1415, 120.6857), (5, 24.1420, 120.6857);
             INSERT INTO osm_way_node (osm_way_id, ordinal, osm_node_id) VALUES
                 (100, 1, 1), (100, 2, 2), (100, 3, 3), (100, 4, 4), (100, 5, 5);
             INSERT INTO seg_match_way
                 (subroute_version_id, direction, ordinal, osm_way_id,
                  entry_node_id, exit_node_id, cum_start_m, cum_end_m) VALUES
                 (1, 0, 1, 100, 1, 5, 0.0, 400.0),
                 (2, 0, 1, 100, 1, 5, 0.0, 400.0);"
        ))
        .unwrap();
    }

    fn attach(xno: &str, direction: i64, after: Option<&str>) -> AttachCmd {
        AttachCmd {
            xno: xno.into(),
            direction,
            after_stop_code: after.map(str::to_string),
        }
    }

    fn cmd(name: &str, lat: f64, lon: f64, attachments: Vec<AttachCmd>) -> AddStopCmd {
        AddStopCmd {
            name: name.into(),
            lat,
            lon,
            station_id: None,
            attachments,
        }
    }

    #[test]
    fn plan_resolves_route_by_xno_and_reports_verdict() {
        let conn = open_in_memory().unwrap();
        plan_fixture(&conn);
        let p = plan_add_stop(
            &conn,
            &cmd("X", 24.1405, 120.6857, vec![attach("1", 0, None)]),
        )
        .unwrap();
        assert_eq!(p.name, "X");
        assert!(matches!(p.station, StationVerdict::CreateNew));
        assert_eq!(p.attachments.len(), 1);
        let a = &p.attachments[0];
        assert_eq!(a.xno, "1");
        assert_eq!(a.direction, 0);
        assert_eq!(a.route_id, 1, "xno resolved through route.ext_uid");
        assert_eq!(a.insert_after_stop_code, None, "append");
        assert_eq!(a.neighbor_before.as_deref(), Some("B"), "last stop");
        assert_eq!(a.neighbor_after, None);
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
    }

    #[test]
    fn plan_mid_insert_positions_and_neighbors() {
        let conn = open_in_memory().unwrap();
        plan_fixture(&conn);
        let p = plan_add_stop(
            &conn,
            &cmd("X", 24.1405, 120.6857, vec![attach("1", 0, Some("001"))]),
        )
        .unwrap();
        let a = &p.attachments[0];
        assert_eq!(a.insert_after_stop_code.as_deref(), Some("001"));
        assert_eq!(a.neighbor_before.as_deref(), Some("A"), "referenced stop");
        assert_eq!(a.neighbor_after.as_deref(), Some("B"), "its successor");
    }

    #[test]
    fn plan_rejects_unknown_xno_direction_aftercode() {
        let conn = open_in_memory().unwrap();
        plan_fixture(&conn);
        // unknown xno
        let err = plan_add_stop(
            &conn,
            &cmd("X", 24.1405, 120.6857, vec![attach("99", 0, None)]),
        )
        .unwrap_err();
        assert!(
            matches!(
                err.downcast_ref::<EditError>(),
                Some(EditError::UnknownRoute(_))
            ),
            "{err}"
        );
        // known route, absent direction
        let err = plan_add_stop(
            &conn,
            &cmd("X", 24.1405, 120.6857, vec![attach("1", 5, None)]),
        )
        .unwrap_err();
        match err.downcast_ref::<EditError>() {
            Some(EditError::UnknownDirection(xno, dir)) => {
                assert_eq!((xno.as_str(), *dir), ("1", 5))
            }
            other => panic!("expected UnknownDirection, got {other:?}"),
        }
        // direction ok, absent stop_code
        let err = plan_add_stop(
            &conn,
            &cmd("X", 24.1405, 120.6857, vec![attach("1", 0, Some("ZZZ"))]),
        )
        .unwrap_err();
        match err.downcast_ref::<EditError>() {
            Some(EditError::UnknownAfterStop(xno, dir, code)) => {
                assert_eq!((xno.as_str(), *dir, code.as_str()), ("1", 0, "ZZZ"))
            }
            other => panic!("expected UnknownAfterStop, got {other:?}"),
        }
    }

    #[test]
    fn plan_rejects_duplicate_attach() {
        let conn = open_in_memory().unwrap();
        plan_fixture(&conn);
        let err = plan_add_stop(
            &conn,
            &cmd(
                "X",
                24.1405,
                120.6857,
                vec![attach("1", 0, None), attach("1", 0, Some("001"))],
            ),
        )
        .unwrap_err();
        match err.downcast_ref::<EditError>() {
            Some(EditError::DuplicateAttach(xno, dir)) => {
                assert_eq!((xno.as_str(), *dir), ("1", 0))
            }
            other => panic!("expected DuplicateAttach, got {other:?}"),
        }
    }

    #[test]
    fn plan_rejects_unpublished_route() {
        let conn = open_in_memory().unwrap();
        plan_fixture(&conn);
        conn.execute(
            "UPDATE route SET current_route_version_id = NULL WHERE route_id = 2",
            [],
        )
        .unwrap();
        // the route row exists, but a NULL current pointer reads as UnknownRoute
        let err = plan_add_stop(
            &conn,
            &cmd("X", 24.1405, 120.6857, vec![attach("2", 0, None)]),
        )
        .unwrap_err();
        match err.downcast_ref::<EditError>() {
            Some(EditError::UnknownRoute(xno)) => assert_eq!(xno.as_str(), "2"),
            other => panic!("expected UnknownRoute, got {other:?}"),
        }
    }

    #[test]
    fn plan_rejects_501m_off_path_but_499m_passes() {
        let conn = open_in_memory().unwrap();
        plan_fixture(&conn);
        // metres of longitude per degree at the fixture's latitude
        let m_per_deg_lon = M_TO_DEG.recip() * 24.1410f64.to_radians().cos();
        let east = |m: f64| 120.6857 + m / m_per_deg_lon;
        // premise pin: the coords really sit ~499 / ~501 m off the path
        let path = matched_path(&conn, 1, 0).unwrap().unwrap();
        let (_, d499) = project_onto_path(&path, LatLng::new(24.1410, east(499.0))).unwrap();
        assert!((d499 - 499.0).abs() < 1.0, "499 m case got {d499}");
        let (_, d501) = project_onto_path(&path, LatLng::new(24.1410, east(501.0))).unwrap();
        assert!((d501 - 501.0).abs() < 1.0, "501 m case got {d501}");

        let p = plan_add_stop(
            &conn,
            &cmd("N499", 24.1410, east(499.0), vec![attach("1", 0, None)]),
        )
        .unwrap();
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);

        let err = plan_add_stop(
            &conn,
            &cmd("N501", 24.1410, east(501.0), vec![attach("1", 0, None)]),
        )
        .unwrap_err();
        match err.downcast_ref::<EditError>() {
            Some(EditError::OffPathStop {
                xno,
                direction,
                dist_m,
                max_m,
            }) => {
                assert_eq!((xno.as_str(), *direction), ("1", 0));
                assert!(*dist_m > OFF_PATH_MAX_M, "{dist_m}");
                assert_eq!(*max_m, OFF_PATH_MAX_M);
            }
            other => panic!("expected OffPathStop, got {other:?}"),
        }
    }

    #[test]
    fn plan_skips_offpath_check_when_no_matched_path() {
        let conn = open_in_memory().unwrap();
        plan_fixture(&conn);
        // route 2 direction 1 has route_stop_version rows but zero
        // seg_match_way runs: the loader returns None, no rejection
        let p = plan_add_stop(
            &conn,
            &cmd("X", 24.1405, 120.6857, vec![attach("2", 1, None)]),
        )
        .unwrap();
        assert_eq!(p.attachments[0].route_id, 2);
        assert_eq!(p.warnings.len(), 1, "{:?}", p.warnings);
        assert!(
            p.warnings[0].contains("no matched path"),
            "{:?}",
            p.warnings
        );
    }

    #[test]
    fn plan_warns_but_succeeds_on_same_name_nearby() {
        let conn = open_in_memory().unwrap();
        plan_fixture(&conn);
        // stop 'B' sits on the path; no station named 'B' exists, so the
        // verdict stays CreateNew but the same-name pole check fires
        let p = plan_add_stop(
            &conn,
            &cmd("B", 24.1405, 120.6857, vec![attach("1", 0, None)]),
        )
        .unwrap();
        assert!(matches!(p.station, StationVerdict::CreateNew));
        assert_eq!(p.warnings.len(), 1, "{:?}", p.warnings);
        assert!(p.warnings[0].contains("same-name"), "{:?}", p.warnings);
    }

    #[test]
    fn plan_counts_exact_for_two_attach() {
        let conn = open_in_memory().unwrap();
        plan_fixture(&conn);
        let p = plan_add_stop(
            &conn,
            &cmd(
                "X",
                24.1410,
                120.6857,
                vec![attach("1", 0, None), attach("2", 0, None)],
            ),
        )
        .unwrap();
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
        assert_eq!(p.stale_routes, vec![1, 2], "distinct route ids ascending");
        assert_eq!(
            p.row_counts,
            RowCounts {
                stop: 1,
                stop_version: 2, // one per attachment
                station: 1,      // CreateNew: group + station chains minted
                station_version: 1,
                station_group: 1,
                station_group_version: 1,
                route_version: 2,      // one per distinct route
                subroute_version: 2,   // one per distinct route
                route_stop_version: 6, // 4 current rows across both routes re-minted + 1 per attach
                stale_mark: 6,         // 2 seg_match keys + 2 path_stop_projection keys
                                       // + stop_bearing '*' + station_bearing '*'
                                       // ('*'-keyed rows INSERT OR REPLACE-collapse;
                                       // mark_position_stale only REPLACEs keys
                                       // mark_line_stale already wrote)
            }
        );
    }

    #[test]
    fn plan_leaves_db_unchanged() {
        let conn = open_in_memory().unwrap();
        plan_fixture(&conn);
        let before = dump(&conn);
        plan_add_stop(
            &conn,
            &cmd("X", 24.1405, 120.6857, vec![attach("1", 0, Some("001"))]),
        )
        .unwrap();
        assert_eq!(dump(&conn), before, "plan is read-only");
    }

    /// Every user table's full contents, name-then-rowid order — the
    /// byte-identical read-only check.
    fn dump(conn: &Connection) -> String {
        use rusqlite::types::ValueRef;
        let names: Vec<String> = conn
            .prepare(
                "SELECT name FROM sqlite_master
                 WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
                 ORDER BY name",
            )
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let mut out = String::new();
        for t in names {
            out.push_str(&format!("-- {t}\n"));
            let mut stmt = conn
                .prepare(&format!("SELECT * FROM \"{t}\" ORDER BY rowid"))
                .unwrap();
            let ncols = stmt.column_count();
            let mut rows = stmt.query([]).unwrap();
            while let Some(row) = rows.next().unwrap() {
                for i in 0..ncols {
                    match row.get_ref(i).unwrap() {
                        ValueRef::Null => out.push_str("· "),
                        ValueRef::Integer(n) => out.push_str(&format!("{n} ")),
                        ValueRef::Real(f) => out.push_str(&format!("{f:?} ")),
                        ValueRef::Text(s) => {
                            out.push_str(&format!("{:?} ", String::from_utf8_lossy(s)))
                        }
                        ValueRef::Blob(b) => out.push_str(&format!("blob:{b:?} ")),
                    }
                }
                out.push('\n');
            }
        }
        out
    }
}
