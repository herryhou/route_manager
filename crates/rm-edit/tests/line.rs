//! Manual whole-line polyline override: load → plan → apply roundtrips
//! through the service, plus every plan guard.

use rm_db::open_in_memory;
use rm_edit::{edit_polyline_line, EditError, LineEditCmd};

/// route 1 (ext_uid 'tcg:1') + batch 1 + a 2-pt src polyline.
fn fixture(conn: &rusqlite::Connection) {
    conn.execute_batch(
        "INSERT INTO route (route_id, route_code, ext_uid, created_at, updated_at)
             VALUES (1, '1', 'tcg:1', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
         INSERT INTO import_batch (batch_id, source_dir, file_count, captured_at, valid_from, status, created_at)
             VALUES (1, 'assets/x', 1, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'CLOSED', '2026-01-01T00:00:00Z');
         INSERT INTO src_route_point (batch_id, xno, direction, encoded)
             VALUES (1, '1', 1, '_p~iF~ps|U_ulLnnqC');",
    )
    .unwrap();
}

fn cmd() -> LineEditCmd {
    LineEditCmd {
        xno: "1".into(),
        direction: 1,
        // exactly two points — the legal minimum
        points: vec![[38.5, -120.2], [40.7, -120.95]],
        expected_updated_at: None,
    }
}

#[test]
fn line_create_end_to_end() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let out = edit_polyline_line(&conn, cmd(), "herry", "fix").unwrap();
    assert!(out.created);
    let encoded: String = conn
        .query_row(
            "SELECT encoded FROM polyline_override WHERE xno = '1' AND direction = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(encoded, "_p~iF~ps|U_ulLnnqC");
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 4, "all four derived datasets marked stale");
}

#[test]
fn line_rejects_single_point_and_unknown_route() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let mut c = cmd();
    c.points = vec![[38.5, -120.2]];
    let err = edit_polyline_line(&conn, c, "h", "").unwrap_err();
    assert!(err.to_string().contains("≥2 points"), "{err}");
    let mut c = cmd();
    c.xno = "999".into();
    let err = edit_polyline_line(&conn, c, "h", "").unwrap_err();
    assert!(err.to_string().contains("does not exist"), "{err}");
}

#[test]
fn line_stale_expectation_refused() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let first = edit_polyline_line(&conn, cmd(), "herry", "a").unwrap();
    let mut second = cmd();
    second.expected_updated_at = Some("2020-01-01T00:00:00Z".into());
    let err = edit_polyline_line(&conn, second, "herry", "b").unwrap_err();
    assert!(err.to_string().contains("edited by someone else"), "{err}");
    let _ = first; // token proven stale above
}

#[test]
fn line_expectation_without_override_refused() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    // client carries a token but the override was undone away in between
    let mut c = cmd();
    c.expected_updated_at = Some("2026-01-01T00:00:00Z".into());
    let err = edit_polyline_line(&conn, c, "h", "").unwrap_err();
    assert!(err.to_string().contains("edited by someone else"), "{err}");
}

#[test]
fn line_identical_resave_is_noop() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let first = edit_polyline_line(&conn, cmd(), "herry", "a").unwrap();
    let mut again = cmd();
    again.expected_updated_at = Some(first.updated_at.clone());
    let err = edit_polyline_line(&conn, again, "herry", "b").unwrap_err();
    assert!(err.to_string().contains("sets no field"), "{err}");
}

#[test]
fn line_create_ok_without_src_polyline() {
    let conn = open_in_memory().unwrap();
    // route exists but no src_route_point row (capture missing this direction)
    conn.execute_batch(
        "INSERT INTO route (route_id, route_code, ext_uid, created_at, updated_at)
             VALUES (1, '1', 'tcg:1', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');",
    )
    .unwrap();
    let out = edit_polyline_line(&conn, cmd(), "herry", "draw").unwrap();
    assert!(out.created);
}

// ---------- undo planning ----------

use rm_edit::{plan_line_undo, undo_polyline_line};

fn line_row(action: &str, changes: serde_json::Value) -> rm_db::PolylineEditRow {
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
        "kind": "polyline_override", "xno": "1", "direction": 1,
        "encoded": { "new": "_p~iF~ps|U_ulLnnqC" }
    })
}

#[test]
fn plan_parses_create_into_delete() {
    let plan = plan_line_undo(&line_row("create", create_changes())).unwrap();
    assert_eq!(plan.restored.is_none(), true, "create undo deletes");
    assert_eq!(
        (plan.xno.as_str(), plan.direction, plan.route_id),
        ("1", 1, 1)
    );
    assert_eq!(plan.undone_ts, "2026-01-02T00:00:00Z");
}

