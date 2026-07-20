use axum::{
    Extension, Json, Router,
    extract::{OriginalUri, Path, State, rejection::JsonRejection},
    http::{
        HeaderMap, HeaderName, HeaderValue, StatusCode,
        header::{CONTENT_TYPE, ETAG, IF_MATCH},
    },
    response::{IntoResponse, Response},
    routing::{get, put},
};
use secrecy::SecretString;
use serde::Deserialize;
use serde_json::Value as JsonValue;

use crate::profiles::{CreateProfile, ProfileEngineState, ProfileError, Versioned};

use super::{
    AppState, RequestId,
    error::{ApiError, RequestContext},
};

static IDEMPOTENCY_KEY: HeaderName = HeaderName::from_static("idempotency-key");

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/providers", get(list_providers))
        .route("/api/v1/profiles", get(list_profiles).post(create_profile))
        .route(
            "/api/v1/profiles/{profile_id}",
            get(get_profile)
                .patch(update_profile)
                .delete(delete_profile),
        )
        .route(
            "/api/v1/profiles/{profile_id}/active",
            put(activate_profile),
        )
        .route(
            "/api/v1/profiles/{profile_id}/config",
            get(get_profile_config).patch(update_profile_config),
        )
        .route(
            "/api/v1/profiles/{profile_id}/secrets",
            get(list_secret_statuses),
        )
        .route(
            "/api/v1/profiles/{profile_id}/secrets/{secret_name}",
            put(put_secret).delete(delete_secret),
        )
}

async fn list_providers(State(state): State<AppState>) -> Json<Vec<crate::profiles::Provider>> {
    Json(state.profiles.providers())
}

async fn list_profiles(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
) -> Result<Json<Vec<crate::profiles::ProfileSummary>>, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    let engine_state = current_engine_state(&state);
    let service = state.profiles.clone();
    run_blocking(context, move || service.list_profiles(engine_state))
        .await
        .map(Json)
}

async fn create_profile(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    payload: Result<Json<CreateProfile>, JsonRejection>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    require_content_type(&context, &headers, "application/json")?;
    let idempotency_key = parse_idempotency_key(&context, &headers)?;
    let request = parse_json(&context, payload)?;
    let service = state.profiles.clone();
    let result = run_blocking(context.clone(), move || {
        service.create_profile(&request, &idempotency_key)
    })
    .await?;
    Ok(versioned_json(StatusCode::CREATED, result))
}

async fn get_profile(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(profile_id): Path<String>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    let service = state.profiles.clone();
    let result = run_blocking(context, move || service.get_profile(&profile_id)).await?;
    Ok(versioned_json(StatusCode::OK, result))
}

