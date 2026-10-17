//! Detach-stop edit primitive: version-chain stop removal with
//! pointer-restore undo (spec decisions 7-9 of the rm-job agent CLI).
//!
//! Detach NEVER deletes versioned rows: it mints a new route_version +
//! subroute_version with the detached occurrence(s) left out and moves
//! `route.current_route_version_id` onto it — the detached rsv rows stay
//! under the old version as history. Undo is a pointer restore that also
//! deletes the chain the apply minted (the add-stop undo semantics). An
//! edit carrying `--delete-pole` additionally tears the orphaned pole down
//! (one-way: the whole edit is `NotUndoable`).

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::json;

use super::{load_stop_edit, EditError, StopEditRow};
use crate::stale::mark_line_stale;

/// Detach command: the route/direction/stop_code addressed occurrence set,
/// the loop disambiguators, and the one-way pole teardown flag.
#[derive(Debug, Clone)]
pub struct DetachCmd {
    pub xno: String,
    pub direction: i64,
    pub stop_code: String,
    /// When the code occurs more than once in the direction: pick the
    /// occurrence at this `stop_sequence` (`--sequence`).
    pub sequence: Option<i64>,
    /// When the code occurs more than once in the direction: take every
    /// occurrence (`--all`).
    pub all: bool,
    /// Delete the pole itself when the detach orphans it. ONE-WAY: an edit
    /// carrying this flag is `NotUndoable` as a whole, flag honored or not.
    pub delete_pole: bool,
}

/// One detached occurrence: the rsv row being left out of the new chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetachOccurrence {
    pub route_stop_version_id: i64,
    pub sequence: i64,
}

/// Read-only preview of a detach: the resolved pole, the occurrence set
/// the apply leaves out, the orphan verdict, and every condition short of
/// a rejection as a warning.
#[derive(Debug)]
pub struct DetachPlan {
    pub xno: String,
    pub direction: i64,
    pub route_id: i64,
    /// The route's current `route_version_id` at plan time — apply
    /// re-validates the route pointer against it inside the tx.
    pub route_version_id: i64,
    pub stop_id: i64,
    pub stop_version_id: i64,
    pub stop_code: String,
    pub stop_name: String,
    /// The occurrences apply leaves out (already disambiguated).
    pub occurrences: Vec<DetachOccurrence>,
    /// True when, after this detach, no current (route, direction) serves
    /// the pole anywhere — apply refuses unless `delete_pole`.
    pub pole_orphan: bool,
    /// This route's current-directions serving the pole (ascending).
    pub serving_directions: Vec<i64>,
    /// Echo of `DetachCmd::delete_pole` — apply's refusal/teardown input.
    pub delete_pole: bool,
    pub stale_routes: Vec<i64>,
    pub warnings: Vec<String>,
}

/// What one applied detach did: the audit row id, the version swap, and
/// whether the one-way pole teardown ran.
#[derive(Debug)]
pub struct DetachOutcome {
    pub edit_id: i64,
    pub old_route_version_id: i64,
    pub new_route_version_id: i64,
    pub pole_deleted: bool,
    pub stale_routes: Vec<i64>,
}

/// A detach undone: the undo's audit row id and the restored pointer.
#[derive(Debug)]
pub struct DetachUndone {
    pub edit_id: i64,
    pub restored_route_version_id: i64,
}

/// Count the (route, direction) pairs serving `stop_version_id` on their
/// CURRENT version, excluding the given (route, direction). Current-ness
/// is the pointer, so history rows never count as serving.
fn count_other_directions(
    conn: &Connection,
    stop_version_id: i64,
    route_id: i64,
    direction: i64,
) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(DISTINCT r.route_id || ':' || rsv.direction)
         FROM route_stop_version rsv
         JOIN subroute_version sv ON sv.subroute_version_id = rsv.subroute_version_id
         JOIN route_version rv ON rv.route_version_id = sv.route_version_id
         JOIN route r ON r.route_id = rv.route_id
                  AND r.current_route_version_id = rv.route_version_id
         WHERE rsv.stop_version_id = ?1
           AND NOT (r.route_id = ?2 AND rsv.direction = ?3)",
        params![stop_version_id, route_id, direction],
        |r| r.get(0),
    )?)
}

// ---------- read-only plan ----------

