# Refresh Button Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** An authed, single-flight "Apply updates" button in rm-web that runs the three `--stale` refresh steps in-process and reports progress via a status endpoint.

**Architecture:** A `RefreshState` enum in `AppState` guards one-flight; `POST /api/edit/refresh` begins a run and spawns the chain on the blocking pool; `GET /api/edit/refresh` reports state + pending mark count; the chain calls the three crates' existing `run_stale` functions in dependency order (match → segments → registry). No child binaries, no new locks, no CLI flags.

**Tech Stack:** Rust / axum (existing rm-web server), rusqlite, tokio `spawn_blocking`, vanilla JS front end (no test infra).

**Spec:** `docs/refresh-button.md` (read together with `docs/web-edit-mode.md` § Staleness & refresh).

## Global Constraints

- No child processes: the chain calls `rm_osm_match::run_stale`, `rm_segments::run_stale`, `rm_registry::run_stale` in-process (spec Decision 2).
- Single-flight: one refresh at a time per server process; second POST → 409 (spec Decision 4).
- The chain must NOT take `state.write_lock()` — it opens its own `rm_db::open` connection; correctness rests on WAL + `busy_timeout` + `cause_edit_id`-guarded mark clears (spec Decision 6).
- Crash safety: marks survive a failed/aborted run; status resets to `Idle` on restart (spec Decision 5). Never clear marks outside the crates' own guarded clears.
- No new CLI flags (spec: no interval knobs, no `--no-auto-refresh` — that idea was dropped in favor of the explicit button).
- `rm_db::now_rfc3339()` (crates/rm-db/src/lib.rs:38, pub) is the timestamp source everywhere.
- Endpoints live under `/api/edit/*` so the existing token middleware applies (401 no token / 403 bad token).
- Verified entry points (do not re-derive):
  - `rm_osm_match::run_stale(&Connection, batch_id: i64) -> anyhow::Result<Option<StaleOutcome>>`, `StaleOutcome { summary, cleared: usize, kept: usize }`
  - `rm_segments::run_stale(&Connection, batch_id: i64, outlier_m: f64) -> anyhow::Result<Option<StaleOutcome>>` (same outcome shape; `outlier_m = 50.0` matches the CLI default)
  - `rm_registry::run_stale(&Connection, batch_id: i64) -> anyhow::Result<Option<StaleRun>>`
  - `rm_db::latest_batch(&Connection) -> anyhow::Result<Option<i64>>`

## Review Focus

1. **Double-click / racing clicks** — a second `POST /api/edit/refresh` while one runs must return 409 and never start a second chain. Pinned by `second_begin_while_running_conflicts` (Task 1).
2. **Marks kept, not lost** — a mark the chain cannot consume (unpublished route, no staging row) must still be there after a `Done` run, with `pending_marks` reflecting it; the button must re-enable. Pinned by `run_chain_keeps_unconsumable_marks` (Task 2).
3. **DB without an import batch** — refresh must land in `Failed` with a message naming the cause, not panic the server or 500 the POST (the POST already returned 202). Pinned by `run_chain_without_batch_fails_with_message` (Task 2).
4. **Non-edit server** — both refresh routes must be absent (404) without `--edit`, and the button never unhidden. Pinned by `refresh_routes_absent_without_edit` (Task 3) + manual step (Task 4).
5. **Step panic inside the spawned task** — a panicking step must surface as `Failed`, server alive. Handled by the `JoinError` branch in `run_chain` (Task 2); code review item, no test (panicking a real step would need a fault-injection hook — not worth it).

---

### Task 1: RefreshState state machine in AppState

**Files:**
- Modify: `crates/rm-web/src/state.rs`
- Test: `crates/rm-web/tests/refresh.rs` (create)

