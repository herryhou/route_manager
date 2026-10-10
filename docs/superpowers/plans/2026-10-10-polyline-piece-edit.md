# Polyline Piece Manual Edit (Phase 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Editors fix bad captured polylines by hand: vertex-level editing of a src-polyline span in the rm-web map, stored as unversioned manual pieces with edit_log audit + forward-write undo. Storage only — readers and stale marks untouched (phase 2 is a future spec).

**Architecture:** New unversioned table `polyline_piece` (one row per piece = current, `updated_at` is the optimistic-concurrency token). New `rm-edit` module `piece.rs` (pure planning: decode/≥2-pt/anchor-on-src/seam/overlap/currency guards) + services, new `rm-db` apply fns (one tx each, currency guard before any write, edit_log row carries old+new encoded so undo is a forward write). rm-web gains one POST endpoint, an undo-dispatch branch, `manual_pieces` on the raw endpoint, and a new `pieceEdit.js` Leaflet interaction.

**Tech Stack:** Rust (rusqlite bundled, axum, thiserror, serde_json), rm-core polyline encode/decode + geo helpers, vanilla-JS Leaflet (vendored).

**Spec:** `docs/superpowers/specs/2026-10-10-polyline-piece-edit-design.md`

## Global Constraints

- Do NOT run `cargo fmt` (baseline ~130 fmt-diff sites; fmt is a dedicated-commit policy).
- Tests are plain `cargo test -p <crate>` (in-memory DBs, fast in debug). Never `cargo run` pipeline/web binaries without `-r`.
- Pre-existing clippy warnings ×6 are out of scope; do not fix unrelated ones.
- `rm-edit` stays zero-SQL; all SQL lives in rm-db / rm-web queries.
- `edit_log` rows: `entity='polyline'`, `entity_id=route_id` (resolved via `ext_uid = 'tcg:' || xno`), `action` ∈ `create|update|undo`, `version_before`/`version_after` stay NULL.
- `changes` JSON shape (pinned by spec): `{"kind":"polyline_piece","xno":…,"direction":…,"piece_id":…,"encoded":{"old"?:…,"new":…},"anchors":{"old"?:[[lat,lon],[lat,lon]],"new":[[lat,lon],[lat,lon]]}}` — `old` absent on create; undo rows carry the SAME shape with the restored state as `new` (deliberate deviation from the `'{}'` undo convention: the undo payload decides restore-vs-delete).
- ZERO stale_mark writes anywhere (phase 1).
- rm-web assets are `include_str!`-baked: any JS change needs `cargo build -r -p rm-web` + server restart or the browser serves stale JS.
- Fixtures are hand-built `execute_batch` chains (house style; no `rm_ingest::testfix` in these crates).
- `import_batch` columns (0001_init.sql:210): `batch_id, source_dir, file_count, captured_at, valid_from, status, created_at`.

## Review Focus

1. **Two editors race on one piece** — second POST carries a stale `expected_updated_at` → 409 `PieceNotCurrent`, ZERO partial writes (no row change, no log row). Pinned: Task 2 `apply_piece_edit_refuses_stale_expectation_zero_partial`, Task 6 `piece_post_stale_expectation_409`.
2. **`piece_id` reused across routes/directions** — client-supplied `piece_id` that exists under a different xno/direction → 400 `PieceScope`, never silent id reuse or PK collision 500. Pinned: Task 2 `apply_piece_edit_refuses_foreign_scope_piece_id`, Task 6 scope case in `piece_post_requires_auth_and_valid_payload`.
3. **Undo after the piece moved on** — undo of an edit that is no longer the piece's latest state → 409 `PieceNotCurrent`, zero partial (no undo log row). Pinned: Task 3 `apply_piece_undo_refuses_after_second_edit_zero_partial`, Task 5 `piece_undo_superseded_refused`.
4. **Direction with no src polyline** (`''` or absent `src_route_point` row) — piece create/edit must still work; anchor-on-src and overlap checks skip (nothing to splice). Pinned: Task 4 `piece_create_ok_without_src_polyline`.
5. **Adjacent pieces sharing an endpoint anchor** — touching spans are LEGAL (adjacency), only strict interval intersection is a 409 `PieceOverlap`. Pinned: Task 4 `piece_adjacent_spans_allowed_overlap_rejected`.

---

### Task 1: Migration `0012_polyline_piece`

**Files:**
- Create: `crates/rm-db/migrations/0012_polyline_piece.sql`
- Modify: `crates/rm-db/src/migrate.rs` (MIGRATIONS array + ordered-names test)
- Test: `crates/rm-db/src/migrate.rs` (`mod tests`)

**Interfaces:**
- Produces: table `polyline_piece (piece_id INTEGER PRIMARY KEY, xno TEXT NOT NULL, direction INTEGER NOT NULL, encoded TEXT NOT NULL, anchor_start_lat REAL NOT NULL, anchor_start_lon REAL NOT NULL, anchor_end_lat REAL NOT NULL, anchor_end_lon REAL NOT NULL, updated_at TEXT NOT NULL)`. Every later task inserts/reads these exact columns.

- [ ] **Step 1: Write the failing test**

In `crates/rm-db/src/migrate.rs`, the existing `migrations_are_ordered_and_named` test asserts the full names array — extend it (this is the failing test), and add a table-existence test:

```rust
    #[test]
    fn polyline_piece_exists_after_migrate() {
        let conn = open_in_memory().unwrap();
        let cols: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('polyline_piece') ORDER BY cid")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            cols,
            vec![
                "piece_id",
                "xno",
                "direction",
                "encoded",
                "anchor_start_lat",
                "anchor_start_lon",
                "anchor_end_lat",
                "anchor_end_lon",
                "updated_at"
            ]
        );
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rm-db --lib migrate`
Expected: FAIL — `migrations_are_ordered_and_named` mismatch (no `0012_polyline_piece`) and/or `no such table: polyline_piece`.

- [ ] **Step 3: Write the migration + registration**

`crates/rm-db/migrations/0012_polyline_piece.sql`:

```sql
-- 0012_polyline_piece: manual src-polyline pieces (phase 1: storage only).
-- One row per piece = current (unversioned); updated_at is the
-- optimistic-concurrency token and equals the producing edit's ts. History
-- is edit_log's `changes` (old/new encoded), not a version chain. See
-- docs/superpowers/specs/2026-10-10-polyline-piece-edit-design.md.
CREATE TABLE polyline_piece (
    piece_id         INTEGER PRIMARY KEY,   -- minted via id_sequence
    xno              TEXT NOT NULL,
    direction        INTEGER NOT NULL,
    encoded          TEXT NOT NULL,         -- Google 1e-5, ≥2 points
    anchor_start_lat REAL NOT NULL,
    anchor_start_lon REAL NOT NULL,
    anchor_end_lat   REAL NOT NULL,
    anchor_end_lon   REAL NOT NULL,
    updated_at       TEXT NOT NULL
);
```

In `crates/rm-db/src/migrate.rs`, append to `MIGRATIONS` after the `0011_drop_segment` entry:

```rust
    (
        "0012_polyline_piece",
        include_str!("../migrations/0012_polyline_piece.sql"),
    ),
```

