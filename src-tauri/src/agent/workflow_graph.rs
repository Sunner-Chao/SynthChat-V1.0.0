use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::{
    error::AppResult,
    models::{now_iso, AgentRunPhaseRecord, AgentRunRecord, SendChatRequest, ToolDefinition},
    store::AppStore,
};

use super::{decision_parser::validated_tool_requests_from_decision, ToolExecutionContext};

pub(super) const SYNTHGRAPH_WORKFLOW_SCHEMA: &str = "synthgraph_workflow_v1";

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum WorkflowNodeName {
    Queue,
    Planner,
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
            Self::Executor => "executor",
            Self::Reviewer => "reviewer",
            Self::Approval => "approval",
            Self::Checkpoint => "checkpoint",
            Self::GroupRoom => "group_room",
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum WorkflowReviewerRoute {
    Completed {
        message_id: String,
        model: Option<String>,
        provider_id: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WorkflowDriver {
    mode: WorkflowMode,
}

impl WorkflowDriver {
    pub(super) fn new(mode: WorkflowMode) -> Self {
        Self { mode }
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

    pub(super) fn planner_running(
        self,
        store: &AppStore,
        run_id: &str,
        iteration: u32,
    ) -> AppResult<()> {
        record_workflow_planner_running(store, run_id, iteration, self.mode)
    }

    pub(super) fn approval_resumed(
        self,
        store: &AppStore,
        run_id: &str,
        server_id: &str,
        tool_name: &str,
    ) -> AppResult<()> {
        record_workflow_approval_resumed(store, run_id, self.mode, server_id, tool_name)
    }

    pub(super) fn planner_route(
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

    pub(super) fn executor_continue(
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

    pub(super) fn executor_approval(
        self,
        store: &AppStore,
        run_id: &str,
        iteration: u32,
        server_id: &str,
        tool_name: &str,
    ) -> AppResult<WorkflowExecutorRoute> {
        resolve_workflow_executor_approval_route(store, run_id, iteration, server_id, tool_name)
    }

    pub(super) fn reviewer_completed(
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
}

pub(super) fn push_workflow_graph_bootstrap(
    run: &mut AgentRunRecord,
    request: &SendChatRequest,
    request_source: &str,
    tool_context: ToolExecutionContext,
    mode: WorkflowMode,
) {
    let updated_at = now_iso();
    let group_room_context = workflow_group_room_context(request.provider_data.as_ref(), request_source);
    let snapshot = json!({
        "schema": SYNTHGRAPH_WORKFLOW_SCHEMA,
        "mode": mode.as_str(),
        "requestSource": request_source,
        "toolContext": workflow_tool_context_label(tool_context),
        "currentNode": WorkflowNodeName::Planner,
        "updatedAt": updated_at,
        "transitions": [],
        "nodes": [
            workflow_node_snapshot(
                WorkflowNodeName::Queue,
                if request.queue_item_id.is_some() {
                    WorkflowNodeStatus::Completed
                } else {
                    WorkflowNodeStatus::Skipped
                },
                request
                    .queue_item_id
                    .as_ref()
                    .map(|queue_item_id| json!({"queueItemId": queue_item_id}))
                    .unwrap_or_else(|| json!({"reason": "direct_turn"})),
                &updated_at,
            ),
            workflow_node_snapshot(
                WorkflowNodeName::GroupRoom,
                if group_room_context.is_some() {
                    WorkflowNodeStatus::Completed
                } else {
                    WorkflowNodeStatus::Skipped
                },
                group_room_context.unwrap_or_else(|| json!({"reason": "no_group_room_context"})),
                &updated_at,
            ),
            workflow_node_snapshot(
                WorkflowNodeName::Planner,
                WorkflowNodeStatus::Pending,
                json!({}),
                &updated_at,
            ),
            workflow_node_snapshot(
                WorkflowNodeName::Executor,
                WorkflowNodeStatus::Pending,
                json!({}),
                &updated_at,
            ),
            workflow_node_snapshot(
                WorkflowNodeName::Approval,
                WorkflowNodeStatus::Pending,
                json!({}),
                &updated_at,
            ),
            workflow_node_snapshot(
                WorkflowNodeName::Checkpoint,
                WorkflowNodeStatus::Pending,
                json!({}),
                &updated_at,
            ),
            workflow_node_snapshot(
                WorkflowNodeName::Reviewer,
                WorkflowNodeStatus::Pending,
                json!({}),
                &updated_at,
            ),
        ],
    });
    run.workflow_graph = Some(snapshot.clone());
    run.phase_events.push(AgentRunPhaseRecord {
        phase: "workflow_graph_initialized".into(),
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
        "workflow_node",
        json!({
            "schema": SYNTHGRAPH_WORKFLOW_SCHEMA,
            "node": node,
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
    append_workflow_phase_event(
        store,
        run_id,
        "workflow_transition",
        json!({
            "schema": SYNTHGRAPH_WORKFLOW_SCHEMA,
            "from": from,
            "to": to,
            "reason": reason,
            "detail": detail,
        }),
        format!("workflow transition {} -> {}", from.as_str(), to.as_str()),
    )
}

pub(super) fn append_workflow_checkpoint_event(
    store: &AppStore,
    run_id: &str,
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
        WorkflowNodeStatus::Completed,
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
    let detail = json!({
        "iteration": iteration,
        "serverId": server_id,
        "toolName": tool_name,
    });
    append_workflow_transition_event(
        store,
        run_id,
        WorkflowNodeName::Executor,
        WorkflowNodeName::Approval,
        "approval_required",
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

pub(super) fn record_workflow_approval_resumed(
    store: &AppStore,
    run_id: &str,
    mode: WorkflowMode,
    server_id: &str,
    tool_name: &str,
) -> AppResult<()> {
    let mut detail = workflow_turn_detail(mode, None);
    detail.insert("serverId".into(), json!(server_id));
    detail.insert("toolName".into(), json!(tool_name));
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
        "approval_resumed",
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

pub(super) fn record_workflow_planner_failed(
    store: &AppStore,
    run_id: &str,
    iteration: u32,
    mode: WorkflowMode,
    error: &str,
) -> AppResult<()> {
    let mut detail = workflow_turn_detail(mode, Some(iteration));
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
) -> AppResult<()> {
    let mut planner_detail = workflow_turn_detail(mode, Some(iteration));
    planner_detail.insert("action".into(), json!("tool"));
    planner_detail.insert("toolCount".into(), json!(tool_count));
    append_workflow_node_event(
        store,
        run_id,
        WorkflowNodeName::Planner,
        WorkflowNodeStatus::Completed,
        Value::Object(planner_detail),
    )?;

    let mut transition_detail = workflow_turn_detail(mode, Some(iteration));
    transition_detail.insert("toolCount".into(), json!(tool_count));
    append_workflow_transition_event(
        store,
        run_id,
        WorkflowNodeName::Planner,
        WorkflowNodeName::Executor,
        "tool_calls",
        Value::Object(transition_detail),
    )?;

    let mut executor_detail = workflow_turn_detail(mode, Some(iteration));
    executor_detail.insert("toolCount".into(), json!(tool_count));
    append_workflow_node_event(
        store,
        run_id,
        WorkflowNodeName::Executor,
        WorkflowNodeStatus::Running,
        Value::Object(executor_detail),
    )
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
        "tool_observations_recorded",
        Value::Object(executor_detail),
    )
}

pub(super) fn record_workflow_planner_to_reviewer(
    store: &AppStore,
    run_id: &str,
    iteration: u32,
    mode: WorkflowMode,
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
        WorkflowNodeName::Reviewer,
        "final_answer_candidate",
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
            let requests = match validated_tool_requests_from_decision(decision, available_tools) {
                Ok(requests) => requests,
                Err(error) => {
                    let error_message = error.to_string();
                    record_workflow_planner_failed(
                        store,
                        run_id,
                        iteration,
                        mode,
                        &error_message,
                    )?;
                    return Ok(WorkflowPlannerRoute::Recover {
                        observation: format!(
                            "{} tool schema error: {}",
                            workflow_iteration_label(mode, iteration),
                            error_message
                        ),
                    });
                }
            };
            if requests.is_empty() {
                let error_message =
                    "planner requested tool action without a valid tool name".to_string();
                record_workflow_planner_failed(store, run_id, iteration, mode, &error_message)?;
                return Ok(WorkflowPlannerRoute::Recover {
                    observation: format!(
                        "{} tool error: {}",
                        workflow_iteration_label(mode, iteration),
                        error_message
                    ),
                });
            }
            let request_count = requests.len();
            record_workflow_planner_to_executor(store, run_id, iteration, mode, request_count)?;
            Ok(WorkflowPlannerRoute::ExecuteTools {
                requests,
                request_count,
            })
        }
        _ => {
            record_workflow_planner_to_reviewer(store, run_id, iteration, mode)?;
            Ok(WorkflowPlannerRoute::ReviewFinal {
                content: decision
                    .get("content")
                    .or_else(|| decision.get("answer"))
                    .and_then(Value::as_str)
                    .unwrap_or(fallback_content.trim())
                    .to_string(),
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

fn append_workflow_phase_event(
    store: &AppStore,
    run_id: &str,
    phase: &str,
    detail: Value,
    activity: String,
) -> AppResult<()> {
    let mut run = store.agent_run(run_id)?;
    let updated_at = now_iso();
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

fn workflow_node_snapshot(
    node: WorkflowNodeName,
    status: WorkflowNodeStatus,
    detail: Value,
    updated_at: &str,
) -> Value {
    json!({
        "node": node,
        "status": status,
        "detail": detail,
        "updatedAt": updated_at,
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

fn workflow_iteration_label(mode: WorkflowMode, iteration: u32) -> String {
    match mode {
        WorkflowMode::ChatTurn => format!("Iteration {iteration}"),
        WorkflowMode::ApprovalContinuation => format!("Continuation iteration {iteration}"),
    }
}

fn apply_workflow_graph_event(
    run: &mut AgentRunRecord,
    phase: &str,
    detail: &Value,
    updated_at: &str,
) {
    match phase {
        "workflow_graph_initialized" => {
            run.workflow_graph = Some(detail.clone());
        }
        "workflow_node" => {
            let root = ensure_workflow_graph_root(run, updated_at);
            apply_workflow_node_update(root, detail, updated_at);
        }
        "workflow_transition" => {
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
            "updatedAt": updated_at,
        }));
    }
    let root = run.workflow_graph.as_mut().expect("workflow graph initialized");
    if let Some(object) = root.as_object_mut() {
        object.insert("updatedAt".into(), json!(updated_at));
        object
            .entry("schema")
            .or_insert_with(|| json!(SYNTHGRAPH_WORKFLOW_SCHEMA));
        object
            .entry("nodes")
            .or_insert_with(|| Value::Array(Vec::new()));
        object
            .entry("transitions")
            .or_insert_with(|| Value::Array(Vec::new()));
        object
            .entry("currentNode")
            .or_insert(Value::Null);
    }
    root
}

fn apply_workflow_node_update(root: &mut Value, detail: &Value, updated_at: &str) {
    let Some(root_object) = root.as_object_mut() else {
        return;
    };
    let node_name = detail
        .get("node")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let status = detail
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
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
                "status": status,
                "detail": node_detail,
                "updatedAt": updated_at,
            });
        } else {
            nodes.push(json!({
                "node": node_name,
                "status": status,
                "detail": node_detail,
                "updatedAt": updated_at,
            }));
        }
    }
    if status.as_str() != "skipped" {
        root_object.insert("currentNode".into(), json!(node_name));
    }
}

fn apply_workflow_transition_update(root: &mut Value, detail: &Value, updated_at: &str) {
    let Some(root_object) = root.as_object_mut() else {
        return;
    };
    let transitions = root_object
        .entry("transitions")
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Some(transitions) = transitions.as_array_mut() {
        transitions.push(json!({
            "from": detail.get("from").cloned().unwrap_or(Value::Null),
            "to": detail.get("to").cloned().unwrap_or(Value::Null),
            "reason": detail.get("reason").cloned().unwrap_or(Value::Null),
            "detail": detail.get("detail").cloned().unwrap_or_else(|| json!({})),
            "updatedAt": updated_at,
        }));
    }
    if let Some(target) = detail.get("to").cloned() {
        root_object.insert("currentNode".into(), target);
    }
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
    let room_id =
        workflow_string_arg(provider_data, &["roomId", "room_id", "chatRoomId", "chat_room_id"]);
    let channel_id =
        workflow_string_arg(provider_data, &["channelId", "channel_id", "channel"]);
    let chat_id = workflow_string_arg(provider_data, &["chatId", "chat_id"]);
    let thread_id =
        workflow_string_arg(provider_data, &["threadId", "thread_id", "message_thread_id"]);
    let group_id =
        workflow_string_arg(provider_data, &["groupId", "group_id", "groupCode", "group_code"]);
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
