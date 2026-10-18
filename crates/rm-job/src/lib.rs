//! rm-job: dry-run-first job CLI over `rm_db`'s manual-edit primitives.
//!
//! The binary is a thin wrapper (parse → run → print → exit); everything
//! testable lives here. SQLite is touched only through `rm_db`. Every run
//! prints exactly one envelope JSON object to stdout (`envelope`); human
//! text goes to stderr.

pub mod envelope;
pub mod extend;
pub mod params;
pub mod queries;

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Instant;

use clap::{Parser, Subcommand};
use serde::Serialize;

use rm_db::{
    add_stop, detach_stop, edit_polyline_line, edit_stop, entity_history, move_station,
    plan_add_stop, plan_detach_stop, plan_move_station, plan_polyline_line, plan_regroup_stop,
    plan_rename_station, preview_stop_edit, regroup_stop, rename_station, undo_edit, AddStopCmd,
    AddStopOutcome, AddStopPlan, AttachCmd, DetachCmd, DetachPlan, LineEditCmd, RegroupStop,
    RenameLevel, StationMove, StationRename, StationVerdict, StopEdit, UndoOutcome,
};

/// Everything one run wrote: `out` is the envelope JSON (the only stdout
/// content), `err` the human text — rejection/no-op reasons, usage lines
/// (stderr). Both buffered so tests can assert content without process
/// capture.
#[derive(Debug, Default)]
pub struct Report {
    pub out: String,
    pub err: String,
}

/// The agent-facing help tail: discover the schema, call with one JSON
/// payload, read the exit code (agent-friendly-cli skill).
const AGENT_USAGE: &str = "\
AGENT USAGE:
  # 1. Discover parameter shapes
  rm-job schema
  rm-job schema add-stop

  # 2. Execute with the discovered schema (payload-only; the legacy flags
  #    remain and are ignored when --json is given)
  rm-job add-stop --json '{\"name\":\"X\",\"lat\":24.14,\"lon\":120.69,\"attach\":[\"1:0\"],\"db\":\"data/taichung.db\"}'

  # 3. Check exit code, parse stdout
  # exit 0 ok, 1 rejected, 2 no-op; stdout is exactly one envelope JSON:
  # {\"status\":\"ok|rejected|noop\",\"command\":\"add-stop\",\"data\":{...},\"logs\":[...],\"duration_ms\":N}
";

