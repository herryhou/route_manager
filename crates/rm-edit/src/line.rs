//! Manual whole-line polyline override (phase 2). Pure planning over the
//! loaded snapshot; persistence is rm-db's. An edit replaces the entire
//! direction polyline: effective = override ?? src. Validation = decode
//! round-trip + ≥2 points — no anchors, no seams, nothing to splice.

use anyhow::Result;
use rm_core::LatLng;
use rusqlite::Connection;

use crate::{map_db_err, EditError};

#[derive(Debug, Clone)]
pub struct LineEditCmd {
    pub xno: String,
    pub direction: i64,
    /// `[lat, lon]` pairs, draw order.
    pub points: Vec<[f64; 2]>,
    pub expected_updated_at: Option<String>,
}

#[derive(Debug)]
pub struct LineEditPlan {
    pub encoded: String,
    pub expected_updated_at: Option<String>,
}

fn latlng(pt: [f64; 2]) -> Result<LatLng, EditError> {
    let (lat, lon) = (pt[0], pt[1]);
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Err(EditError::InvalidLine("coordinate out of range".into()));
    }
    Ok(LatLng::new(lat, lon))
}

/// Pure planning policy. Guards, in order: coordinates valid; ≥2 points
/// (consecutive duplicates dropped, decode's rule); currency token matches
/// the current override — or is refused when the override is gone; an
/// identical re-save is a no-op. No I/O.
pub fn plan_line_edit(
    snap: &rm_db::LineSnapshot,
    cmd: LineEditCmd,
) -> Result<LineEditPlan, EditError> {
    let mut pts: Vec<LatLng> = Vec::with_capacity(cmd.points.len());
    for p in &cmd.points {
        let ll = latlng(*p)?;
        if pts.last() != Some(&ll) {
            pts.push(ll);
        }
    }
    if pts.len() < 2 {
        return Err(EditError::InvalidLine("line needs ≥2 points".into()));
    }
    let encoded = rm_core::polyline::encode(&pts);

    match &snap.override_row {
        Some(row) => {
            if Some(row.updated_at.as_str()) != cmd.expected_updated_at.as_deref() {
                return Err(EditError::LineNotCurrent(cmd.xno.clone(), cmd.direction));
            }
            if row.encoded == encoded {
                return Err(EditError::NoOpEdit);
            }
        }
        // token carried but the override was undone away in between
        None if cmd.expected_updated_at.is_some() => {
            return Err(EditError::LineNotCurrent(cmd.xno.clone(), cmd.direction));
        }
        None => {}
    }

    Ok(LineEditPlan {
        encoded,
        expected_updated_at: cmd.expected_updated_at,
    })
}

#[derive(Debug)]
pub struct LineOutcome {
    pub edit_id: i64,
    pub updated_at: String,
    pub created: bool,
}

/// Application service: load → plan (pure) → apply (one tx).
pub fn edit_polyline_line(
    conn: &Connection,
    cmd: LineEditCmd,
    editor: &str,
    note: &str,
) -> Result<LineOutcome> {
    let snap = rm_db::load_polyline_line(conn, &cmd.xno, cmd.direction)?
        .ok_or_else(|| EditError::UnknownRoute(cmd.xno.clone()))?;
    let plan = plan_line_edit(&snap, cmd.clone())?;
    let applied = rm_db::apply_line_edit(
        conn,
        &rm_db::LineEditOp {
            xno: cmd.xno,
            direction: cmd.direction,
            encoded: plan.encoded,
            expected_updated_at: plan.expected_updated_at,
        },
        editor,
        note,
    )
    .map_err(map_db_err)?;
    Ok(LineOutcome {
        edit_id: applied.edit_id,
        updated_at: applied.updated_at,
        created: applied.created,
    })
}

#[derive(Debug)]
pub struct LineUndoPlan {
    pub edit_id: i64,
    pub route_id: i64,
    pub xno: String,
    pub direction: i64,
    pub undone_ts: String,
    pub restored: Option<String>,
}

/// Pure guard + inversion from the logged payload: only a non-undo
/// polyline row with `kind = 'polyline_override'` and a parseable xno +
/// direction is invertible. The `encoded.old` branch decides
/// restore-vs-delete: `old` present → restore that state; `old` absent (a
/// create row) → delete the override. Missing/half payloads and legacy
/// `polyline_piece` rows are corrupt-for-this-purpose → NotUndoable.
pub fn plan_line_undo(edit: &rm_db::PolylineEditRow) -> Result<LineUndoPlan, EditError> {
    if edit.action == "undo"
        || edit.changes.get("kind").and_then(|k| k.as_str()) != Some("polyline_override")
    {
        return Err(EditError::NotUndoable(edit.edit_id));
    }
    let xno = edit
        .changes
        .get("xno")
        .and_then(|v| v.as_str())
        .ok_or(EditError::NotUndoable(edit.edit_id))?
        .to_string();
    let direction = edit
        .changes
        .get("direction")
        .and_then(|v| v.as_i64())
        .ok_or(EditError::NotUndoable(edit.edit_id))?;
    let encoded = edit
        .changes
        .get("encoded")
        .ok_or(EditError::NotUndoable(edit.edit_id))?;
    let old = encoded.get("old").and_then(|v| v.as_str());
    let new = encoded.get("new").and_then(|v| v.as_str());
    let restored = match (old, new) {
        // every undoable row has a `new` (the state it put in place)
        (None, Some(_)) => None, // create → undo deletes the override
        (Some(encoded), Some(_)) => Some(encoded.to_string()), // update → restore prior
        _ => return Err(EditError::NotUndoable(edit.edit_id)),
    };

    Ok(LineUndoPlan {
        edit_id: edit.edit_id,
        route_id: edit.entity_id,
        xno,
        direction,
        undone_ts: edit.ts.clone(),
        restored,
    })
}

#[derive(Debug)]
pub struct LineUndoOutcome {
    pub edit_id: i64,
    pub deleted: bool,
}

/// Application service: load → plan (pure) → apply (one tx). Forward-write
/// undo — no version chain; the apply's ts currency guard is the authority.
pub fn undo_polyline_line(
    conn: &Connection,
    edit_id: i64,
    editor: &str,
    note: &str,
) -> Result<LineUndoOutcome> {
    let row = rm_db::load_polyline_edit(conn, edit_id)?.ok_or(EditError::NotUndoable(edit_id))?;
    let plan = plan_line_undo(&row)?;
    let undone = rm_db::apply_line_undo(
        conn,
        &rm_db::LineUndoOp {
            edit_id: plan.edit_id,
            route_id: plan.route_id,
            xno: plan.xno,
            direction: plan.direction,
            undone_ts: plan.undone_ts,
            restored: plan.restored,
        },
        editor,
        note,
    )
    .map_err(map_db_err)?;
    Ok(LineUndoOutcome {
        edit_id: undone.edit_id,
        deleted: undone.deleted,
    })
}
