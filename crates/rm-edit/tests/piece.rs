//! Manual polyline piece edit: load → plan → apply roundtrips through the
//! service, plus every plan guard.

use rm_db::open_in_memory;
use rm_edit::{edit_polyline_piece, EditError, PieceEditCmd};

/// route 1 (ext_uid 'tcg:1') + batch 1 + a caller-supplied src polyline.
fn fixture_src(conn: &rusqlite::Connection, src_encoded: &str) {
    conn.execute_batch(&format!(
        "INSERT INTO route (route_id, route_code, ext_uid, created_at, updated_at)
             VALUES (1, '1', 'tcg:1', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
         INSERT INTO import_batch (batch_id, source_dir, file_count, captured_at, valid_from, status, created_at)
             VALUES (1, 'assets/x', 1, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'CLOSED', '2026-01-01T00:00:00Z');
         INSERT INTO src_route_point (batch_id, xno, direction, encoded)
             VALUES (1, '1', 1, '{src_encoded}');",
    ))
    .unwrap();
}

/// Default fixture: 2-pt src line (38.5,-120.2)→(40.7,-120.95).
fn fixture(conn: &rusqlite::Connection) {
    fixture_src(conn, "_p~iF~ps|U_ulLnnqC");
}

fn cmd() -> PieceEditCmd {
    PieceEditCmd {
        xno: "1".into(),
        direction: 1,
        piece_id: None,
        // same two anchor points — a trivial "replace the whole 2-pt line"
        points: vec![[38.5, -120.2], [40.7, -120.95]],
        anchor_start: [38.5, -120.2],
        anchor_end: [40.7, -120.95],
        expected_updated_at: None,
    }
}

#[test]
fn piece_create_end_to_end() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let out = edit_polyline_piece(&conn, cmd(), "herry", "fix").unwrap();
    assert!(out.created);
    let encoded: String = conn
        .query_row("SELECT encoded FROM polyline_piece WHERE piece_id = 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(encoded, "_p~iF~ps|U_ulLnnqC");
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0);
}

#[test]
fn piece_rejects_single_point_and_unknown_route() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let mut c = cmd();
    c.points = vec![[38.5, -120.2]];
    let err = edit_polyline_piece(&conn, c, "h", "").unwrap_err();
    assert!(err.to_string().contains("invalid polyline piece"), "{err}");
    let mut c = cmd();
    c.xno = "999".into();
    let err = edit_polyline_piece(&conn, c, "h", "").unwrap_err();
    assert!(err.to_string().contains("does not exist"), "{err}");
}

#[test]
fn piece_rejects_anchors_off_the_src_line() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let mut c = cmd();
    c.anchor_end = [41.7, -120.95]; // ~111km off the 2-pt src line
    c.points = vec![[38.5, -120.2], [41.7, -120.95]];
    let err = edit_polyline_piece(&conn, c, "h", "").unwrap_err();
    assert!(err.to_string().contains("anchors must lie on"), "{err}");
}

#[test]
fn piece_rejects_reversed_and_coincident_anchors() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let mut c = cmd();
    c.anchor_start = [40.7, -120.95];
    c.anchor_end = [38.5, -120.2];
    let err = edit_polyline_piece(&conn, c, "h", "").unwrap_err();
    assert!(err.to_string().contains("precede"), "{err}");
    let mut c = cmd();
    c.anchor_end = c.anchor_start;
    let err = edit_polyline_piece(&conn, c, "h", "").unwrap_err();
    assert!(err.to_string().contains("precede"), "{err}");
}

#[test]
fn piece_rejects_seam_gap_between_piece_and_anchors() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let mut c = cmd();
    // piece starts ~1.1km away from anchor_start (0.01° lat) — seam broken
    c.points = vec![[38.51, -120.2], [40.7, -120.95]];
    let err = edit_polyline_piece(&conn, c, "h", "").unwrap_err();
    assert!(err.to_string().contains("endpoints must equal"), "{err}");
}

