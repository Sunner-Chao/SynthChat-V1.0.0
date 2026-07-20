use axum::{
    Extension, Json, Router,
    extract::{OriginalUri, Path, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get},
};
use serde::Deserialize;

use super::{
    AppState, RequestId,
    error::{ApiError, RequestContext},
    profile_routes::{parse_idempotency_key, parse_json, require_content_type},
};

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RegisterWorkspace {
    path: String,
}

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/profiles/{profile_id}/workspaces",
            get(list_workspaces).post(register_workspace),
        )
        .route(
            "/api/v1/profiles/{profile_id}/workspaces/{workspace_id}",
            delete(delete_workspace),
        )
}

async fn list_workspaces(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(profile_id): Path<String>,
) -> Result<Json<Vec<crate::sessions::Workspace>>, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    state
        .profiles
        .get_config(&profile_id)
        .map_err(|error| ApiError::from_profile(context.clone(), error))?;
    let sessions = state.sessions.clone();
    tokio::task::spawn_blocking(move || sessions.list_workspaces(&profile_id))
        .await
        .map_err(|_| {
            ApiError::from_session(
                context.clone(),
                crate::sessions::SessionError::StorageUnavailable,
            )
        })?
        .map(Json)
        .map_err(|error| ApiError::from_session(context, error))
}

async fn register_workspace(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(profile_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<RegisterWorkspace>, JsonRejection>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    require_content_type(&context, &headers, "application/json")?;
    let idempotency_key = parse_idempotency_key(&context, &headers)?;
    let request = parse_json(&context, payload)?;
    state
        .profiles
        .get_config(&profile_id)
        .map_err(|error| ApiError::from_profile(context.clone(), error))?;
    let sessions = state.sessions.clone();
    let workspace = tokio::task::spawn_blocking(move || {
        sessions.register_workspace(&profile_id, &request.path, &idempotency_key)
    })
    .await
    .map_err(|_| {
        ApiError::from_session(
            context.clone(),
            crate::sessions::SessionError::StorageUnavailable,
        )
    })?
    .map_err(|error| ApiError::from_session(context, error))?;
    Ok((StatusCode::CREATED, Json(workspace)).into_response())
}

async fn delete_workspace(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path((profile_id, workspace_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    state
        .profiles
        .get_config(&profile_id)
        .map_err(|error| ApiError::from_profile(context.clone(), error))?;
    let sessions = state.sessions.clone();
    tokio::task::spawn_blocking(move || sessions.delete_workspace(&profile_id, &workspace_id))
        .await
        .map_err(|_| {
            ApiError::from_session(
                context.clone(),
                crate::sessions::SessionError::StorageUnavailable,
            )
        })?
        .map_err(|error| ApiError::from_session(context, error))?;
    Ok(StatusCode::NO_CONTENT)
}
