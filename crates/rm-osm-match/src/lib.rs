//! Viterbi map matching of direction polylines onto OSM ways.

pub mod graph;
pub mod models;
pub mod viterbi;

use std::sync::Arc;

use rayon::prelude::*;
use rusqlite::Connection;

use graph::RoadGraph;
use models::{BasicTransition, NewsonKrummEmission};
use viterbi::MatchOutput;

#[derive(Debug, Default, Clone)]
pub struct Summary {
    pub batch_id: i64,
    pub directions: usize, // published route+direction pairs considered
    pub matched: usize,    // directions producing >= 1 way run
    pub unmatched: usize,  // directions with no runs (missing/degenerate polyline, no fit)
    pub failed: usize,     // directions that errored (non-fatal)
    pub total_runs: usize,
    pub gap_repairs: usize,
}

/// One unit of matching work: a published direction with its polyline.
struct Job {
    route_version_id: i64,
    subroute_version_id: i64,
    direction: i64,
    xno: String,
    stops: i64,
    encoded: String,
}

/// Enumerate the published directions of `batch` and resolve each job's
/// effective polyline (override ?? src — phase 2: the run matches the line
/// consumers see, not the raw capture). The `src_route_point` JOIN stays in
/// the query: it decides which directions exist as jobs at all.
fn collect_jobs(conn: &Connection, batch_id: i64) -> anyhow::Result<Vec<Job>> {
    let mut jobs: Vec<Job> = {
        let mut stmt = conn.prepare(
            "SELECT rv.route_version_id, sv.subroute_version_id, srp.direction,
                    substr(r.ext_uid, 5),
                    (SELECT COUNT(*) FROM route_stop_version o
                      WHERE o.subroute_version_id = sv.subroute_version_id
                        AND o.direction = srp.direction)
             FROM route r
             JOIN route_version rv ON rv.route_id = r.route_id
                  AND rv.route_version_id = r.current_route_version_id
             JOIN subroute_version sv ON sv.route_version_id = rv.route_version_id
             JOIN src_route_point srp
                  ON srp.batch_id = ?1 AND srp.xno = substr(r.ext_uid, 5)
             ORDER BY rv.route_version_id, srp.direction",
        )?;
        let rows = stmt.query_map([batch_id], |r| {
            Ok(Job {
                route_version_id: r.get(0)?,
                subroute_version_id: r.get(1)?,
                direction: r.get(2)?,
                xno: r.get(3)?,
                stops: r.get(4)?,
                encoded: String::new(),
            })
        })?;
        rows.collect::<Result<_, _>>()?
    };
    for job in &mut jobs {
        job.encoded = rm_db::effective_encoded(conn, batch_id, &job.xno, job.direction)?;
    }
    Ok(jobs)
}

/// Match every published direction of `batch` onto the OSM road graph and
/// write seg_match_way + seg_match_stat. Idempotent: both tables are
/// rebuilt from scratch each run. Per-direction failures are non-fatal.
pub fn run(conn: &Connection, batch_id: i64) -> anyhow::Result<Summary> {
    let graph = Arc::new(RoadGraph::from_conn(conn)?);

    let jobs = collect_jobs(conn, batch_id)?;

    struct Outcome {
        job: Job,
        result: anyhow::Result<MatchOutput>,
    }
    let outcomes: Vec<Outcome> = jobs
        .into_par_iter()
        .map_init(|| viterbi::Ctx::new(&graph), |ctx, job| {
            let result = (|| {
                let pts = rm_core::polyline::decode(&job.encoded)?;
                Ok(viterbi::match_path(
                    ctx,
                    &pts,
                    &NewsonKrummEmission::default(),
                    &BasicTransition::default(),
                ))
            })();
            Outcome { job, result }
        })
        .collect();

    let mut summary = Summary {
        batch_id,
        directions: outcomes.len(),
        ..Default::default()
    };

    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM seg_match_way", [])?;
    tx.execute("DELETE FROM seg_match_stat", [])?;
    {
        let mut ins_way = tx.prepare(
            "INSERT INTO seg_match_way
                (subroute_version_id, direction, ordinal, osm_way_id,
                 entry_node_id, exit_node_id, cum_start_m, cum_end_m)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        let mut ins_stat = tx.prepare(
            "INSERT INTO seg_match_stat
                (route_version_id, direction, stops, path_length_m, matched_length_m,
                 snap_dist_p50, snap_dist_p95, snap_dist_max, outliers, gap_repairs,
                 dev_p50_m, dev_p95_m, dev_max_m)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        )?;
        for oc in outcomes {
            let m = match oc.result {
                Ok(m) => m,
                Err(e) => {
                    summary.failed += 1;
                    tracing::warn!(xno = %oc.job.xno, direction = oc.job.direction, error = %e,
                        "match failed; direction skipped");
                    continue;
                }
            };
            if m.runs.is_empty() {
                summary.unmatched += 1;
                tracing::info!(xno = %oc.job.xno, direction = oc.job.direction,
                    "no way runs for direction");
                continue;
            }
            summary.matched += 1;
            summary.total_runs += m.runs.len();
            summary.gap_repairs += m.gap_repairs;
            for (i, r) in m.runs.iter().enumerate() {
                ins_way.execute(rusqlite::params![
                    oc.job.subroute_version_id,
                    oc.job.direction,
                    (i + 1) as i64,
                    r.way_id,
                    r.entry_node,
                    r.exit_node,
                    r.cum_start_m,
                    r.cum_end_m,
                ])?;
            }
            ins_stat.execute(rusqlite::params![
                oc.job.route_version_id,
                oc.job.direction,
                oc.job.stops,
                m.path_length_m,
                m.matched_length_m,
                m.snap_p50_m,
                m.snap_p95_m,
                m.snap_max_m,
                m.outliers as i64,
                m.gap_repairs as i64,
                m.dev_p50_m,
                m.dev_p95_m,
                m.dev_max_m,
            ])?;
        }
    }
    tx.commit()?;
    Ok(summary)
}
