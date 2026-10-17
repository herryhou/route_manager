//! Add-stop edit primitive: the command types, the qualify-or-create
//! station search, and the read-only plan that previews the write.

use std::collections::BTreeSet;

use anyhow::Result;
use rm_core::{geo, LatLng};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::json;

use super::matched_path::{bearing_at, matched_path, project_onto_path, OFF_PATH_MAX_M};
use super::{load_stop_edit, EditError, EditOutcome, StopEditRow};
use crate::stale::{mark_line_stale, mark_position_stale, seg_match_key};

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
    /// The route's current `route_version_id` at plan time — apply
    /// re-validates the route pointer against it inside the tx.
    pub route_version_id: i64,
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
                    route_version_id: r.route_version_id,
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

// ---------- apply ----------

/// What one applied add-stop did: the audit row id, the minted pole, its
/// pole versions in attachment order, the resolved station, whether the
/// station (and with it the group) was newly minted, and the distinct
/// routes whose derived data went stale (ascending).
#[derive(Debug)]
pub struct AddStopOutcome {
    pub edit_id: i64,
    pub stop_id: i64,
    pub stop_version_ids: Vec<i64>,
    pub station_id: i64,
    pub station_created: bool,
    pub stale_routes: Vec<i64>,
}

/// Per-route state captured before the pointer move: the plan's old
/// version plus the revision/updated_at the undo restores.
struct RouteState {
    route_id: i64,
    old_rv: i64,
    old_revision: i64,
    old_updated_at: String,
}

/// Application service: plan (read-only) → apply (one tx).
pub fn add_stop(
    conn: &Connection,
    cmd: AddStopCmd,
    editor: &str,
    note: &str,
) -> Result<AddStopOutcome> {
    let plan = plan_add_stop(conn, &cmd)?;
    apply_add_stop(conn, &plan, editor, note)
}

