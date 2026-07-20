mod cursor;
mod import;
pub(crate) mod process_store;
mod run_store;
mod schema;
mod store;
mod workspace_store;

#[cfg(test)]
mod tests;

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

pub use import::{
    HermesImportConflict, HermesImportConflictCode, HermesImportConflictReport,
    HermesImportDisposition, HermesImportError, HermesImportWarningSummary, HermesV21ImportPreview,
    HermesV21ImportRequest, HermesV21ImportResult,
};
pub use store::SessionService;

pub const SESSION_SCHEMA_VERSION: u32 = 12;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RuntimeLease {
    pub owner_id: String,
    pub epoch: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RuntimeLeaseState {
    Unmanaged,
    Held(RuntimeLease),
    Fenced,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub id: String,
    pub profile_id: String,
    pub display_name: String,
    pub available: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ToolApprovalDecision {
    Once,
    Session,
    Always,
    Deny,
}

impl ToolApprovalDecision {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::Session => "session",
            Self::Always => "always",
            Self::Deny => "deny",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolApprovalState {
    Pending,
    Resolved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolApprovalResolvedBy {
    User,
    Expiry,
    Cancellation,
}

impl ToolApprovalResolvedBy {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Expiry => "expiry",
            Self::Cancellation => "cancellation",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ToolApprovalRequest {
    pub approval_id: String,
    pub call_id: String,
    pub tool_name: String,
    pub input_summary: Option<String>,
    pub choices: Vec<ToolApprovalDecision>,
    pub expires_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ToolApprovalExecutionBinding {
    pub run_id: String,
    pub profile_id: String,
    pub session_id: String,
    pub workspace_id: Option<String>,
    pub call_id: String,
    pub tool_name: String,
    pub invocation_checkpoint: u64,
    pub arguments_sha256: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StoredToolApproval {
    pub approval_id: String,
    pub run_id: String,
    pub profile_id: String,
    pub session_id: String,
    pub workspace_id: Option<String>,
    pub call_id: String,
    pub invocation_checkpoint: u64,
    pub tool_name: String,
    pub arguments_sha256: [u8; 32],
    pub input_summary: Option<String>,
    pub choices: Vec<ToolApprovalDecision>,
    pub expires_at: String,
    pub expires_at_unix_ms: i64,
    pub state: ToolApprovalState,
    pub decision: Option<ToolApprovalDecision>,
    pub reason: Option<String>,
    pub resolved_by: Option<ToolApprovalResolvedBy>,
    pub created_at: String,
    pub resolved_at: Option<String>,
    pub execution_claimed_at: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolApprovalResolutionDisposition {
    Accepted,
    Replayed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ToolApprovalResolution {
    pub approval: StoredToolApproval,
    pub disposition: ToolApprovalResolutionDisposition,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum ToolApprovalError {
    #[error("invalid tool approval request")]
    InvalidRequest,
    #[error("tool approval not found")]
    NotFound,
    #[error("tool approval request conflicts with its immutable record")]
    RequestConflict,
    #[error("the approval choice was not offered")]
    ChoiceNotOffered,
    #[error("the approval decision conflicts with its immutable record")]
    DecisionConflict,
    #[error("the tool approval expired")]
    Expired,
    #[error("the tool approval is no longer pending")]
    NoLongerPending,
    #[error("the tool approval has not expired")]
    NotExpired,
    #[error("the tool approval does not grant an execution claim")]
    ExecutionNotAuthorized,
    #[error("the tool approval execution claim was already consumed")]
    ExecutionAlreadyClaimed,
    #[error("session storage is busy")]
    StorageBusy,
    #[error("session storage is unavailable")]
    StorageUnavailable,
    #[error("tool approval data is invalid")]
    DataInvalid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClarificationState {
    Pending,
    Resolved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClarificationResolvedBy {
    User,
    Cancellation,
    Failure,
}

impl ClarificationResolvedBy {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Cancellation => "cancellation",
            Self::Failure => "failure",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClarificationRequest {
    pub request_id: String,
    pub call_id: String,
    pub question: String,
    pub choices: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClarificationContinuationBinding {
    pub run_id: String,
    pub call_id: String,
    pub invocation_checkpoint: u64,
    pub arguments_sha256: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StoredClarification {
    pub request_id: String,
    pub run_id: String,
    pub call_id: String,
    pub invocation_checkpoint: u64,
    pub arguments_sha256: [u8; 32],
    pub question: String,
    pub choices: Vec<String>,
    pub state: ClarificationState,
    pub answer: Option<String>,
    pub resolved_by: Option<ClarificationResolvedBy>,
    pub created_at: String,
    pub resolved_at: Option<String>,
    pub continuation_claimed_at: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClarificationResolutionDisposition {
    Accepted,
    Replayed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClarificationResolution {
    pub clarification: StoredClarification,
    pub disposition: ClarificationResolutionDisposition,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum ClarificationError {
    #[error("invalid clarification request")]
    InvalidRequest,
    #[error("clarification request not found")]
    NotFound,
    #[error("clarification request conflicts with its immutable record")]
    RequestConflict,
    #[error("the clarification answer was not an offered choice")]
    ChoiceNotOffered,
    #[error("the clarification answer conflicts with its immutable record")]
    AnswerConflict,
    #[error("the clarification is no longer pending")]
    NoLongerPending,
    #[error("the clarification answer does not grant a continuation claim")]
    ContinuationNotAuthorized,
    #[error("the clarification continuation claim was already consumed")]
    ContinuationAlreadyClaimed,
    #[error("session storage is busy")]
    StorageBusy,
    #[error("session storage is unavailable")]
    StorageUnavailable,
    #[error("clarification data is invalid")]
    DataInvalid,
}

// This journal boundary is consumed by the next RunService slice. The
// non-test build keeps it internal until that wiring lands, while unit tests
// exercise the complete persistence and recovery contract now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProviderTurnFinish {
    Stop,
    ToolCalls,
    Length,
    ContentFilter,
}

impl ProviderTurnFinish {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::ToolCalls => "toolCalls",
            Self::Length => "length",
            Self::ContentFilter => "contentFilter",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RawToolCallPlan {
    pub call_id: String,
    pub tool_name: String,
    pub arguments_json: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ProviderTurnPlan {
    pub turn_index: u32,
    pub assistant_message_id: String,
    pub content: Option<String>,
    pub reasoning: Option<String>,
    pub finish: ProviderTurnFinish,
    pub usage: Usage,
    pub tool_calls: Vec<RawToolCallPlan>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CompleteRunPlan {
    pub message_id: String,
    pub text: String,
    pub reasoning: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Usage,
    pub model_label: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolInvocationStatus {
    Planned,
    Running,
    Completed,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolInvocationOrigin {
    Provider,
    CodeRpc,
}

impl ToolInvocationStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    pub(crate) fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StoredToolInvocation {
    pub run_id: String,
    pub turn_index: u32,
    pub call_index: u32,
    pub call_id: String,
    pub tool_name: String,
    pub arguments_json: String,
    pub status: ToolInvocationStatus,
    pub attempt: u32,
    pub checkpoint: u64,
    pub result_json: Option<String>,
    pub error_json: Option<String>,
    pub provider_content: Option<String>,
    pub origin: ToolInvocationOrigin,
    pub parent_call_id: Option<String>,
    pub rpc_sequence: Option<u32>,
    pub planned_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub updated_at: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StoredProviderTurn {
    pub run_id: String,
    pub turn_index: u32,
    pub assistant_message_id: String,
    pub content: Option<String>,
    pub reasoning: Option<String>,
    pub finish: ProviderTurnFinish,
    pub usage: Usage,
    pub created_at: String,
    pub tool_calls: Vec<StoredToolInvocation>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ProviderContextMessage {
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant {
        content: Option<String>,
        tool_calls: Vec<RawToolCallPlan>,
    },
    Tool {
        tool_call_id: String,
        content: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchMode {
    Fts5,
    Like,
    Unavailable,
}

impl SearchMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fts5 => "fts5",
            Self::Like => "like",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Clone)]
pub(crate) enum StorageState {
    Ready(StorageReady),
    Unavailable,
}

#[derive(Clone)]
pub(crate) struct StorageReady {
    pub(crate) db_path: Arc<PathBuf>,
    pub(crate) search_mode: SearchMode,
    pub(crate) write_lock: Arc<Mutex<()>>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    pub profile_id: String,
    pub title: String,
    pub preview: String,
    pub source: String,
    pub model: String,
    pub message_count: u64,
    pub archived: bool,
    pub revision: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(rename = "match")]
    pub search_match: Option<SearchMatch>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SearchMatch {
    pub field: SearchField,
    pub message_id: Option<String>,
    pub snippet: String,
    pub ranges: Vec<SearchRange>,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SearchField {
    Title,
    Id,
    Message,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SearchRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateSession {
    pub profile_id: String,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionPatch {
    #[serde(default)]
    pub title: PatchField<String>,
    #[serde(default)]
    pub archived: PatchField<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum PatchField<T> {
    #[default]
    Missing,
    Value(T),
}

impl<'de, T> Deserialize<'de> for PatchField<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        T::deserialize(deserializer).map(Self::Value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListSessions {
    pub profile_id: String,
    pub query: Option<String>,
    pub archived: bool,
    pub cursor: Option<String>,
    pub limit: usize,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionPage {
    pub items: Vec<Session>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Tool,
}

impl MessageRole {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::System => "system",
            Self::Tool => "tool",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum MessagePart {
    Text {
        text: String,
    },
    File {
        #[serde(rename = "fileId")]
        file_id: String,
        name: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ToolCallStatus {
    Unknown,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolCall {
    pub call_id: String,
    pub name: String,
    pub status: ToolCallStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<FileRef>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileRef {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
    pub created_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: String,
    pub session_id: String,
    pub sequence: u64,
    pub role: MessageRole,
    pub parts: Vec<MessagePart>,
    pub reasoning: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Option<Usage>,
    pub created_at: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CommitMessage {
    pub role: MessageRole,
    pub parts: Vec<MessagePart>,
    pub reasoning: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Option<Usage>,
    pub model: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListMessages {
    pub cursor: Option<String>,
    pub limit: usize,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MessagePage {
    pub items: Vec<Message>,
    pub next_cursor: Option<String>,
    pub snapshot_last_sequence: u64,
    pub first_sequence: Option<u64>,
    pub last_sequence: Option<u64>,
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("invalid session request")]
    InvalidRequest,
    #[error("invalid session id")]
    InvalidSessionId,
    #[error("invalid cursor")]
    InvalidCursor,
    #[error("session not found")]
    NotFound,
    #[error("session revision conflict")]
    RevisionConflict { current_etag: String },
    #[error("a session precondition is required")]
    PreconditionRequired,
    #[error("idempotency key conflict")]
    IdempotencyConflict,
    #[error("the idempotent session was deleted")]
    IdempotentResourceDeleted,
    #[error("the session is archived")]
    Archived,
    #[error("the session has a non-terminal run")]
    Busy,
    #[error("session search is unavailable")]
    SearchUnavailable,
    #[error("session storage is busy")]
    StorageBusy,
    #[error("session storage is unavailable")]
    StorageUnavailable,
    #[error("session data is invalid")]
    DataInvalid,
    #[error("workspace path is invalid or unavailable")]
    InvalidWorkspacePath,
    #[error("workspace is referenced by a run")]
    WorkspaceInUse,
}