#[test]
fn plan_parses_update_into_restore() {
    let mut c = create_changes();
    c["encoded"] = serde_json::json!({
        "old": "_ulLnnqC_p~iF~ps|U", "new": "_p~iF~ps|U_ulLnnqC"
    });
    let plan = plan_line_undo(&line_row("update", c)).unwrap();
    assert_eq!(plan.restored.as_deref(), Some("_ulLnnqC_p~iF~ps|U"));
}

#[test]
fn plan_rejects_undo_rows_foreign_kinds_and_broken_payloads() {
    // undo-of-undo
    let err = plan_line_undo(&line_row("undo", create_changes())).unwrap_err();
    assert!(matches!(err, EditError::NotUndoable(7)), "{err}");
    // foreign kind
    let mut c = create_changes();
    c["kind"] = serde_json::json!("station_move");
    let err = plan_line_undo(&line_row("update", c)).unwrap_err();
    assert!(matches!(err, EditError::NotUndoable(7)), "{err}");
    // REVIEW-FOCUS pin: legacy phase-1 piece rows refuse cleanly — history
    // stays, undo moves on
    let mut c = create_changes();
    c["kind"] = serde_json::json!("polyline_piece");
    let err = plan_line_undo(&line_row("update", c)).unwrap_err();
    assert!(matches!(err, EditError::NotUndoable(7)), "{err}");
    // missing xno / direction
    let mut c = create_changes();
    c["xno"] = serde_json::Value::Null;
    let err = plan_line_undo(&line_row("create", c)).unwrap_err();
    assert!(matches!(err, EditError::NotUndoable(7)), "{err}");
    // corrupt payload shapes: no encoded, half payload
    for bad in [
        serde_json::json!({ "kind": "polyline_override", "xno": "1", "direction": 1 }),
        serde_json::json!({ "kind": "polyline_override", "xno": "1", "direction": 1,
                            "encoded": { "old": "x" } }),
        serde_json::json!({ "kind": "polyline_override", "xno": "1", "direction": 1,
                            "encoded": { "old": null, "new": null } }),
    ] {
        let err = plan_line_undo(&line_row("update", bad)).unwrap_err();
        assert!(matches!(err, EditError::NotUndoable(7)), "{err}");
    }
}

#[test]
fn line_undo_end_to_end_create_then_undo_deletes() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let first = edit_polyline_line(&conn, cmd(), "herry", "a").unwrap();
    let out = undo_polyline_line(&conn, first.edit_id, "h", "rev").unwrap();
    assert!(out.deleted);
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM polyline_override", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    // undo of the undo → NotUndoable
    let err = undo_polyline_line(&conn, out.edit_id, "h", "again").unwrap_err();
    assert!(err.to_string().contains("cannot be undone"), "{err}");
}

#[test]
fn line_undo_end_to_end_update_then_undo_restores() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let first = edit_polyline_line(&conn, cmd(), "herry", "a").unwrap();
    let mut second = cmd();
    second.expected_updated_at = Some(first.updated_at.clone());
    second.points = vec![[38.5, -120.2], [39.6, -120.575], [40.7, -120.95]];
    edit_polyline_line(&conn, second, "herry", "b").unwrap();
    // undo the UPDATE (edit 2): newest non-undo polyline row for the route
    let newest: i64 = conn
        .query_row(
            "SELECT edit_id FROM edit_log
             WHERE entity = 'polyline' AND entity_id = 1 AND action != 'undo'
             ORDER BY edit_id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let out = undo_polyline_line(&conn, newest, "h", "rev").unwrap();
    assert!(!out.deleted);
    let enc: String = conn
        .query_row(
            "SELECT encoded FROM polyline_override WHERE xno = '1' AND direction = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(enc, "_p~iF~ps|U_ulLnnqC");
}

#[test]
fn line_undo_superseded_refused() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let first = edit_polyline_line(&conn, cmd(), "herry", "a").unwrap();
    let mut second = cmd();
    second.expected_updated_at = Some(first.updated_at.clone());
    second.points = vec![[38.5, -120.2], [39.6, -120.575], [40.7, -120.95]];
    edit_polyline_line(&conn, second, "herry", "b").unwrap();
    // now_rfc3339 is second-granular, so both edits usually land in the same
    // wall-clock second. Diverge the row directly to make "the line moved
    // on" deterministic.
    conn.execute(
        "UPDATE polyline_override SET updated_at = '2030-01-01T00:00:00Z'
         WHERE xno = '1' AND direction = 1",
        [],
    )
    .unwrap();
    // undo of the FIRST edit: line moved on → 409-shaped refusal
    let err = undo_polyline_line(&conn, first.edit_id, "h", "rev").unwrap_err();
    assert!(err.to_string().contains("edited by someone else"), "{err}");
}

#[test]
fn line_undo_unknown_or_foreign_edit_refused() {
    let conn = open_in_memory().unwrap();
    fixture(&conn);
    let err = undo_polyline_line(&conn, 999, "h", "").unwrap_err();
    assert!(err.to_string().contains("cannot be undone"), "{err}");
}