and extend the `names` assert in `migrations_are_ordered_and_named` with `"0012_polyline_piece"`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rm-db --lib migrate`
Expected: PASS (all migrate tests).

- [ ] **Step 5: Commit**

```bash
git add crates/rm-db/migrations/0012_polyline_piece.sql crates/rm-db/src/migrate.rs
git commit -m "rm-db: 0012_polyline_piece — unversioned manual polyline pieces"
```

---

### Task 2: rm-db — polyline snapshot load + `apply_piece_edit`

**Files:**
- Modify: `crates/rm-db/src/edit.rs` (new public fns/structs + `EditError` variants + `mod tests`)
- Modify: `crates/rm-db/src/lib.rs` (re-export list)

**Interfaces:**
- Consumes: `polyline_piece` (Task 1), `crate::mint(&tx, "polyline_piece")`, `crate::now_rfc3339()`, existing `EditError` enum at edit.rs:10.
- Produces (rm-db, all `pub`):
  - `EditError::UnknownRoute(String)`, `EditError::PieceNotCurrent(i64)`, `EditError::PieceScope(i64)` (new variants).
  - `struct PieceRow { piece_id: i64, encoded: String, anchor_start_lat: f64, anchor_start_lon: f64, anchor_end_lat: f64, anchor_end_lon: f64, updated_at: String }`
  - `struct PolylineSnapshot { route_id: i64, src_encoded: Option<String>, pieces: Vec<PieceRow> }`
  - `fn load_polyline_snapshot(conn: &Connection, xno: &str, direction: i64) -> Result<Option<PolylineSnapshot>>`
  - `struct PieceEditOp { xno: String, direction: i64, piece_id: Option<i64>, encoded: String, anchor_start_lat: f64, anchor_start_lon: f64, anchor_end_lat: f64, anchor_end_lon: f64, expected_updated_at: Option<String> }`
  - `struct PieceApplied { edit_id: i64, piece_id: i64, updated_at: String, created: bool }`
  - `fn apply_piece_edit(conn: &Connection, op: &PieceEditOp, editor: &str, note: &str) -> Result<PieceApplied>`

- [ ] **Step 1: Write the failing tests**

In `crates/rm-db/src/edit.rs` `mod tests` (fixture helpers live at the bottom of the module — add these next to `fixture`):

```rust
    /// import_batch 1 (CLOSED) + src_route_point for xno '1' dir 1: the
    /// 2-point Google reference polyline (38.5,-120.2)→(40.7,-120.95).
    fn src_fixture(conn: &Connection) {
        conn.execute(
            "INSERT INTO import_batch (batch_id, source_dir, file_count, captured_at, valid_from, status, created_at)
             VALUES (1, 'assets/x', 1, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'CLOSED', '2026-01-01T00:00:00Z')", []).unwrap();
        conn.execute(
            "INSERT INTO src_route_point (batch_id, xno, direction, encoded)
             VALUES (1, '1', 1, '_p~iF~ps|U_ulLnnqC')", []).unwrap();
    }

    fn piece_op(pid: Option<i64>) -> PieceEditOp {
        PieceEditOp {
            xno: "1".into(),
            direction: 1,
            piece_id: pid,
            encoded: "_ulLnnqC_p~iF~ps|U".into(),
            anchor_start_lat: 38.5,
            anchor_start_lon: -120.2,
            anchor_end_lat: 40.7,
            anchor_end_lon: -120.95,
            expected_updated_at: None,
        }
    }
```

(The existing `fixture(&conn)` in this module already inserts route 1 with `ext_uid 'tcg:1'` — verify with `grep -n "tcg:1" crates/rm-db/src/edit.rs` and reuse it; if its ext_uid differs, insert a plain `route` row with `ext_uid 'tcg:1'` in `src_fixture` instead of relying on it.)

```rust
    #[test]
    fn apply_piece_edit_creates_row_and_log() {
        let conn = open_in_memory().unwrap();
        let _ = fixture(&conn);
        src_fixture(&conn);
        let out = apply_piece_edit(&conn, &piece_op(None), "herry", "fix").unwrap();
        assert!(out.created);
        assert_eq!(out.piece_id, 1); // id_sequence auto-seeds at 1
        let (enc, up): (String, String) = conn.query_row(
            "SELECT encoded, updated_at FROM polyline_piece WHERE piece_id = 1", [],
            |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(enc, "_ulLnnqC_p~iF~ps|U");
        assert_eq!(up, out.updated_at);
        // log row: entity polyline, entity_id = route_id 1, action create, changes carry new only
        let (ent, eid, act, changes): (String, i64, String, String) = conn.query_row(
            "SELECT entity, entity_id, action, changes FROM edit_log WHERE edit_id = ?1",
            [out.edit_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap();
        assert_eq!((ent.as_str(), eid, act.as_str()), ("polyline", 1, "create"));
        let c: serde_json::Value = serde_json::from_str(&changes).unwrap();
        assert_eq!(c["kind"], "polyline_piece");
        assert!(c["encoded"].get("old").is_none());
        assert_eq!(c["encoded"]["new"], "_ulLnnqC_p~iF~ps|U");
        assert_eq!(c["anchors"]["new"][0][0], 38.5);
        // zero stale marks (phase 1)
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }

    #[test]
    fn apply_piece_edit_updates_existing_and_logs_old() {
        let conn = open_in_memory().unwrap();
        let _ = fixture(&conn);
        src_fixture(&conn);
        let first = apply_piece_edit(&conn, &piece_op(None), "herry", "a").unwrap();
        let mut second = piece_op(Some(first.piece_id));
        second.expected_updated_at = Some(first.updated_at.clone());
        second.encoded = "_p~iF~ps|U_ulLnnqC".into();
        let out = apply_piece_edit(&conn, &second, "herry", "b").unwrap();
        assert!(!out.created);
        let (act, changes): (String, String) = conn.query_row(
            "SELECT action, changes FROM edit_log WHERE edit_id = ?1",
            [out.edit_id], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(act, "update");
        let c: serde_json::Value = serde_json::from_str(&changes).unwrap();
        assert_eq!(c["encoded"]["old"], "_ulLnnqC_p~iF~ps|U");
        assert_eq!(c["encoded"]["new"], "_p~iF~ps|U_ulLnnqC");
    }

    #[test]
    fn apply_piece_edit_refuses_stale_expectation_zero_partial() {
        let conn = open_in_memory().unwrap();
        let _ = fixture(&conn);
        src_fixture(&conn);
        let first = apply_piece_edit(&conn, &piece_op(None), "herry", "a").unwrap();
        let mut second = piece_op(Some(first.piece_id));
        second.expected_updated_at = Some("2020-01-01T00:00:00Z".into());
        let err = apply_piece_edit(&conn, &second, "herry", "b").unwrap_err();
        assert!(err.to_string().contains("edited by someone else"), "{err}");
        // zero partial: one row (original), one log row (the create)
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM polyline_piece", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM edit_log", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        let _ = first;
    }

    #[test]
    fn apply_piece_edit_refuses_foreign_scope_piece_id() {
        let conn = open_in_memory().unwrap();
        let _ = fixture(&conn);
        src_fixture(&conn);
        let first = apply_piece_edit(&conn, &piece_op(None), "herry", "a").unwrap();
        // same id, different direction → scope refusal, not silent reuse
        let mut foreign = piece_op(Some(first.piece_id));
        foreign.direction = 2;
        let err = apply_piece_edit(&conn, &foreign, "herry", "x").unwrap_err();
        assert!(err.to_string().contains("different route/direction"), "{err}");
    }

    #[test]
    fn apply_piece_edit_recreates_deleted_piece_with_matching_expectation_absent() {
        let conn = open_in_memory().unwrap();
        let _ = fixture(&conn);
        src_fixture(&conn);
        let first = apply_piece_edit(&conn, &piece_op(None), "herry", "a").unwrap();
        conn.execute("DELETE FROM polyline_piece WHERE piece_id = ?1", [first.piece_id]).unwrap();
        // no expectation + absent row = legal recreate (undo-of-create edge)
        let out = apply_piece_edit(&conn, &piece_op(Some(first.piece_id)), "herry", "again").unwrap();
        assert!(out.created);
        // expectation on an absent row = stale
        let mut ghost = piece_op(Some(99));
        ghost.expected_updated_at = Some("2026-01-01T00:00:00Z".into());
        let err = apply_piece_edit(&conn, &ghost, "herry", "x").unwrap_err();
        assert!(err.to_string().contains("edited by someone else"), "{err}");
    }

    #[test]
    fn load_polyline_snapshot_resolves_route_src_and_pieces() {
        let conn = open_in_memory().unwrap();
        let _ = fixture(&conn);
        src_fixture(&conn);
        let snap = load_polyline_snapshot(&conn, "1", 1).unwrap().unwrap();
        assert_eq!(snap.route_id, 1);
        assert_eq!(snap.src_encoded.as_deref(), Some("_p~iF~ps|U_ulLnnqC"));
        assert!(snap.pieces.is_empty());
        // unknown xno → None
        assert!(load_polyline_snapshot(&conn, "999", 1).unwrap().is_none());
        // missing src row (direction 2 exists as a route but no capture) → Some with src None
        let snap2 = load_polyline_snapshot(&conn, "1", 2).unwrap().unwrap();
        assert!(snap2.src_encoded.is_none());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rm-db --lib apply_piece`
Expected: FAIL — `PieceEditOp`, `apply_piece_edit`, `load_polyline_snapshot` not defined; `UnknownRoute`/`PieceNotCurrent`/`PieceScope` variants missing.

- [ ] **Step 3: Implement**

In `crates/rm-db/src/edit.rs`, extend the `EditError` enum (edit.rs:10):

```rust
    #[error("route {0} does not exist")]
    UnknownRoute(String),
    #[error("piece {0} was edited by someone else — reload and retry")]
    PieceNotCurrent(i64),
    #[error("piece {0} exists under a different route/direction")]
    PieceScope(i64),
```

Add the snapshot/op/apply code (place after `apply_station_move`, before the undo-op section at edit.rs:934):

```rust
/// One current manual polyline piece, as stored (unversioned — the row IS
/// the current state; history is edit_log's).
#[derive(Debug, Clone)]
pub struct PieceRow {
    pub piece_id: i64,
    pub encoded: String,
    pub anchor_start_lat: f64,
    pub anchor_start_lon: f64,
    pub anchor_end_lat: f64,
    pub anchor_end_lon: f64,
    pub updated_at: String,
}

/// Read-side snapshot for polyline piece edits: the route id (edit_log's
/// entity_id), the latest-batch src polyline (`None`/empty when the capture
/// has no polyline for this direction), and the direction's current pieces.
#[derive(Debug)]
pub struct PolylineSnapshot {
    pub route_id: i64,
    pub src_encoded: Option<String>,
    pub pieces: Vec<PieceRow>,
}

/// `xno` is stable route identity (`route.ext_uid = 'tcg:' || xno`).
/// `None` when no route carries that ext_uid.
pub fn load_polyline_snapshot(
    conn: &Connection,
    xno: &str,
    direction: i64,
) -> Result<Option<PolylineSnapshot>> {
    let route_id: i64 = match conn.query_row(
        "SELECT route_id FROM route WHERE ext_uid = ?1",
        [format!("tcg:{xno}")],
        |r| r.get(0),
    ) {
        Ok(id) => id,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let batch_id: i64 = conn.query_row(
        "SELECT COALESCE(MAX(batch_id), 0) FROM import_batch",
        [],
        |r| r.get(0),
    )?;
    let src_encoded: Option<String> = conn
        .query_row(
            "SELECT encoded FROM src_route_point
             WHERE batch_id = ?1 AND xno = ?2 AND direction = ?3",
            params![batch_id, xno, direction],
            |r| r.get(0),
        )
        .optional()?;
    let mut stmt = conn.prepare(
        "SELECT piece_id, encoded, anchor_start_lat, anchor_start_lon,
                anchor_end_lat, anchor_end_lon, updated_at
         FROM polyline_piece WHERE xno = ?1 AND direction = ?2
         ORDER BY piece_id",
    )?;
    let pieces = stmt
        .query_map(params![xno, direction], |r| {
            Ok(PieceRow {
                piece_id: r.get(0)?,
                encoded: r.get(1)?,
                anchor_start_lat: r.get(2)?,
                anchor_start_lon: r.get(3)?,
                anchor_end_lat: r.get(4)?,
                anchor_end_lon: r.get(5)?,
                updated_at: r.get(6)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(Some(PolylineSnapshot { route_id, src_encoded, pieces }))
}

/// Write-side op for a manual piece edit (rm-edit maps a PieceEditPlan into
/// this; rm-db knows nothing about domain geometry).
#[derive(Debug)]
pub struct PieceEditOp {
    pub xno: String,
    pub direction: i64,
    pub piece_id: Option<i64>,
    pub encoded: String,
    pub anchor_start_lat: f64,
    pub anchor_start_lon: f64,
    pub anchor_end_lat: f64,
    pub anchor_end_lon: f64,
    pub expected_updated_at: Option<String>,
}

#[derive(Debug)]
pub struct PieceApplied {
    pub edit_id: i64,
    pub piece_id: i64,
    pub updated_at: String,
    pub created: bool,
}

/// Apply a manual piece edit in ONE transaction. Unversioned: the row IS
/// current; `updated_at` doubles as the optimistic-concurrency token and
/// the producing edit's ts. Currency + scope guards run before ANY write.
/// ZERO stale marks (phase 1: nothing consumes manual pieces yet).
pub fn apply_piece_edit(
    conn: &Connection,
    op: &PieceEditOp,
    editor: &str,
    note: &str,
) -> Result<PieceApplied> {
    let tx = conn.unchecked_transaction()?;
    let now = crate::now_rfc3339();

    let route_id: i64 = match tx.query_row(
        "SELECT route_id FROM route WHERE ext_uid = ?1",
        [format!("tcg:{}", op.xno)],
        |r| r.get(0),
    ) {
        Ok(id) => id,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Err(EditError::UnknownRoute(op.xno.clone()).into())
        }
        Err(e) => return Err(e.into()),
    };

    // Currency/scope: all guards before any write.
    let mut created = true;
    let mut old: Option<(String, [f64; 2], [f64; 2])> = None;
    if let Some(pid) = op.piece_id {
        let row = tx
            .query_row(
                "SELECT updated_at, encoded, anchor_start_lat, anchor_start_lon,
                        anchor_end_lat, anchor_end_lon
                 FROM polyline_piece
                 WHERE piece_id = ?1 AND xno = ?2 AND direction = ?3",
                params![pid, op.xno, op.direction],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, f64>(2)?,
                        r.get::<_, f64>(3)?,
                        r.get::<_, f64>(4)?,
                        r.get::<_, f64>(5)?,
                    ))
                },
            )
            .optional()?;
        match row {
            Some((updated_at, encoded, aslat, aslon, aelat, aelon)) => {
                created = false;
                if Some(&updated_at) != op.expected_updated_at.as_ref() {
                    return Err(EditError::PieceNotCurrent(pid).into());
                }
                old = Some((encoded, [aslat, aslon], [aelat, aelon]));
            }
            None => {
                // Recreate-after-undo edge: absent row is legal only with no
                // expectation; and the id must not exist under another scope.
                if op.expected_updated_at.is_some() {
                    return Err(EditError::PieceNotCurrent(pid).into());
                }
                let scope: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM polyline_piece
                     WHERE piece_id = ?1 AND (xno != ?2 OR direction != ?3)",
                    params![pid, op.xno, op.direction],
                    |r| r.get(0),
                )?;
                if scope > 0 {
                    return Err(EditError::PieceScope(pid).into());
                }
            }
        }
    }

    let piece_id = match op.piece_id {
        Some(pid) => pid,
        None => crate::mint(&tx, "polyline_piece")?,
    };
    tx.execute(
        "INSERT INTO polyline_piece
            (piece_id, xno, direction, encoded,
             anchor_start_lat, anchor_start_lon, anchor_end_lat, anchor_end_lon, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(piece_id) DO UPDATE SET
           encoded = ?4, anchor_start_lat = ?5, anchor_start_lon = ?6,
           anchor_end_lat = ?7, anchor_end_lon = ?8, updated_at = ?9",
        params![
            piece_id,
            op.xno,
            op.direction,
            op.encoded,
            op.anchor_start_lat,
            op.anchor_start_lon,
            op.anchor_end_lat,
            op.anchor_end_lon,
            now
        ],
    )?;

    // changes carries old+new so undo is a forward write (spec-pinned shape;
    // `old` absent on create).
    let mut encoded_change = json!({ "new": op.encoded });
    let mut anchors_change = json!({ "new": [
        [op.anchor_start_lat, op.anchor_start_lon],
        [op.anchor_end_lat, op.anchor_end_lon]
    ] });
    if let Some((enc, s, e)) = &old {
        encoded_change["old"] = json!(enc);
        anchors_change["old"] = json!([s, e]);
    }
    let changes = json!({
        "kind": "polyline_piece",
        "xno": op.xno,
        "direction": op.direction,
        "piece_id": piece_id,
        "encoded": encoded_change,
        "anchors": anchors_change,
    })
    .to_string();
    tx.execute(
        "INSERT INTO edit_log (ts, editor, entity, entity_id, action, changes, note)
         VALUES (?1, ?2, 'polyline', ?3, ?4, ?5, ?6)",
        params![
            now,
            editor,
            route_id,
            if created { "create" } else { "update" },
            changes,
            if note.is_empty() { None } else { Some(note) }
        ],
    )?;
    let edit_id = tx.last_insert_rowid();

    tx.commit()?;
    Ok(PieceApplied { edit_id, piece_id, updated_at: now, created })
}
```

In `crates/rm-db/src/lib.rs`, extend the `pub use edit::{…}` list with:

```rust
    PieceRow, PolylineSnapshot, load_polyline_snapshot,
    PieceEditOp, PieceApplied, apply_piece_edit,
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rm-db --lib`
Expected: PASS (whole rm-db lib suite — the new tests plus everything prior; the exhaustive `map_db_err` match lives in rm-edit, not here).

- [ ] **Step 5: Commit**

```bash
git add crates/rm-db/src/edit.rs crates/rm-db/src/lib.rs
git commit -m "rm-db: polyline piece snapshot load + apply_piece_edit — one tx, currency guard, piece log rows"
```

---

### Task 3: rm-db — `apply_piece_undo` + `load_polyline_edit`

**Files:**
- Modify: `crates/rm-db/src/edit.rs` (+ `mod tests`)
- Modify: `crates/rm-db/src/lib.rs` (re-export list)

**Interfaces:**
- Consumes: Task 2's `EditError::PieceNotCurrent`, `polyline_piece` table, `edit_log` rows written by `apply_piece_edit`.
- Produces (rm-db, `pub`):
  - `struct PolylineEditRow { edit_id: i64, entity_id: i64, action: String, ts: String, changes: serde_json::Value }`
  - `fn load_polyline_edit(conn: &Connection, edit_id: i64) -> Result<Option<PolylineEditRow>>`
  - `struct PieceRestoreState { encoded: String, anchor_start_lat: f64, anchor_start_lon: f64, anchor_end_lat: f64, anchor_end_lon: f64 }`
  - `struct PieceUndoOp { edit_id: i64, route_id: i64, xno: String, direction: i64, piece_id: i64, undone_ts: String, restored: Option<PieceRestoreState> }`
  - `struct PieceUndone { edit_id: i64, piece_id: i64, deleted: bool }`
  - `fn apply_piece_undo(conn: &Connection, op: &PieceUndoOp, editor: &str, note: &str) -> Result<PieceUndone>`

- [ ] **Step 1: Write the failing tests**

In `crates/rm-db/src/edit.rs` `mod tests` (reuse Task 2's `src_fixture`/`piece_op`):

```rust
    fn piece_undo_op(
        edit_id: i64,
        piece_id: i64,
        restored: Option<(&str, [f64; 2], [f64; 2])>,
    ) -> PieceUndoOp {
        PieceUndoOp {
            edit_id,
            route_id: 1,
            xno: "1".into(),
            direction: 1,
            piece_id,
            undone_ts: String::new(), // set per-test from the applied row
            restored: restored.map(|(encoded, s, e)| PieceRestoreState {
                encoded: encoded.into(),
                anchor_start_lat: s[0],
                anchor_start_lon: s[1],
                anchor_end_lat: e[0],
                anchor_end_lon: e[1],
            }),
        }
    }

    #[test]
    fn apply_piece_undo_of_create_deletes_row() {
        let conn = open_in_memory().unwrap();
        let _ = fixture(&conn);
        src_fixture(&conn);
        let first = apply_piece_edit(&conn, &piece_op(None), "herry", "a").unwrap();
        let mut op = piece_undo_op(first.edit_id, first.piece_id, None);
        op.undone_ts = first.updated_at.clone();
        let out = apply_piece_undo(&conn, &op, "h", "rev").unwrap();
        assert!(out.deleted);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM polyline_piece", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        // undo row: action undo, undo_of set, payload decides delete (new null)
        let (act, undo_of, changes): (String, i64, String) = conn.query_row(
            "SELECT action, undo_of, changes FROM edit_log WHERE edit_id = ?1",
            [out.edit_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!((act.as_str(), undo_of), ("undo", first.edit_id));
        let c: serde_json::Value = serde_json::from_str(&changes).unwrap();
        assert_eq!(c["kind"], "polyline_piece");
        assert_eq!(c["piece_id"], first.piece_id);
        assert!(c["encoded"]["new"].is_null());
        assert_eq!(c["encoded"]["old"], "_ulLnnqC_p~iF~ps|U");
    }

    #[test]
    fn apply_piece_undo_of_update_restores_old_state() {
        let conn = open_in_memory().unwrap();
        let _ = fixture(&conn);
        src_fixture(&conn);
        let first = apply_piece_edit(&conn, &piece_op(None), "herry", "a").unwrap();
        let mut second = piece_op(Some(first.piece_id));
        second.expected_updated_at = Some(first.updated_at.clone());
        second.encoded = "_p~iF~ps|U_ulLnnqC".into();
        second.anchor_start_lat = 40.7;
        second.anchor_start_lon = -120.95;
        second.anchor_end_lat = 38.5;
        second.anchor_end_lon = -120.2;
        let edit2 = apply_piece_edit(&conn, &second, "herry", "b").unwrap();
        let mut op = piece_undo_op(
            edit2.edit_id,
            first.piece_id,
            Some(("_ulLnnqC_p~iF~ps|U", [38.5, -120.2], [40.7, -120.95])),
        );
        op.undone_ts = edit2.updated_at.clone();
        let out = apply_piece_undo(&conn, &op, "h", "rev").unwrap();
        assert!(!out.deleted);
        let (enc, s, e): (String, f64, f64) = conn.query_row(
            "SELECT encoded, anchor_start_lat, anchor_end_lat FROM polyline_piece WHERE piece_id = ?1",
            [first.piece_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!((enc.as_str(), s, e), ("_ulLnnqC_p~iF~ps|U", 38.5, 40.7));
    }

    #[test]
    fn apply_piece_undo_refuses_after_second_edit_zero_partial() {
        let conn = open_in_memory().unwrap();
        let _ = fixture(&conn);
        src_fixture(&conn);
        let first = apply_piece_edit(&conn, &piece_op(None), "herry", "a").unwrap();
        let mut second = piece_op(Some(first.piece_id));
        second.expected_updated_at = Some(first.updated_at.clone());
        second.encoded = "_p~iF~ps|U_ulLnnqC".into();
        let _edit2 = apply_piece_edit(&conn, &second, "herry", "b").unwrap();
        // undo of the FIRST edit: piece has moved on → refuse
        let mut op = piece_undo_op(first.edit_id, first.piece_id, None);
        op.undone_ts = first.updated_at.clone();
        let err = apply_piece_undo(&conn, &op, "h", "rev").unwrap_err();
        assert!(err.to_string().contains("edited by someone else"), "{err}");
        // zero partial: no undo log row, piece still present at second's state
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM edit_log WHERE action = 'undo'", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(conn.query_row("SELECT encoded FROM polyline_piece WHERE piece_id = ?1", [first.piece_id], |r| r.get::<_, String>(0)).unwrap(), "_p~iF~ps|U_ulLnnqC");
    }

    #[test]
    fn load_polyline_edit_filters_entity_and_returns_ts() {
        let conn = open_in_memory().unwrap();
        let _ = fixture(&conn);
        src_fixture(&conn);
        let first = apply_piece_edit(&conn, &piece_op(None), "herry", "a").unwrap();
        let row = load_polyline_edit(&conn, first.edit_id).unwrap().unwrap();
        assert_eq!((row.entity_id, row.action.as_str()), (1, "create"));
        assert_eq!(row.ts, first.updated_at);
        assert_eq!(row.changes["kind"], "polyline_piece");
        // stop-entity row → None (kind guard discipline: the caller refuses).
        // Same construction as the existing `edit_entity_returns_entity_or_none`
        // test (fixture returns the stop_version_id).
        let old = fixture(&conn);
        let stop_edit = apply_stop_edit(&conn, &StopEditOp {
            stop_id: 1, stop_version_id: old, stop_name: Some("N".into()), ..Default::default()
        }, "h", "n").unwrap();
        assert!(load_polyline_edit(&conn, stop_edit.edit_id).unwrap().is_none());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rm-db --lib piece_undo`
Expected: FAIL — `PieceUndoOp`/`apply_piece_undo`/`load_polyline_edit`/`PieceRestoreState` undefined.

- [ ] **Step 3: Implement**

In `crates/rm-db/src/edit.rs`, after Task 2's `apply_piece_edit`:

```rust
/// One `entity='polyline'` edit_log row (the undo loader). Filtering on
/// entity in the WHERE keeps other entities' rows returning `None` — the
/// plan's kind guard stays the discriminator within the polyline entity.
#[derive(Debug)]
pub struct PolylineEditRow {
    pub edit_id: i64,
    pub entity_id: i64, // route_id
    pub action: String,
    pub ts: String,
    pub changes: serde_json::Value,
}

pub fn load_polyline_edit(conn: &Connection, edit_id: i64) -> Result<Option<PolylineEditRow>> {
    let row = conn
        .query_row(
            "SELECT entity_id, action, ts, changes FROM edit_log
             WHERE edit_id = ?1 AND entity = 'polyline'",
            [edit_id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
    Ok(row.map(|(entity_id, action, ts, changes)| PolylineEditRow {
        edit_id,
        entity_id,
        action,
        ts,
        changes: serde_json::from_str(&changes)?,
    }))
}

/// The state an undo writes back. `None` = the undone edit was a create →
/// undo deletes the piece row (back to pure src).
#[derive(Debug)]
pub struct PieceRestoreState {
    pub encoded: String,
    pub anchor_start_lat: f64,
    pub anchor_start_lon: f64,
    pub anchor_end_lat: f64,
    pub anchor_end_lon: f64,
}

/// Write-side op for the piece undo: restore-as-forward-write (or delete).
/// `undone_ts` is the undone edit's ts — the currency token the apply tx
/// re-validates against the row.
#[derive(Debug)]
pub struct PieceUndoOp {
    pub edit_id: i64,
    pub route_id: i64,
    pub xno: String,
    pub direction: i64,
    pub piece_id: i64,
    pub undone_ts: String,
    pub restored: Option<PieceRestoreState>,
}

#[derive(Debug)]
pub struct PieceUndone {
    pub edit_id: i64,
    pub piece_id: i64,
    pub deleted: bool,
}

/// Undo a manual piece edit in ONE transaction. Currency guard (the piece's
/// `updated_at` must still equal the undone edit's ts) runs before any
/// write, so a superseded undo is a clean 409 with zero partial state.
/// The undo log row carries the swapped payload shape (spec deviation from
/// `'{}'`: the payload is what future tooling reads the piece history from).
pub fn apply_piece_undo(
    conn: &Connection,
    op: &PieceUndoOp,
    editor: &str,
    note: &str,
) -> Result<PieceUndone> {
    let tx = conn.unchecked_transaction()?;
    let now = crate::now_rfc3339();

    let (updated_at, cur_encoded, cur_aslat, cur_aslon, cur_aelat, cur_aelon) = match tx
        .query_row(
            "SELECT updated_at, encoded, anchor_start_lat, anchor_start_lon,
                    anchor_end_lat, anchor_end_lon
             FROM polyline_piece
             WHERE piece_id = ?1 AND xno = ?2 AND direction = ?3",
            params![op.piece_id, op.xno, op.direction],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, f64>(2)?,
                    r.get::<_, f64>(3)?,
                    r.get::<_, f64>(4)?,
                    r.get::<_, f64>(5)?,
                ))
            },
        )
        .optional()?
    {
        Some(v) => v,
        None => return Err(EditError::PieceNotCurrent(op.piece_id).into()),
    };
    if updated_at != op.undone_ts {
        return Err(EditError::PieceNotCurrent(op.piece_id).into());
    }

    let (deleted, new_encoded, new_anchors) = match &op.restored {
        Some(r) => {
            tx.execute(
                "UPDATE polyline_piece SET encoded = ?1, anchor_start_lat = ?2,
                        anchor_start_lon = ?3, anchor_end_lat = ?4, anchor_end_lon = ?5,
                        updated_at = ?6
                 WHERE piece_id = ?7",
                params![
                    r.encoded,
                    r.anchor_start_lat,
                    r.anchor_start_lon,
                    r.anchor_end_lat,
                    r.anchor_end_lon,
                    now,
                    op.piece_id
                ],
            )?;
            (false, json!(r.encoded), json!([
                [r.anchor_start_lat, r.anchor_start_lon],
                [r.anchor_end_lat, r.anchor_end_lon]
            ]))
        }
        None => {
            tx.execute(
                "DELETE FROM polyline_piece WHERE piece_id = ?1",
                [op.piece_id],
            )?;
            (true, serde_json::Value::Null, serde_json::Value::Null)
        }
    };

    let changes = json!({
        "kind": "polyline_piece",
        "xno": op.xno,
        "direction": op.direction,
        "piece_id": op.piece_id,
        "encoded": { "old": cur_encoded, "new": new_encoded },
        "anchors": { "old": json!([
            [cur_aslat, cur_aslon],
            [cur_aelat, cur_aelon]
        ]), "new": new_anchors },
    })
    .to_string();
    tx.execute(
        "INSERT INTO edit_log (ts, editor, entity, entity_id, action, changes, note, undo_of)
         VALUES (?1, ?2, 'polyline', ?3, 'undo', ?4, ?5, ?6)",
        params![
            now,
            editor,
            op.route_id,
            changes,
            if note.is_empty() { None } else { Some(note) },
            op.edit_id
        ],
    )?;
    let edit_id = tx.last_insert_rowid();

    tx.commit()?;
    Ok(PieceUndone { edit_id, piece_id: op.piece_id, deleted })
}
```

Extend the `pub use edit::{…}` list in `crates/rm-db/src/lib.rs` with:

```rust
    PolylineEditRow, load_polyline_edit,
    PieceRestoreState, PieceUndoOp, PieceUndone, apply_piece_undo,
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rm-db --lib`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/rm-db/src/edit.rs crates/rm-db/src/lib.rs
git commit -m "rm-db: apply_piece_undo — forward-write restore or delete, ts currency guard"
```

---

### Task 4: rm-edit — `piece.rs` planning + `edit_polyline_piece` service

**Files:**
- Modify: `crates/rm-edit/Cargo.toml` (add `rm-core.workspace = true` under `[dependencies]`)
- Modify: `crates/rm-edit/src/lib.rs` (declare module, extend `map_db_err`)
- Modify: `crates/rm-edit/src/rename.rs` (extend `EditError` enum at rename.rs:52)
- Create: `crates/rm-edit/src/piece.rs`
- Test: `crates/rm-edit/tests/piece.rs`

**Interfaces:**
- Consumes: Task 2's `rm_db::{PolylineSnapshot, PieceRow, PieceEditOp, PieceApplied, load_polyline_snapshot, apply_piece_edit, EditError::{UnknownRoute, PieceNotCurrent, PieceScope}}`, `rm_core::polyline::{encode, decode}`, `rm_core::geo::{point_to_segment_m, haversine_m}`, `rm_core::LatLng`.
- Produces (rm-edit, `pub`):
  - `EditError::InvalidPiece(String)` (→400), `EditError::UnknownRoute(String)`, `EditError::PieceNotCurrent(i64)` (→409), `EditError::PieceOverlap(i64)` (→409), `EditError::PieceScope(i64)` (→400).
  - `pub const SEAM_TOL_M: f64` (= 5.0)
  - `struct PieceEditCmd { xno: String, direction: i64, piece_id: Option<i64>, points: Vec<[f64; 2]>, anchor_start: [f64; 2], anchor_end: [f64; 2], expected_updated_at: Option<String> }`
  - `fn plan_piece_edit(snap: &rm_db::PolylineSnapshot, cmd: PieceEditCmd) -> Result<PieceEditPlan, EditError>`
  - `struct PieceEditPlan { piece_id: Option<i64>, encoded: String, anchor_start: [f64; 2], anchor_end: [f64; 2], expected_updated_at: Option<String> }`
  - `struct PieceOutcome { edit_id: i64, piece_id: i64, updated_at: String, created: bool }`
  - `fn edit_polyline_piece(conn: &Connection, cmd: PieceEditCmd, editor: &str, note: &str) -> Result<PieceOutcome>`

- [ ] **Step 1: Write the failing tests**

`crates/rm-edit/tests/piece.rs`:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rm-edit --test piece`
Expected: FAIL — `PieceEditCmd`/`edit_polyline_piece` undefined; `EditError::PieceOverlap` missing; `rm_core` unresolved in Cargo.toml.

- [ ] **Step 3: Implement**

`crates/rm-edit/Cargo.toml` — under `[dependencies]` add:

```toml
rm-core.workspace = true
```

`crates/rm-edit/src/rename.rs` — extend `EditError` (rename.rs:52) with:

```rust
    #[error("invalid polyline piece: {0}")]
    InvalidPiece(String),
    #[error("route {0} does not exist")]
    UnknownRoute(String),
    #[error("piece {0} was edited by someone else — reload and retry")]
    PieceNotCurrent(i64),
    #[error("piece {0} overlaps an existing piece span")]
    PieceOverlap(i64),
    #[error("piece {0} exists under a different route/direction")]
    PieceScope(i64),
```

`crates/rm-edit/src/lib.rs` — add `mod piece;` + `pub use piece::*;` alongside the existing module lines (lib.rs:4-12), and extend `map_db_err`'s match (lib.rs:17-23):

```rust
        Some(rm_db::EditError::UnknownRoute(x)) => EditError::UnknownRoute(x.clone()).into(),
        Some(rm_db::EditError::PieceNotCurrent(id)) => EditError::PieceNotCurrent(*id).into(),
        Some(rm_db::EditError::PieceScope(id)) => EditError::PieceScope(*id).into(),
```

`crates/rm-edit/src/piece.rs`:

```rust
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
```

Note `cmd.clone()`: `PieceEditCmd` derives `Clone` (declared above); `cmd.xno` is moved into the op after the plan call.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rm-edit`
Expected: PASS (new piece tests + all prior rm-edit suites; `map_db_err`'s exhaustive match now compiles with the new arms).

- [ ] **Step 5: Commit**

```bash
git add crates/rm-edit/Cargo.toml crates/rm-edit/src/lib.rs crates/rm-edit/src/rename.rs crates/rm-edit/src/piece.rs crates/rm-edit/tests/piece.rs
git commit -m "rm-edit: polyline piece planning — seam/overlap/currency guards + edit service"
```

---

### Task 5: rm-edit — piece undo (`plan_piece_undo` + `undo_polyline_piece`)

**Files:**
- Modify: `crates/rm-edit/src/piece.rs`
- Test: `crates/rm-edit/tests/piece.rs`

**Interfaces:**
- Consumes: Task 3's `rm_db::{PolylineEditRow, PieceRestoreState, PieceUndoOp, PieceUndone, load_polyline_edit, apply_piece_undo}`, Task 4's module.
- Produces (rm-edit, `pub`):
  - `struct PieceUndoPlan { edit_id: i64, route_id: i64, xno: String, direction: i64, piece_id: i64, undone_ts: String, restored: Option<rm_db::PieceRestoreState> }`
  - `fn plan_piece_undo(edit: &rm_db::PolylineEditRow) -> Result<PieceUndoPlan, EditError>`
  - `struct PieceUndoOutcome { edit_id: i64, piece_id: i64, deleted: bool }`
  - `fn undo_polyline_piece(conn: &Connection, edit_id: i64, editor: &str, note: &str) -> Result<PieceUndoOutcome>`

- [ ] **Step 1: Write the failing tests**

Append to `crates/rm-edit/tests/piece.rs`:

```rust
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
```

(Note: the `c["piece_id"] = serde_json::serde_json::Value::Null;` line above should read `serde_json::Value::Null` — write it as `c["piece_id"] = serde_json::Value::Null;`.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rm-edit --test piece`
Expected: FAIL — `plan_piece_undo`/`undo_polyline_piece` undefined.

- [ ] **Step 3: Implement**

Append to `crates/rm-edit/src/piece.rs`:

```rust
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
        [a[0].as_f64()?, a[1].as_f64()?],
        [b[0].as_f64()?, b[1].as_f64()?],
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rm-edit`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/rm-edit/src/piece.rs crates/rm-edit/tests/piece.rs
git commit -m "rm-edit: polyline piece undo — payload-driven restore-vs-delete, forward write"
```

---

### Task 6: rm-web — HTTP endpoints + raw payload + undo dispatch

**Files:**
- Modify: `crates/rm-web/src/lib.rs` (router_with, after the station move route at lib.rs:65-68)
- Modify: `crates/rm-web/src/handlers/edit.rs` (body struct + handler + `edit_conflict` arms + undo dispatch)
- Modify: `crates/rm-web/src/queries/routes.rs` (`raw` fn — add `manual_pieces`)
- Test: `crates/rm-web/tests/edit.rs`

**Interfaces:**
- Consumes: `rm_edit::{edit_polyline_piece, undo_polyline_piece, PieceEditCmd}`, `rm_db::edit_entity`, `crate::queries::routes::raw`.
- Produces:
  - `POST /api/edit/polyline/{xno}/{direction}/piece` (spec-pinned path) — body `{points: [[lat,lon],…], anchor_start: [lat,lon], anchor_end: [lat,lon], piece_id?: i64, expected_updated_at?: string, note?: string}` → `{edit_id, piece_id, updated_at, created}`; 400 invalid/unknown-route/scope, 409 stale/overlap.
  - `POST /api/edit/undo/{edit_id}` — polyline rows return `{edit_id, restored: {piece_id, deleted}}`.
  - `GET /api/routes/{id}/directions/{dir}/raw` — payload gains `manual_pieces: [{piece_id, points, anchor_start, anchor_end, updated_at}]`.

- [ ] **Step 1: Write the failing tests**

In `crates/rm-web/tests/edit.rs`, extend the fixture's `execute_batch` tail (the existing fixture inserts route 1 `ext_uid 'tcg:1'`, batch chain and stop/route_stop_version rows) with:

```rust
    conn.execute(
        "INSERT INTO import_batch (batch_id, source_dir, file_count, captured_at, valid_from, status, created_at)
         VALUES (1, 'assets/x', 1, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'CLOSED', '2026-01-01T00:00:00Z')", []).unwrap();
    conn.execute(
        "INSERT INTO src_route_point (batch_id, xno, direction, encoded)
         VALUES (1, '1', 1, '_p~iF~ps|U_ulLnnqC')", []).unwrap();
```

(If the fixture's route ext_uid differs from `'tcg:1'`, adapt the xno in these rows AND in the tests below to match — check with `grep -n "tcg:" crates/rm-web/tests/edit.rs`.)

New tests (helper `post_json` mirrors the file's existing POST helper — reuse the existing one; its name is visible at the top of the undo tests, e.g. around edit.rs:229):

```rust
fn piece_body() -> serde_json::Value {
    serde_json::json!({
        "points": [[38.5, -120.2], [40.7, -120.95]],
        "anchor_start": [38.5, -120.2],
        "anchor_end": [40.7, -120.95],
        "note": "fix loop"
    })
}

#[tokio::test]
async fn piece_post_happy_path_and_raw_readback() {
    let dir = tempfile::tempdir().unwrap();
    let app = rm_web::router_with(edit_state(&dir));
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/edit/polyline/1/1/piece")
                .header("authorization", "Bearer sekrit")
                .header("content-type", "application/json")
                .body(Body::from(piece_body().to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["created"], true);
    assert_eq!(v["piece_id"], 1);
    // raw endpoint exposes the manual piece decoded
    let raw = get_json(&app, "/api/routes/1/directions/1/raw").await;
    assert_eq!(raw["manual_pieces"][0]["piece_id"], 1);
    assert_eq!(raw["manual_pieces"][0]["points"][0], serde_json::json!([38.5, -120.2]));
}

#[tokio::test]
async fn piece_post_requires_auth_and_valid_payload() {
    let dir = tempfile::tempdir().unwrap();
    let app = rm_web::router_with(edit_state(&dir));
    let post = |app: &axum::Router, uri: &str, body: serde_json::Value, auth: Option<&str>| {
        let mut b = Request::builder().method("POST").uri(uri)
            .header("content-type", "application/json");
        if let Some(a) = auth {
            b = b.header("authorization", a);
        }
        app.clone().oneshot(b.body(Body::from(body.to_string())).unwrap())
    };
    // 401 absent token
    let res = post(&app, "/api/edit/polyline/1/1/piece", piece_body(), None).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    // 403 bad token
    let res = post(&app, "/api/edit/polyline/1/1/piece", piece_body(), Some("Bearer nope")).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    // 400 single point
    let mut bad = piece_body();
    bad["points"] = serde_json::json!([[38.5, -120.2]]);
    let res = post(&app, "/api/edit/polyline/1/1/piece", bad, Some("Bearer sekrit")).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    // 400 unknown route
    let res = post(&app, "/api/edit/polyline/999/1/piece", piece_body(), Some("Bearer sekrit")).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    // 400 piece_id scoped to another direction: create under dir 1, replay
    // the id under dir 2 (no expectation) → PieceScope, never silent reuse
    let res = post(&app, "/api/edit/polyline/1/1/piece", piece_body(), Some("Bearer sekrit")).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let created = body_json(res).await;
    let mut foreign = piece_body();
    foreign["piece_id"] = serde_json::json!(created["piece_id"]);
    let res = post(&app, "/api/edit/polyline/1/2/piece", foreign, Some("Bearer sekrit")).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn piece_post_stale_expectation_409() {
    let dir = tempfile::tempdir().unwrap();
    let app = rm_web::router_with(edit_state(&dir));
    let auth = |req: axum::http::request::Builder| req.header("authorization", "Bearer sekrit").header("content-type", "application/json");
    let res = app.clone().oneshot(auth(Request::builder().method("POST").uri("/api/edit/polyline/1/1/piece")).body(Body::from(piece_body().to_string())).unwrap()).await.unwrap();
    let first = body_json(res).await;
    // second POST with the SAME (now-stale) expectation → 409
    let mut body = piece_body();
    body["piece_id"] = serde_json::json!(first["piece_id"]);
    body["expected_updated_at"] = serde_json::json!("2020-01-01T00:00:00Z");
    let res = app.oneshot(auth(Request::builder().method("POST").uri("/api/edit/polyline/1/1/piece")).body(Body::from(body.to_string())).unwrap()).await.unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn piece_undo_via_route_deletes_piece() {
    let dir = tempfile::tempdir().unwrap();
    let app = rm_web::router_with(edit_state(&dir));
    let auth = |req: axum::http::request::Builder| req.header("authorization", "Bearer sekrit").header("content-type", "application/json");
    let res = app.clone().oneshot(auth(Request::builder().method("POST").uri("/api/edit/polyline/1/1/piece")).body(Body::from(piece_body().to_string())).unwrap()).await.unwrap();
    let first = body_json(res).await;
    let res = app
        .clone()
        .oneshot(
            Request::builder().method("POST").uri(format!("/api/edit/undo/{}", first["edit_id"]))
                .header("authorization", "Bearer sekrit")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"note":"rev"}"#.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["restored"]["piece_id"], first["piece_id"]);
    assert_eq!(v["restored"]["deleted"], true);
    // readback: no manual pieces left
    let raw = get_json(&app, "/api/routes/1/directions/1/raw").await;
    assert_eq!(raw["manual_pieces"].as_array().map(Vec::len), Some(0));
}
```

Also extend the existing "write routes absent without --edit" test's path list with `/api/edit/polyline/1/1/piece` (find it via `grep -n "404" crates/rm-web/tests/edit.rs | head`).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rm-web --test edit piece`
Expected: FAIL — 404s / compile errors (`rm_edit::edit_polyline_piece` exists but no handler).

- [ ] **Step 3: Implement**

`crates/rm-web/src/handlers/edit.rs` — after `move_station` (edit.rs:317):

```rust
#[derive(Deserialize)]
pub struct PolylinePieceBody {
    points: Vec<[f64; 2]>,
    anchor_start: [f64; 2],
    anchor_end: [f64; 2],
    piece_id: Option<i64>,
    expected_updated_at: Option<String>,
    note: Option<String>,
}

fn coord_ok(pt: [f64; 2]) -> bool {
    (-90.0..=90.0).contains(&pt[0]) && (-180.0..=180.0).contains(&pt[1])
}

/// POST /api/edit/polyline/{xno}/{direction}/piece — store a manual polyline
/// piece. Server encodes via rm-core; the client sends plain coordinates.
pub async fn edit_polyline_piece(
    Path((xno, direction)): Path<(String, i64)>,
    State(state): State<AppState>,
    Editor(editor): Editor,
    ApiJson(b): ApiJson<PolylinePieceBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if b.points.iter().any(|p| !coord_ok(*p))
        || !coord_ok(b.anchor_start)
        || !coord_ok(b.anchor_end)
    {
        return Err(ApiError::bad_request("coordinate out of range"));
    }
    let out = run_db_write(state, move |conn| {
        rm_edit::edit_polyline_piece(
            conn,
            rm_edit::PieceEditCmd {
                xno,
                direction,
                piece_id: b.piece_id,
                points: b.points,
                anchor_start: b.anchor_start,
                anchor_end: b.anchor_end,
                expected_updated_at: b.expected_updated_at,
            },
            &editor,
            b.note.as_deref().unwrap_or(""),
        )
    })
    .await
    .map_err(edit_conflict)?;
    Ok(Json(json!({
        "edit_id": out.edit_id,
        "piece_id": out.piece_id,
        "updated_at": out.updated_at,
        "created": out.created,
    })))
}
```

Extend `edit_conflict` (edit.rs:76-92): add `rm_edit::EditError::InvalidPiece(_) | rm_edit::EditError::PieceScope(_) | rm_edit::EditError::UnknownRoute(_)` to the 400 arm, and `rm_edit::EditError::PieceNotCurrent(_) | rm_edit::EditError::PieceOverlap(_)` to the 409 arm.

Extend the `undo` handler's entity dispatch (edit.rs:200-234) — the handler already peeks `rm_db::edit_entity(conn, edit_id)` and dispatches stop vs station rows; add a polyline arm BEFORE the catch-all stop arm, keeping the existing arms and response shaping untouched:

```rust
        Some("polyline") => rm_edit::undo_polyline_piece(conn, edit_id, &editor, note)
            .map(|o| (o.edit_id, json!({
                "restored": { "piece_id": o.piece_id, "deleted": o.deleted }
            }))),
```

If the existing arms build their response objects directly rather than through a tuple, mirror that style: return `json!({ "edit_id": o.edit_id, "restored": { "piece_id": o.piece_id, "deleted": o.deleted } })` the same way they do. Update the handler's doc-comment: "Stop rows restore the stop version; station rows (rename/move) restore the station version; polyline rows restore or delete the manual piece."

`crates/rm-web/src/lib.rs` — in `router_with`, after the station-move route:

```rust
        .route(
            "/api/edit/polyline/{xno}/{direction}/piece",
            axum::routing::post(handlers::edit::edit_polyline_piece),
        )
```

`crates/rm-web/src/queries/routes.rs` — in `raw`, after the stops collection and before the final `Ok(json!(…))`, add the pieces query and merge the field:

```rust
    let mut stmt = conn.prepare(
        "SELECT piece_id, encoded, anchor_start_lat, anchor_start_lon,
                anchor_end_lat, anchor_end_lon, updated_at
         FROM polyline_piece WHERE xno = ?1 AND direction = ?2
         ORDER BY piece_id",
    )?;
    let manual_pieces: Vec<Value> = stmt
        .query_map(rusqlite::params![xno, dir], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, f64>(3)?,
                r.get::<_, f64>(4)?,
                r.get::<_, f64>(5)?,
                r.get::<_, String>(6)?,
            ))
        })?
        .filter_map(|row| {
            let (piece_id, encoded, aslat, aslon, aelat, aelon, updated_at) = row.ok()?;
            let pts = polyline::decode(&encoded).ok()?;
            if pts.len() < 2 {
                return None;
            }
            let points: Vec<Vec<f64>> =
                pts.iter().map(|p| vec![p.lat, p.lon]).collect();
            Some(json!({
                "piece_id": piece_id,
                "points": points,
                "anchor_start": [aslat, aslon],
                "anchor_end": [aelat, aelon],
                "updated_at": updated_at,
            }))
        })
        .collect();
```

and add `"manual_pieces": manual_pieces,` into the final response `json!({...})` object (read the fn tail to place it alongside `path`/`stops`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rm-web`
Expected: PASS (new tests + full rm-web suite).

- [ ] **Step 5: Commit**

```bash
git add crates/rm-web/src/lib.rs crates/rm-web/src/handlers/edit.rs crates/rm-web/src/queries/routes.rs crates/rm-web/tests/edit.rs
git commit -m "rm-web: polyline piece POST + raw manual_pieces + undo dispatch branch"
```

---

### Task 7: UI — `pieceEdit.js` vertex editing + wiring

**Files:**
- Create: `crates/rm-web/assets/js/pieceEdit.js`
- Modify: `crates/rm-web/assets/js/api.js` (one helper)
- Modify: `crates/rm-web/assets/js/layers.js` (`drawRaw` renders manual pieces + click-to-edit)
- Modify: `crates/rm-web/assets/js/app.js` (loadRaw wiring, edit-toggle teardown, floating button)
- Modify: `crates/rm-web/assets/js/panels.js` (`changeText` polyline branch at panels.js:343)
- Modify: `crates/rm-web/src/handlers/mod.rs` (serve the new asset)
- Test: none (house rule: JS has no test infra; verified by Task 8's e2e drive)

**Interfaces:**
- Consumes: Task 6's endpoints; `layers.raw` layerGroup (layers.js:19); `api.editToken` pattern (api.js:33).
- Produces: `startPieceEdit(routeId, dir, xno, raw, piece, onSaved)`, `stopPieceEdit()`, `pieceEditActive()` from `pieceEdit.js`; `drawRaw(raw, { onEditPiece })` signature change.

- [ ] **Step 1: api.js helper**

Append after `moveStation` (api.js:54):

```js
export const editPolylinePiece = (xno, dir, body) =>
  editFetch(`/api/edit/polyline/${encodeURIComponent(xno)}/${dir}/piece`, body);
```

- [ ] **Step 2: handlers/mod.rs asset route**

In `pub fn router()` (mod.rs:15), after the `/js/panels.js` route:

```rust
        .route("/js/pieceEdit.js", get(assets::serve))
```

- [ ] **Step 3: layers.js — render manual pieces**

Change `drawRaw` (layers.js:130) signature to `export function drawRaw(raw, { onEditPiece } = {})` and append before its closing brace:

```js
  for (const p of raw?.manual_pieces ?? []) {
    const line = L.polyline(p.points, {
      color: '#f59e0b', weight: 4, opacity: 0.95,
    })
      .bindTooltip(`manual piece #${p.piece_id}`, { direction: 'top' })
      .addTo(layers.raw);
    if (onEditPiece) line.on('click', () => onEditPiece(p));
  }
```

- [ ] **Step 4: pieceEdit.js — the interaction**

Create `crates/rm-web/assets/js/pieceEdit.js`:

```js
// Manual src-polyline piece editing (phase 1). One armed session at a time:
// click two raw vertices = span (the server enforces chainage order), then
// drag/insert/delete the span's vertices, save = POST .../piece. The server
// owns polyline math; this file only collects [lat,lon] pairs. Esc /
// edit-off tear down. src coords arrive GeoJSON lon-first and are
// normalized to [lat,lon] exactly once, here.
import { editPolylinePiece } from './api.js';

const VTX = (color) =>
  L.divIcon({
    className: '',
    html: `<div style="width:10px;height:10px;border-radius:50%;background:${color};border:2px solid #fff;box-shadow:0 0 2px #0008;"></div>`,
    iconSize: [10, 10],
    iconAnchor: [5, 5],
  });

let session = null;
// { map, group, line, src, routeId, dir, xno, pieceId, expectedUpdatedAt,
//   a, b, pts, btn, onSaved }

export function pieceEditActive() {
  return session != null;
}

export function stopPieceEdit() {
  if (!session) return;
  session.map.doubleClickZoom.enable();
  session.group.remove();
  session.btn?.remove();
  session = null;
}

// Full rebuild of the session layer. NEVER called while a vertex marker is
// mid-drag: drag updates the line in place via setLatLngs; dragend (or any
// discrete op) triggers the rebuild. clearLayers() during a drag destroys
// the dragged marker and kills the gesture.
function render(s) {
  s.group.clearLayers();
  s.line = null;
  // dimmed full src line (already [lat,lon])
  L.polyline(s.src, { color: '#7c3aed', weight: 2, dashArray: '2 7', opacity: 0.4 })
    .bindTooltip('src (raw)').addTo(s.group);
  // raw vertices: click = anchor pick
  s.src.forEach((p, i) => {
    L.marker(p, { icon: VTX('#7c3aed') }).addTo(s.group).on('click', () => pick(s, i));
  });
  if (s.b == null) return; // span not cut yet — nothing editable
  // current piece geometry, kept as a reference for in-place drag updates
  s.line = L.polyline(s.pts, { color: '#f59e0b', weight: 4, opacity: 0.95 }).addTo(s.group);
  // span vertices; endpoints are the anchors (pinned), interior are editable
  s.pts.forEach((p, i) => {
    if (i === 0 || i === s.pts.length - 1) {
      L.marker(p, { icon: VTX('#334155') }).addTo(s.group)
        .bindTooltip(i === 0 ? 'anchor start' : 'anchor end');
      return;
    }
    const m = L.marker(p, { icon: VTX('#f59e0b'), draggable: true }).addTo(s.group);
    m.on('drag', (e) => {
      s.pts[i] = [e.latlng.lat, e.latlng.lng];
      s.line.setLatLngs(s.pts);
    });
    m.on('dragend', () => render(s));
    m.on('dblclick', () => {
      if (s.pts.length > 2) { s.pts.splice(i, 1); render(s); }
    });
  });
  // midpoint insert handles
  for (let i = 0; i < s.pts.length - 1; i++) {
    const mid = [(s.pts[i][0] + s.pts[i + 1][0]) / 2, (s.pts[i][1] + s.pts[i + 1][1]) / 2];
    L.marker(mid, {
      icon: L.divIcon({
        className: '',
        html: '<div style="width:8px;height:8px;background:#fff;border:2px solid #f59e0b;border-radius:50%;"></div>',
        iconSize: [8, 8], iconAnchor: [4, 4],
      }),
    }).addTo(s.group).on('click', () => {
      s.pts.splice(i + 1, 0, mid);
      render(s);
    });
  }
}

function pick(s, i) {
  if (s.a == null || s.b != null) {
    // fresh pick (or re-pick after a complete span)
    s.a = i; s.b = null; s.pts = [s.src[i]];
  } else if (i !== s.a) {
    const [lo, hi] = i > s.a ? [s.a, i] : [i, s.a];
    s.a = lo; s.b = hi;
    s.pts = s.src.slice(lo, hi + 1);
  }
  render(s);
}

export function startPieceEdit(routeId, dir, xno, raw, piece, onSaved) {
  const coords = raw?.path?.geometry?.coordinates ?? [];
  if (coords.length < 2) return;
  stopPieceEdit();
  const map = window.__rmMap;
  const src = coords.map(([lon, lat]) => [lat, lon]); // GeoJSON lon-first → bus [lat,lon], once
  const group = L.layerGroup().addTo(map);
  map.doubleClickZoom.disable();
  session = {
    map, group, line: null, src, routeId, dir, xno,
    pieceId: piece?.piece_id ?? null,
    expectedUpdatedAt: piece?.updated_at ?? null,
    a: null, b: null, pts: [], btn: null, onSaved,
  };
  // editing an existing piece: its endpoints snap to the nearest raw
  // vertices so the span logic has indices; the piece's own geometry is
  // what gets edited (anchors re-sent from pts endpoints — pinned, so they
  // still equal the stored anchors within SEAM_TOL_M)
  if (piece) {
    const near = (pt) => src.reduce(
      (best, q, i) => {
        const d = (q[0] - pt[0]) ** 2 + (q[1] - pt[1]) ** 2;
        return d < best.d ? { d, i } : best;
      }, { d: Infinity, i: 0 }).i;
    session.a = near(piece.anchor_start);
    session.b = near(piece.anchor_end);
    if (session.a > session.b) [session.a, session.b] = [session.b, session.a];
    session.pts = piece.points.map((p) => [p[0], p[1]]);
  }
  const btn = document.createElement('div');
  btn.style.cssText = 'position:absolute;bottom:56px;right:12px;z-index:1000;display:flex;gap:6px;';
  btn.innerHTML = `
    <button id="piece-save" style="padding:4px 10px;cursor:pointer;">✓ 存_piece</button>
    <button id="piece-cancel" style="padding:4px 10px;cursor:pointer;">✕</button>`;
  document.body.appendChild(btn);
  session.btn = btn;
  btn.querySelector('#piece-cancel').onclick = () => stopPieceEdit();
  btn.querySelector('#piece-save').onclick = () => save(session);
  render(session);
}

async function save(s) {
  if (s.b == null || s.pts.length < 2) return;
  const note = prompt('piece note:');
  if (note == null) return; // cancelled
  try {
    const out = await editPolylinePiece(s.xno, s.dir, {
      points: s.pts,
      anchor_start: s.pts[0],
      anchor_end: s.pts[s.pts.length - 1],
      piece_id: s.pieceId ?? undefined,
      expected_updated_at: s.expectedUpdatedAt ?? undefined,
      note,
    });
    const done = s.onSaved;
    stopPieceEdit();
    done?.(out);
  } catch (e) {
    s.onSaved?.(null, e);
  }
}
```

(Endpoints are never draggable — the render loop pins index 0 and the last index — so `anchor_start`/`anchor_end` re-sent from `pts` endpoints always match the stored anchors. `window.__rmMap` is set by app.js in Step 5.)

- [ ] **Step 5: app.js wiring**

At the top of `app.js`, import:

```js
import { startPieceEdit, stopPieceEdit, pieceEditActive } from './pieceEdit.js';
```

Expose the map + status for pieceEdit (after the map is created — the `L.map` call near the top of app.js; reuse whatever variable holds it and the existing `status()` fn):

```js
window.__rmMap = map;
window.__rmStatus = status;
```

In `loadRaw` (app.js:200-208), pass the edit hook and cache the payload:

```js
async function loadRaw(routeId, dir) {
  try {
    const v = await api.routeRaw(routeId, dir);
    if (selectedRoute?.route_id !== routeId || selectedRoute.dir !== dir) return; // stale
    lastRaw = { routeId, dir, v };
    drawRaw(v, {
      onEditPiece: (piece) => {
        if (!editMode || pieceEditActive()) return;
        startPieceEdit(routeId, dir, selectedRoute.xno, v, piece, onPieceSaved);
      },
    });
  } catch (e) {
    status(`raw load failed: ${e.message}`);
  }
}
```

Add module state + refresh + the floating 修線 entry (near `lastRouteDraw` at app.js:39):

```js
let lastRaw = null; // { routeId, dir, v } — for piece-edit entry

function onPieceSaved(out, err) {
  if (err) { status(`piece save failed: ${err.message}`); return; }
  status(out.created ? `piece #${out.piece_id} created` : `piece #${out.piece_id} updated`);
  if (lastRaw && document.getElementById('layer-raw').checked) loadRaw(lastRaw.routeId, lastRaw.dir);
}
```

(`selectedRoute.xno` — if the route detail payload does not carry `xno`, add it to `queries::routes::detail`'s route json in the same commit: `"xno": <substr(ext_uid,5)>`, exactly like `raw` resolves it at routes.rs:416-424. Check with `grep -n "xno" crates/rm-web/src/queries/routes.rs` before editing.)

Add the 修線 button next to the edit toggle (`document.getElementById('edit-toggle')` — no topbar element exists; append to its parent node). Visible only when edit mode + raw layer + a raw payload are up, and torn down with them:

```js
  const pieceBtn = document.createElement('button');
  pieceBtn.id = 'piece-edit-btn';
  pieceBtn.textContent = '修線';
  pieceBtn.style.cssText = 'display:none;margin-left:6px;';
  pieceBtn.addEventListener('click', () => {
    if (!lastRaw || pieceEditActive()) return;
    startPieceEdit(lastRaw.routeId, lastRaw.dir, selectedRoute.xno, lastRaw.v, null, onPieceSaved);
  });
  document.getElementById('edit-toggle').parentElement.appendChild(pieceBtn);
  const syncPieceBtn = () => {
    pieceBtn.style.display = editMode && document.getElementById('layer-raw').checked && lastRaw ? '' : 'none';
    if (!editMode) stopPieceEdit();
  };
  document.getElementById('layer-raw').addEventListener('change', syncPieceBtn);
  // call syncPieceBtn() inside the existing edit-toggle handler body (where editMode flips)
```

(Wire `syncPieceBtn` into the existing edit-toggle handler body; Esc teardown: `document.addEventListener('keydown', (e) => { if (e.key === 'Escape') stopPieceEdit(); });` — one listener, top level.)

- [ ] **Step 6: panels.js — changeText branch**

In `changeText` (panels.js:343-356), add before `return parts.join(' · ');`:

```js
  if (c.kind === 'polyline_piece') {
    parts.push(`線 #${c.piece_id} ${c.xno}/${c.direction}${c.encoded?.old != null ? ' 改' : ' 新增'}`);
  }
```

- [ ] **Step 7: Verify manually**

```bash
cargo build -r -p rm-web
cargo run -r -p rm-web -- --db data/taipei.db --edit --edit-tokens herry:dev --port 8811
```

Browser: toggle edit → enable raw layer → select a route whose raw layer draws → 修線 → click two vertices → drag one → ✓ 存_piece → note → orange piece appears; refresh keeps it; ↩ in the log panel removes it. Restart the server after any JS edit (`include_str!` bake).

- [ ] **Step 8: Commit**

```bash
git add crates/rm-web/assets/js/pieceEdit.js crates/rm-web/assets/js/api.js crates/rm-web/assets/js/layers.js crates/rm-web/assets/js/app.js crates/rm-web/assets/js/panels.js crates/rm-web/src/handlers/mod.rs crates/rm-web/src/queries/routes.rs
git commit -m "rm-web: pieceEdit UI — span pick, vertex drag/insert/delete, save via piece POST"
```

---

### Task 8: E2E drive on a DB copy

**Files:**
- No source changes (verification only); scratch files go in a temp dir, not the repo.

**Interfaces:**
- Consumes: everything shipped in Tasks 1-7, built release.

- [ ] **Step 1: Build + serve a DB copy on a fresh port**

```bash
cargo build -r -p rm-web
cp data/taipei.db /tmp/rm-polyline-e2e.db
cargo run -r -p rm-web -- --db /tmp/rm-polyline-e2e.db --edit --edit-tokens herry:dev --port 8823 &
# verify it is YOUR db: batch timestamp must match the copy
curl -s localhost:8823/api/stats | jq .batch_ts
```

- [ ] **Step 2: Find a drive target**

```bash
sqlite3 /tmp/rm-polyline-e2e.db "
  SELECT r.route_id, srp.xno, srp.direction
  FROM src_route_point srp JOIN route r ON r.ext_uid = 'tcg:' || srp.xno
  WHERE srp.encoded != '' LIMIT 5;"
```

Pick one row; remember (route_id, xno, direction).

- [ ] **Step 3: API-level drive (curl)**

```bash
T='authorization: Bearer dev'
# create
curl -s -X POST localhost:8823/api/edit/polyline/<xno>/<dir>/piece -H "$T" -H 'content-type: application/json' \
  -d '{"points":[[24.14,120.68],[24.145,120.682]],"anchor_start":[24.14,120.68],"anchor_end":[24.145,120.682],"note":"e2e"}'
# → expect edit_id/piece_id/updated_at/created:true  (if anchors are off the src line → 400; that validates guard 4/5 too)
curl -s localhost:8823/api/routes/<route_id>/directions/<dir>/raw | jq '.manual_pieces'
# race: re-POST same piece_id with stale expected_updated_at → expect 409
# undo
curl -s -X POST localhost:8823/api/edit/undo/<edit_id> -H "$T" -H 'content-type: application/json' -d '{"note":"undo e2e"}'
curl -s localhost:8823/api/routes/<route_id>/directions/<dir>/raw | jq '.manual_pieces | length'   # expect 0
```

- [ ] **Step 4: Browser drive (playwright, house pattern)**

sessionStorage token BEFORE boot; toggle edit AFTER boot; enable the raw layer; select the route; 修線 → click two adjacent raw vertices → drag a span vertex → ✓ 存_piece → note prompt → status shows `piece #N created` → orange piece visible → reload page → piece persists → open route panel edit log → ↩ on the piece row → piece gone. Screenshot the edited span for the task report.

- [ ] **Step 5: Cleanup + report**

```bash
kill %1; rm -f /tmp/rm-polyline-e2e.db
```

Report: which route/direction was driven, API transcript (create/409/undo), browser steps observed, any guard that fired.

- [ ] **Step 6: No commit** (nothing in the repo changed; the server run is ephemeral)
