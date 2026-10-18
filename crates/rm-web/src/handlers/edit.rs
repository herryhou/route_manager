//! Edit endpoints — mounted only when --edit passes tokens.

use axum::extract::{FromRef, FromRequest, FromRequestParts, Path, Query, Request, State};
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::Json;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::ApiError;
use crate::state::{run_db, run_db_write, AppState};

/// Bearer-token → editor name. 401 absent, 403 unknown.
pub struct Editor(pub String);

impl<S> FromRequestParts<S> for Editor
where
    S: Send + Sync,
    AppState: FromRef<S>,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app = AppState::from_ref(state);
        let tokens = app
            .write_tokens()
            .ok_or_else(|| ApiError::not_found("edit"))?;
        let hdr = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(|| ApiError::unauthorized("missing bearer token"))?;
        // tokens maps editor name → token, so the presented token is a VALUE:
        // reverse lookup, then report the matched name as the editor.
        let editor = tokens
            .iter()
            .find(|(_, tok)| tok.as_str() == hdr)
            .map(|(name, _)| name.clone())
            .ok_or_else(|| ApiError::forbidden("unknown token"))?;
        Ok(Editor(editor))
    }
}

/// JSON body answering 400 on any malformed payload (plain `Json` uses
/// 422 for type errors, 415 for a missing content-type).
pub struct ApiJson<T>(T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let Json(v) = Json::<T>::from_request(req, state)
            .await
            .map_err(|e| ApiError::bad_request(&e.body_text()))?;
        Ok(ApiJson(v))
    }
}

#[derive(Deserialize)]
pub struct StopEditBody {
    lat: Option<f64>,
    lon: Option<f64>,
    stop_name: Option<String>,
    note: Option<String>,
    apply: Option<bool>,
}

/// Serialize mirror of rm_db's `StopEditPreview` (non-`Serialize` by
/// design) — field names verbatim, nested `op` included.
#[derive(Serialize)]
pub struct StopEditPreviewData {
    stop_id: i64,
    stop_version_id: i64,
    current_stop_name: String,
    current_lat: f64,
    current_lon: f64,
    op: StopEditOpData,
}

#[derive(Serialize)]
pub struct StopEditOpData {
    stop_id: i64,
    stop_version_id: i64,
    stop_name: Option<String>,
    lat: Option<f64>,
    lon: Option<f64>,
}

impl From<&rm_db::StopEditPreview> for StopEditPreviewData {
    fn from(p: &rm_db::StopEditPreview) -> Self {
        Self {
            stop_id: p.stop_id,
            stop_version_id: p.stop_version_id,
            current_stop_name: p.current_stop_name.clone(),
            current_lat: p.current_lat,
            current_lon: p.current_lon,
            op: StopEditOpData {
                stop_id: p.op.stop_id,
                stop_version_id: p.op.stop_version_id,
                stop_name: p.op.stop_name.clone(),
                lat: p.op.lat,
                lon: p.op.lon,
            },
        }
    }
}

/// `rm_db::EditError` splits into request problems (400: empty/no-op
/// edit, invalid line override, unknown route) and races / bad targets
/// (409: version no longer current, un-undoable edit, unknown regroup
/// target).
fn edit_conflict(err: ApiError) -> ApiError {
    if let Some(e) = err.downcast_ref::<rm_db::EditError>() {
        return match e {
            rm_db::EditError::EmptyEdit
            | rm_db::EditError::NoOpEdit
            | rm_db::EditError::EmptyNewName
            | rm_db::EditError::NoOpRename
            | rm_db::EditError::InvalidLine(_)
            | rm_db::EditError::UnknownRoute(_)
            | rm_db::EditError::UnknownDirection(_, _)
            | rm_db::EditError::UnknownAfterStop(_, _, _)
            | rm_db::EditError::OffPathStop { .. }
            | rm_db::EditError::DuplicateAttach(_, _)
            // detach verdicts: both ask the caller to adjust the request
            // (--sequence/--all, --delete-pole), not retry later.
            | rm_db::EditError::DuplicateStopOccurrence { .. }
            | rm_db::EditError::OrphanPoleRefused(_) => ApiError::bad_request(&e.to_string()),
            rm_db::EditError::NotCurrent(_)
            | rm_db::EditError::StationNotCurrent(_)
            | rm_db::EditError::NotUndoable(_)
            | rm_db::EditError::UnknownStation(_)
            | rm_db::EditError::LineNotCurrent(_, _) => ApiError::conflict(&e.to_string()),
        };
    }
    err
}