**Interfaces:**
- Consumes: existing `AppState` (`with_edit`, `with_tiles`), `ApiError::conflict`.
- Produces (Tasks 2–3 rely on exactly these):
  - `pub enum RefreshStep { Match, Segments, Registry }` with `pub fn as_str(&self) -> &'static str` → `"match" | "segments" | "registry"`
  - `pub enum RefreshState { Idle, Running { step: RefreshStep, started_at: String }, Done { finished_at: String }, Failed { error: String, finished_at: String } }` (all `Debug, Clone, PartialEq`)
  - `AppState::try_begin_refresh(&self, started_at: String) -> Result<(), ApiError>` — `Idle`/`Done`/`Failed` → `Running { step: Match, started_at }`, `Ok(())`; already `Running` → `Err(ApiError::conflict("refresh already running"))`
  - `AppState::refresh_state(&self) -> RefreshState` (clone out)
  - `AppState::set_refresh_state(&self, s: RefreshState)`
  - `AppState::set_refresh_step(&self, step: RefreshStep)` — rewrite `Running` preserving `started_at`

- [ ] **Step 1: Write the failing tests**

Create `crates/rm-web/tests/refresh.rs`:

```rust
//! Apply-updates refresh: state machine, chain runner, endpoints.

use axum::http::{Request, StatusCode};
use axum::body::Body;
use rusqlite::Connection;
use tower::ServiceExt; // oneshot

// --- Task 1: state machine ---

#[test]
fn begin_refresh_then_conflict_then_recover() {
    let dir = tempfile::tempdir().unwrap();
    let state = rm_web::state::AppState::with_edit(
        dir.path().join("t.db"),
        None,
        [("herry".to_string(), "sekrit".to_string())].into(),
    );
    assert_eq!(state.refresh_state(), rm_web::state::RefreshState::Idle);

    state.try_begin_refresh("t0".into()).unwrap();
    match state.refresh_state() {
        rm_web::state::RefreshState::Running { step, started_at } => {
            assert_eq!(step, rm_web::state::RefreshStep::Match);
            assert_eq!(started_at, "t0");
        }
        other => panic!("expected Running, got {other:?}"),
    }

    // single-flight: second begin while running is a 409-shaped conflict
    let err = state.try_begin_refresh("t1".into()).unwrap_err();
    assert!(err.to_string().contains("already running"), "{err}");

    // step rewrite keeps started_at
    state.set_refresh_step(rm_web::state::RefreshStep::Segments);
    match state.refresh_state() {
        rm_web::state::RefreshState::Running { step, started_at } => {
            assert_eq!(step, rm_web::state::RefreshStep::Segments);
            assert_eq!(started_at, "t0");
        }
        other => panic!("expected Running, got {other:?}"),
    }

    // terminal states open the gate again
    state.set_refresh_state(rm_web::state::RefreshState::Done { finished_at: "t2".into() });
    state.try_begin_refresh("t3".into()).unwrap();
    state.set_refresh_state(rm_web::state::RefreshState::Failed {
        error: "x".into(),
        finished_at: "t4".into(),
    });
    state.try_begin_refresh("t5".into()).unwrap();
}
```

Note: `[..].into()` builds the `HashMap<String, String>` — check `with_edit`'s signature (state.rs:36, takes `HashMap<String, String>`); `.into()` from an array of tuples works. If the executor prefers, use the explicit `HashMap` insert style from `tests/edit.rs:15-17`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -r -p rm-web --test refresh begin_refresh 2>&1 | tail -5`
Expected: compile FAIL — `RefreshState` / `try_begin_refresh` not found in `rm_web::state`.

- [ ] **Step 3: Implement the state machine**

In `crates/rm-web/src/state.rs`, add below the existing `use` lines:

```rust
use std::sync::Mutex;
```

Add the enums above `pub struct AppState`:

```rust
/// Which `--stale` step the refresh chain is on.
#[derive(Debug, Clone, PartialEq)]
pub enum RefreshStep {
    Match,
    Segments,
    Registry,
}

impl RefreshStep {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Match => "match",
            Self::Segments => "segments",
            Self::Registry => "registry",
        }
    }
}

/// Lifecycle of one Apply-updates run. `Idle` until the first POST; a
/// server restart resets to `Idle` (marks, not status, are the durable
/// state — see docs/refresh-button.md Decision 5).
#[derive(Debug, Clone, PartialEq)]
pub enum RefreshState {
    Idle,
    Running { step: RefreshStep, started_at: String },
    Done { finished_at: String },
    Failed { error: String, finished_at: String },
}
```

