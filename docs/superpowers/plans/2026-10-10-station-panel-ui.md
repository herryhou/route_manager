# Station Panel UI (Scenario 7) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire the shipped station primitives (rename / move / undo) into rm-web: two new POST endpoints, an entity-dispatched undo route, `station_version_id` in the station payload, and ✎/移動/↩歷史 buttons on the station panel.

**Architecture:** Handlers in `handlers/edit.rs` reuse `Editor` + `ApiJson` + `run_db_write` + the existing exhaustive `edit_conflict` map; no rm-edit changes, one tiny rm-db read (`edit_entity`). The undo route peeks the log row's entity and dispatches stop vs station. UI replays the stop-panel patterns: mini-btns under the `editable` gate, `armPlace` one-shot map click, `editLogPanel` with the exact-undo hint.

**Tech Stack:** Rust (axum, rusqlite), vanilla ES modules served via `include_str!`.

**Spec:** `docs/superpowers/specs/2026-10-10-station-panel-ui-design.md`

## Global Constraints

- Release builds for any heavy binary run: `cargo run -r -p <crate>` (CLAUDE.md) — applies to the e2e drive.
- No rm-edit changes. rm-db changes limited to `edit_entity` + its re-export + its unit test.
- Wire level strings: `"station"` / `"matching"` / `"all"` → `rm_edit::RenameLevel::{StationOnly, MatchingPoles, AllPoles}`; anything else → 400.
- Endpoint responses: rename/move → `{edit_id, new_station_version_id}`; station undo → `{edit_id, restored_station_version_id}`; the stop undo response `{edit_id, new_stop_version_id}` stays byte-identical.
- `note` optional server-side (`unwrap_or("")`), required by the UI (empty note cancels).
- No rustfmt commit — repo is not fmt-clean (~130 pre-existing diff sites); do not run `cargo fmt`.
- Workspace `cargo test` green at the end of every task (255 tests baseline + new).
- Commit messages end with `Co-Authored-By: Claude Code <noreply@anthropic.com>`.

## Review Focus

- **Concurrent edit race:** panel loaded, someone else edits the station, then this editor POSTs → 409 `StationNotCurrent` must surface as a status-bar message, never a crash. Pinned by `rename_stale_svi_conflicts` / `move_stale_svi_conflicts` + `editFail` (UI, e2e).
- **Stale undo target:** an older station edit undone after a newer one → 409 `NotUndoable`, and the UI's exact-undo hint (newest non-undone row only) hides the dead click. Pinned by rm-edit `plan_rejects_undo_action` + `editLogPanel` logic (existing).
- **CJK / exact-byte names:** rename stores the name verbatim, no trimming (`"台北車站 "` stays), and `MatchingPoles` matches exact bytes only. Pinned by rm-edit `matching_poles_with_trailing_space_does_not_match` (existing); UI passes the prompt string through untouched (e2e).
- **Zero-match rename:** `matching` level with no byte-equal poles is a SUCCESS (station-only effect), not an error. Pinned by `rename_level_maps_to_pole_outcomes` (Task 2).
- **Unknown edit id on the undo route:** peek returns `None` → routed down the stop path → 409, never a 500/panic. Pinned by `undo_unknown_edit_still_conflicts` (Task 4).

---

### Task 1: `station_version_id` in the station detail payload

**Files:**
- Modify: `crates/rm-web/src/queries/stops.rs:198-272` (`station_detail`)
- Test: `crates/rm-web/tests/edit.rs` (append)

**Interfaces:**
- Consumes: existing `station_detail(conn, station_id) -> Result<Option<Value>>`.
- Produces: `station` object in the JSON payload carries `"station_version_id": i64` — Tasks 2/3 key edits on it; UI Task 5 reads `d.station.station_version_id`.

- [ ] **Step 1: Write the failing test**

Append to `crates/rm-web/tests/edit.rs` (helpers `edit_state`, `get_json`, and fixture seed `station_version_id = 1` already exist in that file):

```rust
#[tokio::test]
async fn station_detail_exposes_station_version_id() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    let app = rm_web::router_with(state);
    let v = get_json(&app, "/api/stations/1").await;
    assert_eq!(v["station"]["station_version_id"].as_i64(), Some(1), "{v}");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rm-web --test edit station_detail_exposes_station_version_id`
