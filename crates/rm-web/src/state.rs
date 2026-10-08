//! Shared server state: DB path + spawn_blocking query helper.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use rusqlite::Connection;
use tokio::sync::Mutex as AsyncMutex;

use crate::error::ApiError;

#[derive(Clone)]
pub struct AppState {
    db_path: Arc<PathBuf>,
    tiles: Option<Arc<String>>,
    edit_tokens: Option<Arc<HashMap<String, String>>>,
    write_lock: Arc<AsyncMutex<()>>,
}

impl AppState {
    pub fn new(db_path: PathBuf) -> Self {
        Self::with_tiles(db_path, None)
    }

    pub fn with_tiles(db_path: PathBuf, tiles: Option<String>) -> Self {
        Self {
            db_path: Arc::new(db_path),
            tiles: tiles.map(Arc::new),
            edit_tokens: None,
            write_lock: Arc::new(AsyncMutex::new(())),
        }
    }

    /// Edit mode: POST routes mounted, writes authenticated by token → editor.
    pub fn with_edit(db_path: PathBuf, tokens: HashMap<String, String>) -> Self {
        Self {
            edit_tokens: Some(Arc::new(tokens)),
            ..Self::with_tiles(db_path, None)
        }
    }

    pub fn tiles(&self) -> Option<&str> {
        self.tiles.as_ref().map(|s| s.as_str())
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Edit routes are mounted only when tokens were configured.
    pub fn edit_enabled(&self) -> bool {
        self.edit_tokens.is_some()
    }

    /// Token → editor map (edit mode only).
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