Add the field to `AppState` and initialize it in `with_tiles` (the only full constructor; `with_edit`'s `..with_tiles(...)` picks it up):

```rust
pub struct AppState {
    db_path: Arc<PathBuf>,
    tiles: Option<Arc<String>>,
    edit_tokens: Option<Arc<HashMap<String, String>>>,
    write_lock: Arc<AsyncMutex<()>>,
    refresh: Arc<Mutex<RefreshState>>,
}
```

In `with_tiles`, add to the struct literal:

```rust
            refresh: Arc::new(Mutex::new(RefreshState::Idle)),
```

Add methods inside `impl AppState`:

```rust
    /// Begin a refresh run: any non-running state → `Running(Match)`.
    /// Already running → conflict (single-flight, spec Decision 4).
    pub fn try_begin_refresh(&self, started_at: String) -> Result<(), ApiError> {
        let mut g = self.refresh.lock().expect("refresh lock poisoned");
        match *g {
            RefreshState::Running { .. } => {
                Err(ApiError::conflict("refresh already running"))
            }
            _ => {
                *g = RefreshState::Running { step: RefreshStep::Match, started_at };
                Ok(())
            }
        }
    }

    /// Clone of the current refresh lifecycle state.
    pub fn refresh_state(&self) -> RefreshState {
        self.refresh.lock().expect("refresh lock poisoned").clone()
    }

    /// Overwrite the lifecycle state (runner completion / failure).
    pub fn set_refresh_state(&self, s: RefreshState) {
        *self.refresh.lock().expect("refresh lock poisoned") = s;
    }

    /// Advance `Running.step`, preserving `started_at`.
    pub fn set_refresh_step(&self, step: RefreshStep) {
        let mut g = self.refresh.lock().expect("refresh lock poisoned");
        if let RefreshState::Running { started_at, .. } = &*g {
            let started_at = started_at.clone();
            *g = RefreshState::Running { step, started_at };
        }
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -r -p rm-web --test refresh begin_refresh`
Expected: PASS (1 test).

- [ ] **Step 5: Commit**

```bash
git add crates/rm-web/src/state.rs crates/rm-web/tests/refresh.rs
git commit -m "rm-web: RefreshState lifecycle + single-flight begin in AppState"
```

---

### Task 2: The chain runner (`run_chain`)

**Files:**
- Create: `crates/rm-web/src/refresh.rs`
- Modify: `crates/rm-web/src/lib.rs` (module declaration), `crates/rm-web/Cargo.toml` (three new regular deps)
- Test: `crates/rm-web/tests/refresh.rs`

**Interfaces:**
- Consumes: Task 1's `try_begin_refresh` is NOT used here (the handler calls it); this task uses `set_refresh_state` / `set_refresh_step`, `rm_db::open`, `rm_db::latest_batch`, `rm_db::now_rfc3339`, and the three `run_stale` entry points listed under Global Constraints.
- Produces: `pub async fn run_chain(state: AppState)` — runs the whole chain on a fresh mutable connection, advances `Running.step` between steps, and finishes by setting `Done { finished_at }` or `Failed { error, finished_at }`. The handler (Task 3) spawns it after `try_begin_refresh`.

- [ ] **Step 1: Add the three dependencies**

`crates/rm-web/Cargo.toml` — the workspace root does NOT carry workspace entries for these crates (verified: `grep rm-segments Cargo.toml` is empty), so use the explicit path form the `[dev-dependencies]` section already uses:

```toml
[dependencies]
# ... existing lines stay ...
rm-segments = { path = "../rm-segments" }
rm-osm-match = { path = "../rm-osm-match" }
rm-registry = { path = "../rm-registry" }
```

(Leave the `[dev-dependencies]` duplicates as they are — harmless.)

- [ ] **Step 2: Write the failing tests**

Append to `crates/rm-web/tests/refresh.rs`:

```rust
// --- Task 2: chain runner ---

/// Minimal DB for runner tests: schema + one CLOSED batch row. The runner's
/// steps are all no-ops (`None`) when `stale_mark` is empty.
fn runner_db(dir: &tempfile::TempDir) -> rm_web::state::AppState {
    let path = dir.path().join("t.db");
    let conn = rm_db::open(&path).unwrap();
    conn.execute(
        "INSERT INTO import_batch (batch_id, source_dir, file_count, captured_at, valid_from, status, created_at)
         VALUES (1, 'assets/x', 1, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'CLOSED', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    drop(conn);
    rm_web::state::AppState::with_edit(
        path,
        None,
        [("herry".to_string(), "sekrit".to_string())].into(),
    )
}

#[tokio::test]
async fn run_chain_no_marks_ends_done() {
    let dir = tempfile::tempdir().unwrap();
    let state = runner_db(&dir);
    rm_web::refresh::run_chain(state.clone()).await;
    assert!(matches!(
        state.refresh_state(),
        rm_web::state::RefreshState::Done { .. }
    ));
}

#[tokio::test]
async fn run_chain_without_batch_fails_with_message() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    let conn = rm_db::open(&path).unwrap();
    drop(conn); // schema, but NO import_batch row
    let state = rm_web::state::AppState::with_edit(
        path,
        None,
        [("herry".to_string(), "sekrit".to_string())].into(),
    );
    rm_web::refresh::run_chain(state.clone()).await;
    match state.refresh_state() {
        rm_web::state::RefreshState::Failed { error, .. } => {
            assert!(error.contains("no import batch"), "{error}");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[tokio::test]
async fn run_chain_keeps_unconsumable_marks() {
    let dir = tempfile::tempdir().unwrap();
    let state = runner_db(&dir);
    // mark for route 999: unpublished -> the match step warns and KEEPS it
    {
        let conn = rm_db::open(&dir.path().join("t.db")).unwrap();
        conn.execute(
            "INSERT INTO edit_log (ts, editor, entity, entity_id, action, changes)
             VALUES ('2026-01-01T00:00:00Z', 't', 'polyline', 999, 'create', '{}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO stale_mark VALUES ('seg_match', '999:0', 1, '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
    }
    rm_web::refresh::run_chain(state.clone()).await;
    assert!(matches!(
        state.refresh_state(),
        rm_web::state::RefreshState::Done { .. }
    ));
    let conn = rm_db::open_readonly(&dir.path().join("t.db")).unwrap();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1, "unconsumable mark must survive a Done run");
}
```

Note on `finished_at_or`: no such helper exists or is needed — `Done`'s `finished_at` is set by the runner from `rm_db::now_rfc3339()` and the tests only pattern-match it.

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -r -p rm-web --test refresh run_chain 2>&1 | tail -5`
Expected: compile FAIL — no module `refresh` in `rm_web`.

- [ ] **Step 4: Implement the runner**

Declare the module in `crates/rm-web/src/lib.rs` (next to the other `pub mod` lines):

```rust
pub mod refresh;
```

Create `crates/rm-web/src/refresh.rs`:

```rust
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
    let joined = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let conn = rm_db::open(&path)?;
        let batch = rm_db::latest_batch(&conn)?
            .ok_or_else(|| anyhow::anyhow!("no import batch; run rm-ingest first"))?;

        state.set_refresh_step(RefreshStep::Match);
        if let Some(o) = rm_osm_match::run_stale(&conn, batch)? {
            tracing::info!(
                "refresh: match: {} directions, {} cleared, {} kept",
                o.summary.directions, o.cleared, o.kept
            );
        }

        state.set_refresh_step(RefreshStep::Segments);
        if let Some(o) = rm_segments::run_stale(&conn, batch, OUTLIER_M)? {
            tracing::info!(
                "refresh: segments: {} directions, {} cleared, {} kept",
                o.summary.directions, o.cleared, o.kept
            );
        }

        state.set_refresh_step(RefreshStep::Registry);
        if let Some(o) = rm_registry::run_stale(&conn, batch)? {
            tracing::info!("refresh: registry: bearing + split rebuild done");
            let _ = o; // StaleRun fields are per-crate summaries; log stays generic
        }
        Ok(())
    })
    .await;

    let finished_at = rm_db::now_rfc3339();
    match joined {
        Ok(Ok(())) => state.set_refresh_state(RefreshState::Done { finished_at }),
        Ok(Err(e)) => state.set_refresh_state(RefreshState::Failed {
            error: e.to_string(),
            finished_at,
        }),
        Err(join) => state.set_refresh_state(RefreshState::Failed {
            error: format!("refresh task failed: {join}"),
            finished_at,
        }),
    }
}
```

Executor note: `o.summary.directions` on rm-osm-match's `Summary` — verify the field exists (`crates/rm-osm-match/src/lib.rs` `pub struct Summary`, has `directions: usize` per `write_outcomes` at lib.rs:109-112). If the type differs, log only `cleared`/`kept` which are on `StaleOutcome` directly.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -r -p rm-web --test refresh`
Expected: PASS (4 tests: 1 from Task 1 + 3 from Task 2).

