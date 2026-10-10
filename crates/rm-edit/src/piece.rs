//! Manual src-polyline piece edit (phase 1: storage only). Pure planning
//! over the loaded snapshot; persistence is rm-db's. A piece replaces a
//! span of the src polyline, anchored by coordinates (never vertex indices
//! — indices shift under edits).

use anyhow::Result;
use rm_core::LatLng;
use rusqlite::Connection;

use crate::{map_db_err, EditError};

/// Seam tolerance: anchors must sit on the src line and the piece's
/// endpoints must sit on the anchors within this distance. Encoded polylines
/// quantize at 1e-5 (~1.1 m), so 5 m is generous for real picks.
pub const SEAM_TOL_M: f64 = 5.0;

#[derive(Debug, Clone)]
pub struct PieceEditCmd {
    pub xno: String,
    pub direction: i64,
    pub piece_id: Option<i64>,
    /// `[lat, lon]` pairs, draw order.
    pub points: Vec<[f64; 2]>,
    pub anchor_start: [f64; 2],
    pub anchor_end: [f64; 2],
    pub expected_updated_at: Option<String>,
}

#[derive(Debug)]
pub struct PieceEditPlan {
    pub piece_id: Option<i64>,
    pub encoded: String,
    pub anchor_start: [f64; 2],
    pub anchor_end: [f64; 2],
    pub expected_updated_at: Option<String>,
}

/// Chainage of `p` along `path` (its nearest point on the polyline) plus the
/// off-path distance. `None` for a degenerate (<2 pt) path.
fn chainage_along(path: &[LatLng], p: LatLng) -> Option<(f64, f64)> {
    if path.len() < 2 {
        return None;
    }
    let mut best: Option<(f64, f64)> = None;
    let mut acc = 0.0;
    for w in path.windows(2) {
        let (proj, d) = rm_core::geo::point_to_segment_m(p, w[0], w[1]);
        if best.map_or(true, |(_, bd)| d < bd) {
            let c = acc + rm_core::geo::haversine_m(w[0], proj);
            best = Some((c, d));
        }
        acc += rm_core::geo::haversine_m(w[0], w[1]);
    }
    best
}

fn latlng(pt: [f64; 2]) -> Result<LatLng, EditError> {
    let (lat, lon) = (pt[0], pt[1]);
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return Err(EditError::InvalidPiece("coordinate out of range".into()));
    }
    Ok(LatLng::new(lat, lon))
}

