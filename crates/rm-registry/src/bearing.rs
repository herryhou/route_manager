//! Stop/station bearings: departure heading of the bus at each stop,
//! taken from the matched OSM way run when one covers the stop, else the
//! direction polyline tangent. Stations get the axial mean of their
//! member stops (one axis ± 180° reverse — a pole serves both ways)
//! plus the largest deviation from that axis (consistency signal).
//!
//! Idempotent: stop_bearing / station_bearing are rebuilt each run.

use std::collections::HashMap;

use rm_core::geo::{angular_diff_deg, axial_mean_deg, bearing_deg};
use rm_core::{geo, LatLng};
use rusqlite::Connection;

#[derive(Debug, Default, Clone)]
pub struct Summary {
    pub stops_osm: usize,      // bearings from matched way runs
    pub stops_polyline: usize, // bearings from the direction polyline
    pub stops_skipped: usize,  // stops with neither source available
    pub stations: usize,
}

struct StopRow {
    stop_version_id: i64,
    lat: f64,
    lon: f64,
    chainage_m: f64,
}

struct Run {
    way_id: i64,
    entry_node_id: i64,
    exit_node_id: i64,
    cum_start_m: f64,
    cum_end_m: f64,
}

pub fn run(conn: &Connection, batch_id: i64) -> anyhow::Result<Summary> {
    let mut summary = Summary::default();

    let dirs: Vec<(i64, i64, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT p.route_version_id, sv.subroute_version_id, p.direction
             FROM path_stop_projection p
             JOIN subroute_version sv ON sv.route_version_id = p.route_version_id
             ORDER BY p.route_version_id, p.direction",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect::<Result<_, _>>()?
    };

    // osm nodes used by any run, loaded once: node id -> (lat, lon)
    let nodes: HashMap<i64, LatLng> = {
        let mut stmt = conn.prepare(
            "SELECT osm_node_id, lat, lon FROM osm_node JOIN osm_way_node USING (osm_node_id)",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, LatLng::new(r.get(1)?, r.get(2)?)))
        })?;
        rows.collect::<Result<_, _>>()?
    };

    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM stop_bearing", [])?;
    tx.execute("DELETE FROM station_bearing", [])?;

    {
        // OR REPLACE: a cycled route can visit the same stop twice in one
        // direction — one bearing row per stop version, last visit wins
        let mut ins = tx.prepare(
            "INSERT OR REPLACE INTO stop_bearing
                (stop_version_id, bearing_deg, source, computed_at)
             VALUES (?1, ?2, ?3, ?4)",
        )?;

        for (rv_id, sv_id, dir) in dirs {
            let stops: Vec<StopRow> = {
                let mut stmt = tx.prepare(
                    "SELECT s.stop_version_id, s.latitude, s.longitude, p.chainage_m
                     FROM path_stop_projection p
                     JOIN stop_of_route_version sorv
                        ON sorv.stop_of_route_version_id = p.stop_of_route_version_id
                     JOIN stop_version s ON s.stop_version_id = sorv.stop_version_id
                     WHERE p.route_version_id = ?1 AND p.direction = ?2
                     ORDER BY p.stop_sequence",
                )?;
                let rows = stmt.query_map([rv_id, dir], |r| {
                    Ok(StopRow {
                        stop_version_id: r.get(0)?,
                        lat: r.get(1)?,
                        lon: r.get(2)?,
                        chainage_m: r.get(3)?,
                    })
                })?;
                rows.collect::<Result<_, _>>()?
            };
            let runs: Vec<Run> = {
                let mut stmt = tx.prepare(
                    "SELECT osm_way_id, entry_node_id, exit_node_id, cum_start_m, cum_end_m
                     FROM seg_match_way
                     WHERE subroute_version_id = ?1 AND direction = ?2
                     ORDER BY ordinal",
                )?;
                let rows = stmt.query_map([sv_id, dir], |r| {
                    Ok(Run {
                        way_id: r.get(0)?,
                        entry_node_id: r.get(1)?,
                        exit_node_id: r.get(2)?,
                        cum_start_m: r.get(3)?,
                        cum_end_m: r.get(4)?,
                    })
                })?;
                rows.collect::<Result<_, _>>()?
            };
            // direction polyline for the fallback source
            let xno: Option<String> = tx
                .query_row(
                    "SELECT substr(r.ext_uid, 5)
                     FROM route_version rv
                     JOIN route r ON r.route_id = rv.route_id
                     WHERE rv.route_version_id = ?1",
                    [rv_id],
                    |r| r.get(0),
                )
                .ok();
            let path: Vec<LatLng> = match &xno {
                Some(x) => tx
                    .query_row(
                        "SELECT encoded FROM src_route_point
                         WHERE batch_id = ?1 AND xno = ?2 AND direction = ?3",
                        rusqlite::params![batch_id, x, dir],
                        |r| r.get::<_, String>(0),
                    )
                    .ok()
                    .and_then(|e| rm_core::polyline::decode(&e).ok())
                    .unwrap_or_default(),
                None => Vec::new(),
            };

            for s in &stops {
                let pole = LatLng::new(s.lat, s.lon);
                if let Some(deg) = osm_bearing(&tx, &nodes, &runs, s.chainage_m, pole)? {
                    ins.execute(rusqlite::params![
                        s.stop_version_id,
                        deg,
                        "OSM",
                        rm_db::now_rfc3339()
                    ])?;
                    summary.stops_osm += 1;
                } else if let Some(deg) = polyline_bearing(&path, s.chainage_m) {
                    ins.execute(rusqlite::params![
                        s.stop_version_id,
                        deg,
                        "POLYLINE",
                        rm_db::now_rfc3339()
                    ])?;
                    summary.stops_polyline += 1;
                } else {
                    summary.stops_skipped += 1;
                }
            }
        }
    }

    // station aggregates over member stop bearings: the axial mean (a pole
    // legitimately serves both directions of travel, so headings 180°
    // apart fold onto one axis) and the largest deviation from that axis.
    // route `direction` is route-relative (each route's own 去程/返程),
    // so it cannot group headings — the axis does.
    {
        let mut stmt = tx.prepare(
            "SELECT s.station_id, b.bearing_deg
             FROM stop_bearing b
             JOIN stop_version s ON s.stop_version_id = b.stop_version_id
             ORDER BY s.station_id",
        )?;
        let rows: Vec<(i64, f64)> = {
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<Result<_, _>>()?
        };
        let mut ins = tx.prepare(
            "INSERT INTO station_bearing
                (station_id, bearing_deg, member_count, max_spread_deg, computed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        // group consecutive rows by station
        let mut stations: Vec<(i64, Vec<f64>)> = Vec::new();
        for (station_id, bearing) in rows {
            if !matches!(stations.last(), Some((st, _)) if *st == station_id) {
                stations.push((station_id, Vec::new()));
            }
            stations.last_mut().unwrap().1.push(bearing);
        }
        for (station_id, bearings) in stations {
            let mean = axial_mean_deg(&bearings).unwrap_or(0.0);
            let mut spread = 0.0f64;
            for &b in &bearings {
                let dev = angular_diff_deg(b, mean).min(angular_diff_deg(b, mean + 180.0));
                spread = spread.max(dev);
            }
            ins.execute(rusqlite::params![
                station_id,
                mean,
                bearings.len() as i64,
                spread,
                rm_db::now_rfc3339()
            ])?;
            summary.stations += 1;
        }
    }

    tx.commit()?;
    Ok(summary)
}

/// Bearing along the way run covering (or nearest to) `chainage_m`:
/// the sub-path entry -> exit in travel order, heading of the segment
/// nearest the pole. None when no run resolves to a 2+ node sub-path.
fn osm_bearing(
    conn: &Connection,
    nodes: &HashMap<i64, LatLng>,
    runs: &[Run],
    chainage_m: f64,
    pole: LatLng,
) -> anyhow::Result<Option<f64>> {
    let run = pick_run(runs, chainage_m)?;
    let Some(run) = run else {
        return Ok(None);
    };
    // way nodes in ordinal order, sliced entry -> exit
    let ord: Vec<(i64, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT ordinal, osm_node_id FROM osm_way_node
             WHERE osm_way_id = ?1 ORDER BY ordinal",
        )?;
        let rows = stmt.query_map([run.way_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })?;
        rows.collect::<Result<_, _>>()?
    };
    let entry = ord.iter().position(|(_, n)| *n == run.entry_node_id);
    let exit = ord.iter().position(|(_, n)| *n == run.exit_node_id);
    let (Some(i), Some(j)) = (entry, exit) else {
        return Ok(None);
    };
    let sub: Vec<LatLng> = if i <= j {
        ord[i..=j]
            .iter()
            .filter_map(|(_, n)| nodes.get(n).copied())
            .collect()
    } else {
        ord[j..=i]
            .iter()
            .rev()
            .filter_map(|(_, n)| nodes.get(n).copied())
            .collect()
    };
    if sub.len() < 2 {
        return Ok(None);
    }
    let mut best = f64::INFINITY;
    let mut best_deg = None;
    for w in sub.windows(2) {
        let (_, dist) = geo::point_to_segment_m(pole, w[0], w[1]);
        if dist < best {
            best = dist;
            best_deg = Some(bearing_deg(w[0], w[1]));
        }
    }
    Ok(best_deg)
}