- [ ] **Step 6: Commit**

```bash
git add crates/rm-web/src/refresh.rs crates/rm-web/src/lib.rs crates/rm-web/Cargo.toml crates/rm-web/Cargo.lock crates/rm-web/tests/refresh.rs
git commit -m "rm-web: refresh chain runner — three run_stale steps, Failed/Done lifecycle"
```

---

### Task 3: POST + GET `/api/edit/refresh` endpoints

**Files:**
- Create: `crates/rm-web/src/handlers/refresh.rs`
- Modify: `crates/rm-web/src/handlers/mod.rs` (module + no route here — edit routes mount in lib.rs), `crates/rm-web/src/lib.rs` (mount under the `edit_enabled` branch)
- Test: `crates/rm-web/tests/refresh.rs`

**Interfaces:**
- Consumes: Task 1's `try_begin_refresh` / `refresh_state` / `RefreshState` / `RefreshStep::as_str`; Task 2's `run_chain`; existing `Editor` extractor (`crates/rm-web/src/handlers/edit.rs:15`, `pub struct Editor(pub String)`), `run_db` (`state.rs:79`), `ApiError`.
- Produces:
  - `POST /api/edit/refresh` → `202 {}` | `409 {"error":"refresh already running"}` (401/403 via middleware)
  - `GET /api/edit/refresh` → `200` with `{"state": "idle|running|done|failed", "step": "match|segments|registry"?, "started_at"?, "finished_at"?, "error"?, "pending_marks": <int>}`

