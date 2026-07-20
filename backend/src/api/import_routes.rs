use std::{fs, io, path::Path};

use axum::{
    Extension, Json, Router,
    extract::{OriginalUri, Path as AxumPath, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
    routing::get,
};

use crate::{
    compat::hermes_v21::{HermesV21Error, read_snapshot},
    sessions::{
        HermesImportError, HermesV21ImportPreview, HermesV21ImportRequest, HermesV21ImportResult,
    },
};

use super::{
    AppState, RequestId,
    error::{ApiError, RequestContext},
    profile_routes::{parse_idempotency_key, parse_json, require_content_type},
};

pub(super) fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/v1/profiles/{profile_id}/session-imports/hermes-v21",
        get(preview_import).post(import_snapshot),
    )
}

async fn preview_import(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    AxumPath(profile_id): AxumPath<String>,
) -> Result<Json<HermesV21ImportPreview>, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    let sessions = state.sessions.clone();
    let operation_profile_id = profile_id.clone();
    with_profile_state_db(context, &state, profile_id, move |path| {
        if !source_exists(path)? {
            return Ok(HermesV21ImportPreview::absent());
        }
        let snapshot = read_snapshot(path).map_err(ImportRouteError::Adapter)?;
        sessions
            .preview_from_snapshot(&operation_profile_id, &snapshot)
            .map_err(ImportRouteError::Import)
    })
    .await
    .map(Json)
}

async fn import_snapshot(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    AxumPath(profile_id): AxumPath<String>,
    headers: HeaderMap,
    payload: Result<Json<HermesV21ImportRequest>, JsonRejection>,
) -> Result<Json<HermesV21ImportResult>, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    require_content_type(&context, &headers, "application/json")?;
    let idempotency_key = parse_idempotency_key(&context, &headers)?;
    let request = parse_json(&context, payload)?;
    let sessions = state.sessions.clone();
    let operation_profile_id = profile_id.clone();
    with_profile_state_db(context, &state, profile_id, move |path| {
        if let Some(replay) = sessions
            .lookup_hermes_v21_replay(&operation_profile_id, &idempotency_key, &request)
            .map_err(ImportRouteError::Import)?
        {
            return Ok(replay);
        }
        if !source_exists(path)? {
            return Err(ImportRouteError::StateNotFound);
        }
        let snapshot = read_snapshot(path).map_err(ImportRouteError::Adapter)?;
        sessions
            .import_hermes_v21_snapshot(
                &operation_profile_id,
                &snapshot,
                &request,
                &idempotency_key,
            )
            .map_err(ImportRouteError::Import)
    })
    .await
    .map(Json)
}

async fn with_profile_state_db<T, F>(
    context: RequestContext,
    state: &AppState,
    profile_id: String,
    operation: F,
) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce(&Path) -> Result<T, ImportRouteError> + Send + 'static,
{
    let profiles = state.profiles.clone();
    match tokio::task::spawn_blocking(move || profiles.with_hermes_state_db(&profile_id, operation))
        .await
    {
        Ok(Ok(Ok(value))) => Ok(value),
        Ok(Ok(Err(error))) => Err(error.into_api_error(context)),
        Ok(Err(error)) => Err(ApiError::from_profile(context, error)),
        Err(error) => {
            tracing::error!(error = ?error, "Hermes import blocking task failed");
            Err(ApiError::blocking_task_failed(context))
        }
    }
}

fn source_exists(path: &Path) -> Result<bool, ImportRouteError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(ImportRouteError::UnsafeSource)
        }
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(ImportRouteError::StateUnavailable),
    }
}

enum ImportRouteError {
    StateNotFound,
    UnsafeSource,
    StateUnavailable,
    Adapter(HermesV21Error),
    Import(HermesImportError),
}

impl ImportRouteError {
    fn into_api_error(self, context: RequestContext) -> ApiError {
        match self {
            Self::StateNotFound => ApiError::new(
                context,
                StatusCode::NOT_FOUND,
                "Hermes state not found",
                "hermes_state_not_found",
                "The selected Profile has no Hermes state database to import.",
                false,
            ),
            Self::UnsafeSource => ApiError::new(
                context,
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid Hermes import source",
                "hermes_import_source_invalid",
                "The Hermes state path is not a regular non-symbolic-link file.",
                false,
            ),
            Self::StateUnavailable => ApiError::new(
                context,
                StatusCode::SERVICE_UNAVAILABLE,
                "Hermes state unavailable",
                "hermes_state_unavailable",
                "The Hermes state database could not be inspected safely.",
                true,
            ),
            Self::Adapter(error) => ApiError::from_hermes_v21(context, error),
            Self::Import(error) => ApiError::from_hermes_import(context, error),
        }
    }
}
