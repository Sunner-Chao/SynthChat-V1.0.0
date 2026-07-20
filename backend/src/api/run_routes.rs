use std::{convert::Infallible, time::Duration};

use async_stream::stream;
use axum::{
    Extension, Json, Router,
    body::Bytes,
    extract::{
        DefaultBodyLimit, OriginalUri, Path, Query, State,
        rejection::{BytesRejection, JsonRejection, QueryRejection},
    },
    http::{
        HeaderMap, HeaderName, HeaderValue, StatusCode,
        header::{CACHE_CONTROL, CONTENT_TYPE},
    },
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{get, post},
};
use serde::{Deserialize, de::DeserializeOwned};
use tokio::sync::broadcast;

use crate::runs::{
    ActionAccepted, ApprovalDecision, ClarificationAnswer, CreateRun, RunError, RunEventBatch,
};

use super::{
    AppState, RequestId,
    error::{ApiError, RequestContext},
    profile_routes::{parse_idempotency_key, parse_json, require_content_type},
};

const SSE_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const MAX_RUN_ACTION_BODY_BYTES: usize = 128 * 1024;
static LAST_EVENT_ID_HEADER: HeaderName = HeaderName::from_static("last-event-id");

pub(super) fn routes() -> Router<AppState> {
    let run_actions = Router::new()
        .route(
            "/api/v1/runs/{run_id}/approvals/{approval_id}",
            post(resolve_approval),
        )
        .route(
            "/api/v1/runs/{run_id}/clarifications/{request_id}",
            post(answer_clarification),
        )
        .layer(DefaultBodyLimit::max(MAX_RUN_ACTION_BODY_BYTES));

    Router::new()
        .route("/api/v1/runs", get(list_active_runs))
        .route("/api/v1/sessions/{session_id}/runs", post(create_run))
        .route("/api/v1/runs/{run_id}", get(get_run))
        .route("/api/v1/runs/{run_id}/events", get(stream_events))
        .route("/api/v1/runs/{run_id}/cancel", post(cancel_run))
        .layer(DefaultBodyLimit::max(8 * 1024 * 1024))
        .merge(run_actions)
}

async fn create_run(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(session_id): Path<String>,
    headers: HeaderMap,
    payload: Result<Json<CreateRun>, JsonRejection>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    require_content_type(&context, &headers, "application/json")?;
    let idempotency_key = parse_idempotency_key(&context, &headers)?;
    let request = parse_json(&context, payload)?;
    let accepted = state
        .runs
        .create_run(session_id, request, idempotency_key)
        .await
        .map_err(|error| ApiError::from_run(context, error))?;
    Ok((StatusCode::ACCEPTED, Json(accepted)).into_response())
}

async fn get_run(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(run_id): Path<String>,
) -> Result<Json<crate::runs::Run>, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    state
        .runs
        .get_run(run_id)
        .await
        .map(Json)
        .map_err(|error| ApiError::from_run(context, error))
}

async fn list_active_runs(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    query: Result<Query<ActiveRunsQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    let Query(ActiveRunsQuery {
        profile_id,
        state: ActiveRunQueryState::Active,
        session_id,
    }) = parse_active_runs_query(&context, query)?;
    ensure_profile(&context, &state, &profile_id).await?;
    let active = state
        .runs
        .list_active_runs(profile_id, session_id)
        .await
        .map_err(|error| ApiError::from_run(context, error))?;
    let mut response = Json(active).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

async fn cancel_run(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(run_id): Path<String>,
) -> Result<(StatusCode, Json<crate::runs::Run>), ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    state
        .runs
        .cancel_run(run_id)
        .await
        .map(|run| (StatusCode::ACCEPTED, Json(run)))
        .map_err(|error| ApiError::from_run(context, error))
}