Expected: FAIL — `station_version_id` is null (`None`), assertion prints `{v}`.

- [ ] **Step 3: Implement**

In `crates/rm-web/src/queries/stops.rs` `station_detail`, make three edits.

The first query's SELECT gains one column at the end, and the tuple gains an 11th field:

```rust
    let found = match conn.query_row(
        "SELECT st.station_id, stv.station_name, stv.latitude, stv.longitude,
                sg.station_group_id, sgv.station_group_name, sgv.source,
                stb.bearing_deg, stb.max_spread_deg, stb.member_count,
                stv.station_version_id
         FROM station st
         JOIN station_version stv
           ON stv.station_id = st.station_id AND stv.valid_to IS NULL
         JOIN station_group sg ON sg.station_group_id = stv.station_group_id
         JOIN station_group_version sgv
           ON sgv.station_group_id = sg.station_group_id AND sgv.valid_to IS NULL
         LEFT JOIN station_bearing stb ON stb.station_id = st.station_id
         WHERE st.station_id = ?1",
        [station_id],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, f64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, Option<f64>>(7)?,
                r.get::<_, Option<f64>>(8)?,
                r.get::<_, Option<i64>>(9)?,
                r.get::<_, i64>(10)?,
            ))
        },
    ) {
```

The returned `"station"` object gains the key (rest of the json! unchanged):

```rust
        "station": {
            "station_id": found.0,
            "station_name": found.1,
            "lat": found.2,
            "lon": found.3,
            "station_version_id": found.10,
        },
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p rm-web --test edit station_detail_exposes_station_version_id`
Expected: PASS

- [ ] **Step 5: Full workspace test + commit**

Run: `cargo test --workspace`
Expected: green (255 + 1).

```bash
git add crates/rm-web/src/queries/stops.rs crates/rm-web/tests/edit.rs
git commit -m "rm-web: station detail payload exposes station_version_id

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 2: Station rename endpoint

**Files:**
- Modify: `crates/rm-web/src/handlers/edit.rs` (append handler + body struct)
- Modify: `crates/rm-web/src/lib.rs:47-61` (`router_with` — add route)
- Test: `crates/rm-web/tests/edit.rs` (append helper + `station_fixture` + tests)

**Interfaces:**
- Consumes: `rm_edit::rename_station(conn, StationRename { station_version_id, new_name, level }, editor, note) -> Result<RenameOutcome { edit_id, new_station_version_id, poles }>`; `rm_edit::RenameLevel::{StationOnly, MatchingPoles, AllPoles}` (all re-exported via `pub use rename::*`). Errors: `EmptyNewName`/`NoOpRename` → 400, `StationNotCurrent` → 409 via `edit_conflict`.
- Produces: `POST /api/edit/station/{station_version_id}` with body `{new_name, level, note?}` → `{edit_id, new_station_version_id}`. Task 4's dispatch test calls this endpoint.

- [ ] **Step 1: Write the failing tests + helpers**

Append to `crates/rm-web/tests/edit.rs` (after the existing `regroup` helper):

```rust
/// fixture + the station_version id_sequence seed station edits mint from
/// (mint reads id_sequence; the plain fixture only seeds stop_version).
fn station_fixture(conn: &Connection) {
    fixture(conn);
    conn.execute(
        "INSERT INTO id_sequence (table_name, next_value) VALUES ('station_version', 2)",
        [],
    )
    .unwrap();
}