/// Pure planning policy. Guards, in order: coordinates valid; ≥2 points
/// (consecutive duplicates dropped, decode's rule); anchors on the src line
/// within `SEAM_TOL_M` (skipped when the direction has no src polyline);
/// start-anchor chainage strictly precedes end-anchor; piece endpoints land
/// on the anchors; span does not overlap another current piece's span
/// (shared endpoint = adjacent = legal); currency token matches; identical
/// re-save is a no-op. No I/O.
pub fn plan_piece_edit(
    snap: &rm_db::PolylineSnapshot,
    cmd: PieceEditCmd,
) -> Result<PieceEditPlan, EditError> {
    let mut pts: Vec<LatLng> = Vec::with_capacity(cmd.points.len());
    for p in &cmd.points {
        let ll = latlng(*p)?;
        if pts.last() != Some(&ll) {
            pts.push(ll);
        }
    }
    if pts.len() < 2 {
        return Err(EditError::InvalidPiece("piece needs ≥2 points".into()));
    }
    let encoded = rm_core::polyline::encode(&pts);
    let a0 = latlng(cmd.anchor_start)?;
    let a1 = latlng(cmd.anchor_end)?;

    let src: Option<Vec<LatLng>> = match snap.src_encoded.as_deref() {
        Some(s) if !s.is_empty() => Some(
            rm_core::polyline::decode(s)
                .map_err(|e| EditError::InvalidPiece(format!("src polyline: {e}")))?,
        ),
        _ => None,
    };

    if let Some(path) = &src {
        let (c0, d0) = chainage_along(path, a0)
            .expect("src decoded with ≥2 points");
        let (c1, d1) = chainage_along(path, a1)
            .expect("src decoded with ≥2 points");
        if d0 > SEAM_TOL_M || d1 > SEAM_TOL_M {
            return Err(EditError::InvalidPiece(
                "anchors must lie on the src polyline".into(),
            ));
        }
        if c0 >= c1 {
            return Err(EditError::InvalidPiece(
                "anchor_start must precede anchor_end along the src line".into(),
            ));
        }
        // Seam continuity: the piece must start and end at its anchors.
        let (_, dp0) = chainage_along(&pts, a0).unwrap_or((0.0, f64::INFINITY));
        let (_, dp1) = chainage_along(&pts, a1).unwrap_or((0.0, f64::INFINITY));
        if dp0 > SEAM_TOL_M || dp1 > SEAM_TOL_M {
            return Err(EditError::InvalidPiece(
                "piece endpoints must equal the anchors".into(),
            ));
        }
        // Span overlap vs the direction's other pieces (shared endpoint =
        // adjacent = legal, hence strict <).
        let mine = cmd.piece_id.unwrap_or(-1);
        for other in &snap.pieces {
            if other.piece_id == mine {
                continue;
            }
            let os = LatLng::new(other.anchor_start_lat, other.anchor_start_lon);
            let oe = LatLng::new(other.anchor_end_lat, other.anchor_end_lon);
            let Some((ocs, _)) = chainage_along(path, os) else {
                continue;
            };
            let Some((oce, _)) = chainage_along(path, oe) else {
                continue;
            };
            if c0.max(ocs) < c1.min(oce) {
                return Err(EditError::PieceOverlap(other.piece_id));
            }
        }
    }

    if let Some(pid) = cmd.piece_id {
        match snap.pieces.iter().find(|p| p.piece_id == pid) {
            Some(row) => {
                if Some(row.updated_at.as_str()) != cmd.expected_updated_at.as_deref() {
                    return Err(EditError::PieceNotCurrent(pid));
                }
                let same = row.encoded == encoded
                    && row.anchor_start_lat == cmd.anchor_start[0]
                    && row.anchor_start_lon == cmd.anchor_start[1]
                    && row.anchor_end_lat == cmd.anchor_end[0]
                    && row.anchor_end_lon == cmd.anchor_end[1];
                if same {
                    return Err(EditError::NoOpEdit);
                }
            }
            None => {
                if cmd.expected_updated_at.is_some() {
                    return Err(EditError::PieceNotCurrent(pid));
                }
                // recreate-after-undo: rm-db's apply re-checks scope in-tx
            }
        }
    }

    Ok(PieceEditPlan {
        piece_id: cmd.piece_id,
        encoded,
        anchor_start: cmd.anchor_start,
        anchor_end: cmd.anchor_end,
        expected_updated_at: cmd.expected_updated_at,
    })
}

#[derive(Debug)]
pub struct PieceOutcome {
    pub edit_id: i64,
    pub piece_id: i64,
    pub updated_at: String,
    pub created: bool,
}

/// Application service: load → plan (pure) → apply (one tx).
pub fn edit_polyline_piece(
    conn: &Connection,
    cmd: PieceEditCmd,
    editor: &str,
    note: &str,
) -> Result<PieceOutcome> {
    let snap = rm_db::load_polyline_snapshot(conn, &cmd.xno, cmd.direction)?
        .ok_or_else(|| EditError::UnknownRoute(cmd.xno.clone()))?;
    let plan = plan_piece_edit(&snap, cmd.clone())?;
    let applied = rm_db::apply_piece_edit(
        conn,
        &rm_db::PieceEditOp {
            xno: cmd.xno,
            direction: cmd.direction,
            piece_id: plan.piece_id,
            encoded: plan.encoded,
            anchor_start_lat: plan.anchor_start[0],
            anchor_start_lon: plan.anchor_start[1],
            anchor_end_lat: plan.anchor_end[0],
            anchor_end_lon: plan.anchor_end[1],
            expected_updated_at: plan.expected_updated_at,
        },
        editor,
        note,
    )
    .map_err(map_db_err)?;
    Ok(PieceOutcome {
        edit_id: applied.edit_id,
        piece_id: applied.piece_id,
        updated_at: applied.updated_at,
        created: applied.created,
    })
}

