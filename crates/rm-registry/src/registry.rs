//! Registry build: cut way runs at stop chainages, hash directed segments,
//! record traversals. Idempotent: seg_* tables are rebuilt each run.

use rusqlite::{Connection, OptionalExtension};

use crate::hash::segment_id;

#[derive(Debug, Default, Clone)]
pub struct Summary {
    pub batch_id: i64,
    pub directions: usize,    // route+direction pairs with way runs
    pub traversals: usize,    // stop pairs registered
    pub segments: usize,      // distinct segment ids written this run
    pub skipped_pairs: usize, // stop pairs with no covering way run
}

struct StopPt {
    rsv_id: i64,
    chainage_m: f64,
    station_id: i64,
    station_ext_uid: String,
}

struct Run {
    way_id: i64,
    cum_start_m: f64,
    cum_end_m: f64,
}

pub fn run(conn: &Connection, batch_id: i64) -> anyhow::Result<Summary> {
    let mut summary = Summary {
        batch_id,
        ..Default::default()
    };

    // every direction with stop projections (subroute_version is 1:1 with
    // route_version in this pipeline); directions without way runs contribute
    // only skipped pairs
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
    summary.directions = dirs.len();

    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM seg_traversal", [])?;
    tx.execute("DELETE FROM seg_segment_way", [])?;
    tx.execute("DELETE FROM seg_segment", [])?;

    {
        let mut ins_seg = tx.prepare(
            "INSERT OR IGNORE INTO seg_segment
                (segment_id, start_station_id, end_station_id,
                 encoded_polyline, length_m, way_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        let mut ins_way = tx.prepare(
            "INSERT OR IGNORE INTO seg_segment_way
                (segment_id, ordinal, osm_way_id, length_m)
             VALUES (?1, ?2, ?3, ?4)",
        )?;
        let mut ins_trav = tx.prepare(
            "INSERT OR REPLACE INTO seg_traversal
                (route_version_id, subroute_version_id, direction,
                 from_route_stop_version_id, to_route_stop_version_id, segment_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;

        for (sv_id, rv_id, dir) in dirs {
            let stops: Vec<StopPt> = {
                let mut stmt = tx.prepare(
                    "SELECT p.route_stop_version_id, p.chainage_m,
                            st.station_id, st.ext_uid
                     FROM path_stop_projection p
                     JOIN route_stop_version rsv
                        ON rsv.route_stop_version_id = p.route_stop_version_id
                     JOIN stop_version s ON s.stop_version_id = rsv.stop_version_id
                     JOIN station st ON st.station_id = s.station_id
                     WHERE p.route_version_id = ?1 AND p.direction = ?2
                     ORDER BY p.stop_sequence",
                )?;
                let rows = stmt.query_map([rv_id, dir], |r| {
                    Ok(StopPt {
                        rsv_id: r.get(0)?,
                        chainage_m: r.get(1)?,
                        station_id: r.get(2)?,
                        station_ext_uid: r.get(3)?,
                    })
                })?;
                rows.collect::<Result<_, _>>()?
            };
            let runs: Vec<Run> = {
                let mut stmt = tx.prepare(
                    "SELECT osm_way_id, cum_start_m, cum_end_m FROM seg_match_way
                     WHERE subroute_version_id = ?1 AND direction = ?2
                     ORDER BY ordinal",
                )?;
                let rows = stmt.query_map([sv_id, dir], |r| {
                    Ok(Run {
                        way_id: r.get(0)?,
                        cum_start_m: r.get(1)?,
                        cum_end_m: r.get(2)?,
                    })
                })?;
                rows.collect::<Result<_, _>>()?
            };

            for pair in stops.windows(2) {
                let (a, b) = (&pair[0], &pair[1]);
                let (lo, hi) = cut_window(a.chainage_m, b.chainage_m, &runs);
                let ways = slice_ways(&runs, lo, hi);
                if ways.is_empty() {
                    summary.skipped_pairs += 1;
                    continue;
                }

                let way_ids: Vec<i64> = ways.iter().map(|(w, _)| *w).collect();
                let seg = segment_id(&a.station_ext_uid, &b.station_ext_uid, &way_ids);
                let ordinal = pair_index(&stops, a.rsv_id);

                // geometry draft for this stop pair, if present
                let (enc, len): (String, f64) = tx
                    .query_row(
                        "SELECT encoded_polyline, length_m FROM seg_geometry_draft
                         WHERE subroute_version_id = ?1 AND direction = ?2 AND ordinal = ?3",
                        rusqlite::params![sv_id, dir, ordinal],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?
                    // no draft (e.g. NO_PATH): fall back to summed way overlap
                    .unwrap_or_else(|| (String::new(), ways.iter().map(|(_, l)| l).sum()));

                ins_seg.execute(rusqlite::params![
                    seg,
                    a.station_id,
                    b.station_id,
                    enc,
                    len,
                    way_ids.len() as i64
                ])?;
                for (i, (w, l)) in ways.iter().enumerate() {
                    ins_way.execute(rusqlite::params![seg, (i + 1) as i64, w, l])?;
                }
                ins_trav.execute(rusqlite::params![
                    rv_id, sv_id, dir, a.rsv_id, b.rsv_id, seg
                ])?;
                summary.traversals += 1;
            }
        }
    }

    let n: i64 = tx.query_row("SELECT COUNT(*) FROM seg_segment", [], |r| r.get(0))?;
    summary.segments = n as usize;
    tx.commit()?;
    Ok(summary)
}

/// Max distance from a stop projection to the nearest way-run junction for
/// the segment cut to snap onto that junction.
const SNAP_EPS_M: f64 = 20.0;

/// Boundary-way overlap below this is matching noise around the cut, not
/// travel: drop it so junction-adjacent stops hash the same way list.
/// Covers residual cases where two junctions sit closer together than the
/// projection jitter (e.g. a 5 m slip lane ahead of the main carriage).
const SLIVER_M: f64 = 6.0;

/// Segment cut window for a stop pair: each cut snaps down to the latest
/// run junction at or below it (within SNAP_EPS_M), never forward, so
/// routes covering the same ways slice at shared nodes instead of jittery
/// per-route projections, and consecutive segments tile at the same
/// junction. A window that snaps degenerate (lo >= hi, dense stops) keeps
/// the raw projections.
pub(crate) fn cut_window(lo_raw: f64, hi_raw: f64, runs: &[Run]) -> (f64, f64) {
    let snap_down = |c: f64| -> f64 {
        let mut best: Option<f64> = None;
        for r in runs {
            for b in [r.cum_start_m, r.cum_end_m] {
                if b <= c && c - b < SNAP_EPS_M && best.is_none_or(|cur| b > cur) {
                    best = Some(b);
                }
            }
        }
        best.unwrap_or(c)
    };
    let (lo, hi) = (snap_down(lo_raw), snap_down(hi_raw));
    if lo < hi {
        (lo, hi)
    } else {
        (lo_raw, hi_raw)
    }
}

/// Ways covering (lo, hi) in travel order with their overlap lengths:
/// zero-overlap runs excluded, consecutive repeats of a way merged (path
/// re-entry), boundary slivers below SLIVER_M dropped. Never returns
/// empty for a non-empty overlap set (sliver drop yields to it).
pub(crate) fn slice_ways(runs: &[Run], lo: f64, hi: f64) -> Vec<(i64, f64)> {
    let mut ways: Vec<(i64, f64)> = Vec::new();
    for r in runs {
        let s = r.cum_start_m.max(lo);
        let e = r.cum_end_m.min(hi);
        if e - s > 1e-6 {
            match ways.last_mut() {
                Some((w, l)) if *w == r.way_id => *l += e - s,
                _ => ways.push((r.way_id, e - s)),
            }
        }
    }
    // boundary slivers are noise unless dropping them would empty the list
    if ways.len() > 1 {
        let head_sliver = ways.first().unwrap().1 < SLIVER_M;
        let tail_sliver = ways.last().unwrap().1 < SLIVER_M;
        if ways.len() - head_sliver as usize - tail_sliver as usize > 0 {
            if head_sliver {
                ways.remove(0);
            }
            if tail_sliver {
                ways.pop();
            }
        }
    }
    ways
}

/// 1-based ordinal of a stop within its direction's stop list.
fn pair_index(stops: &[StopPt], rsv_id: i64) -> i64 {
    stops
        .iter()
        .position(|s| s.rsv_id == rsv_id)
        .map(|i| (i + 1) as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn way(way_id: i64, s: f64, e: f64) -> Run {
        Run {
            way_id,
            cum_start_m: s,
            cum_end_m: e,
        }
    }

    #[test]
    fn cut_window_snaps_down_to_latest_junction() {
        let runs = [way(1, 0.0, 100.0), way(2, 100.0, 290.0)];
        // both cuts snap down to the latest junction at-or-below them
        assert_eq!(cut_window(105.0, 295.0, &runs), (100.0, 290.0));
    }

    #[test]
    fn cut_window_beyond_eps_keeps_raw() {
        let runs = [way(1, 100.0, 200.0)];
        assert_eq!(cut_window(150.0, 180.0, &runs), (150.0, 180.0));
    }

    #[test]
    fn cut_window_never_snaps_forward() {
        // a junction just AHEAD of the cut is next segment's territory
        let runs = [way(1, 100.0, 200.0), way(2, 200.0, 400.0)];
        assert_eq!(cut_window(105.0, 390.0, &runs), (100.0, 390.0));
    }

    #[test]
    fn cut_window_degenerate_snap_keeps_raw() {
        // both stops snap down onto the same junction -> keep raw projections
        let runs = [way(1, 0.0, 100.0), way(2, 100.0, 200.0)];
        assert_eq!(cut_window(100.2, 100.8, &runs), (100.2, 100.8));
    }

    #[test]
    fn slice_ways_collapses_consecutive_repeats() {
        // 3648e5e8 shape: 1149579011 entered, left, re-entered
        let runs = [
            way(609374024, 0.0, 10.0),
            way(711269513, 10.0, 30.0),
            way(238995112, 30.0, 40.0),
            way(1149579011, 40.0, 180.0),
            way(1149579011, 180.0, 340.0),
            way(1149579016, 340.0, 350.0),
        ];
        let ways = slice_ways(&runs, 0.0, 350.0);
        assert_eq!(
            ways,
            vec![
                (609374024, 10.0),
                (711269513, 20.0),
                (238995112, 10.0),
                (1149579011, 300.0),
                (1149579016, 10.0),
            ]
        );
    }

    #[test]
    fn slice_ways_drops_boundary_sliver() {
        // 924d71d4 head shape: 2.5 m of 386971585 past the cut is noise
        let runs = [way(386971585, 0.0, 2.5), way(609374024, 2.5, 40.0)];
        assert_eq!(slice_ways(&runs, 0.0, 40.0), vec![(609374024, 37.5)]);
    }

    #[test]
    fn slice_ways_drops_tail_sliver() {
        // 015c78f8 tail shape: 0.4 m of the bridge way past the end stop
        let runs = [way(1149579015, 0.0, 164.0), way(1149579012, 164.0, 181.0)];
        assert_eq!(slice_ways(&runs, 0.0, 164.4), vec![(1149579015, 164.0)]);
    }

    #[test]
    fn slice_ways_keeps_short_middle_way() {
        // a genuinely crossed 2 m link mid-segment is travel, not noise
        let runs = [way(1, 0.0, 10.0), way(2, 10.0, 12.0), way(3, 12.0, 40.0)];
        assert_eq!(
            slice_ways(&runs, 0.0, 40.0),
            vec![(1, 10.0), (2, 2.0), (3, 28.0)]
        );
    }

    #[test]
    fn slice_ways_sliver_only_keeps_all() {
        // dropping both boundary slivers would empty the list — keep them
        let runs = [way(1, 0.0, 2.0), way(2, 2.0, 4.0)];
        assert_eq!(slice_ways(&runs, 0.0, 4.0), vec![(1, 2.0), (2, 2.0)]);
    }
}
