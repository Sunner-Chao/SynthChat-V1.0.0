use axum::{
    Extension, Json, Router,
    extract::{
        OriginalUri, Path, Query, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::{HeaderMap, HeaderValue, StatusCode, header::ETAG},
    response::{IntoResponse, Response},
    routing::{get, patch},
};
use serde::Deserialize;

use crate::memory::{CreateMemory, ListMemories, MemoryError, MemoryPatch, MemoryTarget};

use super::{
    AppState, RequestId,
    error::{ApiError, RequestContext},
    profile_routes::{
        parse_idempotency_key, parse_if_match, parse_json, require_content_type, versioned_json,
    },
};

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/profiles/{profile_id}/memories",
            get(list_memories).post(create_memory),
        )
        .route(
            "/api/v1/profiles/{profile_id}/memories/{memory_id}",
            patch(update_memory).delete(delete_memory),
        )
}

async fn list_memories(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(profile_id): Path<String>,
    query: Result<Query<MemoryListQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    let Query(query) = query.map_err(|_| invalid_query(&context))?;
    let request = ListMemories {
        target: query.target,
        q: query.q,
        cursor: query.cursor,
        limit: query.limit,
    };
    let service = state.memory.clone();
    let result = run_blocking(context, move || service.list(&profile_id, &request)).await?;
    Ok(versioned_json(StatusCode::OK, result))
}

async fn create_memory(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(profile_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<CreateMemory>, JsonRejection>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    require_content_type(&context, &headers, "application/json")?;
    let idempotency_key = parse_idempotency_key(&context, &headers)?;
    let expected_etag = parse_if_match(&context, &headers)?;
    let request = parse_json(&context, payload)?;
    let service = state.memory.clone();
    let result = run_blocking(context, move || {
        service.create(&profile_id, &request, &idempotency_key, &expected_etag)
    })
    .await?;
    Ok(versioned_json(StatusCode::CREATED, result))
}

async fn update_memory(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path((profile_id, memory_id)): Path<(String, String)>,
    headers: HeaderMap,
    payload: Result<Json<MemoryPatch>, JsonRejection>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    require_content_type(&context, &headers, "application/merge-patch+json")?;
    let expected_etag = parse_if_match(&context, &headers)?;
    let request = parse_json(&context, payload)?;
    let service = state.memory.clone();
    let result = run_blocking(context, move || {
        service.update(&profile_id, &memory_id, &request, &expected_etag)
    })
    .await?;
    Ok(versioned_json(StatusCode::OK, result))
}

async fn delete_memory(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path((profile_id, memory_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    let expected_etag = parse_if_match(&context, &headers)?;
    let service = state.memory.clone();
    let result = run_blocking(context, move || {
        service.delete(&profile_id, &memory_id, &expected_etag)
    })
    .await?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        ETAG,
        HeaderValue::from_str(&result.etag).expect("memory revisions are valid HTTP ETags"),
    );
    Ok(response)
}

async fn run_blocking<T, F>(context: RequestContext, operation: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, MemoryError> + Send + 'static,
{
    match tokio::task::spawn_blocking(operation).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(ApiError::from_memory(context, error)),
        Err(error) => {
            tracing::error!(error = ?error, "memory blocking task failed");
            Err(ApiError::blocking_task_failed(context))
        }
    }
}

fn invalid_query(context: &RequestContext) -> ApiError {
    ApiError::new(
        context.clone(),
        StatusCode::BAD_REQUEST,
        "Invalid query",
        "validation_failed",
        "The Memory query parameters do not match the API contract.",
        false,
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryListQuery {
    target: MemoryTarget,
    #[serde(rename = "q")]
    q: Option<String>,
    cursor: Option<String>,
    limit: Option<usize>,
}
