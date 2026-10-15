//! Viterbi map matching of direction polylines onto OSM ways.

pub mod graph;
pub mod models;
pub mod viterbi;

use std::sync::Arc;

use rayon::prelude::*;
use rusqlite::{Connection, OptionalExtension};

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

struct Outcome {
    job: Job,
    result: anyhow::Result<MatchOutput>,
}

fn match_jobs(graph: &Arc<RoadGraph>, jobs: Vec<Job>) -> Vec<Outcome> {
    jobs.into_par_iter()
        .map_init(
            || viterbi::Ctx::new(graph),
            |ctx, job| {
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
            },
        )
        .collect()
}

/// Write outcomes. `None` = full-run wipe-all; `Some(pairs)` = stale run:
/// per-(route_id, direction) scoped deletes ONLY for the pairs recomputed
/// (rows joined through subroute/route on route_id + direction — EVERY
/// route_version, so rows written under an older version cannot survive
/// the recompute). An empty pair list deletes NOTHING — a stale run whose
/// marks were all kept must never wipe matched rows.
fn write_outcomes(
    conn: &Connection,
    batch_id: i64,
    outcomes: Vec<Outcome>,
    scope: Option<&[(i64, i64)]>,
) -> anyhow::Result<Summary> {
    let mut summary = Summary {
        batch_id,
        directions: outcomes.len(),
        ..Default::default()
    };
    let tx = conn.unchecked_transaction()?;
    match scope {
        None => {
            tx.execute("DELETE FROM seg_match_way", [])?;
            tx.execute("DELETE FROM seg_match_stat", [])?;
        }
        Some(pairs) => {
            for (rid, dir) in pairs {
                tx.execute(
                    "DELETE FROM seg_match_way
                     WHERE direction = ?2 AND subroute_version_id IN (
                         SELECT sv.subroute_version_id FROM subroute_version sv
                         JOIN route_version rv ON rv.route_version_id = sv.route_version_id
                         WHERE rv.route_id = ?1)",
                    rusqlite::params![rid, dir],
                )?;
                tx.execute(
                    "DELETE FROM seg_match_stat
                     WHERE direction = ?2 AND route_version_id IN (
                         SELECT route_version_id FROM route_version WHERE route_id = ?1)",
                    rusqlite::params![rid, dir],
                )?;
            }
        }
    }
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

/// Match every published direction of `batch` onto the OSM road graph and
/// write seg_match_way + seg_match_stat. Idempotent: both tables are
/// rebuilt from scratch each run. Per-direction failures are non-fatal.
pub fn run(conn: &Connection, batch_id: i64) -> anyhow::Result<Summary> {
    let graph = Arc::new(RoadGraph::from_conn(conn)?);
    let jobs = collect_jobs(conn, batch_id)?;
    let outcomes = match_jobs(&graph, jobs);
    write_outcomes(conn, batch_id, outcomes, None)
}

#[derive(Debug)]
pub struct StaleOutcome {
    pub summary: Summary, // counts over processed directions only
    pub cleared: usize,   // mark rowids deleted
    pub kept: usize,      // marks left (unknown key / unpublished route / no staging row)
}

/// One marked (route_id, direction): CURRENT route_version, its stops, its
/// effective polyline. Usually one job; a route_version with several
/// subroute_versions yields several (the full run joins the same way).
fn stale_jobs(
    conn: &Connection,
    batch_id: i64,
    route_id: i64,
    direction: i64,
) -> anyhow::Result<Vec<Job>> {
    let mut jobs: Vec<Job> = {
        let mut stmt = conn.prepare(
            "SELECT rv.route_version_id, sv.subroute_version_id, substr(r.ext_uid, 5),
                    (SELECT COUNT(*) FROM route_stop_version o
                      WHERE o.subroute_version_id = sv.subroute_version_id
                        AND o.direction = ?2)
             FROM route r
             JOIN route_version rv ON rv.route_id = r.route_id
                  AND rv.route_version_id = r.current_route_version_id
             JOIN subroute_version sv ON sv.route_version_id = rv.route_version_id
             JOIN src_route_point srp
                  ON srp.batch_id = ?1 AND srp.xno = substr(r.ext_uid, 5)
                    AND srp.direction = ?2
             WHERE r.route_id = ?3",
        )?;
        let rows = stmt.query_map(rusqlite::params![batch_id, direction, route_id], |r| {
            Ok(Job {
                route_version_id: r.get(0)?,
                subroute_version_id: r.get(1)?,
                direction,
                xno: r.get(2)?,
                stops: r.get(3)?,
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

/// Refresh exactly the (route_id, direction) pairs marked stale in
/// `seg_match` (key `'{route_id}:{direction}'`; route_id stable across
/// route versions — the job resolves the CURRENT route_version here).
/// Snapshot (rowid, cause) pairs first; the clear matches both, so a mark
/// re-inserted onto a reused rowid mid-run carries a different cause and
/// survives. Unknown keys / unpublished routes / directions whose staging
/// row the latest batch dropped: warn, keep — never delete rows that were
/// not recomputed.
pub fn run_stale(conn: &Connection, batch_id: i64) -> anyhow::Result<Option<StaleOutcome>> {
    let snapshot: Vec<(i64, i64, String)> = {
        let mut stmt = conn.prepare(
            "SELECT rowid, cause_edit_id, key FROM stale_mark
             WHERE table_name = 'seg_match'",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect::<Result<_, _>>()?
    };
    if snapshot.is_empty() {
        return Ok(None);
    }
    let graph = Arc::new(RoadGraph::from_conn(conn)?);

    let mut jobs: Vec<Job> = Vec::new();
    let mut scope: Vec<(i64, i64)> = Vec::new();
    let mut clear: Vec<(i64, i64)> = Vec::new();
    let mut kept = 0usize;
    for (rowid, cause, key) in snapshot {
        let Some((rid, dir)) = key
            .split_once(':')
            .and_then(|(r, d)| Some((r.parse::<i64>().ok()?, d.parse::<i64>().ok()?)))
        else {
            tracing::warn!(key = %key, "unparseable seg_match key; keeping mark");
            kept += 1;
            continue;
        };
        let published: Option<Option<i64>> = conn
            .query_row(
                "SELECT current_route_version_id FROM route WHERE route_id = ?1",
                [rid],
                |r| r.get::<_, Option<i64>>(0),
            )
            .optional()?;
        if published.flatten().is_none() {
            tracing::warn!(route = rid, "stale route not published; keeping mark");
            kept += 1;
            continue;
        }
        let mark_jobs = stale_jobs(conn, batch_id, rid, dir)?;
        if mark_jobs.is_empty() {
            // latest batch dropped this direction's staging row: nothing to
            // recompute — keep the mark (a later ingest may restore it)
            tracing::warn!(
                route = rid,
                direction = dir,
                "no staging row for stale direction; keeping mark"
            );
            kept += 1;
            continue;
        }
        jobs.extend(mark_jobs);
        scope.push((rid, dir));
        clear.push((rowid, cause));
    }

    let outcomes = match_jobs(&graph, jobs);
    let summary = write_outcomes(conn, batch_id, outcomes, Some(&scope))?;

    // clear exactly the snapshot (rowid, cause) pairs that were processed —
    // a mark re-written since the snapshot (fresh rowid or cause) survives
    let tx = conn.unchecked_transaction()?;
    for (rowid, cause) in &clear {
        tx.execute(
            "DELETE FROM stale_mark WHERE rowid = ?1 AND cause_edit_id = ?2",
            rusqlite::params![rowid, cause],
        )?;
    }
    tx.commit()?;

    Ok(Some(StaleOutcome {
        summary,
        cleared: clear.len(),
        kept,
    }))
}
