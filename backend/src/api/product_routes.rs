use axum::{
    Extension, Json, Router,
    extract::{OriginalUri, Path, Query, State, rejection::JsonRejection},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{CACHE_CONTROL, ETAG},
    },
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
};
use serde::Deserialize;

use crate::product_catalog::{
    MomentCommentInput, MomentInput, MomentLikeInput, PersonaInput, ProductCatalogError,
    WorldbookInput,
};

use super::{
    AppState, RequestId,
    error::{ApiError, RequestContext},
    profile_routes::{parse_if_match, parse_json, require_content_type},
};

#[derive(Debug, Deserialize)]
pub(super) struct CatalogQuery {
    pub q: Option<String>,
}

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/profiles/{profile_id}/personas",
            get(list_personas).post(create_persona),
        )
        .route(
            "/api/v1/profiles/{profile_id}/personas/{persona_id}",
            get(get_persona)
                .patch(update_persona)
                .delete(delete_persona),
        )
        .route(
            "/api/v1/profiles/{profile_id}/worldbooks",
            get(list_worldbooks).post(create_worldbook),
        )
        .route(
            "/api/v1/profiles/{profile_id}/worldbooks/{worldbook_id}",
            get(get_worldbook)
                .patch(update_worldbook)
                .delete(delete_worldbook),
        )
        .route(
            "/api/v1/profiles/{profile_id}/moments",
            get(list_moments).post(create_moment),
        )
        .route(
            "/api/v1/profiles/{profile_id}/moments/{moment_id}",
            get(get_moment).patch(update_moment).delete(delete_moment),
        )
        .route(
            "/api/v1/profiles/{profile_id}/moments/{moment_id}/comments",
            post(add_comment),
        )
        .route(
            "/api/v1/profiles/{profile_id}/moments/{moment_id}/comments/{comment_id}",
            delete(delete_comment),
        )
        .route(
            "/api/v1/profiles/{profile_id}/moments/{moment_id}/like",
            put(set_like),
        )
}

async fn list_personas(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(profile_id): Path<String>,
    Query(query): Query<CatalogQuery>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    ensure_profile(&state, &profile_id, context.clone())?;
    let service = state.product_catalog.clone();
    let result = blocking(context.clone(), move || {
        service.list_personas(&profile_id, query.q.as_deref())
    })
    .await?;
    Ok(no_store(Json(result).into_response()))
}

