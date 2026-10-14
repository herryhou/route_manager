//! Apply-updates chain: the three `--stale` steps, in dependency order,
//! on one fresh mutable connection. Spawned by `POST /api/edit/refresh`.
//! Holds no durable state: marks are the durable state (spec Decision 5),
//! each step's clears are `cause_edit_id`-guarded inside the crates.

use crate::state::{AppState, RefreshState, RefreshStep};

/// `--stale` OUTLIER threshold, same as the rm-segments CLI default
/// (`crates/rm-segments/src/main.rs` `outlier_m`).
const OUTLIER_M: f64 = 50.0;

/// Run match → segments → registry. Never takes `state.write_lock()`
/// (steps can run for minutes; edits must not block — spec Decision 6):
/// WAL + `busy_timeout` serialize the actual writes, and a fresh mark
/// written mid-run carries a new `cause_edit_id`, so the running pass
/// cannot clear it. Any step error (or join panic) lands in `Failed`.
pub async fn run_chain(state: AppState) {
    let path = state.db_path().to_path_buf();
    // Terminal states keep the run's started_at (spec: started_at shows for
    // running / done / failed). Direct callers (tests) that skipped
    // try_begin_refresh have no Running state to read; stamp now.
    let started_at = match state.refresh_state() {
        RefreshState::Running { started_at, .. } => started_at,
        _ => rm_db::now_rfc3339(),
    };
    // Step updates happen on the worker; `state` itself stays with the
    // caller for the terminal Done/Failed write after the join.
    let st = state.clone();
    let joined = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let conn = rm_db::open(&path)?;
        let batch = rm_db::latest_batch(&conn)?
            .ok_or_else(|| anyhow::anyhow!("no import batch; run rm-ingest first"))?;

        st.set_refresh_step(RefreshStep::Match);
        if let Some(o) = rm_osm_match::run_stale(&conn, batch)? {
            tracing::info!(
                "refresh: match: {} directions, {} cleared, {} kept",
                o.summary.directions,
                o.cleared,
                o.kept
            );
        }

        st.set_refresh_step(RefreshStep::Segments);
        if let Some(o) = rm_segments::run_stale(&conn, batch, OUTLIER_M)? {
            tracing::info!(
                "refresh: segments: {} directions, {} cleared, {} kept",
                o.summary.directions,
                o.cleared,
                o.kept
            );
        }

        st.set_refresh_step(RefreshStep::Registry);
        if let Some(o) = rm_registry::run_stale(&conn, batch)? {
            tracing::info!("refresh: registry: bearing + split rebuild done");
            let _ = o; // StaleRun fields are per-crate summaries; log stays generic
        }
        Ok(())
    })
    .await;

    let finished_at = rm_db::now_rfc3339();
    match joined {
        Ok(Ok(())) => state.set_refresh_state(RefreshState::Done {
            started_at,
            finished_at,
        }),
        Ok(Err(e)) => state.set_refresh_state(RefreshState::Failed {
            error: e.to_string(),
            started_at,
            finished_at,
        }),
        Err(join) => state.set_refresh_state(RefreshState::Failed {
            error: format!("refresh task failed: {join}"),
            started_at,
            finished_at,
        }),
    }
}