#[derive(Parser)]
#[command(
    name = "rm-job",
    about = "Dry-run-first job CLI over rm_db edits",
    after_help = AGENT_USAGE
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Add a stop and attach it to route directions (dry run unless --apply).
    AddStop {
        /// New pole's name
        #[arg(required_unless_present = "json")]
        name: Option<String>,
        /// Latitude
        #[arg(long, required_unless_present = "json")]
        lat: Option<f64>,
        /// Longitude
        #[arg(long, required_unless_present = "json")]
        lon: Option<f64>,
        /// Attachment as XNO:DIR[@CODE] — @CODE inserts after that stop,
        /// absent appends. Repeatable.
        #[arg(long)]
        attach: Vec<String>,
        /// Join this existing station instead of qualify-or-create
        #[arg(long)]
        station: Option<i64>,
        /// SQLite DB path
        #[arg(long, default_value = "data/taichung.db")]
        db: PathBuf,
        /// Editor name recorded on the audit row
        #[arg(long, default_value = "rm-job")]
        editor: String,
        /// Note recorded on the audit row
        #[arg(long)]
        note: Option<String>,
        /// Write the shown plan (default: dry run)
        #[arg(long)]
        apply: bool,
        /// Full invocation as one JSON object (schema: `rm-job schema
        /// add-stop`); flags are ignored when given
        #[arg(long)]
        json: Option<String>,
    },
    /// Move/rename one stop pole (dry run unless --apply).
    MoveStop {
        /// Target pole's stop_version_id (else resolve --xno + --code)
        #[arg(long)]
        stop_version_id: Option<i64>,
        /// Route XNO to resolve the pole on (with --code)
        #[arg(long)]
        xno: Option<String>,
        /// Exact stop code to resolve the pole (with --xno)
        #[arg(long)]
        code: Option<String>,
        /// New latitude
        #[arg(long)]
        lat: Option<f64>,
        /// New longitude
        #[arg(long)]
        lon: Option<f64>,
        /// New stop name
        #[arg(long)]
        stop_name: Option<String>,
        /// SQLite DB path
        #[arg(long, default_value = "data/taichung.db")]
        db: PathBuf,
        /// Editor name recorded on the audit row
        #[arg(long, default_value = "rm-job")]
        editor: String,
        /// Note recorded on the audit row
        #[arg(long)]
        note: Option<String>,
        /// Write the edit (default: dry run)
        #[arg(long)]
        apply: bool,
        /// Full invocation as one JSON object (schema: `rm-job schema
        /// move-stop`); flags are ignored when given
        #[arg(long)]
        json: Option<String>,
    },
    /// Detach a stop occurrence from one route direction (dry run unless
    /// --apply).
    RemoveStop {
        /// Route XNO to detach from
        #[arg(required_unless_present = "json")]
        xno: Option<String>,
        /// Direction (0 = outbound, 1 = inbound)
        #[arg(required_unless_present = "json")]
        direction: Option<i64>,
        /// Exact stop code to detach
        #[arg(required_unless_present = "json")]
        code: Option<String>,
        /// When the code occurs more than once in the direction: the
        /// stop_sequence to take
        #[arg(long)]
        sequence: Option<i64>,
        /// When the code occurs more than once in the direction: take every
        /// occurrence
        #[arg(long)]
        all: bool,
        /// Delete the pole itself when the detach orphans it (one-way, not
        /// undoable)
        #[arg(long)]
        delete_pole: bool,
        /// SQLite DB path
        #[arg(long, default_value = "data/taichung.db")]
        db: PathBuf,
        /// Editor name recorded on the audit row
        #[arg(long, default_value = "rm-job")]
        editor: String,
        /// Note recorded on the audit row
        #[arg(long)]
        note: Option<String>,
        /// Write the edit (default: dry run)
        #[arg(long)]
        apply: bool,
        /// Full invocation as one JSON object (schema: `rm-job schema
        /// remove-stop`); flags are ignored when given
        #[arg(long)]
        json: Option<String>,
    },
    /// Extend a direction's polyline outward along the OSM road graph,
    /// written as a polyline override (dry run unless --apply).
    ExtendPath {
        /// Route XNO
        #[arg(required_unless_present = "json")]
        xno: Option<String>,
        /// Direction (0 = outbound, 1 = inbound)
        #[arg(required_unless_present = "json")]
        direction: Option<i64>,
        /// Extension length per end (m)
        #[arg(long, default_value_t = 10.0)]
        meters: f64,
        /// Which end(s) to extend: both, first or last
        #[arg(long, default_value = "both")]
        end: String,
        /// SQLite DB path
        #[arg(long, default_value = "data/taichung.db")]
        db: PathBuf,
        /// Editor name recorded on the audit row
        #[arg(long, default_value = "rm-job")]
        editor: String,
        /// Note recorded on the audit row
        #[arg(long)]
        note: Option<String>,
        /// Write the extension as a polyline override (default: dry run)
        #[arg(long)]
        apply: bool,
        /// Full invocation as one JSON object (schema: `rm-job schema
        /// extend-path`); flags are ignored when given
        #[arg(long)]
        json: Option<String>,
    },
    /// Move one stop pole to another station (dry run unless --apply).
    RegroupStop {
        /// Target pole's stop_version_id (else resolve --xno + --code)
        #[arg(long)]
        stop_version_id: Option<i64>,
        /// Route XNO to resolve the pole on (with --code)
        #[arg(long)]
        xno: Option<String>,
        /// Exact stop code to resolve the pole (with --xno)
        #[arg(long)]
        code: Option<String>,
        /// Destination station_id
        #[arg(long, required_unless_present = "json")]
        station: Option<i64>,
        /// SQLite DB path
        #[arg(long, default_value = "data/taichung.db")]
        db: PathBuf,
        /// Editor name recorded on the audit row
        #[arg(long, default_value = "rm-job")]
        editor: String,
        /// Note recorded on the audit row
        #[arg(long)]
        note: Option<String>,
        /// Write the edit (default: dry run)
        #[arg(long)]
        apply: bool,
        /// Full invocation as one JSON object (schema: `rm-job schema
        /// regroup-stop`); flags are ignored when given
        #[arg(long)]
        json: Option<String>,
    },
    /// Rename a station, optionally spreading to its poles (dry run unless
    /// --apply).
    RenameStation {
        /// The station's station_id (rm-job read commands print it)
        #[arg(long, required_unless_present = "json")]
        station: Option<i64>,
        /// New station name
        #[arg(long, required_unless_present = "json")]
        new_name: Option<String>,
        /// How far the rename spreads: station, matching or all
        #[arg(long, default_value = "matching")]
        level: String,
        /// SQLite DB path
        #[arg(long, default_value = "data/taichung.db")]
        db: PathBuf,
        /// Editor name recorded on the audit row
        #[arg(long, default_value = "rm-job")]
        editor: String,
        /// Note recorded on the audit row
        #[arg(long)]
        note: Option<String>,
        /// Write the edit (default: dry run)
        #[arg(long)]
        apply: bool,
        /// Full invocation as one JSON object (schema: `rm-job schema
        /// rename-station`); flags are ignored when given
        #[arg(long)]
        json: Option<String>,
    },
    /// Move a station's reference point; poles follow their own coords
    /// (dry run unless --apply).
    MoveStation {
        /// The station's station_id (rm-job read commands print it)
        #[arg(long, required_unless_present = "json")]
        station: Option<i64>,
        /// New latitude
        #[arg(long, required_unless_present = "json")]
        lat: Option<f64>,
        /// New longitude
        #[arg(long, required_unless_present = "json")]
        lon: Option<f64>,
        /// SQLite DB path
        #[arg(long, default_value = "data/taichung.db")]
        db: PathBuf,
        /// Editor name recorded on the audit row
        #[arg(long, default_value = "rm-job")]
        editor: String,
        /// Note recorded on the audit row
        #[arg(long)]
        note: Option<String>,
        /// Write the edit (default: dry run)
        #[arg(long)]
        apply: bool,
        /// Full invocation as one JSON object (schema: `rm-job schema
        /// move-station`); flags are ignored when given
        #[arg(long)]
        json: Option<String>,
    },
    /// Replace a direction's polyline with a whole-line override (dry run
    /// unless --apply).
    SetLine {
        /// Route XNO
        #[arg(required_unless_present = "json")]
        xno: Option<String>,
        /// Direction (0 = outbound, 1 = inbound)
        #[arg(required_unless_present = "json")]
        direction: Option<i64>,
        /// Path point as LAT,LON — repeatable, in path order
        #[arg(long = "point")]
        point: Vec<String>,
        /// Concurrency token: the current override's updated_at (show-path
        /// prints it); required once an override exists
        #[arg(long)]
        expected_updated_at: Option<String>,
        /// SQLite DB path
        #[arg(long, default_value = "data/taichung.db")]
        db: PathBuf,
        /// Editor name recorded on the audit row
        #[arg(long, default_value = "rm-job")]
        editor: String,
        /// Note recorded on the audit row
        #[arg(long)]
        note: Option<String>,
        /// Write the override (default: dry run)
        #[arg(long)]
        apply: bool,
        /// Full invocation as one JSON object (schema: `rm-job schema
        /// set-line`); flags are ignored when given
        #[arg(long)]
        json: Option<String>,
    },
    /// Undo a job edit by its edit_log id.
    Undo {
        /// The edit_log id to undo (printed by --apply)
        #[arg(required_unless_present = "json")]
        edit_id: Option<i64>,
        /// SQLite DB path
        #[arg(long, default_value = "data/taichung.db")]
        db: PathBuf,
        /// Editor name recorded on the undo's audit row
        #[arg(long, default_value = "rm-job")]
        editor: String,
        /// Full invocation as one JSON object (schema: `rm-job schema
        /// undo`); flags are ignored when given
        #[arg(long)]
        json: Option<String>,
    },
    /// Find current stops on one route by name substring or exact code
    /// (read-only).
    FindStop {
        /// Route XNO to search within
        #[arg(required_unless_present = "json")]
        xno: Option<String>,
        /// Stop-name substring (matched with SQL LIKE %q%)
        #[arg(long)]
        name: Option<String>,
        /// Exact stop code
        #[arg(long)]
        code: Option<String>,
        /// SQLite DB path
        #[arg(long, default_value = "data/taichung.db")]
        db: PathBuf,
        /// Full invocation as one JSON object (schema: `rm-job schema
        /// find-stop`); flags are ignored when given
        #[arg(long)]
        json: Option<String>,
    },
    /// One route's current stops per direction and per-direction path state
    /// (read-only).
    ShowRoute {
        /// Route XNO
        #[arg(required_unless_present = "json")]
        xno: Option<String>,
        /// SQLite DB path
        #[arg(long, default_value = "data/taichung.db")]
        db: PathBuf,
        /// Full invocation as one JSON object (schema: `rm-job schema
        /// show-route`); flags are ignored when given
        #[arg(long)]
        json: Option<String>,
    },
    /// One route direction's effective polyline: source, encoded, endpoints
    /// (read-only).
    ShowPath {
        /// Route XNO
        #[arg(required_unless_present = "json")]
        xno: Option<String>,
        /// Direction (0 = outbound, 1 = inbound)
        #[arg(required_unless_present = "json")]
        direction: Option<i64>,
        /// SQLite DB path
        #[arg(long, default_value = "data/taichung.db")]
        db: PathBuf,
        /// Full invocation as one JSON object (schema: `rm-job schema
        /// show-path`); flags are ignored when given
        #[arg(long)]
        json: Option<String>,
    },
    /// One entity's edit_log rows, newest first (read-only).
    EditLog {
        /// Entity kind: stop, station, polyline, stop_add or stop_detach
        #[arg(required_unless_present = "json")]
        entity: Option<String>,
        /// The entity's id
        #[arg(required_unless_present = "json")]
        entity_id: Option<i64>,
        /// SQLite DB path
        #[arg(long, default_value = "data/taichung.db")]
        db: PathBuf,
        /// Full invocation as one JSON object (schema: `rm-job schema
        /// edit-log`); flags are ignored when given
        #[arg(long)]
        json: Option<String>,
    },
    /// Print command schemas: every command, or one --json payload schema.
    Schema {
        /// The command to describe (omit: list every command)
        command: Option<String>,
    },
}

const ATTACH_USAGE: &str =
    "usage: --attach XNO:DIR[@CODE] (repeatable; @CODE inserts after that stop code)";

/// Parse one `--attach` value: split `'@'` (insert-after code) then `':'`
/// → `(xno, direction)`.
pub fn parse_attach(s: &str) -> Result<AttachCmd, String> {
    let (body, after) = match s.split_once('@') {
        Some((body, code)) => (body, Some(code)),
        None => (s, None),
    };
    if after == Some("") {
        return Err(format!(
            "invalid --attach '{s}': empty code after '@'; {ATTACH_USAGE}"
        ));
    }
    let (xno, dir) = body
        .split_once(':')
        .ok_or_else(|| format!("invalid --attach '{s}': expected XNO:DIR; {ATTACH_USAGE}"))?;
    let direction = dir.parse::<i64>().map_err(|_| {
        format!("invalid --attach '{s}': direction '{dir}' is not a number; {ATTACH_USAGE}")
    })?;
    Ok(AttachCmd {
        xno: xno.to_string(),
        direction,
        after_stop_code: after.map(str::to_string),
    })
}