- [ ] **Step 1: Write the failing tests**

Append to `crates/rm-web/tests/refresh.rs`:

```rust
// --- Task 3: endpoints ---

fn authed_req(method: &str, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", "Bearer sekrit")
        .body(Body::empty())
        .unwrap()
}

/// Authed GET /api/edit/refresh (the endpoint sits behind the token
/// middleware, so the request must carry the header).
async fn status_json(app: &axum::Router) -> serde_json::Value {
    let res = app
        .clone()
        .oneshot(authed_req("GET", "/api/edit/refresh"))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let b = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    serde_json::from_slice(&b).unwrap()
}

/// Poll GET /api/edit/refresh until the state leaves `running` (max ~5 s).
async fn settled(app: axum::Router) -> serde_json::Value {
    for _ in 0..100 {
        let v = status_json(&app).await;
        if v["state"] != "running" {
            return v;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("refresh never settled");
}

#[tokio::test]
async fn refresh_requires_token() {
    let dir = tempfile::tempdir().unwrap();
    let state = runner_db(&dir);
    for (method, auth, want) in [
        ("POST", None::<&str>, StatusCode::UNAUTHORIZED),
        ("POST", Some("Bearer nope"), StatusCode::FORBIDDEN),
        ("GET", None::<&str>, StatusCode::UNAUTHORIZED),
        ("GET", Some("Bearer nope"), StatusCode::FORBIDDEN),
    ] {
        let mut req = Request::builder().method(method).uri("/api/edit/refresh");
        if let Some(a) = auth {
            req = req.header("authorization", a);
        }
        let res = rm_web::router_with(state.clone())
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), want, "{method} {auth:?}");
    }
}

#[tokio::test]
async fn refresh_start_runs_to_done() {
    let dir = tempfile::tempdir().unwrap();
    let state = runner_db(&dir);
    let app = rm_web::router_with(state.clone());

    let res = app
        .clone()
        .oneshot(authed_req("POST", "/api/edit/refresh"))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);

    let v = settled(app).await;
    assert_eq!(v["state"], "done");
    assert_eq!(v["pending_marks"], 0);
    assert!(v["started_at"].is_string());
    assert!(v["finished_at"].is_string());
    assert!(v["error"].is_null());
}

#[tokio::test]
async fn refresh_status_shapes_idle_state() {
    let dir = tempfile::tempdir().unwrap();
    let state = runner_db(&dir);
    let app = rm_web::router_with(state.clone());
    let v = status_json(&app).await; // idle snapshot before any POST
    assert_eq!(v["state"], "idle");
    assert_eq!(v["pending_marks"], 0);
    assert!(v["step"].is_null());
}

#[tokio::test]
async fn refresh_routes_absent_without_edit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    let conn = rm_db::open(&path).unwrap();
    drop(conn);
    let state = rm_web::state::AppState::new(path); // no tokens -> edit routes unmounted
    for method in ["POST", "GET"] {
        let res = rm_web::router_with(state.clone())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri("/api/edit/refresh")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{method}");
    }
}
```