/// POST /api/edit/stop/{stop_version_id} — append a manual stop version.
/// The default (and `apply: true`) performs the write; `apply: false`
/// answers the read-only preview (`preview_stop_edit`).
pub async fn edit_stop(
    Path(stop_version_id): Path<i64>,
    State(state): State<AppState>,
    Editor(editor): Editor,
    ApiJson(b): ApiJson<StopEditBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let StopEditBody {
        lat,
        lon,
        stop_name,
        note,
        apply,
    } = b;
    if lat.is_none() && lon.is_none() && stop_name.is_none() {
        return Err(ApiError::bad_request("no fields to edit"));
    }
    if let Some(lat) = lat {
        if !(-90.0..=90.0).contains(&lat) {
            return Err(ApiError::bad_request("lat out of range"));
        }
    }
    if let Some(lon) = lon {
        if !(-180.0..=180.0).contains(&lon) {
            return Err(ApiError::bad_request("lon out of range"));
        }
    }
    if apply == Some(false) {
        let plan = run_db(state, move |conn| {
            rm_db::preview_stop_edit(
                conn,
                stop_version_id,
                rm_db::StopEdit {
                    lat,
                    lon,
                    stop_name,
                },
            )
        })
        .await
        .map_err(edit_conflict)?;
        return Ok(Json(serde_json::to_value(StopEditPreviewData::from(
            &plan,
        ))?));
    }
    let note = note.unwrap_or_default();
    let out = run_db_write(state, move |conn| {
        rm_db::edit_stop(
            conn,
            stop_version_id,
            rm_db::StopEdit {
                lat,
                lon,
                stop_name,
            },
            &editor,
            &note,
        )
    })
    .await
    .map_err(edit_conflict)?;
    Ok(Json(json!({
        "edit_id": out.edit_id,
        "new_stop_version_id": out.new_version_id,
        "stale_routes": out.stale_routes,
    })))
}

#[derive(Deserialize)]
pub struct AddStopBody {
    name: String,
    lat: f64,
    lon: f64,
    station: Option<i64>,
    attach: Vec<String>,
    note: Option<String>,
    apply: Option<bool>,
}

/// Serialize mirrors of the add-stop plan types — `rm_db`'s structs carry
/// no serde derives by design, and the response contract is the rm_db
/// struct's field names verbatim.
#[derive(Serialize)]
pub struct AddStopPlanData {
    name: String,
    lat: f64,
    lon: f64,
    station: StationVerdictData,
    attachments: Vec<PlannedAttachData>,
    row_counts: RowCountsData,
    stale_routes: Vec<i64>,
    warnings: Vec<String>,
}

#[derive(Serialize)]
pub enum StationVerdictData {
    UseExisting { station_id: i64, name: String },
    CreateNew,
    Pinned { station_id: i64, name: String },
}

#[derive(Serialize)]
pub struct PlannedAttachData {
    xno: String,
    direction: i64,
    route_id: i64,
    route_version_id: i64,
    insert_after_stop_code: Option<String>,
    neighbor_before: Option<String>,
    neighbor_after: Option<String>,
}

#[derive(Serialize)]
pub struct RowCountsData {
    stop: u32,
    stop_version: u32,
    station: u32,
    station_version: u32,
    station_group: u32,
    station_group_version: u32,
    route_version: u32,
    subroute_version: u32,
    route_stop_version: u32,
    stale_mark: u32,
}