/// Dry-run the detach against the current DB without writing a byte.
/// Validation order (first error wins): xno resolution (missing route row
/// or NULL `current_route_version_id` both → `UnknownRoute`) → direction
/// missing from the current version's `route_stop_version` rows
/// (`UnknownDirection`, read via the subroute `LIMIT 1` precedent) → stop
/// code absent from the direction (`UnknownAfterStop`) → duplicate
/// occurrence without `--sequence`/`--all` (`DuplicateStopOccurrence`).
/// Verdicts: `pole_orphan` counts the pole's remaining references — this
/// direction's surviving occurrences plus every other current (route,
/// direction) serving it across ALL routes; `serving_directions` lists
/// this route's serving directions. Warnings: a detached first/last stop
/// notes the neighbors' sequence shift.
pub fn plan_detach_stop(conn: &Connection, cmd: &DetachCmd) -> Result<DetachPlan> {
    let (route_id, route_version_id): (i64, Option<i64>) = match conn.query_row(
        "SELECT route_id, current_route_version_id FROM route WHERE ext_uid = ?1",
        [format!("tcg:{}", cmd.xno)],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ) {
        Ok(v) => v,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Err(EditError::UnknownRoute(cmd.xno.clone()).into())
        }
        Err(e) => return Err(e.into()),
    };
    let Some(route_version_id) = route_version_id else {
        return Err(EditError::UnknownRoute(cmd.xno.clone()).into());
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
    let Some(subroute_version_id) = subroute_version_id else {
        return Err(EditError::UnknownDirection(cmd.xno.clone(), cmd.direction).into());
    };

    // the direction's rows in travel order:
    // (rsv_id, stop_sequence, stop_id, stop_version_id, stop_code, stop_name)
    let rows: Vec<(i64, i64, i64, i64, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT rsv.route_stop_version_id, rsv.stop_sequence,
                    s.stop_id, s.stop_version_id, s.stop_code, s.stop_name
             FROM route_stop_version rsv
             JOIN stop_version s ON s.stop_version_id = rsv.stop_version_id
             WHERE rsv.subroute_version_id = ?1 AND rsv.direction = ?2
             ORDER BY rsv.stop_sequence",
        )?;
        let out = stmt
            .query_map(params![subroute_version_id, cmd.direction], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        out
    };
    if rows.is_empty() {
        return Err(EditError::UnknownDirection(cmd.xno.clone(), cmd.direction).into());
    }

    // the pole's occurrences in this direction
    let matched: Vec<&(i64, i64, i64, i64, String, String)> =
        rows.iter().filter(|r| r.4 == cmd.stop_code).collect();
    if matched.is_empty() {
        return Err(EditError::UnknownAfterStop(
            cmd.xno.clone(),
            cmd.direction,
            cmd.stop_code.clone(),
        )
        .into());
    }
    let (stop_id, stop_version_id, stop_name) = (matched[0].2, matched[0].3, matched[0].5.clone());

    // duplicate disambiguation: --sequence wins over --all; a sequence the
    // pole does not occupy is as unknown as the code
    let occurrences: Vec<DetachOccurrence> = if let Some(seq) = cmd.sequence {
        match matched.iter().find(|r| r.1 == seq) {
            Some(r) => vec![DetachOccurrence {
                route_stop_version_id: r.0,
                sequence: r.1,
            }],
            None => {
                return Err(EditError::UnknownAfterStop(
                    cmd.xno.clone(),
                    cmd.direction,
                    cmd.stop_code.clone(),
                )
                .into())
            }
        }
    } else if matched.len() > 1 {
        if cmd.all {
            matched
                .iter()
                .map(|r| DetachOccurrence {
                    route_stop_version_id: r.0,
                    sequence: r.1,
                })
                .collect()
        } else {
            return Err(EditError::DuplicateStopOccurrence {
                xno: cmd.xno.clone(),
                direction: cmd.direction,
                stop_code: cmd.stop_code.clone(),
                count: matched.len() as i64,
            }
            .into());
        }
    } else {
        vec![DetachOccurrence {
            route_stop_version_id: matched[0].0,
            sequence: matched[0].1,
        }]
    };

    // this route's serving directions (current chain, any subroute)
    let serving_directions: Vec<i64> = {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT rsv.direction
             FROM route_stop_version rsv
             JOIN subroute_version sv ON sv.subroute_version_id = rsv.subroute_version_id
             WHERE sv.route_version_id = ?1 AND rsv.stop_version_id = ?2
             ORDER BY rsv.direction",
        )?;
        let out = stmt
            .query_map(params![route_version_id, stop_version_id], |r| r.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        out
    };

    // orphan verdict: what still serves the pole after this detach?
    let remaining_here = (matched.len() - occurrences.len()) as i64;
    let others = count_other_directions(conn, stop_version_id, route_id, cmd.direction)?;
    let pole_orphan = remaining_here == 0 && others == 0;

    // warnings: a detached first/last stop shifts its neighbors
    let mut warnings = Vec::new();
    let first_seq = rows[0].1;
    let last_seq = rows[rows.len() - 1].1;
    for occ in &occurrences {
        let pos = if occ.sequence == first_seq {
            Some("first")
        } else if occ.sequence == last_seq {
            Some("last")
        } else {
            None
        };
        if let Some(pos) = pos {
            warnings.push(format!(
                "route {} direction {}: stop '{}' at sequence {} is the {} stop \
                 — neighbors' sequence shifts",
                cmd.xno, cmd.direction, cmd.stop_code, occ.sequence, pos
            ));
        }
    }

    Ok(DetachPlan {
        xno: cmd.xno.clone(),
        direction: cmd.direction,
        route_id,
        route_version_id,
        stop_id,
        stop_version_id,
        stop_code: cmd.stop_code.clone(),
        stop_name,
        occurrences,
        pole_orphan,
        serving_directions,
        delete_pole: cmd.delete_pole,
        stale_routes: vec![route_id],
        warnings,
    })
}

// ---------- apply ----------

/// Application service: plan (read-only) → apply (one tx).
pub fn detach_stop(
    conn: &Connection,
    cmd: DetachCmd,
    editor: &str,
    note: &str,
) -> Result<DetachOutcome> {
    let plan = plan_detach_stop(conn, &cmd)?;
    apply_detach_stop(conn, &plan, editor, note)
}

/// Per-route state captured before the pointer move: the plan's old
/// version plus the revision/updated_at the undo restores.
struct RouteState {
    old_revision: i64,
    old_updated_at: String,
}

