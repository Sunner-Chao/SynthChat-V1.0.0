use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
};

use futures::future::join_all;
use serde_json::{json, Value};

use crate::{
    error::{AppError, AppResult},
    models::{
        new_id, now_iso, AgentDefinition, AgentRunRecord, ChatConfig, ChatMessage, SendChatRequest,
        ToolEvent,
    },
    store::AppStore,
};

use super::{
    acp_events::acp_tool_event_kind,
    acp_server::{acp_json_rpc_error, acp_json_rpc_response},
    acp_session::latest_run_record_for_session,
    append_parent_phase_event, normalize_toolset_name, payload_string_array,
    push_tool_event_record, redact_json_value, redact_sensitive_text,
    run_chat_turn_with_toolset_policy_and_iteration_limit,
};

#[cfg(test)]
use super::acp_prompt::acp_idle_steer_prompt_text;

#[cfg(test)]
use super::acp_queue::acp_queue_update_for_current_state;

#[cfg(test)]
use super::acp_auth::{
    acp_authenticate_result_from_methods, acp_terminal_setup_auth_method,
    ACP_TERMINAL_SETUP_AUTH_METHOD_ID,
};

pub(super) use super::acp_commands::{
    acp_compact_text_for_prompt, acp_context_text_for_prompt, acp_help_text_for_prompt,
    acp_local_command_reply_for_prompt, acp_model_text_for_prompt, acp_reset_text_for_prompt,
    acp_tools_text_for_prompt, acp_version_text_for_prompt,
};
pub(super) use super::acp_events::acp_final_agent_message_notifications;
pub(super) use super::acp_events::acp_tool_event_update_from_value;
pub(super) use super::acp_prompt::acp_prompt_provider_data_from_params;
pub(super) use super::acp_prompt_runtime::acp_emit_new_tool_notifications;
pub(super) use super::acp_session::{
    acp_list_sessions_for_store, acp_take_interrupted_prompt_text,
};
pub(super) use super::acp_subprocess::{
    acp_closed_error, acp_delegate_error_implies_aborted, acp_delegate_was_aborted,
    acp_handle_server_message, acp_observer_aborted, acp_rpc_request, acp_timeout_error,
    append_acp_observer_phase, run_acp_prompt, AcpRunObserver,
};

static DELEGATION_SPAWN_PAUSED: OnceLock<Mutex<bool>> = OnceLock::new();

// Per-run lock that serializes all delegate_task calls for the same parent run.
// Without this, two parallel tool calls in the same agent batch can both read
// the same child_count, both pass the max_subagents check, and then both spawn
// their full child sets — exceeding the limit and producing duplicate child_index
// values.
static DELEGATION_RUN_LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    OnceLock::new();