impl From<rm_db::AddStopPlan> for AddStopPlanData {
    fn from(p: rm_db::AddStopPlan) -> Self {
        Self {
            name: p.name,
            lat: p.lat,
            lon: p.lon,
            station: match p.station {
                rm_db::StationVerdict::UseExisting { station_id, name } => {
                    StationVerdictData::UseExisting { station_id, name }
                }
                rm_db::StationVerdict::CreateNew => StationVerdictData::CreateNew,
                rm_db::StationVerdict::Pinned { station_id, name } => {
                    StationVerdictData::Pinned { station_id, name }
                }
            },
            attachments: p
                .attachments
                .into_iter()
                .map(|a| PlannedAttachData {
                    xno: a.xno,
                    direction: a.direction,
                    route_id: a.route_id,
                    route_version_id: a.route_version_id,
                    insert_after_stop_code: a.insert_after_stop_code,
                    neighbor_before: a.neighbor_before,
                    neighbor_after: a.neighbor_after,
                })
                .collect(),
            row_counts: RowCountsData {
                stop: p.row_counts.stop,
                stop_version: p.row_counts.stop_version,
                station: p.row_counts.station,
                station_version: p.row_counts.station_version,
                station_group: p.row_counts.station_group,
                station_group_version: p.row_counts.station_group_version,
                route_version: p.row_counts.route_version,
                subroute_version: p.row_counts.subroute_version,
                route_stop_version: p.row_counts.route_stop_version,
                stale_mark: p.row_counts.stale_mark,
            },
            stale_routes: p.stale_routes,
            warnings: p.warnings,
        }
    }
}

/// POST /api/edit/stop — add a new pole to one or more directions. The
/// default (and `apply: false`) answers the read-only dry-run plan;
/// `apply: true` performs the write. Attach strings parse before any
/// connection opens and an empty attach list is a 400 — both carried
/// over from the retired job CLI's add-stop.
pub async fn add_stop(
    State(state): State<AppState>,
    Editor(editor): Editor,
    ApiJson(b): ApiJson<AddStopBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if !(-90.0..=90.0).contains(&b.lat) {
        return Err(ApiError::bad_request("lat out of range"));
    }
    if !(-180.0..=180.0).contains(&b.lon) {
        return Err(ApiError::bad_request("lon out of range"));
    }
    // parse before open: a rejected attach string must not touch the db
    let mut attachments = Vec::with_capacity(b.attach.len());
    for a in &b.attach {
        attachments.push(rm_db::parse_attach(a).map_err(|e| ApiError::bad_request(&e))?);
    }
    if attachments.is_empty() {
        return Err(ApiError::bad_request(
            "nothing to attach — give at least one attach XNO:DIR[@CODE]",
        ));
    }
    let cmd = rm_db::AddStopCmd {
        name: b.name,
        lat: b.lat,
        lon: b.lon,
        station_id: b.station,
        attachments,
    };
    if !b.apply.unwrap_or(false) {
        let plan = run_db(state, move |conn| rm_db::plan_add_stop(conn, &cmd))
            .await
            .map_err(edit_conflict)?;
        return Ok(Json(serde_json::to_value(AddStopPlanData::from(plan))?));
    }
    let out = run_db_write(state, move |conn| {
        rm_db::add_stop(conn, cmd, &editor, b.note.as_deref().unwrap_or(""))
    })
    .await
    .map_err(edit_conflict)?;
    Ok(Json(json!({
        "edit_id": out.edit_id,
        "stop_id": out.stop_id,
        "stop_version_ids": out.stop_version_ids,
        "station_id": out.station_id,
        "station_created": out.station_created,
        "stale_routes": out.stale_routes,
    })))
}

#[derive(Deserialize)]
pub struct DetachStopBody {
    xno: String,
    direction: i64,
    code: String,
    sequence: Option<i64>,
    all: Option<bool>,
    delete_pole: Option<bool>,
    note: Option<String>,
    apply: Option<bool>,
}