#[test]
fn piece_adjacent_spans_allowed_overlap_rejected() {
    let conn = open_in_memory().unwrap();
    // 3-pt src A→B→C so two spans can sit side by side sharing anchor B.
    let src = rm_core::polyline::encode(&[
        rm_core::LatLng::new(38.5, -120.2),
        rm_core::LatLng::new(39.6, -120.575),
        rm_core::LatLng::new(40.7, -120.95),
    ]);
    fixture_src(&conn, &src);
    let a = [38.5, -120.2];
    let b = [39.6, -120.575];
    let c = [40.7, -120.95];
    // piece 1 = A→B
    let mut p1 = cmd();
    p1.anchor_end = b;
    p1.points = vec![a, b];
    let out1 = edit_polyline_piece(&conn, p1, "herry", "1").unwrap();
    // piece 2 = B→C — shares anchor B with piece 1 = ADJACENT = allowed
    let mut p2 = cmd();
    p2.anchor_start = b;
    p2.points = vec![b, c];
    edit_polyline_piece(&conn, p2, "herry", "2").unwrap();
    // piece 3 = A→B again — strictly inside piece 1's interval = overlap 409
    let mut p3 = cmd();
    p3.anchor_end = b;
    p3.points = vec![a, b];
    let err = edit_polyline_piece(&conn, p3, "herry", "3").unwrap_err();
    assert!(err.to_string().contains("overlaps"), "{err}");
    // re-edit piece 1 (own id skipped by the overlap scan) with an extra
    // vertex — still legal against adjacent piece 2
    let mut again = cmd();
    again.piece_id = Some(out1.piece_id);
    again.expected_updated_at = Some(out1.updated_at.clone());
    again.anchor_end = b;
    again.points = vec![a, [39.55, -120.56], b];
    let out = edit_polyline_piece(&conn, again, "herry", "4").unwrap();
    assert!(!out.created);
}

#[test]
fn piece_stale_expectation_refused() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let first = edit_polyline_piece(&conn, cmd(), "herry", "a").unwrap();
    let mut second = cmd();
    second.piece_id = Some(first.piece_id);
    second.expected_updated_at = Some("2020-01-01T00:00:00Z".into());
    let err = edit_polyline_piece(&conn, second, "herry", "b").unwrap_err();
    assert!(err.to_string().contains("edited by someone else"), "{err}");
}

#[test]
fn piece_identical_resave_is_noop() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let first = edit_polyline_piece(&conn, cmd(), "herry", "a").unwrap();
    let mut again = cmd();
    again.piece_id = Some(first.piece_id);
    again.expected_updated_at = Some(first.updated_at.clone());
    let err = edit_polyline_piece(&conn, again, "herry", "b").unwrap_err();
    assert!(err.to_string().contains("sets no field"), "{err}");
}

#[test]
fn piece_create_ok_without_src_polyline() {
    let conn = open_in_memory().unwrap();
    // route exists but no src_route_point row (capture missing this direction)
    conn.execute_batch(
        "INSERT INTO route (route_id, route_code, ext_uid, created_at, updated_at)
             VALUES (1, '1', 'tcg:1', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');",
    )
    .unwrap();
    let mut c = cmd();
    // anchors far from anything — no src line, so anchor checks skip
    c.anchor_start = [0.0, 0.0];
    c.anchor_end = [1.0, 1.0];
    let out = edit_polyline_piece(&conn, c, "herry", "draw").unwrap();
    assert!(out.created);
}

use rm_edit::{plan_piece_undo, undo_polyline_piece};

fn polyline_row(action: &str, changes: serde_json::Value) -> rm_db::PolylineEditRow {
    rm_db::PolylineEditRow {
        edit_id: 7,
        entity_id: 1,
        action: action.into(),
        ts: "2026-01-02T00:00:00Z".into(),
        changes,
    }
}

fn create_changes() -> serde_json::Value {
    serde_json::json!({
        "kind": "polyline_piece", "xno": "1", "direction": 1, "piece_id": 3,
        "encoded": { "new": "_p~iF~ps|U_ulLnnqC" },
        "anchors": { "new": [[38.5, -120.2], [40.7, -120.95]] }
    })
}

#[test]
fn plan_parses_create_into_delete() {
    let plan = plan_piece_undo(&polyline_row("create", create_changes())).unwrap();
    assert_eq!((plan.piece_id, plan.restored.is_none()), (3, true));
    assert_eq!((plan.xno.as_str(), plan.direction, plan.route_id), ("1", 1, 1));
}

#[test]
fn plan_parses_update_into_restore() {
    let mut c = create_changes();
    c["action_hint"] = serde_json::json!("ignored");
    c["encoded"] = serde_json::json!({
        "old": "_ulLnnqC_p~iF~ps|U", "new": "_p~iF~ps|U_ulLnnqC"
    });
    c["anchors"] = serde_json::json!({
        "old": [[37.5, -121.2], [39.7, -121.95]],
        "new": [[38.5, -120.2], [40.7, -120.95]]
    });
    let plan = plan_piece_undo(&polyline_row("update", c)).unwrap();
    let r = plan.restored.expect("restore state");
    assert_eq!(r.encoded, "_ulLnnqC_p~iF~ps|U");
    assert_eq!((r.anchor_start_lat, r.anchor_end_lon), (37.5, -121.95));
}

#[test]
fn plan_rejects_undo_rows_foreign_kinds_and_broken_payloads() {
    // undo-of-undo
    let err = plan_piece_undo(&polyline_row("undo", create_changes())).unwrap_err();
    assert!(matches!(err, EditError::NotUndoable(7)), "{err}");
    // foreign kind
    let mut c = create_changes();
    c["kind"] = serde_json::json!("station_move");
    let err = plan_piece_undo(&polyline_row("update", c)).unwrap_err();
    assert!(matches!(err, EditError::NotUndoable(7)), "{err}");
    // missing piece_id
    let mut c = create_changes();
    c["piece_id"] = serde_json::Value::Null;
    let err = plan_piece_undo(&polyline_row("create", c)).unwrap_err();
    assert!(matches!(err, EditError::NotUndoable(7)), "{err}");
    // half payload: encoded.old without anchors.old
    let mut c = create_changes();
    c["encoded"] = serde_json::json!({ "old": "x", "new": "y" });
    let err = plan_piece_undo(&polyline_row("update", c)).unwrap_err();
    assert!(matches!(err, EditError::NotUndoable(7)), "{err}");
    // short anchor array: corrupt → refuse, not panic
    let mut c = create_changes();
    c["encoded"] = serde_json::json!({ "old": "x", "new": "y" });
    c["anchors"] = serde_json::json!({ "old": [[38.5]], "new": [[38.5, -120.2], [40.7, -120.95]] });
    let err = plan_piece_undo(&polyline_row("update", c)).unwrap_err();
    assert!(matches!(err, EditError::NotUndoable(7)), "{err}");
}

#[test]
fn piece_undo_end_to_end_create_then_undo_deletes() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let first = edit_polyline_piece(&conn, cmd(), "herry", "a").unwrap();
    let out = undo_polyline_piece(&conn, first.edit_id, "h", "rev").unwrap();
    assert!(out.deleted);
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM polyline_piece", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    // undo of the undo → NotUndoable
    let err = undo_polyline_piece(&conn, out.edit_id, "h", "again").unwrap_err();
    assert!(err.to_string().contains("cannot be undone"), "{err}");
}