/// Apply a plan in ONE transaction: mint a new `route_version` +
/// `subroute_version` with the whole current `route_stop_version` set
/// re-minted (directions ascending, sequences renumbered) minus the
/// detached occurrence(s), move `route.current_route_version_id`, write
/// one tagged `edit_log` row, and mark the line datasets stale for the
/// edited direction. The detached rsv rows stay under the old version —
/// no forward edit destroys versioned rows.
///
/// Guards run inside the tx BEFORE any write: the route pointer must still
/// sit on the planned version (`NotCurrent`), every planned occurrence
/// must still sit in the old chain on the planned pole version
/// (`NotCurrent`), and a freshly-recomputed orphan verdict refuses with
/// `OrphanPoleRefused` unless the plan carries `delete_pole` — whose
/// teardown (pole versions, stop row, and the station iff the pole was
/// its last member) mirrors `apply_stop_add_undo`'s deleted row set.
pub fn apply_detach_stop(
    conn: &Connection,
    plan: &DetachPlan,
    editor: &str,
    note: &str,
) -> Result<DetachOutcome> {
    let tx = conn.unchecked_transaction()?;
    let now = crate::now_rfc3339();

    // in-tx re-validation BEFORE any write: the pointer must still sit on
    // the planned version; the pre-move revision/updated_at are the undo
    // payload's restore values
    let (current, old_revision, old_updated_at): (Option<i64>, i64, String) = tx
        .query_row(
            "SELECT current_route_version_id, revision, updated_at
             FROM route WHERE route_id = ?1",
            [plan.route_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .ok_or(EditError::NotCurrent(plan.route_version_id))?;
    if current != Some(plan.route_version_id) {
        return Err(EditError::NotCurrent(plan.route_version_id).into());
    }
    let state = RouteState {
        old_revision,
        old_updated_at,
    };

    // the old version's whole rsv set in travel order (directions
    // ascending): (direction, stop_sequence, rsv_id, stop_version_id,
    // source_order_no)
    let old_rows: Vec<(i64, i64, i64, i64, Option<i64>)> = {
        let mut stmt = tx.prepare(
            "SELECT rsv.direction, rsv.stop_sequence, rsv.route_stop_version_id,
                    rsv.stop_version_id, rsv.source_order_no
             FROM route_stop_version rsv
             JOIN subroute_version sv ON sv.subroute_version_id = rsv.subroute_version_id
             WHERE sv.route_version_id = ?1
             ORDER BY rsv.direction, rsv.stop_sequence",
        )?;
        let out = stmt
            .query_map([plan.route_version_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        out
    };

    // occurrence guard: each planned occurrence must still sit in the old
    // chain, on the edited direction, pointing at the planned pole version
    // (a pole rename repoints rsv rows in place — that moved-on sequence
    // is not the planned one)
    for occ in &plan.occurrences {
        if !old_rows.iter().any(|r| {
            r.2 == occ.route_stop_version_id && r.0 == plan.direction && r.3 == plan.stop_version_id
        }) {
            return Err(EditError::NotCurrent(plan.route_version_id).into());
        }
    }

    // fresh orphan verdict: references may have changed since plan time.
    // remaining_here counts this direction's surviving occurrences of the
    // pole; others counts every other current (route, direction).
    let remaining_here = (old_rows
        .iter()
        .filter(|r| r.0 == plan.direction && r.3 == plan.stop_version_id)
        .count()
        - plan.occurrences.len()) as i64;
    let others = count_other_directions(&tx, plan.stop_version_id, plan.route_id, plan.direction)?;
    let pole_orphan_now = remaining_here == 0 && others == 0;
    if pole_orphan_now && !plan.delete_pole {
        return Err(EditError::OrphanPoleRefused(format!(
            "detaching '{}' (stop {}) from route {} direction {} drops its last \
             serving direction",
            plan.stop_code, plan.stop_id, plan.xno, plan.direction
        ))
        .into());
    }

    // 1. the new chain: route_version (next version_no, PUBLISHED, open
    // end) + subroute_version (the LIMIT 1 subroute copy)
    let max_no: i64 = tx.query_row(
        "SELECT COALESCE(MAX(version_no), 0) FROM route_version WHERE route_id = ?1",
        [plan.route_id],
        |r| r.get(0),
    )?;
    let new_rv = crate::mint(&tx, "route_version")?;
    tx.execute(
        "INSERT INTO route_version
            (route_version_id, route_id, version_no, valid_from, valid_to, status, created_at, published_at)
         VALUES (?1, ?2, ?3, ?4, NULL, 'PUBLISHED', ?4, ?4)",
        params![new_rv, plan.route_id, max_no + 1, now],
    )?;
    let (subroute_id, subroute_name): (i64, String) = tx.query_row(
        "SELECT subroute_id, subroute_name FROM subroute_version
         WHERE route_version_id = ?1 ORDER BY subroute_version_id LIMIT 1",
        [plan.route_version_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let new_sv_id = crate::mint(&tx, "subroute_version")?;
    tx.execute(
        "INSERT INTO subroute_version
            (subroute_version_id, route_version_id, subroute_id, subroute_name, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![new_sv_id, new_rv, subroute_id, subroute_name, now],
    )?;

    // 2. every direction re-minted renumbered, the detached occurrence(s)
    // left out (the add-stop re-mint loop, minus the splice)
    let mut i = 0;
    while i < old_rows.len() {
        let dir = old_rows[i].0;
        let mut j = i;
        while j < old_rows.len() && old_rows[j].0 == dir {
            j += 1;
        }
        let mut n: i64 = 0;
        for row in &old_rows[i..j] {
            let detached = dir == plan.direction
                && plan
                    .occurrences
                    .iter()
                    .any(|o| o.route_stop_version_id == row.2);
            if detached {
                continue;
            }
            n += 1;
            let rsv_id = crate::mint(&tx, "route_stop_version")?;
            tx.execute(
                "INSERT INTO route_stop_version
                    (route_stop_version_id, subroute_version_id, stop_version_id,
                     direction, stop_sequence, source_order_no, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![rsv_id, new_sv_id, row.3, dir, n, row.4, now],
            )?;
        }
        i = j;
    }

    // 3. the pointer move — the only UPDATE a re-versioning write makes
    tx.execute(
        "UPDATE route SET current_route_version_id = ?1, revision = ?2, updated_at = ?3
         WHERE route_id = ?4",
        params![new_rv, state.old_revision + 1, now, plan.route_id],
    )?;

    // 4. the one-way pole teardown (delete_pole, honored only on a fresh
    // orphan verdict — a pole still serving elsewhere keeps its rows)
    let pole_deleted = pole_orphan_now && plan.delete_pole;
    if pole_deleted {
        delete_pole_rows(&tx, plan)?;
    }

    // 5. one tagged audit row
    let sequences: Vec<i64> = plan.occurrences.iter().map(|o| o.sequence).collect();
    let changes = json!({
        "kind": "stop_detach",
        "xno": plan.xno,
        "direction": plan.direction,
        "stop_code": plan.stop_code,
        "sequences": sequences,
        "delete_pole": plan.delete_pole,
        "old_route_version_id": plan.route_version_id,
        "new_route_version_id": new_rv,
        "pole_deleted": pole_deleted,
        "old_revision": state.old_revision,
        "old_updated_at": state.old_updated_at,
    });
    tx.execute(
        "INSERT INTO edit_log (ts, editor, entity, entity_id, action, changes, note, version_before, version_after)
         VALUES (?1, ?2, 'stop_detach', ?3, 'detach', ?4, ?5, ?6, ?7)",
        params![
            now,
            editor,
            plan.stop_id,
            changes.to_string(),
            if note.is_empty() { None } else { Some(note) },
            plan.route_version_id,
            new_rv
        ],
    )?;
    let edit_id = tx.last_insert_rowid();

    // 6. stale marks: the line datasets for the edited direction
    mark_line_stale(&tx, plan.route_id, plan.direction, edit_id, &now)?;
    tx.commit()?;

    Ok(DetachOutcome {
        edit_id,
        old_route_version_id: plan.route_version_id,
        new_route_version_id: new_rv,
        pole_deleted,
        stale_routes: vec![plan.route_id],
    })
}

/// The `--delete-pole` teardown, mirroring `apply_stop_add_undo`'s deleted
/// row set: the pole's attachments (history chains keep rsv rows for the
/// pole — with the pole itself gone they must go), its bearing rows, its
/// `stop_version` rows, the `stop` row, and the station link (the
/// station, its versions, and its bearing iff this pole was its last
/// member). Children before parents throughout; ONE-WAY — no undo.
fn delete_pole_rows(tx: &Connection, plan: &DetachPlan) -> Result<()> {
    let station_id: i64 = tx.query_row(
        "SELECT station_id FROM stop_version WHERE stop_version_id = ?1",
        [plan.stop_version_id],
        |r| r.get(0),
    )?;
    tx.execute(
        "DELETE FROM route_stop_version WHERE stop_version_id IN
             (SELECT stop_version_id FROM stop_version WHERE stop_id = ?1)",
        [plan.stop_id],
    )?;
    tx.execute(
        "DELETE FROM stop_bearing WHERE stop_version_id IN
             (SELECT stop_version_id FROM stop_version WHERE stop_id = ?1)",
        [plan.stop_id],
    )?;
    tx.execute(
        "DELETE FROM stop_version WHERE stop_id = ?1",
        [plan.stop_id],
    )?;
    // the station iff the pole was its last member (any validity — closed
    // versions of other poles keep a shared station alive)
    let members: i64 = tx.query_row(
        "SELECT COUNT(*) FROM stop_version WHERE station_id = ?1",
        [station_id],
        |r| r.get(0),
    )?;
    if members == 0 {
        tx.execute(
            "DELETE FROM station_bearing WHERE station_id = ?1",
            [station_id],
        )?;
        tx.execute(
            "DELETE FROM station_version WHERE station_id = ?1",
            [station_id],
        )?;
        tx.execute("DELETE FROM station WHERE station_id = ?1", [station_id])?;
    }
    tx.execute("DELETE FROM stop WHERE stop_id = ?1", [plan.stop_id])?;
    Ok(())
}

// ---------- undo ----------

/// Write-side op for the detach undo, parsed from the logged changes.
struct StopDetachUndoOp {
    edit_id: i64,
    stop_id: i64,
    old_rv: i64,
    new_rv: i64,
    old_revision: i64,
    old_updated_at: String,
}

/// Pure guard + parse: only a non-undo `stop_detach` row whose payload is
/// fully formed AND carries no `delete_pole` flag is invertible (an edit
/// carrying the flag is one-way as a whole). Every missing or malformed
/// field is a refusal (`NotUndoable`), never a panic.
fn plan_stop_detach_undo(edit: &StopEditRow) -> Result<StopDetachUndoOp, EditError> {
    let refused = || EditError::NotUndoable(edit.edit_id);
    if edit.entity != "stop_detach" || edit.action == "undo" {
        return Err(refused());
    }
    let c = &edit.changes;
    if c.get("kind").and_then(|k| k.as_str()) != Some("stop_detach") {
        return Err(refused());
    }
    if c.get("delete_pole").and_then(|v| v.as_bool()) != Some(false) {
        return Err(refused());
    }
    let (Some(old_rv), Some(new_rv), Some(old_revision)) = (
        c.get("old_route_version_id").and_then(|v| v.as_i64()),
        c.get("new_route_version_id").and_then(|v| v.as_i64()),
        c.get("old_revision").and_then(|v| v.as_i64()),
    ) else {
        return Err(refused());
    };
    let Some(old_updated_at) = c
        .get("old_updated_at")
        .and_then(|v| v.as_str())
        .map(str::to_string)
    else {
        return Err(refused());
    };
    Ok(StopDetachUndoOp {
        edit_id: edit.edit_id,
        stop_id: edit.stop_id,
        old_rv,
        new_rv,
        old_revision,
        old_updated_at,
    })
}

/// Undo a stop_detach in ONE transaction: delete the chain the apply
/// minted (its rsv rows, its subroute_version, then — after the pointer
/// restore, because `route.current_route_version_id` → `route_version`
/// forbids deleting the pointed-at row — the route_version), restore the
/// pointer + revision + updated_at, delete the job's own stale marks, and
/// write the undo's audit row (`action='undo'`, `undo_of`). Mints nothing
/// and marks nothing stale — the derived data reads the restored rows
/// again.
///
/// Guards run inside the tx BEFORE any write: some route must still point
/// at the detach's `new_route_version_id` (a later re-versioning edit
/// means the detach is no longer the newest write), and every rsv row of
/// the minted chain must still point at a stop_version the old chain also
/// serves (an in-place pole edit repointed rows under unchanged ids).
/// Anything else is `NotUndoable`.
pub fn undo_detach_stop(
    conn: &Connection,
    edit_id: i64,
    editor: &str,
    note: &str,
) -> Result<DetachUndone> {
    let row = load_stop_edit(conn, edit_id)?.ok_or(EditError::NotUndoable(edit_id))?;
    let op = plan_stop_detach_undo(&row)?;
    apply_stop_detach_undo(conn, &op, editor, note)
}

fn apply_stop_detach_undo(
    conn: &Connection,
    op: &StopDetachUndoOp,
    editor: &str,
    note: &str,
) -> Result<DetachUndone> {
    let tx = conn.unchecked_transaction()?;

    // guards BEFORE any write: the route that points at the minted version
    // (also derives the route_id the payload does not carry)
    let (route_id, current): (i64, Option<i64>) = tx
        .query_row(
            "SELECT route_id, current_route_version_id FROM route
             WHERE current_route_version_id = ?1",
            [op.new_rv],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or(EditError::NotUndoable(op.edit_id))?;
    if current != Some(op.new_rv) {
        return Err(EditError::NotUndoable(op.edit_id).into());
    }

    // the minted chain's rows: (rsv_id, stop_version_id)
    let new_chain: Vec<(i64, i64)> = {
        let mut stmt = tx.prepare(
            "SELECT rsv.route_stop_version_id, rsv.stop_version_id
             FROM route_stop_version rsv
             JOIN subroute_version sv ON sv.subroute_version_id = rsv.subroute_version_id
             WHERE sv.route_version_id = ?1
             ORDER BY rsv.route_stop_version_id",
        )?;
        let out = stmt
            .query_map([op.new_rv], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        out
    };
    // in-place repoint guard: every kept row must still point at a
    // stop_version the old chain also serves
    let old_svs: Vec<i64> = {
        let mut stmt = tx.prepare(
            "SELECT DISTINCT rsv.stop_version_id
             FROM route_stop_version rsv
             JOIN subroute_version sv ON sv.subroute_version_id = rsv.subroute_version_id
             WHERE sv.route_version_id = ?1",
        )?;
        let out = stmt
            .query_map([op.old_rv], |r| r.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        out
    };
    if new_chain.iter().any(|&(_, sv)| !old_svs.contains(&sv)) {
        return Err(EditError::NotUndoable(op.edit_id).into());
    }
    let new_sv_ids: Vec<i64> = {
        let mut stmt = tx.prepare(
            "SELECT subroute_version_id FROM subroute_version
             WHERE route_version_id = ?1
             ORDER BY subroute_version_id",
        )?;
        let out = stmt
            .query_map([op.new_rv], |r| r.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        out
    };

    // the pinned delete order: children before parents, the pointer
    // restore between the subroute_version and route_version deletes
    for &(rsv, _) in &new_chain {
        tx.execute(
            "DELETE FROM route_stop_version WHERE route_stop_version_id = ?1",
            [rsv],
        )?;
    }
    for &sv in &new_sv_ids {
        tx.execute(
            "DELETE FROM subroute_version WHERE subroute_version_id = ?1",
            [sv],
        )?;
    }
    tx.execute(
        "UPDATE route SET current_route_version_id = ?1, revision = ?2, updated_at = ?3
         WHERE route_id = ?4",
        params![op.old_rv, op.old_revision, op.old_updated_at, route_id],
    )?;
    tx.execute(
        "DELETE FROM route_version WHERE route_version_id = ?1",
        [op.new_rv],
    )?;
    tx.execute(
        "DELETE FROM stale_mark WHERE cause_edit_id = ?1",
        [op.edit_id],
    )?;

    let now = crate::now_rfc3339();
    let changes = json!({ "kind": "stop_detach_undo" });
    tx.execute(
        "INSERT INTO edit_log (ts, editor, entity, entity_id, action, changes, note, undo_of, version_before, version_after)
         VALUES (?1, ?2, 'stop_detach', ?3, 'undo', ?4, ?5, ?6, NULL, NULL)",
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
    Ok(DetachUndone {
        edit_id: undo_edit_id,
        restored_route_version_id: op.old_rv,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_in_memory;
    use crate::{add_stop, undo_edit, AddStopCmd, AttachCmd, UndoOutcome};
    use rusqlite::Connection;

    const T0: &str = "2026-01-01T00:00:00Z";

    /// Route 1 ('tcg:1') dir 0 = A(001) → B(002) → C(003) → D(004), dir 1 =
    /// C → B → A, published via route_version 11 → subroute_version 11.
    /// B serves both directions (pole never orphaned by a dir-0 detach);
    /// D (stop 4, station 2's only pole) serves dir 0 alone.
    fn base_fixture(conn: &Connection) {
        conn.execute_batch(&format!(
            "INSERT INTO station_group (station_group_id, created_at) VALUES (1, '{T0}');
             INSERT INTO station_group_version (station_group_version_id, station_group_id, station_group_name, source, valid_from, created_at)
                 VALUES (1, 1, 'g', 'capture', '{T0}', '{T0}');
             INSERT INTO station (station_id, ext_uid, created_at) VALUES
                 (1, 'tcg:node:1', '{T0}'), (2, 'tcg:node:2', '{T0}');
             INSERT INTO station_version (station_version_id, station_id, station_group_id, station_name, latitude, longitude, valid_from, created_at) VALUES
                 (20, 1, 1, 'S', 24.0, 120.0, '{T0}', '{T0}'),
                 (21, 2, 1, 'D站', 24.5, 120.5, '{T0}', '{T0}');
             INSERT INTO stop (stop_id, ext_uid, created_at) VALUES
                 (1, 'tcg:stop:1', '{T0}'), (2, 'tcg:stop:2', '{T0}'),
                 (3, 'tcg:stop:3', '{T0}'), (4, 'tcg:stop:4', '{T0}');
             INSERT INTO stop_version (stop_version_id, stop_id, station_id, stop_code, stop_name, direction, latitude, longitude, valid_from, created_at) VALUES
                 (10, 1, 1, '001', 'A', 0, 24.1400, 120.6857, '{T0}', '{T0}'),
                 (11, 2, 1, '002', 'B', 0, 24.1405, 120.6857, '{T0}', '{T0}'),
                 (12, 3, 1, '003', 'C', 0, 24.1410, 120.6857, '{T0}', '{T0}'),
                 (13, 4, 2, '004', 'D', 0, 24.1415, 120.6857, '{T0}', '{T0}');
             INSERT INTO route (route_id, route_code, ext_uid, revision, created_at, updated_at)
                 VALUES (1, '1', 'tcg:1', 1, '{T0}', '{T0}');
             INSERT INTO route_version (route_version_id, route_id, version_no, status, valid_from, created_at)
                 VALUES (11, 1, 1, 'PUBLISHED', '{T0}', '{T0}');
             UPDATE route SET current_route_version_id = 11 WHERE route_id = 1;
             INSERT INTO subroute (subroute_id, route_id, subroute_code, created_at)
                 VALUES (11, 1, '1', '{T0}');
             INSERT INTO subroute_version (subroute_version_id, route_version_id, subroute_id, subroute_name, created_at)
                 VALUES (11, 11, 11, 'A - D', '{T0}');
             INSERT INTO route_stop_version (route_stop_version_id, subroute_version_id, stop_version_id, direction, stop_sequence, created_at) VALUES
                 (1, 11, 10, 0, 1, '{T0}'), (2, 11, 11, 0, 2, '{T0}'),
                 (3, 11, 12, 0, 3, '{T0}'), (4, 11, 13, 0, 4, '{T0}'),
                 (5, 11, 12, 1, 1, '{T0}'), (6, 11, 11, 1, 2, '{T0}'),
                 (7, 11, 10, 1, 3, '{T0}');
             INSERT INTO osm_way (osm_way_id, highway, oneway, name)
                 VALUES (100, 'residential', 0, '南北路');
             INSERT INTO osm_node (osm_node_id, lat, lon) VALUES
                 (1, 24.1400, 120.6857), (2, 24.1405, 120.6857), (3, 24.1410, 120.6857);
             INSERT INTO osm_way_node (osm_way_id, ordinal, osm_node_id) VALUES
                 (100, 1, 1), (100, 2, 2), (100, 3, 3);
             INSERT INTO id_sequence (table_name, next_value) VALUES
                 ('route_version', 12), ('subroute_version', 12), ('route_stop_version', 8),
                 ('stop', 5), ('stop_version', 14),
                 ('station', 3), ('station_version', 22),
                 ('station_group', 2), ('station_group_version', 2);"
        ))
        .unwrap();
    }

    /// base_fixture + route 2 ('tcg:2'), a loop whose dir 1 serves the SAME
    /// pole B (sv 11) at sequences 29 and 42.
    fn loop_fixture(conn: &Connection) {
        base_fixture(conn);
        conn.execute_batch(&format!(
            "INSERT INTO route (route_id, route_code, ext_uid, revision, created_at, updated_at)
                 VALUES (2, '2', 'tcg:2', 1, '{T0}', '{T0}');
             INSERT INTO route_version (route_version_id, route_id, version_no, status, valid_from, created_at)
                 VALUES (21, 2, 1, 'PUBLISHED', '{T0}', '{T0}');
             UPDATE route SET current_route_version_id = 21 WHERE route_id = 2;
             INSERT INTO subroute (subroute_id, route_id, subroute_code, created_at)
                 VALUES (21, 2, '2', '{T0}');
             INSERT INTO subroute_version (subroute_version_id, route_version_id, subroute_id, subroute_name, created_at)
                 VALUES (21, 21, 21, 'loop', '{T0}');
             INSERT INTO route_stop_version (route_stop_version_id, subroute_version_id, stop_version_id, direction, stop_sequence, created_at) VALUES
                 (71, 21, 11, 1, 29, '{T0}'), (72, 21, 11, 1, 42, '{T0}');
             UPDATE id_sequence SET next_value = 22 WHERE table_name = 'route_version';
             UPDATE id_sequence SET next_value = 22 WHERE table_name = 'subroute_version';
             UPDATE id_sequence SET next_value = 73 WHERE table_name = 'route_stop_version';"
        ))
        .unwrap();
    }

    fn cmd(xno: &str, direction: i64, stop_code: &str) -> DetachCmd {
        DetachCmd {
            xno: xno.into(),
            direction,
            stop_code: stop_code.into(),
            sequence: None,
            all: false,
            delete_pole: false,
        }
    }

    /// Round-trip exclusions: the audit rows (undo writes its own by
    /// design), the stale marks (the job's own are deleted by undo), and
    /// the id allocator (ids are never reused — undo restores data, not
    /// ids). Same set as the add-stop round-trip.
    const ROUNDTRIP_SKIP: &[&str] = &["edit_log", "stale_mark", "id_sequence"];

    /// Every user table's full contents, name-then-rowid order — the
    /// byte-identical check. `skip` drops tables the comparison excludes.
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

    // ---------- plan_detach_stop ----------

    #[test]
    fn plan_reports_found_and_orphan_verdicts() {
        let conn = open_in_memory().unwrap();
        base_fixture(&conn);
        // B serves dir 0 AND dir 1: not orphaned, no terminal warning (seq 2
        // of 1..4)
        let p = plan_detach_stop(&conn, &cmd("1", 0, "002")).unwrap();
        assert_eq!(p.route_id, 1);
        assert_eq!(p.route_version_id, 11);
        assert_eq!(p.stop_id, 2);
        assert_eq!(p.stop_version_id, 11);
        assert_eq!(p.stop_code, "002");
        assert_eq!(p.stop_name, "B");
        assert_eq!(
            p.occurrences,
            vec![DetachOccurrence {
                route_stop_version_id: 2,
                sequence: 2
            }]
        );
        assert!(!p.pole_orphan);
        assert_eq!(p.serving_directions, vec![0, 1]);
        assert_eq!(p.stale_routes, vec![1]);
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
        // D serves dir 0 only: detaching it orphans the pole; seq 4 is the
        // direction's last stop
        let p = plan_detach_stop(&conn, &cmd("1", 0, "004")).unwrap();
        assert!(p.pole_orphan);
        assert_eq!(p.serving_directions, vec![0]);
        assert_eq!(p.warnings.len(), 1, "{:?}", p.warnings);
        assert!(p.warnings[0].contains("last stop"), "{:?}", p.warnings);
        // unknown route / direction / stop code
        let err = plan_detach_stop(&conn, &cmd("9", 0, "002")).unwrap_err();
        assert!(
            matches!(
                err.downcast_ref::<EditError>(),
                Some(EditError::UnknownRoute(_))
            ),
            "{err}"
        );
        let err = plan_detach_stop(&conn, &cmd("1", 5, "002")).unwrap_err();
        match err.downcast_ref::<EditError>() {
            Some(EditError::UnknownDirection(xno, dir)) => {
                assert_eq!((xno.as_str(), *dir), ("1", 5))
            }
            other => panic!("expected UnknownDirection, got {other:?}"),
        }
        let err = plan_detach_stop(&conn, &cmd("1", 0, "ZZZ")).unwrap_err();
        match err.downcast_ref::<EditError>() {
            Some(EditError::UnknownAfterStop(xno, dir, code)) => {
                assert_eq!((xno.as_str(), *dir, code.as_str()), ("1", 0, "ZZZ"))
            }
            other => panic!("expected UnknownAfterStop, got {other:?}"),
        }
    }

    #[test]
    fn plan_duplicate_occurrence_needs_sequence_or_all() {
        let conn = open_in_memory().unwrap();
        loop_fixture(&conn);
        // same pole at seq 29 and 42 in dir 1: no disambiguator → refuse
        let err = plan_detach_stop(&conn, &cmd("2", 1, "002")).unwrap_err();
        match err.downcast_ref::<EditError>() {
            Some(EditError::DuplicateStopOccurrence {
                xno,
                direction,
                stop_code,
                count,
            }) => {
                assert_eq!(
                    (xno.as_str(), *direction, stop_code.as_str(), *count),
                    ("2", 1, "002", 2)
                );
            }
            other => panic!("expected DuplicateStopOccurrence, got {other:?}"),
        }
        // --sequence picks exactly one occurrence
        let p = plan_detach_stop(
            &conn,
            &DetachCmd {
                sequence: Some(42),
                ..cmd("2", 1, "002")
            },
        )
        .unwrap();
        assert_eq!(
            p.occurrences,
            vec![DetachOccurrence {
                route_stop_version_id: 72,
                sequence: 42
            }]
        );
        // --all takes both
        let p = plan_detach_stop(
            &conn,
            &DetachCmd {
                all: true,
                ..cmd("2", 1, "002")
            },
        )
        .unwrap();
        assert_eq!(
            p.occurrences,
            vec![
                DetachOccurrence {
                    route_stop_version_id: 71,
                    sequence: 29
                },
                DetachOccurrence {
                    route_stop_version_id: 72,
                    sequence: 42
                },
            ]
        );
        // a sequence the pole does not occupy is as unknown as the code
        let err = plan_detach_stop(
            &conn,
            &DetachCmd {
                sequence: Some(99),
                ..cmd("2", 1, "002")
            },
        )
        .unwrap_err();
        assert!(
            matches!(
                err.downcast_ref::<EditError>(),
                Some(EditError::UnknownAfterStop(_, _, _))
            ),
            "{err}"
        );
    }

    // ---------- apply_detach_stop ----------

    #[test]
    fn apply_mints_new_chain_and_moves_pointer() {
        let conn = open_in_memory().unwrap();
        base_fixture(&conn);
        let out = detach_stop(&conn, cmd("1", 0, "002"), "herry", "job").unwrap();
        assert_eq!(out.old_route_version_id, 11);
        assert_eq!(out.new_route_version_id, 12);
        assert!(!out.pole_deleted);
        assert_eq!(out.stale_routes, vec![1]);

        let ts: String = conn
            .query_row(
                "SELECT ts FROM edit_log WHERE edit_id = ?1",
                [out.edit_id],
                |r| r.get(0),
            )
            .unwrap();
        // pointer moved, revision bumped, updated_at = the edit's ts
        let row: (i64, i64, String) = conn
            .query_row(
                "SELECT current_route_version_id, revision, updated_at
                 FROM route WHERE route_id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(row, (12, 2, ts));

        // new chain: dir 0 minus B (A → C → D renumbered), dir 1 untouched
        let rows: Vec<(i64, i64, i64)> = conn
            .prepare(
                "SELECT rsv.direction, rsv.stop_sequence, rsv.stop_version_id
                 FROM route_stop_version rsv
                 JOIN subroute_version sv ON sv.subroute_version_id = rsv.subroute_version_id
                 WHERE sv.route_version_id = 12
                 ORDER BY rsv.direction, rsv.stop_sequence",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                (0, 1, 10),
                (0, 2, 12),
                (0, 3, 13),
                (1, 1, 12),
                (1, 2, 11),
                (1, 3, 10)
            ]
        );

        // old version untouched: all 7 rows remain, the detached rsv (id 2,
        // pole B) still exists under the OLD route_version
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM route_stop_version rsv
                 JOIN subroute_version sv ON sv.subroute_version_id = rsv.subroute_version_id
                 WHERE sv.route_version_id = 11",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 7, "versioned history preserved");
        let (sv, svi): (i64, i64) = conn
            .query_row(
                "SELECT subroute_version_id, stop_version_id
                 FROM route_stop_version WHERE route_stop_version_id = 2",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((sv, svi), (11, 11), "detached rsv still under old rv");

        // stale marks: the four mark_line_stale datasets for (1, 0)
        let marks: Vec<(String, String, i64)> = conn
            .prepare(
                "SELECT table_name, key, cause_edit_id FROM stale_mark ORDER BY table_name, key",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            marks,
            vec![
                ("path_stop_projection".into(), "1".into(), out.edit_id),
                ("seg_match".into(), "1:0".into(), out.edit_id),
                ("station_bearing".into(), "*".into(), out.edit_id),
                ("stop_bearing".into(), "*".into(), out.edit_id),
            ]
        );

        // the audit row: entity/entity_id/action/version pair + exact
        // changes payload shape
        let (entity, entity_id, action, vb, va, changes): (
            String,
            i64,
            String,
            Option<i64>,
            Option<i64>,
            String,
        ) = conn
            .query_row(
                "SELECT entity, entity_id, action, version_before, version_after, changes
                 FROM edit_log WHERE edit_id = ?1",
                [out.edit_id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            (entity.as_str(), entity_id, action.as_str(), vb, va),
            ("stop_detach", 2, "detach", Some(11), Some(12))
        );
        let c: serde_json::Value = serde_json::from_str(&changes).unwrap();
        let mut keys: Vec<&str> = c.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "delete_pole",
                "direction",
                "kind",
                "new_route_version_id",
                "old_revision",
                "old_route_version_id",
                "old_updated_at",
                "pole_deleted",
                "sequences",
                "stop_code",
                "xno",
            ]
        );
        assert_eq!(c["kind"], "stop_detach");
        assert_eq!(c["xno"], "1");
        assert_eq!(c["direction"], 0);
        assert_eq!(c["stop_code"], "002");
        assert_eq!(c["sequences"], serde_json::json!([2]));
        assert_eq!(c["delete_pole"], false);
        assert_eq!(c["old_route_version_id"], 11);
        assert_eq!(c["new_route_version_id"], 12);
        assert_eq!(c["pole_deleted"], false);
        assert_eq!(c["old_revision"], 1);
        assert_eq!(c["old_updated_at"], T0);
    }

    // ---------- undo_detach_stop ----------

    #[test]
    fn undo_pointer_restore_is_byte_identical() {
        let conn = open_in_memory().unwrap();
        base_fixture(&conn);
        let b0 = dump_with(&conn, ROUNDTRIP_SKIP);
        let out = detach_stop(&conn, cmd("1", 0, "002"), "herry", "job").unwrap();
        let u = undo_detach_stop(&conn, out.edit_id, "herry", "revert").unwrap();
        assert_eq!(u.restored_route_version_id, 11);
        assert_eq!(
            dump_with(&conn, ROUNDTRIP_SKIP),
            b0,
            "apply then undo is byte-identical"
        );
        // the undo's own audit row (excluded from the dump)
        let (entity, action, undo_of, changes): (String, String, Option<i64>, String) = conn
            .query_row(
                "SELECT entity, action, undo_of, changes FROM edit_log WHERE undo_of = ?1",
                [out.edit_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!((entity.as_str(), action.as_str()), ("stop_detach", "undo"));
        assert_eq!(undo_of, Some(out.edit_id));
        let c: serde_json::Value = serde_json::from_str(&changes).unwrap();
        assert_eq!(c["kind"], "stop_detach_undo");
        // second undo → NotUndoable
        let err = undo_detach_stop(&conn, u.edit_id, "herry", "again").unwrap_err();
        assert!(
            matches!(
                err.downcast_ref::<EditError>(),
                Some(EditError::NotUndoable(_))
            ),
            "{err}"
        );
    }

    #[test]
    fn undo_refused_after_route_moved_on() {
        let conn = open_in_memory().unwrap();
        base_fixture(&conn);
        let first = detach_stop(&conn, cmd("1", 0, "002"), "herry", "job").unwrap();
        // an add-stop on the same route moves its pointer past the detach's
        // new rv — the detach is no longer the newest write
        add_stop(
            &conn,
            AddStopCmd {
                name: "X".into(),
                lat: 24.1407,
                lon: 120.6857,
                station_id: None,
                attachments: vec![AttachCmd {
                    xno: "1".into(),
                    direction: 0,
                    after_stop_code: Some("001".into()),
                }],
            },
            "herry",
            "grow",
        )
        .unwrap();
        let before = dump_with(&conn, ROUNDTRIP_SKIP);
        let err = undo_detach_stop(&conn, first.edit_id, "herry", "revert").unwrap_err();
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

    // ---------- delete_pole (one-way) ----------

    #[test]
    fn orphan_pole_refused_without_delete_pole() {
        let conn = open_in_memory().unwrap();
        base_fixture(&conn);
        // D serves only dir 0: the detach would orphan its pole
        let before = dump_with(&conn, ROUNDTRIP_SKIP);
        let err = detach_stop(&conn, cmd("1", 0, "004"), "herry", "job").unwrap_err();
        let msg = format!("{err}");
        assert!(
            matches!(
                err.downcast_ref::<EditError>(),
                Some(EditError::OrphanPoleRefused(_))
            ),
            "{err}"
        );
        assert!(msg.contains("--delete-pole"), "{msg}");
        assert!(msg.contains("one-way"), "{msg}");
        assert_eq!(
            dump_with(&conn, ROUNDTRIP_SKIP),
            before,
            "refused apply writes nothing"
        );
        // with the flag: the pole, its versions, and its station link go
        let out = detach_stop(
            &conn,
            DetachCmd {
                delete_pole: true,
                ..cmd("1", 0, "004")
            },
            "herry",
            "job",
        )
        .unwrap();
        assert!(out.pole_deleted);
        for (sql, want) in [
            ("SELECT COUNT(*) FROM stop WHERE stop_id = 4", 0i64),
            ("SELECT COUNT(*) FROM stop_version WHERE stop_id = 4", 0),
            (
                "SELECT COUNT(*) FROM station_version WHERE station_id = 2",
                0,
            ),
            ("SELECT COUNT(*) FROM station WHERE station_id = 2", 0),
            (
                "SELECT COUNT(*) FROM route_stop_version WHERE route_stop_version_id = 4",
                0,
            ),
            // the pole's history row went with it; the rest of the old
            // chain survives
            (
                "SELECT COUNT(*) FROM route_stop_version WHERE subroute_version_id = 11",
                6,
            ),
            // shared station 1 and its group survive
            (
                "SELECT COUNT(*) FROM station_version WHERE station_id = 1",
                1,
            ),
            (
                "SELECT COUNT(*) FROM station_group WHERE station_group_id = 1",
                1,
            ),
        ] {
            let n: i64 = conn.query_row(sql, [], |r| r.get(0)).unwrap();
            assert_eq!(n, want, "{sql}");
        }
        // the new chain serves A → B → C in dir 0 (D detached), dir 1 intact
        let rows: Vec<(i64, i64, i64)> = conn
            .prepare(
                "SELECT rsv.direction, rsv.stop_sequence, rsv.stop_version_id
                 FROM route_stop_version rsv
                 JOIN subroute_version sv ON sv.subroute_version_id = rsv.subroute_version_id
                 WHERE sv.route_version_id = ?1
                 ORDER BY rsv.direction, rsv.stop_sequence",
            )
            .unwrap()
            .query_map([out.new_route_version_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                (0, 1, 10),
                (0, 2, 11),
                (0, 3, 12),
                (1, 1, 12),
                (1, 2, 11),
                (1, 3, 10)
            ]
        );
    }

    #[test]
    fn delete_pole_undo_refused() {
        let conn = open_in_memory().unwrap();
        base_fixture(&conn);
        let out = detach_stop(
            &conn,
            DetachCmd {
                delete_pole: true,
                ..cmd("1", 0, "004")
            },
            "herry",
            "job",
        )
        .unwrap();
        // the pointer never moved on — the refusal is the one-way flag alone
        let err = undo_detach_stop(&conn, out.edit_id, "herry", "revert").unwrap_err();
        match err.downcast_ref::<EditError>() {
            Some(EditError::NotUndoable(id)) => assert_eq!(*id, out.edit_id),
            other => panic!("expected NotUndoable, got {other:?}"),
        }
    }

    #[test]
    fn undo_edit_dispatches_stop_detach() {
        let conn = open_in_memory().unwrap();
        base_fixture(&conn);
        let out = detach_stop(&conn, cmd("1", 0, "002"), "herry", "job").unwrap();
        let o = undo_edit(&conn, out.edit_id, "herry", "revert").unwrap();
        let UndoOutcome::StopDetach {
            edit_id,
            restored_route_version_id,
        } = o
        else {
            panic!("expected StopDetach, got {o:?}");
        };
        assert!(
            edit_id > out.edit_id,
            "edit_id is the undo's own audit row id"
        );
        assert_eq!(restored_route_version_id, 11);
    }
}
