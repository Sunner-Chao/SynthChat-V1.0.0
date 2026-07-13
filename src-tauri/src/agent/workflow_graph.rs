use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::{
    error::AppResult,
    models::{now_iso, AgentRunPhaseRecord, AgentRunRecord, SendChatRequest, ToolDefinition},
    store::AppStore,
};

use super::{
    decision_parser::{
        validated_tool_calls_from_decision_with_error, AgentToolCall, ToolCallValidationErrorKind,
    },
    tool_policy::is_internal_tool,
    tool_registry::resolve_mcp_tool,
    ToolExecutionContext,
};

pub(super) const SYNTHGRAPH_WORKFLOW_SCHEMA: &str = "synthgraph_workflow_v1";
pub(super) const SYNTHCHAT_HUMAN_GATE_SCHEMA: &str = "synthchat_human_gate_v1";
pub(super) const WORKFLOW_RUNTIME_SOURCE: &str = "agent_run.workflow_graph";
pub(super) const WORKFLOW_PHASE_INITIALIZED: &str = "workflow_graph_initialized";
pub(super) const WORKFLOW_PHASE_NODE: &str = "workflow_node";
pub(super) const WORKFLOW_PHASE_TRANSITION: &str = "workflow_transition";
pub(super) const WORKFLOW_RUNTIME_KIND_SNAPSHOT: &str = "workflow_snapshot";
pub(super) const WORKFLOW_RUNTIME_KIND_TRANSITION: &str = WORKFLOW_PHASE_TRANSITION;
pub(super) const WORKFLOW_RUNTIME_NODE_KIND_PREFIX: &str = "workflow_node_";
pub(super) const WORKFLOW_API_EVENT_SNAPSHOT: &str = "workflow.snapshot";
pub(super) const WORKFLOW_API_EVENT_NODE_PREFIX: &str = "workflow.node.";
pub(super) const WORKFLOW_API_EVENT_NODE_TEMPLATE: &str = "workflow.node.<status>";
pub(super) const WORKFLOW_API_EVENT_TRANSITION: &str = "workflow.transition";
pub(super) const WORKFLOW_INTERNAL_TOOL_SERVER_ID: &str = "__internal";
pub(super) const WORKFLOW_REASON_QUEUED_TURN: &str = "queued_turn";
pub(super) const WORKFLOW_REASON_DIRECT_TURN: &str = "direct_turn";
pub(super) const WORKFLOW_REASON_GROUP_CONTEXT_READY: &str = "group_context_ready";
pub(super) const WORKFLOW_REASON_NO_GROUP_ROOM_CONTEXT: &str = "no_group_room_context";
pub(super) const WORKFLOW_REASON_TOOL_CALLS: &str = "tool_calls";
pub(super) const WORKFLOW_REASON_TOOL_OBSERVATIONS_RECORDED: &str = "tool_observations_recorded";
pub(super) const WORKFLOW_REASON_APPROVAL_REQUIRED: &str = "approval_required";
pub(super) const WORKFLOW_REASON_APPROVAL_RESUMED: &str = "approval_resumed";
pub(super) const WORKFLOW_REASON_CLARIFY_REQUIRES_USER_INPUT: &str = "clarify_requires_user_input";
pub(super) const WORKFLOW_REASON_FUTURE_CHECKPOINT_WAIT: &str = "future_checkpoint_wait";
pub(super) const WORKFLOW_REASON_RESUME_CHECKPOINT_REQUESTED: &str = "resume_checkpoint_requested";
pub(super) const WORKFLOW_REASON_RESUME_CHECKPOINT_CONTINUED: &str = "resume_checkpoint_continued";
pub(super) const WORKFLOW_REASON_FINAL_ANSWER_CANDIDATE: &str = "final_answer_candidate";
pub(super) const WORKFLOW_REASON_COMPLETION_GATE_PASSED: &str = "completion_gate_passed";
pub(super) const WORKFLOW_REASON_DELEGATE_TASK_STARTED: &str = "delegate_task_started";
pub(super) const WORKFLOW_REASON_DELEGATE_TASK_COMPLETED: &str = "delegate_task_completed";
pub(super) const WORKFLOW_REASON_DELEGATE_TASK_FAILED: &str = "delegate_task_failed";
pub(super) const WORKFLOW_REASON_ORDER: &[&str] = &[
    WORKFLOW_REASON_QUEUED_TURN,
    WORKFLOW_REASON_DIRECT_TURN,
    WORKFLOW_REASON_GROUP_CONTEXT_READY,
    WORKFLOW_REASON_NO_GROUP_ROOM_CONTEXT,
    WORKFLOW_REASON_TOOL_CALLS,
    WORKFLOW_REASON_TOOL_OBSERVATIONS_RECORDED,
    WORKFLOW_REASON_APPROVAL_REQUIRED,
    WORKFLOW_REASON_APPROVAL_RESUMED,
    WORKFLOW_REASON_CLARIFY_REQUIRES_USER_INPUT,
    WORKFLOW_REASON_FUTURE_CHECKPOINT_WAIT,
    WORKFLOW_REASON_RESUME_CHECKPOINT_REQUESTED,
    WORKFLOW_REASON_RESUME_CHECKPOINT_CONTINUED,
    WORKFLOW_REASON_FINAL_ANSWER_CANDIDATE,
    WORKFLOW_REASON_COMPLETION_GATE_PASSED,
    WORKFLOW_REASON_DELEGATE_TASK_STARTED,
    WORKFLOW_REASON_DELEGATE_TASK_COMPLETED,
    WORKFLOW_REASON_DELEGATE_TASK_FAILED,
];
pub(super) const WORKFLOW_NODE_ORDER: &[&str] = &[
    "queue",
    "group_room",
    "planner",
    "executor",
    "approval",
    "checkpoint",
    "completion_gate",
    "reviewer",
];
pub(super) const WORKFLOW_STATUS_ORDER: &[&str] = &[
    "failed",
    "canceled",
    "waiting",
    "running",
    "pending",
    "completed",
    "skipped",
];
pub(super) const WORKFLOW_DETAIL_ALIAS_PAIRS: &[(&str, &str)] = &[
    ("requestSource", "request_source"),
    ("toolContext", "tool_context"),
    ("queueItemId", "queue_item_id"),
    ("queueStatus", "queue_status"),
    ("queueLifecycle", "queue_lifecycle"),
    ("preserveCurrent", "preserve_current"),
    ("conversationKind", "conversation_kind"),
    ("roomId", "room_id"),
    ("channelId", "channel_id"),
    ("chatId", "chat_id"),
    ("threadId", "thread_id"),
    ("groupId", "group_id"),
    ("humanGate", "human_gate"),
    ("runId", "run_id"),
    ("callId", "call_id"),
    ("requiresUserInput", "requires_user_input"),
    ("approvalId", "approval_id"),
    ("checkpointId", "checkpoint_id"),
    ("checkpointScope", "checkpoint_scope"),
    ("checkpointState", "checkpoint_state"),
    ("checkpointSummary", "checkpoint_summary"),
    ("checkpointIteration", "checkpoint_iteration"),
    ("previousState", "previous_state"),
    ("runState", "run_state"),
    ("mutationKind", "mutation_kind"),
    ("targetSummary", "target_summary"),
    ("toolCount", "tool_count"),
    ("toolProtocol", "tool_protocol"),
    ("toolOrigins", "tool_origins"),
    ("toolCallIds", "tool_call_ids"),
    ("toolCalls", "tool_calls"),
    ("providerNative", "provider_native"),
    ("requestedName", "requested_name"),
    ("serverId", "server_id"),
    ("toolName", "tool_name"),
    ("toolKind", "tool_kind"),
    ("sourceLabel", "source_label"),
    ("definitionName", "definition_name"),
    ("requiresApproval", "requires_approval"),
    ("directBridge", "direct_bridge"),
    ("approvedToolCallReplay", "approved_tool_call_replay"),
    ("bridgeStatus", "bridge_status"),
    ("bridgeRejectionReason", "bridge_rejection_reason"),
    ("bridgeStage", "bridge_stage"),
    ("lastBridgeTarget", "last_bridge_target"),
    ("messageId", "message_id"),
    ("providerId", "provider_id"),
    ("errorKind", "error_kind"),
    ("timeoutSeconds", "timeout_seconds"),
    ("requestedChildren", "requested_children"),
    ("existingChildren", "existing_children"),
    ("parentDepth", "parent_depth"),
    ("childDepth", "child_depth"),
    ("maxSubagents", "max_subagents"),
    ("maxSubagentDepth", "max_subagent_depth"),
    ("maxConcurrentChildren", "max_concurrent_children"),
    ("orchestratorEnabled", "orchestrator_enabled"),
    ("subagentAutoApprove", "subagent_auto_approve"),
    ("inheritMcpToolsets", "inherit_mcp_toolsets"),
    ("completedChildren", "completed_children"),
    ("failedChildren", "failed_children"),
    ("abortedChildren", "aborted_children"),
    ("unknownChildren", "unknown_children"),
    ("childIndex", "child_index"),
    ("taskPreview", "task_preview"),
    ("canDelegate", "can_delegate"),
    ("maxIterations", "max_iterations"),
    ("acpCommand", "acp_command"),
    ("acpSessionMode", "acp_session_mode"),
    ("childRunId", "child_run_id"),
    ("childConversationId", "child_conversation_id"),
    ("resultPreview", "result_preview"),
    ("errorPreview", "error_preview"),
    ("hasDiagnosticArtifact", "has_diagnostic_artifact"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum WorkflowMode {
    ChatTurn,
    ApprovalContinuation,
}

impl WorkflowMode {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::ChatTurn => "chat_turn",
            Self::ApprovalContinuation => "approval_continuation",
        }
    }

    pub(super) fn from_str(value: &str) -> Option<Self> {
        match value {
            "chat_turn" => Some(Self::ChatTurn),
            "approval_continuation" => Some(Self::ApprovalContinuation),
            _ => None,
        }
    }
}

pub(super) fn workflow_mode_for_run(run: &AgentRunRecord) -> WorkflowMode {
    run.workflow_graph
        .as_ref()
        .and_then(workflow_mode_from_current_node)
        .or_else(|| {
            run.workflow_graph
                .as_ref()
                .and_then(|graph| graph.get("mode"))
                .and_then(Value::as_str)
                .and_then(WorkflowMode::from_str)
        })
        .unwrap_or(WorkflowMode::ChatTurn)
}