#[test]
fn piece_undo_end_to_end_update_then_undo_restores() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let first = edit_polyline_piece(&conn, cmd(), "herry", "a").unwrap();
    let mut second = cmd();
    second.piece_id = Some(first.piece_id);
    second.expected_updated_at = Some(first.updated_at.clone());
    second.points = vec![[38.5, -120.2], [39.6, -120.575], [40.7, -120.95]];
    edit_polyline_piece(&conn, second, "herry", "b").unwrap();
    // undo the UPDATE (edit 2): newest non-undo polyline row for the route
    let newest: i64 = conn.query_row(
        "SELECT edit_id FROM edit_log
         WHERE entity = 'polyline' AND entity_id = 1 AND action != 'undo'
         ORDER BY edit_id DESC LIMIT 1",
        [], |r| r.get(0)).unwrap();
    let out = undo_polyline_piece(&conn, newest, "h", "rev").unwrap();
    assert!(!out.deleted);
    let enc: String = conn
        .query_row("SELECT encoded FROM polyline_piece WHERE piece_id = ?1", [first.piece_id], |r| r.get(0))
        .unwrap();
    assert_eq!(enc, "_p~iF~ps|U_ulLnnqC");
}

#[test]
fn piece_undo_superseded_refused() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let first = edit_polyline_piece(&conn, cmd(), "herry", "a").unwrap();
    let mut second = cmd();
    second.piece_id = Some(first.piece_id);
    second.expected_updated_at = Some(first.updated_at.clone());
    second.points = vec![[38.5, -120.2], [39.6, -120.575], [40.7, -120.95]];
    edit_polyline_piece(&conn, second, "herry", "b").unwrap();
    // now_rfc3339 is second-granular, so both edits usually land in the same
    // wall-clock second and undone_ts == the row's updated_at. Diverge the row
    // directly to make "the piece moved on" deterministic.
    conn.execute(
        "UPDATE polyline_piece SET updated_at = '2030-01-01T00:00:00Z' WHERE piece_id = ?1",
        [first.piece_id],
    ).unwrap();
    // undo of the FIRST edit: piece moved on → 409-shaped refusal
    let err = undo_polyline_piece(&conn, first.edit_id, "h", "rev").unwrap_err();
    assert!(err.to_string().contains("edited by someone else"), "{err}");
}

#[test]
fn piece_undo_unknown_or_foreign_edit_refused() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let err = undo_polyline_piece(&conn, 999, "h", "").unwrap_err();
    assert!(err.to_string().contains("cannot be undone"), "{err}");
}
