use axum::{
    Json,
    http::{HeaderValue, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use serde::Serialize;

use crate::{
    compat::hermes_v21::HermesV21Error,
    files::FileError,
    mcp::McpError,
    memory::MemoryError,
    operations::OperationError,
    profiles::ProfileError,
    runs::RunError,
    sessions::{HermesImportError, SessionError},
    skills::{LifecycleError, LifecycleStartError, SkillError},
    tools::ToolsetError,
};

use super::RequestId;

#[derive(Clone, Debug)]
pub(crate) struct RequestContext {
    request_id: String,
    instance: String,
}

impl RequestContext {
    pub(crate) fn new(request_id: RequestId, instance: impl Into<String>) -> Self {
        Self {
            request_id: request_id.0,
            instance: instance.into(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct ApiError {
    context: Box<RequestContext>,
    status: StatusCode,
    title: &'static str,
    code: &'static str,
    detail: &'static str,
    retryable: bool,
    etag: Option<String>,
    retry_after_seconds: Option<u64>,
    import_conflicts: Option<Box<crate::sessions::HermesImportConflictReport>>,
}

impl ApiError {
    pub(crate) fn new(
        context: RequestContext,
        status: StatusCode,
        title: &'static str,
        code: &'static str,
        detail: &'static str,
        retryable: bool,
    ) -> Self {
        Self {
            context: Box::new(context),
            status,
            title,
            code,
            detail,
            retryable,
            etag: None,
            retry_after_seconds: None,
            import_conflicts: None,
        }
    }

    pub(crate) fn unauthorized(context: RequestContext) -> Self {
        Self::new(
            context,
            StatusCode::UNAUTHORIZED,
            "Unauthorized",
            "unauthorized",
            "A valid desktop session token is required.",
            false,
        )
    }

    pub(crate) fn invalid_json(context: RequestContext) -> Self {
        Self::new(
            context,
            StatusCode::BAD_REQUEST,
            "Invalid request body",
            "invalid_json",
            "The request body must be valid JSON matching the API contract.",
            false,
        )
    }

    pub(crate) fn payload_too_large(context: RequestContext) -> Self {
        Self::new(
            context,
            StatusCode::PAYLOAD_TOO_LARGE,
            "Payload too large",
            "payload_too_large",
            "The request body exceeds the supported API limit.",
            false,
        )
    }

    pub(crate) fn unsupported_media_type(context: RequestContext) -> Self {
        Self::new(
            context,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Unsupported media type",
            "unsupported_media_type",
            "The request Content-Type does not match the API contract.",
            false,
        )
    }

    pub(crate) fn not_found(context: RequestContext) -> Self {
        Self::new(
            context,
            StatusCode::NOT_FOUND,
            "Not found",
            "route_not_found",
            "The requested API route does not exist.",
            false,
        )
    }

    pub(crate) fn method_not_allowed(context: RequestContext) -> Self {
        Self::new(
            context,
            StatusCode::METHOD_NOT_ALLOWED,
            "Method not allowed",
            "method_not_allowed",
            "The HTTP method is not supported by this route.",
            false,
        )
    }

    pub(crate) fn blocking_task_failed(context: RequestContext) -> Self {
        Self::new(
            context,
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal server error",
            "internal_error",
            "The backend could not complete the local operation.",
            true,
        )
    }

    pub(crate) fn skill_management_unavailable(context: RequestContext) -> Self {
        Self::new(
            context,
            StatusCode::SERVICE_UNAVAILABLE,
            "Skill management unavailable",
            "skill_management_unavailable",
            "The backend could not start the local Skill management operation.",
            true,
        )
    }

    pub(crate) fn from_profile(context: RequestContext, error: ProfileError) -> Self {
        Self::from_profile_ref(context, &error)
    }

    fn from_profile_ref(context: RequestContext, error: &ProfileError) -> Self {
        match error {
            ProfileError::InvalidProfileId => Self::new(
                context,
                StatusCode::BAD_REQUEST,
                "Invalid profile ID",
                "invalid_profile_id",
                "The profile ID does not match the supported Hermes format.",
                false,
            ),
            ProfileError::ReservedProfileId => Self::new(
                context,
                StatusCode::BAD_REQUEST,
                "Reserved profile ID",
                "reserved_profile_id",
                "The default profile ID is reserved and cannot be created.",
                false,
            ),
            ProfileError::InvalidProfileMetadata => Self::new(
                context,
                StatusCode::BAD_REQUEST,
                "Invalid profile metadata",
                "invalid_profile_metadata",
                "The profile metadata does not match the API contract.",
                false,
            ),
            ProfileError::InvalidProfileConfig => Self::new(
                context,
                StatusCode::BAD_REQUEST,
                "Invalid profile configuration",
                "invalid_profile_config",
                "The profile configuration does not match the API contract.",
                false,
            ),
            ProfileError::InvalidSecretName => Self::new(
                context,
                StatusCode::BAD_REQUEST,
                "Invalid secret name",
                "invalid_secret_name",
                "The secret name does not match the supported environment-variable format.",
                false,
            ),
            ProfileError::InvalidSecretValue => Self::new(
                context,
                StatusCode::BAD_REQUEST,
                "Invalid secret value",
                "invalid_secret_value",
                "The secret value is empty or exceeds the cross-platform keychain limit.",
                false,
            ),
            ProfileError::ProfileNotFound => Self::new(
                context,
                StatusCode::NOT_FOUND,
                "Profile not found",
                "profile_not_found",
                "The requested profile does not exist.",
                false,
            ),
            ProfileError::ProfileAlreadyExists => Self::new(
                context,
                StatusCode::CONFLICT,
                "Profile already exists",
                "profile_exists",
                "A profile with this ID already exists.",
                false,
            ),
            ProfileError::ProfileDeleteConflict => Self::new(
                context,
                StatusCode::CONFLICT,
                "Profile cannot be deleted",
                "profile_delete_conflict",
                "The default or active profile cannot be deleted.",
                false,
            ),
            ProfileError::IdempotencyConflict => Self::new(
                context,
                StatusCode::CONFLICT,
                "Idempotency conflict",
                "idempotency_conflict",
                "The idempotency key was already used for a different request.",
                false,
            ),
            ProfileError::IdempotencyResourceGone => Self::new(
                context,
                StatusCode::CONFLICT,
                "Idempotent resource is unavailable",
                "idempotency_resource_gone",
                "The original idempotent resource no longer exists.",
                false,
            ),
            ProfileError::RevisionConflict { current_etag } => {
                let mut response = Self::new(
                    context,
                    StatusCode::CONFLICT,
                    "Revision conflict",
                    "revision_conflict",
                    "The resource changed after it was read. Reload it before saving.",
                    false,
                );
                response.etag = Some(current_etag.clone());
                response
            }
            ProfileError::SecretStorageUnavailable => Self::new(
                context,
                StatusCode::SERVICE_UNAVAILABLE,
                "Secret storage unavailable",
                "secret_storage_unavailable",
                "The operating system keychain is unavailable or locked.",
                true,
            ),
            ProfileError::UnsafeProfilePath => Self::new(
                context,
                StatusCode::CONFLICT,
                "Unsafe profile path",
                "unsafe_profile_path",
                "The profile contains a symbolic link or path outside HERMES_HOME.",
                false,
            ),
            ProfileError::DataTooLarge => Self::new(
                context,
                StatusCode::CONFLICT,
                "Profile data is too large",
                "profile_data_too_large",
                "An existing profile file exceeds the supported local size limit.",
                false,
            ),
            ProfileError::DataInvalid => Self::new(
                context,
                StatusCode::CONFLICT,
                "Profile data is malformed",
                "profile_data_invalid",
                "An existing profile file cannot be read safely.",
                false,
            ),
            ProfileError::Storage(error) => {
                tracing::error!(error = ?error, "profile storage operation failed");
                Self::new(
                    context,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Profile storage failed",
                    "profile_storage_failed",
                    "The backend could not persist the profile operation.",
                    true,
                )
            }
        }
    }

    pub(crate) fn from_mcp(context: RequestContext, error: McpError) -> Self {
        match error {
            McpError::InvalidRequest | McpError::InvalidServerId => Self::new(
                context,
                StatusCode::BAD_REQUEST,
                "Invalid MCP server request",
                "validation_failed",
                "The MCP server request does not match the API contract.",
                false,
            ),
            McpError::ServerNotFound => Self::new(
                context,
                StatusCode::NOT_FOUND,
                "MCP server not found",
                "resource_not_found",
                "The requested MCP server does not exist.",
                false,
            ),
            McpError::NameConflict => Self::new(
                context,
                StatusCode::CONFLICT,
                "MCP server name already exists",
                "mcp_server_exists",
                "A server with this name already exists in the Profile.",
                false,
            ),
            McpError::CapacityExceeded => Self::new(
                context,
                StatusCode::CONFLICT,
                "MCP configuration capacity exceeded",
                "capacity_exceeded",
                "The Profile cannot retain another MCP server or idempotency record.",
                false,
            ),
            McpError::IdempotencyConflict => Self::new(
                context,
                StatusCode::CONFLICT,
                "Idempotency conflict",
                "idempotency_conflict",
                "The idempotency key was already used for a different request.",
                false,
            ),
            McpError::IdempotencyResourceGone => Self::new(
                context,
                StatusCode::CONFLICT,
                "Idempotent resource is unavailable",
                "idempotency_resource_gone",
                "The original idempotent resource no longer exists.",
                false,
            ),
            McpError::StoredConfigInvalid => Self::new(
                context,
                StatusCode::CONFLICT,
                "Stored MCP configuration is invalid",
                "mcp_config_invalid",
                "The Profile contains MCP configuration that cannot be projected safely.",
                false,
            ),
            McpError::Profile(error) => Self::from_profile(context, error),
        }
    }

    pub(crate) fn from_file(context: RequestContext, error: FileError) -> Self {
        Self::from_file_ref(context, &error)
    }

    fn from_file_ref(context: RequestContext, error: &FileError) -> Self {
        match error {
            FileError::InvalidRequest | FileError::InvalidFileId => Self::new(
                context,
                StatusCode::BAD_REQUEST,
                "Invalid file request",
                "validation_failed",
                "The file request does not match the API contract.",
                false,
            ),
            FileError::UnsupportedMimeType => Self::new(
                context,
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "Unsupported file type",
                "unsupported_file_type",
                "The file MIME type is not enabled by the backend capabilities.",
                false,
            ),
            FileError::PayloadTooLarge => Self::payload_too_large(context),
            FileError::QuotaExceeded => Self::new(
                context,
                StatusCode::INSUFFICIENT_STORAGE,
                "File storage quota exhausted",
                "file_quota_exceeded",
                "The retained file store cannot accept another snapshot.",
                false,
            ),
            FileError::NotFound => Self::new(
                context,
                StatusCode::NOT_FOUND,
                "File not found",
                "resource_not_found",
                "The requested file does not exist.",
                false,
            ),
            FileError::IdempotencyConflict => Self::new(
                context,
                StatusCode::CONFLICT,
                "Idempotency conflict",
                "idempotency_conflict",
                "The idempotency key was already used for a different request.",
                false,
            ),
            FileError::IdempotencyResourceGone => Self::new(
                context,
                StatusCode::CONFLICT,
                "Idempotent resource is unavailable",
                "idempotency_resource_gone",
                "The original idempotent file was explicitly deleted.",
                false,
            ),
            FileError::UnsafePath => Self::new(
                context,
                StatusCode::CONFLICT,
                "Unsafe file store path",
                "unsafe_file_path",
                "The file store contains a symbolic link, reparse point, or unexpected entry.",
                false,
            ),
            FileError::DataInvalid => Self::new(
                context,
                StatusCode::SERVICE_UNAVAILABLE,
                "File storage unavailable",
                "file_storage_unavailable",
                "The local file store could not be read safely.",
                true,
            ),
            FileError::Storage(error) => {
                tracing::error!(error = ?error, "file storage operation failed");
                Self::new(
                    context,
                    StatusCode::SERVICE_UNAVAILABLE,
                    "File storage unavailable",
                    "file_storage_unavailable",
                    "The backend could not access the local file store.",
                    true,
                )
            }
        }
    }

    pub(crate) fn from_session(context: RequestContext, error: SessionError) -> Self {
        match error {
            SessionError::InvalidRequest | SessionError::InvalidSessionId => Self::new(
                context,
                StatusCode::BAD_REQUEST,
                "Invalid session request",
                "validation_failed",
                "The session request does not match the API contract.",
                false,
            ),
            SessionError::InvalidWorkspacePath => Self::new(
                context,
                StatusCode::UNPROCESSABLE_ENTITY,
                "Workspace unavailable",
                "workspace_unavailable",
                "The workspace root must be an existing absolute directory.",
                false,
            ),
            SessionError::WorkspaceInUse => Self::new(
                context,
                StatusCode::CONFLICT,
                "Workspace in use",
                "workspace_in_use",
                "A persisted Run still references this workspace registration.",
                false,
            ),
            SessionError::InvalidCursor => Self::new(
                context,
                StatusCode::BAD_REQUEST,
                "Invalid cursor",
                "invalid_cursor",
                "The cursor is invalid, expired, or belongs to different filters.",
                false,
            ),
            SessionError::NotFound => Self::new(
                context,
                StatusCode::NOT_FOUND,
                "Session not found",
                "resource_not_found",
                "The requested session does not exist.",
                false,
            ),
            SessionError::RevisionConflict { current_etag } => {
                let mut response = Self::new(
                    context,
                    StatusCode::CONFLICT,
                    "Revision conflict",
                    "revision_conflict",
                    "The session changed after it was read. Reload it before saving.",
                    false,
                );
                response.etag = Some(current_etag);
                response
            }
            SessionError::PreconditionRequired => Self::new(
                context,
                StatusCode::PRECONDITION_REQUIRED,
                "Precondition required",
                "precondition_required",
                "A current strong Session If-Match value is required.",
                false,
            ),
            SessionError::IdempotencyConflict => Self::new(
                context,
                StatusCode::CONFLICT,
                "Idempotency conflict",
                "idempotency_conflict",
                "The idempotency key was already used for a different request.",
                false,
            ),
            SessionError::IdempotentResourceDeleted => Self::new(
                context,
                StatusCode::GONE,
                "Idempotent resource deleted",
                "idempotent_resource_deleted",
                "The session created by this idempotency key was explicitly deleted.",
                false,
            ),
            SessionError::Archived => Self::new(
                context,
                StatusCode::CONFLICT,
                "Session archived",
                "session_archived",
                "Restore the session before adding another message.",
                false,
            ),
            SessionError::Busy => Self::new(
                context,
                StatusCode::CONFLICT,
                "Session busy",
                "session_busy",
                "The session has a non-terminal run and cannot be deleted.",
                false,
            ),
            SessionError::SearchUnavailable => Self::new(
                context,
                StatusCode::UNPROCESSABLE_ENTITY,
                "Session search unavailable",
                "session_search_unavailable",
                "Literal session search is not available in the local store.",
                false,
            ),
            SessionError::StorageBusy => {
                let mut response = Self::new(
                    context,
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Session storage busy",
                    "session_storage_busy",
                    "The local session store remained locked past its bounded timeout.",
                    true,
                );
                response.retry_after_seconds = Some(1);
                response
            }
            SessionError::StorageUnavailable | SessionError::DataInvalid => Self::new(
                context,
                StatusCode::SERVICE_UNAVAILABLE,
                "Session storage unavailable",
                "session_storage_unavailable",
                "The local session store could not be read safely.",
                true,
            ),
        }
    }

    pub(crate) fn from_toolset(context: RequestContext, error: ToolsetError) -> Self {
        match error {
            ToolsetError::NotFound => Self::new(
                context,
                StatusCode::NOT_FOUND,
                "Toolset not found",
                "resource_not_found",
                "The requested toolset is not registered by the Rust backend.",
                false,
            ),
            ToolsetError::Profile(error) => Self::from_profile(context, error),
        }
    }

    pub(crate) fn from_skill(context: RequestContext, error: SkillError) -> Self {
        match error {
            SkillError::InvalidRequest => Self::new(
                context,
                StatusCode::BAD_REQUEST,
                "Invalid skill request",
                "validation_failed",
                "The skill request does not match the API contract.",
                false,
            ),
            SkillError::InvalidCursor => Self::new(
                context,
                StatusCode::BAD_REQUEST,
                "Invalid cursor",
                "invalid_cursor",
                "The skill cursor is invalid, expired, or no longer matches the installed skills.",
                false,
            ),
            SkillError::NotFound => Self::new(
                context,
                StatusCode::NOT_FOUND,
                "Skill not found",
                "resource_not_found",
                "The requested skill is not installed for this profile.",
                false,
            ),
            SkillError::DataInvalid => Self::new(
                context,
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid skill data",
                "skill_data_invalid",
                "An installed skill does not match the supported Hermes skill format.",
                false,
            ),
            SkillError::StorageUnavailable => Self::new(
                context,
                StatusCode::SERVICE_UNAVAILABLE,
                "Skill storage unavailable",
                "skill_storage_unavailable",
                "The profile skill directory could not be read safely.",
                true,
            ),
            SkillError::Lifecycle(error) => Self::from_skill_lifecycle(context, error),
            SkillError::Profile(error) => Self::from_profile(context, error),
        }
    }

    fn from_skill_lifecycle(context: RequestContext, error: LifecycleStartError) -> Self {
        match error {
            LifecycleStartError::Lifecycle(error) => Self::from_lifecycle(context, error),
            LifecycleStartError::Operation(error) => Self::from_operation(context, &error),
            LifecycleStartError::Profile(error) => Self::from_profile(context, error),
        }
    }

    fn from_lifecycle(context: RequestContext, error: LifecycleError) -> Self {
        match error {
            LifecycleError::InvalidRequest => Self::new(
                context,
                StatusCode::BAD_REQUEST,
                "Invalid skill request",
                "validation_failed",
                "The skill request does not match the API contract.",
                false,
            ),
            LifecycleError::UnsafeSource => Self::new(
                context,
                StatusCode::UNPROCESSABLE_ENTITY,
                "Unsafe Skill source",
                "skill_source_unsafe",
                "The Skill source violated the local path or network safety policy.",
                false,
            ),
            LifecycleError::InvalidBundle => Self::new(
                context,
                StatusCode::UNPROCESSABLE_ENTITY,
                "Skill installation rejected",
                "skill_bundle_invalid",
                "The Skill source did not match the supported bundle contract.",
                false,
            ),
            LifecycleError::BundleTooLarge => Self::new(
                context,
                StatusCode::PAYLOAD_TOO_LARGE,
                "Skill bundle too large",
                "skill_bundle_too_large",
                "The Skill source exceeded the file count or byte limits.",
                false,
            ),
            LifecycleError::SecurityBlocked => Self::new(
                context,
                StatusCode::UNPROCESSABLE_ENTITY,
                "Skill blocked by security policy",
                "skill_security_blocked",
                "The Skill contained instructions or code blocked by the static security policy.",
                false,
            ),
            LifecycleError::SourceNotFound => Self::new(
                context,
                StatusCode::NOT_FOUND,
                "Skill source not found",
                "skill_source_not_found",
                "The exact Skill source could not be found.",
                false,
            ),
            LifecycleError::Transport => Self::new(
                context,
                StatusCode::BAD_GATEWAY,
                "Skill source unavailable",
                "skill_source_unavailable",
                "The external Skill source could not be fetched.",
                true,
            ),
            LifecycleError::RateLimited => Self::new(
                context,
                StatusCode::TOO_MANY_REQUESTS,
                "Skill source rate limited",
                "skill_source_rate_limited",
                "The external Skill source rate limit was exceeded.",
                true,
            ),
            LifecycleError::OperationCapacity => Self::new(
                context,
                StatusCode::TOO_MANY_REQUESTS,
                "Skill operation capacity exceeded",
                "skill_operation_capacity",
                "The local concurrent Skill installation capacity was exceeded.",
                true,
            ),
            LifecycleError::Conflict => Self::new(
                context,
                StatusCode::CONFLICT,
                "Skill catalog conflict",
                "skill_install_conflict",
                "The Skill name, manifest, or installed content changed before the operation committed.",
                false,
            ),
            LifecycleError::Storage => Self::new(
                context,
                StatusCode::SERVICE_UNAVAILABLE,
                "Skill storage unavailable",
                "skill_storage_unavailable",
                "The local Skill store could not complete the operation.",
                true,
            ),
        }
    }

    fn from_operation(context: RequestContext, error: &OperationError) -> Self {
        match error {
            OperationError::InvalidId => Self::new(
                context,
                StatusCode::BAD_REQUEST,
                "Invalid operation ID",
                "invalid_operation_id",
                "The operation ID does not match the supported opaque format.",
                false,
            ),
            OperationError::InvalidIdempotencyKey => Self::new(
                context,
                StatusCode::BAD_REQUEST,
                "Invalid idempotency key",
                "invalid_idempotency_key",
                "Idempotency-Key must be a single 8 to 128 character visible ASCII value.",
                false,
            ),
            OperationError::NotFound => Self::new(
                context,
                StatusCode::NOT_FOUND,
                "Operation not found",
                "resource_not_found",
                "The requested operation does not exist.",
                false,
            ),
            OperationError::IdempotencyConflict => Self::new(
                context,
                StatusCode::CONFLICT,
                "Idempotency conflict",
                "idempotency_conflict",
                "The idempotency key was already used for a different request.",
                false,
            ),
            OperationError::TransitionConflict => Self::new(
                context,
                StatusCode::CONFLICT,
                "Operation state conflict",
                "operation_state_conflict",
                "The operation is already in an incompatible terminal or running state.",
                false,
            ),
            OperationError::DataInvalid => Self::new(
                context,
                StatusCode::SERVICE_UNAVAILABLE,
                "Operation storage unavailable",
                "operation_data_invalid",
                "The local operation record could not be read safely.",
                true,
            ),
            OperationError::StorageUnavailable => Self::new(
                context,
                StatusCode::SERVICE_UNAVAILABLE,
                "Operation storage unavailable",
                "operation_storage_unavailable",
                "The backend could not access the local operation store.",
                true,
            ),
        }
    }

    pub(crate) fn from_memory(context: RequestContext, error: MemoryError) -> Self {
        match error {
            MemoryError::InvalidRequest { .. } => Self::new(
                context,
                StatusCode::BAD_REQUEST,
                "Invalid Memory request",
                "validation_failed",
                "The Memory request does not match the API contract.",
                false,
            ),
            MemoryError::Profile(error) => Self::from_profile(context, error),
            MemoryError::NotFound | MemoryError::InvalidMemoryId => Self::new(
                context,
                StatusCode::NOT_FOUND,
                "Memory item not found",
                "resource_not_found",
                "The requested Memory item does not exist in this target revision.",
                false,
            ),
            MemoryError::RevisionConflict { current_etag } => {
                let mut response = Self::new(
                    context,
                    StatusCode::CONFLICT,
                    "Memory revision conflict",
                    "revision_conflict",
                    "The Memory target changed after it was read. Reload it before saving.",
                    false,
                );
                response.etag = Some(current_etag);
                response
            }
            MemoryError::ProviderUnsupported { .. } => Self::new(
                context,
                StatusCode::UNPROCESSABLE_ENTITY,
                "Memory provider unsupported",
                "memory_provider_unsupported",
                "This Profile does not use the builtin Memory provider.",
                false,
            ),
            MemoryError::Threat { .. } => Self::new(
                context,
                StatusCode::UNPROCESSABLE_ENTITY,
                "Memory content blocked",
                "memory_content_blocked",
                "The Memory content matched a strict prompt-injection or exfiltration pattern.",
                false,
            ),
            MemoryError::ContentLimit { .. } => Self::new(
                context,
                StatusCode::UNPROCESSABLE_ENTITY,
                "Memory capacity exceeded",
                "memory_capacity_exceeded",
                "The normalized Memory target would exceed its configured character limit.",
                false,
            ),
            MemoryError::InvalidCursor => Self::new(
                context,
                StatusCode::BAD_REQUEST,
                "Invalid Memory cursor",
                "invalid_cursor",
                "The Memory cursor is invalid or belongs to different filters.",
                false,
            ),
            MemoryError::IdempotencyConflict => Self::new(
                context,
                StatusCode::CONFLICT,
                "Idempotency conflict",
                "idempotency_conflict",
                "The idempotency key was already used for a different Memory request.",
                false,
            ),
            MemoryError::IdempotencyResourceGone => Self::new(
                context,
                StatusCode::GONE,
                "Idempotent Memory item deleted",
                "idempotent_resource_deleted",
                "The Memory item created by this idempotency key was explicitly removed.",
                false,
            ),
            MemoryError::Drift { .. } => Self::new(
                context,
                StatusCode::CONFLICT,
                "Memory storage drift detected",
                "memory_storage_drift",
                "The Memory file cannot be rewritten losslessly. Resolve the external edit before retrying.",
                false,
            ),
            MemoryError::Disabled => Self::new(
                context,
                StatusCode::UNPROCESSABLE_ENTITY,
                "Memory disabled",
                "engine_capability_missing",
                "Memory is disabled for this Profile.",
                false,
            ),
            MemoryError::NoMatch { .. } | MemoryError::AmbiguousMatch { .. } => Self::new(
                context,
                StatusCode::CONFLICT,
                "Memory match conflict",
                "memory_match_conflict",
                "The Memory substring did not identify exactly one entry.",
                false,
            ),
            MemoryError::DataTooLarge | MemoryError::DataInvalid => Self::new(
                context,
                StatusCode::CONFLICT,
                "Memory storage invalid",
                "memory_storage_invalid",
                "The builtin Memory file cannot be read safely.",
                false,
            ),
            MemoryError::UnsafePath => Self::new(
                context,
                StatusCode::CONFLICT,
                "Unsafe profile path",
                "unsafe_profile_path",
                "The Profile Memory directory contains a symbolic link or unsafe path.",
                false,
            ),
            MemoryError::Storage(error) => {
                tracing::error!(error = ?error, "memory storage operation failed");
                Self::new(
                    context,
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Memory storage unavailable",
                    "memory_storage_unavailable",
                    "The backend could not read or persist builtin Memory safely.",
                    true,
                )
            }
        }
    }

    pub(crate) fn from_run(context: RequestContext, error: RunError) -> Self {
        match error {
            RunError::InvalidRequest | RunError::InvalidRunId => Self::new(
                context,
                StatusCode::BAD_REQUEST,
                "Invalid run request",
                "validation_failed",
                "The run request does not match the API contract.",
                false,
            ),
            RunError::InvalidApprovalRequest => Self::new(
                context,
                StatusCode::BAD_REQUEST,
                "Invalid approval request",
                "validation_failed",
                "The approval request does not match the API contract.",
                false,
            ),
            RunError::InvalidClarificationRequest => Self::new(
                context,
                StatusCode::BAD_REQUEST,
                "Invalid clarification request",
                "validation_failed",
                "The clarification request does not match the API contract.",
                false,
            ),
            RunError::ApprovalNotFound => Self::new(
                context,
                StatusCode::NOT_FOUND,
                "Approval not found",
                "approval_not_found",
                "The requested approval does not exist in this run.",
                false,
            ),
            RunError::ApprovalChoiceNotOffered => Self::new(
                context,
                StatusCode::CONFLICT,
                "Approval choice not offered",
                "approval_choice_not_offered",
                "The decision is not one of the choices offered for this approval.",
                false,
            ),
            RunError::ApprovalDecisionConflict => Self::new(
                context,
                StatusCode::CONFLICT,
                "Approval decision conflict",
                "approval_decision_conflict",
                "This approval was already resolved with a different decision payload.",
                false,
            ),
            RunError::ApprovalExpired => Self::new(
                context,
                StatusCode::CONFLICT,
                "Approval expired",
                "approval_expired",
                "The approval expired and was denied before this decision could be accepted.",
                false,
            ),
            RunError::ApprovalNoLongerPending => Self::new(
                context,
                StatusCode::CONFLICT,
                "Approval no longer pending",
                "approval_no_longer_pending",
                "The run no longer has this approval as its pending action.",
                false,
            ),
            RunError::ClarificationNotFound => Self::new(
                context,
                StatusCode::NOT_FOUND,
                "Clarification not found",
                "clarification_not_found",
                "The requested clarification does not exist in this run.",
                false,
            ),
            RunError::ClarificationChoiceNotOffered => Self::new(
                context,
                StatusCode::CONFLICT,
                "Clarification choice not offered",
                "clarification_choice_not_offered",
                "The answer is not one of the choices offered for this clarification.",
                false,
            ),
            RunError::ClarificationAnswerConflict => Self::new(
                context,
                StatusCode::CONFLICT,
                "Clarification answer conflict",
                "clarification_answer_conflict",
                "This clarification was already resolved with a different answer.",
                false,
            ),
            RunError::ClarificationNoLongerPending => Self::new(
                context,
                StatusCode::CONFLICT,
                "Clarification no longer pending",
                "clarification_no_longer_pending",
                "The run no longer has this clarification as its pending action.",
                false,
            ),
            RunError::InvalidEventId => Self::new(
                context,
                StatusCode::BAD_REQUEST,
                "Invalid event ID",
                "validation_failed",
                "Last-Event-ID does not identify an event in this run.",
                false,
            ),
            RunError::NotFound => Self::new(
                context,
                StatusCode::NOT_FOUND,
                "Run not found",
                "resource_not_found",
                "The requested run does not exist.",
                false,
            ),
            RunError::SessionBusy => Self::new(
                context,
                StatusCode::CONFLICT,
                "Session busy",
                "session_busy",
                "Wait for the current run to finish or cancel it before sending again.",
                false,
            ),
            RunError::SessionArchived => Self::new(
                context,
                StatusCode::CONFLICT,
                "Session archived",
                "session_archived",
                "Restore the session before creating a run.",
                false,
            ),
            RunError::CapacityExceeded => Self::new(
                context,
                StatusCode::TOO_MANY_REQUESTS,
                "Run capacity exceeded",
                "capacity_exceeded",
                "The local inference scheduler is at capacity.",
                true,
            ),
            RunError::IdempotencyConflict => Self::new(
                context,
                StatusCode::CONFLICT,
                "Idempotency conflict",
                "idempotency_conflict",
                "The idempotency key was already used for a different run request.",
                false,
            ),
            RunError::IdempotentResourceDeleted => Self::new(
                context,
                StatusCode::GONE,
                "Idempotent resource deleted",
                "idempotent_resource_deleted",
                "The session or user message for this idempotent run was deleted.",
                false,
            ),
            RunError::EventHistoryExpired => Self::new(
                context,
                StatusCode::CONFLICT,
                "Event history expired",
                "event_history_expired",
                "The requested run events are outside the retained replay window.",
                false,
            ),
            RunError::CapabilityMissing
            | RunError::ApprovalCapabilityMissing
            | RunError::ClarificationCapabilityMissing => Self::new(
                context,
                StatusCode::UNPROCESSABLE_ENTITY,
                "Engine capability missing",
                "engine_capability_missing",
                "This Rust engine slice does not support the requested run capability.",
                false,
            ),
            RunError::EngineUnavailable => Self::new(
                context,
                StatusCode::BAD_GATEWAY,
                "Inference engine unavailable",
                "engine_unavailable",
                "The selected model provider is not configured or supported.",
                true,
            ),
            RunError::SecretStorageUnavailable => Self::new(
                context,
                StatusCode::SERVICE_UNAVAILABLE,
                "Secret storage unavailable",
                "secret_storage_unavailable",
                "The OS keychain could not provide the selected provider credential.",
                true,
            ),
            RunError::StorageBusy => {
                let mut response = Self::new(
                    context,
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Session storage busy",
                    "session_storage_busy",
                    "The local session store remained locked past its bounded timeout.",
                    true,
                );
                response.retry_after_seconds = Some(1);
                response
            }
            RunError::StorageUnavailable | RunError::DataInvalid => Self::new(
                context,
                StatusCode::SERVICE_UNAVAILABLE,
                "Session storage unavailable",
                "session_storage_unavailable",
                "The local run journal could not be read or written safely.",
                true,
            ),
        }
    }

    pub(crate) fn from_hermes_v21(context: RequestContext, error: HermesV21Error) -> Self {
        match error {
            HermesV21Error::SnapshotTooLarge => Self::new(
                context,
                StatusCode::PAYLOAD_TOO_LARGE,
                "Hermes import is too large",
                "hermes_import_too_large",
                "The Hermes session snapshot exceeds the fixed local import limits.",
                false,
            ),
            HermesV21Error::UnsupportedSchemaVersion { .. } => Self::new(
                context,
                StatusCode::UNPROCESSABLE_ENTITY,
                "Unsupported Hermes schema",
                "hermes_schema_unsupported",
                "The Hermes state database does not use the supported schema version.",
                false,
            ),
            HermesV21Error::MissingTable { .. }
            | HermesV21Error::MissingColumn { .. }
            | HermesV21Error::MissingSchemaVersion
            | HermesV21Error::AmbiguousSchemaVersion
            | HermesV21Error::InvalidSchemaVersion
            | HermesV21Error::InvalidValue { .. }
            | HermesV21Error::FingerprintFailed => Self::new(
                context,
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid Hermes state database",
                "hermes_import_source_invalid",
                "The Hermes state database does not match the fixed import contract.",
                false,
            ),
            HermesV21Error::OpenFailed
            | HermesV21Error::InvalidDatabaseLocation
            | HermesV21Error::ReadOnlyEnforcementFailed
            | HermesV21Error::TransactionFailed => Self::new(
                context,
                StatusCode::SERVICE_UNAVAILABLE,
                "Hermes state unavailable",
                "hermes_state_unavailable",
                "The Hermes state database could not be read safely in read-only mode.",
                true,
            ),
        }
    }

    pub(crate) fn from_hermes_import(context: RequestContext, error: HermesImportError) -> Self {
        match error {
            HermesImportError::InvalidRequest => Self::new(
                context,
                StatusCode::BAD_REQUEST,
                "Invalid Hermes import request",
                "validation_failed",
                "The Hermes import request does not match the API contract.",
                false,
            ),
            HermesImportError::IdempotencyConflict => Self::new(
                context,
                StatusCode::CONFLICT,
                "Idempotency conflict",
                "idempotency_conflict",
                "The idempotency key was already used for a different import request.",
                false,
            ),
            HermesImportError::SourceChanged => Self::new(
                context,
                StatusCode::CONFLICT,
                "Hermes import source changed",
                "hermes_import_source_changed",
                "The Hermes state snapshot changed after preview. Preview it again before importing.",
                false,
            ),
            HermesImportError::AttachmentsRequirePolicy => Self::new(
                context,
                StatusCode::UNPROCESSABLE_ENTITY,
                "Hermes attachments require a policy",
                "hermes_attachments_require_policy",
                "The snapshot contains attachments. Explicitly allow attachment omission before importing.",
                false,
            ),
            HermesImportError::SourceInvalid => Self::new(
                context,
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid Hermes import source",
                "hermes_import_source_invalid",
                "The Hermes snapshot contains data that cannot be mapped safely.",
                false,
            ),
            HermesImportError::Conflict(report) => {
                let mut response = Self::new(
                    context,
                    StatusCode::CONFLICT,
                    "Hermes import conflict",
                    "hermes_import_conflict",
                    "No data was imported because the source mapping or target Sessions changed.",
                    false,
                );
                response.import_conflicts = Some(Box::new(report));
                response
            }
            HermesImportError::StorageBusy => {
                let mut response = Self::new(
                    context,
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Session storage busy",
                    "session_storage_busy",
                    "The local session store remained locked past its bounded timeout.",
                    true,
                );
                response.retry_after_seconds = Some(1);
                response
            }
            HermesImportError::StorageUnavailable => Self::new(
                context,
                StatusCode::SERVICE_UNAVAILABLE,
                "Session storage unavailable",
                "session_storage_unavailable",
                "The local session store could not persist the Hermes import safely.",
                true,
            ),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (conflict_count, conflicts, conflicts_dropped) = self
            .import_conflicts
            .map(|report| {
                (
                    Some(report.conflict_count),
                    Some(report.conflicts),
                    Some(report.conflicts_dropped),
                )
            })
            .unwrap_or((None, None, None));
        let problem = Problem {
            problem_type: format!("urn:synthchat:error:{}", self.code),
            title: self.title,
            status: self.status.as_u16(),
            detail: self.detail,
            instance: self.context.instance,
            code: self.code,
            request_id: self.context.request_id,
            retryable: self.retryable,
            conflict_count,
            conflicts,
            conflicts_dropped,
        };
        let mut response = (self.status, Json(problem)).into_response();
        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        if let Some(etag) = self.etag
            && let Ok(value) = HeaderValue::from_str(&etag)
        {
            response
                .headers_mut()
                .insert(axum::http::header::ETAG, value);
        }
        if let Some(seconds) = self.retry_after_seconds
            && let Ok(value) = HeaderValue::from_str(&seconds.to_string())
        {
            response
                .headers_mut()
                .insert(axum::http::header::RETRY_AFTER, value);
        }
        response
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Problem {
    #[serde(rename = "type")]
    problem_type: String,
    title: &'static str,
    status: u16,
    detail: &'static str,
    instance: String,
    code: &'static str,
    request_id: String,
    retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    conflict_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    conflicts: Option<Vec<crate::sessions::HermesImportConflict>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    conflicts_dropped: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_errors_have_stable_statuses_and_codes() {
        let cases = [
            (
                RunError::ApprovalNotFound,
                StatusCode::NOT_FOUND,
                "approval_not_found",
            ),
            (
                RunError::ApprovalChoiceNotOffered,
                StatusCode::CONFLICT,
                "approval_choice_not_offered",
            ),
            (
                RunError::ApprovalDecisionConflict,
                StatusCode::CONFLICT,
                "approval_decision_conflict",
            ),
            (
                RunError::ApprovalExpired,
                StatusCode::CONFLICT,
                "approval_expired",
            ),
            (
                RunError::ApprovalNoLongerPending,
                StatusCode::CONFLICT,
                "approval_no_longer_pending",
            ),
        ];

        for (run_error, status, code) in cases {
            let error = ApiError::from_run(
                RequestContext::new(RequestId("request".to_owned()), "/approval"),
                run_error,
            );
            assert_eq!(error.status, status);
            assert_eq!(error.code, code);
        }
    }

    #[test]
    fn local_skill_capacity_is_distinct_from_upstream_rate_limiting() {
        let error = ApiError::from_lifecycle(
            RequestContext::new(RequestId("request".to_owned()), "/skills/install"),
            LifecycleError::OperationCapacity,
        );
        assert_eq!(error.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(error.code, "skill_operation_capacity");
        assert!(error.retryable);
    }
}
