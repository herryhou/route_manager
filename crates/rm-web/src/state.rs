//! Shared server state: DB path + spawn_blocking query helper.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;

use anyhow::Result;
use rusqlite::Connection;
use tokio::sync::Mutex as AsyncMutex;

use crate::error::ApiError;

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
    Running {
        step: RefreshStep,
        started_at: String,
    },
    Done {
        started_at: String,
        finished_at: String,
    },
    Failed {
        error: String,
        started_at: String,
        finished_at: String,
    },
}

#[derive(Clone)]
pub struct AppState {
    db_path: Arc<PathBuf>,
    tiles: Option<Arc<String>>,
    /// Current-stop bbox [w, s, e, n], computed once at boot — seeds the
    /// client's initial map view (`None` for tests / empty dbs).
    bounds: Option<Arc<[f64; 4]>>,
    edit_tokens: Option<Arc<HashMap<String, String>>>,
    write_lock: Arc<AsyncMutex<()>>,
    refresh: Arc<Mutex<RefreshState>>,
}

impl AppState {
    pub fn new(db_path: PathBuf) -> Self {
        Self::with_tiles(db_path, None)
    }

    pub fn with_tiles(db_path: PathBuf, tiles: Option<String>) -> Self {
        Self {
            db_path: Arc::new(db_path),
            tiles: tiles.map(Arc::new),
            bounds: None,
            edit_tokens: None,
            write_lock: Arc::new(AsyncMutex::new(())),
            refresh: Arc::new(Mutex::new(RefreshState::Idle)),
        }
    }

    /// Edit mode: POST routes mounted, writes authenticated by token → editor.
    pub fn with_edit(
        db_path: PathBuf,
        tiles: Option<String>,
        tokens: HashMap<String, String>,
    ) -> Self {
        Self {
            edit_tokens: Some(Arc::new(tokens)),
            ..Self::with_tiles(db_path, tiles)
        }
    }

    pub fn tiles(&self) -> Option<&str> {
        self.tiles.as_ref().map(|s| s.as_str())
    }

    /// Boot-seeded dataset bbox ([w, s, e, n]); set once by `serve`.
    pub fn bounds(&self) -> Option<[f64; 4]> {
        self.bounds.as_ref().map(|b| **b)
    }

    pub fn set_bounds(&mut self, b: [f64; 4]) {
        self.bounds = Some(Arc::new(b));
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Edit routes are mounted only when tokens were configured.
    pub fn edit_enabled(&self) -> bool {
        self.edit_tokens.is_some()
    }

    /// Editor name → token map (edit mode only).
    pub fn write_tokens(&self) -> Option<&HashMap<String, String>> {
        self.edit_tokens.as_deref()
    }

    /// Serializes all write transactions app-wide.
    pub fn write_lock(&self) -> &AsyncMutex<()> {
        &self.write_lock
    }

    /// Open a fresh read-only connection (schema-checked).
    pub fn conn(&self) -> Result<Connection> {
        let conn = rm_db::open_readonly(&self.db_path)?;
        crate::db::ensure_schema(&conn)?;
        Ok(conn)
    }

    /// Begin a refresh run: any non-running state → `Running(Match)`.
    /// Already running → conflict (single-flight, spec Decision 4).
    pub fn try_begin_refresh(&self, started_at: String) -> Result<(), ApiError> {
        let mut g = self.refresh.lock().expect("refresh lock poisoned");
        match *g {
            RefreshState::Running { .. } => Err(ApiError::conflict("refresh already running")),
            _ => {
                *g = RefreshState::Running {
                    step: RefreshStep::Match,
                    started_at,
                };
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
}

/// Run one sync query closure on the blocking pool with a fresh connection.
pub async fn run_db<T, F>(state: AppState, f: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce(&Connection) -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let conn = state.conn()?;
        f(&conn)
    })
    .await
    .expect("spawn_blocking join")
    .map_err(ApiError::from)
}

/// Run one sync WRITE closure, serialized app-wide on a mutable connection.
pub async fn run_db_write<T, F>(state: AppState, f: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce(&Connection) -> Result<T> + Send + 'static,
{
    let _guard = state.write_lock().lock().await;
    let path = state.db_path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        let conn = rm_db::open(&path)?;
        f(&conn)
    })
    .await
    .expect("spawn_blocking join")
    .map_err(ApiError::from)
}