Note on `refresh_status_shapes_all_states`: the `serde_json::from_value(...).unwrap_or(...)` wrapper is pointless — write it as a plain `let v = get_json(&rm_web::router_with(state.clone()), "/api/edit/refresh").await;` and assert on `v` directly.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -r -p rm-web --test refresh refresh_ 2>&1 | tail -5`
Expected: compile FAIL — `handlers::refresh` does not exist / routes 404.

- [ ] **Step 3: Implement the handlers**

Create `crates/rm-web/src/handlers/refresh.rs`:

```rust
//! POST /api/edit/refresh — begin the apply-updates chain (single-flight).
//! GET  /api/edit/refresh — lifecycle state + pending mark count.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use crate::handlers::edit::Editor;
use crate::state::{run_db, AppState, RefreshState};

/// Begin one refresh run. 202 immediately; the chain runs on the blocking
/// pool and updates AppState. Second POST while running: 409.
pub async fn start(
    State(state): State<AppState>,
    Editor(_editor): Editor,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    state.try_begin_refresh(rm_db::now_rfc3339())?;
    tokio::spawn(crate::refresh::run_chain(state.clone()));
    Ok((StatusCode::ACCEPTED, Json(json!({}))))
}

/// Status snapshot for the UI button (polled every ~2 s while running).
pub async fn status(State(state): State<AppState>, Editor(_editor): Editor) -> Result<Json<Value>, ApiError> {
    let pending: i64 = run_db(state.clone(), |conn| {
        conn.query_row("SELECT COUNT(*) FROM stale_mark", [], |r| r.get(0))
            .map_err(Into::into)
    })
    .await?;
    let mut v = match state.refresh_state() {
        RefreshState::Idle => json!({ "state": "idle" }),
        RefreshState::Running { step, started_at } => json!({
            "state": "running", "step": step.as_str(), "started_at": started_at,
        }),
        RefreshState::Done { finished_at } => json!({
            "state": "done", "finished_at": finished_at,
        }),
        RefreshState::Failed { error, finished_at } => json!({
            "state": "failed", "error": error, "finished_at": finished_at,
        }),
    };
    v["pending_marks"] = json!(pending);
    Ok(Json(v))
}

use crate::error::ApiError;
```

Executor notes: (a) move the `use crate::error::ApiError;` to the top use-block with the others; (b) `Editor` implements `FromRequestParts` over `AppState` (edit.rs:15-17) — if its extraction needs the `State` extractor to come first, keep the parameter order `State` then `Editor` as shown; (c) `run_db`'s closure error type must be `anyhow::Result` — `.map_err(Into::into)` on the `rusqlite::Result` handles it.

Declare the module in `crates/rm-web/src/handlers/mod.rs` (next to `pub mod edit;`):

```rust
pub mod refresh;
```

Mount the routes in `crates/rm-web/src/lib.rs`, inside the `if state.edit_enabled()` branch of `router_with`, next to the polyline route:

```rust
        .route(
            "/api/edit/refresh",
            axum::routing::post(handlers::refresh::start).get(handlers::refresh::status),
        )
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -r -p rm-web --test refresh`
Expected: PASS (8 tests: 1 + 3 + 4).

Run the whole crate's suite for collateral damage:
`cargo test -r -p rm-web`
Expected: all PASS (edit.rs tests untouched).

- [ ] **Step 5: Commit**

```bash
git add crates/rm-web/src/handlers/refresh.rs crates/rm-web/src/handlers/mod.rs crates/rm-web/src/lib.rs crates/rm-web/tests/refresh.rs
git commit -m "rm-web: POST/GET /api/edit/refresh — single-flight start + status snapshot"
```

---

### Task 4: UI — button, api helper, wiring

**Files:**
- Modify: `crates/rm-web/assets/index.html` (topbar), `crates/rm-web/assets/js/api.js` (two helpers), `crates/rm-web/assets/js/app.js` (show/hide + click + poll)

**Interfaces:**
- Consumes: Task 3's endpoints; existing `editFetch` (api.js:35), `setChip` (app.js:33), `status` (app.js:23), `editMode` (app.js:46), `enableEditMode`/`disableEditMode` (app.js:340/352), the `if (s.edit_enabled)` unhide block in `boot()` (app.js:788-790).
- Produces: button `#refresh-btn` (behavior only; no JS module interface).