async fn get_persona(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path((profile_id, persona_id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    ensure_profile(&state, &profile_id, context.clone())?;
    let service = state.product_catalog.clone();
    let result = blocking(context.clone(), move || {
        service.get_persona(&profile_id, &persona_id)
    })
    .await?;
    let revision = result.revision;
    Ok(no_store(etag_json(
        StatusCode::OK,
        result,
        "persona",
        revision,
    )))
}

async fn create_persona(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(profile_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<PersonaInput>, JsonRejection>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    ensure_profile(&state, &profile_id, context.clone())?;
    require_content_type(&context, &headers, "application/json")?;
    let request = parse_json(&context, payload)?;
    let service = state.product_catalog.clone();
    let result = blocking(context.clone(), move || {
        service.create_persona(&profile_id, &request)
    })
    .await?;
    Ok(no_store(etag_json(
        StatusCode::CREATED,
        result,
        "persona",
        1,
    )))
}

async fn update_persona(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path((profile_id, persona_id)): Path<(String, String)>,
    headers: HeaderMap,
    payload: Result<Json<PersonaInput>, JsonRejection>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    ensure_profile(&state, &profile_id, context.clone())?;
    require_content_type(&context, &headers, "application/json")?;
    let revision = parse_product_etag(&context, &parse_if_match(&context, &headers)?, "persona")?;
    let request = parse_json(&context, payload)?;
    let service = state.product_catalog.clone();
    let result = blocking(context.clone(), move || {
        service.update_persona(&profile_id, &persona_id, revision, &request)
    })
    .await?;
    Ok(no_store(etag_json(
        StatusCode::OK,
        result,
        "persona",
        revision + 1,
    )))
}

async fn delete_persona(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path((profile_id, persona_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    ensure_profile(&state, &profile_id, context.clone())?;
    let revision = parse_product_etag(&context, &parse_if_match(&context, &headers)?, "persona")?;
    let service = state.product_catalog.clone();
    blocking(context, move || {
        service.delete_persona(&profile_id, &persona_id, revision)
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_worldbooks(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(profile_id): Path<String>,
    Query(query): Query<CatalogQuery>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    ensure_profile(&state, &profile_id, context.clone())?;
    let service = state.product_catalog.clone();
    let result = blocking(context.clone(), move || {
        service.list_worldbooks(&profile_id, query.q.as_deref())
    })
    .await?;
    Ok(no_store(Json(result).into_response()))
}

async fn get_worldbook(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path((profile_id, worldbook_id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    ensure_profile(&state, &profile_id, context.clone())?;
    let service = state.product_catalog.clone();
    let result = blocking(context.clone(), move || {
        service.get_worldbook(&profile_id, &worldbook_id)
    })
    .await?;
    let revision = result.revision;
    Ok(no_store(etag_json(
        StatusCode::OK,
        result,
        "worldbook",
        revision,
    )))
}

async fn create_worldbook(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(profile_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<WorldbookInput>, JsonRejection>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    ensure_profile(&state, &profile_id, context.clone())?;
    require_content_type(&context, &headers, "application/json")?;
    let request = parse_json(&context, payload)?;
    let service = state.product_catalog.clone();
    let result = blocking(context.clone(), move || {
        service.create_worldbook(&profile_id, &request)
    })
    .await?;
    Ok(no_store(etag_json(
        StatusCode::CREATED,
        result,
        "worldbook",
        1,
    )))
}

async fn update_worldbook(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path((profile_id, worldbook_id)): Path<(String, String)>,
    headers: HeaderMap,
    payload: Result<Json<WorldbookInput>, JsonRejection>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    ensure_profile(&state, &profile_id, context.clone())?;
    require_content_type(&context, &headers, "application/json")?;
    let revision = parse_product_etag(&context, &parse_if_match(&context, &headers)?, "worldbook")?;
    let request = parse_json(&context, payload)?;
    let service = state.product_catalog.clone();
    let result = blocking(context.clone(), move || {
        service.update_worldbook(&profile_id, &worldbook_id, revision, &request)
    })
    .await?;
    Ok(no_store(etag_json(
        StatusCode::OK,
        result,
        "worldbook",
        revision + 1,
    )))
}

async fn delete_worldbook(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path((profile_id, worldbook_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    ensure_profile(&state, &profile_id, context.clone())?;
    let revision = parse_product_etag(&context, &parse_if_match(&context, &headers)?, "worldbook")?;
    let service = state.product_catalog.clone();
    blocking(context, move || {
        service.delete_worldbook(&profile_id, &worldbook_id, revision)
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_moments(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(profile_id): Path<String>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    ensure_profile(&state, &profile_id, context.clone())?;
    let service = state.product_catalog.clone();
    let result = blocking(context.clone(), move || service.list_moments(&profile_id)).await?;
    Ok(no_store(Json(result).into_response()))
}

async fn get_moment(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path((profile_id, moment_id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    ensure_profile(&state, &profile_id, context.clone())?;
    let service = state.product_catalog.clone();
    let result = blocking(context.clone(), move || {
        service.get_moment(&profile_id, &moment_id)
    })
    .await?;
    let revision = result.revision;
    Ok(no_store(etag_json(
        StatusCode::OK,
        result,
        "moment",
        revision,
    )))
}

async fn create_moment(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(profile_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<MomentInput>, JsonRejection>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    ensure_profile(&state, &profile_id, context.clone())?;
    require_content_type(&context, &headers, "application/json")?;
    let request = parse_json(&context, payload)?;
    let service = state.product_catalog.clone();
    let result = blocking(context.clone(), move || {
        service.create_moment(&profile_id, &request)
    })
    .await?;
    Ok(no_store(etag_json(
        StatusCode::CREATED,
        result,
        "moment",
        1,
    )))
}

async fn update_moment(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path((profile_id, moment_id)): Path<(String, String)>,
    headers: HeaderMap,
    payload: Result<Json<MomentInput>, JsonRejection>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    ensure_profile(&state, &profile_id, context.clone())?;
    require_content_type(&context, &headers, "application/json")?;
    let revision = parse_product_etag(&context, &parse_if_match(&context, &headers)?, "moment")?;
    let request = parse_json(&context, payload)?;
    let service = state.product_catalog.clone();
    let result = blocking(context.clone(), move || {
        service.update_moment(&profile_id, &moment_id, revision, &request)
    })
    .await?;
    Ok(no_store(etag_json(
        StatusCode::OK,
        result,
        "moment",
        revision + 1,
    )))
}

async fn delete_moment(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path((profile_id, moment_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    ensure_profile(&state, &profile_id, context.clone())?;
    let revision = parse_product_etag(&context, &parse_if_match(&context, &headers)?, "moment")?;
    let service = state.product_catalog.clone();
    blocking(context, move || {
        service.delete_moment(&profile_id, &moment_id, revision)
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn add_comment(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path((profile_id, moment_id)): Path<(String, String)>,
    headers: HeaderMap,
    payload: Result<Json<MomentCommentInput>, JsonRejection>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    ensure_profile(&state, &profile_id, context.clone())?;
    require_content_type(&context, &headers, "application/json")?;
    let revision = parse_product_etag(&context, &parse_if_match(&context, &headers)?, "moment")?;
    let request = parse_json(&context, payload)?;
    let service = state.product_catalog.clone();
    let result = blocking(context.clone(), move || {
        service.add_moment_comment(&profile_id, &moment_id, revision, &request)
    })
    .await?;
    Ok(no_store(etag_json(
        StatusCode::OK,
        result,
        "moment",
        revision + 1,
    )))
}

async fn delete_comment(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path((profile_id, moment_id, comment_id)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    ensure_profile(&state, &profile_id, context.clone())?;
    let revision = parse_product_etag(&context, &parse_if_match(&context, &headers)?, "moment")?;
    let service = state.product_catalog.clone();
    let result = blocking(context.clone(), move || {
        service.delete_moment_comment(&profile_id, &moment_id, &comment_id, revision)
    })
    .await?;
    Ok(no_store(etag_json(
        StatusCode::OK,
        result,
        "moment",
        revision + 1,
    )))
}

async fn set_like(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path((profile_id, moment_id)): Path<(String, String)>,
    headers: HeaderMap,
    payload: Result<Json<MomentLikeInput>, JsonRejection>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    ensure_profile(&state, &profile_id, context.clone())?;
    require_content_type(&context, &headers, "application/json")?;
    let revision = parse_product_etag(&context, &parse_if_match(&context, &headers)?, "moment")?;
    let request = parse_json(&context, payload)?;
    let service = state.product_catalog.clone();
    let result = blocking(context.clone(), move || {
        service.set_moment_like(&profile_id, &moment_id, revision, &request)
    })
    .await?;
    Ok(no_store(etag_json(
        StatusCode::OK,
        result,
        "moment",
        revision + 1,
    )))
}

fn ensure_profile(
    state: &AppState,
    profile_id: &str,
    context: RequestContext,
) -> Result<(), ApiError> {
    state
        .profiles
        .get_config(profile_id)
        .map(|_| ())
        .map_err(|error| ApiError::from_profile(context, error))
}

async fn blocking<T, F>(context: RequestContext, operation: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, ProductCatalogError> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|_| ApiError::blocking_task_failed(context.clone()))?
        .map_err(|error| map_error(context, error))
}

fn map_error(context: RequestContext, error: ProductCatalogError) -> ApiError {
    match error {
        ProductCatalogError::InvalidRequest => ApiError::new(
            context,
            StatusCode::BAD_REQUEST,
            "Invalid product request",
            "validation_failed",
            "The product payload does not match the API contract.",
            false,
        ),
        ProductCatalogError::NotFound => ApiError::new(
            context,
            StatusCode::NOT_FOUND,
            "Product item not found",
            "product_not_found",
            "The requested product item does not exist.",
            false,
        ),
        ProductCatalogError::RevisionConflict { .. } => ApiError::new(
            context,
            StatusCode::CONFLICT,
            "Product revision conflict",
            "revision_conflict",
            "The product item changed since it was read; refresh before updating.",
            false,
        ),
        ProductCatalogError::LimitReached => ApiError::new(
            context,
            StatusCode::PAYLOAD_TOO_LARGE,
            "Product catalog limit reached",
            "product_catalog_limit",
            "The product catalog has reached its bounded item limit.",
            false,
        ),
        ProductCatalogError::StorageUnavailable => ApiError::new(
            context,
            StatusCode::SERVICE_UNAVAILABLE,
            "Product catalog unavailable",
            "product_catalog_unavailable",
            "The local product catalog could not complete the operation.",
            true,
        ),
    }
}

fn parse_product_etag(context: &RequestContext, value: &str, kind: &str) -> Result<u64, ApiError> {
    let inner = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or_default();
    let prefix = format!("product-{kind}-");
    let Some(number) = inner.strip_prefix(&prefix) else {
        return Err(ApiError::new(
            context.clone(),
            StatusCode::BAD_REQUEST,
            "Invalid product revision",
            "invalid_if_match",
            "If-Match does not identify the requested product revision.",
            false,
        ));
    };
    number.parse().map_err(|_| {
        ApiError::new(
            context.clone(),
            StatusCode::BAD_REQUEST,
            "Invalid product revision",
            "invalid_if_match",
            "If-Match does not identify the requested product revision.",
            false,
        )
    })
}

fn etag_json<T: serde::Serialize>(
    status: StatusCode,
    value: T,
    kind: &str,
    revision: u64,
) -> Response {
    let mut response = (status, Json(value)).into_response();
    response.headers_mut().insert(
        ETAG,
        HeaderValue::from_str(&format!("\"product-{kind}-{revision}\""))
            .expect("product revision ETags are valid"),
    );
    response
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
