use axum::{
    Extension, Json, Router,
    extract::{OriginalUri, Path, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderValue, StatusCode, header::CACHE_CONTROL},
    response::{IntoResponse, Response},
    routing::{get, patch, post},
};

use crate::wechat::{
    QrStartRequest, QrStatusRequest, WechatAccountLinkPatch, WechatConfigPatch, WechatError,
    WechatPollRequest, WechatSendRequest,
};

use super::{
    AppState, RequestId,
    error::{ApiError, RequestContext},
    profile_routes::{parse_if_match, parse_json, require_content_type, versioned_json},
};

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/profiles/{profile_id}/wechat",
            get(get_config).patch(update_config),
        )
        .route("/api/v1/profiles/{profile_id}/wechat/qr", post(start_qr))
        .route(
            "/api/v1/profiles/{profile_id}/wechat/qr/status",
            post(check_qr),
        )
        .route(
            "/api/v1/profiles/{profile_id}/wechat/accounts/{account_id}",
            patch(update_account_link),
        )
        .route(
            "/api/v1/profiles/{profile_id}/wechat/accounts/{account_id}/poll",
            post(poll_messages),
        )
        .route(
            "/api/v1/profiles/{profile_id}/wechat/accounts/{account_id}/messages",
            post(send_message),
        )
}

async fn get_config(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(profile_id): Path<String>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    let service = state.wechat.clone();
    let result = tokio::task::spawn_blocking(move || service.get_config(&profile_id))
        .await
        .map_err(|_| ApiError::blocking_task_failed(context.clone()))?
        .map_err(|error| map_error(context.clone(), error))?;
    Ok(no_store(versioned_json(StatusCode::OK, result)))
}

async fn update_config(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(profile_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<WechatConfigPatch>, JsonRejection>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    require_content_type(&context, &headers, "application/merge-patch+json")?;
    let etag = parse_if_match(&context, &headers)?;
    let patch = parse_json(&context, payload)?;
    let service = state.wechat.clone();
    let result =
        tokio::task::spawn_blocking(move || service.update_config(&profile_id, &etag, &patch))
            .await
            .map_err(|_| ApiError::blocking_task_failed(context.clone()))?
            .map_err(|error| map_error(context.clone(), error))?;
    Ok(no_store(versioned_json(StatusCode::OK, result)))
}

async fn start_qr(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(profile_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<QrStartRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    require_content_type(&context, &headers, "application/json")?;
    let request = parse_json(&context, payload)?;
    let service = state.wechat.clone();
    let result = service
        .start_qr(&profile_id, &request)
        .await
        .map_err(|error| map_error(context.clone(), error))?;
    Ok(no_store(Json(result).into_response()))
}

async fn check_qr(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(profile_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<QrStatusRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    require_content_type(&context, &headers, "application/json")?;
    let request = parse_json(&context, payload)?;
    let service = state.wechat.clone();
    let result = service
        .check_qr(&profile_id, &request)
        .await
        .map_err(|error| map_error(context.clone(), error))?;
    Ok(no_store(Json(result).into_response()))
}

async fn update_account_link(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path((profile_id, account_id)): Path<(String, String)>,
    headers: HeaderMap,
    payload: Result<Json<WechatAccountLinkPatch>, JsonRejection>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    require_content_type(&context, &headers, "application/json")?;
    let etag = parse_if_match(&context, &headers)?;
    let patch = parse_json(&context, payload)?;
    let service = state.wechat.clone();
    let result = tokio::task::spawn_blocking(move || {
        service.update_account_link(&profile_id, &account_id, &etag, &patch)
    })
    .await
    .map_err(|_| ApiError::blocking_task_failed(context.clone()))?
    .map_err(|error| map_error(context.clone(), error))?;
    Ok(no_store(versioned_json(StatusCode::OK, result)))
}

async fn poll_messages(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path((profile_id, account_id)): Path<(String, String)>,
    headers: HeaderMap,
    payload: Result<Json<WechatPollRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    require_content_type(&context, &headers, "application/json")?;
    let request = parse_json(&context, payload)?;
    let service = state.wechat.clone();
    let result = service
        .poll_messages(&profile_id, &account_id, &request)
        .await
        .map_err(|error| map_error(context.clone(), error))?;
    Ok(no_store(Json(result).into_response()))
}

async fn send_message(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path((profile_id, account_id)): Path<(String, String)>,
    headers: HeaderMap,
    payload: Result<Json<WechatSendRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    require_content_type(&context, &headers, "application/json")?;
    let request = parse_json(&context, payload)?;
    let service = state.wechat.clone();
    let result = service
        .send_message(&profile_id, &account_id, &request)
        .await
        .map_err(|error| map_error(context.clone(), error))?;
    Ok(no_store(Json(result).into_response()))
}

fn map_error(context: RequestContext, error: WechatError) -> ApiError {
    match error {
        WechatError::Profile(error) => ApiError::from_profile(context, error),
        WechatError::InvalidConfig
        | WechatError::InvalidQrRequest
        | WechatError::InvalidRequest => ApiError::new(
            context,
            StatusCode::BAD_REQUEST,
            "Invalid WeChat request",
            "validation_failed",
            "The WeChat configuration or QR request does not match the API contract.",
            false,
        ),
        WechatError::AccountNotFound => ApiError::new(
            context,
            StatusCode::NOT_FOUND,
            "WeChat account not found",
            "wechat_account_not_found",
            "The requested WeChat account does not exist for this Profile.",
            false,
        ),
        WechatError::PersonaNotFound => ApiError::new(
            context,
            StatusCode::NOT_FOUND,
            "Persona not found",
            "product_not_found",
            "The linked Persona does not exist for this Profile.",
            false,
        ),
        WechatError::PersonaLinkConflict => ApiError::new(
            context,
            StatusCode::CONFLICT,
            "WeChat Persona link conflict",
            "wechat_persona_link_conflict",
            "A Persona can be linked to only one WeChat account in a Profile.",
            false,
        ),
        WechatError::ProductCatalogUnavailable => ApiError::new(
            context,
            StatusCode::SERVICE_UNAVAILABLE,
            "Product catalog unavailable",
            "product_catalog_unavailable",
            "The local product catalog could not verify the requested Persona link.",
            true,
        ),
        WechatError::CredentialNotConfigured => ApiError::new(
            context,
            StatusCode::UNPROCESSABLE_ENTITY,
            "WeChat credential is not configured",
            "wechat_credential_not_configured",
            "Scan and confirm WeChat QR login before polling or sending messages.",
            false,
        ),
        WechatError::Rejected => ApiError::new(
            context,
            StatusCode::BAD_GATEWAY,
            "WeChat provider rejected request",
            "wechat_provider_rejected",
            "The WeChat provider rejected the request.",
            false,
        ),
        WechatError::Unavailable => ApiError::new(
            context,
            StatusCode::BAD_GATEWAY,
            "WeChat provider unavailable",
            "wechat_provider_unavailable",
            "The WeChat provider could not complete the request.",
            true,
        ),
        WechatError::InvalidResponse => ApiError::new(
            context,
            StatusCode::BAD_GATEWAY,
            "Invalid WeChat provider response",
            "wechat_provider_invalid_response",
            "The WeChat provider returned an invalid response.",
            true,
        ),
        WechatError::MissingCredential => ApiError::new(
            context,
            StatusCode::BAD_GATEWAY,
            "WeChat credential missing",
            "wechat_credential_missing",
            "The QR login completed without a usable bot credential.",
            false,
        ),
    }
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
