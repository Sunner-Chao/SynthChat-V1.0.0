mod service;
mod task_registry;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use thiserror::Error;

use crate::sessions::{Message, Usage};

pub use service::RunService;

pub(crate) const MAX_RUN_EVENTS: usize = 2_048;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatInput {
    pub text: String,
    pub file_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunModelConfig {
    pub provider: String,
    pub model: String,
    pub base_url: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateRun {
    pub client_request_id: String,
    pub message: ChatInput,
    #[serde(default)]
    pub model_override: Option<RunModelConfig>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub workspace_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalChoice {
    Once,
    Session,
    Always,
    Deny,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApprovalDecision {
    pub decision: ApprovalChoice,
    #[serde(default)]
    pub reason: Option<String>,
}

impl ApprovalDecision {
    pub(crate) fn validate(&self) -> Result<(), RunError> {
        if self
            .reason
            .as_ref()
            .is_some_and(|reason| reason.chars().count() > 2_000)
        {
            return Err(RunError::InvalidApprovalRequest);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClarificationAnswer {
    pub answer: String,
}

impl ClarificationAnswer {
    pub(crate) fn validate(&self) -> Result<(), RunError> {
        if !(1..=10_000).contains(&self.answer.chars().count()) {
            return Err(RunError::InvalidClarificationRequest);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionAccepted {
    pub accepted: bool,
}

impl ActionAccepted {
    pub const fn accepted() -> Self {
        Self { accepted: true }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RunStatus {
    Queued,
    Running,
    WaitingApproval,
    WaitingClarification,
    Cancelling,
    Completed,
    Cancelled,
    Failed,
}

impl RunStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::WaitingApproval => "waitingApproval",
            Self::WaitingClarification => "waitingClarification",
            Self::Cancelling => "cancelling",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Failed)
    }
}

impl TryFrom<&str> for RunStatus {
    type Error = RunError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "waitingApproval" => Ok(Self::WaitingApproval),
            "waitingClarification" => Ok(Self::WaitingClarification),
            "cancelling" => Ok(Self::Cancelling),
            "completed" => Ok(Self::Completed),
            "cancelled" => Ok(Self::Cancelled),
            "failed" => Ok(Self::Failed),
            _ => Err(RunError::DataInvalid),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum PendingAction {
    Approval {
        approval_id: String,
        call_id: String,
        tool_name: String,
        input_summary: Option<String>,
        choices: Vec<String>,
        expires_at: String,
    },
    Clarification {
        request_id: String,
        question: String,
        choices: Vec<String>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunProblem {
    #[serde(rename = "type")]
    pub problem_type: String,
    pub title: String,
    pub status: u16,
    pub detail: Option<String>,
    pub instance: Option<String>,
    pub code: String,
    pub request_id: String,
    pub retryable: bool,
}

impl RunProblem {
    pub(crate) fn tool(run_id: &str, call_id: &str) -> Self {
        Self {
            problem_type: "urn:synthchat:error:tool_failed".to_owned(),
            title: "Tool execution failed".to_owned(),
            status: 422,
            detail: Some("The local Rust tool could not complete this call.".to_owned()),
            instance: Some(format!("/api/v1/runs/{run_id}")),
            code: "tool_failed".to_owned(),
            request_id: format!("tool:{call_id}"),
            retryable: false,
        }
    }

    pub(crate) fn engine(run_id: &str, error: &crate::providers::ProviderError) -> Self {
        let (status, title, code, detail, retryable) = match error {
            crate::providers::ProviderError::Timeout => (
                504,
                "Inference timed out",
                "engine_timeout",
                "The model provider did not finish within the allowed time.",
                true,
            ),
            crate::providers::ProviderError::Cancelled => (
                499,
                "Inference cancelled",
                "run_cancelled",
                "The run was cancelled.",
                false,
            ),
            crate::providers::ProviderError::Unavailable
            | crate::providers::ProviderError::InvalidResponse => (
                502,
                "Inference unavailable",
                "engine_unavailable",
                "The model provider could not complete the request.",
                true,
            ),
        };
        Self {
            problem_type: format!("urn:synthchat:error:{code}"),
            title: title.to_owned(),
            status,
            detail: Some(detail.to_owned()),
            instance: Some(format!("/api/v1/runs/{run_id}")),
            code: code.to_owned(),
            request_id: format!("run:{run_id}"),
            retryable,
        }
    }

    pub(crate) fn backend_restarted(run_id: &str) -> Self {
        Self {
            problem_type: "urn:synthchat:error:engine_unavailable".to_owned(),
            title: "Run interrupted".to_owned(),
            status: 502,
            detail: Some(
                "The local backend restarted before the run reached a terminal state.".to_owned(),
            ),
            instance: Some(format!("/api/v1/runs/{run_id}")),
            code: "engine_unavailable".to_owned(),
            request_id: format!("run:{run_id}"),
            retryable: true,
        }
    }

    pub(crate) fn local_failure(run_id: &str) -> Self {
        Self {
            problem_type: "urn:synthchat:error:engine_unavailable".to_owned(),
            title: "Run failed".to_owned(),
            status: 502,
            detail: Some("The local Rust inference engine could not finish the run.".to_owned()),
            instance: Some(format!("/api/v1/runs/{run_id}")),
            code: "engine_unavailable".to_owned(),
            request_id: format!("run:{run_id}"),
            retryable: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Run {
    pub id: String,
    pub session_id: String,
    pub profile_id: String,
    pub status: RunStatus,
    pub last_sequence: u64,
    pub message_id: Option<String>,
    pub usage: Option<Usage>,
    pub error: Option<RunProblem>,
    pub pending_action: Option<PendingAction>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActiveRun {
    pub run: Run,
    pub queue_item_id: Option<String>,
    pub user_message: Message,
    pub session_revision: String,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActiveRunList {
    pub items: Vec<ActiveRun>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RunDisposition {
    Started,
    Queued,
    Replayed,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunAccepted {
    pub run: Run,
    pub disposition: RunDisposition,
    pub queue_item_id: Option<String>,
    pub user_message: Message,
    pub session_revision: String,
}

#[derive(Clone, Debug)]
pub(crate) struct QueuedRunClaim {
    pub run: Run,
    pub request: CreateRun,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RunEventRecord {
    pub sequence: u64,
    pub event_name: String,
    pub envelope_json: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RunEventBatch {
    pub events: Vec<RunEventRecord>,
    pub last_sequence: u64,
    pub terminal: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CancelDisposition {
    SignalExecutor,
    CancelledQueued,
    AlreadyTerminal,
}

#[derive(Debug, Error)]
pub enum RunError {
    #[error("invalid run request")]
    InvalidRequest,
    #[error("invalid run id")]
    InvalidRunId,
    #[error("invalid approval request")]
    InvalidApprovalRequest,
    #[error("invalid clarification request")]
    InvalidClarificationRequest,
    #[error("approval not found")]
    ApprovalNotFound,
    #[error("the approval choice was not offered")]
    ApprovalChoiceNotOffered,
    #[error("the approval decision conflicts with its immutable record")]
    ApprovalDecisionConflict,
    #[error("the approval expired")]
    ApprovalExpired,
    #[error("the approval is no longer pending")]
    ApprovalNoLongerPending,
    #[error("clarification not found")]
    ClarificationNotFound,
    #[error("the clarification answer was not an offered choice")]
    ClarificationChoiceNotOffered,
    #[error("the clarification answer conflicts with its immutable record")]
    ClarificationAnswerConflict,
    #[error("the clarification is no longer pending")]
    ClarificationNoLongerPending,
    #[error("invalid event id")]
    InvalidEventId,
    #[error("run not found")]
    NotFound,
    #[error("the session has an active run")]
    SessionBusy,
    #[error("the session is archived")]
    SessionArchived,
    #[error("run capacity exceeded")]
    CapacityExceeded,
    #[error("idempotency key conflict")]
    IdempotencyConflict,
    #[error("the idempotent resource was deleted")]
    IdempotentResourceDeleted,
    #[error("event history expired")]
    EventHistoryExpired,
    #[error("the requested capability is unavailable")]
    CapabilityMissing,
    #[error("approval capability is unavailable")]
    ApprovalCapabilityMissing,
    #[error("clarification capability is unavailable")]
    ClarificationCapabilityMissing,
    #[error("inference engine is unavailable")]
    EngineUnavailable,
    #[error("secret storage is unavailable")]
    SecretStorageUnavailable,
    #[error("session storage is busy")]
    StorageBusy,
    #[error("session storage is unavailable")]
    StorageUnavailable,
    #[error("run data is invalid")]
    DataInvalid,
}

pub(crate) fn event_data<T: Serialize>(value: T) -> Result<JsonValue, RunError> {
    serde_json::to_value(value).map_err(|_| RunError::DataInvalid)
}