async fn rename_st(state: &rm_web::state::AppState, svi: i64, body: &str) -> axum::response::Response {
    rm_web::router_with(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&format!("/api/edit/station/{svi}"))
                .header("authorization", "Bearer sekrit")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn rename_all_poles_mints_station_and_pole_versions() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    {
        let conn = rm_db::open(&dir.path().join("t.db")).unwrap();
        station_fixture(&conn);
    }
    let res = rename_st(&state, 1, r#"{"new_name": "N", "level": "all", "note": "r"}"#).await;
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["new_station_version_id"].as_i64(), Some(2), "{v}");
    let conn = rm_db::open_readonly(&dir.path().join("t.db")).unwrap();
    // station: new manual version is current, named N
    let (name, src): (String, String) = conn
        .query_row(
            "SELECT station_name, source FROM station_version WHERE valid_to IS NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((name.as_str(), src), ("N", "manual"));
    // pole followed the rename (minted version 11 is now current)
    let pole: String = conn
        .query_row(
            "SELECT stop_name FROM stop_version WHERE stop_id = 1 AND valid_to IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pole, "N");
    // log row carries the rename kind + level
    let (kind, level): (String, String) = conn
        .query_row(
            "SELECT json_extract(changes, '$.kind'), json_extract(changes, '$.level') FROM edit_log",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((kind.as_str(), level.as_str()), ("station_rename", "AllPoles"));
}

#[tokio::test]
async fn rename_level_maps_to_pole_outcomes() {
    for (level, want_pole) in [("station", "Old"), ("matching", "Old"), ("all", "N")] {
        let dir = tempfile::tempdir().unwrap();
        let state = edit_state(&dir);
        {
            let conn = rm_db::open(&dir.path().join("t.db")).unwrap();
            station_fixture(&conn);
        }
        let res = rename_st(
            &state,
            1,
            &format!(r#"{{"new_name": "N", "level": "{level}", "note": "x"}}"#),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK, "level {level}");
        drop(res);
        let conn = rm_db::open_readonly(&dir.path().join("t.db")).unwrap();
        let pole: String = conn
            .query_row(
                "SELECT stop_name FROM stop_version WHERE stop_id = 1 AND valid_to IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pole, want_pole, "level {level}");
    }
}

#[tokio::test]
async fn rename_rejects_bad_level_empty_and_noop() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    for (body, want) in [
        (r#"{"new_name": "N", "level": "poles", "note": "x"}"#, StatusCode::BAD_REQUEST),
        (r#"{"new_name": "  ", "level": "all", "note": "x"}"#, StatusCode::BAD_REQUEST),
        (r#"{"new_name": "S", "level": "all", "note": "x"}"#, StatusCode::BAD_REQUEST),
        (r#"{"level": "all"}"#, StatusCode::BAD_REQUEST),
    ] {
        let res = rename_st(&state, 1, body).await;
        assert_eq!(res.status(), want, "body {body}");
    }
    // nothing was written
    let conn = rm_db::open_readonly(&dir.path().join("t.db")).unwrap();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM edit_log", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn rename_stale_svi_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    let res = rename_st(&state, 999, r#"{"new_name": "N", "level": "all", "note": "x"}"#).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rm-web --test edit rename_`
Expected: FAIL — 404s (route not mounted).

- [ ] **Step 3: Implement**

In `crates/rm-web/src/handlers/edit.rs`, append (note the handler's `rm_edit::` qualification — same name-sharing pattern as `edit_stop`):

```rust
#[derive(Deserialize)]
pub struct StationRenameBody {
    new_name: String,
    level: String,
    note: Option<String>,
}

/// POST /api/edit/station/{station_version_id} — rename the station, with
/// an editor-chosen level for how far the rename reaches into pole names.
pub async fn rename_station(
    Path(station_version_id): Path<i64>,
    State(state): State<AppState>,
    Editor(editor): Editor,
    ApiJson(b): ApiJson<StationRenameBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let level = match b.level.as_str() {
        "station" => rm_edit::RenameLevel::StationOnly,
        "matching" => rm_edit::RenameLevel::MatchingPoles,
        "all" => rm_edit::RenameLevel::AllPoles,
        other => return Err(ApiError::bad_request(&format!("unknown level {other:?}"))),
    };
    let out = run_db_write(state, move |conn| {
        rm_edit::rename_station(
            conn,
            rm_edit::StationRename {
                station_version_id,
                new_name: b.new_name,
                level,
            },
            &editor,
            b.note.as_deref().unwrap_or(""),
        )
    })
    .await
    .map_err(edit_conflict)?;
    Ok(Json(json!({
        "edit_id": out.edit_id,
        "new_station_version_id": out.new_station_version_id,
    })))
}
```

In `crates/rm-web/src/lib.rs` `router_with`, inside the `edit_enabled` branch, add:

```rust
        .route(
            "/api/edit/station/{station_version_id}",
            axum::routing::post(handlers::edit::rename_station),
        )
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rm-web --test edit rename_`
Expected: PASS (4 tests).

- [ ] **Step 5: Full workspace test + commit**

Run: `cargo test --workspace`
Expected: green.

```bash
git add crates/rm-web/src/handlers/edit.rs crates/rm-web/src/lib.rs crates/rm-web/tests/edit.rs
git commit -m "rm-web: POST /api/edit/station/{svi} — rename with pole level

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 3: Station move endpoint

**Files:**
- Modify: `crates/rm-web/src/handlers/edit.rs` (append handler + body struct)
- Modify: `crates/rm-web/src/lib.rs:47-61` (add route)
- Test: `crates/rm-web/tests/edit.rs` (append)

**Interfaces:**
- Consumes: `rm_edit::move_station(conn, StationMove { station_version_id, latitude, longitude }, editor, note) -> Result<MoveOutcome { edit_id, new_station_version_id }>` (re-exported via `pub use station_move::*`). NOTE the field names: `latitude`/`longitude`, not `new_*` — those live on `StationMovePlan`.
- Produces: `POST /api/edit/station/{station_version_id}/move` with body `{lat, lon, note?}` → `{edit_id, new_station_version_id}`.

- [ ] **Step 1: Write the failing tests**

Append to `crates/rm-web/tests/edit.rs`:

```rust
async fn move_st(state: &rm_web::state::AppState, svi: i64, body: &str) -> axum::response::Response {
    rm_web::router_with(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&format!("/api/edit/station/{svi}/move"))
                .header("authorization", "Bearer sekrit")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn move_station_mints_version_zero_marks_and_leaves_poles() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    {
        let conn = rm_db::open(&dir.path().join("t.db")).unwrap();
        station_fixture(&conn);
    }
    let res = move_st(&state, 1, r#"{"lat": 24.001, "lon": 120.001, "note": "m"}"#).await;
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["new_station_version_id"].as_i64(), Some(2), "{v}");
    let conn = rm_db::open_readonly(&dir.path().join("t.db")).unwrap();
    let (lat, lon, src): (f64, f64, String) = conn
        .query_row(
            "SELECT latitude, longitude, source FROM station_version WHERE valid_to IS NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!((lat, lon, src), (24.001, 120.001, "manual".to_string()));
    // poles stay put — the pole's current version is untouched
    let (plat, plon): (f64, f64) = conn
        .query_row(
            "SELECT latitude, longitude FROM stop_version WHERE stop_id = 1 AND valid_to IS NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((plat, plon), (24.0, 120.0));
    // station coords derive nothing: ZERO stale marks
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
    // log row: move kind + coord old/new pairs
    let (kind, lat_new): (String, f64) = conn
        .query_row(
            "SELECT json_extract(changes, '$.kind'), json_extract(changes, '$.latitude.new') FROM edit_log",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((kind.as_str(), lat_new), ("station_move", 24.001));
}

#[tokio::test]
async fn move_station_validates_body_and_stale() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    for (body, want) in [
        (r#"{"lat": 91.0, "lon": 120.0, "note": "x"}"#, StatusCode::BAD_REQUEST),
        (r#"{"lat": 24.0, "lon": -181.0, "note": "x"}"#, StatusCode::BAD_REQUEST),
        (r#"{"lat": 24.0, "note": "x"}"#, StatusCode::BAD_REQUEST),
    ] {
        let res = move_st(&state, 1, body).await;
        assert_eq!(res.status(), want, "body {body}");
    }
    let res = move_st(&state, 999, r#"{"lat": 24.0, "lon": 120.0, "note": "x"}"#).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rm-web --test edit move_station_`
Expected: FAIL — 404s (route not mounted).

- [ ] **Step 3: Implement**

In `crates/rm-web/src/handlers/edit.rs`, append:

```rust
#[derive(Deserialize)]
pub struct StationMoveBody {
    lat: f64,
    lon: f64,
    note: Option<String>,
}

/// POST /api/edit/station/{station_version_id}/move — move the station's
/// own coords. Poles stay where they are; nothing derives from station
/// coords, so no stale marks fire.
pub async fn move_station(
    Path(station_version_id): Path<i64>,
    State(state): State<AppState>,
    Editor(editor): Editor,
    ApiJson(b): ApiJson<StationMoveBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if !(-90.0..=90.0).contains(&b.lat) {
        return Err(ApiError::bad_request("lat out of range"));
    }
    if !(-180.0..=180.0).contains(&b.lon) {
        return Err(ApiError::bad_request("lon out of range"));
    }
    let out = run_db_write(state, move |conn| {
        rm_edit::move_station(
            conn,
            rm_edit::StationMove {
                station_version_id,
                latitude: b.lat,
                longitude: b.lon,
            },
            &editor,
            b.note.as_deref().unwrap_or(""),
        )
    })
    .await
    .map_err(edit_conflict)?;
    Ok(Json(json!({
        "edit_id": out.edit_id,
        "new_station_version_id": out.new_station_version_id,
    })))
}
```

In `crates/rm-web/src/lib.rs` `router_with`, inside the `edit_enabled` branch, add:

```rust
        .route(
            "/api/edit/station/{station_version_id}/move",
            axum::routing::post(handlers::edit::move_station),
        )
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rm-web --test edit move_station_`
Expected: PASS (2 tests).

- [ ] **Step 5: Full workspace test + commit**

Run: `cargo test --workspace`
Expected: green.

```bash
git add crates/rm-web/src/handlers/edit.rs crates/rm-web/src/lib.rs crates/rm-web/tests/edit.rs
git commit -m "rm-web: POST /api/edit/station/{svi}/move — station coords move

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 4: `edit_entity` + entity-dispatched undo route

**Files:**
- Modify: `crates/rm-db/src/edit.rs` (append `edit_entity` + unit test)
- Modify: `crates/rm-db/src/lib.rs:18-29` (re-export list)
- Modify: `crates/rm-web/src/handlers/edit.rs:196-213` (`undo` handler)
- Test: `crates/rm-web/tests/edit.rs` (append)

**Interfaces:**
- Consumes: `rm_edit::undo_station(conn, edit_id, editor, note) -> Result<UndoOutcome { edit_id, restored_station_version_id }>`; `rm_edit::undo_stop(...) -> Result<UndoOutcome { edit_id, new_stop_version_id }>` (already used).
- Produces: `rm_db::edit_entity(conn, edit_id) -> Result<Option<String>>`; the undo route now answers station rows with `{edit_id, restored_station_version_id}` and stop rows byte-identically as before. Task 5's `undoStationEdit` consumes the station shape.

- [ ] **Step 1: Write the failing tests**

In `crates/rm-db/src/edit.rs`, inside the existing `mod tests`, append:

```rust
    #[test]
    fn edit_entity_returns_entity_or_none() {
        let conn = open_in_memory().unwrap();
        let old = fixture(&conn);
        let out = apply_stop_edit(&conn, &StopEditOp {
            stop_id: 1, stop_version_id: old, stop_name: Some("N".into()), ..Default::default()
        }, "h", "n").unwrap();
        assert_eq!(edit_entity(&conn, out.edit_id).unwrap().as_deref(), Some("stop"));
        assert!(edit_entity(&conn, 999).unwrap().is_none());
    }
```

Append to `crates/rm-web/tests/edit.rs`:

```rust
async fn undo_edit(state: &rm_web::state::AppState, edit_id: i64) -> axum::response::Response {
    rm_web::router_with(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&format!("/api/edit/undo/{edit_id}"))
                .header("authorization", "Bearer sekrit")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"note": "rev"}"#.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn undo_route_dispatches_station_rename_rows() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    {
        let conn = rm_db::open(&dir.path().join("t.db")).unwrap();
        station_fixture(&conn);
    }
    let res = rename_st(&state, 1, r#"{"new_name": "N", "level": "all", "note": "r"}"#).await;
    assert_eq!(res.status(), StatusCode::OK);
    let edit_id = body_json(res).await["edit_id"].as_i64().unwrap();

    let res = undo_edit(&state, edit_id).await;
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["restored_station_version_id"].as_i64(), Some(1), "{v}");
    // readback: station 1 current again named S, pole 10 current named Old
    let conn = rm_db::open_readonly(&dir.path().join("t.db")).unwrap();
    let (name, cur): (String, i64) = conn
        .query_row(
            "SELECT station_name, station_version_id FROM station_version WHERE valid_to IS NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((name.as_str(), cur), ("S", 1));
    let pole: String = conn
        .query_row(
            "SELECT stop_name FROM stop_version WHERE valid_to IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pole, "Old");
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM edit_log WHERE action = 'undo'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

#[tokio::test]
async fn undo_unknown_edit_still_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    let state = edit_state(&dir);
    let res = undo_edit(&state, 999).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
}
```

(The stop path's response shape and behavior stay pinned by the existing `log_and_undo_roundtrip` test — do not modify it.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p rm-db edit_entity_returns` and `cargo test -p rm-web --test edit undo_route_dispatches`
Expected: rm-db FAIL (no `edit_entity`); rm-web FAIL — `restored_station_version_id` null (station row goes down the stop path today).

- [ ] **Step 3: Implement**

In `crates/rm-db/src/edit.rs` (near `entity_history`), append:

```rust
/// Which entity an edit_log row belongs to (`"stop"`, `"station"`); `None`
/// for unknown ids. The undo route's dispatcher peek.
pub fn edit_entity(conn: &Connection, edit_id: i64) -> Result<Option<String>> {
    conn.query_row(
        "SELECT entity FROM edit_log WHERE edit_id = ?1",
        [edit_id],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .map_err(Into::into)
}
```

In `crates/rm-db/src/lib.rs` re-export list, add `edit_entity` to the `pub use edit::{ ... }` set (alongside `entity_history`).

In `crates/rm-web/src/handlers/edit.rs`, replace the `undo` handler body:

```rust
/// POST /api/edit/undo/{edit_id} — revert an edit by restoring the version
/// that preceded it. Stop rows restore the stop version; station rows
/// (rename/move) restore the station version. Unknown ids route down the
/// stop path and surface as its 409 NotUndoable, as before.
pub async fn undo(
    Path(edit_id): Path<i64>,
    State(state): State<AppState>,
    Editor(editor): Editor,
    ApiJson(b): ApiJson<UndoBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let is_station = run_db(state.clone(), move |conn| {
        rm_db::edit_entity(conn, edit_id)
    })
    .await?
    .as_deref()
        == Some("station");
    let out = run_db_write(state, move |conn| {
        if is_station {
            rm_edit::undo_station(conn, edit_id, &editor, b.note.as_deref().unwrap_or(""))
                .map(|o| (o.edit_id, o.restored_station_version_id))
        } else {
            rm_edit::undo_stop(conn, edit_id, &editor, b.note.as_deref().unwrap_or(""))
                .map(|o| (o.edit_id, o.new_stop_version_id))
        }
    })
    .await
    .map_err(edit_conflict)?;
    if is_station {
        Ok(Json(json!({
            "edit_id": out.0,
            "restored_station_version_id": out.1,
        })))
    } else {
        Ok(Json(json!({
            "edit_id": out.0,
            "new_stop_version_id": out.1,
        })))
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rm-db edit_entity_returns` and `cargo test -p rm-web --test edit undo_`
Expected: PASS (rm-db 1; rm-web new 2 + existing stop undo tests still green).

- [ ] **Step 5: Full workspace test + commit**

Run: `cargo test --workspace`
Expected: green.

```bash
git add crates/rm-db/src/edit.rs crates/rm-db/src/lib.rs crates/rm-web/src/handlers/edit.rs crates/rm-web/tests/edit.rs
git commit -m "rm-web: undo route dispatches stop vs station rows via rm_db::edit_entity

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

---

### Task 5: Station panel UI — buttons, flows, edit log

**Files:**
- Modify: `crates/rm-web/assets/js/api.js:49-52` (append two wrappers)
- Modify: `crates/rm-web/assets/js/panels.js:343-355` (`changeText`), `527-556` (`stationPanel`)
- Modify: `crates/rm-web/assets/js/app.js` (append handlers; wire two call sites)
- Verify: e2e drive (manual checklist below — no JS test infra exists)

**Interfaces:**
- Consumes: `POST /api/edit/station/{svi}`, `POST /api/edit/station/{svi}/move`, dispatched `POST /api/edit/undo/{edit_id}` (station shape `{edit_id, restored_station_version_id}`), payload `d.station.station_version_id` (Task 1), `armPlace(from, onClick)` (app.js:339), `editLogPanel(rows, {onUndo, canUndo})` (panels.js:363), module-global `editable` (set via `setEditable`).
- Produces: `api.renameStation`, `api.moveStation`; `onEditStation(act, d)`; `stationPanel(d, {onPickRoute, onPickStation, onEditStation})`.

- [ ] **Step 1: api.js wrappers**

Append to the edit-mode section of `crates/rm-web/assets/js/api.js`:

```javascript
export const renameStation = (svi, body) => editFetch(`/api/edit/station/${svi}`, body);
export const moveStation = (svi, body) => editFetch(`/api/edit/station/${svi}/move`, body);
```

- [ ] **Step 2: panels.js — changeText + stationPanel buttons**

In `changeText` (panels.js:343), insert BEFORE the `c.stop_name` branch (station rows carry `kind`; the move kind needs no branch — the existing generic `latitude`/`longitude` branch already humanizes it):

```javascript
  if (c.kind === 'station_rename') parts.push(`名 ${c.old_name}→${c.new_name} · ${c.level}`);
```

In `stationPanel` (panels.js:527): change the signature to accept `onEditStation`:

```javascript
export function stationPanel(d, { onPickRoute, onPickStation, onEditStation }) {
```

Prepend an edit row to the `kv([...])` list (first entry, so buttons sit at the top like the stop panel's header row). Full replacement for the `showPane` call:

```javascript
  const pane = showPane('point', `
    ${kv([
    ...(editable && onEditStation ? [['edit',
      '<button class="mini-btn" data-act="rename" title="rename station">✎</button> ' +
      '<button class="mini-btn" data-act="move" title="move station position">移動</button> ' +
      '<button class="mini-btn" data-act="log" title="edit history / undo">↩ 歷史</button>',
    ]] : []),
    ['axis', d.bearing ? `${Math.round(d.bearing.bearing_deg)}°` : '—'],
    ['spread', d.bearing ? `${Math.round(d.bearing.max_spread_deg)}°` : '—'],
    ['members', d.bearing?.member_count ?? stops.length],
    ['group', d.group ? d.group.station_group_name : '—'],
    ...(d.stale?.bearings ? [['stale', '<span class="pp-hot">⚠ bearings stale</span>']] : []),
  ])}
    <h3>其它 Stations (${others.length})</h3>
    <div class="other-station-chips"></div>
    <h3>Stops (${stops.length})</h3>
    ${rows}
    <h3>Routes (${uniqRoutes(d.routes).length})</h3>
    <div class="station-routes"></div>
  `, `${d.station.station_name} #${d.station.station_id}`);
```

After the existing `pane.querySelector('.station-routes')...` line, add:

```javascript
  if (editable && onEditStation) {
    pane.querySelectorAll('.mini-btn').forEach((el) =>
      el.addEventListener('click', () => onEditStation(el.dataset.act, d)));
  }
```

- [ ] **Step 3: app.js — flows + wiring**

Append to `crates/rm-web/assets/js/app.js` (near the stop-side edit handlers):

```javascript
/// station-panel edit actions: rename (prompted name + pole level),
/// move = one-shot place mode, log = station edit history with per-row undo.
function onEditStation(act, d) {
  if (!editMode) return;
  const st = d.station;
  if (act === 'log') {
    openStationEditLog(st);
  } else if (act === 'rename') {
    const name = prompt(`rename station #${st.station_id}:`, st.station_name);
    if (!name || name === st.station_name) return;
    const stops = d.stops ?? [];
    const matches = stops.filter((s) => s.stop_name === st.station_name).length;
    const lv = prompt(
      `level:\n1 station only\n2 matching poles (${matches}/${stops.length} match)\n3 all poles (default)`, '3');
    if (!lv) return;
    const level = { 1: 'station', 2: 'matching', 3: 'all' }[lv.trim()];
    if (!level) return status(`bad level: ${lv}`);
    const reach = level === 'all' ? ` (${stops.length} poles)`
      : level === 'matching' ? ` (${matches} poles)` : '';
    if (!confirm(`${st.station_name} → ${name}\nlevel: ${level}${reach}`)) return status('');
    const note = prompt('note (required):', '') ?? '';
    if (!note) return status('');
    api.renameStation(st.station_version_id, { new_name: name, level, note })
      .then(async () => {
        status('renamed');
        await refreshAfterEdit();
        return selectStation(st.station_id);
      })
      .catch((e) => editFail('rename', e));
  } else if (act === 'move') {
    status('click the new station position');
    armPlace([st.lat, st.lon], (e) => {
      if (!editMode) return;
      const dist = Math.round(L.latLng([st.lat, st.lon]).distanceTo(e.latlng));
      if (!confirm(`move station #${st.station_id} ${dist} m?\nstops stay put\n(${st.lat.toFixed(5)},${st.lon.toFixed(5)}) → (${e.latlng.lat.toFixed(5)},${e.latlng.lng.toFixed(5)})`)) return status('');
      const note = prompt('note (required):', '') ?? '';
      if (!note) return status('');
      api.moveStation(st.station_version_id, { lat: e.latlng.lat, lon: e.latlng.lng, note })
        .then(async () => {
          status('moved');
          await refreshAfterEdit();
          return selectStation(st.station_id);
        })
        .catch((err) => editFail('move', err));
    });
  }
}

async function openStationEditLog(st) {
  busy(true);
  try {
    const v = await api.editLog('station', st.station_id);
    paneOpen('context', `stationeditlog:${st.station_id}`, () => openStationEditLog(st));
    editLogPanel(v.rows ?? [], {
      canUndo: editMode,
      onUndo: (editId) => undoStationEdit(st, editId),
    });
  } catch (e) {
    status(`edit log load failed: ${e.message}`);
  } finally {
    busy(false);
  }
}

async function undoStationEdit(st, editId) {
  if (!editMode) return;
  if (!confirm(`undo edit #${editId} on station #${st.station_id}?`)) return;
  const note = prompt('note (required):', '') ?? '';
  if (!note) return;
  try {
    const r = await api.undoEdit(editId, note);
    status('undone');
    await refreshAfterEdit();
    await selectStation(st.station_id);
    await openStationEditLog(st); // fresh rows — the undo row is now newest
  } catch (e) {
    editFail('undo', e);
  }
}
```

Wire the two existing `stationPanel` call sites — both get `onEditStation` added:

- `openStationPopup` (app.js:234): `stationPanel(d, { onPickRoute, onPickStation: selectStation, onEditStation });`
- `selectStation` (app.js:256): `stationPanel(d, { onPickRoute: (rid, dir) => selectRouteById(rid, dir ?? 0), onPickStation: selectStation, onEditStation });`

- [ ] **Step 4: Build + workspace test**

Run: `cargo build -r -p rm-web && cargo test --workspace`
Expected: build OK; all tests green. (Assets are `include_str!`-baked — this rebuild is what makes the browser serve the new JS.)

- [ ] **Step 5: Commit**

```bash
git add crates/rm-web/assets/js/api.js crates/rm-web/assets/js/panels.js crates/rm-web/assets/js/app.js
git commit -m "rm-web: station panel edit UI — rename, move, edit-log undo

Co-Authored-By: Claude Code <noreply@anthropic.com>"
```

- [ ] **Step 6: e2e drive checklist (manual, report results)**

1. Copy a real DB; `cargo run -r -p rm-web -- <copy.db> --edit herry:<token>` (+ tiles if available).
2. In the browser console BEFORE boot: `sessionStorage.setItem('rm-edit-token', '<token>')`; reload; toggle edit mode AFTER boot.
3. Station panel (via search or map click): ✎/移動/↩歷史 visible in edit mode, absent when edit off.
4. Rename: prompt → level prompt shows `N/M match` → confirm summary → POST → status `renamed` → panel + map re-render new name; try level 2 on a station whose poles differ (expect station-only effect, no error).
5. Move: click 移動 → crosshair + elastic guide → Esc cancels → click → confirm shows distance + "stops stay put" → note → status `moved`, station marker moves, poles unmoved.
6. Log: ↩歷史 → rows newest-first with badges; undo the newest row → status `undone`, panel re-renders restored state, log re-fetches with the undo row on top and no new ↩ on it.
7. Stop-panel regression: stop rename/move/regroup/log + stop undo still work (the dispatched undo route must not have changed the stop shape).
8. Race check (optional): edit the same station from a second tab, then POST from the first → 409 text lands in the status bar, no crash.
