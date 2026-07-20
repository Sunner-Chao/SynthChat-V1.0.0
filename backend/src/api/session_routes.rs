use axum::{
    Extension, Json, Router,
    extract::{
        OriginalUri, Path, Query, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{IF_MATCH, LOCATION},
    },
    response::Response,
    routing::get,
};
use serde::Deserialize;

use crate::sessions::{
    CreateSession, ListMessages, ListSessions, MessagePage, SessionError, SessionPage, SessionPatch,
};

use super::{
    AppState, RequestId,
    error::{ApiError, RequestContext},
    profile_routes::{
        parse_idempotency_key, parse_if_match, parse_json, require_content_type, versioned_json,
    },
};

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/sessions", get(list_sessions).post(create_session))
        .route(
            "/api/v1/sessions/{session_id}",
            get(get_session)
                .patch(update_session)
                .delete(delete_session),
        )
        .route("/api/v1/sessions/{session_id}/messages", get(list_messages))
}

async fn list_sessions(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    query: Result<Query<SessionListQuery>, QueryRejection>,
) -> Result<Json<SessionPage>, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    let Query(query) = parse_query(&context, query)?;
    ensure_profile(&context, &state, &query.profile_id).await?;
    let request = ListSessions {
        profile_id: query.profile_id,
        query: query.query,
        archived: query.archived.unwrap_or(false),
        cursor: query.cursor,
        limit: query.limit.unwrap_or(30),
    };
    let service = state.sessions.clone();
    run_session(context, move || service.list_sessions(&request))
        .await
        .map(Json)
}

async fn create_session(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    payload: Result<Json<CreateSession>, JsonRejection>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    require_content_type(&context, &headers, "application/json")?;
    let idempotency_key = parse_idempotency_key(&context, &headers)?;
    let request = parse_json(&context, payload)?;
    let service = state.sessions.clone();
    let profile_id = request.profile_id.clone();
    let created = run_session_for_existing_profile(context, &state, profile_id, move || {
        service.create_session(&request, &idempotency_key)
    })
    .await?;
    let location = format!("/api/v1/sessions/{}", created.value.id);
    let mut response = versioned_json(StatusCode::CREATED, created);
    response.headers_mut().insert(
        LOCATION,
        HeaderValue::from_str(&location).expect("generated Session IDs form valid locations"),
    );
    Ok(response)
}

async fn get_session(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(session_id): Path<String>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    let service = state.sessions.clone();
    let session = run_session(context, move || service.get_session(&session_id)).await?;
    Ok(versioned_json(StatusCode::OK, session))
}

async fn update_session(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(session_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<SessionPatch>, JsonRejection>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    require_content_type(&context, &headers, "application/merge-patch+json")?;
    let etag = parse_if_match(&context, &headers)?;
    let patch = parse_json(&context, payload)?;
    let service = state.sessions.clone();
    let updated = run_session(context, move || {
        service.update_session(&session_id, &etag, &patch)
    })
    .await?;
    Ok(versioned_json(StatusCode::OK, updated))
}

async fn delete_session(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(session_id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    let etag = optional_if_match(&context, &headers)?;
    let service = state.sessions.clone();
    run_session(context, move || {
        service.delete_session(&session_id, etag.as_deref())
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_messages(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(session_id): Path<String>,
    query: Result<Query<MessageListQuery>, QueryRejection>,
) -> Result<Json<MessagePage>, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    let Query(query) = parse_query(&context, query)?;
    let request = ListMessages {
        cursor: query.cursor,
        limit: query.limit.unwrap_or(30),
    };
    let service = state.sessions.clone();
    run_session(context, move || {
        service.list_messages(&session_id, &request)
    })
    .await
    .map(Json)
}

async fn ensure_profile(
    context: &RequestContext,
    state: &AppState,
    profile_id: &str,
) -> Result<(), ApiError> {
    let service = state.profiles.clone();
    let profile_id = profile_id.to_owned();
    match tokio::task::spawn_blocking(move || service.get_profile(&profile_id)).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(error)) => Err(ApiError::from_profile(context.clone(), error)),
        Err(error) => {
            tracing::error!(error = ?error, "profile validation task failed");
            Err(ApiError::blocking_task_failed(context.clone()))
        }
    }
}

async fn run_session<T, F>(context: RequestContext, operation: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, SessionError> + Send + 'static,
{
    match tokio::task::spawn_blocking(operation).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(ApiError::from_session(context, error)),
        Err(error) => {
            tracing::error!(error = ?error, "session blocking task failed");
            Err(ApiError::blocking_task_failed(context))
        }
    }
}

async fn run_session_for_existing_profile<T, F>(
    context: RequestContext,
    state: &AppState,
    profile_id: String,
    operation: F,
) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, SessionError> + Send + 'static,
{
    let profiles = state.profiles.clone();
    match tokio::task::spawn_blocking(move || {
        profiles.with_existing_profile(&profile_id, operation)
    })
    .await
    {
        Ok(Ok(Ok(value))) => Ok(value),
        Ok(Ok(Err(error))) => Err(ApiError::from_session(context, error)),
        Ok(Err(error)) => Err(ApiError::from_profile(context, error)),
        Err(error) => {
            tracing::error!(error = ?error, "session/profile blocking task failed");
            Err(ApiError::blocking_task_failed(context))
        }
    }
}

fn parse_query<T>(
    context: &RequestContext,
    query: Result<Query<T>, QueryRejection>,
) -> Result<Query<T>, ApiError> {
    query.map_err(|_| {
        ApiError::new(
            context.clone(),
            StatusCode::BAD_REQUEST,
            "Invalid query",
            "validation_failed",
            "The query parameters do not match the API contract.",
            false,
        )
    })
}

fn optional_if_match(
    context: &RequestContext,
    headers: &HeaderMap,
) -> Result<Option<String>, ApiError> {
    if headers.contains_key(IF_MATCH) {
        parse_if_match(context, headers).map(Some)
    } else {
        Ok(None)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionListQuery {
    profile_id: String,
    #[serde(rename = "q")]
    query: Option<String>,
    archived: Option<bool>,
    cursor: Option<String>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageListQuery {
    cursor: Option<String>,
    limit: Option<usize>,
}