const LEVEL_USAGE: &str = "usage: --level station|matching|all";

/// Parse `--level`: rm-web's three rename spreads — the flag spellings are
/// lowercase, rm_db's `RenameLevel::as_str` (the audit row + plan JSON)
/// uses the PascalCase variants.
pub fn parse_level(s: &str) -> Result<RenameLevel, String> {
    match s {
        "station" => Ok(RenameLevel::StationOnly),
        "matching" => Ok(RenameLevel::MatchingPoles),
        "all" => Ok(RenameLevel::AllPoles),
        _ => Err(format!(
            "invalid --level '{s}': expected station, matching or all; {LEVEL_USAGE}"
        )),
    }
}

const POINT_USAGE: &str = "usage: --point LAT,LON (repeatable, in path order)";

/// Parse one `--point LAT,LON`.
pub fn parse_point(s: &str) -> Result<[f64; 2], String> {
    let (lat, lon) = s
        .split_once(',')
        .ok_or_else(|| format!("invalid --point '{s}': expected LAT,LON; {POINT_USAGE}"))?;
    let lat = lat.trim().parse::<f64>().map_err(|_| {
        format!("invalid --point '{s}': latitude '{lat}' is not a number; {POINT_USAGE}")
    })?;
    let lon = lon.trim().parse::<f64>().map_err(|_| {
        format!("invalid --point '{s}': longitude '{lon}' is not a number; {POINT_USAGE}")
    })?;
    Ok([lat, lon])
}

/// What one command concluded; `run` maps it to the envelope and exit code.
enum Outcome {
    /// Success — the command's `data` payload plus any progress logs.
    Ok(serde_json::Value, Vec<String>),
    /// Rejected — human reason for stderr and `data.reason`; exit 1.
    Rejected(String),
    /// No-op — human reason for stderr and `data.reason`; exit 2.
    Noop(String),
}

/// Run one CLI invocation to completion. Returns the process exit code
/// (0 ok, 1 rejected, 2 no-op) and the buffered report; `report.out` is
/// always the envelope JSON, `report.err` the human text.
pub fn run(cli: Cli) -> (i32, Report) {
    let started = Instant::now();
    let (command, outcome) = dispatch(cli);
    let (mut env, code) = match &outcome {
        Outcome::Ok(data, logs) => {
            let mut e = envelope::ok(command, data.clone());
            e.logs = logs.clone();
            (e, 0)
        }
        Outcome::Rejected(reason) => envelope::rejected(command, reason),
        Outcome::Noop(reason) => envelope::noop(command, reason),
    };
    env.duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let mut report = Report {
        out: serde_json::to_string(&env).expect("envelope always serializes"),
        ..Report::default()
    };
    match &outcome {
        Outcome::Ok(..) => {}
        Outcome::Rejected(reason) => {
            let _ = writeln!(report.err, "rejected: {reason}");
        }
        Outcome::Noop(reason) => {
            let _ = writeln!(report.err, "no-op: {reason}");
        }
    }
    (code, report)
}

