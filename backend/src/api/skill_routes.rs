use axum::{
    Extension, Json, Router,
    extract::{
        OriginalUri, Path, Query, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::{HeaderMap, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;

use crate::skills::{InstallSkill, ListSkills, SkillError, SkillPatch};

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
            "/api/v1/profiles/{profile_id}/skills",
            get(list_profile_skills),
        )
        .route(
            "/api/v1/profiles/{profile_id}/skills/install",
            axum::routing::post(install_profile_skill),
        )
        .route(
            "/api/v1/profiles/{profile_id}/skills/{skill_id}",
            axum::routing::patch(update_profile_skill).delete(uninstall_profile_skill),
        )
}

async fn list_profile_skills(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(profile_id): Path<String>,
    query: Result<Query<SkillListQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    let Query(query) = query.map_err(|_| {
        ApiError::new(
            context.clone(),
            StatusCode::BAD_REQUEST,
            "Invalid query",
            "validation_failed",
            "The query parameters do not match the API contract.",
            false,
        )
    })?;
    let request = ListSkills {
        query: query.query,
        cursor: query.cursor,
        limit: query.limit.unwrap_or(30),
    };
    let service = state.skills.clone();
    let result = run_blocking(context, move || service.list(&profile_id, &request)).await?;
    Ok(versioned_json(StatusCode::OK, result))
}

async fn install_profile_skill(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(profile_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<InstallSkill>, JsonRejection>,
) -> Result<Response, ApiError> {
    let origin_request_id = request_id.0.clone();
    let context = RequestContext::new(request_id, uri.path());
    if !state.skills.management_available() {
        return Err(ApiError::skill_management_unavailable(context));
    }
    require_single_content_type(&context, &headers, "application/json")?;
    let idempotency_key = parse_idempotency_key(&context, &headers)?;
    let request = parse_json(&context, payload)?;
    validate_install_request(&context, &request)?;
    let operation = state
        .skills
        .install(profile_id, request, idempotency_key, origin_request_id)
        .await
        .map_err(|error| ApiError::from_skill(context, error))?;
    Ok((StatusCode::ACCEPTED, Json(operation)).into_response())
}

async fn update_profile_skill(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path((profile_id, skill_id)): Path<(String, String)>,
    headers: HeaderMap,
    payload: Result<Json<SkillPatch>, JsonRejection>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    validate_skill_id(&context, &skill_id)?;
    require_content_type(&context, &headers, "application/merge-patch+json")?;
    let expected_etag = parse_if_match(&context, &headers)?;
    let patch = parse_json(&context, payload)?;
    let service = state.skills.clone();
    let result = run_blocking(context, move || {
        service.update(&profile_id, &skill_id, &patch, &expected_etag)
    })
    .await?;
    Ok(versioned_json(StatusCode::OK, result))
}

async fn uninstall_profile_skill(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path((profile_id, skill_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let origin_request_id = request_id.0.clone();
    let context = RequestContext::new(request_id, uri.path());
    validate_skill_id(&context, &skill_id)?;
    if !state.skills.management_available() {
        return Err(ApiError::skill_management_unavailable(context));
    }
    let idempotency_key = parse_idempotency_key(&context, &headers)?;
    let service = state.skills.clone();
    let operation = run_blocking(context, move || {
        service.uninstall(profile_id, &skill_id, idempotency_key, origin_request_id)
    })
    .await?;
    Ok((StatusCode::ACCEPTED, Json(operation)).into_response())
}

fn validate_install_request(
    context: &RequestContext,
    request: &InstallSkill,
) -> Result<(), ApiError> {
    let source_count = usize::from(request.registry_id.is_some())
        + usize::from(request.url.is_some())
        + usize::from(request.file_id.is_some());
    if source_count != 1 {
        return Err(invalid_install_request(context));
    }

    if let Some(value) = request.registry_id.as_deref()
        && !valid_install_value(value, 512)
    {
        return Err(invalid_install_request(context));
    }
    if let Some(value) = request.url.as_deref()
        && !valid_install_value(value, 2_048)
    {
        return Err(invalid_install_request(context));
    }
    if let Some(value) = request.file_id.as_deref()
        && (!valid_install_value(value, 128) || !valid_file_id(value))
    {
        return Err(invalid_install_request(context));
    }
    Ok(())
}

fn require_single_content_type(
    context: &RequestContext,
    headers: &HeaderMap,
    expected: &str,
) -> Result<(), ApiError> {
    let mut values = headers.get_all(CONTENT_TYPE).iter();
    if values.next().is_none() || values.next().is_some() {
        return Err(ApiError::unsupported_media_type(context.clone()));
    }
    require_content_type(context, headers, expected)
}

fn valid_install_value(value: &str, maximum_chars: usize) -> bool {
    value.trim() == value
        && !value.is_empty()
        && value.chars().count() <= maximum_chars
        && !value.chars().any(char::is_control)
}

fn valid_file_id(value: &str) -> bool {
    value.strip_prefix("file_").is_some_and(|suffix| {
        suffix.len() == 32
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn validate_skill_id(context: &RequestContext, value: &str) -> Result<(), ApiError> {
    let valid = value.strip_prefix("skill_").is_some_and(|suffix| {
        suffix.len() == 32
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    });
    if valid {
        Ok(())
    } else {
        Err(ApiError::from_skill(
            context.clone(),
            SkillError::InvalidRequest,
        ))
    }
}

fn invalid_install_request(context: &RequestContext) -> ApiError {
    ApiError::from_skill(context.clone(), SkillError::InvalidRequest)
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
            tracing::error!(error = ?error, "skill blocking task failed");
            Err(ApiError::blocking_task_failed(context))
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillListQuery {
    #[serde(rename = "q")]
    query: Option<String>,
    cursor: Option<String>,
    limit: Option<usize>,
}
