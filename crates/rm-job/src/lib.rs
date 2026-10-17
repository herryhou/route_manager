//! rm-job: dry-run-first job CLI over `rm_db`'s manual-edit primitives.
//!
//! The binary is a thin wrapper (parse → run → print → exit); everything
//! testable lives here. SQLite is touched only through `rm_db`. Every run
//! prints exactly one envelope JSON object to stdout (`envelope`); human
//! text goes to stderr.

pub mod envelope;
pub mod params;
pub mod queries;

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Instant;

use clap::{Parser, Subcommand};

use rm_db::{
    add_stop, plan_add_stop, undo_edit, AddStopCmd, AddStopOutcome, AddStopPlan, AttachCmd,
    StationVerdict, UndoOutcome,
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

/// The `schema` command's list form: every command with its description.
pub fn schema_list() -> serde_json::Value {
    serde_json::json!({
        "commands": [
            {
                "name": "add-stop",
                "description": "Add a stop and attach it to route directions (dry run unless --apply).",
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