/// Parse and run one command. Flags are ignored when `--json` carries the
/// invocation; the payload is deserialized into the command's `params`
/// struct. Never writes — the outcome carries everything.
fn dispatch(cli: Cli) -> (&'static str, Outcome) {
    match cli.command {
        Command::AddStop {
            name,
            lat,
            lon,
            attach,
            station,
            db,
            editor,
            note,
            apply,
            json,
        } => {
            let p = match json {
                Some(payload) => match serde_json::from_str::<params::AddStopParams>(&payload) {
                    Ok(p) => p,
                    Err(e) => {
                        return (
                            "add-stop",
                            Outcome::Rejected(format!("invalid --json payload: {e}")),
                        )
                    }
                },
                None => params::AddStopParams {
                    // clap requires these unless --json is present, so the
                    // fallbacks below are unreachable from the real CLI
                    name: name.unwrap_or_default(),
                    lat: lat.unwrap_or(0.0),
                    lon: lon.unwrap_or(0.0),
                    attach,
                    station,
                    db: db.display().to_string(),
                    editor,
                    note,
                    apply,
                },
            };
            add_stop_job(p)
        }
        Command::MoveStop {
            stop_version_id,
            xno,
            code,
            lat,
            lon,
            stop_name,
            db,
            editor,
            note,
            apply,
            json,
        } => {
            let p = match json {
                Some(payload) => match serde_json::from_str::<params::MoveStopParams>(&payload) {
                    Ok(p) => p,
                    Err(e) => {
                        return (
                            "move-stop",
                            Outcome::Rejected(format!("invalid --json payload: {e}")),
                        )
                    }
                },
                None => params::MoveStopParams {
                    stop_version_id,
                    xno,
                    code,
                    lat,
                    lon,
                    stop_name,
                    db: db.display().to_string(),
                    editor,
                    note,
                    apply,
                },
            };
            move_stop_job(p)
        }
        Command::RemoveStop {
            xno,
            direction,
            code,
            sequence,
            all,
            delete_pole,
            db,
            editor,
            note,
            apply,
            json,
        } => {
            let p = match json {
                Some(payload) => match serde_json::from_str::<params::RemoveStopParams>(&payload) {
                    Ok(p) => p,
                    Err(e) => {
                        return (
                            "remove-stop",
                            Outcome::Rejected(format!("invalid --json payload: {e}")),
                        )
                    }
                },
                None => params::RemoveStopParams {
                    // clap requires xno, direction and code unless --json is
                    // present
                    xno: xno.unwrap_or_default(),
                    direction: direction.unwrap_or(0),
                    code: code.unwrap_or_default(),
                    sequence,
                    all,
                    delete_pole,
                    db: db.display().to_string(),
                    editor,
                    note,
                    apply,
                },
            };
            remove_stop_job(p)
        }
        Command::ExtendPath {
            xno,
            direction,
            meters,
            end,
            db,
            editor,
            note,
            apply,
            json,
        } => {
            let p = match json {
                Some(payload) => match serde_json::from_str::<params::ExtendPathParams>(&payload) {
                    Ok(p) => p,
                    Err(e) => {
                        return (
                            "extend-path",
                            Outcome::Rejected(format!("invalid --json payload: {e}")),
                        )
                    }
                },
                None => params::ExtendPathParams {
                    // clap requires xno and direction unless --json is
                    // present
                    xno: xno.unwrap_or_default(),
                    direction: direction.unwrap_or(0),
                    meters,
                    end,
                    db: db.display().to_string(),
                    editor,
                    note,
                    apply,
                },
            };
            extend_path_job(p)
        }
        Command::RegroupStop {
            stop_version_id,
            xno,
            code,
            station,
            db,
            editor,
            note,
            apply,
            json,
        } => {
            let p = match json {
                Some(payload) => match serde_json::from_str::<params::RegroupStopParams>(&payload)
                {
                    Ok(p) => p,
                    Err(e) => {
                        return (
                            "regroup-stop",
                            Outcome::Rejected(format!("invalid --json payload: {e}")),
                        )
                    }
                },
                None => params::RegroupStopParams {
                    stop_version_id,
                    xno,
                    code,
                    // clap requires station unless --json is present
                    station: station.unwrap_or(0),
                    db: db.display().to_string(),
                    editor,
                    note,
                    apply,
                },
            };
            regroup_stop_job(p)
        }
        Command::RenameStation {
            station,
            new_name,
            level,
            db,
            editor,
            note,
            apply,
            json,
        } => {
            let p = match json {
                Some(payload) => match serde_json::from_str::<params::RenameStationParams>(&payload)
                {
                    Ok(p) => p,
                    Err(e) => {
                        return (
                            "rename-station",
                            Outcome::Rejected(format!("invalid --json payload: {e}")),
                        )
                    }
                },
                None => params::RenameStationParams {
                    // clap requires station and new_name unless --json is present
                    station: station.unwrap_or(0),
                    new_name: new_name.unwrap_or_default(),
                    level,
                    db: db.display().to_string(),
                    editor,
                    note,
                    apply,
                },
            };
            rename_station_job(p)
        }
        Command::MoveStation {
            station,
            lat,
            lon,
            db,
            editor,
            note,
            apply,
            json,
        } => {
            let p = match json {
                Some(payload) => match serde_json::from_str::<params::MoveStationParams>(&payload) {
                    Ok(p) => p,
                    Err(e) => {
                        return (
                            "move-station",
                            Outcome::Rejected(format!("invalid --json payload: {e}")),
                        )
                    }
                },
                None => params::MoveStationParams {
                    // clap requires station, lat and lon unless --json is present
                    station: station.unwrap_or(0),
                    lat: lat.unwrap_or(0.0),
                    lon: lon.unwrap_or(0.0),
                    db: db.display().to_string(),
                    editor,
                    note,
                    apply,
                },
            };
            move_station_job(p)
        }
        Command::SetLine {
            xno,
            direction,
            point,
            expected_updated_at,
            db,
            editor,
            note,
            apply,
            json,
        } => {
            // parse the flag points before anything else — a bad LAT,LON
            // must reject without touching the DB ("parse before open")
            let points = match point.iter().map(|s| parse_point(s)).collect() {
                Ok(points) => points,
                Err(e) => return ("set-line", Outcome::Rejected(e)),
            };
            let p = match json {
                Some(payload) => match serde_json::from_str::<params::SetLineParams>(&payload) {
                    Ok(p) => p,
                    Err(e) => {
                        return (
                            "set-line",
                            Outcome::Rejected(format!("invalid --json payload: {e}")),
                        )
                    }
                },
                None => params::SetLineParams {
                    // clap requires xno and direction unless --json is present
                    xno: xno.unwrap_or_default(),
                    direction: direction.unwrap_or(0),
                    points,
                    expected_updated_at,
                    db: db.display().to_string(),
                    editor,
                    note,
                    apply,
                },
            };
            set_line_job(p)
        }
        Command::Undo {
            edit_id,
            db,
            editor,
            json,
        } => {
            let p = match json {
                Some(payload) => match serde_json::from_str::<params::UndoParams>(&payload) {
                    Ok(p) => p,
                    Err(e) => {
                        return ("undo", Outcome::Rejected(format!("invalid --json payload: {e}")))
                    }
                },
                None => params::UndoParams {
                    // clap requires edit_id unless --json is present
                    edit_id: edit_id.unwrap_or(0),
                    db: db.display().to_string(),
                    editor,
                },
            };
            undo_job(p)
        }
        Command::FindStop {
            xno,
            name,
            code,
            db,
            json,
        } => {
            let p = match json {
                Some(payload) => match serde_json::from_str::<params::FindStopParams>(&payload) {
                    Ok(p) => p,
                    Err(e) => {
                        return (
                            "find-stop",
                            Outcome::Rejected(format!("invalid --json payload: {e}")),
                        )
                    }
                },
                None => params::FindStopParams {
                    // clap requires xno unless --json is present
                    xno: xno.unwrap_or_default(),
                    name,
                    code,
                    db: db.display().to_string(),
                },
            };
            find_stop_job(p)
        }
        Command::ShowRoute { xno, db, json } => {
            let p = match json {
                Some(payload) => match serde_json::from_str::<params::ShowRouteParams>(&payload) {
                    Ok(p) => p,
                    Err(e) => {
                        return (
                            "show-route",
                            Outcome::Rejected(format!("invalid --json payload: {e}")),
                        )
                    }
                },
                None => params::ShowRouteParams {
                    // clap requires xno unless --json is present
                    xno: xno.unwrap_or_default(),
                    db: db.display().to_string(),
                },
            };
            show_route_job(p)
        }
        Command::ShowPath {
            xno,
            direction,
            db,
            json,
        } => {
            let p = match json {
                Some(payload) => match serde_json::from_str::<params::ShowPathParams>(&payload) {
                    Ok(p) => p,
                    Err(e) => {
                        return (
                            "show-path",
                            Outcome::Rejected(format!("invalid --json payload: {e}")),
                        )
                    }
                },
                None => params::ShowPathParams {
                    // clap requires xno and direction unless --json is present
                    xno: xno.unwrap_or_default(),
                    direction: direction.unwrap_or(0),
                    db: db.display().to_string(),
                },
            };
            show_path_job(p)
        }
        Command::EditLog {
            entity,
            entity_id,
            db,
            json,
        } => {
            let p = match json {
                Some(payload) => match serde_json::from_str::<params::EditLogParams>(&payload) {
                    Ok(p) => p,
                    Err(e) => {
                        return (
                            "edit-log",
                            Outcome::Rejected(format!("invalid --json payload: {e}")),
                        )
                    }
                },
                None => params::EditLogParams {
                    // clap requires entity and entity_id unless --json is present
                    entity: entity.unwrap_or_default(),
                    entity_id: entity_id.unwrap_or(0),
                    db: db.display().to_string(),
                },
            };
            edit_log_job(p)
        }
        Command::Schema { command } => match command.as_deref() {
            None => ("schema", Outcome::Ok(schema_list(), Vec::new())),
            Some(name) => match schema_for(name) {
                Some(data) => ("schema", Outcome::Ok(data, Vec::new())),
                None => (
                    "schema",
                    Outcome::Rejected(format!(
                        "unknown command '{name}': no --json schema; run `rm-job schema` for the list"
                    )),
                ),
            },
        },
    }
}

/// The add-stop job over one resolved payload.
fn add_stop_job(p: params::AddStopParams) -> (&'static str, Outcome) {
    // parse before open: a rejected attach must not create the db
    let mut attachments = Vec::with_capacity(p.attach.len());
    for a in &p.attach {
        match parse_attach(a) {
            Ok(c) => attachments.push(c),
            Err(e) => return ("add-stop", Outcome::Rejected(e)),
        }
    }
    if attachments.is_empty() {
        return (
            "add-stop",
            Outcome::Noop("nothing to attach — give at least one --attach XNO:DIR[@CODE]".into()),
        );
    }
    let conn = match rm_db::open(&PathBuf::from(&p.db)) {
        Ok(c) => c,
        Err(e) => return ("add-stop", Outcome::Rejected(format!("{e:#}"))),
    };
    let cmd = AddStopCmd {
        name: p.name,
        lat: p.lat,
        lon: p.lon,
        station_id: p.station,
        attachments,
    };
    let mut out = String::new();
    if p.apply {
        match add_stop(&conn, cmd, &p.editor, p.note.as_deref().unwrap_or("")) {
            Ok(o) => write_outcome(&mut out, &o),
            Err(e) => return ("add-stop", Outcome::Rejected(format!("{e:#}"))),
        }
    } else {
        match plan_add_stop(&conn, &cmd) {
            Ok(plan) => write_plan(&mut out, &plan),
            Err(e) => return ("add-stop", Outcome::Rejected(format!("{e:#}"))),
        }
    }
    (
        "add-stop",
        Outcome::Ok(serde_json::json!({ "report": out }), Vec::new()),
    )
}