No JS test infra exists (spec: manual verification) — each step below includes its manual check. Start the server once for all checks:

```sh
cp data/taichung.db /tmp/refresh-ui-test.db
cargo run -r -p rm-web -- --db /tmp/refresh-ui-test.db --edit --edit-tokens herry:test --addr 127.0.0.1:8799
# open http://127.0.0.1:8799/ , click ✎ edit, token: test
```

- [ ] **Step 1: Button markup**

In `crates/rm-web/assets/index.html`, right after the `#edit-toggle` button (line 25), add:

```html
    <button id="refresh-btn" class="chip chip-click" hidden
      title="Re-run match / projections / bearings for stale edits">⟳ apply updates <b>—</b></button>
```

(`chip chip-click` reuses the topbar chip styling; `setChip` turns the count red when > 0 — desired: pending work is visible at a glance.)

- [ ] **Step 2: api.js helpers**

In `crates/rm-web/assets/js/api.js`, append to the edit-mode section (after `editPolylineLine`, line 55-56):

```javascript
export const refreshStatus = () => fetchJson('/api/edit/refresh');
export const refreshApply = () => editFetch('/api/edit/refresh', {});
```

- [ ] **Step 3: app.js wiring**

In `crates/rm-web/assets/js/app.js`:

(a) Next to `let editMode = false;` (line 46), add:

```javascript
let refreshTimer = null;
```

(b) In `enableEditMode()` (after `status('edit mode on');`):

```javascript
  document.getElementById('refresh-btn').hidden = false;
```

In `disableEditMode()` (after `status('');`):

```javascript
  document.getElementById('refresh-btn').hidden = true;
  clearInterval(refreshTimer);
  refreshTimer = null;
```

(c) In `boot()`, inside `if (s.edit_enabled) { ... }` (right after the `edit-toggle` unhide at ~line 789), add the click handler:

```javascript
      const rbtn = document.getElementById('refresh-btn');
      rbtn.hidden = false;
      // Seed the pending-count badge. The button stays ENABLED at 0
      // pending (a no-op run is harmless) — edits made later in the
      // session have no push channel, so a disabled-at-0 rule would
      // brick the button after new edits (spec deviation, flagged).
      api.refreshStatus().then((v) => {
        setChip('refresh-btn', v.pending_marks > 0 ? v.pending_marks : '—');
      }).catch(() => {});
      rbtn.addEventListener('click', async () => {
        if (refreshTimer) return; // a run is already in flight
        if (!confirm('Apply updates? Re-runs match, projections and bearings for all stale edits.')) return;
        try {
          await api.refreshApply();
        } catch (e) {
          status(`apply failed: ${e.message}`);
          return;
        }
        rbtn.disabled = true;
        refreshTimer = setInterval(async () => {
          try {
            const v = await api.refreshStatus();
            if (v.state === 'running') {
              rbtn.querySelector('b').textContent = v.step;
              return;
            }
            clearInterval(refreshTimer);
            refreshTimer = null;
            rbtn.disabled = false;
            setChip('refresh-btn', v.pending_marks > 0 ? v.pending_marks : '—');
            if (v.state === 'failed') status(`apply failed: ${v.error}`);
            else status(v.pending_marks > 0 ? `updates applied, ${v.pending_marks} mark(s) kept` : 'updates applied');
            if (typeof syncLineBtn === 'function') syncLineBtn(); // redraw raw layer if open
          } catch (e) {
            clearInterval(refreshTimer);
            refreshTimer = null;
            rbtn.disabled = false;
            status(`apply status failed: ${e.message}`);
          }
        }, 2000);
      });
```

