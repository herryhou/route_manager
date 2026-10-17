//! Add-stop edit primitive: the command types and the qualify-or-create
//! station search that decides whether a proposed pole joins an existing
//! station or warrants a new one.

use anyhow::Result;
use rm_core::{geo, LatLng};
use rusqlite::{Connection, OptionalExtension};

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
}
