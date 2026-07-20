use axum::{
    Extension, Router,
    body::Body,
    extract::{
        DefaultBodyLimit, Multipart, OriginalUri, Path, State,
        multipart::{MultipartError, MultipartRejection},
    },
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE},
    },
    response::{IntoResponse, Response},
    routing::get,
};

use crate::files::{
    FileError, FileUpload, MAX_FILE_BYTES, MAX_MULTIPART_BYTES, normalize_mime_type,
};

use super::{
    AppState, RequestId,
    error::{ApiError, RequestContext},
    profile_routes::parse_idempotency_key,
};

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/files", axum::routing::post(upload_file))
        .route("/api/v1/files/{file_id}/content", get(get_file_content))
        .route(
            "/api/v1/files/{file_id}",
            axum::routing::delete(delete_file),
        )
        .layer(DefaultBodyLimit::max(MAX_MULTIPART_BYTES))
}

async fn upload_file(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    multipart: Result<Multipart, MultipartRejection>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    require_multipart_content_type(&context, &headers)?;
    let idempotency_key = parse_idempotency_key(&context, &headers)?;
    let multipart = multipart.map_err(|_| invalid_multipart(&context))?;
    let upload = parse_upload(&context, multipart).await?;
    let service = state.files.clone();
    let file = run_blocking(context, move || service.upload(&upload, &idempotency_key)).await?;
    Ok((StatusCode::CREATED, axum::Json(file)).into_response())
}

async fn get_file_content(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(file_id): Path<String>,
) -> Result<Response, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    let service = state.files.clone();
    let snapshot = run_blocking(context, move || service.read(&file_id)).await?;
    let content_type = HeaderValue::from_str(&snapshot.reference.mime_type)
        .expect("stored MIME types come from the static allowlist");
    let content_length = HeaderValue::from_str(&snapshot.bytes.len().to_string())
        .expect("a byte length is a valid HTTP header value");
    let mut response = Response::new(Body::from(snapshot.bytes));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(CONTENT_TYPE, content_type);
    response
        .headers_mut()
        .insert(CONTENT_LENGTH, content_length);
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        axum::http::HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    Ok(response)
}

async fn delete_file(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OriginalUri(uri): OriginalUri,
    Path(file_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let context = RequestContext::new(request_id, uri.path());
    let service = state.files.clone();
    run_blocking(context, move || service.delete(&file_id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn parse_upload(
    context: &RequestContext,
    mut multipart: Multipart,
) -> Result<FileUpload, ApiError> {
    let mut upload = None;
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|error| multipart_error(context, error))?
    {
        if upload.is_some() || field.name() != Some("file") {
            return Err(invalid_multipart(context));
        }
        let name = field
            .file_name()
            .map(ToOwned::to_owned)
            .ok_or_else(|| invalid_multipart(context))?;
        let mime_type = field
            .content_type()
            .ok_or_else(|| ApiError::unsupported_media_type(context.clone()))
            .and_then(|value| {
                normalize_mime_type(value)
                    .map_err(|error| ApiError::from_file(context.clone(), error))
            })?;
        let mut bytes = Vec::new();
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|error| multipart_error(context, error))?
        {
            let new_length = bytes
                .len()
                .checked_add(chunk.len())
                .ok_or_else(|| ApiError::payload_too_large(context.clone()))?;
            if new_length as u64 > MAX_FILE_BYTES {
                return Err(ApiError::payload_too_large(context.clone()));
            }
            bytes.extend_from_slice(&chunk);
        }
        upload = Some(FileUpload {
            name,
            mime_type,
            bytes,
        });
    }
    upload.ok_or_else(|| invalid_multipart(context))
}

fn require_multipart_content_type(
    context: &RequestContext,
    headers: &HeaderMap,
) -> Result<(), ApiError> {
    let mut values = headers.get_all(CONTENT_TYPE).iter();
    let Some(value) = values.next() else {
        return Err(ApiError::unsupported_media_type(context.clone()));
    };
    if values.next().is_some() {
        return Err(ApiError::unsupported_media_type(context.clone()));
    }
    let Ok(value) = value.to_str() else {
        return Err(ApiError::unsupported_media_type(context.clone()));
    };
    let media_type = value.split(';').next().map(str::trim).unwrap_or_default();
    if media_type.eq_ignore_ascii_case("multipart/form-data") {
        Ok(())
    } else {
        Err(ApiError::unsupported_media_type(context.clone()))
    }
}

fn multipart_error(context: &RequestContext, error: MultipartError) -> ApiError {
    if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
        ApiError::payload_too_large(context.clone())
    } else {
        invalid_multipart(context)
    }
}

fn invalid_multipart(context: &RequestContext) -> ApiError {
    ApiError::new(
        context.clone(),
        StatusCode::BAD_REQUEST,
        "Invalid multipart upload",
        "validation_failed",
        "The multipart body must contain exactly one valid file field.",
        false,
    )
}

async fn run_blocking<T, F>(context: RequestContext, operation: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, FileError> + Send + 'static,
{
    match tokio::task::spawn_blocking(operation).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(ApiError::from_file(context, error)),
        Err(error) => {
            tracing::error!(error = ?error, "file blocking task failed");
            Err(ApiError::blocking_task_failed(context))
        }
    }
}