/// Open the DB by mode: apply legs read-write (`rm_db::open` — migrations
/// run, a missing file is created), dry runs read-only — a dry run must
/// neither migrate an old DB nor create a stray file on a typo'd path.
fn open_conn(apply: bool, db: &str) -> anyhow::Result<rusqlite::Connection> {
    if apply {
        rm_db::open(&PathBuf::from(db))
    } else {
        rm_db::open_readonly(&PathBuf::from(db))
    }
}

/// Resolve the target pole for a stop-level edit: `stop_version_id` if
/// given, else `xno`+`code` must match exactly one current pole. `Err`
/// carries the CLI rejection reason. Shared by move-stop and regroup-stop.
fn resolve_stop_svi(
    conn: &rusqlite::Connection,
    stop_version_id: Option<i64>,
    xno: Option<&str>,
    code: Option<&str>,
) -> Result<i64, String> {
    match stop_version_id {
        Some(svi) => Ok(svi),
        None => match (xno, code) {
            (Some(xno), Some(code)) => match queries::find_stop(conn, xno, None, Some(code)) {
                Ok(candidates) if candidates.len() == 1 => Ok(candidates[0].stop_version_id),
                Ok(candidates) if candidates.is_empty() => Err(format!(
                    "cannot resolve stop: route '{xno}' has no current stop with code '{code}'"
                )),
                Ok(candidates) => {
                    let ids = candidates
                        .iter()
                        .map(|c| c.stop_version_id.to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    Err(format!(
                        "cannot resolve stop: {} current poles on route '{xno}' match \
                         code '{code}' — pass stop_version_id ({ids})",
                        candidates.len()
                    ))
                }
                Err(e) => Err(format!("{e:#}")),
            },
            _ => Err("cannot resolve stop: give stop_version_id, or both xno and code".into()),
        },
    }
}

/// The move-stop job over one resolved payload: resolve the target pole
/// (`stop_version_id` if given, else `xno`+`code` must match exactly one
/// current pole), then preview (dry run) or apply.
fn move_stop_job(p: params::MoveStopParams) -> (&'static str, Outcome) {
    let conn = match open_conn(p.apply, &p.db) {
        Ok(c) => c,
        Err(e) => return ("move-stop", Outcome::Rejected(format!("{e:#}"))),
    };
    let svi = match resolve_stop_svi(
        &conn,
        p.stop_version_id,
        p.xno.as_deref(),
        p.code.as_deref(),
    ) {
        Ok(svi) => svi,
        Err(e) => return ("move-stop", Outcome::Rejected(e)),
    };
    let cmd = StopEdit {
        lat: p.lat,
        lon: p.lon,
        stop_name: p.stop_name,
    };
    if p.apply {
        let out = match edit_stop(&conn, svi, cmd, &p.editor, p.note.as_deref().unwrap_or("")) {
            Ok(o) => o,
            Err(e) => return ("move-stop", Outcome::Rejected(format!("{e:#}"))),
        };
        (
            "move-stop",
            Outcome::Ok(
                serde_json::json!({
                    "edit_id": out.edit_id,
                    "new_stop_version_id": out.new_version_id,
                    "stale_routes": out.stale_routes,
                }),
                Vec::new(),
            ),
        )
    } else {
        match preview_stop_edit(&conn, svi, cmd) {
            Ok(prev) => (
                "move-stop",
                Outcome::Ok(
                    serde_json::json!({
                        "stop_id": prev.stop_id,
                        "stop_version_id": prev.stop_version_id,
                        "changes": {
                            "lat": prev.op.lat,
                            "lon": prev.op.lon,
                            "stop_name": prev.op.stop_name,
                        },
                    }),
                    Vec::new(),
                ),
            ),
            Err(e) => return ("move-stop", Outcome::Rejected(format!("{e:#}"))),
        }
    }
}

/// The regroup-stop job: resolve the target pole like move-stop, then plan.
/// The plan runs on --apply too — its station ids feed the outcome — and the
/// apply leg re-validates currency inside its tx.
fn regroup_stop_job(p: params::RegroupStopParams) -> (&'static str, Outcome) {
    let conn = match open_conn(p.apply, &p.db) {
        Ok(c) => c,
        Err(e) => return ("regroup-stop", Outcome::Rejected(format!("{e:#}"))),
    };
    let svi = match resolve_stop_svi(
        &conn,
        p.stop_version_id,
        p.xno.as_deref(),
        p.code.as_deref(),
    ) {
        Ok(svi) => svi,
        Err(e) => return ("regroup-stop", Outcome::Rejected(e)),
    };
    let cmd = || RegroupStop {
        stop_version_id: svi,
        new_station_id: p.station,
    };
    let plan = match plan_regroup_stop(&conn, cmd()) {
        Ok(plan) => plan,
        Err(e) => return ("regroup-stop", Outcome::Rejected(format!("{e:#}"))),
    };
    if !p.apply {
        return (
            "regroup-stop",
            Outcome::Ok(
                serde_json::json!({
                    "stop_id": plan.stop_id,
                    "stop_version_id": plan.stop_version_id,
                    "stop_code": plan.stop_code,
                    "stop_name": plan.stop_name,
                    "old_station_id": plan.old_station_id,
                    "old_station_name": plan.old_station_name,
                    "new_station_id": plan.new_station_id,
                    "new_station_name": plan.new_station_name,
                }),
                Vec::new(),
            ),
        );
    }
    let out = match regroup_stop(&conn, cmd(), &p.editor, p.note.as_deref().unwrap_or("")) {
        Ok(o) => o,
        Err(e) => return ("regroup-stop", Outcome::Rejected(format!("{e:#}"))),
    };
    (
        "regroup-stop",
        Outcome::Ok(
            serde_json::json!({
                "edit_id": out.edit_id,
                "new_stop_version_id": out.new_version_id,
                "old_station_id": plan.old_station_id,
                "new_station_id": plan.new_station_id,
            }),
            Vec::new(),
        ),
    )
}

/// The rename-station job: parse the level, resolve the station's current
/// version from its stable station_id, then plan (dry run) or apply. Apply
/// reports the churned poles with their new versions.
fn rename_station_job(p: params::RenameStationParams) -> (&'static str, Outcome) {
    let level = match parse_level(&p.level) {
        Ok(l) => l,
        Err(e) => return ("rename-station", Outcome::Rejected(e)),
    };
    let conn = match open_conn(p.apply, &p.db) {
        Ok(c) => c,
        Err(e) => return ("rename-station", Outcome::Rejected(format!("{e:#}"))),
    };
    let svi = match queries::resolve_station(&conn, p.station) {
        Ok(Some((svi, _))) => svi,
        Ok(None) => {
            return (
                "rename-station",
                Outcome::Rejected(format!(
                    "station {} does not exist or is not current",
                    p.station
                )),
            )
        }
        Err(e) => return ("rename-station", Outcome::Rejected(format!("{e:#}"))),
    };
    let cmd = StationRename {
        station_version_id: svi,
        new_name: p.new_name,
        level,
    };
    if p.apply {
        match rename_station(&conn, cmd, &p.editor, p.note.as_deref().unwrap_or("")) {
            Ok(o) => (
                "rename-station",
                Outcome::Ok(
                    serde_json::json!({
                        "edit_id": o.edit_id,
                        "new_station_version_id": o.new_station_version_id,
                        "poles_follow": o.poles.len(),
                        "poles": o.poles
                            .iter()
                            .map(|pl| serde_json::json!({
                                "stop_id": pl.stop_id,
                                "old_stop_version_id": pl.old_svi,
                                "new_stop_version_id": pl.new_svi,
                            }))
                            .collect::<Vec<_>>(),
                    }),
                    Vec::new(),
                ),
            ),
            Err(e) => ("rename-station", Outcome::Rejected(format!("{e:#}"))),
        }
    } else {
        match plan_rename_station(&conn, cmd) {
            Ok(plan) => (
                "rename-station",
                Outcome::Ok(
                    serde_json::json!({
                        "station_id": plan.station_id,
                        "station_version_id": plan.station_version_id,
                        "old_name": plan.old_name,
                        "new_name": plan.new_name,
                        "level": plan.level,
                        "poles_follow": plan.poles.len(),
                        "poles": plan.poles
                            .iter()
                            .map(|pl| serde_json::json!({
                                "stop_id": pl.stop_id,
                                "stop_version_id": pl.stop_version_id,
                                "old_name": pl.old_name,
                                "new_name": pl.new_name,
                            }))
                            .collect::<Vec<_>>(),
                    }),
                    Vec::new(),
                ),
            ),
            Err(e) => ("rename-station", Outcome::Rejected(format!("{e:#}"))),
        }
    }
}

/// The move-station job: range-check the coords before the DB opens (same
/// check as rm-web's endpoint), resolve the station's current version, then
/// plan (dry run) or apply. Poles keep their own coords — the station point
/// is the reference only.
fn move_station_job(p: params::MoveStationParams) -> (&'static str, Outcome) {
    if !(-90.0..=90.0).contains(&p.lat) {
        return ("move-station", Outcome::Rejected("lat out of range".into()));
    }
    if !(-180.0..=180.0).contains(&p.lon) {
        return ("move-station", Outcome::Rejected("lon out of range".into()));
    }
    let conn = match open_conn(p.apply, &p.db) {
        Ok(c) => c,
        Err(e) => return ("move-station", Outcome::Rejected(format!("{e:#}"))),
    };
    let svi = match queries::resolve_station(&conn, p.station) {
        Ok(Some((svi, _))) => svi,
        Ok(None) => {
            return (
                "move-station",
                Outcome::Rejected(format!(
                    "station {} does not exist or is not current",
                    p.station
                )),
            )
        }
        Err(e) => return ("move-station", Outcome::Rejected(format!("{e:#}"))),
    };
    let cmd = StationMove {
        station_version_id: svi,
        latitude: p.lat,
        longitude: p.lon,
    };
    if p.apply {
        match move_station(&conn, cmd, &p.editor, p.note.as_deref().unwrap_or("")) {
            Ok(o) => (
                "move-station",
                Outcome::Ok(
                    serde_json::json!({
                        "edit_id": o.edit_id,
                        "new_station_version_id": o.new_station_version_id,
                    }),
                    Vec::new(),
                ),
            ),
            Err(e) => ("move-station", Outcome::Rejected(format!("{e:#}"))),
        }
    } else {
        match plan_move_station(&conn, cmd) {
            Ok(plan) => (
                "move-station",
                Outcome::Ok(
                    serde_json::json!({
                        "station_id": plan.station_id,
                        "station_version_id": plan.station_version_id,
                        "station_name": plan.station_name,
                        "old_lat": plan.old_latitude,
                        "old_lon": plan.old_longitude,
                        "new_lat": plan.new_latitude,
                        "new_lon": plan.new_longitude,
                    }),
                    Vec::new(),
                ),
            ),
            Err(e) => ("move-station", Outcome::Rejected(format!("{e:#}"))),
        }
    }
}

/// The set-line job over one resolved payload (points already parsed): plan
/// the whole-line override (dry run) or apply it. The dry run reports
/// `override_updated_at` — the concurrency token agents pass back as
/// `expected_updated_at`; overwriting an existing override without it
/// rejects as "edited by someone else".
fn set_line_job(p: params::SetLineParams) -> (&'static str, Outcome) {
    let n = p.points.len();
    let conn = match open_conn(p.apply, &p.db) {
        Ok(c) => c,
        Err(e) => return ("set-line", Outcome::Rejected(format!("{e:#}"))),
    };
    let cmd = LineEditCmd {
        xno: p.xno.clone(),
        direction: p.direction,
        points: p.points,
        expected_updated_at: p.expected_updated_at,
    };
    if p.apply {
        match edit_polyline_line(&conn, cmd, &p.editor, p.note.as_deref().unwrap_or("")) {
            Ok(o) => (
                "set-line",
                Outcome::Ok(
                    serde_json::json!({
                        "edit_id": o.edit_id,
                        "created": o.created,
                        "updated_at": o.updated_at,
                        "points": n,
                    }),
                    Vec::new(),
                ),
            ),
            Err(e) => ("set-line", Outcome::Rejected(format!("{e:#}"))),
        }
    } else {
        match plan_polyline_line(&conn, cmd) {
            Ok(plan) => (
                "set-line",
                Outcome::Ok(
                    serde_json::json!({
                        "route_id": plan.route_id,
                        "route_version_id": plan.route_version_id,
                        "xno": plan.xno,
                        "direction": plan.direction,
                        "replaces": plan.replaces,
                        "created": plan.created,
                        "points": plan.points,
                        "encoded": plan.encoded,
                        "override_updated_at": plan.expected_updated_at,
                    }),
                    Vec::new(),
                ),
            ),
            Err(e) => ("set-line", Outcome::Rejected(format!("{e:#}"))),
        }
    }
}

/// The remove-stop dry-run envelope data: a serde mirror of rm_db's
/// `DetachPlan` (which intentionally stays non-`Serialize`).
#[derive(Serialize)]
struct PlanData {
    xno: String,
    direction: i64,
    route_id: i64,
    route_version_id: i64,
    stop_id: i64,
    stop_version_id: i64,
    stop_code: String,
    stop_name: String,
    occurrences: Vec<PlanOccurrence>,
    pole_orphan: bool,
    serving_directions: Vec<i64>,
    delete_pole: bool,
    stale_routes: Vec<i64>,
    warnings: Vec<String>,
}

/// One detached occurrence in the envelope: the rsv row left out of the
/// new chain and its sequence.
#[derive(Serialize)]
struct PlanOccurrence {
    route_stop_version_id: i64,
    sequence: i64,
}

impl From<&DetachPlan> for PlanData {
    fn from(p: &DetachPlan) -> Self {
        PlanData {
            xno: p.xno.clone(),
            direction: p.direction,
            route_id: p.route_id,
            route_version_id: p.route_version_id,
            stop_id: p.stop_id,
            stop_version_id: p.stop_version_id,
            stop_code: p.stop_code.clone(),
            stop_name: p.stop_name.clone(),
            occurrences: p
                .occurrences
                .iter()
                .map(|o| PlanOccurrence {
                    route_stop_version_id: o.route_stop_version_id,
                    sequence: o.sequence,
                })
                .collect(),
            pole_orphan: p.pole_orphan,
            serving_directions: p.serving_directions.clone(),
            delete_pole: p.delete_pole,
            stale_routes: p.stale_routes.clone(),
            warnings: p.warnings.clone(),
        }
    }
}

/// The remove-stop job over one resolved payload: plan (dry run) or
/// detach (apply) through rm_db's detach primitive. Rejections carry the
/// `EditError` text.
fn remove_stop_job(p: params::RemoveStopParams) -> (&'static str, Outcome) {
    let conn = match open_conn(p.apply, &p.db) {
        Ok(c) => c,
        Err(e) => return ("remove-stop", Outcome::Rejected(format!("{e:#}"))),
    };
    let cmd = DetachCmd {
        xno: p.xno,
        direction: p.direction,
        stop_code: p.code,
        sequence: p.sequence,
        all: p.all,
        delete_pole: p.delete_pole,
    };
    if p.apply {
        match detach_stop(&conn, cmd, &p.editor, p.note.as_deref().unwrap_or("")) {
            Ok(o) => (
                "remove-stop",
                Outcome::Ok(
                    serde_json::json!({
                        "edit_id": o.edit_id,
                        "old_route_version_id": o.old_route_version_id,
                        "new_route_version_id": o.new_route_version_id,
                        "pole_deleted": o.pole_deleted,
                        "stale_routes": o.stale_routes,
                    }),
                    Vec::new(),
                ),
            ),
            Err(e) => return ("remove-stop", Outcome::Rejected(format!("{e:#}"))),
        }
    } else {
        match plan_detach_stop(&conn, &cmd) {
            Ok(plan) => (
                "remove-stop",
                Outcome::Ok(
                    serde_json::to_value(PlanData::from(&plan))
                        .expect("plan mirror always serializes"),
                    Vec::new(),
                ),
            ),
            Err(e) => return ("remove-stop", Outcome::Rejected(format!("{e:#}"))),
        }
    }
}

/// The extend-path job over one resolved payload: plan the outward walk
/// (read-only), then on --apply write it as a whole-line polyline override.
/// Dry runs open read-only; the rejection text carries rm_db's / the walk's
/// own error wording verbatim (e.g. "run rm-osm-prepare").
fn extend_path_job(p: params::ExtendPathParams) -> (&'static str, Outcome) {
    if !matches!(p.end.as_str(), "both" | "first" | "last") {
        return (
            "extend-path",
            Outcome::Rejected(format!(
                "invalid end '{}' — expected both, first or last",
                p.end
            )),
        );
    }
    let conn = match open_conn(p.apply, &p.db) {
        Ok(c) => c,
        Err(e) => return ("extend-path", Outcome::Rejected(format!("{e:#}"))),
    };
    let mut logs = Vec::new();
    let (candidate, extensions, source) =
        match extend::plan_extension(&conn, &p.xno, p.direction, p.meters, &p.end, &mut logs) {
            Ok(v) => v,
            Err(e) => return ("extend-path", Outcome::Rejected(format!("{e:#}"))),
        };
    let exts = serde_json::to_value(&extensions).expect("extensions always serialize");
    if !p.apply {
        return (
            "extend-path",
            Outcome::Ok(
                serde_json::json!({ "source": source, "extensions": exts }),
                logs,
            ),
        );
    }
    if extensions.iter().all(|e| e.new_vertices.is_empty()) {
        return (
            "extend-path",
            Outcome::Noop(format!(
                "graph offers no extension within {} m of either end",
                p.meters
            )),
        );
    }
    // currency token from a fresh snapshot (apply's open migrated the db,
    // so the override table exists here)
    let snap = match rm_db::load_polyline_line(&conn, &p.xno, p.direction) {
        Ok(Some(s)) => s,
        Ok(None) => {
            return (
                "extend-path",
                Outcome::Rejected(format!("route '{}' does not exist", p.xno)),
            )
        }
        Err(e) => return ("extend-path", Outcome::Rejected(format!("{e:#}"))),
    };
    let cmd = LineEditCmd {
        xno: p.xno.clone(),
        direction: p.direction,
        points: candidate.iter().map(|q| [q.lat, q.lon]).collect(),
        expected_updated_at: snap.override_row.map(|o| o.updated_at),
    };
    match edit_polyline_line(&conn, cmd, &p.editor, p.note.as_deref().unwrap_or("")) {
        Ok(o) => (
            "extend-path",
            Outcome::Ok(
                serde_json::json!({
                    "edit_id": o.edit_id,
                    "created": o.created,
                    "updated_at": o.updated_at,
                    "extensions": exts,
                }),
                logs,
            ),
        ),
        Err(e) => return ("extend-path", Outcome::Rejected(format!("{e:#}"))),
    }
}

/// The undo job over one resolved payload.
fn undo_job(p: params::UndoParams) -> (&'static str, Outcome) {
    let conn = match rm_db::open(&PathBuf::from(&p.db)) {
        Ok(c) => c,
        Err(e) => return ("undo", Outcome::Rejected(format!("{e:#}"))),
    };
    let mut out = String::new();
    match undo_edit(&conn, p.edit_id, &p.editor, "") {
        Ok(o) => write_undo(&mut out, p.edit_id, &o),
        Err(e) => return ("undo", Outcome::Rejected(format!("{e:#}"))),
    }
    (
        "undo",
        Outcome::Ok(serde_json::json!({ "report": out }), Vec::new()),
    )
}

/// The find-stop job over one resolved payload: read-only, no side effects.
fn find_stop_job(p: params::FindStopParams) -> (&'static str, Outcome) {
    let conn = match rm_db::open_readonly(&PathBuf::from(&p.db)) {
        Ok(c) => c,
        Err(e) => return ("find-stop", Outcome::Rejected(format!("{e:#}"))),
    };
    match queries::find_stop(&conn, &p.xno, p.name.as_deref(), p.code.as_deref()) {
        Ok(candidates) => (
            "find-stop",
            Outcome::Ok(serde_json::json!({ "candidates": candidates }), Vec::new()),
        ),
        Err(e) => ("find-stop", Outcome::Rejected(format!("{e:#}"))),
    }
}

/// The show-route job over one resolved payload: read-only, no side effects.
fn show_route_job(p: params::ShowRouteParams) -> (&'static str, Outcome) {
    let conn = match rm_db::open_readonly(&PathBuf::from(&p.db)) {
        Ok(c) => c,
        Err(e) => return ("show-route", Outcome::Rejected(format!("{e:#}"))),
    };
    let mut logs = Vec::new();
    match queries::show_route(&conn, &p.xno, &mut logs) {
        Ok(Some(v)) => (
            "show-route",
            Outcome::Ok(
                serde_json::to_value(v).expect("overview always serializes"),
                logs,
            ),
        ),
        Ok(None) => (
            "show-route",
            Outcome::Rejected(format!("route '{}' does not exist", p.xno)),
        ),
        Err(e) => ("show-route", Outcome::Rejected(format!("{e:#}"))),
    }
}

/// The show-path job over one resolved payload: read-only, no side effects.
fn show_path_job(p: params::ShowPathParams) -> (&'static str, Outcome) {
    let conn = match rm_db::open_readonly(&PathBuf::from(&p.db)) {
        Ok(c) => c,
        Err(e) => return ("show-path", Outcome::Rejected(format!("{e:#}"))),
    };
    let mut logs = Vec::new();
    match queries::show_path(&conn, &p.xno, p.direction, &mut logs) {
        Ok(Some(v)) => (
            "show-path",
            Outcome::Ok(
                serde_json::to_value(v).expect("overview always serializes"),
                logs,
            ),
        ),
        Ok(None) => (
            "show-path",
            Outcome::Rejected(format!("route '{}' does not exist", p.xno)),
        ),
        Err(e) => ("show-path", Outcome::Rejected(format!("{e:#}"))),
    }
}

/// The edit-log job: one entity's audit rows, newest first. Unknown entity
/// kinds and ids are an empty list — the log is a fact, not a lookup with
/// a miss.
fn edit_log_job(p: params::EditLogParams) -> (&'static str, Outcome) {
    let conn = match rm_db::open_readonly(&PathBuf::from(&p.db)) {
        Ok(c) => c,
        Err(e) => return ("edit-log", Outcome::Rejected(format!("{e:#}"))),
    };
    match entity_history(&conn, &p.entity, p.entity_id) {
        Ok(rows) => (
            "edit-log",
            Outcome::Ok(serde_json::json!({ "rows": rows }), Vec::new()),
        ),
        Err(e) => ("edit-log", Outcome::Rejected(format!("{e:#}"))),
    }
}

/// The `schema` command's list form: every command with its description.
pub fn schema_list() -> serde_json::Value {
    serde_json::json!({
        "commands": [
            {
                "name": "add-stop",
                "description": "Add a stop and attach it to route directions (dry run unless --apply).",
            },
            {
                "name": "move-stop",
                "description": "Move/rename one stop pole (dry run unless --apply).",
            },
            {
                "name": "remove-stop",
                "description": "Detach a stop occurrence from one route direction (dry run unless --apply).",
            },
            {
                "name": "extend-path",
                "description": "Extend a direction's polyline outward along the OSM road graph, written as a polyline override (dry run unless --apply).",
            },
            {
                "name": "regroup-stop",
                "description": "Move one stop pole to another station (dry run unless --apply).",
            },
            {
                "name": "rename-station",
                "description": "Rename a station, optionally spreading to its poles (dry run unless --apply).",
            },
            {
                "name": "move-station",
                "description": "Move a station's reference point; poles follow their own coords (dry run unless --apply).",
            },
            {
                "name": "set-line",
                "description": "Replace a direction's polyline with a whole-line override (dry run unless --apply).",
            },
            {
                "name": "undo",
                "description": "Undo a job edit by its edit_log id.",
            },
            {
                "name": "find-stop",
                "description": "Find current stops on one route by name substring or exact code (read-only).",
            },
            {
                "name": "show-route",
                "description": "One route's current stops per direction and per-direction path state (read-only).",
            },
            {
                "name": "show-path",
                "description": "One route direction's effective polyline: source, encoded, endpoints (read-only).",
            },
            {
                "name": "edit-log",
                "description": "One entity's edit_log rows, newest first (read-only).",
            },
            {
                "name": "schema",
                "description": "Print command schemas: every command, or one --json payload schema.",
            },
        ]
    })
}

/// One command's `--json` payload schema, or None for an unknown name.
pub fn schema_for(name: &str) -> Option<serde_json::Value> {
    match name {
        "add-stop" => Some(serde_json::json!({
            "command": name,
            "schema": schemars::schema_for!(params::AddStopParams),
        })),
        "move-stop" => Some(serde_json::json!({
            "command": name,
            "schema": schemars::schema_for!(params::MoveStopParams),
        })),
        "remove-stop" => Some(serde_json::json!({
            "command": name,
            "schema": schemars::schema_for!(params::RemoveStopParams),
        })),
        "extend-path" => Some(serde_json::json!({
            "command": name,
            "schema": schemars::schema_for!(params::ExtendPathParams),
        })),
        "regroup-stop" => Some(serde_json::json!({
            "command": name,
            "schema": schemars::schema_for!(params::RegroupStopParams),
        })),
        "rename-station" => Some(serde_json::json!({
            "command": name,
            "schema": schemars::schema_for!(params::RenameStationParams),
        })),
        "move-station" => Some(serde_json::json!({
            "command": name,
            "schema": schemars::schema_for!(params::MoveStationParams),
        })),
        "set-line" => Some(serde_json::json!({
            "command": name,
            "schema": schemars::schema_for!(params::SetLineParams),
        })),
        "undo" => Some(serde_json::json!({
            "command": name,
            "schema": schemars::schema_for!(params::UndoParams),
        })),
        "find-stop" => Some(serde_json::json!({
            "command": name,
            "schema": schemars::schema_for!(params::FindStopParams),
        })),
        "show-route" => Some(serde_json::json!({
            "command": name,
            "schema": schemars::schema_for!(params::ShowRouteParams),
        })),
        "show-path" => Some(serde_json::json!({
            "command": name,
            "schema": schemars::schema_for!(params::ShowPathParams),
        })),
        "edit-log" => Some(serde_json::json!({
            "command": name,
            "schema": schemars::schema_for!(params::EditLogParams),
        })),
        _ => None,
    }
}

/// The dry-run report: station verdict, one line per attach (position +
/// neighbors), per-table row counts, the stale routes, the warnings.
fn write_plan(out: &mut String, plan: &AddStopPlan) {
    let _ = writeln!(
        out,
        "add-stop plan for '{}' @ ({:.6}, {:.6})",
        plan.name, plan.lat, plan.lon
    );
    match &plan.station {
        StationVerdict::UseExisting { station_id, name } => {
            let _ = writeln!(out, "station: use existing station {station_id} '{name}'");
        }
        StationVerdict::Pinned { station_id, name } => {
            let _ = writeln!(out, "station: pinned station {station_id} '{name}'");
        }
        StationVerdict::CreateNew => {
            let _ = writeln!(out, "station: create new station");
        }
    }
    for a in &plan.attachments {
        let _ = writeln!(
            out,
            "route {} dir {}: insert after {}, between {} → {}",
            a.xno,
            a.direction,
            a.insert_after_stop_code.as_deref().unwrap_or("end"),
            a.neighbor_before.as_deref().unwrap_or("-"),
            a.neighbor_after.as_deref().unwrap_or("-"),
        );
    }
    let c = &plan.row_counts;
    let _ = writeln!(
        out,
        "rows: stop {}, stop_version {}, station {}, station_version {}, \
         station_group {}, station_group_version {}, route_version {}, \
         subroute_version {}, route_stop_version {}, stale_mark {}",
        c.stop,
        c.stop_version,
        c.station,
        c.station_version,
        c.station_group,
        c.station_group_version,
        c.route_version,
        c.subroute_version,
        c.route_stop_version,
        c.stale_mark
    );
    let _ = writeln!(
        out,
        "refresh needed for routes: {}",
        ids_csv(&plan.stale_routes)
    );
    for w in &plan.warnings {
        let _ = writeln!(out, "warning: {w}");
    }
}

/// The apply report: the audit id, the minted ids, the stale routes.
fn write_outcome(out: &mut String, o: &AddStopOutcome) {
    let _ = writeln!(
        out,
        "applied: edit_id {}, stop_id {}, station_id {}, station_created {}",
        o.edit_id, o.stop_id, o.station_id, o.station_created
    );
    let _ = writeln!(out, "stop_version_ids: {}", ids_csv(&o.stop_version_ids));
    let _ = writeln!(
        out,
        "refresh needed for routes: {}",
        ids_csv(&o.stale_routes)
    );
}

/// The undo report: the outcome variant per entity kind.
fn write_undo(out: &mut String, undone: i64, o: &UndoOutcome) {
    match o {
        UndoOutcome::StopAdd { edit_id } => {
            let _ = writeln!(
                out,
                "undo ok: edit {undone} reverted — stop_add rows deleted (audit edit_id {edit_id})"
            );
        }
        UndoOutcome::StopDetach {
            edit_id,
            restored_route_version_id,
        } => {
            let _ = writeln!(
                out,
                "undo ok: edit {undone} reverted — route version {restored_route_version_id} restored (audit edit_id {edit_id})"
            );
        }
        UndoOutcome::Stop {
            edit_id,
            restored_version_id,
        } => {
            let _ = writeln!(
                out,
                "undo ok: edit {undone} reverted — stop version {restored_version_id} restored (audit edit_id {edit_id})"
            );
        }
        UndoOutcome::Station {
            edit_id,
            restored_version_id,
        } => {
            let _ = writeln!(
                out,
                "undo ok: edit {undone} reverted — station version {restored_version_id} restored (audit edit_id {edit_id})"
            );
        }
        UndoOutcome::Polyline { edit_id, deleted } => {
            let _ = writeln!(
                out,
                "undo ok: edit {undone} reverted — polyline override removed: {deleted} (audit edit_id {edit_id})"
            );
        }
    }
}

fn ids_csv(ids: &[i64]) -> String {
    ids.iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
