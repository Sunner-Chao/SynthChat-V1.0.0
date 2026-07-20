use axum::{
    Extension, Json, Router,
    extract::{OriginalUri, Path, State},
    http::{HeaderValue, StatusCode, header::CACHE_CONTROL},
    response::{IntoResponse, Response},
    routing::get,
};

use crate::skills::SkillError;

use super::{
    AppState, RequestId,
    error::{ApiError, RequestContext},
};

pub(super) fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/operations/{operation_id}", get(get_operation))
}

async fn get_operation(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(operation_id): Path<String>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    let service = state.skills.clone();
    let operation = run_blocking(context, move || service.operation(&operation_id)).await?;
    let mut response = (StatusCode::OK, Json(operation)).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

async fn run_blocking<T, F>(context: RequestContext, operation: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, SkillError> + Send + 'static,
{
    match tokio::task::spawn_blocking(operation).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(ApiError::from_skill(context, error)),
        Err(error) => {
            tracing::error!(error = ?error, "operation lookup task failed");
            Err(ApiError::blocking_task_failed(context))
        }
    }
}