async fn update_profile(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(profile_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<JsonValue>, JsonRejection>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    require_content_type(&context, &headers, "application/merge-patch+json")?;
    let etag = parse_if_match(&context, &headers)?;
    let patch = parse_json(&context, payload)?;
    let service = state.profiles.clone();
    let result = run_blocking(context, move || {
        service.update_profile(&profile_id, &etag, &patch)
    })
    .await?;
    Ok(versioned_json(StatusCode::OK, result))
}

async fn delete_profile(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(profile_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    let service = state.profiles.clone();
    run_blocking(context, move || service.delete_profile(&profile_id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn activate_profile(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(profile_id): Path<String>,
) -> Result<Json<crate::profiles::ProfileSummary>, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    let engine_state = current_engine_state(&state);
    let service = state.profiles.clone();
    run_blocking(context, move || {
        service.activate_profile(&profile_id, engine_state)
    })
    .await
    .map(Json)
}

fn current_engine_state(state: &AppState) -> ProfileEngineState {
    if state.runs.is_available() {
        ProfileEngineState::Running
    } else {
        ProfileEngineState::Failed
    }
}

async fn get_profile_config(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(profile_id): Path<String>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    let service = state.profiles.clone();
    let result = run_blocking(context, move || service.get_config(&profile_id)).await?;
    Ok(versioned_json(StatusCode::OK, result))
}

async fn update_profile_config(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(profile_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<JsonValue>, JsonRejection>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    require_content_type(&context, &headers, "application/merge-patch+json")?;
    let etag = parse_if_match(&context, &headers)?;
    let patch = parse_json(&context, payload)?;
    let service = state.profiles.clone();
    let result = run_blocking(context, move || {
        service.update_config(&profile_id, &etag, &patch)
    })
    .await?;
    Ok(versioned_json(StatusCode::OK, result))
}

async fn list_secret_statuses(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(profile_id): Path<String>,
) -> Result<Json<Vec<crate::profiles::SecretStatus>>, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    let service = state.profiles.clone();
    run_blocking(context, move || service.list_secret_statuses(&profile_id))
        .await
        .map(Json)
}

async fn put_secret(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path((profile_id, secret_name)): Path<(String, String)>,
    headers: HeaderMap,
    payload: Result<Json<SecretValue>, JsonRejection>,
) -> Result<Json<crate::profiles::SecretStatus>, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    require_content_type(&context, &headers, "application/json")?;
    let request = parse_json(&context, payload)?;
    let service = state.profiles.clone();
    run_blocking(context, move || {
        service.put_secret(&profile_id, &secret_name, &request.value)
    })
    .await
    .map(Json)
}

async fn delete_secret(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path((profile_id, secret_name)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    let service = state.profiles.clone();
    run_blocking(context, move || {
        service.delete_secret(&profile_id, &secret_name)
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn run_blocking<T, F>(context: RequestContext, operation: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, ProfileError> + Send + 'static,
{
    match tokio::task::spawn_blocking(operation).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(ApiError::from_profile(context, error)),
        Err(error) => {
            tracing::error!(error = ?error, "profile blocking task failed");
            Err(ApiError::blocking_task_failed(context))
        }
    }
}

pub(super) fn parse_json<T>(
    context: &RequestContext,
    payload: Result<Json<T>, JsonRejection>,
) -> Result<T, ApiError> {
    payload.map(|Json(value)| value).map_err(|rejection| {
        if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
            ApiError::payload_too_large(context.clone())
        } else {
            ApiError::invalid_json(context.clone())
        }
    })
}

pub(super) fn require_content_type(
    context: &RequestContext,
    headers: &HeaderMap,
    expected: &str,
) -> Result<(), ApiError> {
    let Some(value) = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
    else {
        return Err(ApiError::unsupported_media_type(context.clone()));
    };
    let media_type = value.split(';').next().map(str::trim).unwrap_or_default();
    if media_type.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(ApiError::unsupported_media_type(context.clone()))
    }
}

pub(super) fn parse_if_match(
    context: &RequestContext,
    headers: &HeaderMap,
) -> Result<String, ApiError> {
    let mut values = headers.get_all(IF_MATCH).iter();
    let Some(value) = values.next() else {
        return Err(ApiError::new(
            context.clone(),
            StatusCode::PRECONDITION_REQUIRED,
            "Precondition required",
            "precondition_required",
            "A single strong If-Match value is required.",
            false,
        ));
    };
    if values.next().is_some() {
        return Err(invalid_if_match(context));
    }
    let Ok(value) = value.to_str() else {
        return Err(invalid_if_match(context));
    };
    if value.starts_with("W/") || value == "*" || value.len() > 128 {
        return Err(invalid_if_match(context));
    }
    let Some(inner) = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    else {
        return Err(invalid_if_match(context));
    };
    if inner.is_empty()
        || !inner
            .bytes()
            .all(|byte| byte == 0x21 || (0x23..=0x7e).contains(&byte))
    {
        return Err(invalid_if_match(context));
    }
    Ok(value.to_owned())
}

fn invalid_if_match(context: &RequestContext) -> ApiError {
    ApiError::new(
        context.clone(),
        StatusCode::BAD_REQUEST,
        "Invalid If-Match header",
        "invalid_if_match",
        "If-Match must contain exactly one quoted strong revision.",
        false,
    )
}

pub(super) fn parse_idempotency_key(
    context: &RequestContext,
    headers: &HeaderMap,
) -> Result<String, ApiError> {
    let mut values = headers.get_all(&IDEMPOTENCY_KEY).iter();
    let Some(value) = values.next() else {
        return Err(invalid_idempotency_key(context));
    };
    if values.next().is_some() {
        return Err(invalid_idempotency_key(context));
    }
    let Ok(value) = value.to_str() else {
        return Err(invalid_idempotency_key(context));
    };
    if !(8..=128).contains(&value.len()) || !value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
    {
        return Err(invalid_idempotency_key(context));
    }
    Ok(value.to_owned())
}

fn invalid_idempotency_key(context: &RequestContext) -> ApiError {
    ApiError::new(
        context.clone(),
        StatusCode::BAD_REQUEST,
        "Invalid idempotency key",
        "invalid_idempotency_key",
        "Idempotency-Key must be a single 8 to 128 character visible ASCII value.",
        false,
    )
}

pub(super) fn versioned_json<T: serde::Serialize>(
    status: StatusCode,
    result: Versioned<T>,
) -> Response {
    let mut response = (status, Json(result.value)).into_response();
    response.headers_mut().insert(
        ETAG,
        HeaderValue::from_str(&result.etag).expect("profile revisions are valid HTTP ETags"),
    );
    response
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SecretValue {
    value: SecretString,
}