/// Serialize mirror of rm_db's `DetachPlan` (which intentionally stays
/// non-`Serialize`) — same shape as the retired job CLI's `PlanData`/`PlanOccurrence`.
#[derive(Serialize)]
pub struct DetachPlanData {
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

#[derive(Serialize)]
pub struct PlanOccurrence {
    route_stop_version_id: i64,
    sequence: i64,
}

impl From<&rm_db::DetachPlan> for DetachPlanData {
    fn from(p: &rm_db::DetachPlan) -> Self {
        Self {
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

/// POST /api/edit/stop/detach — remove stop `code` from a route direction
/// by re-versioning (never deleting history). The default (and
/// `apply: false`) answers the read-only dry-run plan; `apply: true`
/// performs the write. The orphan guard lives in rm_db: an apply that
/// would drop the pole's last serving direction is refused unless the
/// request carries `delete_pole` — whose teardown is one-way (the whole
/// edit is not undoable).
pub async fn detach_stop(
    State(state): State<AppState>,
    Editor(editor): Editor,
    ApiJson(b): ApiJson<DetachStopBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let cmd = rm_db::DetachCmd {
        xno: b.xno,
        direction: b.direction,
        stop_code: b.code,
        sequence: b.sequence,
        all: b.all.unwrap_or(false),
        delete_pole: b.delete_pole.unwrap_or(false),
    };
    if !b.apply.unwrap_or(false) {
        let plan = run_db(state, move |conn| rm_db::plan_detach_stop(conn, &cmd))
            .await
            .map_err(edit_conflict)?;
        return Ok(Json(serde_json::to_value(DetachPlanData::from(&plan))?));
    }
    let out = run_db_write(state, move |conn| {
        rm_db::detach_stop(conn, cmd, &editor, b.note.as_deref().unwrap_or(""))
    })
    .await
    .map_err(edit_conflict)?;
    Ok(Json(json!({
        "edit_id": out.edit_id,
        "old_route_version_id": out.old_route_version_id,
        "new_route_version_id": out.new_route_version_id,
        "pole_deleted": out.pole_deleted,
        "stale_routes": out.stale_routes,
    })))
}

#[derive(Deserialize)]
pub struct ExtendBody {
    meters: f64,
    end: String,
    note: Option<String>,
    apply: Option<bool>,
}

/// Serialize mirror of `rm_osm_match::extend::Extension` — the response
/// contract is the struct's field names verbatim.
#[derive(Serialize)]
pub struct ExtensionData {
    end: String,
    way_ids: Vec<i64>,
    new_vertices: Vec<[f64; 2]>,
    snap_dist_m: f64,
}

impl From<&rm_osm_match::extend::Extension> for ExtensionData {
    fn from(e: &rm_osm_match::extend::Extension) -> Self {
        Self {
            end: e.end.clone(),
            way_ids: e.way_ids.clone(),
            new_vertices: e.new_vertices.clone(),
            snap_dist_m: e.snap_dist_m,
        }
    }
}

/// Extend-plan rejections (bad meters/end, endpoint beyond the 50 m snap
/// limit, empty osm tables) are request problems, not server faults — but
/// `plan_extension` reports them as plain anyhow strings. This marker
/// carries the message through the closure; the handler unwraps it to a
/// 400 (edit errors keep the `edit_conflict` mapping).
#[derive(Debug)]
struct PlanError(String);

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for PlanError {}

/// POST /api/edit/polyline/{xno}/{direction}/extend — walk the OSM road
/// graph outward from the effective polyline's chosen end(s). The default
/// (and `apply: false`) answers the read-only plan; `apply: true` plans
/// and writes inside one closure, so "both" with either end failing
/// leaves nothing behind. The write is a whole-line override — the same
/// primitive `.../line` stores — so undo works through /api/edit/undo.
pub async fn extend_polyline(
    Path((xno, direction)): Path<(String, i64)>,
    State(state): State<AppState>,
    Editor(editor): Editor,
    ApiJson(b): ApiJson<ExtendBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ExtendBody {
        meters,
        end,
        note,
        apply,
    } = b;
    if meters <= 0.0 {
        return Err(ApiError::bad_request(&format!(
            "meters must be > 0 (got {meters})"
        )));
    }
    if !matches!(end.as_str(), "both" | "first" | "last") {
        return Err(ApiError::bad_request(&format!(
            "invalid end '{end}' — expected both, first or last"
        )));
    }
    let map_plan_err = |e: ApiError| match e.downcast_ref::<PlanError>() {
        Some(p) => ApiError::bad_request(&p.0),
        None => e,
    };
    if !apply.unwrap_or(false) {
        let plan = run_db(state, move |conn| {
            let mut logs = Vec::new();
            let (candidate, extensions, source) = rm_osm_match::extend::plan_extension(
                conn, &xno, direction, meters, &end, &mut logs,
            )
            .map_err(|e| PlanError(e.to_string()))?;
            Ok(json!({
                "source": source,
                "candidate": candidate
                    .iter()
                    .map(|p| [p.lat, p.lon])
                    .collect::<Vec<[f64; 2]>>(),
                "extensions": extensions
                    .iter()
                    .map(ExtensionData::from)
                    .collect::<Vec<ExtensionData>>(),
                "logs": logs,
            }))
        })
        .await
        .map_err(map_plan_err)?;
        return Ok(Json(plan));
    }
    let note = note.unwrap_or_default();
    let out = run_db_write(state, move |conn| {
        let mut logs = Vec::new();
        let (candidate, extensions, source) =
            rm_osm_match::extend::plan_extension(conn, &xno, direction, meters, &end, &mut logs)
                .map_err(|e| PlanError(e.to_string()))?;
        let extensions = extensions
            .iter()
            .map(ExtensionData::from)
            .collect::<Vec<ExtensionData>>();
        // the override row's updated_at doubles as the concurrency token;
        // None when the effective line is still plain src
        let expected_updated_at = rm_db::load_polyline_line(conn, &xno, direction)?
            .ok_or_else(|| anyhow::anyhow!("route '{xno}' does not exist"))?
            .override_row
            .map(|o| o.updated_at);
        let applied = rm_db::edit_polyline_line(
            conn,
            rm_db::LineEditCmd {
                xno,
                direction,
                points: candidate
                    .iter()
                    .map(|p| [p.lat, p.lon])
                    .collect::<Vec<[f64; 2]>>(),
                expected_updated_at,
            },
            &editor,
            &note,
        )?;
        Ok(json!({
            "edit_id": applied.edit_id,
            "updated_at": applied.updated_at,
            "created": applied.created,
            "source": source,
            "extensions": extensions,
        }))
    })
    .await
    .map_err(map_plan_err)
    .map_err(edit_conflict)?;
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct LogParams {
    entity: String,
    entity_id: i64,
}

#[derive(Deserialize)]
pub struct UndoBody {
    note: Option<String>,
}

#[derive(Deserialize)]
pub struct RegroupBody {
    new_station_id: i64,
    note: Option<String>,
    apply: Option<bool>,
}

/// Serialize mirror of rm_db's `RegroupStopPlan` (non-`Serialize` by
/// design) — field names verbatim, nested `op` included.
#[derive(Serialize)]
pub struct RegroupStopPlanData {
    stop_id: i64,
    stop_version_id: i64,
    stop_code: String,
    stop_name: String,
    old_station_id: i64,
    old_station_name: String,
    new_station_id: i64,
    new_station_name: String,
    op: StopRegroupOpData,
}

#[derive(Serialize)]
pub struct StopRegroupOpData {
    stop_id: i64,
    stop_version_id: i64,
    new_station_id: i64,
}

impl From<&rm_db::RegroupStopPlan> for RegroupStopPlanData {
    fn from(p: &rm_db::RegroupStopPlan) -> Self {
        Self {
            stop_id: p.stop_id,
            stop_version_id: p.stop_version_id,
            stop_code: p.stop_code.clone(),
            stop_name: p.stop_name.clone(),
            old_station_id: p.old_station_id,
            old_station_name: p.old_station_name.clone(),
            new_station_id: p.new_station_id,
            new_station_name: p.new_station_name.clone(),
            op: StopRegroupOpData {
                stop_id: p.op.stop_id,
                stop_version_id: p.op.stop_version_id,
                new_station_id: p.op.new_station_id,
            },
        }
    }
}

/// POST /api/edit/stop/{stop_version_id}/regroup — move a stop to another
/// station. The default (and `apply: true`) performs the write;
/// `apply: false` answers the read-only plan (`plan_regroup_stop`). The
/// regroup primitive writes `action='move_station'`.
pub async fn regroup(
    Path(stop_version_id): Path<i64>,
    State(state): State<AppState>,
    Editor(editor): Editor,
    ApiJson(b): ApiJson<RegroupBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if b.apply == Some(false) {
        let cmd = rm_db::RegroupStop {
            stop_version_id,
            new_station_id: b.new_station_id,
        };
        let plan = run_db(state, move |conn| rm_db::plan_regroup_stop(conn, cmd))
            .await
            .map_err(edit_conflict)?;
        return Ok(Json(serde_json::to_value(RegroupStopPlanData::from(
            &plan,
        ))?));
    }
    let out = run_db_write(state, move |conn| {
        rm_db::regroup_stop(
            conn,
            rm_db::RegroupStop {
                stop_version_id,
                new_station_id: b.new_station_id,
            },
            &editor,
            b.note.as_deref().unwrap_or(""),
        )
    })
    .await
    .map_err(edit_conflict)?;
    Ok(Json(json!({
        "edit_id": out.edit_id,
        "new_stop_version_id": out.new_version_id,
    })))
}

/// GET /api/edit/log?entity=stop&entity_id=1 — audit trail for one entity
/// (unauthenticated: the log is not secret; notes are for the team).
pub async fn log(
    Query(q): Query<LogParams>,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let rows = run_db(state, move |conn| {
        rm_db::entity_history(conn, &q.entity, q.entity_id)
    })
    .await?;
    Ok(Json(json!({ "rows": rows })))
}

/// POST /api/edit/undo/{edit_id} — revert an edit by restoring the version
/// that preceded it (or deleting an add's own rows). rm_db's dispatcher
/// picks the kind's undo service and returns one tagged outcome (`kind`:
/// stop/stop_add/station/polyline) either way; unknown ids route down the
/// stop arm and surface as its 409 NotUndoable, as before.
pub async fn undo(
    Path(edit_id): Path<i64>,
    State(state): State<AppState>,
    Editor(editor): Editor,
    ApiJson(b): ApiJson<UndoBody>,
) -> Result<Json<rm_db::UndoOutcome>, ApiError> {
    let out = run_db_write(state, move |conn| {
        rm_db::undo_edit(conn, edit_id, &editor, b.note.as_deref().unwrap_or(""))
    })
    .await
    .map_err(edit_conflict)?;
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct StationRenameBody {
    new_name: String,
    level: String,
    note: Option<String>,
    apply: Option<bool>,
}

/// Serialize mirrors of rm_db's `StationRenamePlan` / `PoleRenamePlan` /
/// `StationRenameOp` / `PoleOp` (non-`Serialize` by design) — field names
/// verbatim.
#[derive(Serialize)]
pub struct StationRenamePlanData {
    station_id: i64,
    station_version_id: i64,
    old_name: String,
    new_name: String,
    level: String,
    poles: Vec<PoleRenamePlanData>,
    op: StationRenameOpData,
}

#[derive(Serialize)]
pub struct PoleRenamePlanData {
    stop_id: i64,
    stop_version_id: i64,
    old_name: String,
    new_name: String,
}

#[derive(Serialize)]
pub struct StationRenameOpData {
    station_id: i64,
    old_station_version_id: i64,
    old_station_name: String,
    new_station_name: String,
    level: String,
    poles: Vec<PoleOpData>,
}

#[derive(Serialize)]
pub struct PoleOpData {
    stop_id: i64,
    old_stop_version_id: i64,
    new_stop_name: String,
}

impl From<&rm_db::StationRenamePlan> for StationRenamePlanData {
    fn from(p: &rm_db::StationRenamePlan) -> Self {
        Self {
            station_id: p.station_id,
            station_version_id: p.station_version_id,
            old_name: p.old_name.clone(),
            new_name: p.new_name.clone(),
            level: p.level.clone(),
            poles: p
                .poles
                .iter()
                .map(|pole| PoleRenamePlanData {
                    stop_id: pole.stop_id,
                    stop_version_id: pole.stop_version_id,
                    old_name: pole.old_name.clone(),
                    new_name: pole.new_name.clone(),
                })
                .collect(),
            op: StationRenameOpData {
                station_id: p.op.station_id,
                old_station_version_id: p.op.old_station_version_id,
                old_station_name: p.op.old_station_name.clone(),
                new_station_name: p.op.new_station_name.clone(),
                level: p.op.level.clone(),
                poles: p
                    .op
                    .poles
                    .iter()
                    .map(|pole| PoleOpData {
                        stop_id: pole.stop_id,
                        old_stop_version_id: pole.old_stop_version_id,
                        new_stop_name: pole.new_stop_name.clone(),
                    })
                    .collect(),
            },
        }
    }
}

/// POST /api/edit/station/{station_version_id} — rename the station, with
/// an editor-chosen level for how far the rename reaches into pole names.
/// The default (and `apply: true`) performs the write; `apply: false`
/// answers the read-only plan (`plan_rename_station`).
pub async fn rename_station(
    Path(station_version_id): Path<i64>,
    State(state): State<AppState>,
    Editor(editor): Editor,
    ApiJson(b): ApiJson<StationRenameBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let level = match b.level.as_str() {
        "station" => rm_db::RenameLevel::StationOnly,
        "matching" => rm_db::RenameLevel::MatchingPoles,
        "all" => rm_db::RenameLevel::AllPoles,
        other => return Err(ApiError::bad_request(&format!("unknown level {other:?}"))),
    };
    if b.apply == Some(false) {
        let cmd = rm_db::StationRename {
            station_version_id,
            new_name: b.new_name,
            level,
        };
        let plan = run_db(state, move |conn| rm_db::plan_rename_station(conn, cmd))
            .await
            .map_err(edit_conflict)?;
        return Ok(Json(serde_json::to_value(StationRenamePlanData::from(
            &plan,
        ))?));
    }
    let out = run_db_write(state, move |conn| {
        rm_db::rename_station(
            conn,
            rm_db::StationRename {
                station_version_id,
                new_name: b.new_name,
                level,
            },
            &editor,
            b.note.as_deref().unwrap_or(""),
        )
    })
    .await
    .map_err(edit_conflict)?;
    Ok(Json(json!({
        "edit_id": out.edit_id,
        "new_station_version_id": out.new_station_version_id,
    })))
}

#[derive(Deserialize)]
pub struct StationMoveBody {
    lat: f64,
    lon: f64,
    note: Option<String>,
    apply: Option<bool>,
}

/// Serialize mirror of rm_db's `StationMovePlan` (non-`Serialize` by
/// design) — field names verbatim, nested `op` included.
#[derive(Serialize)]
pub struct StationMovePlanData {
    station_id: i64,
    station_version_id: i64,
    station_name: String,
    old_latitude: f64,
    old_longitude: f64,
    new_latitude: f64,
    new_longitude: f64,
    op: StationMoveOpData,
}

#[derive(Serialize)]
pub struct StationMoveOpData {
    station_id: i64,
    old_station_version_id: i64,
    new_latitude: f64,
    new_longitude: f64,
}

impl From<&rm_db::StationMovePlan> for StationMovePlanData {
    fn from(p: &rm_db::StationMovePlan) -> Self {
        Self {
            station_id: p.station_id,
            station_version_id: p.station_version_id,
            station_name: p.station_name.clone(),
            old_latitude: p.old_latitude,
            old_longitude: p.old_longitude,
            new_latitude: p.new_latitude,
            new_longitude: p.new_longitude,
            op: StationMoveOpData {
                station_id: p.op.station_id,
                old_station_version_id: p.op.old_station_version_id,
                new_latitude: p.op.new_latitude,
                new_longitude: p.op.new_longitude,
            },
        }
    }
}

/// POST /api/edit/station/{station_version_id}/move — move the station's
/// own coords. The default (and `apply: true`) performs the write;
/// `apply: false` answers the read-only plan (`plan_move_station`). Poles
/// stay where they are; nothing derives from station coords, so no stale
/// marks fire.
pub async fn move_station(
    Path(station_version_id): Path<i64>,
    State(state): State<AppState>,
    Editor(editor): Editor,
    ApiJson(b): ApiJson<StationMoveBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if !(-90.0..=90.0).contains(&b.lat) {
        return Err(ApiError::bad_request("lat out of range"));
    }
    if !(-180.0..=180.0).contains(&b.lon) {
        return Err(ApiError::bad_request("lon out of range"));
    }
    if b.apply == Some(false) {
        let cmd = rm_db::StationMove {
            station_version_id,
            latitude: b.lat,
            longitude: b.lon,
        };
        let plan = run_db(state, move |conn| rm_db::plan_move_station(conn, cmd))
            .await
            .map_err(edit_conflict)?;
        return Ok(Json(serde_json::to_value(StationMovePlanData::from(
            &plan,
        ))?));
    }
    let out = run_db_write(state, move |conn| {
        rm_db::move_station(
            conn,
            rm_db::StationMove {
                station_version_id,
                latitude: b.lat,
                longitude: b.lon,
            },
            &editor,
            b.note.as_deref().unwrap_or(""),
        )
    })
    .await
    .map_err(edit_conflict)?;
    Ok(Json(json!({
        "edit_id": out.edit_id,
        "new_station_version_id": out.new_station_version_id,
    })))
}

#[derive(Deserialize)]
pub struct PolylineBody {
    points: Vec<[f64; 2]>,
    expected_updated_at: Option<String>,
    note: Option<String>,
    apply: Option<bool>,
}

/// Serialize mirror of rm_db's `LineEditPlan` (non-`Serialize` by design) —
/// field names verbatim, nested `op` included (`replaces` stays the
/// static "override"/"src" tag).
#[derive(Serialize)]
pub struct LineEditPlanData {
    route_id: i64,
    route_version_id: Option<i64>,
    xno: String,
    direction: i64,
    replaces: &'static str,
    created: bool,
    points: usize,
    encoded: String,
    expected_updated_at: Option<String>,
    op: LineEditOpData,
}

#[derive(Serialize)]
pub struct LineEditOpData {
    xno: String,
    direction: i64,
    encoded: String,
    expected_updated_at: Option<String>,
}

impl From<&rm_db::LineEditPlan> for LineEditPlanData {
    fn from(p: &rm_db::LineEditPlan) -> Self {
        Self {
            route_id: p.route_id,
            route_version_id: p.route_version_id,
            xno: p.xno.clone(),
            direction: p.direction,
            replaces: p.replaces,
            created: p.created,
            points: p.points,
            encoded: p.encoded.clone(),
            expected_updated_at: p.expected_updated_at.clone(),
            op: LineEditOpData {
                xno: p.op.xno.clone(),
                direction: p.op.direction,
                encoded: p.op.encoded.clone(),
                expected_updated_at: p.op.expected_updated_at.clone(),
            },
        }
    }
}

fn coord_ok(pt: [f64; 2]) -> bool {
    (-90.0..=90.0).contains(&pt[0]) && (-180.0..=180.0).contains(&pt[1])
}

/// POST /api/edit/polyline/{xno}/{direction}/line — store a whole-line
/// override (effective = override ?? src). The default (and `apply: true`)
/// performs the write; `apply: false` answers the read-only plan
/// (`plan_polyline_line`). Server encodes via rm-core; the client sends
/// plain coordinates.
pub async fn edit_polyline_line(
    Path((xno, direction)): Path<(String, i64)>,
    State(state): State<AppState>,
    Editor(editor): Editor,
    ApiJson(b): ApiJson<PolylineBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if b.points.iter().any(|p| !coord_ok(*p)) {
        return Err(ApiError::bad_request("coordinate out of range"));
    }
    if b.apply == Some(false) {
        let cmd = rm_db::LineEditCmd {
            xno,
            direction,
            points: b.points,
            expected_updated_at: b.expected_updated_at,
        };
        let plan = run_db(state, move |conn| rm_db::plan_polyline_line(conn, cmd))
            .await
            .map_err(edit_conflict)?;
        return Ok(Json(serde_json::to_value(LineEditPlanData::from(&plan))?));
    }
    let out = run_db_write(state, move |conn| {
        rm_db::edit_polyline_line(
            conn,
            rm_db::LineEditCmd {
                xno,
                direction,
                points: b.points,
                expected_updated_at: b.expected_updated_at,
            },
            &editor,
            b.note.as_deref().unwrap_or(""),
        )
    })
    .await
    .map_err(edit_conflict)?;
    Ok(Json(json!({
        "edit_id": out.edit_id,
        "updated_at": out.updated_at,
        "created": out.created,
    })))
}
