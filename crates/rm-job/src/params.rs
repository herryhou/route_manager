//! One serde + schemars struct per command: the `--json` payload shapes the
//! `schema` command publishes and the CLI accepts. Flag defaults that differ
//! from these (`db`, `editor`) are the --json defaults; flags keep their own.

use schemars::JsonSchema;
use serde::Deserialize;

fn default_editor() -> String {
    "agent".to_string()
}

fn default_meters() -> f64 {
    10.0
}

fn default_end() -> String {
    "both".to_string()
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

/// `rm-job remove-stop --json '...'`
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RemoveStopParams {
    /// SQLite DB path
    pub db: String,
    /// Route XNO to detach from
    pub xno: String,
    /// Direction (0 = outbound, 1 = inbound)
    pub direction: i64,
    /// Exact stop code to detach
    pub code: String,
    /// When the code occurs more than once in the direction: the stop_sequence to take
    #[serde(default)]
    pub sequence: Option<i64>,
    /// When the code occurs more than once in the direction: take every occurrence
    #[serde(default)]
    pub all: bool,
    /// Delete the pole itself when the detach orphans it (one-way, not undoable)
    #[serde(default)]
    pub delete_pole: bool,
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

/// `rm-job extend-path --json '...'`
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExtendPathParams {
    /// SQLite DB path
    pub db: String,
    /// Route XNO
    pub xno: String,
    /// Direction (0 = outbound, 1 = inbound)
    pub direction: i64,
    /// Extension length per end (m)
    #[serde(default = "default_meters")]
    pub meters: f64,
    /// Which end(s) to extend: both, first or last
    #[serde(default = "default_end")]
    pub end: String,
    /// Editor name recorded on the audit row
    #[serde(default = "default_editor")]
    pub editor: String,
    /// Note recorded on the audit row
    #[serde(default)]
    pub note: Option<String>,
    /// Write the extension as a polyline override (default: dry run)
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