async fn stream_events(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(run_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    let after_sequence = parse_last_event_id(&context, &headers, &run_id)?;
    let receiver = state.runs.subscribe(&run_id);
    let first = state
        .runs
        .event_batch(run_id.clone(), after_sequence)
        .await
        .map_err(|error| ApiError::from_run(context.clone(), error))?;
    let runs = state.runs.clone();
    let events = event_stream(runs, run_id, after_sequence, first, receiver);
    Ok(Sse::new(events)
        .keep_alive(
            KeepAlive::new()
                .interval(SSE_HEARTBEAT_INTERVAL)
                .text("heartbeat"),
        )
        .into_response())
}

fn event_stream(
    runs: std::sync::Arc<crate::runs::RunService>,
    run_id: String,
    after_sequence: u64,
    first: RunEventBatch,
    mut receiver: broadcast::Receiver<()>,
) -> impl futures_util::Stream<Item = Result<Event, Infallible>> {
    stream! {
        let mut cursor = after_sequence;
        let mut next_batch = Some(first);
        loop {
            let batch = match next_batch.take() {
                Some(batch) => batch,
                None => match runs.event_batch(run_id.clone(), cursor).await {
                    Ok(batch) => batch,
                    Err(error) => {
                        tracing::warn!(run_id, ?error, "run event replay stopped");
                        break;
                    }
                },
            };
            for record in batch.events {
                if record.sequence <= cursor {
                    continue;
                }
                cursor = record.sequence;
                yield Ok(Event::default()
                    .id(format!("{run_id}:{}", record.sequence))
                    .event(record.event_name)
                    .data(record.envelope_json));
            }
            if batch.terminal && cursor >= batch.last_sequence {
                break;
            }
            match receiver.recv().await {
                Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }
}

fn parse_last_event_id(
    context: &RequestContext,
    headers: &HeaderMap,
    run_id: &str,
) -> Result<u64, ApiError> {
    let mut values = headers.get_all(&LAST_EVENT_ID_HEADER).iter();
    let Some(value) = values.next() else {
        return Ok(0);
    };
    if values.next().is_some() {
        return Err(ApiError::from_run(
            context.clone(),
            RunError::InvalidEventId,
        ));
    }
    let value = value
        .to_str()
        .map_err(|_| ApiError::from_run(context.clone(), RunError::InvalidEventId))?;
    let (event_run_id, sequence) = value
        .rsplit_once(':')
        .ok_or_else(|| ApiError::from_run(context.clone(), RunError::InvalidEventId))?;
    if event_run_id != run_id
        || sequence.is_empty()
        || !sequence.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(ApiError::from_run(
            context.clone(),
            RunError::InvalidEventId,
        ));
    }
    sequence
        .parse()
        .map_err(|_| ApiError::from_run(context.clone(), RunError::InvalidEventId))
}

async fn resolve_approval(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path((run_id, approval_id)): Path<(String, String)>,
    headers: HeaderMap,
    payload: Result<Bytes, BytesRejection>,
) -> Result<Json<ActionAccepted>, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    if !valid_resource_id(&run_id, "run_") || !valid_resource_id(&approval_id, "approval_") {
        return Err(ApiError::from_run(
            context,
            RunError::InvalidApprovalRequest,
        ));
    }
    require_run_action_content_type(&context, &headers)?;
    let decision: ApprovalDecision = parse_run_action_json(&context, payload)?;
    decision
        .validate()
        .map_err(|error| ApiError::from_run(context.clone(), error))?;
    state
        .runs
        .resolve_approval(run_id, approval_id, decision)
        .await
        .map(Json)
        .map_err(|error| ApiError::from_run(context, error))
}

async fn answer_clarification(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path((run_id, clarification_id)): Path<(String, String)>,
    headers: HeaderMap,
    payload: Result<Bytes, BytesRejection>,
) -> Result<Json<ActionAccepted>, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    if !valid_resource_id(&run_id, "run_")
        || !valid_resource_id(&clarification_id, "clarification_")
    {
        return Err(ApiError::from_run(
            context,
            RunError::InvalidClarificationRequest,
        ));
    }
    require_run_action_content_type(&context, &headers)?;
    let answer: ClarificationAnswer = parse_run_action_json(&context, payload)?;
    answer
        .validate()
        .map_err(|error| ApiError::from_run(context.clone(), error))?;
    state
        .runs
        .answer_clarification(run_id, clarification_id, answer)
        .await
        .map(Json)
        .map_err(|error| ApiError::from_run(context, error))
}

fn parse_run_action_json<T: DeserializeOwned>(
    context: &RequestContext,
    payload: Result<Bytes, BytesRejection>,
) -> Result<T, ApiError> {
    let bytes = payload.map_err(|rejection| {
        if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
            ApiError::payload_too_large(context.clone())
        } else {
            ApiError::invalid_json(context.clone())
        }
    })?;
    if bytes.len() > MAX_RUN_ACTION_BODY_BYTES {
        return Err(ApiError::payload_too_large(context.clone()));
    }
    serde_json::from_slice(&bytes).map_err(|_| ApiError::invalid_json(context.clone()))
}

fn require_run_action_content_type(
    context: &RequestContext,
    headers: &HeaderMap,
) -> Result<(), ApiError> {
    if headers.get_all(CONTENT_TYPE).iter().count() != 1 {
        return Err(ApiError::unsupported_media_type(context.clone()));
    }
    require_content_type(context, headers, "application/json")
}

fn valid_resource_id(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|suffix| {
        suffix.len() == 32
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

async fn ensure_profile(
    context: &RequestContext,
    state: &AppState,
    profile_id: &str,
) -> Result<(), ApiError> {
    let profiles = state.profiles.clone();
    let profile_id = profile_id.to_owned();
    match tokio::task::spawn_blocking(move || profiles.get_profile(&profile_id)).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(error)) => Err(ApiError::from_profile(context.clone(), error)),
        Err(error) => {
            tracing::error!(error = ?error, "active Run profile validation task failed");
            Err(ApiError::blocking_task_failed(context.clone()))
        }
    }
}

fn parse_active_runs_query<T>(
    context: &RequestContext,
    query: Result<Query<T>, QueryRejection>,
) -> Result<Query<T>, ApiError> {
    query.map_err(|_| {
        ApiError::new(
            context.clone(),
            StatusCode::BAD_REQUEST,
            "Invalid query",
            "validation_failed",
            "The active Run query does not match the API contract.",
            false,
        )
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ActiveRunsQuery {
    profile_id: String,
    state: ActiveRunQueryState,
    session_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum ActiveRunQueryState {
    Active,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn context() -> RequestContext {
        RequestContext::new(RequestId("request".to_owned()), "/events")
    }

    #[test]
    fn last_event_id_is_run_bound_and_strict() {
        let mut headers = HeaderMap::new();
        assert_eq!(
            parse_last_event_id(&context(), &headers, "run_1").unwrap(),
            0
        );
        headers.insert(
            LAST_EVENT_ID_HEADER.clone(),
            HeaderValue::from_static("run_1:42"),
        );
        assert_eq!(
            parse_last_event_id(&context(), &headers, "run_1").unwrap(),
            42
        );
        for invalid in ["run_2:42", "run_1:-1", "run_1:+1", "run_1:", "42"] {
            headers.insert(
                LAST_EVENT_ID_HEADER.clone(),
                HeaderValue::from_str(invalid).unwrap(),
            );
            assert!(parse_last_event_id(&context(), &headers, "run_1").is_err());
        }
    }
}
