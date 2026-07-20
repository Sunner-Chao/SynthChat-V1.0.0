use axum::{
    Extension, Json, Router,
    extract::{OriginalUri, Path, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::get,
};
use serde::Deserialize;

use crate::tools::{ToolsetError, list_toolsets, update_toolset};

use super::{
    AppState, RequestId,
    error::{ApiError, RequestContext},
    profile_routes::{parse_if_match, parse_json, require_content_type, versioned_json},
};

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/profiles/{profile_id}/toolsets",
            get(list_profile_toolsets),
        )
        .route(
            "/api/v1/profiles/{profile_id}/toolsets/{toolset_id}",
            axum::routing::patch(update_profile_toolset),
        )
}

async fn list_profile_toolsets(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(profile_id): Path<String>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    let profiles = state.profiles.clone();
    let result = run_blocking(context, move || list_toolsets(&profiles, &profile_id)).await?;
    Ok(versioned_json(StatusCode::OK, result))
}

async fn update_profile_toolset(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path((profile_id, toolset_id)): Path<(String, String)>,
    headers: HeaderMap,
    payload: Result<Json<ToolsetPatch>, JsonRejection>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    require_content_type(&context, &headers, "application/merge-patch+json")?;
    let expected_etag = parse_if_match(&context, &headers)?;
    let patch = parse_json(&context, payload)?;
    let profiles = state.profiles.clone();
    let result = run_blocking(context, move || {
        update_toolset(
            &profiles,
            &profile_id,
            &toolset_id,
            patch.enabled,
            &expected_etag,
        )
    })
    .await?;
    Ok(versioned_json(StatusCode::OK, result))
}

async fn run_blocking<T, F>(context: RequestContext, operation: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, ToolsetError> + Send + 'static,
{
    match tokio::task::spawn_blocking(operation).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(ApiError::from_toolset(context, error)),
        Err(error) => {
            tracing::error!(error = ?error, "toolset blocking task failed");
            Err(ApiError::blocking_task_failed(context))
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolsetPatch {
    enabled: bool,
}