Executor notes: (a) if `syncLineBtn` is not a global function in app.js, drop that line — panel data refreshes on next open; do not invent a helper. (b) `setChip('refresh-btn', '—')` resets the count badge; `setChip` reads `document.getElementById(id)` so the button id works.

- [ ] **Step 4: Manual verification**

With the server from above running:

1. Button hidden before edit mode; visible after ✎ edit. Check.
2. Button shows `⟳ apply updates —` with count `—`; click → confirm dialog. Cancel → nothing. Check.
3. Confirm → count badge shows `match…`/`segments…`/`registry…`, button disabled. On an empty-mark DB this flashes by in <1 s and ends `updates applied`. Check.
4. Plant a mark to see the full lifecycle:
   ```sh
   sqlite3 /tmp/refresh-ui-test.db "INSERT INTO edit_log (ts, editor, entity, entity_id, action, changes) VALUES (datetime('now'),'herry','polyline',999,'create','{}'); INSERT INTO stale_mark VALUES ('seg_match','999:0',(SELECT MAX(edit_id) FROM edit_log),datetime('now'));"
   ```
   Reload the page (badge count appears after next status read — refresh the page once), click apply → status line ends `updates applied, 1 mark(s) kept`, badge shows `1` in red. Check.
5. Second-click guard: during a real (multi-second) run on taipei.db, a second click does nothing (`refreshTimer` guard) and curling POST again returns 409. Check.
6. `Esc`/navigation during run: polling continues; run completes server-side regardless. Check.

- [ ] **Step 5: Commit**

```bash
git add crates/rm-web/assets/index.html crates/rm-web/assets/js/api.js crates/rm-web/assets/js/app.js
git commit -m "rm-web ui: apply-updates button — confirm, step spinner, kept-mark count"
```

---

### Task 5: Docs + full-suite gate

**Files:**
- Modify: `docs/web-edit-mode.md` (5 sections), `README.md` (one line), `docs/refresh-button.md` (status flip)

**Interfaces:** none (docs only). Gate: whole workspace green.

- [ ] **Step 1: web-edit-mode.md — § Staleness & refresh**

After the three-command list and the order paragraph, insert a lead-in paragraph BEFORE the manual chain (so the button reads as primary, CLI as fallback). Adjust the section to open with:

```markdown
Refresh runs from the web UI: the **Apply updates** button (edit mode)
starts one in-server pass over all pending marks — see
`docs/refresh-button.md`. The CLI chain below is the ops fallback and
the reference for what each step consumes.
```

Then follow the existing § *UI* bullet list (add before the stale-badge bullet):

```markdown
- **Apply updates**: topbar button (edit mode only) — one confirm, then
  the three `--stale` steps run in-server; the button shows the current
  step and the kept-mark count after.
```

In § *Server write path & auth* endpoint list, add:

```markdown
  - `POST /api/edit/refresh` — begin the apply-updates chain
    (single-flight; `409` while one runs).
  - `GET  /api/edit/refresh` — lifecycle state + pending `stale_mark`
    count.
```

In § *Non-goals*, DELETE the line `- UI buttons that trigger refresh/pipeline runs.` and replace with:

```markdown
- Automatic (timer-driven) refresh — refresh is explicit: the
  Apply-updates button or the CLI (`docs/refresh-button.md`).
```

In § *Phasing*, append:

```markdown
5. **Apply-updates button** (shipped): single-flight in-server refresh
   chain + status endpoint — see `docs/refresh-button.md`.
```

- [ ] **Step 2: README.md**

In the Run section, directly under the three `--stale` lines (after line 61), add:

```markdown
# In the web UI (edit mode) the same chain is one button: ⟳ apply updates
```

- [ ] **Step 3: spec status flip**

In `docs/refresh-button.md`, change the status line to:

```markdown
Status: shipped
```

- [ ] **Step 4: Whole-workspace gate**

Run: `cargo test --workspace 2>&1 | tail -15`
Expected: all crates PASS, 0 failed.

- [ ] **Step 5: Commit**

```bash
git add docs/web-edit-mode.md README.md docs/refresh-button.md
git commit -m "docs: apply-updates button is the primary refresh path; CLI stays as fallback"
```
