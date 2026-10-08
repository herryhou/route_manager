//! Edit endpoints — mounted only when --edit passes tokens.

use axum::extract::{FromRef, FromRequest, FromRequestParts, Path, Request, State};
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::Json;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::json;

use crate::error::ApiError;
use crate::state::{run_db_write, AppState};

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
        let editor = tokens
            .get(hdr)
            .ok_or_else(|| ApiError::forbidden("unknown token"))?;
        Ok(Editor(editor.clone()))
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
    let edit = rm_db::StopEdit {
        lat,
        lon,
        stop_name,
    };
    let note = note.unwrap_or_default();
    let out = run_db_write(state, move |conn| {
        rm_db::edit_stop(conn, stop_version_id, &edit, &editor, &note)
    })
    .await?;
    Ok(Json(json!({
        "edit_id": out.edit_id,
        "new_stop_version_id": out.new_version_id,
        "stale_routes": out.stale_routes,
    })))
}
