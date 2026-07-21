use axum::{
    Extension, Json, Router,
    extract::{OriginalUri, Path, State, rejection::JsonRejection},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{CACHE_CONTROL, ETAG},
    },
    response::{IntoResponse, Response},
    routing::{get, patch, post},
};

use crate::plugins::{InstallPlugin, PluginError, PluginPatch};

use super::{
    AppState, RequestId,
    error::{ApiError, RequestContext},
    profile_routes::{parse_if_match, parse_json, require_content_type},
};

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/plugins", get(list_plugins))
        .route("/api/v1/plugins/install", post(install_plugin))
        .route(
            "/api/v1/plugins/{plugin_id}",
            patch(update_plugin).delete(uninstall_plugin),
        )
}

async fn list_plugins(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    let service = state.plugins.clone();
    let result = blocking(context.clone(), move || service.list()).await?;
    Ok(no_store(etag_json(
        StatusCode::OK,
        result.value,
        result.revision,
    )))
}

async fn install_plugin(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    payload: Result<Json<InstallPlugin>, JsonRejection>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    require_content_type(&context, &headers, "application/json")?;
    let request = parse_json(&context, payload)?;
    let service = state.plugins.clone();
    let result = blocking(context.clone(), move || service.install(&request)).await?;
    Ok(no_store(etag_json(
        StatusCode::CREATED,
        result.value,
        result.revision,
    )))
}

async fn update_plugin(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(plugin_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<PluginPatch>, JsonRejection>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    require_content_type(&context, &headers, "application/merge-patch+json")?;
    let revision = parse_plugin_etag(&context, &parse_if_match(&context, &headers)?)?;
    let request = parse_json(&context, payload)?;
    let service = state.plugins.clone();
    let result = blocking(context.clone(), move || {
        service.update(&plugin_id, &request, revision)
    })
    .await?;
    Ok(no_store(etag_json(
        StatusCode::OK,
        result.value,
        result.revision,
    )))
}

async fn uninstall_plugin(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(plugin_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    let revision = parse_plugin_etag(&context, &parse_if_match(&context, &headers)?)?;
    let service = state.plugins.clone();
    let next_revision = blocking(context.clone(), move || {
        service.uninstall(&plugin_id, revision)
    })
    .await?;
    Ok(no_store(etag_empty(StatusCode::NO_CONTENT, next_revision)))
}

async fn blocking<T, F>(context: RequestContext, operation: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, PluginError> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|_| ApiError::blocking_task_failed(context.clone()))?
        .map_err(|error| map_error(context, error))
}

fn map_error(context: RequestContext, error: PluginError) -> ApiError {
    match error {
        PluginError::InvalidRequest => ApiError::new(
            context,
            StatusCode::BAD_REQUEST,
            "Invalid plugin request",
            "plugin_validation_failed",
            "The plugin request does not match the manifest-only catalog contract.",
            false,
        ),
        PluginError::NotFound => ApiError::new(
            context,
            StatusCode::NOT_FOUND,
            "Plugin not found",
            "plugin_not_found",
            "The requested local plugin registration does not exist.",
            false,
        ),
        PluginError::AlreadyInstalled => ApiError::new(
            context,
            StatusCode::CONFLICT,
            "Plugin already installed",
            "plugin_already_installed",
            "This local plugin directory is already registered.",
            false,
        ),
        PluginError::RevisionConflict { .. } => ApiError::new(
            context,
            StatusCode::CONFLICT,
            "Plugin catalog revision conflict",
            "revision_conflict",
            "The plugin catalog changed since it was read; refresh before updating.",
            false,
        ),
        PluginError::ManifestInvalid => ApiError::new(
            context,
            StatusCode::UNPROCESSABLE_ENTITY,
            "Plugin manifest is invalid",
            "plugin_manifest_invalid",
            "plugin.json must be a bounded, non-symlinked manifest with only supported fields.",
            false,
        ),
        PluginError::LimitReached => ApiError::new(
            context,
            StatusCode::PAYLOAD_TOO_LARGE,
            "Plugin catalog limit reached",
            "plugin_catalog_limit",
            "The local plugin catalog has reached its bounded limit.",
            false,
        ),
        PluginError::StorageUnavailable => ApiError::new(
            context,
            StatusCode::SERVICE_UNAVAILABLE,
            "Plugin catalog unavailable",
            "plugin_catalog_unavailable",
            "The local plugin catalog could not complete the operation.",
            true,
        ),
    }
}

fn parse_plugin_etag(context: &RequestContext, value: &str) -> Result<u64, ApiError> {
    let inner = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or_default();
    let Some(revision) = inner.strip_prefix("plugin-catalog-") else {
        return Err(invalid_plugin_etag(context));
    };
    let revision = revision
        .parse::<u64>()
        .map_err(|_| invalid_plugin_etag(context))?;
    Ok(revision)
}

fn invalid_plugin_etag(context: &RequestContext) -> ApiError {
    ApiError::new(
        context.clone(),
        StatusCode::BAD_REQUEST,
        "Invalid plugin revision",
        "invalid_if_match",
        "If-Match must identify a plugin-catalog revision.",
        false,
    )
}

fn etag_json<T: serde::Serialize>(status: StatusCode, value: T, revision: u64) -> Response {
    let mut response = (status, Json(value)).into_response();
    response.headers_mut().insert(
        ETAG,
        HeaderValue::from_str(&format!("\"plugin-catalog-{revision}\""))
            .expect("plugin catalog ETags are valid"),
    );
    response
}

fn etag_empty(status: StatusCode, revision: u64) -> Response {
    let mut response = status.into_response();
    response.headers_mut().insert(
        ETAG,
        HeaderValue::from_str(&format!("\"plugin-catalog-{revision}\""))
            .expect("plugin catalog ETags are valid"),
    );
    response
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