fn workflow_mode_from_current_node(graph: &Value) -> Option<WorkflowMode> {
    let current_node = graph
        .get("currentNode")
        .or_else(|| graph.get("current_node"))
        .and_then(Value::as_str)
        .filter(|node| !node.trim().is_empty())?;
    graph
        .get("nodes")
        .and_then(Value::as_array)?
        .iter()
        .find(|node| node.get("node").and_then(Value::as_str) == Some(current_node))
        .and_then(|node| node.get("detail"))
        .and_then(|detail| detail.get("mode"))
        .and_then(Value::as_str)
        .and_then(WorkflowMode::from_str)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum WorkflowNodeName {
    Queue,
    Planner,
    CompletionGate,
    Executor,
    Reviewer,
    Approval,
    Checkpoint,
    GroupRoom,
}

impl WorkflowNodeName {
    fn as_str(self) -> &'static str {
        match self {
            Self::Queue => "queue",
            Self::Planner => "planner",
            Self::CompletionGate => "completion_gate",
            Self::Executor => "executor",
            Self::Reviewer => "reviewer",
            Self::Approval => "approval",
            Self::Checkpoint => "checkpoint",
            Self::GroupRoom => "group_room",
        }
    }

    fn role(self) -> &'static str {
        match self {
            Self::Queue => "queue admission",
            Self::GroupRoom => "group context",
            Self::Planner => "decision planning",
            Self::CompletionGate => "completion gate",
            Self::Executor => "tool execution",
            Self::Approval => "human gate",
            Self::Checkpoint => "state checkpoint",
            Self::Reviewer => "final review",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "queue" => Some(Self::Queue),
            "planner" => Some(Self::Planner),
            "completion_gate" => Some(Self::CompletionGate),
            "executor" => Some(Self::Executor),
            "reviewer" => Some(Self::Reviewer),
            "approval" => Some(Self::Approval),
            "checkpoint" => Some(Self::Checkpoint),
            "group_room" => Some(Self::GroupRoom),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum WorkflowNodeStatus {
    Pending,
    Running,
    Completed,
    Waiting,
    Failed,
    Canceled,
    Skipped,
}

impl WorkflowNodeStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Waiting => "waiting",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
            Self::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum WorkflowPlannerRoute {
    ExecuteTools {
        requests: Vec<(String, Value)>,
        request_count: usize,
    },
    ReviewFinal {
        content: String,
    },
    Recover {
        observation: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WorkflowPlannerErrorKind {
    CompletionGate,
    ContextCompression,
    IterationBudgetExhausted,
    LlmError,
    LlmRecoveryExhausted,
    NoFinalAnswer,
    ProviderTurnAborted,
    ToolApprovalRequired,
    ToolSchemaValidation,
    ToolUnavailable,
    ToolRequest,
}

impl WorkflowPlannerErrorKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::CompletionGate => "completion_gate",
            Self::ContextCompression => "context_compression",
            Self::IterationBudgetExhausted => "iteration_budget_exhausted",
            Self::LlmError => "llm_error",
            Self::LlmRecoveryExhausted => "llm_recovery_exhausted",
            Self::NoFinalAnswer => "no_final_answer",
            Self::ProviderTurnAborted => "provider_turn_aborted",
            Self::ToolApprovalRequired => "tool_approval_required",
            Self::ToolSchemaValidation => "tool_schema_validation",
            Self::ToolUnavailable => "tool_unavailable",
            Self::ToolRequest => "tool_request",
        }
    }

    fn observation_label(self) -> &'static str {
        match self {
            Self::CompletionGate => "completion gate",
            Self::ContextCompression => "context compression error",
            Self::IterationBudgetExhausted => "iteration budget exhausted",
            Self::LlmError => "LLM error",
            Self::LlmRecoveryExhausted => "LLM recovery exhausted",
            Self::NoFinalAnswer => "no final answer",
            Self::ProviderTurnAborted => "provider turn aborted",
            Self::ToolApprovalRequired => "tool approval required",
            Self::ToolSchemaValidation => "tool schema error",
            Self::ToolUnavailable => "tool unavailable error",
            Self::ToolRequest => "tool error",
        }
    }

    fn from_tool_validation_error_kind(kind: ToolCallValidationErrorKind) -> Self {
        match kind {
            ToolCallValidationErrorKind::ToolUnavailable => Self::ToolUnavailable,
            ToolCallValidationErrorKind::SchemaValidation => Self::ToolSchemaValidation,
            ToolCallValidationErrorKind::ApprovalRequired => Self::ToolApprovalRequired,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum WorkflowExecutorRoute {
    ContinuePlanning {
        tool_count: usize,
        parallel: Option<bool>,
    },
    AwaitApproval {
        server_id: String,
        tool_name: String,
    },
}

#[derive(Debug, Clone)]
pub(super) enum WorkflowExecutorToolResolution {
    Internal(WorkflowExecutorToolIdentity),
    Mcp {
        identity: WorkflowExecutorToolIdentity,
        definition: ToolDefinition,
    },
    Unavailable {
        requested_name: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WorkflowExecutorApprovalWait {
    identity: WorkflowExecutorToolIdentity,
    approval_id: Option<String>,
    reason: Option<String>,
}

impl WorkflowExecutorApprovalWait {
    fn new(
        identity: &WorkflowExecutorToolIdentity,
        approval_id: Option<&str>,
        reason: Option<&str>,
    ) -> Self {
        Self {
            identity: identity.clone(),
            approval_id: approval_id.map(str::to_string),
            reason: reason.map(str::to_string),
        }
    }

    pub(super) fn identity(&self) -> &WorkflowExecutorToolIdentity {
        &self.identity
    }

    pub(super) fn approval_id(&self) -> Option<&str> {
        self.approval_id.as_deref()
    }

    pub(super) fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WorkflowExecutorToolKind {
    Internal,
    Mcp,
}

impl WorkflowExecutorToolKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Internal => "internal",
            Self::Mcp => "mcp",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WorkflowExecutorApprovalPolicyStage {
    Base,
    Scheduled,
    Smart,
    Subagent,
    Final,
}

impl WorkflowExecutorApprovalPolicyStage {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Base => "base_policy",
            Self::Scheduled => "scheduled_policy",
            Self::Smart => "smart_policy",
            Self::Subagent => "subagent_policy",
            Self::Final => "approval_policy",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WorkflowExecutorToolIdentity {
    requested_name: String,
    server_id: String,
    tool_name: String,
    kind: WorkflowExecutorToolKind,
}

impl WorkflowExecutorToolIdentity {
    fn internal(tool_name: &str) -> Self {
        Self {
            requested_name: tool_name.to_string(),
            server_id: WORKFLOW_INTERNAL_TOOL_SERVER_ID.into(),
            tool_name: tool_name.to_string(),
            kind: WorkflowExecutorToolKind::Internal,
        }
    }

    fn mcp(requested_name: &str, server_id: &str, tool_name: &str) -> Self {
        Self {
            requested_name: requested_name.to_string(),
            server_id: server_id.to_string(),
            tool_name: tool_name.to_string(),
            kind: WorkflowExecutorToolKind::Mcp,
        }
    }

    pub(super) fn requested_name(&self) -> &str {
        &self.requested_name
    }

    pub(super) fn server_id(&self) -> &str {
        &self.server_id
    }

    pub(super) fn tool_name(&self) -> &str {
        &self.tool_name
    }

    pub(super) fn kind(&self) -> WorkflowExecutorToolKind {
        self.kind
    }

    pub(super) fn is_internal(&self) -> bool {
        self.kind == WorkflowExecutorToolKind::Internal
    }

    pub(super) fn is_mcp(&self) -> bool {
        self.kind == WorkflowExecutorToolKind::Mcp
    }

    pub(super) fn source_label(&self) -> String {
        if self.is_internal() {
            self.requested_name.clone()
        } else {
            format!("{}:{}", self.server_id, self.tool_name)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum WorkflowReviewerRoute {
    Completed {
        message_id: String,
        model: Option<String>,
        provider_id: Option<String>,
    },
    Skipped {
        message_id: String,
        reason: String,
        model: Option<String>,
        provider_id: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WorkflowDriver {
    mode: WorkflowMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WorkflowApprovalNode {
    driver: WorkflowDriver,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WorkflowQueueNode {
    driver: WorkflowDriver,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WorkflowGroupRoomNode {
    driver: WorkflowDriver,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WorkflowPlannerNode {
    driver: WorkflowDriver,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WorkflowExecutorNode {
    driver: WorkflowDriver,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WorkflowReviewerNode {
    driver: WorkflowDriver,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WorkflowCheckpointNode {
    driver: WorkflowDriver,
}

impl WorkflowDriver {
    pub(super) fn new(mode: WorkflowMode) -> Self {
        Self { mode }
    }

    pub(super) fn queue(self) -> WorkflowQueueNode {
        WorkflowQueueNode { driver: self }
    }

    pub(super) fn group_room(self) -> WorkflowGroupRoomNode {
        WorkflowGroupRoomNode { driver: self }
    }

    pub(super) fn approval(self) -> WorkflowApprovalNode {
        WorkflowApprovalNode { driver: self }
    }

    pub(super) fn planner(self) -> WorkflowPlannerNode {
        WorkflowPlannerNode { driver: self }
    }

    pub(super) fn executor(self) -> WorkflowExecutorNode {
        WorkflowExecutorNode { driver: self }
    }

    pub(super) fn reviewer(self) -> WorkflowReviewerNode {
        WorkflowReviewerNode { driver: self }
    }

    pub(super) fn checkpoint(self) -> WorkflowCheckpointNode {
        WorkflowCheckpointNode { driver: self }
    }

    pub(super) fn bootstrap(
        self,
        run: &mut AgentRunRecord,
        request: &SendChatRequest,
        request_source: &str,
        tool_context: ToolExecutionContext,
    ) {
        push_workflow_graph_bootstrap(run, request, request_source, tool_context, self.mode);
    }

    pub(super) fn timeout(
        self,
        store: &AppStore,
        run_id: &str,
        reason: &str,
        timeout_seconds: u64,
    ) -> AppResult<()> {
        record_workflow_timeout(store, run_id, self.mode, reason, timeout_seconds)
    }

    fn planner_running(self, store: &AppStore, run_id: &str, iteration: u32) -> AppResult<()> {
        record_workflow_planner_running(store, run_id, iteration, self.mode)
    }

    fn planner_failed(
        self,
        store: &AppStore,
        run_id: &str,
        iteration: Option<u32>,
        error_kind: WorkflowPlannerErrorKind,
        error: &str,
    ) -> AppResult<()> {
        record_workflow_planner_failed(store, run_id, iteration, self.mode, error_kind, error)
    }

    fn queue_completed(self, store: &AppStore, run_id: &str, queue_item_id: &str) -> AppResult<()> {
        record_workflow_queue_completed(store, run_id, self.mode, queue_item_id)
    }

    fn queue_skipped(self, store: &AppStore, run_id: &str, reason: &str) -> AppResult<()> {
        record_workflow_queue_skipped(store, run_id, self.mode, reason)
    }

    fn queue_terminal(
        self,
        store: &AppStore,
        run_id: &str,
        queue_item_id: &str,
        status: &str,
        error: Option<&str>,
    ) -> AppResult<()> {
        record_workflow_queue_terminal(store, run_id, self.mode, queue_item_id, status, error)
    }

    fn group_room_completed(self, store: &AppStore, run_id: &str, context: Value) -> AppResult<()> {
        record_workflow_group_room_completed(store, run_id, self.mode, context)
    }

    fn group_room_running(self, store: &AppStore, run_id: &str, context: Value) -> AppResult<()> {
        record_workflow_group_room_running(store, run_id, self.mode, context)
    }

    fn group_room_failed(self, store: &AppStore, run_id: &str, context: Value) -> AppResult<()> {
        record_workflow_group_room_failed(store, run_id, self.mode, context)
    }

    fn group_room_skipped(self, store: &AppStore, run_id: &str, reason: &str) -> AppResult<()> {
        record_workflow_group_room_skipped(store, run_id, self.mode, reason)
    }

    fn approval_resumed(
        self,
        store: &AppStore,
        run_id: &str,
        server_id: &str,
        tool_name: &str,
        approval_id: Option<&str>,
        status: Option<&str>,
        reason: Option<&str>,
    ) -> AppResult<()> {
        record_workflow_approval_resumed(
            store,
            run_id,
            self.mode,
            server_id,
            tool_name,
            approval_id,
            status,
            reason,
        )
    }

    fn approval_resolved(
        self,
        store: &AppStore,
        run_id: &str,
        server_id: &str,
        tool_name: &str,
        approval_id: Option<&str>,
        status: &str,
        reason: Option<&str>,
        error: Option<&str>,
    ) -> AppResult<()> {
        record_workflow_approval_resolved(
            store,
            run_id,
            self.mode,
            server_id,
            tool_name,
            approval_id,
            status,
            reason,
            error,
        )
    }

    fn planner_route(
        self,
        store: &AppStore,
        run_id: &str,
        iteration: u32,
        decision: &Value,
        fallback_content: &str,
        available_tools: &[ToolDefinition],
    ) -> AppResult<WorkflowPlannerRoute> {
        resolve_workflow_planner_route(
            store,
            run_id,
            iteration,
            self.mode,
            decision,
            fallback_content,
            available_tools,
        )
    }

    fn executor_continue(
        self,
        store: &AppStore,
        run_id: &str,
        iteration: u32,
        tool_count: usize,
        parallel: Option<bool>,
    ) -> AppResult<WorkflowExecutorRoute> {
        resolve_workflow_executor_continue_route(
            store, run_id, iteration, self.mode, tool_count, parallel,
        )
    }

    fn executor_parallel_batch_started(
        self,
        store: &AppStore,
        run_id: &str,
        iteration: u32,
        tool_count: usize,
        tool_names: &[String],
    ) -> AppResult<()> {
        record_workflow_executor_parallel_batch_started(
            store, run_id, iteration, self.mode, tool_count, tool_names,
        )
    }

    fn executor_parallel_batch_completed(
        self,
        store: &AppStore,
        run_id: &str,
        iteration: u32,
        tool_count: usize,
        tool_names: &[String],
        succeeded: usize,
        failed: usize,
        halted: bool,
    ) -> AppResult<()> {
        record_workflow_executor_parallel_batch_completed(
            store, run_id, iteration, self.mode, tool_count, tool_names, succeeded, failed, halted,
        )
    }

    fn executor_tool_started(
        self,
        store: &AppStore,
        run_id: &str,
        iteration: u32,
        identity: &WorkflowExecutorToolIdentity,
    ) -> AppResult<()> {
        record_workflow_executor_tool_started(store, run_id, iteration, self.mode, identity)
    }

    fn executor_tool_call_bridge_target(
        self,
        store: &AppStore,
        run_id: &str,
        target_name: &str,
        server_id: &str,
        tool_name: &str,
        tool_kind: &str,
        requires_approval: bool,
        approved_replay_context: bool,
        bridge_status: &str,
        bridge_rejection_reason: Option<&str>,
    ) -> AppResult<()> {
        record_workflow_executor_tool_call_bridge_target(
            store,
            run_id,
            self.mode,
            target_name,
            server_id,
            tool_name,
            tool_kind,
            requires_approval,
            approved_replay_context,
            bridge_status,
            bridge_rejection_reason,
        )
    }

    fn executor_approval(
        self,
        store: &AppStore,
        run_id: &str,
        iteration: u32,
        server_id: &str,
        tool_name: &str,
    ) -> AppResult<WorkflowExecutorRoute> {
        resolve_workflow_executor_approval_route(store, run_id, iteration, server_id, tool_name)
    }

    fn executor_tool_approval(
        self,
        store: &AppStore,
        run_id: &str,
        iteration: u32,
        wait: &WorkflowExecutorApprovalWait,
    ) -> AppResult<WorkflowExecutorRoute> {
        resolve_workflow_executor_tool_approval_route(store, run_id, iteration, self.mode, wait)
    }

    fn executor_tool_resolution(
        self,
        store: &AppStore,
        run_id: &str,
        iteration: u32,
        resolution: &WorkflowExecutorToolResolution,
    ) -> AppResult<()> {
        record_workflow_executor_tool_resolution(store, run_id, iteration, self.mode, resolution)
    }

    fn executor_failed(
        self,
        store: &AppStore,
        run_id: &str,
        iteration: Option<u32>,
        requested_name: &str,
        server_id: &str,
        tool_name: &str,
        error: &str,
    ) -> AppResult<()> {
        record_workflow_executor_failed(
            store,
            run_id,
            self.mode,
            iteration,
            requested_name,
            server_id,
            tool_name,
            error,
        )
    }

    fn executor_approval_policy(
        self,
        store: &AppStore,
        run_id: &str,
        iteration: u32,
        identity: &WorkflowExecutorToolIdentity,
        reason: Option<&str>,
    ) -> AppResult<()> {
        record_workflow_executor_approval_policy(
            store, run_id, iteration, self.mode, identity, reason,
        )
    }

    fn executor_approval_policy_stage(
        self,
        store: &AppStore,
        run_id: &str,
        iteration: u32,
        identity: &WorkflowExecutorToolIdentity,
        stage: WorkflowExecutorApprovalPolicyStage,
        reason: Option<&str>,
    ) -> AppResult<()> {
        record_workflow_executor_approval_policy_stage(
            store, run_id, iteration, self.mode, identity, stage, reason,
        )
    }

    fn reviewer_completed(
        self,
        store: &AppStore,
        run_id: &str,
        message_id: &str,
        model: Option<&str>,
        provider_id: Option<&str>,
    ) -> AppResult<WorkflowReviewerRoute> {
        resolve_workflow_reviewer_completed_route(
            store,
            run_id,
            self.mode,
            message_id,
            model,
            provider_id,
        )
    }

    fn reviewer_skipped(
        self,
        store: &AppStore,
        run_id: &str,
        message_id: &str,
        reason: &str,
        model: Option<&str>,
        provider_id: Option<&str>,
    ) -> AppResult<WorkflowReviewerRoute> {
        resolve_workflow_reviewer_skipped_route(
            store,
            run_id,
            self.mode,
            message_id,
            reason,
            model,
            provider_id,
        )
    }

    fn checkpoint_completed(
        self,
        store: &AppStore,
        run_id: &str,
        state: &str,
        summary: &str,
        detail: Value,
    ) -> AppResult<()> {
        let mut checkpoint_detail = workflow_turn_detail(self.mode, None);
        match detail {
            Value::Object(detail) => {
                for (key, value) in detail {
                    checkpoint_detail.insert(key, value);
                }
            }
            other => {
                checkpoint_detail.insert("value".into(), other);
            }
        }
        record_workflow_checkpoint_completed(
            store,
            run_id,
            state,
            summary,
            Value::Object(checkpoint_detail),
        )
    }

    fn checkpoint_failed(
        self,
        store: &AppStore,
        run_id: &str,
        state: &str,
        summary: &str,
        detail: Value,
    ) -> AppResult<()> {
        let mut checkpoint_detail = workflow_turn_detail(self.mode, None);
        match detail {
            Value::Object(detail) => {
                for (key, value) in detail {
                    checkpoint_detail.insert(key, value);
                }
            }
            other => {
                checkpoint_detail.insert("value".into(), other);
            }
        }
        record_workflow_checkpoint_failed(
            store,
            run_id,
            state,
            summary,
            Value::Object(checkpoint_detail),
        )
    }

    fn checkpoint_waiting(
        self,
        store: &AppStore,
        run_id: &str,
        state: &str,
        summary: &str,
        detail: Value,
    ) -> AppResult<()> {
        let mut checkpoint_detail = workflow_turn_detail(self.mode, None);
        match detail {
            Value::Object(detail) => {
                for (key, value) in detail {
                    checkpoint_detail.insert(key, value);
                }
            }
            other => {
                checkpoint_detail.insert("value".into(), other);
            }
        }
        record_workflow_checkpoint_waiting(
            store,
            run_id,
            state,
            summary,
            Value::Object(checkpoint_detail),
        )
    }
}

impl WorkflowApprovalNode {
    pub(super) fn resumed(
        self,
        store: &AppStore,
        run_id: &str,
        server_id: &str,
        tool_name: &str,
        approval_id: Option<&str>,
        status: Option<&str>,
        reason: Option<&str>,
    ) -> AppResult<()> {
        self.driver.approval_resumed(
            store,
            run_id,
            server_id,
            tool_name,
            approval_id,
            status,
            reason,
        )
    }

    pub(super) fn resolved(
        self,
        store: &AppStore,
        run_id: &str,
        server_id: &str,
        tool_name: &str,
        approval_id: Option<&str>,
        status: &str,
        reason: Option<&str>,
        error: Option<&str>,
    ) -> AppResult<()> {
        self.driver.approval_resolved(
            store,
            run_id,
            server_id,
            tool_name,
            approval_id,
            status,
            reason,
            error,
        )
    }
}

impl WorkflowQueueNode {
    pub(super) fn completed(
        self,
        store: &AppStore,
        run_id: &str,
        queue_item_id: &str,
    ) -> AppResult<()> {
        self.driver.queue_completed(store, run_id, queue_item_id)
    }

    pub(super) fn skipped(self, store: &AppStore, run_id: &str, reason: &str) -> AppResult<()> {
        self.driver.queue_skipped(store, run_id, reason)
    }

    pub(super) fn terminal(
        self,
        store: &AppStore,
        run_id: &str,
        queue_item_id: &str,
        status: &str,
        error: Option<&str>,
    ) -> AppResult<()> {
        self.driver
            .queue_terminal(store, run_id, queue_item_id, status, error)
    }
}

impl WorkflowGroupRoomNode {
    pub(super) fn running(self, store: &AppStore, run_id: &str, context: Value) -> AppResult<()> {
        self.driver.group_room_running(store, run_id, context)
    }

    pub(super) fn completed(self, store: &AppStore, run_id: &str, context: Value) -> AppResult<()> {
        self.driver.group_room_completed(store, run_id, context)
    }

    pub(super) fn failed(self, store: &AppStore, run_id: &str, context: Value) -> AppResult<()> {
        self.driver.group_room_failed(store, run_id, context)
    }

    pub(super) fn skipped(self, store: &AppStore, run_id: &str, reason: &str) -> AppResult<()> {
        self.driver.group_room_skipped(store, run_id, reason)
    }
}

impl WorkflowPlannerNode {
    pub(super) fn running(self, store: &AppStore, run_id: &str, iteration: u32) -> AppResult<()> {
        self.driver.planner_running(store, run_id, iteration)
    }

    pub(super) fn route(
        self,
        store: &AppStore,
        run_id: &str,
        iteration: u32,
        decision: &Value,
        fallback_content: &str,
        available_tools: &[ToolDefinition],
    ) -> AppResult<WorkflowPlannerRoute> {
        self.driver.planner_route(
            store,
            run_id,
            iteration,
            decision,
            fallback_content,
            available_tools,
        )
    }

    pub(super) fn failed(
        self,
        store: &AppStore,
        run_id: &str,
        iteration: Option<u32>,
        error_kind: WorkflowPlannerErrorKind,
        error: &str,
    ) -> AppResult<()> {
        self.driver
            .planner_failed(store, run_id, iteration, error_kind, error)
    }
}

impl WorkflowExecutorNode {
    pub(super) fn approval_wait(
        self,
        identity: &WorkflowExecutorToolIdentity,
        approval_id: Option<&str>,
        reason: Option<&str>,
    ) -> WorkflowExecutorApprovalWait {
        WorkflowExecutorApprovalWait::new(identity, approval_id, reason)
    }

    pub(super) fn resolve_tool(
        self,
        requested_name: &str,
        mcp_tools: &[ToolDefinition],
    ) -> WorkflowExecutorToolResolution {
        if is_internal_tool(requested_name) {
            return WorkflowExecutorToolResolution::Internal(self.internal_tool(requested_name));
        }
        if let Some(definition) = resolve_mcp_tool(mcp_tools, requested_name) {
            let identity =
                self.mcp_tool(requested_name, &definition.server_id, &definition.tool_name);
            return WorkflowExecutorToolResolution::Mcp {
                identity,
                definition,
            };
        }
        WorkflowExecutorToolResolution::Unavailable {
            requested_name: requested_name.to_string(),
        }
    }

    pub(super) fn record_tool_resolution(
        self,
        store: &AppStore,
        run_id: &str,
        iteration: u32,
        resolution: &WorkflowExecutorToolResolution,
    ) -> AppResult<()> {
        self.driver
            .executor_tool_resolution(store, run_id, iteration, resolution)
    }

    pub(super) fn tool_started(
        self,
        store: &AppStore,
        run_id: &str,
        iteration: u32,
        identity: &WorkflowExecutorToolIdentity,
    ) -> AppResult<()> {
        self.driver
            .executor_tool_started(store, run_id, iteration, identity)
    }

    pub(super) fn tool_call_bridge_target(
        self,
        store: &AppStore,
        run_id: &str,
        target_name: &str,
        server_id: &str,
        tool_name: &str,
        tool_kind: &str,
        requires_approval: bool,
        approved_replay_context: bool,
        bridge_status: &str,
        bridge_rejection_reason: Option<&str>,
    ) -> AppResult<()> {
        self.driver.executor_tool_call_bridge_target(
            store,
            run_id,
            target_name,
            server_id,
            tool_name,
            tool_kind,
            requires_approval,
            approved_replay_context,
            bridge_status,
            bridge_rejection_reason,
        )
    }

    pub(super) fn failed(
        self,
        store: &AppStore,
        run_id: &str,
        iteration: Option<u32>,
        requested_name: &str,
        server_id: &str,
        tool_name: &str,
        error: &str,
    ) -> AppResult<()> {
        self.driver.executor_failed(
            store,
            run_id,
            iteration,
            requested_name,
            server_id,
            tool_name,
            error,
        )
    }

    pub(super) fn record_approval_policy(
        self,
        store: &AppStore,
        run_id: &str,
        iteration: u32,
        identity: &WorkflowExecutorToolIdentity,
        reason: Option<&str>,
    ) -> AppResult<()> {
        self.driver
            .executor_approval_policy(store, run_id, iteration, identity, reason)
    }

    pub(super) fn record_approval_policy_stage(
        self,
        store: &AppStore,
        run_id: &str,
        iteration: u32,
        identity: &WorkflowExecutorToolIdentity,
        stage: WorkflowExecutorApprovalPolicyStage,
        reason: Option<&str>,
    ) -> AppResult<()> {
        self.driver
            .executor_approval_policy_stage(store, run_id, iteration, identity, stage, reason)
    }

    pub(super) fn internal_tool(self, tool_name: &str) -> WorkflowExecutorToolIdentity {
        WorkflowExecutorToolIdentity::internal(tool_name)
    }

    pub(super) fn mcp_tool(
        self,
        requested_name: &str,
        server_id: &str,
        tool_name: &str,
    ) -> WorkflowExecutorToolIdentity {
        WorkflowExecutorToolIdentity::mcp(requested_name, server_id, tool_name)
    }

    pub(super) fn continue_planning(
        self,
        store: &AppStore,
        run_id: &str,
        iteration: u32,
        tool_count: usize,
        parallel: Option<bool>,
    ) -> AppResult<WorkflowExecutorRoute> {
        self.driver
            .executor_continue(store, run_id, iteration, tool_count, parallel)
    }

    pub(super) fn parallel_batch_started(
        self,
        store: &AppStore,
        run_id: &str,
        iteration: u32,
        tool_count: usize,
        tool_names: &[String],
    ) -> AppResult<()> {
        self.driver
            .executor_parallel_batch_started(store, run_id, iteration, tool_count, tool_names)
    }

    pub(super) fn parallel_batch_completed(
        self,
        store: &AppStore,
        run_id: &str,
        iteration: u32,
        tool_count: usize,
        tool_names: &[String],
        succeeded: usize,
        failed: usize,
        halted: bool,
    ) -> AppResult<()> {
        self.driver.executor_parallel_batch_completed(
            store, run_id, iteration, tool_count, tool_names, succeeded, failed, halted,
        )
    }

    fn await_approval(
        self,
        store: &AppStore,
        run_id: &str,
        iteration: u32,
        server_id: &str,
        tool_name: &str,
    ) -> AppResult<WorkflowExecutorRoute> {
        self.driver
            .executor_approval(store, run_id, iteration, server_id, tool_name)
    }

    pub(super) fn await_tool_approval(
        self,
        store: &AppStore,
        run_id: &str,
        iteration: u32,
        wait: &WorkflowExecutorApprovalWait,
    ) -> AppResult<WorkflowExecutorRoute> {
        self.driver
            .executor_tool_approval(store, run_id, iteration, wait)
    }
}

impl WorkflowReviewerNode {
    pub(super) fn completed(
        self,
        store: &AppStore,
        run_id: &str,
        message_id: &str,
        model: Option<&str>,
        provider_id: Option<&str>,
    ) -> AppResult<WorkflowReviewerRoute> {
        self.driver
            .reviewer_completed(store, run_id, message_id, model, provider_id)
    }

    pub(super) fn skipped(
        self,
        store: &AppStore,
        run_id: &str,
        message_id: &str,
        reason: &str,
        model: Option<&str>,
        provider_id: Option<&str>,
    ) -> AppResult<WorkflowReviewerRoute> {
        self.driver
            .reviewer_skipped(store, run_id, message_id, reason, model, provider_id)
    }
}

impl WorkflowCheckpointNode {
    pub(super) fn resume_requested_from_current(
        self,
        store: &AppStore,
        run: &AgentRunRecord,
        detail: Value,
    ) -> AppResult<()> {
        let from = run
            .workflow_graph
            .as_ref()
            .and_then(|graph| graph.get("currentNode"))
            .or_else(|| {
                run.workflow_graph
                    .as_ref()
                    .and_then(|graph| graph.get("current_node"))
            })
            .and_then(Value::as_str)
            .and_then(WorkflowNodeName::from_str)
            .unwrap_or(WorkflowNodeName::Planner);
        if from == WorkflowNodeName::Checkpoint {
            return Ok(());
        }
        let mut transition_detail = workflow_turn_detail(self.driver.mode, None);
        match detail {
            Value::Object(detail) => {
                for (key, value) in detail {
                    transition_detail.insert(key, value);
                }
            }
            other => {
                transition_detail.insert("value".into(), other);
            }
        }
        append_workflow_transition_event(
            store,
            &run.run_id,
            from,
            WorkflowNodeName::Checkpoint,
            WORKFLOW_REASON_RESUME_CHECKPOINT_REQUESTED,
            Value::Object(transition_detail),
        )
    }

    pub(super) fn completed(
        self,
        store: &AppStore,
        run_id: &str,
        state: &str,
        summary: &str,
        detail: Value,
    ) -> AppResult<()> {
        self.driver
            .checkpoint_completed(store, run_id, state, summary, detail)
    }

    pub(super) fn waiting(
        self,
        store: &AppStore,
        run_id: &str,
        state: &str,
        summary: &str,
        detail: Value,
    ) -> AppResult<()> {
        self.driver
            .checkpoint_waiting(store, run_id, state, summary, detail)
    }

    pub(super) fn failed(
        self,
        store: &AppStore,
        run_id: &str,
        state: &str,
        summary: &str,
        detail: Value,
    ) -> AppResult<()> {
        self.driver
            .checkpoint_failed(store, run_id, state, summary, detail)
    }

    pub(super) fn resume_continued_to_planner(
        self,
        store: &AppStore,
        run_id: &str,
        detail: Value,
    ) -> AppResult<()> {
        let mut transition_detail = workflow_turn_detail(self.driver.mode, None);
        match detail {
            Value::Object(detail) => {
                for (key, value) in detail {
                    transition_detail.insert(key, value);
                }
            }
            other => {
                transition_detail.insert("value".into(), other);
            }
        }
        append_workflow_transition_event(
            store,
            run_id,
            WorkflowNodeName::Checkpoint,
            WorkflowNodeName::Planner,
            WORKFLOW_REASON_RESUME_CHECKPOINT_CONTINUED,
            Value::Object(transition_detail),
        )
    }

    pub(super) fn waiting_from_executor(
        self,
        store: &AppStore,
        run_id: &str,
        reason: &str,
        transition_detail: Value,
        state: &str,
        summary: &str,
        detail: Value,
    ) -> AppResult<()> {
        append_workflow_transition_event(
            store,
            run_id,
            WorkflowNodeName::Executor,
            WorkflowNodeName::Checkpoint,
            reason,
            transition_detail,
        )?;
        self.waiting(store, run_id, state, summary, detail)
    }

    pub(super) fn future_wait_from_executor(
        self,
        store: &AppStore,
        run_id: &str,
        state: &str,
        summary: &str,
        detail: Value,
    ) -> AppResult<()> {
        let mut transition_detail = workflow_turn_detail(self.driver.mode, None);
        transition_detail.insert("state".into(), json!(state));
        transition_detail.insert("summary".into(), json!(summary));
        match detail.clone() {
            Value::Object(detail) => {
                for (key, value) in detail {
                    transition_detail.insert(key, value);
                }
            }
            other => {
                transition_detail.insert("value".into(), other);
            }
        }
        self.waiting_from_executor(
            store,
            run_id,
            WORKFLOW_REASON_FUTURE_CHECKPOINT_WAIT,
            Value::Object(transition_detail),
            state,
            summary,
            detail,
        )
    }
}

pub(super) fn push_workflow_graph_bootstrap(
    run: &mut AgentRunRecord,
    request: &SendChatRequest,
    request_source: &str,
    tool_context: ToolExecutionContext,
    mode: WorkflowMode,
) {
    let updated_at = now_iso();
    let group_room_context =
        workflow_group_room_context(request.provider_data.as_ref(), request_source);
    let tool_context_label = workflow_tool_context_label(tool_context);
    let mut queue_detail = workflow_bootstrap_node_detail(mode, request_source, tool_context_label);
    if let Some(queue_item_id) = request.queue_item_id.as_ref() {
        queue_detail.insert("queueItemId".into(), json!(queue_item_id));
        queue_detail.insert("admission".into(), json!(WORKFLOW_REASON_QUEUED_TURN));
        queue_detail.insert("queueStatus".into(), json!("claimed"));
        queue_detail.insert("queueLifecycle".into(), json!("dequeued_for_run"));
    } else {
        queue_detail.insert("reason".into(), json!(WORKFLOW_REASON_DIRECT_TURN));
        queue_detail.insert("admission".into(), json!(WORKFLOW_REASON_DIRECT_TURN));
        queue_detail.insert("queueStatus".into(), json!("not_queued"));
        queue_detail.insert("queueLifecycle".into(), json!("not_applicable"));
    }
    let mut group_room_detail =
        workflow_bootstrap_node_detail(mode, request_source, tool_context_label);
    match group_room_context {
        Some(Value::Object(context)) => {
            for (key, value) in context {
                group_room_detail.insert(key, value);
            }
        }
        Some(context) => {
            group_room_detail.insert("context".into(), context);
        }
        None => {
            group_room_detail.insert(
                "reason".into(),
                json!(WORKFLOW_REASON_NO_GROUP_ROOM_CONTEXT),
            );
        }
    }
    let pending_detail = || {
        Value::Object(workflow_bootstrap_node_detail(
            mode,
            request_source,
            tool_context_label,
        ))
    };
    let queue_status = if request.queue_item_id.is_some() {
        WorkflowNodeStatus::Completed
    } else {
        WorkflowNodeStatus::Skipped
    };
    let group_room_status = if group_room_detail.get("reason").is_none() {
        WorkflowNodeStatus::Completed
    } else {
        WorkflowNodeStatus::Skipped
    };
    let mut queue_transition_detail =
        workflow_bootstrap_node_detail(mode, request_source, tool_context_label);
    if let Some(queue_item_id) = request.queue_item_id.as_ref() {
        queue_transition_detail.insert("queueItemId".into(), json!(queue_item_id));
        queue_transition_detail.insert("admission".into(), json!(WORKFLOW_REASON_QUEUED_TURN));
        queue_transition_detail.insert("queueStatus".into(), json!("claimed"));
        queue_transition_detail.insert("queueLifecycle".into(), json!("dequeued_for_run"));
    } else {
        queue_transition_detail.insert("admission".into(), json!(WORKFLOW_REASON_DIRECT_TURN));
        queue_transition_detail.insert("queueStatus".into(), json!("not_queued"));
        queue_transition_detail.insert("queueLifecycle".into(), json!("not_applicable"));
    }
    let mut planner_transition_detail =
        workflow_bootstrap_node_detail(mode, request_source, tool_context_label);
    if group_room_status == WorkflowNodeStatus::Completed {
        planner_transition_detail.insert("groupRoom".into(), json!("context_ready"));
    } else {
        planner_transition_detail.insert("groupRoom".into(), json!("not_applicable"));
    }
    let snapshot = json!({
        "schema": SYNTHGRAPH_WORKFLOW_SCHEMA,
        "mode": mode.as_str(),
        "requestSource": request_source,
        "request_source": request_source,
        "toolContext": tool_context_label,
        "tool_context": tool_context_label,
        "currentNode": WorkflowNodeName::Planner,
        "current_node": WorkflowNodeName::Planner,
        "currentStatus": WorkflowNodeStatus::Pending,
        "current_status": WorkflowNodeStatus::Pending,
        "lastEventSequence": 0,
        "last_event_sequence": 0,
        "updatedAt": updated_at,
        "updated_at": updated_at,
        "transitions": [
            workflow_transition_snapshot(
                WorkflowNodeName::Queue,
                WorkflowNodeName::GroupRoom,
                if request.queue_item_id.is_some() {
                    WORKFLOW_REASON_QUEUED_TURN
                } else {
                    WORKFLOW_REASON_DIRECT_TURN
                },
                Value::Object(queue_transition_detail),
                &updated_at,
            ),
            workflow_transition_snapshot(
                WorkflowNodeName::GroupRoom,
                WorkflowNodeName::Planner,
                if group_room_status == WorkflowNodeStatus::Completed {
                    WORKFLOW_REASON_GROUP_CONTEXT_READY
                } else {
                    WORKFLOW_REASON_NO_GROUP_ROOM_CONTEXT
                },
                Value::Object(planner_transition_detail),
                &updated_at,
            ),
        ],
        "nodes": [
            workflow_node_snapshot(
                WorkflowNodeName::Queue,
                queue_status,
                Value::Object(queue_detail),
                &updated_at,
            ),
            workflow_node_snapshot(
                WorkflowNodeName::GroupRoom,
                group_room_status,
                Value::Object(group_room_detail),
                &updated_at,
            ),
            workflow_node_snapshot(
                WorkflowNodeName::Planner,
                WorkflowNodeStatus::Pending,
                pending_detail(),
                &updated_at,
            ),
            workflow_node_snapshot(
                WorkflowNodeName::Executor,
                WorkflowNodeStatus::Pending,
                pending_detail(),
                &updated_at,
            ),
            workflow_node_snapshot(
                WorkflowNodeName::Approval,
                WorkflowNodeStatus::Pending,
                pending_detail(),
                &updated_at,
            ),
            workflow_node_snapshot(
                WorkflowNodeName::Checkpoint,
                WorkflowNodeStatus::Pending,
                pending_detail(),
                &updated_at,
            ),
            workflow_node_snapshot(
                WorkflowNodeName::Reviewer,
                WorkflowNodeStatus::Pending,
                pending_detail(),
                &updated_at,
            ),
        ],
    });
    run.workflow_graph = Some(snapshot.clone());
    run.phase_events.push(AgentRunPhaseRecord {
        phase: WORKFLOW_PHASE_INITIALIZED.into(),
        detail: snapshot,
        updated_at,
    });
}

pub(super) fn append_workflow_node_event(
    store: &AppStore,
    run_id: &str,
    node: WorkflowNodeName,
    status: WorkflowNodeStatus,
    detail: Value,
) -> AppResult<()> {
    append_workflow_phase_event(
        store,
        run_id,
        WORKFLOW_PHASE_NODE,
        json!({
            "schema": SYNTHGRAPH_WORKFLOW_SCHEMA,
            "node": node,
            "role": node.role(),
            "status": status,
            "detail": detail,
        }),
        format!("workflow node {} {}", node.as_str(), status.as_str()),
    )
}

pub(super) fn append_workflow_transition_event(
    store: &AppStore,
    run_id: &str,
    from: WorkflowNodeName,
    to: WorkflowNodeName,
    reason: &str,
    detail: Value,
) -> AppResult<()> {
    let topology = workflow_transition_topology_metadata(from, to, reason);
    append_workflow_phase_event(
        store,
        run_id,
        WORKFLOW_PHASE_TRANSITION,
        json!({
            "schema": SYNTHGRAPH_WORKFLOW_SCHEMA,
            "from": from,
            "to": to,
            "reason": reason,
            "topologyEdgeKnown": topology.edge_known,
            "topology_edge_known": topology.edge_known,
            "topologyReasonKnown": topology.reason_known,
            "topology_reason_known": topology.reason_known,
            "topologyEdgeSource": topology.source.clone(),
            "topology_edge_source": topology.source,
            "topologyEdgeLabel": topology.label.clone(),
            "topology_edge_label": topology.label,
            "detail": detail,
        }),
        format!("workflow transition {} -> {}", from.as_str(), to.as_str()),
    )
}

#[derive(Debug, Clone)]
struct WorkflowTransitionTopologyMetadata {
    edge_known: bool,
    reason_known: bool,
    source: Value,
    label: String,
}

fn workflow_transition_topology_metadata(
    from: WorkflowNodeName,
    to: WorkflowNodeName,
    reason: &str,
) -> WorkflowTransitionTopologyMetadata {
    let source = workflow_transition_topology_source(from, to, reason);
    WorkflowTransitionTopologyMetadata {
        edge_known: source.is_some(),
        reason_known: WORKFLOW_REASON_ORDER.contains(&reason),
        source: source.map(Value::from).unwrap_or(Value::Null),
        label: format!("{} -> {} ({reason})", from.as_str(), to.as_str()),
    }
}

fn workflow_transition_topology_metadata_from_object(
    object: &Map<String, Value>,
) -> Option<WorkflowTransitionTopologyMetadata> {
    let from = object
        .get("from")
        .and_then(Value::as_str)
        .and_then(WorkflowNodeName::from_str)?;
    let to = object
        .get("to")
        .and_then(Value::as_str)
        .and_then(WorkflowNodeName::from_str)?;
    let reason = object.get("reason").and_then(Value::as_str)?;
    Some(workflow_transition_topology_metadata(from, to, reason))
}

fn workflow_transition_topology_source(
    from: WorkflowNodeName,
    to: WorkflowNodeName,
    reason: &str,
) -> Option<&'static str> {
    match (from, to, reason) {
        (WorkflowNodeName::Queue, WorkflowNodeName::GroupRoom, WORKFLOW_REASON_QUEUED_TURN)
        | (WorkflowNodeName::Queue, WorkflowNodeName::GroupRoom, WORKFLOW_REASON_DIRECT_TURN) => {
            Some("bootstrap")
        }
        (
            WorkflowNodeName::GroupRoom,
            WorkflowNodeName::Planner,
            WORKFLOW_REASON_GROUP_CONTEXT_READY,
        )
        | (
            WorkflowNodeName::GroupRoom,
            WorkflowNodeName::Planner,
            WORKFLOW_REASON_NO_GROUP_ROOM_CONTEXT,
        ) => Some("bootstrap"),
        (WorkflowNodeName::Planner, WorkflowNodeName::Executor, WORKFLOW_REASON_TOOL_CALLS) => {
            Some("planner route")
        }
        (
            WorkflowNodeName::Executor,
            WorkflowNodeName::Planner,
            WORKFLOW_REASON_TOOL_OBSERVATIONS_RECORDED,
        ) => Some("executor route"),
        (
            WorkflowNodeName::Executor,
            WorkflowNodeName::Approval,
            WORKFLOW_REASON_APPROVAL_REQUIRED,
        ) => Some("approval gate"),
        (
            WorkflowNodeName::Approval,
            WorkflowNodeName::Planner,
            WORKFLOW_REASON_APPROVAL_RESUMED,
        ) => Some("approval continuation"),
        (
            WorkflowNodeName::Executor,
            WorkflowNodeName::Checkpoint,
            WORKFLOW_REASON_CLARIFY_REQUIRES_USER_INPUT,
        )
        | (
            WorkflowNodeName::Executor,
            WorkflowNodeName::Checkpoint,
            WORKFLOW_REASON_FUTURE_CHECKPOINT_WAIT,
        ) => Some("checkpoint gate"),
        (_, WorkflowNodeName::Checkpoint, WORKFLOW_REASON_RESUME_CHECKPOINT_REQUESTED) => {
            Some("resume management")
        }
        (
            WorkflowNodeName::Checkpoint,
            WorkflowNodeName::Planner,
            WORKFLOW_REASON_RESUME_CHECKPOINT_CONTINUED,
        ) => Some("resume management"),
        (
            WorkflowNodeName::Planner,
            WorkflowNodeName::CompletionGate,
            WORKFLOW_REASON_FINAL_ANSWER_CANDIDATE,
        ) => Some("completion gate route"),
        (
            WorkflowNodeName::Planner,
            WorkflowNodeName::Reviewer,
            WORKFLOW_REASON_FINAL_ANSWER_CANDIDATE,
        ) => Some("review route"),
        (
            WorkflowNodeName::CompletionGate,
            WorkflowNodeName::Reviewer,
            WORKFLOW_REASON_COMPLETION_GATE_PASSED,
        ) => Some("completion gate"),
        (
            WorkflowNodeName::Executor,
            WorkflowNodeName::GroupRoom,
            WORKFLOW_REASON_DELEGATE_TASK_STARTED,
        ) => Some("delegation"),
        (
            WorkflowNodeName::GroupRoom,
            WorkflowNodeName::Executor,
            WORKFLOW_REASON_DELEGATE_TASK_COMPLETED,
        )
        | (
            WorkflowNodeName::GroupRoom,
            WorkflowNodeName::Executor,
            WORKFLOW_REASON_DELEGATE_TASK_FAILED,
        ) => Some("delegation"),
        _ => None,
    }
}

pub(super) fn record_workflow_queue_completed(
    store: &AppStore,
    run_id: &str,
    mode: WorkflowMode,
    queue_item_id: &str,
) -> AppResult<()> {
    let mut detail = workflow_turn_detail(mode, None);
    detail.insert("queueItemId".into(), json!(queue_item_id));
    append_workflow_node_event(
        store,
        run_id,
        WorkflowNodeName::Queue,
        WorkflowNodeStatus::Completed,
        Value::Object(detail),
    )
}

pub(super) fn record_workflow_queue_skipped(
    store: &AppStore,
    run_id: &str,
    mode: WorkflowMode,
    reason: &str,
) -> AppResult<()> {
    let mut detail = workflow_turn_detail(mode, None);
    detail.insert("reason".into(), json!(reason));
    append_workflow_node_event(
        store,
        run_id,
        WorkflowNodeName::Queue,
        WorkflowNodeStatus::Skipped,
        Value::Object(detail),
    )
}

pub(super) fn record_workflow_queue_terminal(
    store: &AppStore,
    run_id: &str,
    mode: WorkflowMode,
    queue_item_id: &str,
    status: &str,
    error: Option<&str>,
) -> AppResult<()> {
    let normalized_status = status.trim().to_ascii_lowercase();
    let node_status = match normalized_status.as_str() {
        "failed" => WorkflowNodeStatus::Failed,
        "canceled" | "cancelled" => WorkflowNodeStatus::Canceled,
        _ => WorkflowNodeStatus::Completed,
    };
    let queue_status = match normalized_status.as_str() {
        "" => "completed",
        "cancelled" => "canceled",
        other => other,
    };
    let queue_lifecycle = match normalized_status.as_str() {
        "completed" => "turn_completed",
        "failed" => "turn_failed",
        "canceled" | "cancelled" => "canceled",
        _ => "terminal",
    };
    let mut detail = workflow_turn_detail(mode, None);
    detail.insert("queueItemId".into(), json!(queue_item_id));
    detail.insert("queueStatus".into(), json!(queue_status));
    detail.insert("queueLifecycle".into(), json!(queue_lifecycle));
    detail.insert(
        "preserveCurrent".into(),
        json!(!matches!(
            normalized_status.as_str(),
            "canceled" | "cancelled"
        )),
    );
    if let Some(error) = error.filter(|value| !value.trim().is_empty()) {
        detail.insert("error".into(), json!(error));
    }
    append_workflow_node_event(
        store,
        run_id,
        WorkflowNodeName::Queue,
        node_status,
        Value::Object(detail),
    )
}

pub(super) fn record_workflow_group_room_completed(
    store: &AppStore,
    run_id: &str,
    mode: WorkflowMode,
    context: Value,
) -> AppResult<()> {
    append_workflow_node_event(
        store,
        run_id,
        WorkflowNodeName::GroupRoom,
        WorkflowNodeStatus::Completed,
        Value::Object(workflow_group_room_detail(mode, context)),
    )
}

pub(super) fn record_workflow_group_room_running(
    store: &AppStore,
    run_id: &str,
    mode: WorkflowMode,
    context: Value,
) -> AppResult<()> {
    append_workflow_node_event(
        store,
        run_id,
        WorkflowNodeName::GroupRoom,
        WorkflowNodeStatus::Running,
        Value::Object(workflow_group_room_detail(mode, context)),
    )
}

pub(super) fn record_workflow_group_room_failed(
    store: &AppStore,
    run_id: &str,
    mode: WorkflowMode,
    context: Value,
) -> AppResult<()> {
    append_workflow_node_event(
        store,
        run_id,
        WorkflowNodeName::GroupRoom,
        WorkflowNodeStatus::Failed,
        Value::Object(workflow_group_room_detail(mode, context)),
    )
}

pub(super) fn record_workflow_group_room_skipped(
    store: &AppStore,
    run_id: &str,
    mode: WorkflowMode,
    reason: &str,
) -> AppResult<()> {
    let mut detail = workflow_turn_detail(mode, None);
    detail.insert("reason".into(), json!(reason));
    append_workflow_node_event(
        store,
        run_id,
        WorkflowNodeName::GroupRoom,
        WorkflowNodeStatus::Skipped,
        Value::Object(detail),
    )
}

fn workflow_group_room_detail(mode: WorkflowMode, context: Value) -> Map<String, Value> {
    let mut detail = workflow_turn_detail(mode, None);
    match context {
        Value::Object(context) => {
            for (key, value) in context {
                detail.insert(key, value);
            }
        }
        other => {
            detail.insert("context".into(), other);
        }
    }
    detail
}

pub(super) fn record_workflow_checkpoint_completed(
    store: &AppStore,
    run_id: &str,
    state: &str,
    summary: &str,
    detail: Value,
) -> AppResult<()> {
    append_workflow_checkpoint_event(
        store,
        run_id,
        WorkflowNodeStatus::Completed,
        state,
        summary,
        detail,
    )
}

pub(super) fn record_workflow_checkpoint_failed(
    store: &AppStore,
    run_id: &str,
    state: &str,
    summary: &str,
    detail: Value,
) -> AppResult<()> {
    append_workflow_checkpoint_event(
        store,
        run_id,
        WorkflowNodeStatus::Failed,
        state,
        summary,
        detail,
    )
}

pub(super) fn record_workflow_checkpoint_waiting(
    store: &AppStore,
    run_id: &str,
    state: &str,
    summary: &str,
    detail: Value,
) -> AppResult<()> {
    append_workflow_checkpoint_event(
        store,
        run_id,
        WorkflowNodeStatus::Waiting,
        state,
        summary,
        detail,
    )
}

fn append_workflow_checkpoint_event(
    store: &AppStore,
    run_id: &str,
    status: WorkflowNodeStatus,
    state: &str,
    summary: &str,
    detail: Value,
) -> AppResult<()> {
    let mut detail_object = match detail {
        Value::Object(object) => object,
        other => {
            let mut object = Map::new();
            object.insert("value".into(), other);
            object
        }
    };
    detail_object.insert("state".into(), json!(state));
    detail_object.insert("summary".into(), json!(summary));
    append_workflow_node_event(
        store,
        run_id,
        WorkflowNodeName::Checkpoint,
        status,
        Value::Object(detail_object),
    )
}

pub(super) fn record_workflow_pending_approval(
    store: &AppStore,
    run_id: &str,
    iteration: u32,
    server_id: &str,
    tool_name: &str,
) -> AppResult<()> {
    let mut detail = Map::new();
    detail.insert("iteration".into(), json!(iteration));
    insert_workflow_detail_alias_value(&mut detail, "serverId", "server_id", json!(server_id));
    insert_workflow_detail_alias_value(&mut detail, "toolName", "tool_name", json!(tool_name));
    let mut gate = workflow_human_gate_detail("tool_approval", "waiting", run_id);
    gate.insert("iteration".into(), json!(iteration));
    insert_workflow_detail_alias_value(&mut gate, "serverId", "server_id", json!(server_id));
    insert_workflow_detail_alias_value(&mut gate, "toolName", "tool_name", json!(tool_name));
    insert_workflow_human_gate(&mut detail, gate);
    append_workflow_pending_approval_detail(store, run_id, Value::Object(detail))
}

fn append_workflow_pending_approval_detail(
    store: &AppStore,
    run_id: &str,
    detail: Value,
) -> AppResult<()> {
    append_workflow_transition_event(
        store,
        run_id,
        WorkflowNodeName::Executor,
        WorkflowNodeName::Approval,
        WORKFLOW_REASON_APPROVAL_REQUIRED,
        detail.clone(),
    )?;
    append_workflow_node_event(
        store,
        run_id,
        WorkflowNodeName::Approval,
        WorkflowNodeStatus::Waiting,
        detail,
    )
}

pub(super) fn record_workflow_pending_tool_approval(
    store: &AppStore,
    run_id: &str,
    iteration: u32,
    mode: WorkflowMode,
    wait: &WorkflowExecutorApprovalWait,
) -> AppResult<()> {
    let mut detail = workflow_turn_detail(mode, Some(iteration));
    append_workflow_tool_identity_detail(&mut detail, wait.identity());
    if let Some(approval_id) = wait.approval_id() {
        detail.insert("approvalId".into(), json!(approval_id));
    }
    if let Some(reason) = wait.reason() {
        detail.insert("reason".into(), json!(reason));
    }
    let mut gate = workflow_human_gate_detail("tool_approval", "waiting", run_id);
    gate.insert("mode".into(), json!(mode.as_str()));
    gate.insert("iteration".into(), json!(iteration));
    append_workflow_tool_identity_detail(&mut gate, wait.identity());
    if let Some(approval_id) = wait.approval_id() {
        insert_workflow_detail_alias_value(
            &mut gate,
            "approvalId",
            "approval_id",
            json!(approval_id),
        );
    }
    if let Some(reason) = wait.reason() {
        gate.insert("reason".into(), json!(reason));
    }
    insert_workflow_human_gate(&mut detail, gate);
    append_workflow_pending_approval_detail(store, run_id, Value::Object(detail))
}

pub(super) fn record_workflow_approval_resumed(
    store: &AppStore,
    run_id: &str,
    mode: WorkflowMode,
    server_id: &str,
    tool_name: &str,
    approval_id: Option<&str>,
    status: Option<&str>,
    reason: Option<&str>,
) -> AppResult<()> {
    let mut detail = workflow_turn_detail(mode, None);
    insert_workflow_detail_alias_value(&mut detail, "serverId", "server_id", json!(server_id));
    insert_workflow_detail_alias_value(&mut detail, "toolName", "tool_name", json!(tool_name));
    if let Some(approval_id) = approval_id {
        detail.insert("approvalId".into(), json!(approval_id));
    }
    if let Some(status) = status {
        detail.insert("status".into(), json!(status));
    }
    if let Some(reason) = reason {
        detail.insert("reason".into(), json!(reason));
    }
    let mut gate = workflow_human_gate_detail("tool_approval", status.unwrap_or("resumed"), run_id);
    gate.insert("mode".into(), json!(mode.as_str()));
    insert_workflow_detail_alias_value(&mut gate, "serverId", "server_id", json!(server_id));
    insert_workflow_detail_alias_value(&mut gate, "toolName", "tool_name", json!(tool_name));
    if let Some(approval_id) = approval_id {
        insert_workflow_detail_alias_value(
            &mut gate,
            "approvalId",
            "approval_id",
            json!(approval_id),
        );
    }
    if let Some(reason) = reason {
        gate.insert("reason".into(), json!(reason));
    }
    insert_workflow_human_gate(&mut detail, gate);
    append_workflow_node_event(
        store,
        run_id,
        WorkflowNodeName::Approval,
        WorkflowNodeStatus::Completed,
        Value::Object(detail.clone()),
    )?;
    append_workflow_transition_event(
        store,
        run_id,
        WorkflowNodeName::Approval,
        WorkflowNodeName::Planner,
        WORKFLOW_REASON_APPROVAL_RESUMED,
        Value::Object(detail),
    )
}

pub(super) fn record_workflow_approval_resolved(
    store: &AppStore,
    run_id: &str,
    mode: WorkflowMode,
    server_id: &str,
    tool_name: &str,
    approval_id: Option<&str>,
    status: &str,
    reason: Option<&str>,
    error: Option<&str>,
) -> AppResult<()> {
    let mut detail = workflow_turn_detail(mode, None);
    insert_workflow_detail_alias_value(&mut detail, "serverId", "server_id", json!(server_id));
    insert_workflow_detail_alias_value(&mut detail, "toolName", "tool_name", json!(tool_name));
    detail.insert("status".into(), json!(status));
    if let Some(approval_id) = approval_id {
        detail.insert("approvalId".into(), json!(approval_id));
    }
    if let Some(reason) = reason {
        detail.insert("reason".into(), json!(reason));
    }
    if let Some(error) = error {
        detail.insert("error".into(), json!(error));
    }
    let mut gate = workflow_human_gate_detail("tool_approval", status, run_id);
    gate.insert("mode".into(), json!(mode.as_str()));
    insert_workflow_detail_alias_value(&mut gate, "serverId", "server_id", json!(server_id));
    insert_workflow_detail_alias_value(&mut gate, "toolName", "tool_name", json!(tool_name));
    if let Some(approval_id) = approval_id {
        insert_workflow_detail_alias_value(
            &mut gate,
            "approvalId",
            "approval_id",
            json!(approval_id),
        );
    }
    if let Some(reason) = reason {
        gate.insert("reason".into(), json!(reason));
    }
    if let Some(error) = error {
        gate.insert("error".into(), json!(error));
    }
    insert_workflow_human_gate(&mut detail, gate);
    let node_status = match status {
        "failed" => WorkflowNodeStatus::Failed,
        "denied" | "canceled" | "cancelled" => WorkflowNodeStatus::Canceled,
        _ => WorkflowNodeStatus::Completed,
    };
    append_workflow_node_event(
        store,
        run_id,
        WorkflowNodeName::Approval,
        node_status,
        Value::Object(detail),
    )
}

pub(super) fn record_workflow_planner_running(
    store: &AppStore,
    run_id: &str,
    iteration: u32,
    mode: WorkflowMode,
) -> AppResult<()> {
    append_workflow_node_event(
        store,
        run_id,
        WorkflowNodeName::Planner,
        WorkflowNodeStatus::Running,
        Value::Object(workflow_turn_detail(mode, Some(iteration))),
    )
}

fn record_workflow_planner_failed(
    store: &AppStore,
    run_id: &str,
    iteration: Option<u32>,
    mode: WorkflowMode,
    error_kind: WorkflowPlannerErrorKind,
    error: &str,
) -> AppResult<()> {
    let mut detail = workflow_turn_detail(mode, iteration);
    detail.insert("errorKind".into(), json!(error_kind.as_str()));
    detail.insert("error".into(), json!(error));
    append_workflow_node_event(
        store,
        run_id,
        WorkflowNodeName::Planner,
        WorkflowNodeStatus::Failed,
        Value::Object(detail),
    )
}

pub(super) fn record_workflow_planner_to_executor(
    store: &AppStore,
    run_id: &str,
    iteration: u32,
    mode: WorkflowMode,
    tool_count: usize,
    tool_names: &[String],
    tool_calls: &[AgentToolCall],
) -> AppResult<()> {
    let mut planner_detail = workflow_turn_detail(mode, Some(iteration));
    planner_detail.insert("action".into(), json!("tool"));
    planner_detail.insert("toolCount".into(), json!(tool_count));
    planner_detail.insert("tools".into(), json!(tool_names));
    insert_workflow_tool_call_protocol_detail(&mut planner_detail, tool_calls);
    append_workflow_node_event(
        store,
        run_id,
        WorkflowNodeName::Planner,
        WorkflowNodeStatus::Completed,
        Value::Object(planner_detail),
    )?;

    let mut transition_detail = workflow_turn_detail(mode, Some(iteration));
    transition_detail.insert("toolCount".into(), json!(tool_count));
    transition_detail.insert("tools".into(), json!(tool_names));
    insert_workflow_tool_call_protocol_detail(&mut transition_detail, tool_calls);
    append_workflow_transition_event(
        store,
        run_id,
        WorkflowNodeName::Planner,
        WorkflowNodeName::Executor,
        WORKFLOW_REASON_TOOL_CALLS,
        Value::Object(transition_detail),
    )?;

    let mut executor_detail = workflow_turn_detail(mode, Some(iteration));
    executor_detail.insert("toolCount".into(), json!(tool_count));
    executor_detail.insert("tools".into(), json!(tool_names));
    insert_workflow_tool_call_protocol_detail(&mut executor_detail, tool_calls);
    append_workflow_node_event(
        store,
        run_id,
        WorkflowNodeName::Executor,
        WorkflowNodeStatus::Running,
        Value::Object(executor_detail),
    )
}

fn insert_workflow_tool_call_protocol_detail(
    detail: &mut Map<String, Value>,
    tool_calls: &[AgentToolCall],
) {
    if tool_calls.is_empty() {
        return;
    }
    let origins = tool_calls
        .iter()
        .map(|call| call.origin.as_str())
        .collect::<Vec<_>>();
    let call_ids = tool_calls
        .iter()
        .filter_map(|call| call.id.as_deref())
        .collect::<Vec<_>>();
    let summaries = tool_calls
        .iter()
        .map(|call| {
            let mut summary = Map::new();
            summary.insert("name".into(), json!(&call.name));
            summary.insert("origin".into(), json!(call.origin.as_str()));
            if let Some(id) = call.id.as_deref() {
                summary.insert("id".into(), json!(id));
            }
            if call.provider_meta.is_some() {
                summary.insert("providerNative".into(), json!(true));
                summary.insert("provider_native".into(), json!(true));
            }
            Value::Object(summary)
        })
        .collect::<Vec<_>>();

    detail.insert("toolProtocol".into(), json!("canonical_tool_call_v1"));
    detail.insert("tool_protocol".into(), json!("canonical_tool_call_v1"));
    detail.insert("toolOrigins".into(), json!(origins.clone()));
    detail.insert("tool_origins".into(), json!(origins));
    detail.insert("toolCallIds".into(), json!(call_ids.clone()));
    detail.insert("tool_call_ids".into(), json!(call_ids));
    detail.insert("toolCalls".into(), Value::Array(summaries.clone()));
    detail.insert("tool_calls".into(), Value::Array(summaries));
}

pub(super) fn record_workflow_executor_to_planner(
    store: &AppStore,
    run_id: &str,
    iteration: u32,
    mode: WorkflowMode,
    tool_count: usize,
    parallel: Option<bool>,
) -> AppResult<()> {
    let mut executor_detail = workflow_turn_detail(mode, Some(iteration));
    executor_detail.insert("toolCount".into(), json!(tool_count));
    if let Some(parallel) = parallel {
        executor_detail.insert("parallel".into(), json!(parallel));
    }
    if let Some(bridge_detail) = workflow_latest_executor_bridge_target_detail(store, run_id)? {
        append_workflow_bridge_target_completion_detail(&mut executor_detail, &bridge_detail);
    }
    append_workflow_node_event(
        store,
        run_id,
        WorkflowNodeName::Executor,
        WorkflowNodeStatus::Completed,
        Value::Object(executor_detail.clone()),
    )?;
    append_workflow_transition_event(
        store,
        run_id,
        WorkflowNodeName::Executor,
        WorkflowNodeName::Planner,
        WORKFLOW_REASON_TOOL_OBSERVATIONS_RECORDED,
        Value::Object(executor_detail),
    )
}

fn workflow_latest_executor_bridge_target_detail(
    store: &AppStore,
    run_id: &str,
) -> AppResult<Option<Map<String, Value>>> {
    let run = store.agent_run(run_id)?;
    let cycle_start = workflow_latest_transition_sequence(&run);
    if let Some(detail) = run.phase_events.iter().rev().find_map(|event| {
        workflow_executor_bridge_target_detail_from_phase_event(event, cycle_start)
    }) {
        return Ok(Some(detail));
    }
    let detail = run
        .workflow_graph
        .as_ref()
        .and_then(|graph| graph.get("nodes"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|node| node.get("node").and_then(Value::as_str) == Some("executor"))
        .and_then(|node| node.get("detail"))
        .and_then(Value::as_object)
        .filter(|detail| {
            detail.get("stage").and_then(Value::as_str) == Some("tool_call_bridge_target")
        })
        .cloned();
    Ok(detail)
}

fn workflow_latest_transition_sequence(run: &AgentRunRecord) -> Option<u64> {
    run.phase_events.iter().rev().find_map(|event| {
        if event.phase != WORKFLOW_PHASE_TRANSITION {
            return None;
        }
        workflow_event_sequence_u64(&event.detail)
    })
}

fn workflow_executor_bridge_target_detail_from_phase_event(
    event: &AgentRunPhaseRecord,
    cycle_start: Option<u64>,
) -> Option<Map<String, Value>> {
    if event.phase != WORKFLOW_PHASE_NODE {
        return None;
    }
    let sequence = workflow_event_sequence_u64(&event.detail)?;
    if let Some(start) = cycle_start {
        if sequence <= start {
            return None;
        }
    }
    let object = event.detail.as_object()?;
    if object.get("node").and_then(Value::as_str) != Some("executor") {
        return None;
    }
    let detail = object.get("detail")?.as_object()?;
    if detail.get("stage").and_then(Value::as_str) == Some("tool_call_bridge_target") {
        Some(detail.clone())
    } else {
        None
    }
}

fn append_workflow_bridge_target_completion_detail(
    executor_detail: &mut Map<String, Value>,
    bridge_detail: &Map<String, Value>,
) {
    let mut last_bridge_target = Map::new();
    append_workflow_bridge_target_completion_field(
        executor_detail,
        &mut last_bridge_target,
        bridge_detail,
        "source",
    );
    append_workflow_bridge_target_completion_alias_pair(
        executor_detail,
        &mut last_bridge_target,
        bridge_detail,
        "directBridge",
        "direct_bridge",
    );
    append_workflow_bridge_target_completion_alias_pair(
        executor_detail,
        &mut last_bridge_target,
        bridge_detail,
        "requestedName",
        "requested_name",
    );
    append_workflow_bridge_target_completion_alias_pair(
        executor_detail,
        &mut last_bridge_target,
        bridge_detail,
        "serverId",
        "server_id",
    );
    append_workflow_bridge_target_completion_alias_pair(
        executor_detail,
        &mut last_bridge_target,
        bridge_detail,
        "toolName",
        "tool_name",
    );
    append_workflow_bridge_target_completion_alias_pair(
        executor_detail,
        &mut last_bridge_target,
        bridge_detail,
        "toolKind",
        "tool_kind",
    );
    append_workflow_bridge_target_completion_alias_pair(
        executor_detail,
        &mut last_bridge_target,
        bridge_detail,
        "sourceLabel",
        "source_label",
    );
    append_workflow_bridge_target_completion_alias_pair(
        executor_detail,
        &mut last_bridge_target,
        bridge_detail,
        "requiresApproval",
        "requires_approval",
    );
    append_workflow_bridge_target_completion_alias_pair(
        executor_detail,
        &mut last_bridge_target,
        bridge_detail,
        "approvedToolCallReplay",
        "approved_tool_call_replay",
    );
    append_workflow_bridge_target_completion_alias_pair(
        executor_detail,
        &mut last_bridge_target,
        bridge_detail,
        "bridgeStatus",
        "bridge_status",
    );
    append_workflow_bridge_target_completion_alias_pair(
        executor_detail,
        &mut last_bridge_target,
        bridge_detail,
        "bridgeRejectionReason",
        "bridge_rejection_reason",
    );
    if !last_bridge_target.is_empty() {
        last_bridge_target.insert("stage".into(), json!("tool_call_bridge_target"));
        executor_detail.insert("bridgeStage".into(), json!("tool_call_bridge_target"));
        executor_detail.insert("bridge_stage".into(), json!("tool_call_bridge_target"));
        let target = Value::Object(last_bridge_target);
        executor_detail.insert("lastBridgeTarget".into(), target.clone());
        executor_detail.insert("last_bridge_target".into(), target);
    }
}

fn append_workflow_bridge_target_completion_field(
    executor_detail: &mut Map<String, Value>,
    last_bridge_target: &mut Map<String, Value>,
    bridge_detail: &Map<String, Value>,
    key: &str,
) {
    if let Some(value) = bridge_detail.get(key).cloned() {
        executor_detail.insert(key.into(), value.clone());
        last_bridge_target.insert(key.into(), value);
    }
}

fn append_workflow_bridge_target_completion_alias_pair(
    executor_detail: &mut Map<String, Value>,
    last_bridge_target: &mut Map<String, Value>,
    bridge_detail: &Map<String, Value>,
    camel_key: &str,
    snake_key: &str,
) {
    if let Some(value) = bridge_detail
        .get(camel_key)
        .or_else(|| bridge_detail.get(snake_key))
        .cloned()
    {
        executor_detail.insert(camel_key.into(), value.clone());
        executor_detail.insert(snake_key.into(), value.clone());
        last_bridge_target.insert(camel_key.into(), value.clone());
        last_bridge_target.insert(snake_key.into(), value);
    }
}

pub(super) fn record_workflow_executor_parallel_batch_started(
    store: &AppStore,
    run_id: &str,
    iteration: u32,
    mode: WorkflowMode,
    tool_count: usize,
    tool_names: &[String],
) -> AppResult<()> {
    let mut detail = workflow_turn_detail(mode, Some(iteration));
    detail.insert("stage".into(), json!("parallel_batch_started"));
    detail.insert("toolCount".into(), json!(tool_count));
    detail.insert("tools".into(), json!(tool_names));
    detail.insert("parallel".into(), json!(true));
    append_workflow_node_event(
        store,
        run_id,
        WorkflowNodeName::Executor,
        WorkflowNodeStatus::Running,
        Value::Object(detail),
    )
}

pub(super) fn record_workflow_executor_parallel_batch_completed(
    store: &AppStore,
    run_id: &str,
    iteration: u32,
    mode: WorkflowMode,
    tool_count: usize,
    tool_names: &[String],
    succeeded: usize,
    failed: usize,
    halted: bool,
) -> AppResult<()> {
    let mut detail = workflow_turn_detail(mode, Some(iteration));
    detail.insert("stage".into(), json!("parallel_batch_completed"));
    detail.insert("toolCount".into(), json!(tool_count));
    detail.insert("tools".into(), json!(tool_names));
    detail.insert("succeeded".into(), json!(succeeded));
    detail.insert("failed".into(), json!(failed));
    detail.insert("halted".into(), json!(halted));
    detail.insert("parallel".into(), json!(true));
    if let Some(bridge_detail) = workflow_latest_executor_bridge_target_detail(store, run_id)? {
        append_workflow_bridge_target_completion_detail(&mut detail, &bridge_detail);
    }
    append_workflow_node_event(
        store,
        run_id,
        WorkflowNodeName::Executor,
        WorkflowNodeStatus::Completed,
        Value::Object(detail),
    )
}

pub(super) fn record_workflow_executor_tool_started(
    store: &AppStore,
    run_id: &str,
    iteration: u32,
    mode: WorkflowMode,
    identity: &WorkflowExecutorToolIdentity,
) -> AppResult<()> {
    let mut detail = workflow_turn_detail(mode, Some(iteration));
    detail.insert("stage".into(), json!("tool_started"));
    append_workflow_tool_identity_detail(&mut detail, identity);
    append_workflow_node_event(
        store,
        run_id,
        WorkflowNodeName::Executor,
        WorkflowNodeStatus::Running,
        Value::Object(detail),
    )
}

pub(super) fn record_workflow_executor_tool_call_bridge_target(
    store: &AppStore,
    run_id: &str,
    mode: WorkflowMode,
    target_name: &str,
    server_id: &str,
    tool_name: &str,
    tool_kind: &str,
    requires_approval: bool,
    approved_replay_context: bool,
    bridge_status: &str,
    bridge_rejection_reason: Option<&str>,
) -> AppResult<()> {
    let mut detail = workflow_turn_detail(mode, None);
    detail.insert("stage".into(), json!("tool_call_bridge_target"));
    detail.insert("source".into(), json!("tool_call"));
    detail.insert("directBridge".into(), json!(true));
    detail.insert("direct_bridge".into(), json!(true));
    insert_workflow_detail_alias_value(
        &mut detail,
        "requestedName",
        "requested_name",
        json!(target_name),
    );
    insert_workflow_detail_alias_value(&mut detail, "serverId", "server_id", json!(server_id));
    insert_workflow_detail_alias_value(&mut detail, "toolName", "tool_name", json!(tool_name));
    insert_workflow_detail_alias_value(&mut detail, "toolKind", "tool_kind", json!(tool_kind));
    insert_workflow_detail_alias_value(
        &mut detail,
        "sourceLabel",
        "source_label",
        json!(if server_id == WORKFLOW_INTERNAL_TOOL_SERVER_ID {
            tool_name.to_string()
        } else if server_id == "<missing>" {
            target_name.to_string()
        } else {
            format!("{server_id}:{tool_name}")
        }),
    );
    insert_workflow_detail_alias_value(
        &mut detail,
        "requiresApproval",
        "requires_approval",
        json!(requires_approval),
    );
    detail.insert(
        "approvedToolCallReplay".into(),
        json!(approved_replay_context),
    );
    detail.insert(
        "approved_tool_call_replay".into(),
        json!(approved_replay_context),
    );
    detail.insert("bridgeStatus".into(), json!(bridge_status));
    detail.insert("bridge_status".into(), json!(bridge_status));
    if let Some(reason) = bridge_rejection_reason.filter(|reason| !reason.trim().is_empty()) {
        detail.insert("bridgeRejectionReason".into(), json!(reason));
        detail.insert("bridge_rejection_reason".into(), json!(reason));
    }
    append_workflow_node_event(
        store,
        run_id,
        WorkflowNodeName::Executor,
        WorkflowNodeStatus::Running,
        Value::Object(detail),
    )
}

pub(super) fn record_workflow_executor_tool_resolution(
    store: &AppStore,
    run_id: &str,
    iteration: u32,
    mode: WorkflowMode,
    resolution: &WorkflowExecutorToolResolution,
) -> AppResult<()> {
    let mut detail = workflow_turn_detail(mode, Some(iteration));
    let status = match resolution {
        WorkflowExecutorToolResolution::Internal(identity) => {
            detail.insert("resolution".into(), json!("internal"));
            detail.insert("available".into(), json!(true));
            append_workflow_tool_identity_detail(&mut detail, identity);
            WorkflowNodeStatus::Running
        }
        WorkflowExecutorToolResolution::Mcp {
            identity,
            definition,
        } => {
            detail.insert("resolution".into(), json!("mcp"));
            detail.insert("available".into(), json!(true));
            detail.insert("definitionName".into(), json!(definition.name.as_str()));
            insert_workflow_detail_alias_value(
                &mut detail,
                "requiresApproval",
                "requires_approval",
                json!(definition.requires_approval),
            );
            append_workflow_tool_identity_detail(&mut detail, identity);
            WorkflowNodeStatus::Running
        }
        WorkflowExecutorToolResolution::Unavailable { requested_name } => {
            detail.insert("resolution".into(), json!("unavailable"));
            detail.insert("available".into(), json!(false));
            insert_workflow_detail_alias_value(
                &mut detail,
                "requestedName",
                "requested_name",
                json!(requested_name),
            );
            WorkflowNodeStatus::Failed
        }
    };
    append_workflow_node_event(
        store,
        run_id,
        WorkflowNodeName::Executor,
        status,
        Value::Object(detail),
    )
}

pub(super) fn record_workflow_executor_failed(
    store: &AppStore,
    run_id: &str,
    mode: WorkflowMode,
    iteration: Option<u32>,
    requested_name: &str,
    server_id: &str,
    tool_name: &str,
    error: &str,
) -> AppResult<()> {
    let mut detail = workflow_turn_detail(mode, iteration);
    insert_workflow_detail_alias_value(
        &mut detail,
        "requestedName",
        "requested_name",
        json!(requested_name),
    );
    insert_workflow_detail_alias_value(&mut detail, "serverId", "server_id", json!(server_id));
    insert_workflow_detail_alias_value(&mut detail, "toolName", "tool_name", json!(tool_name));
    insert_workflow_detail_alias_value(
        &mut detail,
        "toolKind",
        "tool_kind",
        json!(if server_id == WORKFLOW_INTERNAL_TOOL_SERVER_ID {
            "internal"
        } else {
            "mcp"
        }),
    );
    detail.insert("error".into(), json!(error));
    append_workflow_node_event(
        store,
        run_id,
        WorkflowNodeName::Executor,
        WorkflowNodeStatus::Failed,
        Value::Object(detail),
    )
}

pub(super) fn record_workflow_executor_approval_policy(
    store: &AppStore,
    run_id: &str,
    iteration: u32,
    mode: WorkflowMode,
    identity: &WorkflowExecutorToolIdentity,
    reason: Option<&str>,
) -> AppResult<()> {
    record_workflow_executor_approval_policy_stage(
        store,
        run_id,
        iteration,
        mode,
        identity,
        WorkflowExecutorApprovalPolicyStage::Final,
        reason,
    )
}

pub(super) fn record_workflow_executor_approval_policy_stage(
    store: &AppStore,
    run_id: &str,
    iteration: u32,
    mode: WorkflowMode,
    identity: &WorkflowExecutorToolIdentity,
    stage: WorkflowExecutorApprovalPolicyStage,
    reason: Option<&str>,
) -> AppResult<()> {
    let mut detail = workflow_turn_detail(mode, Some(iteration));
    detail.insert("stage".into(), json!(stage.as_str()));
    insert_workflow_detail_alias_value(
        &mut detail,
        "requiresApproval",
        "requires_approval",
        json!(reason.is_some()),
    );
    if let Some(reason) = reason {
        detail.insert("reason".into(), json!(reason));
    }
    append_workflow_tool_identity_detail(&mut detail, identity);
    append_workflow_node_event(
        store,
        run_id,
        WorkflowNodeName::Executor,
        WorkflowNodeStatus::Running,
        Value::Object(detail),
    )
}

pub(super) fn record_workflow_planner_to_reviewer(
    store: &AppStore,
    run_id: &str,
    iteration: u32,
    mode: WorkflowMode,
) -> AppResult<()> {
    record_workflow_planner_to_reviewer_with_completion_gate(store, run_id, iteration, mode, None)
}

fn record_workflow_planner_to_reviewer_with_completion_gate(
    store: &AppStore,
    run_id: &str,
    iteration: u32,
    mode: WorkflowMode,
    completion_gate: Option<&CompletionGateAssessment>,
) -> AppResult<()> {
    let mut planner_detail = workflow_turn_detail(mode, Some(iteration));
    planner_detail.insert("action".into(), json!("final"));
    append_workflow_node_event(
        store,
        run_id,
        WorkflowNodeName::Planner,
        WorkflowNodeStatus::Completed,
        Value::Object(planner_detail),
    )?;
    append_workflow_transition_event(
        store,
        run_id,
        WorkflowNodeName::Planner,
        WorkflowNodeName::CompletionGate,
        WORKFLOW_REASON_FINAL_ANSWER_CANDIDATE,
        Value::Object(workflow_turn_detail(mode, Some(iteration))),
    )?;
    let mut gate_detail = workflow_turn_detail(mode, Some(iteration));
    gate_detail.insert("decision".into(), json!("accepted"));
    if let Some(assessment) = completion_gate {
        gate_detail.insert("completionGate".into(), assessment.to_value());
        gate_detail.insert("completion_gate".into(), assessment.to_value());
    }
    append_workflow_node_event(
        store,
        run_id,
        WorkflowNodeName::CompletionGate,
        WorkflowNodeStatus::Running,
        Value::Object(gate_detail.clone()),
    )?;
    append_workflow_node_event(
        store,
        run_id,
        WorkflowNodeName::CompletionGate,
        WorkflowNodeStatus::Completed,
        Value::Object(gate_detail),
    )?;
    append_workflow_transition_event(
        store,
        run_id,
        WorkflowNodeName::CompletionGate,
        WorkflowNodeName::Reviewer,
        WORKFLOW_REASON_COMPLETION_GATE_PASSED,
        Value::Object(workflow_turn_detail(mode, Some(iteration))),
    )?;
    append_workflow_node_event(
        store,
        run_id,
        WorkflowNodeName::Reviewer,
        WorkflowNodeStatus::Running,
        Value::Object(workflow_turn_detail(mode, Some(iteration))),
    )
}

fn record_workflow_completion_gate_rejected(
    store: &AppStore,
    run_id: &str,
    iteration: u32,
    mode: WorkflowMode,
    assessment: &CompletionGateAssessment,
    observation: &str,
) -> AppResult<()> {
    append_workflow_transition_event(
        store,
        run_id,
        WorkflowNodeName::Planner,
        WorkflowNodeName::CompletionGate,
        WORKFLOW_REASON_FINAL_ANSWER_CANDIDATE,
        Value::Object(workflow_turn_detail(mode, Some(iteration))),
    )?;
    let mut detail = workflow_turn_detail(mode, Some(iteration));
    detail.insert("decision".into(), json!("rejected"));
    detail.insert("reason".into(), json!(observation));
    detail.insert("completionGate".into(), assessment.to_value());
    detail.insert("completion_gate".into(), assessment.to_value());
    append_workflow_node_event(
        store,
        run_id,
        WorkflowNodeName::CompletionGate,
        WorkflowNodeStatus::Running,
        Value::Object(detail.clone()),
    )?;
    append_workflow_node_event(
        store,
        run_id,
        WorkflowNodeName::CompletionGate,
        WorkflowNodeStatus::Failed,
        Value::Object(detail),
    )
}

pub(super) fn record_workflow_reviewer_completed(
    store: &AppStore,
    run_id: &str,
    mode: WorkflowMode,
    message_id: &str,
    model: Option<&str>,
    provider_id: Option<&str>,
) -> AppResult<()> {
    let mut detail = workflow_turn_detail(mode, None);
    detail.insert("messageId".into(), json!(message_id));
    detail.insert("model".into(), json!(model));
    detail.insert("providerId".into(), json!(provider_id));
    append_workflow_node_event(
        store,
        run_id,
        WorkflowNodeName::Reviewer,
        WorkflowNodeStatus::Completed,
        Value::Object(detail),
    )
}

pub(super) fn record_workflow_reviewer_skipped(
    store: &AppStore,
    run_id: &str,
    mode: WorkflowMode,
    message_id: &str,
    reason: &str,
    model: Option<&str>,
    provider_id: Option<&str>,
) -> AppResult<()> {
    let mut detail = workflow_turn_detail(mode, None);
    detail.insert("messageId".into(), json!(message_id));
    detail.insert("reason".into(), json!(reason));
    detail.insert("model".into(), json!(model));
    detail.insert("providerId".into(), json!(provider_id));
    if workflow_run_current_status_is_failure(store, run_id)? {
        insert_workflow_detail_alias_value(
            &mut detail,
            "preserveCurrent",
            "preserve_current",
            json!(true),
        );
    }
    append_workflow_node_event(
        store,
        run_id,
        WorkflowNodeName::Reviewer,
        WorkflowNodeStatus::Skipped,
        Value::Object(detail),
    )
}

fn workflow_run_current_status_is_failure(store: &AppStore, run_id: &str) -> AppResult<bool> {
    let run = store.agent_run(run_id)?;
    Ok(workflow_graph_current_status_is_failure(
        run.workflow_graph.as_ref(),
    ))
}

fn workflow_graph_current_status_is_failure(graph: Option<&Value>) -> bool {
    let Some(object) = graph.and_then(Value::as_object) else {
        return false;
    };
    let Some(current_node) = object
        .get("currentNode")
        .or_else(|| object.get("current_node"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|node| !node.is_empty())
    else {
        return false;
    };
    let status = object
        .get("currentStatus")
        .or_else(|| object.get("current_status"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            workflow_current_status_for_target(object, current_node)
                .as_str()
                .map(str::to_string)
        });
    matches!(status.as_deref(), Some("failed" | "canceled" | "cancelled"))
}

pub(super) fn record_workflow_timeout(
    store: &AppStore,
    run_id: &str,
    mode: WorkflowMode,
    reason: &str,
    timeout_seconds: u64,
) -> AppResult<()> {
    let run = store.agent_run(run_id)?;
    let node = run
        .workflow_graph
        .as_ref()
        .and_then(|graph| graph.get("currentNode"))
        .or_else(|| {
            run.workflow_graph
                .as_ref()
                .and_then(|graph| graph.get("current_node"))
        })
        .and_then(Value::as_str)
        .and_then(WorkflowNodeName::from_str)
        .unwrap_or(WorkflowNodeName::Planner);
    let mut detail = workflow_turn_detail(mode, None);
    detail.insert("errorKind".into(), json!("agent_run_timeout"));
    detail.insert("reason".into(), json!(reason));
    detail.insert("timeoutSeconds".into(), json!(timeout_seconds));
    // Mark the active node as failed.
    append_workflow_node_event(
        store,
        run_id,
        node,
        WorkflowNodeStatus::Failed,
        Value::Object(detail.clone()),
    )?;
    // Mark every other node that is still pending/waiting as canceled so that
    // observers do not see dangling "pending" nodes after the run terminates.
    // A node stuck in "waiting" state (e.g. Approval awaiting human input)
    // is especially misleading if left uncleaned after a timeout.
    let all_nodes = [
        WorkflowNodeName::Planner,
        WorkflowNodeName::Executor,
        WorkflowNodeName::Approval,
        WorkflowNodeName::Checkpoint,
        WorkflowNodeName::Reviewer,
    ];
    let cancel_detail = {
        let mut d = workflow_turn_detail(mode, None);
        d.insert("canceledBy".into(), json!("run_timeout"));
        d
    };
    let graph_map = run
        .workflow_graph
        .as_ref()
        .and_then(Value::as_object);
    for other in all_nodes {
        if other == node {
            continue;
        }
        let other_status = graph_map
            .map(|m| workflow_current_status_for_target(m, other.as_str()))
            .and_then(|v| v.as_str().map(str::to_string));
        if matches!(other_status.as_deref(), Some("pending") | Some("waiting") | None) {
            let _ = append_workflow_node_event(
                store,
                run_id,
                other,
                WorkflowNodeStatus::Canceled,
                Value::Object(cancel_detail.clone()),
            );
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskRequirement {
    WebResult,
    ImageArtifact,
    FileArtifact,
    MessageDelivery,
    WechatDelivery,
}

impl TaskRequirement {
    fn as_str(self) -> &'static str {
        match self {
            Self::WebResult => "web_result",
            Self::ImageArtifact => "image_artifact",
            Self::FileArtifact => "file_artifact",
            Self::MessageDelivery => "message_sent",
            Self::WechatDelivery => "wechat_delivered",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::WebResult => "search or web evidence",
            Self::ImageArtifact => "generated image artifact",
            Self::FileArtifact => "created or modified file/artifact",
            Self::MessageDelivery => "sent message evidence",
            Self::WechatDelivery => "WeChat delivery evidence",
        }
    }

    fn is_satisfied_by(self, evidence: &EvidenceRegistry) -> bool {
        match self {
            Self::WebResult => evidence.web_result,
            Self::ImageArtifact => evidence.image_generated || evidence.artifact_ready,
            Self::FileArtifact => evidence.file_created || evidence.artifact_ready,
            Self::MessageDelivery => evidence.message_sent || evidence.wechat_delivered,
            Self::WechatDelivery => evidence.wechat_delivered || evidence.message_sent,
        }
    }

    fn is_attempted_by(self, tool_name: &str) -> bool {
        match self {
            Self::WebResult => {
                matches!(
                    tool_name,
                    "web_search" | "x_search" | "web_extract" | "web_request"
                ) || tool_name.starts_with("browser_")
            }
            Self::ImageArtifact => tool_name == "image_generate",
            Self::FileArtifact => matches!(
                tool_name,
                "write_file"
                    | "patch"
                    | "artifact"
                    | "document"
                    | "image_generate"
                    | "video_generate"
                    | "text_to_speech"
            ),
            Self::MessageDelivery | Self::WechatDelivery => tool_name == "send_message",
        }
    }
}

#[derive(Debug, Clone, Default)]
struct TaskContract {
    requirements: Vec<TaskRequirement>,
}

impl TaskContract {
    fn from_run(run: &AgentRunRecord) -> Self {
        let mut contract = Self::default();
        let request = run.user_request.as_str();
        let normalized = request.to_ascii_lowercase();
        let capability_inquiry = request_is_capability_inquiry(request, &normalized);

        if !capability_inquiry
            && (text_contains_any(
                request,
                &[
                    "搜索", "查询", "查找", "联网", "网上", "最新", "今天", "新闻", "价格",
                ],
            ) || ascii_contains_any(
                &normalized,
                &[
                    "search", "web", "look up", "latest", "today", "news", "price",
                ],
            ))
        {
            contract.require(TaskRequirement::WebResult);
        }

        if !capability_inquiry
            && (text_contains_any(request, &["生图", "生成图片", "画一张", "绘制", "图片生成"])
                || ascii_contains_any(
                    &normalized,
                    &["generate image", "draw an image", "image generation"],
                ))
        {
            contract.require(TaskRequirement::ImageArtifact);
        }

        let asks_for_file = !capability_inquiry
            && ((text_contains_any(request, &["生成", "创建", "导出", "保存", "写入", "下载"])
                && text_contains_any(
                    request,
                    &[
                        "文件", "文档", "桌面", "报告", "Word", "PDF", "Excel", "PPT", "docx",
                        "xlsx", "pptx",
                    ],
                ))
                || ascii_contains_any(
                    &normalized,
                    &[
                        "create file",
                        "save file",
                        "export",
                        "download",
                        "generate word",
                        "generate pdf",
                        "save to desktop",
                    ],
                ));
        if asks_for_file {
            contract.require(TaskRequirement::FileArtifact);
        }

        let asks_for_delivery = !capability_inquiry
            && (text_contains_any(request, &["发送", "发给", "通知", "转发", "推送"])
                || ascii_contains_any(&normalized, &["send", "deliver", "notify", "message"]));
        let mentions_wechat = text_contains_any(request, &["微信", "企微", "微信群"])
            || ascii_contains_any(&normalized, &["wechat", "weixin", "wecom"]);
        let mentions_messaging = mentions_wechat
            || text_contains_any(
                request,
                &["邮箱", "邮件", "短信", "飞书", "钉钉", "群", "Teams"],
            )
            || ascii_contains_any(
                &normalized,
                &["email", "sms", "slack", "telegram", "discord", "teams"],
            );
        if asks_for_delivery && mentions_messaging {
            contract.require(TaskRequirement::MessageDelivery);
        }
        if asks_for_delivery && mentions_wechat {
            contract.require(TaskRequirement::WechatDelivery);
        }

        if !capability_inquiry {
            for event in &run.tool_events {
                match tool_event_name(event).as_str() {
                    "web_search" | "x_search" | "web_extract" | "web_request" => {
                        contract.require(TaskRequirement::WebResult)
                    }
                    "image_generate" => contract.require(TaskRequirement::ImageArtifact),
                    "write_file" | "patch" | "artifact" | "document" | "text_to_speech"
                    | "video_generate" => contract.require(TaskRequirement::FileArtifact),
                    "send_message" => contract.require(TaskRequirement::MessageDelivery),
                    _ => {}
                }
            }
        }

        contract
    }

    fn require(&mut self, requirement: TaskRequirement) {
        if !self.requirements.contains(&requirement) {
            self.requirements.push(requirement);
        }
    }

    fn unsatisfied(&self, evidence: &EvidenceRegistry) -> Vec<TaskRequirement> {
        self.requirements
            .iter()
            .copied()
            .filter(|requirement| !requirement.is_satisfied_by(evidence))
            .collect()
    }

    fn to_value(&self) -> Value {
        json!({
            "requirements": self.requirements.iter().map(|requirement| requirement.as_str()).collect::<Vec<_>>(),
            "descriptions": self.requirements.iter().map(|requirement| requirement.description()).collect::<Vec<_>>(),
        })
    }
}

#[derive(Debug, Clone, Default)]
struct EvidenceRegistry {
    web_result: bool,
    image_generated: bool,
    file_created: bool,
    artifact_ready: bool,
    message_sent: bool,
    wechat_delivered: bool,
    running_tools: Vec<String>,
    failed_tools: Vec<String>,
}

impl EvidenceRegistry {
    fn from_run(run: &AgentRunRecord) -> Self {
        let mut evidence = Self::default();
        for event in latest_tool_events(run) {
            let tool_name = tool_event_name(event);
            let status = tool_event_status(event);
            if matches!(status, Some("running" | "pending")) {
                evidence
                    .running_tools
                    .push(tool_event_label(event, &tool_name));
                continue;
            }
            if matches!(status, Some("failed")) {
                evidence
                    .failed_tools
                    .push(tool_event_label(event, &tool_name));
                continue;
            }
            if !tool_event_completed_ok(event) {
                continue;
            }

            let name = tool_name.as_str();
            if matches!(
                name,
                "web_search" | "x_search" | "web_extract" | "web_request"
            ) || name.starts_with("browser_")
            {
                evidence.web_result = true;
            }
            if name == "image_generate" {
                evidence.image_generated = true;
                evidence.artifact_ready = true;
            }
            if matches!(name, "write_file" | "patch") {
                evidence.file_created = true;
            }
            if matches!(
                name,
                "artifact" | "document" | "image_generate" | "video_generate" | "text_to_speech"
            ) || tool_event_has_artifact(event)
            {
                evidence.artifact_ready = true;
            }
            if tool_event_has_path(event)
                && matches!(name, "write_file" | "patch" | "artifact" | "document")
            {
                evidence.file_created = true;
            }
            if name == "send_message" {
                evidence.message_sent = true;
                let blob = tool_event_text_blob(event).to_ascii_lowercase();
                if blob.contains("wechat")
                    || blob.contains("weixin")
                    || blob.contains("wecom")
                    || blob.contains("微信")
                {
                    evidence.wechat_delivered = true;
                }
            }
        }
        evidence
    }

    fn to_value(&self) -> Value {
        json!({
            "webResult": self.web_result,
            "imageGenerated": self.image_generated,
            "fileCreated": self.file_created,
            "artifactReady": self.artifact_ready,
            "messageSent": self.message_sent,
            "wechatDelivered": self.wechat_delivered,
            "runningTools": self.running_tools,
            "failedTools": self.failed_tools,
        })
    }
}

#[derive(Debug, Clone)]
struct CompletionGateAssessment {
    contract: TaskContract,
    evidence: EvidenceRegistry,
    unsatisfied: Vec<TaskRequirement>,
    has_blocker: bool,
    looks_intermediate: bool,
    looks_tool_or_transport_leak: bool,
}

impl CompletionGateAssessment {
    fn to_value(&self) -> Value {
        json!({
            "schema": "synthchat_completion_gate_v1",
            "contract": self.contract.to_value(),
            "evidence": self.evidence.to_value(),
            "unsatisfied": self.unsatisfied.iter().map(|requirement| requirement.as_str()).collect::<Vec<_>>(),
            "hasBlocker": self.has_blocker,
            "looksIntermediate": self.looks_intermediate,
            "looksToolOrTransportLeak": self.looks_tool_or_transport_leak,
        })
    }
}

#[derive(Debug, Clone)]
struct CompletionGateDecision {
    assessment: CompletionGateAssessment,
    observation: Option<String>,
    terminal_content: Option<String>,
}

fn final_answer_completion_gate_decision(
    store: &AppStore,
    run_id: &str,
    content: &str,
) -> AppResult<CompletionGateDecision> {
    let trimmed = content.trim();
    let run = store.agent_run(run_id)?;
    let contract = TaskContract::from_run(&run);
    let evidence = EvidenceRegistry::from_run(&run);
    let unsatisfied = contract.unsatisfied(&evidence);
    let has_blocker = final_answer_reports_blocker(trimmed);
    let looks_intermediate = final_answer_looks_like_intermediate_progress(trimmed);
    let looks_tool_or_transport_leak = final_answer_looks_like_tool_or_transport_leak(trimmed);
    let rejection_count = completion_gate_rejection_count(&run, &unsatisfied);
    let failed_required_tools = failed_tools_for_requirements(&run, &unsatisfied);
    let mut assessment = CompletionGateAssessment {
        contract,
        evidence,
        unsatisfied,
        has_blocker,
        looks_intermediate,
        looks_tool_or_transport_leak,
    };

    if trimmed.is_empty() {
        return Ok(CompletionGateDecision {
            assessment,
            observation: Some(
                "final answer is empty; continue planning or report a concrete blocker".into(),
            ),
            terminal_content: None,
        });
    }

    if !assessment.unsatisfied.is_empty()
        && !assessment.has_blocker
        && assessment.evidence.running_tools.is_empty()
        && (!failed_required_tools.is_empty()
            || rejection_count >= COMPLETION_GATE_RECOVERY_LIMIT)
    {
        let terminal_content =
            completion_gate_terminal_blocker(&assessment, &failed_required_tools);
        assessment.has_blocker = true;
        return Ok(CompletionGateDecision {
            assessment,
            observation: None,
            terminal_content: Some(terminal_content),
        });
    }

    if assessment.looks_tool_or_transport_leak {
        return Ok(CompletionGateDecision {
            observation: Some(
                "completion gate rejected a raw tool/output-looking final answer. Summarize the tool evidence into a human-readable answer, retry the failed tool path, or return a concrete blocker instead of exposing stdout/stderr, function-call errors, or corrupted transport text.".into(),
            ),
            assessment,
            terminal_content: None,
        });
    }

    if !assessment.evidence.running_tools.is_empty() && !assessment.has_blocker {
        return Ok(CompletionGateDecision {
            observation: Some(format!(
                "completion gate found running tools without a blocker: {}. Wait for tool completion, close stale tool events, or return a blocker that names the unfinished work.",
                assessment.evidence.running_tools.join(", ")
            )),
            assessment,
            terminal_content: None,
        });
    }

    if !assessment.unsatisfied.is_empty() && !assessment.has_blocker {
        let missing = assessment
            .unsatisfied
            .iter()
            .map(|requirement| format!("{} ({})", requirement.as_str(), requirement.description()))
            .collect::<Vec<_>>()
            .join(", ");
        return Ok(CompletionGateDecision {
            observation: Some(format!(
                "completion gate rejected final answer because required evidence is missing: {missing}. Continue executing the required tool chain, or return a concrete blocker if the evidence cannot be produced."
            )),
            assessment,
            terminal_content: None,
        });
    }

    if assessment.looks_intermediate && !assessment.has_blocker {
        return Ok(CompletionGateDecision {
            observation: Some(
                "completion gate rejected a progress-style final answer. Continue execution until deliverables are ready, or return a final blocker with the missing evidence.".into(),
            ),
            assessment,
            terminal_content: None,
        });
    }

    Ok(CompletionGateDecision {
        assessment,
        observation: None,
        terminal_content: None,
    })
}

const COMPLETION_GATE_RECOVERY_LIMIT: usize = 2;

fn request_is_capability_inquiry(request: &str, normalized: &str) -> bool {
    let compact = request
        .chars()
        .filter(|ch| {
            !ch.is_whitespace()
                && !matches!(
                    ch,
                    '？' | '?' | '。' | '.' | '！' | '!' | '，' | ',' | '；' | ';' | '：' | ':'
                )
        })
        .collect::<String>();
    let without_suffix = compact
        .strip_suffix('吗')
        .or_else(|| compact.strip_suffix('么'))
        .unwrap_or(&compact);
    let chinese_core = [
        "你是否支持",
        "是否支持",
        "你可不可以",
        "可不可以",
        "你能不能",
        "能不能",
        "你有没有",
        "有没有",
        "你可以",
        "可以",
        "你能",
        "能",
        "你会",
        "会",
        "你支持",
        "支持",
    ]
    .iter()
    .find_map(|prefix| without_suffix.strip_prefix(prefix));
    if chinese_core.is_some_and(|core| {
        matches!(
            core,
            "生图"
                | "生成图片"
                | "生成图像"
                | "画图"
                | "绘图"
                | "联网"
                | "联网搜索"
                | "上网搜索"
                | "搜索网页"
                | "创建文件"
                | "生成文件"
                | "生成文档"
                | "写文件"
                | "发消息"
                | "发送消息"
                | "发微信"
                | "发送微信"
        )
    }) {
        return true;
    }

    let english = normalized
        .trim()
        .trim_matches(|ch: char| {
            ch.is_whitespace() || matches!(ch, '?' | '.' | '!' | ',' | ';' | ':')
        })
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let english_core = [
        "can you ",
        "are you able to ",
        "do you support ",
        "is it possible for you to ",
    ]
    .iter()
    .find_map(|prefix| english.strip_prefix(prefix));
    english_core.is_some_and(|core| {
        matches!(
            core,
            "generate images"
                | "generate an image"
                | "create images"
                | "draw images"
                | "search the web"
                | "browse the web"
                | "create files"
                | "generate documents"
                | "send messages"
                | "send wechat messages"
        )
    })
}

fn failed_tools_for_requirements(
    run: &AgentRunRecord,
    requirements: &[TaskRequirement],
) -> Vec<String> {
    latest_tool_events(run)
        .into_iter()
        .filter_map(|event| {
            if tool_event_status(event) != Some("failed") {
                return None;
            }
            let tool_name = tool_event_name(event);
            requirements
                .iter()
                .any(|requirement| requirement.is_attempted_by(&tool_name))
                .then(|| tool_event_label(event, &tool_name))
        })
        .collect()
}

fn completion_gate_rejection_count(run: &AgentRunRecord, unsatisfied: &[TaskRequirement]) -> usize {
    let expected = unsatisfied
        .iter()
        .map(|requirement| requirement.as_str())
        .collect::<Vec<_>>();
    let mut count = 0;
    for event in run.phase_events.iter().rev() {
        if event.phase != WORKFLOW_PHASE_NODE
            || event.detail.get("node").and_then(Value::as_str) != Some("completion_gate")
        {
            continue;
        }
        match event.detail.get("status").and_then(Value::as_str) {
            Some("completed") => break,
            Some("failed") => {
                let previous = event
                    .detail
                    .get("detail")
                    .and_then(|detail| {
                        detail
                            .get("completionGate")
                            .or_else(|| detail.get("completion_gate"))
                    })
                    .and_then(|gate| gate.get("unsatisfied"))
                    .and_then(Value::as_array)
                    .map(|requirements| {
                        requirements
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                    });
                if previous.as_deref() == Some(expected.as_slice()) {
                    count += 1;
                } else {
                    break;
                }
            }
            _ => {}
        }
    }
    count
}

fn completion_gate_terminal_blocker(
    assessment: &CompletionGateAssessment,
    failed_required_tools: &[String],
) -> String {
    let missing = assessment
        .unsatisfied
        .iter()
        .map(|requirement| requirement.description())
        .collect::<Vec<_>>()
        .join("、");
    if let Some(failure) = failed_required_tools.last() {
        let failure = truncate_completion_gate_detail(failure, 600);
        return format!(
            "本轮任务未能完成，已停止继续重试。\n\n失败原因：{failure}\n缺少交付结果：{missing}。"
        );
    }
    format!("本轮任务未能完成：多次尝试后仍缺少{missing}。为避免持续循环，已停止继续重试。")
}

fn truncate_completion_gate_detail(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut truncated = value
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    truncated.push_str("...");
    truncated
}

fn text_contains_any(content: &str, markers: &[&str]) -> bool {
    markers.iter().any(|marker| content.contains(marker))
}

fn ascii_contains_any(content: &str, markers: &[&str]) -> bool {
    markers.iter().any(|marker| content.contains(marker))
}

fn tool_event_name(event: &Value) -> String {
    event
        .get("toolName")
        .or_else(|| event.get("tool_name"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn tool_event_call_id(event: &Value) -> Option<&str> {
    event
        .get("callId")
        .or_else(|| event.get("call_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|call_id| !call_id.is_empty())
}

fn latest_tool_events(run: &AgentRunRecord) -> Vec<&Value> {
    let mut seen_call_ids = HashSet::new();
    run.tool_events
        .iter()
        .rev()
        .filter(|event| {
            tool_event_call_id(event)
                .map(|call_id| seen_call_ids.insert(call_id.to_string()))
                .unwrap_or(true)
        })
        .collect()
}

fn tool_event_status(event: &Value) -> Option<&str> {
    event.get("status").and_then(Value::as_str)
}

fn tool_event_completed_ok(event: &Value) -> bool {
    let status_completed = matches!(tool_event_status(event), Some("completed" | "success"));
    let ok = event
        .get("ok")
        .and_then(Value::as_bool)
        .unwrap_or(status_completed);
    status_completed && ok
}

fn tool_event_label(event: &Value, fallback_name: &str) -> String {
    let status = tool_event_status(event).unwrap_or("unknown");
    let summary = event
        .get("error")
        .or_else(|| event.get("summary"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(status);
    format!("{fallback_name}: {summary}")
}

fn tool_event_has_path(event: &Value) -> bool {
    let has_display_path = event
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|path| !path.is_empty())
        && event.get("exists").and_then(Value::as_bool) != Some(false);
    has_display_path || tool_event_text_blob(event).contains("\"path\"")
}

fn tool_event_has_artifact(event: &Value) -> bool {
    let blob = tool_event_text_blob(event);
    blob.contains("\"artifact\"")
        || blob.contains("\"artifacts\"")
        || event
            .get("eventType")
            .or_else(|| event.get("event_type"))
            .and_then(Value::as_str)
            .is_some_and(|kind| matches!(kind, "image" | "artifact" | "document"))
}

fn tool_event_text_blob(event: &Value) -> String {
    let mut parts = Vec::new();
    for key in ["summary", "text", "error", "raw"] {
        if let Some(value) = event.get(key) {
            parts.push(match value {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            });
        }
    }
    parts.join("\n")
}

fn final_answer_reports_blocker(content: &str) -> bool {
    let normalized = content.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return false;
    }
    let chinese_markers = [
        "无法",
        "不能",
        "失败",
        "未能",
        "缺少",
        "没有配置",
        "未配置",
        "需要你",
        "需要用户",
        "权限",
        "被拒绝",
        "不可用",
        "报错",
        "阻塞",
        "已达到",
    ];
    if chinese_markers
        .iter()
        .any(|marker| content.contains(marker))
    {
        return true;
    }
    [
        "blocker",
        "blocked",
        "failed",
        "unable",
        "cannot",
        "can't",
        "missing",
        "not configured",
        "permission",
        "denied",
        "unavailable",
        "error",
        "timed out",
        "requires user",
        "need you",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
}

fn final_answer_looks_like_intermediate_progress(content: &str) -> bool {
    let normalized = content.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return true;
    }
    let chinese_markers = [
        "正在",
        "稍等",
        "等等",
        "等一下",
        "请稍候",
        "请稍等",
        "马上",
        "处理中",
        "我看看",
        "我去看",
        "我去查",
        "我来查",
        "我来搜",
        "我再试",
    ];
    if chinese_markers
        .iter()
        .any(|marker| content.contains(marker))
    {
        return true;
    }
    [
        "one moment",
        "please wait",
        "hold on",
        "searching",
        "working on",
        "i am searching",
        "i'm searching",
        "let me check",
        "let me search",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
}

fn final_answer_looks_like_tool_or_transport_leak(content: &str) -> bool {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return false;
    }
    let normalized = trimmed.to_ascii_lowercase();

    if final_answer_contains_tool_action_object(&normalized) {
        return true;
    }

    if text_contains_any(
        trimmed,
        &[
            "<tool_result",
            "</tool_result>",
            "<tool_call",
            "</tool_call>",
            "<function_call",
            "</function_call>",
            "untrusted_tool_result",
        ],
    ) {
        return true;
    }

    let has_mojibake = trimmed.contains('\u{FFFD}') || trimmed.contains("��");
    let has_terminal_field = ascii_contains_any(
        &normalized,
        &[
            "stdout",
            "stderr",
            "exitcode",
            "exit code",
            "cwd:",
            "\"stdout\"",
            "\"stderr\"",
        ],
    );
    let has_function_error = ascii_contains_any(
        &normalized,
        &[
            "function not found",
            "function_not_found",
            "tool not found",
            "unknown tool",
            "invalid tool call",
        ],
    );
    let has_transport_fragment = ascii_contains_any(
        &normalized,
        &[
            "charset=utf-8",
            "utf-8\")",
            "content-type",
            "internal ·",
            "internal.web_request",
            "web_request",
        ],
    );

    if has_mojibake && (has_terminal_field || has_function_error || has_transport_fragment) {
        return true;
    }
    if has_function_error && (has_terminal_field || has_transport_fragment || has_mojibake) {
        return true;
    }

    let terminal_marker_count = [
        "stdout",
        "stderr",
        "exitcode",
        "cwd:",
        "transport:",
        "backend:",
        "target:",
        "sandbox:",
    ]
    .iter()
    .filter(|marker| normalized.contains(**marker))
    .count();
    if terminal_marker_count >= 2 && trimmed.len() < 800 {
        return true;
    }

    if trimmed.starts_with('{')
        && ascii_contains_any(
            &normalized,
            &[
                "\"toolcalls\"",
                "\"tool_calls\"",
                "\"function_call\"",
                "\"function_calls\"",
                "\"arguments\"",
                "\"toolname\"",
                "\"tool_name\"",
                "\"eventtype\"",
                "\"event_type\"",
                "\"stdout\"",
                "\"stderr\"",
            ],
        )
        && (ascii_contains_any(
            &normalized,
            &[
                "\"tool\"",
                "\"toolname\"",
                "\"tool_name\"",
                "\"name\"",
                "\"function\"",
                "\"tool_calls\"",
            ],
        ) || ascii_contains_any(&normalized, &["\"stdout\"", "\"stderr\""]))
    {
        return true;
    }

    false
}

fn final_answer_contains_tool_action_object(normalized: &str) -> bool {
    if !normalized.contains('{') || !normalized.contains('}') {
        return false;
    }
    let compact = normalized
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>();
    let has_action_key = ascii_contains_any(
        &compact,
        &[
            "{\"action\":",
            "{'action':",
            "{action:",
            ",\"action\":",
            ",'action':",
            ",action:",
        ],
    );
    if !has_action_key {
        return false;
    }
    let has_tool_name = ascii_contains_any(
        &compact,
        &[
            "web_extract",
            "web.extract",
            "web_request",
            "web.request",
            "web_search",
            "web.search",
            "browser_navigate",
            "browser.navigate",
            "browser_snapshot",
            "browser.snapshot",
            "browser_cdp",
            "terminal",
            "process",
            "read_file",
            "read.file",
            "write_file",
            "write.file",
            "patch",
            "document",
            "artifact",
            "image_generate",
            "image.generate",
            "vision_analyze",
            "vision.analyze",
            "tool_call",
            "function_call",
        ],
    );
    let has_tool_payload_key = ascii_contains_any(
        &compact,
        &[
            "\"url\":",
            "'url':",
            "\"urls\":",
            "'urls':",
            "\"command\":",
            "'command':",
            "\"payload\":",
            "'payload':",
            "\"arguments\":",
            "'arguments':",
            "\"maxchars\":",
            "'maxchars':",
            "\"tool\":",
            "'tool':",
            "\"path\":",
            "'path':",
        ],
    );
    has_tool_name && (has_tool_payload_key || compact.len() < 1200)
}

#[cfg(test)]
mod completion_gate_tests {
    use super::*;

    fn test_store(name: &str) -> (std::path::PathBuf, AppStore) {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "synthchat-completion-gate-{name}-{}-{nonce}",
            std::process::id()
        ));
        let store = AppStore::new(dir.join("state.json")).unwrap();
        (dir, store)
    }

    #[test]
    fn capability_question_does_not_require_image_artifact() {
        let mut run = AgentRunRecord::new("conv".into(), "persona".into(), "agent".into());
        run.user_request = "你可以生图吗？".into();

        let contract = TaskContract::from_run(&run);

        assert!(contract.requirements.is_empty());
    }

    #[test]
    fn concrete_image_request_requires_image_artifact() {
        let mut run = AgentRunRecord::new("conv".into(), "persona".into(), "agent".into());
        run.user_request = "请生成图片：一只在花园挥爪的小狗".into();

        let contract = TaskContract::from_run(&run);

        assert_eq!(contract.requirements, vec![TaskRequirement::ImageArtifact]);
    }

    #[test]
    fn capability_question_final_answer_passes_without_tool_evidence() {
        let (dir, store) = test_store("capability");
        let mut run = AgentRunRecord::new("conv".into(), "persona".into(), "agent".into());
        run.user_request = "你可以生图吗？".into();
        store.save_agent_run(run.clone()).unwrap();

        let gate = final_answer_completion_gate_decision(
            &store,
            &run.run_id,
            "可以，告诉我主体、风格和尺寸即可。",
        )
        .unwrap();

        assert!(gate.observation.is_none());
        assert!(gate.terminal_content.is_none());
        assert!(gate.assessment.unsatisfied.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn failed_required_tool_becomes_terminal_blocker() {
        let (dir, store) = test_store("failed-tool");
        let mut run = AgentRunRecord::new("conv".into(), "persona".into(), "agent".into());
        run.user_request = "请生成图片：一只小狗".into();
        run.tool_events.push(json!({
            "toolName": "image_generate",
            "callId": "call-image-1",
            "status": "running",
            "ok": true
        }));
        run.tool_events.push(json!({
            "toolName": "image_generate",
            "callId": "call-image-1",
            "status": "failed",
            "ok": false,
            "error": "provider returned 503"
        }));
        store.save_agent_run(run.clone()).unwrap();

        let gate =
            final_answer_completion_gate_decision(
                &store,
                &run.run_id,
                r#"{"name":"image_generate","arguments":{"prompt":"一只小狗"}}"#,
            )
            .unwrap();

        assert!(gate.observation.is_none());
        assert!(gate.assessment.has_blocker);
        assert!(gate.assessment.evidence.running_tools.is_empty());
        let terminal = gate.terminal_content.unwrap();
        assert!(terminal.contains("已停止继续重试"));
        assert!(terminal.contains("image_generate"));
        assert!(terminal.contains("generated image artifact"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unrelated_failed_tool_does_not_end_required_tool_recovery() {
        let (dir, store) = test_store("unrelated-failed-tool");
        let mut run = AgentRunRecord::new("conv".into(), "persona".into(), "agent".into());
        run.user_request = "请生成图片：一只小狗".into();
        run.tool_events.push(json!({
            "toolName": "terminal",
            "status": "failed",
            "ok": false,
            "error": "command failed"
        }));
        store.save_agent_run(run.clone()).unwrap();

        let gate =
            final_answer_completion_gate_decision(&store, &run.run_id, "正在继续生成。").unwrap();

        assert!(gate.observation.is_some());
        assert!(!gate.assessment.has_blocker);
        assert!(gate.terminal_content.is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn repeated_missing_evidence_stops_after_recovery_limit() {
        let (dir, store) = test_store("recovery-limit");
        let mut run = AgentRunRecord::new("conv".into(), "persona".into(), "agent".into());
        run.user_request = "请生成图片：一只小狗".into();
        store.save_agent_run(run.clone()).unwrap();

        for iteration in 1..=COMPLETION_GATE_RECOVERY_LIMIT {
            let gate =
                final_answer_completion_gate_decision(&store, &run.run_id, "已经完成。").unwrap();
            let observation = gate.observation.as_deref().unwrap().to_string();
            assert!(gate.terminal_content.is_none());
            record_workflow_completion_gate_rejected(
                &store,
                &run.run_id,
                iteration as u32,
                WorkflowMode::ChatTurn,
                &gate.assessment,
                &observation,
            )
            .unwrap();
        }

        let gate =
            final_answer_completion_gate_decision(&store, &run.run_id, "已经完成。").unwrap();

        assert!(gate.observation.is_none());
        assert!(gate.assessment.has_blocker);
        assert!(gate
            .terminal_content
            .as_deref()
            .is_some_and(|content| content.contains("为避免持续循环")));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rejects_corrupted_tool_output_as_final_answer() {
        assert!(final_answer_looks_like_tool_or_transport_leak(
            "urusuft-8\")(stdout��\\Function not found 体验功能"
        ));
    }

    #[test]
    fn allows_normal_function_not_found_explanation() {
        assert!(!final_answer_looks_like_tool_or_transport_leak(
            "这是 Function not found 错误，说明服务商没有识别到对应工具名，需要检查工具 schema 和 relay 兼容层。"
        ));
    }

    #[test]
    fn rejects_leaked_provider_tool_call_json_as_final_answer() {
        assert!(final_answer_looks_like_tool_or_transport_leak(
            r#"{"name":"image_generate","arguments":{"prompt":"湖边风景","size":"1024x1024"}}
{"tool_calls":[{"type":"function","function":{"name":"image_generate","arguments":{"prompt":"湖边风景"}}}]}"#
        ));
    }

    #[test]
    fn rejects_single_quoted_tool_action_dict_as_final_answer() {
        assert!(final_answer_looks_like_tool_or_transport_leak(
            "小孙：\n等等哦，我去看看今天有啥热闹～\n\n{'action': 'web_extract', 'url': 'https://www.toutiao.com/', 'maxChars': 3000}"
        ));
    }

    #[test]
    fn rejects_tool_action_without_payload_as_final_answer() {
        assert!(final_answer_looks_like_tool_or_transport_leak(
            r#"{"action":"web_search"}"#
        ));
    }
}

pub(super) fn resolve_workflow_planner_route(
    store: &AppStore,
    run_id: &str,
    iteration: u32,
    mode: WorkflowMode,
    decision: &Value,
    fallback_content: &str,
    available_tools: &[ToolDefinition],
) -> AppResult<WorkflowPlannerRoute> {
    match decision
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("final")
    {
        "tool" => {
            let tool_calls =
                match validated_tool_calls_from_decision_with_error(decision, available_tools) {
                    Ok(tool_calls) => tool_calls,
                    Err(error) => {
                        let error_kind =
                            WorkflowPlannerErrorKind::from_tool_validation_error_kind(error.kind());
                        record_workflow_planner_failed(
                            store,
                            run_id,
                            Some(iteration),
                            mode,
                            error_kind,
                            error.message(),
                        )?;
                        return Ok(WorkflowPlannerRoute::Recover {
                            observation: format!(
                                "{} {}: {}",
                                workflow_iteration_label(mode, iteration),
                                error_kind.observation_label(),
                                error.message()
                            ),
                        });
                    }
                };
            if tool_calls.is_empty() {
                let error_message =
                    "planner requested tool action without a valid tool name".to_string();
                record_workflow_planner_failed(
                    store,
                    run_id,
                    Some(iteration),
                    mode,
                    WorkflowPlannerErrorKind::ToolRequest,
                    &error_message,
                )?;
                return Ok(WorkflowPlannerRoute::Recover {
                    observation: format!(
                        "{} tool error: {}",
                        workflow_iteration_label(mode, iteration),
                        error_message
                    ),
                });
            }
            let request_count = tool_calls.len();
            let tool_names = tool_calls
                .iter()
                .map(|call| call.name.clone())
                .collect::<Vec<_>>();
            record_workflow_planner_to_executor(
                store,
                run_id,
                iteration,
                mode,
                request_count,
                &tool_names,
                &tool_calls,
            )?;
            let requests = tool_calls
                .into_iter()
                .map(|call| (call.name, call.arguments))
                .collect();
            Ok(WorkflowPlannerRoute::ExecuteTools {
                requests,
                request_count,
            })
        }
        _ => {
            let mut final_content = decision
                .get("content")
                .or_else(|| decision.get("answer"))
                .and_then(Value::as_str)
                .unwrap_or(fallback_content.trim())
                .to_string();
            let gate = final_answer_completion_gate_decision(store, run_id, &final_content)?;
            if let Some(observation) = gate.observation.as_deref() {
                record_workflow_completion_gate_rejected(
                    store,
                    run_id,
                    iteration,
                    mode,
                    &gate.assessment,
                    observation,
                )?;
                record_workflow_planner_failed(
                    store,
                    run_id,
                    Some(iteration),
                    mode,
                    WorkflowPlannerErrorKind::CompletionGate,
                    &observation,
                )?;
                return Ok(WorkflowPlannerRoute::Recover {
                    observation: format!(
                        "{} completion gate: {}",
                        workflow_iteration_label(mode, iteration),
                        observation
                    ),
                });
            }
            if let Some(terminal_content) = gate.terminal_content.as_ref() {
                final_content = terminal_content.clone();
            }
            record_workflow_planner_to_reviewer_with_completion_gate(
                store,
                run_id,
                iteration,
                mode,
                Some(&gate.assessment),
            )?;
            Ok(WorkflowPlannerRoute::ReviewFinal {
                content: final_content,
            })
        }
    }
}

pub(super) fn resolve_workflow_executor_continue_route(
    store: &AppStore,
    run_id: &str,
    iteration: u32,
    mode: WorkflowMode,
    tool_count: usize,
    parallel: Option<bool>,
) -> AppResult<WorkflowExecutorRoute> {
    record_workflow_executor_to_planner(store, run_id, iteration, mode, tool_count, parallel)?;
    Ok(WorkflowExecutorRoute::ContinuePlanning {
        tool_count,
        parallel,
    })
}

pub(super) fn resolve_workflow_executor_approval_route(
    store: &AppStore,
    run_id: &str,
    iteration: u32,
    server_id: &str,
    tool_name: &str,
) -> AppResult<WorkflowExecutorRoute> {
    record_workflow_pending_approval(store, run_id, iteration, server_id, tool_name)?;
    Ok(WorkflowExecutorRoute::AwaitApproval {
        server_id: server_id.to_string(),
        tool_name: tool_name.to_string(),
    })
}

pub(super) fn resolve_workflow_executor_tool_approval_route(
    store: &AppStore,
    run_id: &str,
    iteration: u32,
    mode: WorkflowMode,
    wait: &WorkflowExecutorApprovalWait,
) -> AppResult<WorkflowExecutorRoute> {
    record_workflow_pending_tool_approval(store, run_id, iteration, mode, wait)?;
    Ok(WorkflowExecutorRoute::AwaitApproval {
        server_id: wait.identity().server_id().to_string(),
        tool_name: wait.identity().tool_name().to_string(),
    })
}

pub(super) fn resolve_workflow_reviewer_completed_route(
    store: &AppStore,
    run_id: &str,
    mode: WorkflowMode,
    message_id: &str,
    model: Option<&str>,
    provider_id: Option<&str>,
) -> AppResult<WorkflowReviewerRoute> {
    record_workflow_reviewer_completed(store, run_id, mode, message_id, model, provider_id)?;
    Ok(WorkflowReviewerRoute::Completed {
        message_id: message_id.to_string(),
        model: model.map(str::to_string),
        provider_id: provider_id.map(str::to_string),
    })
}

pub(super) fn resolve_workflow_reviewer_skipped_route(
    store: &AppStore,
    run_id: &str,
    mode: WorkflowMode,
    message_id: &str,
    reason: &str,
    model: Option<&str>,
    provider_id: Option<&str>,
) -> AppResult<WorkflowReviewerRoute> {
    record_workflow_reviewer_skipped(store, run_id, mode, message_id, reason, model, provider_id)?;
    Ok(WorkflowReviewerRoute::Skipped {
        message_id: message_id.to_string(),
        reason: reason.to_string(),
        model: model.map(str::to_string),
        provider_id: provider_id.map(str::to_string),
    })
}

fn append_workflow_phase_event(
    store: &AppStore,
    run_id: &str,
    phase: &str,
    detail: Value,
    activity: String,
) -> AppResult<()> {
    let mut run = store.agent_run(run_id)?;
    let updated_at = now_iso();
    let event_sequence = next_workflow_event_sequence(&run);
    let detail = with_workflow_event_sequence(detail, event_sequence);
    run.phase_events.push(AgentRunPhaseRecord {
        phase: phase.to_string(),
        detail: detail.clone(),
        updated_at: updated_at.clone(),
    });
    apply_workflow_graph_event(&mut run, phase, &detail, &updated_at);
    run.touch_activity(activity);
    store.save_agent_run(run)?;
    Ok(())
}

fn next_workflow_event_sequence(run: &AgentRunRecord) -> u64 {
    let phase_sequence = run
        .phase_events
        .iter()
        .filter_map(|event| workflow_event_sequence_u64(&event.detail))
        .max()
        .unwrap_or(0);
    let graph_sequence = run
        .workflow_graph
        .as_ref()
        .and_then(|graph| graph.get("lastEventSequence"))
        .or_else(|| {
            run.workflow_graph
                .as_ref()
                .and_then(|graph| graph.get("last_event_sequence"))
        })
        .and_then(Value::as_u64)
        .unwrap_or(0);
    phase_sequence.max(graph_sequence) + 1
}

fn with_workflow_event_sequence(detail: Value, event_sequence: u64) -> Value {
    match detail {
        Value::Object(mut object) => {
            object.insert("eventSequence".into(), json!(event_sequence));
            object.insert("event_sequence".into(), json!(event_sequence));
            Value::Object(object)
        }
        other => json!({
            "eventSequence": event_sequence,
            "event_sequence": event_sequence,
            "detail": other,
        }),
    }
}

fn workflow_event_sequence_u64(detail: &Value) -> Option<u64> {
    detail
        .get("eventSequence")
        .or_else(|| detail.get("event_sequence"))
        .and_then(Value::as_u64)
}

fn workflow_node_snapshot(
    node: WorkflowNodeName,
    status: WorkflowNodeStatus,
    detail: Value,
    updated_at: &str,
) -> Value {
    json!({
        "node": node,
        "role": node.role(),
        "status": status,
        "detail": detail,
        "eventSequence": 0,
        "event_sequence": 0,
        "updatedAt": updated_at,
        "updated_at": updated_at,
    })
}

pub(super) fn workflow_node_role_label(node: &str) -> &'static str {
    WorkflowNodeName::from_str(node)
        .map(WorkflowNodeName::role)
        .unwrap_or("custom workflow node")
}

pub(super) fn workflow_graph_runtime_summary(graph: Option<&Value>) -> Value {
    let Some(graph) = graph else {
        return Value::Null;
    };
    let nodes = graph.get("nodes").and_then(Value::as_array);
    let transitions = graph.get("transitions").and_then(Value::as_array);
    let current_node = graph
        .get("currentNode")
        .or_else(|| graph.get("current_node"))
        .cloned()
        .unwrap_or(Value::Null);
    let current_status = graph
        .get("currentStatus")
        .or_else(|| graph.get("current_status"))
        .cloned()
        .or_else(|| {
            current_node.as_str().and_then(|current| {
                nodes?
                    .iter()
                    .find(|node| node.get("node").and_then(Value::as_str) == Some(current))
                    .and_then(|node| node.get("status"))
                    .cloned()
            })
        })
        .unwrap_or(Value::Null);
    let request_source = graph
        .get("requestSource")
        .or_else(|| graph.get("request_source"))
        .cloned()
        .unwrap_or(Value::Null);
    let tool_context = graph
        .get("toolContext")
        .or_else(|| graph.get("tool_context"))
        .cloned()
        .unwrap_or(Value::Null);
    let human_gate =
        workflow_graph_runtime_human_gate(nodes.map(Vec::as_slice), current_node.as_str());
    let tool_origins = workflow_graph_runtime_tool_origins(
        nodes.map(Vec::as_slice),
        transitions.map(Vec::as_slice),
    );
    json!({
        "schema": graph.get("schema").cloned().unwrap_or_else(|| json!(SYNTHGRAPH_WORKFLOW_SCHEMA)),
        "mode": graph.get("mode").cloned().unwrap_or(Value::Null),
        "requestSource": request_source.clone(),
        "request_source": request_source,
        "toolContext": tool_context.clone(),
        "tool_context": tool_context,
        "currentNode": current_node.clone(),
        "current_node": current_node,
        "currentStatus": current_status.clone(),
        "current_status": current_status,
        "lastEventSequence": graph.get("lastEventSequence").or_else(|| graph.get("last_event_sequence")).cloned().unwrap_or(Value::Null),
        "last_event_sequence": graph.get("lastEventSequence").or_else(|| graph.get("last_event_sequence")).cloned().unwrap_or(Value::Null),
        "updatedAt": graph.get("updatedAt").or_else(|| graph.get("updated_at")).cloned().unwrap_or(Value::Null),
        "updated_at": graph.get("updatedAt").or_else(|| graph.get("updated_at")).cloned().unwrap_or(Value::Null),
        "nodeCount": nodes.map(Vec::len).unwrap_or(0),
        "node_count": nodes.map(Vec::len).unwrap_or(0),
        "transitionCount": transitions.map(Vec::len).unwrap_or(0),
        "transition_count": transitions.map(Vec::len).unwrap_or(0),
        "statusCounts": workflow_graph_runtime_status_counts(nodes.map(Vec::as_slice)),
        "status_counts": workflow_graph_runtime_status_counts(nodes.map(Vec::as_slice)),
        "humanGate": human_gate.clone(),
        "human_gate": human_gate,
        "toolOrigins": tool_origins.clone(),
        "tool_origins": tool_origins
    })
}

pub(super) fn workflow_graph_run_response_values(graph: Option<&Value>) -> (Value, Value) {
    (
        graph
            .map(workflow_graph_with_runtime_aliases)
            .unwrap_or(Value::Null),
        workflow_graph_runtime_summary(graph),
    )
}

pub(super) fn insert_workflow_graph_run_response_aliases(
    object: &mut Map<String, Value>,
    graph: Option<&Value>,
) {
    let (workflow_graph, workflow_summary) = workflow_graph_run_response_values(graph);
    object.insert("workflow_graph".into(), workflow_graph.clone());
    object.insert("workflowGraph".into(), workflow_graph);
    object.insert("workflow_summary".into(), workflow_summary.clone());
    object.insert("workflowSummary".into(), workflow_summary);
}

fn workflow_graph_runtime_status_counts(nodes: Option<&[Value]>) -> Value {
    let mut counts = Map::new();
    for node in nodes.into_iter().flatten() {
        let Some(status) = node.get("status").and_then(Value::as_str) else {
            continue;
        };
        let count = counts.get(status).and_then(Value::as_u64).unwrap_or(0) + 1;
        counts.insert(status.into(), json!(count));
    }
    Value::Object(counts)
}

fn workflow_graph_runtime_human_gate(nodes: Option<&[Value]>, current_node: Option<&str>) -> Value {
    let Some(nodes) = nodes else {
        return Value::Null;
    };
    if let Some(current_node) = current_node {
        if let Some(gate) = nodes
            .iter()
            .find(|node| node.get("node").and_then(Value::as_str) == Some(current_node))
            .and_then(workflow_node_human_gate)
        {
            return gate;
        }
    }
    nodes
        .iter()
        .rev()
        .filter(|node| node.get("status").and_then(Value::as_str) == Some("waiting"))
        .find_map(workflow_node_human_gate)
        .unwrap_or(Value::Null)
}

fn workflow_node_human_gate(node: &Value) -> Option<Value> {
    node.get("detail")
        .and_then(|detail| {
            detail
                .get("humanGate")
                .or_else(|| detail.get("human_gate"))
                .cloned()
        })
        .filter(|value| !value.is_null())
}

fn workflow_graph_runtime_tool_origins(
    nodes: Option<&[Value]>,
    transitions: Option<&[Value]>,
) -> Value {
    let mut origins = Vec::<String>::new();
    for detail in nodes
        .into_iter()
        .flatten()
        .filter_map(|node| node.get("detail"))
        .chain(
            transitions
                .into_iter()
                .flatten()
                .filter_map(|transition| transition.get("detail")),
        )
    {
        for origin in detail
            .get("toolOrigins")
            .or_else(|| detail.get("tool_origins"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|origin| !origin.is_empty())
        {
            if !origins.iter().any(|existing| existing == origin) {
                origins.push(origin.to_string());
            }
        }
    }
    Value::Array(origins.into_iter().map(Value::from).collect())
}

pub(super) fn workflow_graph_runtime_timestamp(
    value: &Value,
    graph: &Value,
    fallback: &str,
) -> String {
    value
        .get("updatedAt")
        .or_else(|| value.get("updated_at"))
        .or_else(|| graph.get("updatedAt"))
        .or_else(|| graph.get("updated_at"))
        .and_then(Value::as_str)
        .unwrap_or(fallback)
        .to_string()
}

pub(super) fn workflow_graph_with_runtime_aliases(graph: &Value) -> Value {
    let Some(source) = graph.as_object() else {
        return graph.clone();
    };
    let mut object = source.clone();
    insert_workflow_graph_alias_pair(&mut object, "requestSource", "request_source", None);
    insert_workflow_graph_alias_pair(&mut object, "toolContext", "tool_context", None);
    insert_workflow_graph_alias_pair(&mut object, "currentNode", "current_node", None);
    let current_node = object
        .get("currentNode")
        .or_else(|| object.get("current_node"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let inferred_current_status = current_node.as_deref().and_then(|current| {
        object
            .get("nodes")
            .and_then(Value::as_array)
            .and_then(|nodes| {
                nodes
                    .iter()
                    .find(|node| node.get("node").and_then(Value::as_str) == Some(current))
            })
            .and_then(|node| node.get("status"))
            .cloned()
    });
    insert_workflow_graph_alias_pair(
        &mut object,
        "currentStatus",
        "current_status",
        inferred_current_status,
    );
    insert_workflow_graph_alias_pair(
        &mut object,
        "lastEventSequence",
        "last_event_sequence",
        None,
    );
    insert_workflow_graph_alias_pair(&mut object, "updatedAt", "updated_at", None);

    if let Some(nodes) = object.get("nodes").and_then(Value::as_array).map(|nodes| {
        nodes
            .iter()
            .map(workflow_graph_node_with_aliases)
            .collect::<Vec<_>>()
    }) {
        object.insert("nodes".into(), Value::Array(nodes));
    }
    if let Some(transitions) =
        object
            .get("transitions")
            .and_then(Value::as_array)
            .map(|transitions| {
                transitions
                    .iter()
                    .map(workflow_graph_transition_with_aliases)
                    .collect::<Vec<_>>()
            })
    {
        object.insert("transitions".into(), Value::Array(transitions));
    }
    Value::Object(object)
}

fn workflow_graph_node_with_aliases(node: &Value) -> Value {
    let Some(source) = node.as_object() else {
        return node.clone();
    };
    let mut object = source.clone();
    object
        .entry("role")
        .or_insert_with(|| workflow_graph_node_role_value(node));
    insert_workflow_graph_alias_pair(&mut object, "eventSequence", "event_sequence", None);
    insert_workflow_graph_alias_pair(&mut object, "updatedAt", "updated_at", None);
    if let Some(detail) = object
        .get("detail")
        .map(workflow_detail_with_runtime_aliases)
    {
        object.insert("detail".into(), detail);
    }
    Value::Object(object)
}

fn workflow_graph_transition_with_aliases(transition: &Value) -> Value {
    let Some(source) = transition.as_object() else {
        return transition.clone();
    };
    let mut object = source.clone();
    insert_workflow_graph_alias_pair(&mut object, "eventSequence", "event_sequence", None);
    insert_workflow_graph_alias_pair(&mut object, "updatedAt", "updated_at", None);
    let topology = workflow_transition_topology_metadata_from_object(&object);
    insert_workflow_graph_alias_pair(
        &mut object,
        "topologyEdgeKnown",
        "topology_edge_known",
        topology.as_ref().map(|metadata| json!(metadata.edge_known)),
    );
    insert_workflow_graph_alias_pair(
        &mut object,
        "topologyReasonKnown",
        "topology_reason_known",
        topology
            .as_ref()
            .map(|metadata| json!(metadata.reason_known)),
    );
    insert_workflow_graph_alias_pair(
        &mut object,
        "topologyEdgeSource",
        "topology_edge_source",
        topology.as_ref().map(|metadata| metadata.source.clone()),
    );
    insert_workflow_graph_alias_pair(
        &mut object,
        "topologyEdgeLabel",
        "topology_edge_label",
        topology
            .as_ref()
            .map(|metadata| json!(metadata.label.clone())),
    );
    if let Some(detail) = object
        .get("detail")
        .map(workflow_detail_with_runtime_aliases)
    {
        object.insert("detail".into(), detail);
    }
    Value::Object(object)
}

fn insert_workflow_graph_alias_pair(
    object: &mut Map<String, Value>,
    camel_key: &str,
    snake_key: &str,
    fallback: Option<Value>,
) {
    let value = object
        .get(camel_key)
        .or_else(|| object.get(snake_key))
        .cloned()
        .or(fallback);
    if let Some(value) = value {
        object.entry(camel_key).or_insert_with(|| value.clone());
        object.entry(snake_key).or_insert(value);
    }
}

pub(super) fn workflow_graph_node_role_value(node: &Value) -> Value {
    if let Some(role) = node
        .get("role")
        .and_then(Value::as_str)
        .filter(|role| !role.trim().is_empty())
    {
        return json!(role);
    }
    json!(workflow_node_role_label(
        node.get("node").and_then(Value::as_str).unwrap_or_default()
    ))
}

fn workflow_detail_with_runtime_aliases(detail: &Value) -> Value {
    let Some(source) = detail.as_object() else {
        return match detail {
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .map(workflow_detail_with_runtime_aliases)
                    .collect(),
            ),
            _ => detail.clone(),
        };
    };
    let mut object = source.clone();
    for value in object.values_mut() {
        *value = workflow_detail_with_runtime_aliases(value);
    }
    for &(camel_key, snake_key) in WORKFLOW_DETAIL_ALIAS_PAIRS {
        insert_workflow_graph_alias_pair(&mut object, camel_key, snake_key, None);
    }
    Value::Object(object)
}

pub(super) fn workflow_graph_snapshot_runtime_payload(graph: &Value) -> Value {
    let summary = workflow_graph_runtime_summary(Some(graph));
    let graph = workflow_graph_with_runtime_aliases(graph);
    json!({
        "summary": summary.clone(),
        "workflowSummary": summary.clone(),
        "workflow_summary": summary,
        "graph": graph.clone(),
        "workflowGraph": graph.clone(),
        "workflow_graph": graph.clone()
    })
}

pub(super) fn workflow_graph_node_runtime_payload(node: &Value, summary: &Value) -> Value {
    let status = node
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let detail = node
        .get("detail")
        .map(workflow_detail_with_runtime_aliases)
        .unwrap_or_else(|| json!({}));
    json!({
        "node": node.get("node").cloned().unwrap_or(Value::Null),
        "role": workflow_graph_node_role_value(node),
        "status": status,
        "detail": detail,
        "eventSequence": node.get("eventSequence").or_else(|| node.get("event_sequence")).cloned().unwrap_or(Value::Null),
        "event_sequence": node.get("eventSequence").or_else(|| node.get("event_sequence")).cloned().unwrap_or(Value::Null),
        "graphSummary": summary.clone(),
        "graph_summary": summary.clone()
    })
}

pub(super) fn workflow_graph_transition_runtime_payload(
    transition: &Value,
    summary: &Value,
) -> Value {
    let reason = transition
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("transition");
    let detail = transition
        .get("detail")
        .map(workflow_detail_with_runtime_aliases)
        .unwrap_or_else(|| json!({}));
    let topology = transition
        .as_object()
        .and_then(workflow_transition_topology_metadata_from_object);
    let topology_edge_known = transition
        .get("topologyEdgeKnown")
        .or_else(|| transition.get("topology_edge_known"))
        .cloned()
        .or_else(|| topology.as_ref().map(|metadata| json!(metadata.edge_known)))
        .unwrap_or(Value::Null);
    let topology_reason_known = transition
        .get("topologyReasonKnown")
        .or_else(|| transition.get("topology_reason_known"))
        .cloned()
        .or_else(|| {
            topology
                .as_ref()
                .map(|metadata| json!(metadata.reason_known))
        })
        .unwrap_or(Value::Null);
    let topology_edge_source = transition
        .get("topologyEdgeSource")
        .or_else(|| transition.get("topology_edge_source"))
        .cloned()
        .or_else(|| topology.as_ref().map(|metadata| metadata.source.clone()))
        .unwrap_or(Value::Null);
    let topology_edge_label = transition
        .get("topologyEdgeLabel")
        .or_else(|| transition.get("topology_edge_label"))
        .cloned()
        .or_else(|| {
            topology
                .as_ref()
                .map(|metadata| json!(metadata.label.clone()))
        })
        .unwrap_or(Value::Null);
    json!({
        "from": transition.get("from").cloned().unwrap_or(Value::Null),
        "to": transition.get("to").cloned().unwrap_or(Value::Null),
        "reason": reason,
        "topologyEdgeKnown": topology_edge_known.clone(),
        "topology_edge_known": topology_edge_known,
        "topologyReasonKnown": topology_reason_known.clone(),
        "topology_reason_known": topology_reason_known,
        "topologyEdgeSource": topology_edge_source.clone(),
        "topology_edge_source": topology_edge_source,
        "topologyEdgeLabel": topology_edge_label.clone(),
        "topology_edge_label": topology_edge_label,
        "detail": detail,
        "eventSequence": transition.get("eventSequence").or_else(|| transition.get("event_sequence")).cloned().unwrap_or(Value::Null),
        "event_sequence": transition.get("eventSequence").or_else(|| transition.get("event_sequence")).cloned().unwrap_or(Value::Null),
        "graphSummary": summary.clone(),
        "graph_summary": summary.clone()
    })
}

fn workflow_transition_snapshot(
    from: WorkflowNodeName,
    to: WorkflowNodeName,
    reason: &str,
    detail: Value,
    updated_at: &str,
) -> Value {
    let topology = workflow_transition_topology_metadata(from, to, reason);
    json!({
        "from": from,
        "to": to,
        "reason": reason,
        "topologyEdgeKnown": topology.edge_known,
        "topology_edge_known": topology.edge_known,
        "topologyReasonKnown": topology.reason_known,
        "topology_reason_known": topology.reason_known,
        "topologyEdgeSource": topology.source.clone(),
        "topology_edge_source": topology.source,
        "topologyEdgeLabel": topology.label.clone(),
        "topology_edge_label": topology.label,
        "detail": detail,
        "eventSequence": 0,
        "event_sequence": 0,
        "updatedAt": updated_at,
        "updated_at": updated_at,
    })
}

fn workflow_turn_detail(mode: WorkflowMode, iteration: Option<u32>) -> Map<String, Value> {
    let mut detail = Map::new();
    detail.insert("mode".into(), json!(mode.as_str()));
    if let Some(iteration) = iteration {
        detail.insert("iteration".into(), json!(iteration));
    }
    detail
}

fn insert_workflow_detail_alias_value(
    detail: &mut Map<String, Value>,
    camel_key: &str,
    snake_key: &str,
    value: Value,
) {
    detail.insert(camel_key.into(), value.clone());
    detail.insert(snake_key.into(), value);
}

pub(super) fn workflow_human_gate_detail(
    kind: &str,
    status: &str,
    run_id: &str,
) -> Map<String, Value> {
    let mut detail = Map::new();
    detail.insert("schema".into(), json!(SYNTHCHAT_HUMAN_GATE_SCHEMA));
    detail.insert("kind".into(), json!(kind));
    detail.insert("status".into(), json!(status));
    insert_workflow_detail_alias_value(&mut detail, "runId", "run_id", json!(run_id));
    detail
}

pub(super) fn insert_workflow_human_gate(
    detail: &mut Map<String, Value>,
    gate: Map<String, Value>,
) {
    let gate = Value::Object(gate);
    detail.insert("humanGate".into(), gate.clone());
    detail.insert("human_gate".into(), gate);
}

fn workflow_bootstrap_node_detail(
    mode: WorkflowMode,
    request_source: &str,
    tool_context: &str,
) -> Map<String, Value> {
    let mut detail = workflow_turn_detail(mode, None);
    detail.insert("requestSource".into(), json!(request_source));
    detail.insert("request_source".into(), json!(request_source));
    detail.insert("toolContext".into(), json!(tool_context));
    detail.insert("tool_context".into(), json!(tool_context));
    detail
}

fn append_workflow_tool_identity_detail(
    detail: &mut Map<String, Value>,
    identity: &WorkflowExecutorToolIdentity,
) {
    insert_workflow_detail_alias_value(
        detail,
        "requestedName",
        "requested_name",
        json!(identity.requested_name()),
    );
    insert_workflow_detail_alias_value(
        detail,
        "serverId",
        "server_id",
        json!(identity.server_id()),
    );
    insert_workflow_detail_alias_value(
        detail,
        "toolName",
        "tool_name",
        json!(identity.tool_name()),
    );
    insert_workflow_detail_alias_value(
        detail,
        "toolKind",
        "tool_kind",
        json!(identity.kind().as_str()),
    );
    insert_workflow_detail_alias_value(
        detail,
        "sourceLabel",
        "source_label",
        json!(identity.source_label()),
    );
}

fn workflow_iteration_label(mode: WorkflowMode, iteration: u32) -> String {
    match mode {
        WorkflowMode::ChatTurn => format!("Iteration {iteration}"),
        WorkflowMode::ApprovalContinuation => format!("Continuation iteration {iteration}"),
    }
}

pub(super) fn apply_workflow_graph_event(
    run: &mut AgentRunRecord,
    phase: &str,
    detail: &Value,
    updated_at: &str,
) {
    match phase {
        WORKFLOW_PHASE_INITIALIZED => {
            run.workflow_graph = Some(detail.clone());
            let _ = ensure_workflow_graph_root(run, updated_at);
        }
        WORKFLOW_PHASE_NODE => {
            let root = ensure_workflow_graph_root(run, updated_at);
            apply_workflow_node_update(root, detail, updated_at);
        }
        WORKFLOW_PHASE_TRANSITION => {
            let root = ensure_workflow_graph_root(run, updated_at);
            apply_workflow_transition_update(root, detail, updated_at);
        }
        _ => {}
    }
}

fn ensure_workflow_graph_root<'a>(run: &'a mut AgentRunRecord, updated_at: &str) -> &'a mut Value {
    if run.workflow_graph.is_none() {
        run.workflow_graph = Some(json!({
            "schema": SYNTHGRAPH_WORKFLOW_SCHEMA,
            "mode": "recovered",
            "nodes": [],
            "transitions": [],
            "currentNode": Value::Null,
            "current_node": Value::Null,
            "currentStatus": Value::Null,
            "current_status": Value::Null,
            "updatedAt": updated_at,
            "updated_at": updated_at,
        }));
    }
    let root = run
        .workflow_graph
        .as_mut()
        .expect("workflow graph initialized");
    if let Some(object) = root.as_object_mut() {
        object.insert("updatedAt".into(), json!(updated_at));
        object.insert("updated_at".into(), json!(updated_at));
        let last_event_sequence = object
            .get("lastEventSequence")
            .filter(|value| !value.is_null())
            .cloned()
            .or_else(|| {
                object
                    .get("last_event_sequence")
                    .filter(|value| !value.is_null())
                    .cloned()
            })
            .unwrap_or_else(|| json!(0));
        object.insert("lastEventSequence".into(), last_event_sequence.clone());
        object.insert("last_event_sequence".into(), last_event_sequence);
        object
            .entry("schema")
            .or_insert_with(|| json!(SYNTHGRAPH_WORKFLOW_SCHEMA));
        object
            .entry("nodes")
            .or_insert_with(|| Value::Array(Vec::new()));
        object
            .entry("transitions")
            .or_insert_with(|| Value::Array(Vec::new()));
        let request_source = object
            .get("requestSource")
            .filter(|value| !value.is_null())
            .cloned()
            .or_else(|| {
                object
                    .get("request_source")
                    .filter(|value| !value.is_null())
                    .cloned()
            })
            .unwrap_or(Value::Null);
        object.insert("requestSource".into(), request_source.clone());
        object.insert("request_source".into(), request_source);
        let tool_context = object
            .get("toolContext")
            .filter(|value| !value.is_null())
            .cloned()
            .or_else(|| {
                object
                    .get("tool_context")
                    .filter(|value| !value.is_null())
                    .cloned()
            })
            .unwrap_or(Value::Null);
        object.insert("toolContext".into(), tool_context.clone());
        object.insert("tool_context".into(), tool_context);
        let current_node = object
            .get("currentNode")
            .filter(|value| !value.is_null())
            .cloned()
            .or_else(|| {
                object
                    .get("current_node")
                    .filter(|value| !value.is_null())
                    .cloned()
            })
            .unwrap_or(Value::Null);
        object.insert("currentNode".into(), current_node.clone());
        object.insert("current_node".into(), current_node);
        let current_status = object
            .get("currentStatus")
            .filter(|value| !value.is_null())
            .cloned()
            .or_else(|| {
                object
                    .get("current_status")
                    .filter(|value| !value.is_null())
                    .cloned()
            })
            .or_else(|| workflow_current_status_for_current_node(object))
            .unwrap_or(Value::Null);
        object.insert("currentStatus".into(), current_status.clone());
        object.insert("current_status".into(), current_status);
    }
    root
}

fn apply_workflow_node_update(root: &mut Value, detail: &Value, updated_at: &str) {
    let Some(root_object) = root.as_object_mut() else {
        return;
    };
    let event_sequence = workflow_event_sequence_u64(detail)
        .map(Value::from)
        .unwrap_or(Value::Null);
    if !event_sequence.is_null() {
        root_object.insert("lastEventSequence".into(), event_sequence.clone());
        root_object.insert("last_event_sequence".into(), event_sequence.clone());
    }
    let Some(node_name) = detail
        .get("node")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|node| !node.is_empty())
        .map(str::to_string)
    else {
        return;
    };
    let Some(status) = detail
        .get("status")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|status| !status.is_empty())
        .map(str::to_string)
    else {
        return;
    };
    let role = detail
        .get("role")
        .and_then(Value::as_str)
        .filter(|role| !role.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| workflow_node_role_label(&node_name).to_string());
    let node_detail = detail.get("detail").cloned().unwrap_or_else(|| json!({}));
    let nodes = root_object
        .entry("nodes")
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Some(nodes) = nodes.as_array_mut() {
        if let Some(existing) = nodes
            .iter_mut()
            .find(|node| node.get("node").and_then(Value::as_str) == Some(node_name.as_str()))
        {
            *existing = json!({
                "node": node_name,
                "role": role,
                "status": status,
                "detail": node_detail,
                "eventSequence": event_sequence,
                "event_sequence": event_sequence,
                "updatedAt": updated_at,
                "updated_at": updated_at,
            });
        } else {
            nodes.push(json!({
                "node": node_name,
                "role": role,
                "status": status,
                "detail": node_detail,
                "eventSequence": event_sequence,
                "event_sequence": event_sequence,
                "updatedAt": updated_at,
                "updated_at": updated_at,
            }));
        }
    }
    if workflow_node_update_sets_current(root_object, &node_name, &status, &node_detail) {
        root_object.insert("currentNode".into(), json!(node_name));
        root_object.insert("current_node".into(), json!(node_name));
        root_object.insert("currentStatus".into(), json!(status));
        root_object.insert("current_status".into(), json!(status));
    }
}

fn workflow_node_update_sets_current(
    root_object: &Map<String, Value>,
    node_name: &str,
    status: &str,
    detail: &Value,
) -> bool {
    if detail
        .get("preserveCurrent")
        .or_else(|| detail.get("preserve_current"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return false;
    }
    if status != "skipped" {
        return true;
    }
    if node_name == WorkflowNodeName::Reviewer.as_str() {
        return true;
    }
    root_object
        .get("currentNode")
        .or_else(|| root_object.get("current_node"))
        .and_then(Value::as_str)
        == Some(node_name)
}

fn apply_workflow_transition_update(root: &mut Value, detail: &Value, updated_at: &str) {
    let Some(root_object) = root.as_object_mut() else {
        return;
    };
    let event_sequence = workflow_event_sequence_u64(detail)
        .map(Value::from)
        .unwrap_or(Value::Null);
    if !event_sequence.is_null() {
        root_object.insert("lastEventSequence".into(), event_sequence.clone());
        root_object.insert("last_event_sequence".into(), event_sequence.clone());
    }
    let transitions = root_object
        .entry("transitions")
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Some(transitions) = transitions.as_array_mut() {
        transitions.push(json!({
            "from": detail.get("from").cloned().unwrap_or(Value::Null),
            "to": detail.get("to").cloned().unwrap_or(Value::Null),
            "reason": detail.get("reason").cloned().unwrap_or(Value::Null),
            "topologyEdgeKnown": detail.get("topologyEdgeKnown").or_else(|| detail.get("topology_edge_known")).cloned().unwrap_or(Value::Null),
            "topology_edge_known": detail.get("topologyEdgeKnown").or_else(|| detail.get("topology_edge_known")).cloned().unwrap_or(Value::Null),
            "topologyReasonKnown": detail.get("topologyReasonKnown").or_else(|| detail.get("topology_reason_known")).cloned().unwrap_or(Value::Null),
            "topology_reason_known": detail.get("topologyReasonKnown").or_else(|| detail.get("topology_reason_known")).cloned().unwrap_or(Value::Null),
            "topologyEdgeSource": detail.get("topologyEdgeSource").or_else(|| detail.get("topology_edge_source")).cloned().unwrap_or(Value::Null),
            "topology_edge_source": detail.get("topologyEdgeSource").or_else(|| detail.get("topology_edge_source")).cloned().unwrap_or(Value::Null),
            "topologyEdgeLabel": detail.get("topologyEdgeLabel").or_else(|| detail.get("topology_edge_label")).cloned().unwrap_or(Value::Null),
            "topology_edge_label": detail.get("topologyEdgeLabel").or_else(|| detail.get("topology_edge_label")).cloned().unwrap_or(Value::Null),
            "detail": detail.get("detail").cloned().unwrap_or_else(|| json!({})),
            "eventSequence": event_sequence,
            "event_sequence": event_sequence,
            "updatedAt": updated_at,
            "updated_at": updated_at,
        }));
    }
    if let Some(target) = detail
        .get("to")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|target| !target.is_empty())
    {
        let current_status = match workflow_current_status_for_target(root_object, target) {
            Value::Null => workflow_transition_default_current_status(detail),
            status => status,
        };
        root_object.insert("currentNode".into(), json!(target));
        root_object.insert("current_node".into(), json!(target));
        root_object.insert("currentStatus".into(), current_status.clone());
        root_object.insert("current_status".into(), current_status);
    }
}

fn workflow_transition_default_current_status(detail: &Value) -> Value {
    if workflow_transition_preserves_current_status(detail) {
        Value::Null
    } else {
        json!(WorkflowNodeStatus::Running)
    }
}

fn workflow_transition_preserves_current_status(detail: &Value) -> bool {
    let transition_detail = detail.get("detail").unwrap_or(detail);
    if transition_detail
        .get("preserveCurrentStatus")
        .or_else(|| transition_detail.get("preserve_current_status"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return true;
    }
    transition_detail
        .get("runState")
        .or_else(|| transition_detail.get("run_state"))
        .and_then(Value::as_str)
        .map(|status| {
            matches!(
                status.trim().to_ascii_lowercase().as_str(),
                "completed" | "failed" | "canceled" | "cancelled"
            )
        })
        .unwrap_or(false)
}

fn workflow_current_status_for_target(root_object: &Map<String, Value>, target: &str) -> Value {
    root_object
        .get("nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|node| node.get("node").and_then(Value::as_str) == Some(target))
        .and_then(|node| node.get("status"))
        .cloned()
        .unwrap_or(Value::Null)
}

fn workflow_current_status_for_current_node(root_object: &Map<String, Value>) -> Option<Value> {
    let current_node = root_object
        .get("currentNode")
        .or_else(|| root_object.get("current_node"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|node| !node.is_empty())?;
    let status = workflow_current_status_for_target(root_object, current_node);
    (!status.is_null()).then_some(status)
}

fn workflow_group_room_context(
    provider_data: Option<&Value>,
    request_source: &str,
) -> Option<Value> {
    let provider_data = provider_data?;
    let source = request_source.trim();
    let conversation_kind = workflow_string_arg(
        provider_data,
        &[
            "chatType",
            "chat_type",
            "conversationType",
            "conversation_type",
            "targetType",
            "target_type",
        ],
    );
    let room_id = workflow_string_arg(
        provider_data,
        &["roomId", "room_id", "chatRoomId", "chat_room_id"],
    );
    let channel_id = workflow_string_arg(provider_data, &["channelId", "channel_id", "channel"]);
    let chat_id = workflow_string_arg(provider_data, &["chatId", "chat_id"]);
    let thread_id = workflow_string_arg(
        provider_data,
        &["threadId", "thread_id", "message_thread_id"],
    );
    let group_id = workflow_string_arg(
        provider_data,
        &["groupId", "group_id", "groupCode", "group_code"],
    );
    let groupish_kind = conversation_kind
        .as_deref()
        .map(|kind| {
            matches!(
                kind.to_ascii_lowercase().as_str(),
                "group" | "room" | "chat" | "channel" | "thread"
            )
        })
        .unwrap_or(false);
    if !groupish_kind
        && room_id.is_none()
        && channel_id.is_none()
        && chat_id.is_none()
        && thread_id.is_none()
        && group_id.is_none()
    {
        return None;
    }
    let mut detail = Map::new();
    if !source.is_empty() {
        detail.insert("source".into(), json!(source));
    }
    if let Some(kind) = conversation_kind {
        detail.insert("conversationKind".into(), json!(kind));
    }
    if let Some(room_id) = room_id {
        detail.insert("roomId".into(), json!(room_id));
    }
    if let Some(channel_id) = channel_id {
        detail.insert("channelId".into(), json!(channel_id));
    }
    if let Some(chat_id) = chat_id {
        detail.insert("chatId".into(), json!(chat_id));
    }
    if let Some(thread_id) = thread_id {
        detail.insert("threadId".into(), json!(thread_id));
    }
    if let Some(group_id) = group_id {
        detail.insert("groupId".into(), json!(group_id));
    }
    Some(Value::Object(detail))
}

fn workflow_string_arg(value: &Value, keys: &[&str]) -> Option<String> {
    for scope in workflow_scopes(value) {
        for key in keys {
            if let Some(found) = scope
                .get(*key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                return Some(found.to_string());
            }
        }
    }
    None
}

fn workflow_scopes(value: &Value) -> Vec<&Value> {
    let mut scopes = vec![value];
    for key in [
        "source",
        "origin",
        "conversation",
        "sourceContext",
        "source_context",
        "message",
    ] {
        if let Some(scope) = value.get(key).filter(|scope| scope.is_object()) {
            scopes.push(scope);
        }
    }
    scopes
}

fn workflow_tool_context_label(context: ToolExecutionContext) -> &'static str {
    match context {
        ToolExecutionContext::Interactive => "interactive",
        ToolExecutionContext::ScheduledJob => "scheduled_job",
        ToolExecutionContext::SubagentLeaf => "subagent_leaf",
        ToolExecutionContext::SubagentOrchestrator => "subagent_orchestrator",
    }
}