#[derive(Debug)]
pub struct PieceUndoPlan {
    pub edit_id: i64,
    pub route_id: i64,
    pub xno: String,
    pub direction: i64,
    pub piece_id: i64,
    pub undone_ts: String,
    pub restored: Option<rm_db::PieceRestoreState>,
}

fn anchor_pair(v: Option<&serde_json::Value>) -> Option<([f64; 2], [f64; 2])> {
    let v = v?;
    let a = v.get(0)?.as_array()?;
    let b = v.get(1)?.as_array()?;
    Some((
        [a.get(0)?.as_f64()?, a.get(1)?.as_f64()?],
        [b.get(0)?.as_f64()?, b.get(1)?.as_f64()?],
    ))
}

/// Pure guard + inversion from the logged payload: only a non-undo
/// polyline row with `kind = 'polyline_piece'` and a parseable piece_id is
/// invertible. The `old` branches decide restore-vs-delete: both `old`s
/// present → restore that state; both absent (a create row) → delete.
/// Half-payload rows are corrupt → NotUndoable.
pub fn plan_piece_undo(edit: &rm_db::PolylineEditRow) -> Result<PieceUndoPlan, EditError> {
    if edit.action == "undo" || edit.changes.get("kind").and_then(|k| k.as_str()) != Some("polyline_piece")
    {
        return Err(EditError::NotUndoable(edit.edit_id));
    }
    let piece_id = edit
        .changes
        .get("piece_id")
        .and_then(|v| v.as_i64())
        .ok_or(EditError::NotUndoable(edit.edit_id))?;
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

    let old_encoded = edit
        .changes
        .get("encoded")
        .and_then(|e| e.get("old"))
        .and_then(|v| v.as_str());
    let old_anchors = anchor_pair(edit.changes.get("anchors").and_then(|a| a.get("old")));
    let restored = match (old_encoded, old_anchors) {
        (Some(encoded), Some((s, e))) => Some(rm_db::PieceRestoreState {
            encoded: encoded.to_string(),
            anchor_start_lat: s[0],
            anchor_start_lon: s[1],
            anchor_end_lat: e[0],
            anchor_end_lon: e[1],
        }),
        (None, None) => None,
        _ => return Err(EditError::NotUndoable(edit.edit_id)),
    };

    Ok(PieceUndoPlan {
        edit_id: edit.edit_id,
        route_id: edit.entity_id,
        xno,
        direction,
        piece_id,
        undone_ts: edit.ts.clone(),
        restored,
    })
}

#[derive(Debug)]
pub struct PieceUndoOutcome {
    pub edit_id: i64,
    pub piece_id: i64,
    pub deleted: bool,
}

/// Application service: load → plan (pure) → apply (one tx). Forward-write
/// undo — no version chain; the apply's ts currency guard is the authority.
pub fn undo_polyline_piece(
    conn: &Connection,
    edit_id: i64,
    editor: &str,
    note: &str,
) -> Result<PieceUndoOutcome> {
    let row = rm_db::load_polyline_edit(conn, edit_id)?.ok_or(EditError::NotUndoable(edit_id))?;
    let plan = plan_piece_undo(&row)?;
    let undone = rm_db::apply_piece_undo(
        conn,
        &rm_db::PieceUndoOp {
            edit_id: plan.edit_id,
            route_id: plan.route_id,
            xno: plan.xno,
            direction: plan.direction,
            piece_id: plan.piece_id,
            undone_ts: plan.undone_ts,
            restored: plan.restored,
        },
        editor,
        note,
    )
    .map_err(map_db_err)?;
    Ok(PieceUndoOutcome {
        edit_id: undone.edit_id,
        piece_id: undone.piece_id,
        deleted: undone.deleted,
    })
}
