//! One serde + schemars struct per command: the `--json` payload shapes the
//! `schema` command publishes and the CLI accepts. Flag defaults that differ
//! from these (`db`, `editor`) are the --json defaults; flags keep their own.

use schemars::JsonSchema;
use serde::Deserialize;

fn default_editor() -> String {
    "agent".to_string()
}

/// `rm-job add-stop --json '...'`
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddStopParams {
    /// New pole's name
    pub name: String,
    /// Latitude
    pub lat: f64,
    /// Longitude
    pub lon: f64,
    /// Attachment as XNO:DIR[@CODE] — @CODE inserts after that stop, absent appends.
    #[serde(default)]
    pub attach: Vec<String>,
    /// Join this existing station instead of qualify-or-create
    #[serde(default)]
    pub station: Option<i64>,
    /// SQLite DB path
    pub db: String,
    /// Editor name recorded on the audit row
    #[serde(default = "default_editor")]
    pub editor: String,
    /// Note recorded on the audit row
    #[serde(default)]
    pub note: Option<String>,
    /// Write the shown plan (default: dry run)
    #[serde(default)]
    pub apply: bool,
}

/// `rm-job find-stop --json '...'`
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindStopParams {
    /// SQLite DB path
    pub db: String,
    /// Route XNO to search within
    pub xno: String,
    /// Stop-name substring (matched with SQL LIKE %q%)
    #[serde(default)]
    pub name: Option<String>,
    /// Exact stop code
    #[serde(default)]
    pub code: Option<String>,
}

/// `rm-job show-route --json '...'`
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ShowRouteParams {
    /// SQLite DB path
    pub db: String,
    /// Route XNO
    pub xno: String,
}

/// `rm-job show-path --json '...'`
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ShowPathParams {
    /// SQLite DB path
    pub db: String,
    /// Route XNO
    pub xno: String,
    /// Direction (0 = outbound, 1 = inbound)
    pub direction: i64,
}

/// `rm-job move-stop --json '...'`
#[derive(Debug, Deserialize, JsonSchema)]
pub struct MoveStopParams {
    /// SQLite DB path
    pub db: String,
    /// Target pole's stop_version_id (else resolve xno + code)
    #[serde(default)]
    pub stop_version_id: Option<i64>,
    /// Route XNO to resolve the pole on (with code, when stop_version_id is absent)
    #[serde(default)]
    pub xno: Option<String>,
    /// Exact stop code to resolve the pole (with xno, when stop_version_id is absent)
    #[serde(default)]
    pub code: Option<String>,
    /// New latitude
    #[serde(default)]
    pub lat: Option<f64>,
    /// New longitude
    #[serde(default)]
    pub lon: Option<f64>,
    /// New stop name
    #[serde(default)]
    pub stop_name: Option<String>,
    /// Editor name recorded on the audit row
    #[serde(default = "default_editor")]
    pub editor: String,
    /// Note recorded on the audit row
    #[serde(default)]
    pub note: Option<String>,
    /// Write the edit (default: dry run)
    #[serde(default)]
    pub apply: bool,
}

/// `rm-job undo --json '...'`
#[derive(Debug, Deserialize, JsonSchema)]
pub struct UndoParams {
    /// The edit_log id to undo (printed by --apply)
    pub edit_id: i64,
    /// SQLite DB path
    pub db: String,
    /// Editor name recorded on the undo's audit row
    #[serde(default = "default_editor")]
    pub editor: String,
}
