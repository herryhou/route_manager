//! Stats + outlier queries.

use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Value};

/// Overview: counts, match summary, outlier count, latest batch.
/// Stub until step 4 fills in the real queries.
pub fn stats(_conn: &Connection) -> Result<Value> {
    Ok(json!({}))
}