/// Run covering the chainage; between runs (or beyond the ends), the
/// nearest run — the last stop of a direction then reports its arrival
/// heading on the final way.
fn pick_run(runs: &[Run], chainage_m: f64) -> anyhow::Result<Option<&Run>> {
    let mut best: Option<(&Run, f64)> = None;
    for r in runs {
        let d = if chainage_m < r.cum_start_m {
            r.cum_start_m - chainage_m
        } else if chainage_m > r.cum_end_m {
            chainage_m - r.cum_end_m
        } else {
            0.0
        };
        if best.map(|(_, bd)| d < bd).unwrap_or(true) {
            best = Some((r, d));
        }
    }
    Ok(best.map(|(r, _)| r))
}

/// Forward tangent of `path` at `chainage_m`; at/beyond the end, the
/// final leg (arrival heading for the last stop).
fn polyline_bearing(path: &[LatLng], chainage_m: f64) -> Option<f64> {
    if path.len() < 2 {
        return None;
    }
    let mut cum = 0.0;
    for w in path.windows(2) {
        let leg = geo::haversine_m(w[0], w[1]);
        if chainage_m <= cum + leg || w[1] == *path.last().unwrap() {
            return Some(bearing_deg(w[0], w[1]));
        }
        cum += leg;
    }
    None
}
