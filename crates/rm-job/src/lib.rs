//! rm-job: dry-run-first job CLI over `rm_db`'s manual-edit primitives.
//!
//! The binary is a thin wrapper (parse → run → print → exit); everything
//! testable lives here. SQLite is touched only through `rm_db`.

use std::fmt::Write as _;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

use rm_db::{
    add_stop, plan_add_stop, undo_edit, AddStopCmd, AddStopOutcome, AddStopPlan, AttachCmd,
    StationVerdict, UndoOutcome,
};

/// Everything one run wrote: `out` is the report (stdout), `err` the
/// rejection reason or usage line (stderr). Both buffered so tests can
/// assert content without process capture.
#[derive(Debug, Default)]
pub struct Report {
    pub out: String,
    pub err: String,
}

#[derive(Parser)]
#[command(name = "rm-job", about = "Dry-run-first job CLI over rm_db edits")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Add a stop and attach it to route directions (dry run unless --apply).
    AddStop {
        /// New pole's name
        name: String,
        /// Latitude
        #[arg(long)]
        lat: f64,
        /// Longitude
        #[arg(long)]
        lon: f64,
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
    },
    /// Undo a job edit by its edit_log id.
    Undo {
        /// The edit_log id to undo (printed by --apply)
        edit_id: i64,
        /// SQLite DB path
        #[arg(long, default_value = "data/taichung.db")]
        db: PathBuf,
        /// Editor name recorded on the undo's audit row
        #[arg(long, default_value = "rm-job")]
        editor: String,
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

/// Run one CLI invocation to completion. Returns the process exit code
/// (0 ok, 1 rejected, 2 no-op) and the buffered report.
pub fn run(cli: Cli) -> (i32, Report) {
    let mut report = Report::default();
    let code = match cli.command {
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
        } => {
            // parse before open: a rejected attach must not create the db
            let mut attachments = Vec::with_capacity(attach.len());
            for a in &attach {
                match parse_attach(a) {
                    Ok(c) => attachments.push(c),
                    Err(e) => {
                        report.err.push_str(&e);
                        report.err.push('\n');
                        return (1, report);
                    }
                }
            }
            if attachments.is_empty() {
                report.err.push_str(
                    "no-op: nothing to attach — give at least one --attach XNO:DIR[@CODE]\n",
                );
                return (2, report);
            }
            let conn = match rm_db::open(&db) {
                Ok(c) => c,
                Err(e) => return reject(&mut report, e),
            };
            let cmd = AddStopCmd {
                name,
                lat,
                lon,
                station_id: station,
                attachments,
            };
            if apply {
                match add_stop(&conn, cmd, &editor, note.as_deref().unwrap_or("")) {
                    Ok(o) => write_outcome(&mut report.out, &o),
                    Err(e) => return reject(&mut report, e),
                }
            } else {
                match plan_add_stop(&conn, &cmd) {
                    Ok(plan) => write_plan(&mut report.out, &plan),
                    Err(e) => return reject(&mut report, e),
                }
            }
            0
        }
        Command::Undo {
            edit_id,
            db,
            editor,
        } => {
            let conn = match rm_db::open(&db) {
                Ok(c) => c,
                Err(e) => return reject(&mut report, e),
            };
            match undo_edit(&conn, edit_id, &editor, "") {
                Ok(o) => write_undo(&mut report.out, edit_id, &o),
                Err(e) => return reject(&mut report, e),
            }
            0
        }
    };
    (code, report)
}

/// Rejection: the reason to the stderr buffer, exit 1.
fn reject(report: &mut Report, e: anyhow::Error) -> (i32, Report) {
    let _ = writeln!(report.err, "rejected: {e:#}");
    (1, std::mem::take(report))
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
