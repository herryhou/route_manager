//! Shared server state: DB path + spawn_blocking query helper.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use rusqlite::Connection;

use crate::error::ApiError;

#[derive(Clone)]
pub struct AppState {
    db_path: Arc<PathBuf>,
    tiles: Option<Arc<String>>,
}

impl AppState {
    pub fn new(db_path: PathBuf) -> Self {
        Self {
            db_path: Arc::new(db_path),
            tiles: None,
        }
    }

    pub fn with_tiles(db_path: PathBuf, tiles: Option<String>) -> Self {
        Self {
            db_path: Arc::new(db_path),
            tiles: tiles.map(Arc::new),
        }
    }

    pub fn tiles(&self) -> Option<&str> {
        self.tiles.as_ref().map(|s| s.as_str())
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