/// Apply a plan in ONE transaction: mint the station chain (`CreateNew`
/// only), the stop, one pole version per attachment, then per route a new
/// `route_version` + `subroute_version` with the whole current
/// `route_stop_version` set re-minted (the new pole spliced at its planned
/// position), move the pointer, write one tagged `edit_log` row, and mark
/// the line + position datasets stale. Re-validates inside the tx: each
/// route's `current_route_version_id` must still be the version the plan
/// resolved, else `NotCurrent(old_rv)` — a plan applied after the route
/// moved on is refused before any write.
pub fn apply_add_stop(
    conn: &Connection,
    plan: &AddStopPlan,
    editor: &str,
    note: &str,
) -> Result<AddStopOutcome> {
    let tx = conn.unchecked_transaction()?;
    let now = crate::now_rfc3339();

    // distinct routes in attachment first-appearance order (Vec-driven —
    // the mint order below is pinned to this sequence)
    let mut routes: Vec<(i64, i64)> = Vec::new(); // (route_id, planned old rv)
    for a in &plan.attachments {
        if !routes.iter().any(|(rid, _)| *rid == a.route_id) {
            routes.push((a.route_id, a.route_version_id));
        }
    }

    // in-tx re-validation BEFORE any write: the pointer must still sit on
    // the planned version; the pre-move revision/updated_at are the undo
    // payload's restore values
    let mut state: Vec<RouteState> = Vec::with_capacity(routes.len());
    for (rid, old_rv) in &routes {
        let (current, revision, updated_at): (Option<i64>, i64, String) = tx
            .query_row(
                "SELECT current_route_version_id, revision, updated_at
                 FROM route WHERE route_id = ?1",
                [rid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?
            .ok_or(EditError::NotCurrent(*old_rv))?;
        if current != Some(*old_rv) {
            return Err(EditError::NotCurrent(*old_rv).into());
        }
        state.push(RouteState {
            route_id: *rid,
            old_rv: *old_rv,
            old_revision: revision,
            old_updated_at: updated_at,
        });
    }

    // 1. station chain (CreateNew only) — ids minted in the pinned order
    // before the inserts: the group's ext_uid embeds the new station's
    // ext_uid, so the station id must exist when the group row is written
    let station_created = matches!(plan.station, StationVerdict::CreateNew);
    let station_id = match &plan.station {
        StationVerdict::UseExisting {
            station_id: sid, ..
        }
        | StationVerdict::Pinned {
            station_id: sid, ..
        } => *sid,
        StationVerdict::CreateNew => {
            let group_id = crate::mint(&tx, "station_group")?;
            let group_version_id = crate::mint(&tx, "station_group_version")?;
            let new_station_id = crate::mint(&tx, "station")?;
            let station_version_id = crate::mint(&tx, "station_version")?;
            let station_ext_uid = format!("manual:{new_station_id}");
            // rm_identity::group_ext_uid's single-member form, inlined
            // (rm-db cannot depend on rm-identity)
            let group_ext_uid = format!(
                "tcg:grp:{}",
                rm_core::blake3_128_hex(station_ext_uid.as_bytes())
            );
            tx.execute(
                "INSERT INTO station_group (station_group_id, ext_uid, created_at)
                 VALUES (?1, ?2, ?3)",
                params![group_id, group_ext_uid, now],
            )?;
            tx.execute(
                "INSERT INTO station_group_version
                    (station_group_version_id, station_group_id, station_group_name,
                     valid_from, created_at, source)
                 VALUES (?1, ?2, ?3, ?4, ?4, 'manual')",
                params![group_version_id, group_id, plan.name, now],
            )?;
            tx.execute(
                "INSERT INTO station (station_id, ext_uid, created_at) VALUES (?1, ?2, ?3)",
                params![new_station_id, station_ext_uid, now],
            )?;
            tx.execute(
                "INSERT INTO station_version
                    (station_version_id, station_id, station_group_id, station_name,
                     latitude, longitude, valid_from, created_at, source)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, 'manual')",
                params![
                    station_version_id,
                    new_station_id,
                    group_id,
                    plan.name,
                    plan.lat,
                    plan.lon,
                    now
                ],
            )?;
            new_station_id
        }
    };

    // 2. the stop identity
    let stop_id = crate::mint(&tx, "stop")?;
    tx.execute(
        "INSERT INTO stop (stop_id, ext_uid, created_at) VALUES (?1, ?2, ?3)",
        params![stop_id, format!("manual:{stop_id}"), now],
    )?;

    // 3. one pole version per attachment, in attachment order
    let mut stop_version_ids = Vec::with_capacity(plan.attachments.len());
    let mut attach_svi: Vec<(i64, i64, i64)> = Vec::new(); // (route_id, direction, svi)
    for a in &plan.attachments {
        let svi = crate::mint(&tx, "stop_version")?;
        tx.execute(
            "INSERT INTO stop_version
                (stop_version_id, stop_id, station_id, stop_code, stop_name, direction,
                 latitude, longitude, valid_from, created_at, source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, 'manual')",
            params![
                svi,
                stop_id,
                station_id,
                format!("M{stop_id}"),
                plan.name,
                a.direction,
                plan.lat,
                plan.lon,
                now
            ],
        )?;
        stop_version_ids.push(svi);
        attach_svi.push((a.route_id, a.direction, svi));
    }

    // 4. per route (attachment first-appearance order): new route_version +
    // subroute_version + the whole current route_stop_version set re-minted
    // (directions ascending), the new pole spliced at its planned position
    let mut applied_routes: Vec<serde_json::Value> = Vec::new();
    let mut new_rv_of: Vec<(i64, i64)> = Vec::new(); // (route_id, new_rv)
    for st in &state {
        let max_no: i64 = tx.query_row(
            "SELECT COALESCE(MAX(version_no), 0) FROM route_version WHERE route_id = ?1",
            [st.route_id],
            |r| r.get(0),
        )?;
        let new_rv = crate::mint(&tx, "route_version")?;
        tx.execute(
            "INSERT INTO route_version
                (route_version_id, route_id, version_no, valid_from, valid_to, status, created_at, published_at)
             VALUES (?1, ?2, ?3, ?4, NULL, 'PUBLISHED', ?4, ?4)",
            params![new_rv, st.route_id, max_no + 1, now],
        )?;
        let (subroute_id, subroute_name): (i64, String) = tx.query_row(
            "SELECT subroute_id, subroute_name FROM subroute_version
             WHERE route_version_id = ?1 ORDER BY subroute_version_id LIMIT 1",
            [st.old_rv],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let new_sv_id = crate::mint(&tx, "subroute_version")?;
        tx.execute(
            "INSERT INTO subroute_version
                (subroute_version_id, route_version_id, subroute_id, subroute_name, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![new_sv_id, new_rv, subroute_id, subroute_name, now],
        )?;

        // the old version's rows in travel order (directions ascending)
        let old_rows: Vec<(i64, i64, i64, Option<i64>, String)> = {
            let mut stmt = tx.prepare(
                "SELECT rsv.direction, rsv.stop_sequence, rsv.stop_version_id,
                        rsv.source_order_no, s.stop_code
                 FROM route_stop_version rsv
                 JOIN stop_version s ON s.stop_version_id = rsv.stop_version_id
                 JOIN subroute_version sv ON sv.subroute_version_id = rsv.subroute_version_id
                 WHERE sv.route_version_id = ?1
                 ORDER BY rsv.direction, rsv.stop_sequence",
            )?;
            let rows = stmt
                .query_map([st.old_rv], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, Option<i64>>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };

        let mut new_rsv_ids = Vec::new();
        let mut i = 0;
        while i < old_rows.len() {
            let dir = old_rows[i].0;
            let mut j = i;
            while j < old_rows.len() && old_rows[j].0 == dir {
                j += 1;
            }
            let seq: Vec<(i64, Option<i64>)> = old_rows[i..j].iter().map(|r| (r.2, r.3)).collect();
            let codes: Vec<&str> = old_rows[i..j].iter().map(|r| r.4.as_str()).collect();
            let attach = plan
                .attachments
                .iter()
                .find(|a| a.route_id == st.route_id && a.direction == dir);
            let new_svi = if attach.is_some() {
                attach_svi
                    .iter()
                    .find(|(rid, d, _)| *rid == st.route_id && *d == dir)
                    .map(|(_, _, svi)| *svi)
            } else {
                None
            };
            // splice position: after the referenced stop code; None = append
            let insert_at: Option<usize> = match attach.map(|a| &a.insert_after_stop_code) {
                None => None,
                Some(None) => Some(seq.len()),
                Some(Some(code)) => match codes.iter().position(|c| *c == code) {
                    Some(p) => Some(p + 1),
                    // the sequence moved on under the same version
                    // (a pole rename repoints rsv rows) — refuse
                    None => return Err(EditError::NotCurrent(st.old_rv).into()),
                },
            };
            let mut rows_out: Vec<(i64, Option<i64>)> = Vec::with_capacity(seq.len() + 1);
            for (k, (svi, order_no)) in seq.iter().enumerate() {
                if let (Some(svi), Some(at)) = (new_svi, insert_at) {
                    if at == k {
                        rows_out.push((svi, None));
                    }
                }
                rows_out.push((*svi, *order_no));
            }
            if let (Some(svi), Some(at)) = (new_svi, insert_at) {
                if at >= seq.len() {
                    rows_out.push((svi, None));
                }
            }
            for (n, (svi, order_no)) in rows_out.iter().enumerate() {
                let rsv_id = crate::mint(&tx, "route_stop_version")?;
                tx.execute(
                    "INSERT INTO route_stop_version
                        (route_stop_version_id, subroute_version_id, stop_version_id,
                         direction, stop_sequence, source_order_no, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![rsv_id, new_sv_id, svi, dir, (n + 1) as i64, order_no, now],
                )?;
                new_rsv_ids.push(rsv_id);
            }
            i = j;
        }

        applied_routes.push(json!({
            "route_id": st.route_id,
            "old_rv": st.old_rv,
            "new_rv": new_rv,
            "old_revision": st.old_revision,
            "old_updated_at": st.old_updated_at,
            "new_sv_id": new_sv_id,
            "new_rsv_ids": new_rsv_ids,
        }));
        new_rv_of.push((st.route_id, new_rv));
    }

    // 5. the pointer move — the only UPDATE a re-versioning write makes
    for (rid, new_rv) in &new_rv_of {
        tx.execute(
            "UPDATE route SET current_route_version_id = ?1, revision = revision + 1, updated_at = ?2
             WHERE route_id = ?3",
            params![new_rv, now, rid],
        )?;
    }

    // 6. one tagged audit row (group_created == station_created: the group
    // is minted only in the CreateNew verdict)
    let changes = json!({
        "kind": "stop_add",
        "stop_id": stop_id,
        "station_id": station_id,
        "station_created": station_created,
        "group_created": station_created,
        "stop_version_ids": stop_version_ids,
        "routes": applied_routes,
    });
    tx.execute(
        "INSERT INTO edit_log (ts, editor, entity, entity_id, action, changes, note, version_before, version_after)
         VALUES (?1, ?2, 'stop_add', ?3, 'add', ?4, ?5, NULL, NULL)",
        params![
            now,
            editor,
            stop_id,
            changes.to_string(),
            if note.is_empty() { None } else { Some(note) }
        ],
    )?;
    let edit_id = tx.last_insert_rowid();

    // 7. stale marks: the line datasets per attached (route, direction),
    // then the position datasets per new pole version
    for a in &plan.attachments {
        mark_line_stale(&tx, a.route_id, a.direction, edit_id, &now)?;
    }
    for &svi in &stop_version_ids {
        mark_position_stale(&tx, svi, edit_id, &now)?;
    }
    tx.commit()?;

    let mut stale_routes: Vec<i64> = plan.attachments.iter().map(|a| a.route_id).collect();
    stale_routes.sort_unstable();
    stale_routes.dedup();
    Ok(AddStopOutcome {
        edit_id,
        stop_id,
        stop_version_ids,
        station_id,
        station_created,
        stale_routes,
    })
}

// ---------- undo ----------

/// One route's inverse, parsed from the add's logged `routes` array.
#[derive(Debug)]
struct RouteAddUndo {
    route_id: i64,
    old_rv: i64,
    new_rv: i64,
    old_revision: i64,
    old_updated_at: String,
    new_sv_id: i64,
    new_rsv_ids: Vec<i64>,
}

/// Write-side op for the add-stop undo, emitted by `plan_stop_add_undo`
/// from the append-only log; apply re-validates currency inside its tx.
#[derive(Debug)]
struct StopAddUndoOp {
    edit_id: i64,
    stop_id: i64,
    station_id: i64,
    station_created: bool,
    group_created: bool,
    stop_version_ids: Vec<i64>,
    routes: Vec<RouteAddUndo>,
}

/// A JSON value that is exactly an array of integers.
fn i64_vec(v: Option<&serde_json::Value>) -> Option<Vec<i64>> {
    let arr = v?.as_array()?;
    let mut out = Vec::with_capacity(arr.len());
    for x in arr {
        out.push(x.as_i64()?);
    }
    Some(out)
}

/// Pure guard + parse: only a non-undo `stop_add` row whose payload is
/// fully formed is invertible — every missing or malformed field is a
/// refusal (`NotUndoable`), never a panic.
fn plan_stop_add_undo(edit: &StopEditRow) -> Result<StopAddUndoOp, EditError> {
    let refused = || EditError::NotUndoable(edit.edit_id);
    if edit.entity != "stop_add" || edit.action == "undo" {
        return Err(refused());
    }
    let c = &edit.changes;
    if c.get("kind").and_then(|k| k.as_str()) != Some("stop_add") {
        return Err(refused());
    }
    let (Some(stop_id), Some(station_id)) = (
        c.get("stop_id").and_then(|v| v.as_i64()),
        c.get("station_id").and_then(|v| v.as_i64()),
    ) else {
        return Err(refused());
    };
    let (Some(station_created), Some(group_created)) = (
        c.get("station_created").and_then(|v| v.as_bool()),
        c.get("group_created").and_then(|v| v.as_bool()),
    ) else {
        return Err(refused());
    };
    let Some(stop_version_ids) = i64_vec(c.get("stop_version_ids")) else {
        return Err(refused());
    };
    let Some(routes_json) = c.get("routes").and_then(|v| v.as_array()) else {
        return Err(refused());
    };
    let mut routes = Vec::with_capacity(routes_json.len());
    for r in routes_json {
        let (Some(route_id), Some(old_rv), Some(new_rv)) = (
            r.get("route_id").and_then(|v| v.as_i64()),
            r.get("old_rv").and_then(|v| v.as_i64()),
            r.get("new_rv").and_then(|v| v.as_i64()),
        ) else {
            return Err(refused());
        };
        let (Some(old_revision), Some(new_sv_id)) = (
            r.get("old_revision").and_then(|v| v.as_i64()),
            r.get("new_sv_id").and_then(|v| v.as_i64()),
        ) else {
            return Err(refused());
        };
        let Some(old_updated_at) = r
            .get("old_updated_at")
            .and_then(|v| v.as_str())
            .map(str::to_string)
        else {
            return Err(refused());
        };
        let Some(new_rsv_ids) = i64_vec(r.get("new_rsv_ids")) else {
            return Err(refused());
        };
        routes.push(RouteAddUndo {
            route_id,
            old_rv,
            new_rv,
            old_revision,
            old_updated_at,
            new_sv_id,
            new_rsv_ids,
        });
    }
    Ok(StopAddUndoOp {
        edit_id: edit.edit_id,
        stop_id,
        station_id,
        station_created,
        group_created,
        stop_version_ids,
        routes,
    })
}

/// Undo a stop_add in ONE transaction: delete the job's own rows in the
/// pinned child-before-parent order, restore the route pointers, delete
/// the add's own stale marks, and write the undo's audit row
/// (`action='undo'`, `changes.kind='stop_add_undo'`). Mints nothing and
/// marks nothing stale — the derived data reads the restored rows again.
///
/// Guards run inside the tx BEFORE any write: for every route in the
/// payload, `current_route_version_id` must still be the add's `new_rv`
/// and the version's whole `route_stop_version` id set must equal the
/// payload's `new_rsv_ids` — anything else (`NotUndoable`) means the route
/// moved on past this add and it is no longer the newest write.
pub fn undo_stop_add(
    conn: &Connection,
    edit_id: i64,
    editor: &str,
    note: &str,
) -> Result<EditOutcome> {
    let row = load_stop_edit(conn, edit_id)?.ok_or(EditError::NotUndoable(edit_id))?;
    let op = plan_stop_add_undo(&row)?;
    apply_stop_add_undo(conn, &op, editor, note)
}

fn apply_stop_add_undo(
    conn: &Connection,
    op: &StopAddUndoOp,
    editor: &str,
    note: &str,
) -> Result<EditOutcome> {
    let tx = conn.unchecked_transaction()?;

    // guards BEFORE any write: the add must still be the newest write on
    // every route it touched
    for r in &op.routes {
        let current: Option<i64> = tx
            .query_row(
                "SELECT current_route_version_id FROM route WHERE route_id = ?1",
                [r.route_id],
                |row| row.get(0),
            )
            .optional()?;
        if current != Some(r.new_rv) {
            return Err(EditError::NotUndoable(op.edit_id).into());
        }
        let mut ids: Vec<i64> = {
            let mut stmt = tx.prepare(
                "SELECT route_stop_version_id FROM route_stop_version
                 WHERE subroute_version_id IN (
                     SELECT subroute_version_id FROM subroute_version
                     WHERE route_version_id = ?1)",
            )?;
            let rows = stmt
                .query_map([r.new_rv], |row| row.get(0))?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        ids.sort_unstable();
        let mut want = r.new_rsv_ids.clone();
        want.sort_unstable();
        if ids != want {
            return Err(EditError::NotUndoable(op.edit_id).into());
        }
    }

    // the payload carries no group id; the minted station's version rows
    // name the group (read here, before any delete)
    let group_ids: Vec<i64> = if op.group_created {
        let ids: Vec<i64> = {
            let mut stmt =
                tx.prepare("SELECT station_group_id FROM station_version WHERE station_id = ?1")?;
            let rows = stmt
                .query_map([op.station_id], |row| row.get(0))?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        if ids.is_empty() {
            return Err(EditError::NotUndoable(op.edit_id).into());
        }
        ids
    } else {
        Vec::new()
    };

    // the pinned delete order: children before parents. The pointer
    // restore sits between the subroute_version and route_version deletes
    // because the FK route.current_route_version_id → route_version
    // forbids deleting the pointed-at row.
    for r in &op.routes {
        for &rsv in &r.new_rsv_ids {
            tx.execute(
                "DELETE FROM route_stop_version WHERE route_stop_version_id = ?1",
                [rsv],
            )?;
        }
        tx.execute(
            "DELETE FROM subroute_version WHERE subroute_version_id = ?1",
            [r.new_sv_id],
        )?;
        tx.execute(
            "UPDATE route SET current_route_version_id = ?1, revision = ?2, updated_at = ?3
             WHERE route_id = ?4",
            params![r.old_rv, r.old_revision, r.old_updated_at, r.route_id],
        )?;
        tx.execute(
            "DELETE FROM route_version WHERE route_version_id = ?1",
            [r.new_rv],
        )?;
    }
    for &svi in &op.stop_version_ids {
        tx.execute("DELETE FROM stop_version WHERE stop_version_id = ?1", [svi])?;
    }
    tx.execute("DELETE FROM stop WHERE stop_id = ?1", [op.stop_id])?;
    if op.station_created {
        tx.execute(
            "DELETE FROM station_version WHERE station_id = ?1",
            [op.station_id],
        )?;
        tx.execute("DELETE FROM station WHERE station_id = ?1", [op.station_id])?;
    }
    if op.group_created {
        for &gid in &group_ids {
            tx.execute(
                "DELETE FROM station_group_version WHERE station_group_id = ?1",
                [gid],
            )?;
            tx.execute(
                "DELETE FROM station_group WHERE station_group_id = ?1",
                [gid],
            )?;
        }
    }
    tx.execute(
        "DELETE FROM stale_mark WHERE cause_edit_id = ?1",
        [op.edit_id],
    )?;

    let now = crate::now_rfc3339();
    let changes = json!({ "kind": "stop_add_undo" });
    tx.execute(
        "INSERT INTO edit_log (ts, editor, entity, entity_id, action, changes, note, undo_of, version_before, version_after)
         VALUES (?1, ?2, 'stop_add', ?3, 'undo', ?4, ?5, ?6, NULL, NULL)",
        params![
            now,
            editor,
            op.stop_id,
            changes.to_string(),
            if note.is_empty() { None } else { Some(note) },
            op.edit_id
        ],
    )?;
    let undo_edit_id = tx.last_insert_rowid();
    tx.commit()?;
    Ok(EditOutcome {
        edit_id: undo_edit_id,
        new_version_id: 0, // nothing minted or restored — the add's rows are gone
        stale_routes: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_in_memory;
    use crate::{undo_edit, UndoOutcome};
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
    /// byte-identical read-only check. `skip` drops tables the comparison
    /// means to exclude.
    fn dump_with(conn: &Connection, skip: &[&str]) -> String {
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
            if skip.contains(&t.as_str()) {
                continue;
            }
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

    /// The read-only check: nothing excluded.
    fn dump(conn: &Connection) -> String {
        dump_with(conn, &[])
    }

    // ---------- apply_add_stop ----------

    /// plan_fixture + id_sequence seeds covering every table apply mints
    /// (existing max + 1), and source_order_no on the current rsv rows so
    /// the carry behavior is observable.
    fn apply_fixture(conn: &Connection) {
        plan_fixture(conn);
        conn.execute_batch(
            "INSERT INTO id_sequence (table_name, next_value) VALUES
                 ('station_group', 2), ('station_group_version', 2),
                 ('station', 2), ('station_version', 21),
                 ('stop', 4), ('stop_version', 14),
                 ('route_version', 3), ('subroute_version', 3),
                 ('route_stop_version', 5);
             UPDATE route_stop_version SET source_order_no = route_stop_version_id * 10
                 WHERE route_stop_version_id <= 4;",
        )
        .unwrap();
    }

    /// The golden two-attach command: mid-insert on route 1 dir 0 (A → new
    /// → B), append on route 2 dir 0 (C → new), sibling route 2 dir 1
    /// re-minted untouched.
    fn golden_cmd() -> AddStopCmd {
        cmd(
            "X",
            24.1410,
            120.6857,
            vec![attach("1", 0, Some("001")), attach("2", 0, None)],
        )
    }

    fn golden_plan(conn: &Connection) -> AddStopPlan {
        plan_add_stop(conn, &golden_cmd()).unwrap()
    }

    #[test]
    fn apply_mints_exact_key_sets_per_table() {
        let conn = open_in_memory().unwrap();
        apply_fixture(&conn);
        let out = apply_add_stop(&conn, &golden_plan(&conn), "herry", "job").unwrap();
        assert_eq!(out.stop_id, 4);
        assert_eq!(out.stop_version_ids, vec![14, 15], "attachment order");
        assert_eq!(out.station_id, 2);
        assert!(out.station_created);

        let ids = |t: &str, col: &str| -> Vec<i64> {
            let mut stmt = conn
                .prepare(&format!("SELECT {col} FROM {t} ORDER BY {col}"))
                .unwrap();
            stmt.query_map([], |r| r.get(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        // the FULL key set per table — pre-existing rows keep their ids
        assert_eq!(ids("stop", "stop_id"), vec![1, 2, 3, 4]);
        assert_eq!(
            ids("stop_version", "stop_version_id"),
            vec![10, 11, 12, 13, 14, 15]
        );
        assert_eq!(ids("station", "station_id"), vec![1, 2]);
        assert_eq!(ids("station_version", "station_version_id"), vec![20, 21]);
        assert_eq!(ids("station_group", "station_group_id"), vec![1, 2]);
        assert_eq!(
            ids("station_group_version", "station_group_version_id"),
            vec![1, 2]
        );
        assert_eq!(ids("route_version", "route_version_id"), vec![1, 2, 3, 4]);
        assert_eq!(
            ids("subroute_version", "subroute_version_id"),
            vec![1, 2, 3, 4]
        );
        assert_eq!(
            ids("route_stop_version", "route_stop_version_id"),
            (1..=10).collect::<Vec<_>>()
        );

        // minted stop: manual ext_uid; one pole version per attachment with
        // the M-code and the attachment's direction
        let stop_ext: String = conn
            .query_row("SELECT ext_uid FROM stop WHERE stop_id = 4", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(stop_ext, "manual:4");
        let poles: Vec<(i64, i64, String, String, i64)> = conn
            .prepare(
                "SELECT stop_version_id, station_id, stop_code, stop_name, direction
                 FROM stop_version WHERE stop_version_id >= 14 ORDER BY stop_version_id",
            )
            .unwrap()
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            poles,
            vec![
                (14, 2, "M4".into(), "X".into(), 0),
                (15, 2, "M4".into(), "X".into(), 0),
            ]
        );

        // new station chain: manual ext_uids; the group ext_uid is the
        // inlined single-member rm_identity::group_ext_uid formula
        let (st_ext, grp_ext): (String, String) = conn
            .query_row(
                "SELECT s.ext_uid, g.ext_uid FROM station s, station_group g
                 WHERE s.station_id = 2 AND g.station_group_id = 2",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(st_ext, "manual:2");
        assert_eq!(
            grp_ext,
            format!("tcg:grp:{}", rm_core::blake3_128_hex(b"manual:2"))
        );
        let (gname, gsource): (String, String) = conn
            .query_row(
                "SELECT station_group_name, source FROM station_group_version
                 WHERE station_group_version_id = 2",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((gname.as_str(), gsource.as_str()), ("X", "manual"));
        let (stname, stsource): (String, String) = conn
            .query_row(
                "SELECT station_name, source FROM station_version WHERE station_version_id = 21",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((stname.as_str(), stsource.as_str()), ("X", "manual"));

        // route re-versioning: version_no = old max + 1, PUBLISHED, open end
        let rvs: Vec<(i64, i64, i64, Option<String>, String)> = conn
            .prepare(
                "SELECT route_version_id, route_id, version_no, valid_to, status
                 FROM route_version WHERE route_version_id >= 3 ORDER BY route_version_id",
            )
            .unwrap()
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            rvs,
            vec![
                (3, 1, 2, None, "PUBLISHED".into()),
                (4, 2, 2, None, "PUBLISHED".into()),
            ]
        );

        // subroute_version: subroute_id resolved per route, name carried
        let svs: Vec<(i64, i64, i64, String)> = conn
            .prepare(
                "SELECT subroute_version_id, route_version_id, subroute_id, subroute_name
                 FROM subroute_version WHERE subroute_version_id >= 3
                 ORDER BY subroute_version_id",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(svs, vec![(3, 3, 1, "1".into()), (4, 4, 2, "2".into())]);

        // route_stop_version: sequences renumbered, the inserted row NULL on
        // source_order_no, re-minted rows carry it, sibling direction keeps
        // its pole version (only rsv rows are new)
        let rsvs: Vec<(i64, i64, i64, i64, i64, Option<i64>)> = conn
            .prepare(
                "SELECT route_stop_version_id, subroute_version_id, stop_version_id,
                        direction, stop_sequence, source_order_no
                 FROM route_stop_version WHERE route_stop_version_id >= 5
                 ORDER BY route_stop_version_id",
            )
            .unwrap()
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            rsvs,
            vec![
                (5, 3, 10, 0, 1, Some(10)),  // route 1 dir 0: A kept
                (6, 3, 14, 0, 2, None),      // inserted after '001'
                (7, 3, 11, 0, 3, Some(20)),  // B shifted +1, same pole version
                (8, 4, 12, 0, 1, Some(30)),  // route 2 dir 0: C kept
                (9, 4, 15, 0, 2, None),      // appended
                (10, 4, 13, 1, 1, Some(40)), // sibling dir re-mint, same pole
            ]
        );
    }

    #[test]
    fn apply_moves_pointer_and_bumps_revision() {
        let conn = open_in_memory().unwrap();
        apply_fixture(&conn);
        let out = apply_add_stop(&conn, &golden_plan(&conn), "herry", "").unwrap();
        let ts: String = conn
            .query_row(
                "SELECT ts FROM edit_log WHERE edit_id = ?1",
                [out.edit_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_ne!(ts, T0);
        let rows: Vec<(i64, i64, i64, String)> = conn
            .prepare("SELECT route_id, current_route_version_id, revision, updated_at FROM route ORDER BY route_id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                (1, 3, 2, ts.clone()), // revision 1 -> 2, pointer 1 -> 3
                (2, 4, 2, ts),         // revision 1 -> 2, pointer 2 -> 4
            ]
        );
    }

    #[test]
    fn apply_writes_one_tagged_edit_log_row() {
        let conn = open_in_memory().unwrap();
        apply_fixture(&conn);
        let out = apply_add_stop(&conn, &golden_plan(&conn), "herry", "job").unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM edit_log", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        let (entity, entity_id, action, changes, note): (
            String,
            i64,
            String,
            String,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT entity, entity_id, action, changes, note FROM edit_log WHERE edit_id = ?1",
                [out.edit_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(
            (entity.as_str(), entity_id, action.as_str()),
            ("stop_add", 4, "add")
        );
        assert_eq!(note.as_deref(), Some("job"));
        let v: serde_json::Value = serde_json::from_str(&changes).unwrap();
        let mut top: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        top.sort_unstable();
        assert_eq!(
            top,
            vec![
                "group_created",
                "kind",
                "routes",
                "station_created",
                "station_id",
                "stop_id",
                "stop_version_ids",
            ]
        );
        assert_eq!(v["kind"], "stop_add");
        assert_eq!(v["stop_id"], 4);
        assert_eq!(v["station_id"], 2);
        assert_eq!(v["station_created"], true);
        assert_eq!(v["group_created"], true);
        assert_eq!(v["stop_version_ids"], serde_json::json!([14, 15]));
        let routes = v["routes"].as_array().unwrap();
        assert_eq!(routes.len(), 2);
        let mut route_keys: Vec<&str> = routes[0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        route_keys.sort_unstable();
        assert_eq!(
            route_keys,
            vec![
                "new_rsv_ids",
                "new_rv",
                "new_sv_id",
                "old_revision",
                "old_rv",
                "old_updated_at",
                "route_id",
            ]
        );
        assert_eq!(
            v["routes"],
            serde_json::json!([
                {"route_id": 1, "old_rv": 1, "new_rv": 3, "old_revision": 1,
                 "old_updated_at": T0, "new_sv_id": 3, "new_rsv_ids": [5, 6, 7]},
                {"route_id": 2, "old_rv": 2, "new_rv": 4, "old_revision": 1,
                 "old_updated_at": T0, "new_sv_id": 4, "new_rsv_ids": [8, 9, 10]},
            ])
        );
    }

    #[test]
    fn apply_marks_line_and_position_stale() {
        let conn = open_in_memory().unwrap();
        apply_fixture(&conn);
        let out = apply_add_stop(&conn, &golden_plan(&conn), "herry", "").unwrap();
        let rows: Vec<(String, String, i64)> = conn
            .prepare(
                "SELECT table_name, key, cause_edit_id FROM stale_mark ORDER BY table_name, key",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                ("path_stop_projection".into(), "1".into(), out.edit_id),
                ("path_stop_projection".into(), "2".into(), out.edit_id),
                ("seg_match".into(), "1:0".into(), out.edit_id),
                ("seg_match".into(), "2:0".into(), out.edit_id),
                ("station_bearing".into(), "*".into(), out.edit_id),
                ("stop_bearing".into(), "*".into(), out.edit_id),
            ]
        );
        assert_eq!(out.stale_routes, vec![1, 2]);
    }

    #[test]
    fn apply_rejects_when_route_moved_between_plan_and_apply() {
        let conn = open_in_memory().unwrap();
        apply_fixture(&conn);
        let plan = plan_add_stop(
            &conn,
            &cmd("X", 24.1410, 120.6857, vec![attach("1", 0, Some("001"))]),
        )
        .unwrap();
        // publish-style pointer move between plan and apply
        conn.execute(
            "INSERT INTO route_version
                (route_version_id, route_id, version_no, valid_from, valid_to, status, created_at, published_at)
             VALUES (3, 1, 2, '2026-01-02T00:00:00Z', NULL, 'PUBLISHED',
                     '2026-01-02T00:00:00Z', '2026-01-02T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE route SET current_route_version_id = 3 WHERE route_id = 1",
            [],
        )
        .unwrap();
        let before = dump(&conn);
        let err = apply_add_stop(&conn, &plan, "herry", "").unwrap_err();
        match err.downcast_ref::<EditError>() {
            Some(EditError::NotCurrent(old)) => assert_eq!(*old, 1, "carries the planned old rv"),
            other => panic!("expected NotCurrent, got {other:?}"),
        }
        assert_eq!(dump(&conn), before, "rejected apply writes nothing");
    }

    #[test]
    fn apply_rejection_leaves_db_byte_identical() {
        let conn = open_in_memory().unwrap();
        apply_fixture(&conn);
        let before = dump(&conn);
        let err = add_stop(
            &conn,
            cmd("X", 24.1410, 120.6857, vec![attach("1", 0, Some("ZZZ"))]),
            "herry",
            "job",
        )
        .unwrap_err();
        assert!(
            matches!(
                err.downcast_ref::<EditError>(),
                Some(EditError::UnknownAfterStop(_, _, _))
            ),
            "{err}"
        );
        assert_eq!(dump(&conn), before, "zero partial write");
    }

    // ---------- undo_stop_add ----------

    /// Round-trip exclusions: the audit rows (undo writes its own by
    /// design, checked separately), the stale marks (the job's own are
    /// deleted by undo, covered by their own test), and the id allocator
    /// (apply mints ids and ids are never reused, so the sequence
    /// legitimately moves — undo restores data, not ids).
    const ROUNDTRIP_SKIP: &[&str] = &["edit_log", "stale_mark", "id_sequence"];

    #[test]
    fn undo_roundtrips_add_to_byte_identical() {
        let conn = open_in_memory().unwrap();
        apply_fixture(&conn);
        let b0 = dump_with(&conn, ROUNDTRIP_SKIP);
        let out = add_stop(&conn, golden_cmd(), "herry", "job").unwrap();
        undo_stop_add(&conn, out.edit_id, "herry", "revert").unwrap();
        let b2 = dump_with(&conn, ROUNDTRIP_SKIP);
        assert_eq!(b2, b0, "apply then undo is byte-identical (B2==B0)");
        // the undo's own audit row (excluded from the dump above)
        let (entity, action, undo_of, changes): (String, String, Option<i64>, String) = conn
            .query_row(
                "SELECT entity, action, undo_of, changes FROM edit_log WHERE undo_of = ?1",
                [out.edit_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!((entity.as_str(), action.as_str()), ("stop_add", "undo"));
        assert_eq!(undo_of, Some(out.edit_id));
        let c: serde_json::Value = serde_json::from_str(&changes).unwrap();
        assert_eq!(c["kind"], "stop_add_undo");
    }

    #[test]
    fn undo_deletes_own_stale_marks_only() {
        let conn = open_in_memory().unwrap();
        apply_fixture(&conn);
        // a foreign edit + its mark on a seg_match key this job never
        // writes ('9:9' vs the job's '1:0'/'2:0')
        conn.execute(
            "INSERT INTO edit_log (edit_id, ts, editor, entity, entity_id, action, changes)
             VALUES (9001, '2026-01-02T00:00:00Z', 'other', 'stop', 1, 'update', '{}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO stale_mark (table_name, key, cause_edit_id, marked_at)
             VALUES ('seg_match', '9:9', 9001, '2026-01-02T00:00:00Z')",
            [],
        )
        .unwrap();
        let out = add_stop(&conn, golden_cmd(), "herry", "job").unwrap();
        let job_marks: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM stale_mark WHERE cause_edit_id = ?1",
                [out.edit_id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(job_marks > 0, "premise: the job marked datasets stale");
        undo_stop_add(&conn, out.edit_id, "herry", "revert").unwrap();
        let rows: Vec<(String, String, i64)> = conn
            .prepare(
                "SELECT table_name, key, cause_edit_id FROM stale_mark ORDER BY table_name, key",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![("seg_match".into(), "9:9".into(), 9001)],
            "the job's marks gone, the foreign one intact"
        );
    }

    #[test]
    fn undo_refuses_after_route_moved_on() {
        let conn = open_in_memory().unwrap();
        apply_fixture(&conn);
        let first = add_stop(&conn, golden_cmd(), "herry", "job").unwrap();
        // a second add on route 1 moves its pointer past the first job's
        // new_rv — the first job is no longer the newest write
        add_stop(
            &conn,
            cmd("Y", 24.1405, 120.6857, vec![attach("1", 0, Some("001"))]),
            "herry",
            "more",
        )
        .unwrap();
        let before = dump_with(&conn, ROUNDTRIP_SKIP);
        let err = undo_stop_add(&conn, first.edit_id, "herry", "revert").unwrap_err();
        match err.downcast_ref::<EditError>() {
            Some(EditError::NotUndoable(id)) => assert_eq!(*id, first.edit_id),
            other => panic!("expected NotUndoable, got {other:?}"),
        }
        assert_eq!(
            dump_with(&conn, ROUNDTRIP_SKIP),
            before,
            "refused undo writes nothing"
        );
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM edit_log WHERE action = 'undo'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "no undo audit row either");
    }

    #[test]
    fn undo_of_undo_refused() {
        let conn = open_in_memory().unwrap();
        apply_fixture(&conn);
        let out = add_stop(&conn, golden_cmd(), "herry", "job").unwrap();
        let u = undo_stop_add(&conn, out.edit_id, "herry", "revert").unwrap();
        let err = undo_stop_add(&conn, u.edit_id, "herry", "again").unwrap_err();
        assert!(
            matches!(
                err.downcast_ref::<EditError>(),
                Some(EditError::NotUndoable(_))
            ),
            "{err}"
        );
    }

    #[test]
    fn undo_edit_dispatches_stop_add() {
        let conn = open_in_memory().unwrap();
        apply_fixture(&conn);
        let out = add_stop(&conn, golden_cmd(), "herry", "job").unwrap();
        let o = undo_edit(&conn, out.edit_id, "herry", "revert").unwrap();
        let UndoOutcome::StopAdd { edit_id } = o else {
            panic!("expected StopAdd, got {o:?}");
        };
        assert!(
            edit_id > out.edit_id,
            "edit_id is the undo's own audit row id"
        );
    }

    /// Replace every second-precision RFC3339 timestamp with `<ts>`: two
    /// runs never share the wall clock, so the determinism check
    /// normalizes before comparing.
    fn normalize_ts(s: &str) -> String {
        fn is_ts(w: &[char]) -> bool {
            w.len() == 20
                && w[4] == '-'
                && w[7] == '-'
                && w[10] == 'T'
                && w[13] == ':'
                && w[16] == ':'
                && w[19] == 'Z'
                && w.iter()
                    .enumerate()
                    .all(|(i, c)| matches!(i, 4 | 7 | 10 | 13 | 16 | 19) || c.is_ascii_digit())
        }
        let chars: Vec<char> = s.chars().collect();
        let mut out = String::with_capacity(s.len());
        let mut i = 0;
        while i < chars.len() {
            if chars.len() - i >= 20 && is_ts(&chars[i..i + 20]) {
                out.push_str("<ts>");
                i += 20;
            } else {
                out.push(chars[i]);
                i += 1;
            }
        }
        out
    }

    #[test]
    fn determinism_same_add_two_fresh_dbs() {
        let run = || -> String {
            let conn = open_in_memory().unwrap();
            apply_fixture(&conn);
            let out = add_stop(&conn, golden_cmd(), "herry", "job").unwrap();
            undo_stop_add(&conn, out.edit_id, "herry", "revert").unwrap();
            normalize_ts(&dump(&conn))
        };
        assert_eq!(
            run(),
            run(),
            "same cmd on two fresh dbs: identical end state incl. audit rows"
        );
    }
}
