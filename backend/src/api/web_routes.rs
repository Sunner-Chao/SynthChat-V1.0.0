use axum::{
    Extension, Json, Router,
    extract::{OriginalUri, Path, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::get,
};

use crate::profiles::{ProfileError, WebConfigPatch};

use super::{
    AppState, RequestId,
    error::{ApiError, RequestContext},
    profile_routes::{parse_if_match, parse_json, require_content_type, versioned_json},
};

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/web/providers", get(list_web_providers))
        .route(
            "/api/v1/profiles/{profile_id}/web",
            get(get_web_config).patch(update_web_config),
        )
}

async fn list_web_providers(
    State(state): State<AppState>,
) -> Json<Vec<crate::profiles::WebProvider>> {
    Json(state.web.providers())
}

async fn get_web_config(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(profile_id): Path<String>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    let profiles = state.profiles.clone();
    let result = run_blocking(context, move || profiles.get_web_config(&profile_id)).await?;
    Ok(versioned_json(StatusCode::OK, result))
}

async fn update_web_config(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(profile_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<WebConfigPatch>, JsonRejection>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    require_content_type(&context, &headers, "application/merge-patch+json")?;
    let expected_etag = parse_if_match(&context, &headers)?;
    let patch = parse_json(&context, payload)?;
    let profiles = state.profiles.clone();
    let result = run_blocking(context, move || {
        profiles.update_web_config(&profile_id, &expected_etag, &patch)
    })
    .await?;
    Ok(versioned_json(StatusCode::OK, result))
}

async fn run_blocking<T, F>(context: RequestContext, operation: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, ProfileError> + Send + 'static,
{
    match tokio::task::spawn_blocking(operation).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(ProfileError::InvalidProfileConfig)) => Err(ApiError::new(
            context,
            StatusCode::BAD_REQUEST,
            "Invalid Web configuration",
            "validation_failed",
            "The Web configuration does not match the API contract.",
            false,
        )),
        Ok(Err(error)) => Err(ApiError::from_profile(context, error)),
        Err(error) => {
            tracing::error!(error = ?error, "Web configuration blocking task failed");
            Err(ApiError::blocking_task_failed(context))
        }
    }
}
