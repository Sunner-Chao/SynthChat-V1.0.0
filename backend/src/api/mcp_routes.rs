use axum::{
    Extension, Json, Router,
    extract::{OriginalUri, Path, State, rejection::JsonRejection},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{CACHE_CONTROL, ETAG},
    },
    response::{IntoResponse, Response},
    routing::get,
};

use crate::mcp::{CreateMcpServer, McpError, McpServerPatch};

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
            "/api/v1/profiles/{profile_id}/mcp/servers",
            get(list_servers).post(create_server),
        )
        .route(
            "/api/v1/profiles/{profile_id}/mcp/servers/{server_id}",
            axum::routing::patch(update_server).delete(delete_server),
        )
}

async fn list_servers(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(profile_id): Path<String>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    let service = state.mcp.clone();
    let result = run_blocking(context, move || service.list_servers(&profile_id)).await?;
    Ok(no_store(versioned_json(StatusCode::OK, result)))
}

async fn create_server(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(profile_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<CreateMcpServer>, JsonRejection>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    require_content_type(&context, &headers, "application/json")?;
    let idempotency_key = parse_idempotency_key(&context, &headers)?;
    let request = parse_json(&context, payload)?;
    let service = state.mcp.clone();
    let result = run_blocking(context, move || {
        service.create_server(&profile_id, &request, &idempotency_key)
    })
    .await?;
    Ok(no_store(versioned_json(StatusCode::CREATED, result)))
}

async fn update_server(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path((profile_id, server_id)): Path<(String, String)>,
    headers: HeaderMap,
    payload: Result<Json<McpServerPatch>, JsonRejection>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    require_content_type(&context, &headers, "application/merge-patch+json")?;
    let etag = parse_if_match(&context, &headers)?;
    let patch = parse_json(&context, payload)?;
    let service = state.mcp.clone();
    let result = run_blocking(context, move || {
        service.update_server(&profile_id, &server_id, &etag, &patch)
    })
    .await?;
    Ok(no_store(versioned_json(StatusCode::OK, result)))
}

async fn delete_server(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path((profile_id, server_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    let etag = parse_if_match(&context, &headers)?;
    let service = state.mcp.clone();
    let result = run_blocking(context, move || {
        service.delete_server(&profile_id, &server_id, &etag)
    })
    .await?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        ETAG,
        HeaderValue::from_str(&result.etag).expect("profile revisions are valid HTTP ETags"),
    );
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn run_blocking<T, F>(context: RequestContext, operation: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, McpError> + Send + 'static,
{
    match tokio::task::spawn_blocking(operation).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(ApiError::from_mcp(context, error)),
        Err(error) => {
            tracing::error!(error = ?error, "MCP configuration blocking task failed");
            Err(ApiError::blocking_task_failed(context))
        }
    }
}
