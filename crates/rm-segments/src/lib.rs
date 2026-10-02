//! rm-segments: decode direction polylines, project stops onto the path
//! (chainage), enforce monotonicity, cut per-consecutive-stop geometry
//! drafts.
//!
//! Methods: PROJECTED (snap within threshold), OUTLIER (snap beyond
//! `--outlier-m`, still projected and used), ENDPOINT (path degenerate to a
//! single point; chainage set to 0), NO_PATH (no polyline for the
//! direction; chainage set to 0). Drafts cut from the path are
//! geometry_method 'path'; straight-line fallbacks between stop coords are
//! 'straight'.

pub mod geometry;

use anyhow::Result;
use geometry::Projection;
use rm_core::LatLng;
use rusqlite::Connection;

#[derive(Debug, Default, PartialEq)]
pub struct Summary {
    pub batch_id: i64,
    pub directions: usize,
    pub projections: usize,
    pub no_path: usize,  // directions without a polyline
    pub endpoint: usize, // directions with a degenerate 1-point polyline
    pub outliers: usize, // stops
    pub clamped: usize,  // stops
    pub draft_rows: usize,
}

struct StopRow {
    sorv_id: i64,
    direction: i64,
    seq: i64,
    lat: f64,
    lon: f64,
}

/// Project every stop of every published route in `batch` and cut geometry
/// drafts. Idempotent: per-route rows are replaced within one transaction.
pub fn run(conn: &Connection, batch_id: i64, outlier_m: f64) -> Result<Summary> {
    let mut summary = Summary {
        batch_id,
        ..Default::default()
    };

    let routes: Vec<(i64, i64, String)> = {
        let mut stmt = conn.prepare(
            "SELECT route_id, current_route_version_id, substr(ext_uid, 5)
             FROM route WHERE current_route_version_id IS NOT NULL
             ORDER BY route_id",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect::<Result<_, _>>()?
    };

    let tx = conn.unchecked_transaction()?;
    for (_route_id, route_version_id, xno) in &routes {
        let sv_id: i64 = tx.query_row(
            "SELECT subroute_version_id FROM subroute_version WHERE route_version_id = ?1",
            [route_version_id],
            |r| r.get(0),
        )?;

        let mut stops: Vec<StopRow> = Vec::new();
        {
            let mut stmt = tx.prepare(
                "SELECT sorv.stop_of_route_version_id, sorv.direction, sorv.stop_sequence,
                        sv.latitude, sv.longitude
                 FROM stop_of_route_version sorv
                 JOIN stop_version sv ON sv.stop_version_id = sorv.stop_version_id
                 WHERE sorv.subroute_version_id = ?1
                 ORDER BY sorv.direction, sorv.stop_sequence",
            )?;
            let rows = stmt.query_map([sv_id], |r| {
                Ok(StopRow {
                    sorv_id: r.get(0)?,
                    direction: r.get(1)?,
                    seq: r.get(2)?,
                    lat: r.get(3)?,
                    lon: r.get(4)?,
                })
            })?;
            for row in rows {
                stops.push(row?);
            }
        }

        // polylines for this route, by direction
        let mut encoded: std::collections::HashMap<i64, String> = std::collections::HashMap::new();
        {
            let mut stmt = tx.prepare(
                "SELECT direction, encoded FROM src_route_point
                 WHERE batch_id = ?1 AND xno = ?2",
            )?;
            let rows = stmt.query_map(rusqlite::params![batch_id, xno], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (d, e) = row?;
                encoded.insert(d, e);
            }
        }

        // per-direction processing; wipe this route's previous output first
        tx.execute(
            "DELETE FROM path_stop_projection WHERE route_version_id = ?1",
            [route_version_id],
        )?;
        tx.execute(
            "DELETE FROM seg_geometry_draft WHERE subroute_version_id = ?1",
            [sv_id],
        )?;

        let mut i = 0;
        while i < stops.len() {
            let dir = stops[i].direction;
            let mut j = i;
            while j < stops.len() && stops[j].direction == dir {
                j += 1;
            }
            let group = &stops[i..j];
            i = j;

            summary.directions += 1;
            let enc = encoded.get(&dir).map(|s| s.as_str()).unwrap_or("");
            let path: Vec<LatLng> = if enc.is_empty() {
                Vec::new()
            } else {
                match rm_core::polyline::decode(enc) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!(xno, direction = dir, error = %e, "polyline decode failed; treating as no_path");
                        Vec::new()
                    }
                }
            };

            let projections: Vec<(Projection, &str, bool)> = if path.len() >= 2 {
                summary.projection_group(&path, group, outlier_m)
            } else {
                if enc.is_empty() {
                    summary.no_path += 1;
                } else {
                    summary.endpoint += 1;
                }
                let anchor = path.first().copied();
                group
                    .iter()
                    .map(|s| {
                        let p = LatLng {
                            lat: s.lat,
                            lon: s.lon,
                        };
                        let snap = anchor
                            .map(|a| rm_core::geo::haversine_m(a, p))
                            .unwrap_or(0.0);
                        (
                            Projection {
                                chainage_m: 0.0,
                                snap_dist_m: snap,
                                projected: anchor.unwrap_or(p),
                            },
                            if enc.is_empty() {
                                "NO_PATH"
                            } else {
                                "ENDPOINT"
                            },
                            false,
                        )
                    })
                    .collect()
            };

            for (s, (pr, method, clamped)) in group.iter().zip(&projections) {
                tx.execute(
                    "INSERT INTO path_stop_projection
                        (stop_of_route_version_id, route_version_id, direction, stop_sequence,
                         chainage_m, snap_dist_m, method, monotonic_clamped,
                         projected_lat, projected_lon)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    rusqlite::params![
                        s.sorv_id,
                        route_version_id,
                        dir,
                        s.seq,
                        pr.chainage_m,
                        pr.snap_dist_m,
                        method,
                        *clamped as i64,
                        pr.projected.lat,
                        pr.projected.lon
                    ],
                )?;
                summary.projections += 1;
                if *method == "OUTLIER" {
                    summary.outliers += 1;
                }
                if *clamped {
                    summary.clamped += 1;
                }
            }

            // drafts between consecutive stops
            let cum = if path.len() >= 2 {
                geometry::cumulative_m(&path)
            } else {
                Vec::new()
            };
            for k in 0..group.len().saturating_sub(1) {
                let (from, to) = (&group[k], &group[k + 1]);
                let (enc_poly, length, gm) = if path.len() >= 2 {
                    let a = projections[k].0.chainage_m;
                    let b = projections[k + 1].0.chainage_m;
                    let (sub, len) = geometry::cut_subpath(&path, &cum, a, b);
                    (rm_core::polyline::encode(&sub), len, "path")
                } else {
                    let a = LatLng {
                        lat: from.lat,
                        lon: from.lon,
                    };
                    let b = LatLng {
                        lat: to.lat,
                        lon: to.lon,
                    };
                    let len = rm_core::geo::haversine_m(a, b);
                    (rm_core::polyline::encode(&[a, b]), len, "straight")
                };
                let ordinal = (k + 1) as i64;
                tx.execute(
                    "INSERT INTO seg_geometry_draft
                        (subroute_version_id, direction, ordinal,
                         from_stop_of_route_version_id, to_stop_of_route_version_id,
                         encoded_polyline, length_m, geometry_method)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    rusqlite::params![
                        sv_id,
                        dir,
                        ordinal,
                        from.sorv_id,
                        to.sorv_id,
                        enc_poly,
                        length,
                        gm
                    ],
                )?;
                summary.draft_rows += 1;
            }
        }
    }
    tx.commit()?;
    Ok(summary)
}

impl Summary {
    /// Project `group` onto `path`, apply outlier classification and
    /// monotonic clamping.
    fn projection_group<'a>(
        &mut self,
        path: &[LatLng],
        group: &'a [StopRow],
        outlier_m: f64,
    ) -> Vec<(Projection, &'a str, bool)> {
        let cum = geometry::cumulative_m(path);
        let mut raw: Vec<Projection> = group
            .iter()
            .map(|s| {
                geometry::project_point(
                    path,
                    &cum,
                    &LatLng {
                        lat: s.lat,
                        lon: s.lon,
                    },
                )
            })
            .collect();
        let (clamped, flags) =
            geometry::clamp_monotonic(&raw.iter().map(|p| p.chainage_m).collect::<Vec<_>>());
        for (p, c) in raw.iter_mut().zip(clamped) {
            p.chainage_m = c;
        }
        raw.into_iter()
            .zip(flags)
            .map(|(p, f)| {
                let method = if p.snap_dist_m > outlier_m {
                    "OUTLIER"
                } else {
                    "PROJECTED"
                };
                (p, method, f)
            })
            .collect()
    }
}
