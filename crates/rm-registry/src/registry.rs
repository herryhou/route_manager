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
                let (lo, hi) = (a.chainage_m, b.chainage_m);
                // ways covering (lo, hi) in travel order, with overlap lengths
                let mut ways: Vec<(i64, f64)> = Vec::new();
                for r in &runs {
                    let s = r.cum_start_m.max(lo);
                    let e = r.cum_end_m.min(hi);
                    if e - s > 1e-6 {
                        ways.push((r.way_id, e - s));
                    }
                }
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

/// 1-based ordinal of a stop within its direction's stop list.
fn pair_index(stops: &[StopPt], rsv_id: i64) -> i64 {
    stops
        .iter()
        .position(|s| s.rsv_id == rsv_id)
        .map(|i| (i + 1) as i64)
        .unwrap_or(0)
}
