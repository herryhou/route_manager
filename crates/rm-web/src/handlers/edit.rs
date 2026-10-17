//! Edit endpoints — mounted only when --edit passes tokens.

use axum::extract::{FromRef, FromRequest, FromRequestParts, Path, Query, Request, State};
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::Json;
use serde::de::DeserializeOwned;
use serde::Deserialize;
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
            | rm_db::EditError::DuplicateAttach(_, _) => ApiError::bad_request(&e.to_string()),
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
}

/// POST /api/edit/stop/{stop_version_id}/regroup — move a stop to another
/// station. The regroup primitive writes `action='move_station'`.
pub async fn regroup(
    Path(stop_version_id): Path<i64>,
    State(state): State<AppState>,
    Editor(editor): Editor,
    ApiJson(b): ApiJson<RegroupBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
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
/// that preceded it. rm_db's dispatcher picks the kind's undo service and
/// returns one tagged outcome (`kind`: stop/station/polyline) either way;
/// unknown ids route down the stop arm and surface as its 409 NotUndoable,
/// as before.
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
}

/// POST /api/edit/station/{station_version_id} — rename the station, with
/// an editor-chosen level for how far the rename reaches into pole names.
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
}

/// POST /api/edit/station/{station_version_id}/move — move the station's
/// own coords. Poles stay where they are; nothing derives from station
/// coords, so no stale marks fire.
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
}

fn coord_ok(pt: [f64; 2]) -> bool {
    (-90.0..=90.0).contains(&pt[0]) && (-180.0..=180.0).contains(&pt[1])
}

/// POST /api/edit/polyline/{xno}/{direction}/line — store a whole-line
/// override (effective = override ?? src). Server encodes via rm-core; the
/// client sends plain coordinates.
pub async fn edit_polyline_line(
    Path((xno, direction)): Path<(String, i64)>,
    State(state): State<AppState>,
    Editor(editor): Editor,
    ApiJson(b): ApiJson<PolylineBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if b.points.iter().any(|p| !coord_ok(*p)) {
        return Err(ApiError::bad_request("coordinate out of range"));
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
