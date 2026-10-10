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

/// `rm_edit::EditError` splits into request problems (400: empty/no-op
/// edit) and races / bad targets (409: version no longer current,
/// un-undoable edit, unknown regroup target).
fn edit_conflict(err: ApiError) -> ApiError {
    if let Some(e) = err.downcast_ref::<rm_edit::EditError>() {
        return match e {
            rm_edit::EditError::EmptyEdit | rm_edit::EditError::NoOpEdit
            | rm_edit::EditError::EmptyNewName | rm_edit::EditError::NoOpRename => {
                ApiError::bad_request(&e.to_string())
            }
            rm_edit::EditError::NotCurrent(_)
            | rm_edit::EditError::StationNotCurrent(_)
            | rm_edit::EditError::NotUndoable(_)
            | rm_edit::EditError::UnknownStation(_) => {
                ApiError::conflict(&e.to_string())
            }
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
        rm_edit::edit_stop(
            conn,
            stop_version_id,
            rm_edit::StopEdit { lat, lon, stop_name },
            &editor,
            &note,
        )
    })
    .await
    .map_err(edit_conflict)?;
    Ok(Json(json!({
        "edit_id": out.edit_id,
        "new_stop_version_id": out.new_stop_version_id,
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
        rm_edit::regroup_stop(
            conn,
            rm_edit::RegroupStop {
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
        "new_stop_version_id": out.new_stop_version_id,
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
/// that preceded it. Stop rows restore the stop version; station rows
/// (rename/move) restore the station version. Unknown ids route down the
/// stop path and surface as its 409 NotUndoable, as before.
pub async fn undo(
    Path(edit_id): Path<i64>,
    State(state): State<AppState>,
    Editor(editor): Editor,
    ApiJson(b): ApiJson<UndoBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let is_station = run_db(state.clone(), move |conn| {
        rm_db::edit_entity(conn, edit_id)
    })
    .await?
    .as_deref()
        == Some("station");
    let out = run_db_write(state, move |conn| {
        if is_station {
            rm_edit::undo_station(conn, edit_id, &editor, b.note.as_deref().unwrap_or(""))
                .map(|o| (o.edit_id, o.restored_station_version_id))
        } else {
            rm_edit::undo_stop(conn, edit_id, &editor, b.note.as_deref().unwrap_or(""))
                .map(|o| (o.edit_id, o.new_stop_version_id))
        }
    })
    .await
    .map_err(edit_conflict)?;
    if is_station {
        Ok(Json(json!({
            "edit_id": out.0,
            "restored_station_version_id": out.1,
        })))
    } else {
        Ok(Json(json!({
            "edit_id": out.0,
            "new_stop_version_id": out.1,
        })))
    }
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
        "station" => rm_edit::RenameLevel::StationOnly,
        "matching" => rm_edit::RenameLevel::MatchingPoles,
        "all" => rm_edit::RenameLevel::AllPoles,
        other => return Err(ApiError::bad_request(&format!("unknown level {other:?}"))),
    };
    let out = run_db_write(state, move |conn| {
        rm_edit::rename_station(
            conn,
            rm_edit::StationRename {
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
        rm_edit::move_station(
            conn,
            rm_edit::StationMove {
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