fn delegation_run_lock(parent_run_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    let locks = DELEGATION_RUN_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = locks.lock().unwrap_or_else(|e| e.into_inner());
    // LRU-style eviction: remove entries that have no external holders.
    if map.len() >= 512 {
        map.retain(|_, arc| Arc::strong_count(arc) > 1);
    }
    map.entry(parent_run_id.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

pub(super) use super::acp_child_events::{
    acp_session_update_kind, acp_session_update_record, acp_session_update_text,
    append_acp_tool_event_record,
};
pub(super) use super::acp_client::{
    acp_prompt_result_error, acp_prompt_result_is_cancelled, acp_prompt_result_stop_reason,
    acp_session_cancel_request, acp_session_start_request,
};
pub(super) use super::acp_client_fs::{
    acp_path_within_cwd, acp_read_text_file_response, acp_sensitive_path_reason,
    acp_write_text_file_response,
};
pub(super) use super::acp_tool_output::acp_string_text;
use super::delegation_acp::execute_acp_delegate_task_request;
pub(super) use super::delegation_artifacts::{
    append_delegation_memory_observation, append_diagnostic_artifact_to_error,
    delegation_file_state_reminder, save_subagent_failure_diagnostic_artifact,
};
pub(super) use super::delegation_request::{
    apply_delegation_iteration_budget, apply_delegation_runtime_config, delegate_task_requests,
    DelegateTaskRequest,
};
use super::delegation_run_state::{latest_run_for_conversation, mark_run_as_subagent};
pub(super) use super::delegation_scope::{acp_mcp_servers_for_agent, delegation_child_toolsets};
use super::delegation_synthchat::execute_synthchat_delegate_task_request;
use super::workflow_graph::{
    append_workflow_transition_event, workflow_mode_for_run, WorkflowDriver, WorkflowMode,
    WorkflowNodeName, WORKFLOW_REASON_DELEGATE_TASK_COMPLETED,
    WORKFLOW_REASON_DELEGATE_TASK_FAILED, WORKFLOW_REASON_DELEGATE_TASK_STARTED,
};

pub(super) async fn delegate_task_tool(
    store: &AppStore,
    agent: &AgentDefinition,
    _conversation_id: &str,
    parent_run_id: &str,
    payload: &Value,
) -> AppResult<String> {
    let is_batch = payload.get("tasks").is_some();
    let mut requests = delegate_task_requests(payload)?;
    let chat_config = store.config()?.chat;
    apply_delegation_iteration_budget(&mut requests, agent.max_tool_iterations);
    let max_concurrent_children = chat_config.delegation_max_concurrent_children.max(1);
    if is_batch && requests.len() as u32 > max_concurrent_children {
        return Err(AppError::BadRequest(format!(
            "delegate_task requested {} concurrent children, but delegationMaxConcurrentChildren is {}",
            requests.len(),
            max_concurrent_children
        )));
    }
    apply_delegation_runtime_config(&mut requests, chat_config.delegation_orchestrator_enabled);
    if delegation_spawn_paused() {
        return Err(AppError::BadRequest(
            "delegate_task spawning is paused. Use /subagents resume before retrying.".into(),
        ));
    }
    let parent = ensure_parent_run_accepts_delegation(store, parent_run_id)?;
    let parent_depth = parent.subagent_depth.unwrap_or(0);
    if parent_depth >= agent.max_subagent_depth {
        return Err(AppError::BadRequest(format!(
            "delegate_task depth limit reached: {}",
            agent.max_subagent_depth
        )));
    }
    // Serialize concurrent delegate_task calls for the same parent run so that
    // child_count is read and the subagent limit is checked atomically with
    // respect to other batches on this run.  Without this lock, two parallel
    // tool calls in the same agent batch can both read the same child_count,
    // both pass the max_subagents check, and each spawn their full child set —
    // exceeding the limit and producing colliding child_index values.
    let run_lock = delegation_run_lock(parent_run_id);
    let _run_guard = run_lock.lock().await;
    let child_count = store
        .agent_runs()?
        .into_iter()
        .filter(|run| run.parent_run_id.as_deref() == Some(parent_run_id))
        .count() as u32;
    if child_count + requests.len() as u32 > agent.max_subagents {
        return Err(AppError::BadRequest(format!(
            "delegate_task subagent limit reached: {} existing + {} requested exceeds {}",
            child_count,
            requests.len(),
            agent.max_subagents
        )));
    }

    let workflow_mode = workflow_mode_for_run(&parent);
    record_parent_delegation_group_room_started(
        store,
        parent_run_id,
        workflow_mode,
        agent,
        &chat_config,
        parent_depth,
        child_count,
        is_batch,
        &requests,
    )?;

    let delegation_started_at = now_iso();
    let results = if is_batch {
        let futures = requests.iter().enumerate().map(|(offset, request)| {
            let child_index = child_count + offset as u32 + 1;
            execute_delegate_task_request(
                store,
                agent,
                parent_run_id,
                parent_depth,
                child_index,
                request,
                &chat_config.delegation_subagent_provider_id,
                &chat_config.delegation_subagent_model,
                chat_config.delegation_subagent_auto_approve,
                chat_config.delegation_inherit_mcp_toolsets,
            )
        });
        let resolved = join_all(futures).await;
        let mut results = Vec::with_capacity(resolved.len());
        let mut errors: Vec<String> = Vec::new();
        for result in resolved {
            match result {
                Ok(value) => results.push(value),
                Err(error) => {
                    // Collect ALL infrastructure errors so none are silently
                    // discarded. Only the first is used as the primary error
                    // returned to the caller, but additional errors are merged
                    // into the message so diagnostics capture the full picture.
                    errors.push(error.to_string());
                }
            }
        }
        if errors.is_empty() {
            Ok(results)
        } else {
            let combined = if errors.len() == 1 {
                errors.remove(0)
            } else {
                format!(
                    "{} subagent infrastructure errors: {}",
                    errors.len(),
                    errors.join(" | ")
                )
            };
            Err((AppError::Agent(combined), results))
        }
    } else {
        execute_delegate_task_request(
            store,
            agent,
            parent_run_id,
            parent_depth,
            child_count + 1,
            requests
                .first()
                .ok_or_else(|| AppError::BadRequest("delegate_task produced no request".into()))?,
            &chat_config.delegation_subagent_provider_id,
            &chat_config.delegation_subagent_model,
            chat_config.delegation_subagent_auto_approve,
            chat_config.delegation_inherit_mcp_toolsets,
        )
        .await
        .map(|result| vec![result])
        .map_err(|error| (error, Vec::new()))
    };
    let results = match results {
        Ok(results) => results,
        Err((error, partial_results)) => {
            let error_text = error.to_string();
            record_parent_delegation_group_room_failed(
                store,
                parent_run_id,
                workflow_mode,
                agent,
                &chat_config,
                parent_depth,
                child_count,
                is_batch,
                &requests,
                &partial_results,
                Some(&error_text),
            )?;
            return Err(error);
        }
    };
    if !is_batch {
        let result = results
            .into_iter()
            .next()
            .ok_or_else(|| AppError::BadRequest("delegate_task produced no result".into()))?;
        if result.get("status").and_then(Value::as_str) == Some("failed") {
            let error_text = result
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("delegate_task failed")
                .to_string();
            record_parent_delegation_group_room_failed(
                store,
                parent_run_id,
                workflow_mode,
                agent,
                &chat_config,
                parent_depth,
                child_count,
                is_batch,
                &requests,
                std::slice::from_ref(&result),
                Some(&error_text),
            )?;
            return Err(AppError::BadRequest(error_text));
        }
        record_parent_delegation_group_room_completed(
            store,
            parent_run_id,
            workflow_mode,
            agent,
            &chat_config,
            parent_depth,
            child_count,
            is_batch,
            &requests,
            std::slice::from_ref(&result),
        )?;
        let mut response = json!({
            "childRunId": result["childRunId"],
            "role": result["role"],
            "task": result["task"],
            "maxIterations": result["maxIterations"],
            "result": result["result"]
        });
        if let Some(reminder) =
            delegation_file_state_reminder(store, parent_run_id, &delegation_started_at)?
        {
            response["fileStateReminder"] = reminder;
        }
        return Ok(serde_json::to_string_pretty(&response)?);
    }
    let ok = results
        .iter()
        .all(|result| result.get("status").and_then(Value::as_str) == Some("completed"));
    record_parent_delegation_group_room_completed(
        store,
        parent_run_id,
        workflow_mode,
        agent,
        &chat_config,
        parent_depth,
        child_count,
        is_batch,
        &requests,
        &results,
    )?;
    let mut response = json!({
        "ok": ok,
        "count": results.len(),
        "results": results
    });
    if let Some(reminder) =
        delegation_file_state_reminder(store, parent_run_id, &delegation_started_at)?
    {
        response["fileStateReminder"] = reminder;
    }
    Ok(serde_json::to_string_pretty(&response)?)
}

fn record_parent_delegation_group_room_started(
    store: &AppStore,
    parent_run_id: &str,
    workflow_mode: WorkflowMode,
    agent: &AgentDefinition,
    chat_config: &ChatConfig,
    parent_depth: u32,
    existing_child_count: u32,
    is_batch: bool,
    requests: &[DelegateTaskRequest],
) -> AppResult<()> {
    let detail = delegation_group_room_started_detail(
        agent,
        chat_config,
        parent_depth,
        existing_child_count,
        is_batch,
        requests,
    );
    append_workflow_transition_event(
        store,
        parent_run_id,
        WorkflowNodeName::Executor,
        WorkflowNodeName::GroupRoom,
        WORKFLOW_REASON_DELEGATE_TASK_STARTED,
        delegation_group_room_transition_detail(&detail),
    )?;
    WorkflowDriver::new(workflow_mode)
        .group_room()
        .running(store, parent_run_id, detail)
}

fn record_parent_delegation_group_room_completed(
    store: &AppStore,
    parent_run_id: &str,
    workflow_mode: WorkflowMode,
    agent: &AgentDefinition,
    chat_config: &ChatConfig,
    parent_depth: u32,
    existing_child_count: u32,
    is_batch: bool,
    requests: &[DelegateTaskRequest],
    results: &[Value],
) -> AppResult<()> {
    let detail = delegation_group_room_finished_detail(
        agent,
        chat_config,
        parent_depth,
        existing_child_count,
        is_batch,
        requests,
        results,
        "completed",
        None,
    );
    WorkflowDriver::new(workflow_mode).group_room().completed(
        store,
        parent_run_id,
        detail.clone(),
    )?;
    append_workflow_transition_event(
        store,
        parent_run_id,
        WorkflowNodeName::GroupRoom,
        WorkflowNodeName::Executor,
        WORKFLOW_REASON_DELEGATE_TASK_COMPLETED,
        delegation_group_room_transition_detail(&detail),
    )
}

fn record_parent_delegation_group_room_failed(
    store: &AppStore,
    parent_run_id: &str,
    workflow_mode: WorkflowMode,
    agent: &AgentDefinition,
    chat_config: &ChatConfig,
    parent_depth: u32,
    existing_child_count: u32,
    is_batch: bool,
    requests: &[DelegateTaskRequest],
    results: &[Value],
    error: Option<&str>,
) -> AppResult<()> {
    let detail = delegation_group_room_finished_detail(
        agent,
        chat_config,
        parent_depth,
        existing_child_count,
        is_batch,
        requests,
        results,
        "failed",
        error,
    );
    append_workflow_transition_event(
        store,
        parent_run_id,
        WorkflowNodeName::GroupRoom,
        WorkflowNodeName::Executor,
        WORKFLOW_REASON_DELEGATE_TASK_FAILED,
        delegation_group_room_transition_detail(&detail),
    )?;
    WorkflowDriver::new(workflow_mode)
        .group_room()
        .failed(store, parent_run_id, detail)
}

fn delegation_group_room_started_detail(
    agent: &AgentDefinition,
    chat_config: &ChatConfig,
    parent_depth: u32,
    existing_child_count: u32,
    is_batch: bool,
    requests: &[DelegateTaskRequest],
) -> Value {
    delegation_group_room_base_detail(
        agent,
        chat_config,
        parent_depth,
        existing_child_count,
        is_batch,
        requests,
        "started",
    )
}

fn delegation_group_room_finished_detail(
    agent: &AgentDefinition,
    chat_config: &ChatConfig,
    parent_depth: u32,
    existing_child_count: u32,
    is_batch: bool,
    requests: &[DelegateTaskRequest],
    results: &[Value],
    phase: &str,
    error: Option<&str>,
) -> Value {
    let mut detail = delegation_group_room_base_detail(
        agent,
        chat_config,
        parent_depth,
        existing_child_count,
        is_batch,
        requests,
        phase,
    );
    let completed_children = delegation_result_status_count(results, "completed");
    let failed_children = delegation_result_status_count(results, "failed");
    let aborted_children = delegation_result_status_count(results, "aborted");
    let known_children = completed_children + failed_children + aborted_children;
    detail["ok"] = json!(!results.is_empty() && failed_children == 0 && aborted_children == 0);
    detail["completedChildren"] = json!(completed_children);
    detail["failedChildren"] = json!(failed_children);
    detail["abortedChildren"] = json!(aborted_children);
    detail["unknownChildren"] = json!(results.len().saturating_sub(known_children));
    detail["results"] = json!(delegation_group_room_result_summaries(results));
    if let Some(error) = error {
        detail["error"] = json!(delegation_summary_text(error, 500));
    }
    detail
}

fn delegation_group_room_base_detail(
    agent: &AgentDefinition,
    chat_config: &ChatConfig,
    parent_depth: u32,
    existing_child_count: u32,
    is_batch: bool,
    requests: &[DelegateTaskRequest],
    phase: &str,
) -> Value {
    json!({
        "source": "delegate_task",
        "phase": phase,
        "batch": is_batch,
        "requestedChildren": requests.len(),
        "existingChildren": existing_child_count,
        "parentDepth": parent_depth,
        "childDepth": parent_depth.saturating_add(1),
        "maxSubagents": agent.max_subagents,
        "maxSubagentDepth": agent.max_subagent_depth,
        "maxConcurrentChildren": chat_config.delegation_max_concurrent_children.max(1),
        "strategy": chat_config.delegation_strategy.clone(),
        "orchestratorEnabled": chat_config.delegation_orchestrator_enabled,
        "subagentAutoApprove": chat_config.delegation_subagent_auto_approve,
        "inheritMcpToolsets": chat_config.delegation_inherit_mcp_toolsets,
        "children": delegation_group_room_request_summaries(existing_child_count, requests)
    })
}

fn delegation_group_room_transition_detail(detail: &Value) -> Value {
    let mut transition = json!({
        "source": detail.get("source").cloned().unwrap_or_else(|| json!("delegate_task")),
        "phase": detail.get("phase").cloned().unwrap_or(Value::Null),
        "batch": detail.get("batch").cloned().unwrap_or(Value::Null),
        "requestedChildren": detail
            .get("requestedChildren")
            .cloned()
            .unwrap_or(Value::Null),
        "existingChildren": detail
            .get("existingChildren")
            .cloned()
            .unwrap_or(Value::Null),
        "parentDepth": detail.get("parentDepth").cloned().unwrap_or(Value::Null),
    });
    for key in [
        "ok",
        "completedChildren",
        "failedChildren",
        "abortedChildren",
        "unknownChildren",
    ] {
        if let Some(value) = detail.get(key) {
            transition[key] = value.clone();
        }
    }
    if let Some(error) = detail.get("error") {
        transition["error"] = error.clone();
    }
    transition
}

fn delegation_group_room_request_summaries(
    existing_child_count: u32,
    requests: &[DelegateTaskRequest],
) -> Vec<Value> {
    requests
        .iter()
        .enumerate()
        .map(|(offset, request)| {
            let mut summary = json!({
                "childIndex": existing_child_count + offset as u32 + 1,
                "role": &request.role,
                "taskPreview": delegation_summary_text(&request.task, 240),
                "toolsets": &request.toolsets,
                "canDelegate": request.can_delegate,
                "maxIterations": request.max_iterations,
                "transport": if request.acp_command.is_empty() { "synthchat" } else { "acp" },
            });
            if !request.acp_command.is_empty() {
                summary["acpCommand"] = json!(delegation_summary_text(&request.acp_command, 120));
            }
            if !request.acp_session_mode.is_empty() {
                summary["acpSessionMode"] = json!(&request.acp_session_mode);
            }
            summary
        })
        .collect()
}

fn delegation_group_room_result_summaries(results: &[Value]) -> Vec<Value> {
    results
        .iter()
        .map(|result| {
            let mut summary = json!({
                "status": result
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
            });
            for key in [
                "childRunId",
                "childConversationId",
                "role",
                "maxIterations",
                "transport",
            ] {
                if let Some(value) = result.get(key) {
                    summary[key] = value.clone();
                }
            }
            if let Some(task) = result.get("task").and_then(Value::as_str) {
                summary["taskPreview"] = json!(delegation_summary_text(task, 240));
            }
            if let Some(output) = result.get("result").and_then(Value::as_str) {
                summary["resultPreview"] = json!(delegation_summary_text(output, 500));
            }
            if let Some(error) = result.get("error").and_then(Value::as_str) {
                summary["errorPreview"] = json!(delegation_summary_text(error, 500));
            }
            if result.get("diagnosticArtifactPath").is_some() {
                summary["hasDiagnosticArtifact"] = json!(true);
            }
            summary
        })
        .collect()
}

fn delegation_result_status_count(results: &[Value], status: &str) -> usize {
    results
        .iter()
        .filter(|result| result.get("status").and_then(Value::as_str) == Some(status))
        .count()
}

fn delegation_summary_text(value: &str, max_chars: usize) -> String {
    let redacted = redact_sensitive_text(value).replace('\r', "");
    let mut chars = redacted.chars();
    let mut summary = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        summary.push_str("...");
    }
    summary
}

pub(super) fn ensure_parent_run_accepts_delegation(
    store: &AppStore,
    parent_run_id: &str,
) -> AppResult<AgentRunRecord> {
    let parent = store.agent_run(parent_run_id)?;
    if matches!(parent.state.as_str(), "completed" | "failed" | "aborted") {
        return Err(AppError::BadRequest(format!(
            "parent agent run {parent_run_id} is already terminal: {}",
            parent.state
        )));
    }
    Ok(parent)
}

pub(super) fn delegation_spawn_paused() -> bool {
    DELEGATION_SPAWN_PAUSED
        .get_or_init(|| Mutex::new(false))
        .lock()
        .map(|guard| *guard)
        .unwrap_or(false)
}

pub(super) fn set_delegation_spawn_paused(paused: bool) -> bool {
    let lock = DELEGATION_SPAWN_PAUSED.get_or_init(|| Mutex::new(false));
    match lock.lock() {
        Ok(mut guard) => {
            let previous = *guard;
            *guard = paused;
            previous
        }
        Err(_) => false,
    }
}

async fn execute_delegate_task_request(
    store: &AppStore,
    agent: &AgentDefinition,
    parent_run_id: &str,
    parent_depth: u32,
    child_index: u32,
    request: &DelegateTaskRequest,
    provider_id_override: &str,
    model_override: &str,
    subagent_auto_approve: bool,
    inherit_mcp_toolsets: bool,
) -> AppResult<Value> {
    if !request.acp_command.is_empty() {
        return execute_acp_delegate_task_request(
            store,
            agent,
            parent_run_id,
            parent_depth,
            child_index,
            request,
            subagent_auto_approve,
            inherit_mcp_toolsets,
        )
        .await;
    }
    execute_synthchat_delegate_task_request(
        store,
        agent,
        parent_run_id,
        parent_depth,
        child_index,
        request,
        provider_id_override,
        model_override,
        subagent_auto_approve,
        inherit_mcp_toolsets,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::super::acp_history::acp_session_history_updates;
    use super::*;
    use std::fs;

    #[test]
    fn delegation_group_room_started_marks_parent_graph_running() {
        let dir = std::env::temp_dir().join(format!(
            "synthchat-delegation-room-start-{}",
            new_id("test")
        ));
        fs::create_dir_all(&dir).unwrap();
        let store = AppStore::new(dir.join("state.json")).unwrap();
        let persona = store.persona(None).unwrap();
        let conversation = store
            .create_conversation(Some("delegation room".into()), Some(persona.id.clone()))
            .unwrap();
        let parent = store
            .save_agent_run(AgentRunRecord::new(
                conversation.id,
                persona.id,
                "default".into(),
            ))
            .unwrap();
        let mut agent = AgentDefinition::default();
        agent.max_subagents = 5;
        agent.max_subagent_depth = 3;
        let mut chat_config = ChatConfig::default();
        chat_config.delegation_strategy = "planner_executor".into();
        let requests = delegate_task_requests(&json!({
            "tasks": [
                {"task": "inspect parser", "role": "reader", "toolsets": ["file"]},
                {"task": "summarize runtime", "role": "reviewer", "acpCommand": "codex-acp"}
            ]
        }))
        .unwrap();

        record_parent_delegation_group_room_started(
            &store,
            &parent.run_id,
            WorkflowMode::ChatTurn,
            &agent,
            &chat_config,
            1,
            2,
            true,
            &requests,
        )
        .unwrap();

        let saved = store.agent_run(&parent.run_id).unwrap();
        let graph = saved.workflow_graph.as_ref().unwrap();
        assert_eq!(graph["currentNode"], "group_room");
        assert_eq!(graph["transitions"][0]["from"], "executor");
        assert_eq!(graph["transitions"][0]["to"], "group_room");
        assert_eq!(
            graph["transitions"][0]["reason"],
            WORKFLOW_REASON_DELEGATE_TASK_STARTED
        );
        let group_room = graph["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["node"] == "group_room")
            .unwrap();
        assert_eq!(group_room["status"], "running");
        assert_eq!(group_room["detail"]["phase"], "started");
        assert_eq!(group_room["detail"]["requestedChildren"], 2);
        assert_eq!(group_room["detail"]["existingChildren"], 2);
        assert_eq!(group_room["detail"]["parentDepth"], 1);
        assert_eq!(group_room["detail"]["strategy"], "planner_executor");
        assert_eq!(group_room["detail"]["children"][0]["childIndex"], 3);
        assert_eq!(
            group_room["detail"]["children"][0]["transport"],
            "synthchat"
        );
        assert_eq!(group_room["detail"]["children"][1]["transport"], "acp");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn delegation_group_room_completed_summarizes_successful_results() {
        let dir = std::env::temp_dir().join(format!(
            "synthchat-delegation-room-complete-{}",
            new_id("test")
        ));
        fs::create_dir_all(&dir).unwrap();
        let store = AppStore::new(dir.join("state.json")).unwrap();
        let persona = store.persona(None).unwrap();
        let conversation = store
            .create_conversation(
                Some("delegation room complete".into()),
                Some(persona.id.clone()),
            )
            .unwrap();
        let parent = store
            .save_agent_run(AgentRunRecord::new(
                conversation.id,
                persona.id,
                "default".into(),
            ))
            .unwrap();
        let mut agent = AgentDefinition::default();
        agent.max_subagents = 4;
        let chat_config = ChatConfig::default();
        let requests = delegate_task_requests(&json!({
            "tasks": [
                {"task": "inspect parser", "role": "reader"},
                {"task": "summarize runtime", "role": "reviewer", "toolsets": ["file", "web"]}
            ]
        }))
        .unwrap();
        let results = vec![
            json!({
                "status": "completed",
                "childRunId": "run-child-parser",
                "childConversationId": "child-conv-parser",
                "role": "reader",
                "task": "inspect parser",
                "result": "parser summary"
            }),
            json!({
                "status": "completed",
                "childRunId": "run-child-runtime",
                "role": "reviewer",
                "task": "summarize runtime",
                "result": "runtime summary"
            }),
        ];

        record_parent_delegation_group_room_completed(
            &store,
            &parent.run_id,
            WorkflowMode::ChatTurn,
            &agent,
            &chat_config,
            0,
            1,
            true,
            &requests,
            &results,
        )
        .unwrap();

        let saved = store.agent_run(&parent.run_id).unwrap();
        let graph = saved.workflow_graph.as_ref().unwrap();
        assert_eq!(graph["currentNode"], "executor");
        assert_eq!(graph["transitions"][0]["from"], "group_room");
        assert_eq!(graph["transitions"][0]["to"], "executor");
        assert_eq!(
            graph["transitions"][0]["reason"],
            WORKFLOW_REASON_DELEGATE_TASK_COMPLETED
        );
        assert_eq!(graph["transitions"][0]["detail"]["completedChildren"], 2);
        assert_eq!(graph["transitions"][0]["detail"]["failedChildren"], 0);
        let group_room = graph["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["node"] == "group_room")
            .unwrap();
        assert_eq!(group_room["status"], "completed");
        assert_eq!(group_room["detail"]["phase"], "completed");
        assert_eq!(group_room["detail"]["ok"], true);
        assert_eq!(group_room["detail"]["completedChildren"], 2);
        assert_eq!(group_room["detail"]["failedChildren"], 0);
        assert_eq!(group_room["detail"]["children"][0]["childIndex"], 2);
        assert_eq!(
            group_room["detail"]["results"][0]["childConversationId"],
            "child-conv-parser"
        );
        assert_eq!(
            group_room["detail"]["results"][1]["resultPreview"],
            "runtime summary"
        );

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn delegation_group_room_failed_summarizes_partial_results() {
        let dir =
            std::env::temp_dir().join(format!("synthchat-delegation-room-fail-{}", new_id("test")));
        fs::create_dir_all(&dir).unwrap();
        let store = AppStore::new(dir.join("state.json")).unwrap();
        let persona = store.persona(None).unwrap();
        let conversation = store
            .create_conversation(
                Some("delegation room fail".into()),
                Some(persona.id.clone()),
            )
            .unwrap();
        let parent = store
            .save_agent_run(AgentRunRecord::new(
                conversation.id,
                persona.id,
                "default".into(),
            ))
            .unwrap();
        let agent = AgentDefinition::default();
        let chat_config = ChatConfig::default();
        let requests = delegate_task_requests(&json!({
            "tasks": [
                {"task": "inspect parser", "role": "reader"},
                {"task": "summarize runtime", "role": "reviewer"}
            ]
        }))
        .unwrap();
        let partial_results = vec![
            json!({
                "status": "completed",
                "childRunId": "run-child-ok",
                "role": "reader",
                "task": "inspect parser",
                "result": "parser summary"
            }),
            json!({
                "status": "failed",
                "childRunId": "run-child-failed",
                "role": "reviewer",
                "task": "summarize runtime",
                "error": "child failed while summarizing runtime"
            }),
        ];

        record_parent_delegation_group_room_failed(
            &store,
            &parent.run_id,
            WorkflowMode::ChatTurn,
            &agent,
            &chat_config,
            0,
            0,
            true,
            &requests,
            &partial_results,
            Some("delegate_task batch failed"),
        )
        .unwrap();

        let saved = store.agent_run(&parent.run_id).unwrap();
        let graph = saved.workflow_graph.as_ref().unwrap();
        assert_eq!(graph["transitions"][0]["from"], "group_room");
        assert_eq!(graph["transitions"][0]["to"], "executor");
        assert_eq!(
            graph["transitions"][0]["reason"],
            WORKFLOW_REASON_DELEGATE_TASK_FAILED
        );
        let group_room = graph["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["node"] == "group_room")
            .unwrap();
        assert_eq!(group_room["status"], "failed");
        assert_eq!(group_room["detail"]["phase"], "failed");
        assert_eq!(group_room["detail"]["ok"], false);
        assert_eq!(group_room["detail"]["completedChildren"], 1);
        assert_eq!(group_room["detail"]["failedChildren"], 1);
        assert_eq!(
            group_room["detail"]["results"][0]["childRunId"],
            "run-child-ok"
        );
        assert_eq!(
            group_room["detail"]["results"][1]["errorPreview"],
            "child failed while summarizing runtime"
        );

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn acp_live_tool_event_maps_cancelled_to_completed_with_prefix() {
        let update = acp_tool_event_update_from_value(&json!({
            "toolName": "todo",
            "callId": "tc-todo",
            "status": "cancelled",
            "text": "Drop stale task",
            "raw": {
                "payload": {
                    "todos": [{"id": "old", "content": "Drop stale task", "status": "cancelled"}]
                }
            }
        }))
        .unwrap();

        assert_eq!(update["sessionUpdate"], "tool_call_update");
        assert_eq!(update["status"], "completed");
        assert_eq!(update["rawOutput"], "[cancelled] Drop stale task");
        assert_eq!(
            update["content"][0]["content"]["text"],
            "[cancelled] Drop stale task"
        );
    }

    #[test]
    fn acp_history_replay_maps_cancelled_tool_result_to_completed() {
        let messages = vec![
            ChatMessage {
                id: "assistant".into(),
                conversation_id: "conv".into(),
                role: "assistant".into(),
                content: json!({
                    "tool_calls": [{
                        "id": "tc-todo",
                        "type": "function",
                        "function": {
                            "name": "todo",
                            "arguments": {"todos": [{"id": "old", "content": "Drop stale task"}]}
                        }
                    }]
                })
                .to_string(),
                created_at: "2026-06-05T00:00:00Z".into(),
                source: "test".into(),
                account_id: None,
                provider_data: None,
            },
            ChatMessage {
                id: "tool".into(),
                conversation_id: "conv".into(),
                role: "tool".into(),
                content: json!({
                    "type": "toolEvent",
                    "event": {
                        "toolName": "todo",
                        "callId": "tc-todo",
                        "status": "cancelled",
                        "text": "Drop stale task"
                    }
                })
                .to_string(),
                created_at: "2026-06-05T00:00:01Z".into(),
                source: "test".into(),
                account_id: None,
                provider_data: None,
            },
        ];

        let updates = acp_session_history_updates(&messages);
        let completion = updates
            .iter()
            .find(|update| update["sessionUpdate"] == "tool_call_update")
            .unwrap();
        assert_eq!(completion["status"], "completed");
        assert_eq!(completion["rawOutput"], "[cancelled] Drop stale task");
    }

    #[test]
    fn acp_idle_steer_prompt_text_rewrites_guidance_only() {
        assert_eq!(
            acp_idle_steer_prompt_text("/steer prefer smaller steps"),
            Some("prefer smaller steps".into())
        );
        assert_eq!(
            acp_idle_steer_prompt_text("／steer use the queue state"),
            Some("use the queue state".into())
        );
        assert!(acp_idle_steer_prompt_text("/steering wheel").is_none());
        assert!(acp_idle_steer_prompt_text("/steer").is_none());
    }

    #[test]
    fn acp_queue_update_reflects_running_and_completed_states() {
        let dir = std::env::temp_dir().join(format!("synthchat-acp-queue-{}", new_id("test")));
        fs::create_dir_all(&dir).unwrap();
        let store = AppStore::new(dir.join("state.json")).unwrap();
        let persona = store.persona(None).unwrap();
        let conversation = store
            .create_conversation(Some("ACP Queue".into()), Some(persona.id.clone()))
            .unwrap();
        let message = ChatMessage::new(
            conversation.id.clone(),
            "user",
            "queued follow-up".into(),
            "test",
        );
        let item = store
            .enqueue_agent_request(conversation.id.clone(), persona.id.clone(), &message)
            .unwrap();
        let claimed = store
            .claim_next_agent_request(&conversation.id)
            .unwrap()
            .unwrap();

        let running =
            acp_queue_update_for_current_state(&store, &conversation.id, &claimed).unwrap();
        assert_eq!(running["params"]["update"]["sessionUpdate"], "queue_update");
        assert_eq!(running["params"]["update"]["queueId"], item.id);
        assert_eq!(running["params"]["update"]["status"], "running");
        assert_eq!(running["params"]["update"]["pendingCount"], 0);

        let completed = store
            .complete_agent_queue_item(&item.id, "completed", None)
            .unwrap()
            .unwrap();
        let terminal =
            acp_queue_update_for_current_state(&store, &conversation.id, &completed).unwrap();
        assert_eq!(terminal["params"]["update"]["status"], "completed");
        assert_eq!(
            terminal["params"]["update"]["content"]["text"],
            "queued follow-up"
        );

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn acp_terminal_setup_auth_method_matches_acp_terminal_shape() {
        let method = acp_terminal_setup_auth_method();
        assert_eq!(method["id"], ACP_TERMINAL_SETUP_AUTH_METHOD_ID);
        assert_eq!(method["type"], "terminal");
        assert_eq!(method["args"], json!(["--setup"]));
        assert!(method["name"]
            .as_str()
            .unwrap()
            .contains("Configure SynthChat provider"));
    }

    #[test]
    fn acp_setup_authenticate_requires_provider_method_after_setup() {
        let setup = acp_terminal_setup_auth_method();
        assert_eq!(
            acp_authenticate_result_from_methods(ACP_TERMINAL_SETUP_AUTH_METHOD_ID, &[setup]),
            Value::Null
        );

        let setup = acp_terminal_setup_auth_method();
        let provider = json!({"id": "openrouter", "name": "openrouter runtime credentials"});
        assert_eq!(
            acp_authenticate_result_from_methods(
                ACP_TERMINAL_SETUP_AUTH_METHOD_ID,
                &[provider, setup]
            ),
            json!({})
        );
    }
}
