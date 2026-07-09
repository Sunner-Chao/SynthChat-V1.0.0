use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use serde_json::{json, Value};
use tauri::AppHandle;

use crate::{
    error::{AppError, AppResult},
    mcp,
    models::{ChatMessage, McpCallResult, ToolApprovalRequest, ToolDefinition},
    store::AppStore,
};

use super::decision_parser::{strip_provider_tool_call_metadata, APPROVED_TOOL_CALL_REPLAY_KEY};
use super::*;

pub async fn call_mcp_tool_with_retry(
    store: &AppStore,
    server_id: String,
    tool_name: String,
    payload: Value,
    timeout_seconds: Option<u64>,
    run_id: Option<&str>,
    retry_count: usize,
    retry_backoff_ms: usize,
) -> AppResult<McpCallResult> {
    let mut last = None;
    for attempt in 0..=retry_count {
        match mcp::call_tool(
            store,
            server_id.clone(),
            tool_name.clone(),
            payload.clone(),
            timeout_seconds,
            run_id,
        )
        .await
        {
            Ok(result) if result.ok || attempt == retry_count => return Ok(result),
            Ok(result)
                if result
                    .error
                    .as_deref()
                    .is_some_and(mcp::mcp_error_needs_reauth) =>
            {
                return Ok(result);
            }
            Ok(result) => last = Some(result),
            Err(error) if attempt == retry_count => return Err(error),
            Err(error) => {
                if mcp::mcp_error_needs_reauth(&error.to_string()) {
                    return Ok(McpCallResult {
                        ok: false,
                        timed_out: false,
                        elapsed_ms: 0,
                        stdout: String::new(),
                        stderr: error.to_string(),
                        error: Some(error.to_string()),
                    });
                }
                last = Some(McpCallResult {
                    ok: false,
                    timed_out: false,
                    elapsed_ms: 0,
                    stdout: String::new(),
                    stderr: error.to_string(),
                    error: Some(error.to_string()),
                });
            }
        }
        tokio::time::sleep(Duration::from_millis(retry_backoff_ms as u64)).await;
    }
    Ok(last.unwrap_or(McpCallResult {
        ok: false,
        timed_out: false,
        elapsed_ms: 0,
        stdout: String::new(),
        stderr: String::new(),
        error: Some("tool call retry loop ended without a result".into()),
    }))
}

async fn continue_agent_run_after_approval(
    store: &AppStore,
    approval: &ToolApprovalRequest,
    app: Option<&AppHandle>,
) -> AppResult<()> {
    let run_id = approval
        .run_id
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("approved tool call missing runId".into()))?;
    let mut run = store.agent_run(run_id)?;
    // Guard: do not revive a run that was aborted or failed while the user
    // was reviewing the approval dialog. Proceeding would silently re-execute
    // a tool inside a run the user (or system) already terminated.
    let terminal_states = ["completed", "failed", "aborted"];
    if terminal_states.contains(&run.state.as_str()) {
        return Err(AppError::BadRequest(format!(
            "cannot resume run {run_id}: run is already in terminal state '{}'",
            run.state
        )));
    }
    let conversation = store.conversation(&run.conversation_id)?;
    let persona = store.persona(Some(&run.persona_id))?;
    let mut agent = store.agent(Some(&run.agent_id))?;
    apply_persona_tool_policy_to_agent(&persona, &mut agent);
    let effective_persona = effective_llm_persona(&persona, &agent);
    let providers = store.provider_candidates(selected_provider_id(&persona, &agent))?;
    let mut history = store.messages(&run.conversation_id, Some(30))?;
    let skill_blocks = crate::skills::prompt_blocks_for_request(store, &agent, &run.user_request)?;
    let memory_blocks = memory_prompt_blocks(store, &persona)?;
    let short_context = store.short_context(&run.conversation_id)?;
    let mcp_tools = available_mcp_tool_definitions(store, &agent)?;
    let visible_tools =
        visible_tool_definitions_for_agent(store, &agent, ToolExecutionContext::Interactive)?;
    let available_tools_for_validation = visible_tools.clone();
    let prompt_mcp_tools = visible_tools
        .iter()
        .filter(|tool| tool.source != "internal")
        .cloned()
        .collect::<Vec<_>>();
    let mut observations = vec![approved_tool_observation(approval)];
    let mut assistant_text = String::new();
    let mut assistant_provider_data: Option<Value> = None;
    let mut assistant_model: Option<String> = None;
    let mut assistant_provider_id: Option<String> = None;
    let mut reviewer_skip_reason: Option<&'static str> = None;
    let mut planner_recovery_exhausted: Option<&'static str> = None;

    let workflow_driver = WorkflowDriver::new(WorkflowMode::ApprovalContinuation);
    let workflow_approval = workflow_driver.approval();
    let workflow_planner = workflow_driver.planner();
    let executor_core = ExecutorCore::new(workflow_driver.executor());
    let workflow_reviewer = workflow_driver.reviewer();
    workflow_approval.resumed(
        store,
        &run.run_id,
        &approval.server_id,
        &approval.tool_name,
        Some(approval.id.as_str()),
        Some(approval.status.as_str()),
        Some(approval.reason.as_str()),
    )?;

    run.state = "running".into();
    run.completed_at = None;
    run.updated_at = now_iso();
    let saved_run = store.save_agent_run(run.clone())?;
    emit_agent_run_record(app, &saved_run, None);

    let run_started_at = Instant::now();
    let chat_config = store.config()?.chat;
    let run_timeout_seconds = chat_config.agent_run_timeout_seconds;
    let post_tool_quiet_timeout_seconds = chat_config.agent_post_tool_quiet_timeout_seconds;
    let mut tool_guardrails = ToolLoopGuardrails::new(&chat_config);
    let mut failed_file_mutations: HashMap<String, String> = HashMap::new();
    let mut empty_response_recoveries: HashMap<String, u32> = HashMap::new();
    let mut iteration_budget = IterationBudget::new(agent.max_tool_iterations.max(1).min(90));
    for iteration in 0..iteration_budget.max_total() {
        if !iteration_budget.consume() {
            append_parent_phase_event(
                store,
                &run.run_id,
                "iteration_budget_exhausted",
                json!({
                    "used": iteration_budget.used(),
                    "maxTotal": iteration_budget.max_total(),
                    "remaining": iteration_budget.remaining(),
                    "continuation": "approval",
                }),
            )?;
            break;
        }
        if check_agent_run_interrupted(
            store,
            &run.run_id,
            run_started_at,
            run_timeout_seconds,
            post_tool_quiet_timeout_seconds,
            app,
        )? {
            return Ok(());
        }
        workflow_planner.running(store, &run.run_id, iteration + 1)?;
        let prompt_observations = observations_for_prompt(store, &run.run_id, &observations)?;
        let planner_prompt = agent_planner_prompt_for_agent_context_with_store(
            store,
            &prompt_observations,
            &skill_blocks,
            &memory_blocks,
            &short_context,
            &prompt_mcp_tools,
            ToolExecutionContext::Interactive,
            &agent,
            Some(&effective_persona),
        );
        let pre_llm_contexts = run_pre_llm_call_hooks(store, &run.run_id, &run.user_request).await;
        let llm_user_content = inject_pre_llm_hook_context(&run.user_request, &pre_llm_contexts);
        let reply = match complete_chat_with_provider_failover(
            store,
            Some(&run.run_id),
            &providers,
            &effective_persona,
            planner_prompt.clone(),
            history.clone(),
            &llm_user_content,
            Some(&visible_tools),
            None,
        )
        .await
        {
            Ok(reply) => reply,
            Err(error) => {
                if check_agent_run_interrupted(
                    store,
                    &run.run_id,
                    run_started_at,
                    run_timeout_seconds,
                    post_tool_quiet_timeout_seconds,
                    app,
                )? {
                    return Ok(());
                }
                let mut failed_run = store.agent_run(&run.run_id)?;
                let mut saved_failed_run = None;
                if failed_run.state != "aborted" {
                    workflow_planner.failed(
                        store,
                        &run.run_id,
                        Some(iteration + 1),
                        WorkflowPlannerErrorKind::LlmError,
                        &error.to_string(),
                    )?;
                    failed_run = store.agent_run(&run.run_id)?;
                    failed_run.state = "failed".into();
                    failed_run.error = Some(error.to_string());
                    failed_run.updated_at = now_iso();
                    failed_run.completed_at = Some(failed_run.updated_at.clone());
                    saved_failed_run = Some(store.save_agent_run(failed_run)?);
                }
                let assistant = store.append_message(ChatMessage::new(
                    run.conversation_id.clone(),
                    "assistant",
                    format!("审批后的继续执行没有返回，是因为模型请求失败：{error}"),
                    "desktop-agent-error",
                ))?;
                if let Some(saved_failed_run) =
                    saved_failed_run.or_else(|| store.agent_run(&run.run_id).ok())
                {
                    emit_agent_run_record(app, &saved_failed_run, Some(&assistant));
                }
                return Ok(());
            }
        };
        if abort_agent_run_for_turn_aborted_marker(store, &run.run_id, &reply.content, app)? {
            return Ok(());
        }
        if check_agent_run_interrupted(
            store,
            &run.run_id,
            run_started_at,
            run_timeout_seconds,
            post_tool_quiet_timeout_seconds,
            app,
        )? {
            return Ok(());
        }
        if reply.finish_reason.as_deref() == Some("incomplete") {
            let recovery_key = "incomplete_response";
            let attempt = {
                let count = empty_response_recoveries
                    .entry(recovery_key.into())
                    .and_modify(|count| *count += 1)
                    .or_insert(1);
                *count
            };
            if attempt == 1 {
                let note = "Provider returned an incomplete Responses continuation (reasoning/commentary without final answer, or unfinished output item). Continue from the approved tool observations and return a valid planner JSON object: either {\"action\":\"tool\",...} or {\"action\":\"final\",\"content\":\"...\"}.";
                observations.push(format!(
                    "Continuation iteration {} LLM recovery: {}",
                    iteration + 1,
                    note
                ));
                append_parent_phase_event(
                    store,
                    &run.run_id,
                    "llm_recovery",
                    json!({
                        "kind": recovery_key,
                        "note": note,
                        "attempt": attempt,
                        "maxAttempts": 1,
                        "continuation": "approval",
                        "finishReason": reply.finish_reason.clone(),
                        "providerId": reply.provider_id.clone(),
                        "model": reply.model.clone(),
                    }),
                )?;
                continue;
            } else {
                planner_recovery_exhausted = Some(recovery_key);
                append_parent_phase_event(
                    store,
                    &run.run_id,
                    "llm_recovery_exhausted",
                    json!({
                        "kind": recovery_key,
                        "attempts": empty_response_recoveries.clone(),
                        "continuation": "approval",
                        "finishReason": reply.finish_reason.clone(),
                        "providerId": reply.provider_id.clone(),
                        "model": reply.model.clone(),
                    }),
                )?;
            }
        }
        // Guard: only run empty-response recovery when the provider did NOT signal
        // an incomplete turn — same fix as agent_loop.rs (BUG-7).
        if reply.content.trim().is_empty()
            && reply.finish_reason.as_deref() != Some("incomplete")
        {
            if let Some(recovery) =
                next_empty_llm_response_recovery(&observations, &mut empty_response_recoveries)
            {
                observations.push(format!(
                    "Continuation iteration {} LLM recovery: {}",
                    iteration + 1,
                    recovery.note
                ));
                append_parent_phase_event(
                    store,
                    &run.run_id,
                    "llm_recovery",
                    json!({
                        "kind": recovery.kind,
                        "note": recovery.note,
                        "attempt": recovery.attempt,
                        "maxAttempts": recovery.max_attempts,
                        "afterTools": recovery.after_tools,
                        "continuation": "approval",
                        "finishReason": reply.finish_reason.clone(),
                        "providerId": reply.provider_id.clone(),
                        "model": reply.model.clone(),
                    }),
                )?;
                continue;
            } else {
                planner_recovery_exhausted = Some(if observations.is_empty() {
                    "empty_response"
                } else {
                    "empty_response_after_tools"
                });
                append_parent_phase_event(
                    store,
                    &run.run_id,
                    "llm_recovery_exhausted",
                    json!({
                        "kind": if observations.is_empty() { "empty_response" } else { "empty_response_after_tools" },
                        "attempts": empty_response_recoveries.clone(),
                        "continuation": "approval",
                        "finishReason": reply.finish_reason.clone(),
                        "providerId": reply.provider_id.clone(),
                        "model": reply.model.clone(),
                    }),
                )?;
            }
        }
        let decision = parse_agent_decision(&reply.content);
        append_planner_trace(
            store,
            &run.run_id,
            &run.conversation_id,
            &persona.id,
            &agent.id,
            iteration + 1,
            &planner_prompt,
            &reply.content,
            &decision,
        )?;

        match workflow_planner.route(
            store,
            &run.run_id,
            iteration + 1,
            &decision,
            reply.content.trim(),
            &available_tools_for_validation,
        )? {
            WorkflowPlannerRoute::ExecuteTools {
                requests,
                request_count,
            } => {
                let refund_iteration_for_execute_code_only =
                    tool_batch_is_execute_code_only(&requests);
                for (tool_name, payload) in requests {
                    let guardrail_payload = payload.clone();
                    if let Some(outcome) =
                        tool_guardrails.before_call(&tool_name, &guardrail_payload)
                    {
                        let guardrail_message = outcome.message.clone();
                        observations.push(format!(
                            "Continuation iteration {} tool {} guardrail: {}",
                            iteration + 1,
                            tool_name,
                            guardrail_message
                        ));
                        if outcome.halt {
                            executor_core.record_tool_failed_with_iteration(
                                store,
                                app,
                                &run.conversation_id,
                                &run.run_id,
                                Some(iteration + 1),
                                &tool_name,
                                &mcp_tools,
                                &guardrail_payload,
                                &AppError::BadRequest(guardrail_message.clone()),
                            )?;
                            assistant_text = guardrail_message;
                            break;
                        }
                    }
                    let run_id_for_resolution = run.run_id.clone();
                    let tool_resolution = executor_core.resolve_tool(
                        store,
                        &run_id_for_resolution,
                        iteration + 1,
                        &tool_name,
                        &mcp_tools,
                    )?;
                    run = store.agent_run(&run_id_for_resolution)?;
                    match tool_resolution {
                        WorkflowExecutorToolResolution::Internal(tool_identity) => {
                            let approval_reason = match executor_core
                                .resolve_approval_policy(
                                    store,
                                    ExecutorApprovalPolicyContext::approval_continuation(
                                        &run.run_id,
                                        &providers,
                                        &effective_persona,
                                    ),
                                    iteration + 1,
                                    &tool_identity,
                                    &tool_name,
                                    &payload,
                                    is_risky_tool_call(&tool_name, &payload),
                                )
                                .await
                            {
                                Ok(reason) => reason,
                                Err(error) => {
                                    executor_core.record_tool_failed_with_iteration(
                                        store,
                                        app,
                                        &run.conversation_id,
                                        &run.run_id,
                                        Some(iteration + 1),
                                        &tool_name,
                                        &mcp_tools,
                                        &guardrail_payload,
                                        &error,
                                    )?;
                                    observations.push(format!(
                                        "Continuation iteration {} tool {} approval error: {}",
                                        iteration + 1,
                                        tool_name,
                                        error
                                    ));
                                    continue;
                                }
                            };
                            if let Some(reason) = approval_reason {
                                let approval_request = executor_core
                                    .request_approval(
                                        store,
                                        ExecutorApprovalRequestContext {
                                            conversation_id: &run.conversation_id,
                                            persona_id: &persona.id,
                                            agent_id: &agent.id,
                                            run_id: &run.run_id,
                                            tool_context: ToolExecutionContext::Interactive,
                                        },
                                        iteration + 1,
                                        &tool_identity,
                                        payload,
                                        reason,
                                    )
                                    .await?;
                                let approval_route = approval_request.route;
                                debug_assert!(matches!(
                                    approval_route,
                                    WorkflowExecutorRoute::AwaitApproval { .. }
                                ));
                                mark_run_pending_approval(store, app, &run.run_id)?;
                                append_waiting_for_approval_message(
                                    store,
                                    &run.conversation_id,
                                    tool_identity.server_id(),
                                    tool_identity.tool_name(),
                                )?;
                                return Ok(());
                            }
                            let run_id_for_start = run.run_id.clone();
                            run = executor_core.start_tool_execution(
                                store,
                                app,
                                &run_id_for_start,
                                &tool_identity,
                                &payload,
                                iteration + 1,
                            )?;
                            match executor_core
                                .execute_internal_tool(
                                    store,
                                    ExecutorInternalToolExecutionContext {
                                        agent: &agent,
                                        conversation_id: &run.conversation_id,
                                        run_id: &run.run_id,
                                        tool_context: ToolExecutionContext::Interactive,
                                        app,
                                        approved_tool_call_replay: false,
                                    },
                                    &tool_name,
                                    payload,
                                )
                                .await
                            {
                                Ok((text, mut event)) => {
                                    record_file_mutation_result(
                                        &mut failed_file_mutations,
                                        &tool_name,
                                        &guardrail_payload,
                                        &text,
                                        false,
                                    );
                                    if check_agent_run_interrupted(
                                        store,
                                        &run.run_id,
                                        run_started_at,
                                        run_timeout_seconds,
                                        post_tool_quiet_timeout_seconds,
                                        app,
                                    )? {
                                        return Ok(());
                                    }
                                    let context_text = persist_large_tool_result_for_context(
                                        store,
                                        &run.run_id,
                                        &tool_name,
                                        &text,
                                        &mut event,
                                    )?;
                                    let observation_text = append_subdirectory_hints_to_tool_result(
                                        &agent,
                                        &tool_name,
                                        &guardrail_payload,
                                        &context_text,
                                    );
                                    observations.push(tool_result_replay_observation(
                                        iteration + 1,
                                        &tool_name,
                                        &tool_name,
                                        &observation_text,
                                    ));
                                    if let Some(outcome) = tool_guardrails.after_call(
                                        &tool_name,
                                        &guardrail_payload,
                                        &context_text,
                                        false,
                                    ) {
                                        observations.push(format!(
                                            "Continuation iteration {} tool {} guardrail: {}",
                                            iteration + 1,
                                            tool_name,
                                            outcome.message
                                        ));
                                        if outcome.halt {
                                            assistant_text = outcome.message.clone();
                                            break;
                                        }
                                    }
                                    let run_id_for_event = run.run_id.clone();
                                    let conversation_id_for_event = run.conversation_id.clone();
                                    run = executor_core.record_tool_event(
                                        store,
                                        app,
                                        &conversation_id_for_event,
                                        &run_id_for_event,
                                        event.clone(),
                                    )?;
                                    let mut tool_message = ChatMessage::new(
                                        run.conversation_id.clone(),
                                        "tool",
                                        json!({"type": "toolEvent", "event": event.clone()})
                                            .to_string(),
                                        "desktop-agent-tool",
                                    );
                                    tool_message.provider_data = reply.provider_data.clone();
                                    let tool_message = store.append_message(tool_message)?;
                                    history.push(tool_message);
                                    if pause_run_for_clarify_tool(
                                        store,
                                        app,
                                        &mut run,
                                        &conversation.id,
                                        WorkflowMode::ApprovalContinuation,
                                        &text,
                                        &event,
                                    )?
                                    .is_some()
                                    {
                                        return Ok(());
                                    }
                                }
                                Err(error) => {
                                    executor_core.record_tool_failed_with_iteration(
                                        store,
                                        app,
                                        &run.conversation_id,
                                        &run.run_id,
                                        Some(iteration + 1),
                                        &tool_name,
                                        &mcp_tools,
                                        &guardrail_payload,
                                        &error,
                                    )?;
                                    record_file_mutation_result(
                                        &mut failed_file_mutations,
                                        &tool_name,
                                        &guardrail_payload,
                                        &error.to_string(),
                                        true,
                                    );
                                    observations.push(format!(
                                        "Continuation iteration {} tool {} error: {}",
                                        iteration + 1,
                                        tool_name,
                                        error
                                    ));
                                    if let Some(outcome) = tool_guardrails.after_call(
                                        &tool_name,
                                        &guardrail_payload,
                                        &error.to_string(),
                                        true,
                                    ) {
                                        observations.push(format!(
                                            "Continuation iteration {} tool {} guardrail: {}",
                                            iteration + 1,
                                            tool_name,
                                            outcome.message
                                        ));
                                        if outcome.halt {
                                            assistant_text = outcome.message.clone();
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                        WorkflowExecutorToolResolution::Mcp {
                            identity: tool_identity,
                            definition,
                        } => {
                            let approval_reason = match executor_core
                                .resolve_approval_policy(
                                    store,
                                    ExecutorApprovalPolicyContext::approval_continuation(
                                        &run.run_id,
                                        &providers,
                                        &effective_persona,
                                    ),
                                    iteration + 1,
                                    &tool_identity,
                                    &tool_name,
                                    &payload,
                                    definition.requires_approval,
                                )
                                .await
                            {
                                Ok(reason) => reason,
                                Err(error) => {
                                    executor_core.record_tool_failed_with_iteration(
                                        store,
                                        app,
                                        &run.conversation_id,
                                        &run.run_id,
                                        Some(iteration + 1),
                                        &tool_name,
                                        &mcp_tools,
                                        &guardrail_payload,
                                        &error,
                                    )?;
                                    observations.push(format!(
                                        "Continuation iteration {} tool {} approval error: {}",
                                        iteration + 1,
                                        tool_name,
                                        error
                                    ));
                                    continue;
                                }
                            };
                            if let Some(reason) = approval_reason {
                                let approval_request = executor_core
                                    .request_approval(
                                        store,
                                        ExecutorApprovalRequestContext {
                                            conversation_id: &run.conversation_id,
                                            persona_id: &persona.id,
                                            agent_id: &agent.id,
                                            run_id: &run.run_id,
                                            tool_context: ToolExecutionContext::Interactive,
                                        },
                                        iteration + 1,
                                        &tool_identity,
                                        payload,
                                        reason,
                                    )
                                    .await?;
                                let approval_route = approval_request.route;
                                debug_assert!(matches!(
                                    approval_route,
                                    WorkflowExecutorRoute::AwaitApproval { .. }
                                ));
                                mark_run_pending_approval(store, app, &run.run_id)?;
                                append_waiting_for_approval_message(
                                    store,
                                    &run.conversation_id,
                                    tool_identity.server_id(),
                                    tool_identity.tool_name(),
                                )?;
                                return Ok(());
                            }
                            let run_id_for_start = run.run_id.clone();
                            run = executor_core.start_tool_execution(
                                store,
                                app,
                                &run_id_for_start,
                                &tool_identity,
                                &payload,
                                iteration + 1,
                            )?;
                            match executor_core
                                .execute_mcp_tool(store, &run.run_id, &definition, payload, None)
                                .await
                            {
                                Ok((text, mut event)) => {
                                    record_file_mutation_result(
                                        &mut failed_file_mutations,
                                        &tool_name,
                                        &guardrail_payload,
                                        &text,
                                        false,
                                    );
                                    if check_agent_run_interrupted(
                                        store,
                                        &run.run_id,
                                        run_started_at,
                                        run_timeout_seconds,
                                        post_tool_quiet_timeout_seconds,
                                        app,
                                    )? {
                                        return Ok(());
                                    }
                                    let context_text = persist_large_tool_result_for_context(
                                        store,
                                        &run.run_id,
                                        &tool_name,
                                        &text,
                                        &mut event,
                                    )?;
                                    let tool_source = tool_identity.source_label();
                                    let observation_text = append_subdirectory_hints_to_tool_result(
                                        &agent,
                                        &tool_name,
                                        &guardrail_payload,
                                        &context_text,
                                    );
                                    observations.push(tool_result_replay_observation(
                                        iteration + 1,
                                        &tool_name,
                                        &tool_source,
                                        &observation_text,
                                    ));
                                    if let Some(outcome) = tool_guardrails.after_call(
                                        &tool_name,
                                        &guardrail_payload,
                                        &context_text,
                                        false,
                                    ) {
                                        observations.push(format!(
                                            "Continuation iteration {} tool {} guardrail: {}",
                                            iteration + 1,
                                            tool_name,
                                            outcome.message
                                        ));
                                        if outcome.halt {
                                            assistant_text = outcome.message.clone();
                                            break;
                                        }
                                    }
                                    let run_id_for_event = run.run_id.clone();
                                    let conversation_id_for_event = run.conversation_id.clone();
                                    run = executor_core.record_tool_event(
                                        store,
                                        app,
                                        &conversation_id_for_event,
                                        &run_id_for_event,
                                        event.clone(),
                                    )?;
                                    let mut tool_message = ChatMessage::new(
                                        run.conversation_id.clone(),
                                        "tool",
                                        json!({"type": "toolEvent", "event": event}).to_string(),
                                        "desktop-agent-tool",
                                    );
                                    tool_message.provider_data = reply.provider_data.clone();
                                    let tool_message = store.append_message(tool_message)?;
                                    history.push(tool_message);
                                }
                                Err(error) => {
                                    executor_core.record_tool_failed_with_iteration(
                                        store,
                                        app,
                                        &run.conversation_id,
                                        &run.run_id,
                                        Some(iteration + 1),
                                        &tool_name,
                                        &mcp_tools,
                                        &guardrail_payload,
                                        &error,
                                    )?;
                                    record_file_mutation_result(
                                        &mut failed_file_mutations,
                                        &tool_name,
                                        &guardrail_payload,
                                        &error.to_string(),
                                        true,
                                    );
                                    observations.push(format!(
                                        "Continuation iteration {} tool {} error: {}",
                                        iteration + 1,
                                        tool_name,
                                        error
                                    ));
                                    if let Some(outcome) = tool_guardrails.after_call(
                                        &tool_name,
                                        &guardrail_payload,
                                        &error.to_string(),
                                        true,
                                    ) {
                                        observations.push(format!(
                                            "Continuation iteration {} tool {} guardrail: {}",
                                            iteration + 1,
                                            tool_name,
                                            outcome.message
                                        ));
                                        if outcome.halt {
                                            assistant_text = outcome.message.clone();
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                        WorkflowExecutorToolResolution::Unavailable { requested_name } => {
                            let error = AppError::BadRequest(format!(
                                "tool is not available: {requested_name}"
                            ));
                            executor_core.record_tool_failed_with_iteration(
                                store,
                                app,
                                &run.conversation_id,
                                &run.run_id,
                                Some(iteration + 1),
                                &tool_name,
                                &mcp_tools,
                                &guardrail_payload,
                                &error,
                            )?;
                            record_file_mutation_result(
                                &mut failed_file_mutations,
                                &tool_name,
                                &guardrail_payload,
                                &error.to_string(),
                                true,
                            );
                            observations.push(format!(
                                "Continuation iteration {} tool {} error: {}",
                                iteration + 1,
                                tool_name,
                                error
                            ));
                            if let Some(outcome) = tool_guardrails.after_call(
                                &tool_name,
                                &guardrail_payload,
                                &error.to_string(),
                                true,
                            ) {
                                observations.push(format!(
                                    "Continuation iteration {} tool {} guardrail: {}",
                                    iteration + 1,
                                    tool_name,
                                    outcome.message
                                ));
                                if outcome.halt {
                                    assistant_text = outcome.message.clone();
                                    break;
                                }
                            }
                        }
                    }
                    if !assistant_text.trim().is_empty() {
                        break;
                    }
                }
                if !assistant_text.trim().is_empty() {
                    break;
                }
                let executor_route = executor_core.continue_planning(
                    store,
                    &run.run_id,
                    iteration + 1,
                    request_count,
                    None,
                )?;
                debug_assert!(matches!(
                    executor_route,
                    WorkflowExecutorRoute::ContinuePlanning { .. }
                ));
                if refund_iteration_for_execute_code_only {
                    iteration_budget.refund();
                }
                continue;
            }
            WorkflowPlannerRoute::ReviewFinal { content } => {
                assistant_text = content;
                assistant_provider_data = reply.provider_data.clone();
                assistant_model = reply.model.clone();
                assistant_provider_id = reply.provider_id.clone();
                break;
            }
            WorkflowPlannerRoute::Recover { observation } => {
                observations.push(observation);
                continue;
            }
        }
    }

    if assistant_text.trim().is_empty() {
        if iteration_budget.exhausted() {
            append_parent_phase_event(
                store,
                &run.run_id,
                "iteration_budget_exhausted",
                json!({
                    "used": iteration_budget.used(),
                    "maxTotal": iteration_budget.max_total(),
                    "remaining": iteration_budget.remaining(),
                    "continuation": "approval",
                }),
            )?;
        }
        let (planner_error_kind, planner_error) = if iteration_budget.exhausted() {
            (
                WorkflowPlannerErrorKind::IterationBudgetExhausted,
                format!(
                    "Approval continuation iteration budget exhausted before a final answer ({}/{}).",
                    iteration_budget.used(),
                    iteration_budget.max_total()
                ),
            )
        } else if let Some(kind) = planner_recovery_exhausted {
            (
                WorkflowPlannerErrorKind::LlmRecoveryExhausted,
                format!(
                    "Approval continuation LLM recovery exhausted before a final answer: {kind}."
                ),
            )
        } else {
            (
                WorkflowPlannerErrorKind::NoFinalAnswer,
                "Approval continuation ended without a final answer.".to_string(),
            )
        };
        workflow_planner.failed(store, &run.run_id, None, planner_error_kind, &planner_error)?;
        if iteration_budget.exhausted() {
            reviewer_skip_reason = Some("iteration_budget_exhausted");
            assistant_text = format!(
                "审批后的继续执行已达到 agent 迭代预算（{}/{}），当前没有得到最终回答。\n\n{}",
                iteration_budget.used(),
                iteration_budget.max_total(),
                observations.join("\n\n")
            );
        } else {
            reviewer_skip_reason = Some("no_final_answer");
            assistant_text = format!(
                "审批后的工具调用已执行，但当前恢复版 agent loop 未得到最终回答。\n\n{}",
                observations.join("\n\n")
            );
        }
    }
    normalize_guardrail_halt_reply(&mut assistant_text, &observations);
    append_file_mutation_footer(&mut assistant_text, &failed_file_mutations);
    assistant_text = sanitize_visible_assistant_reply(&assistant_text);
    assistant_text = run_transform_llm_output_hooks(
        store,
        &run.run_id,
        &run.user_request,
        &assistant_text,
        assistant_model.as_deref(),
        assistant_provider_id.as_deref(),
    )
    .await;
    run_post_llm_call_hooks(
        store,
        &run.run_id,
        &run.user_request,
        &assistant_text,
        assistant_model.as_deref(),
        assistant_provider_id.as_deref(),
    )
    .await;
    if check_agent_run_interrupted(
        store,
        &run.run_id,
        run_started_at,
        run_timeout_seconds,
        post_tool_quiet_timeout_seconds,
        app,
    )? {
        return Ok(());
    }
    let mut assistant_message = ChatMessage::new(
        conversation.id,
        "assistant",
        assistant_text,
        "desktop-agent",
    );
    assistant_message.provider_data = assistant_provider_data;
    let assistant = store.append_message(assistant_message)?;
    let mut final_run = store.agent_run(&run.run_id)?;
    final_run.state = "completed".into();
    final_run.updated_at = now_iso();
    final_run.completed_at = Some(final_run.updated_at.clone());
    let mut saved_final_run = store.save_agent_run(final_run)?;
    let reviewer_route = if let Some(reason) = reviewer_skip_reason {
        workflow_reviewer.skipped(
            store,
            &saved_final_run.run_id,
            &assistant.id,
            reason,
            assistant_model.as_deref(),
            assistant_provider_id.as_deref(),
        )?
    } else {
        workflow_reviewer.completed(
            store,
            &saved_final_run.run_id,
            &assistant.id,
            assistant_model.as_deref(),
            assistant_provider_id.as_deref(),
        )?
    };
    debug_assert!(matches!(
        reviewer_route,
        WorkflowReviewerRoute::Completed { .. } | WorkflowReviewerRoute::Skipped { .. }
    ));
    saved_final_run = store.agent_run(&saved_final_run.run_id)?;
    run_session_finished_hooks(
        store,
        &saved_final_run,
        json!({"source": "approval_continuation"}),
    )
    .await;
    emit_agent_run_record(app, &saved_final_run, Some(&assistant));
    Ok(())
}

pub(super) fn approved_tool_observation(approval: &ToolApprovalRequest) -> String {
    let result = approval.result.as_ref();
    let stdout = result
        .and_then(|value| value.get("stdout"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let stderr = result
        .and_then(|value| value.get("stderr"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let error = result
        .and_then(|value| value.get("error"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let text = if !stdout.trim().is_empty() {
        stdout
    } else if !stderr.trim().is_empty() {
        stderr
    } else {
        error
    };
    format!(
        "Approved tool {}.{} result:\n{}",
        approval.server_id, approval.tool_name, text
    )
}

fn mark_run_pending_approval(
    store: &AppStore,
    app: Option<&AppHandle>,
    run_id: &str,
) -> AppResult<()> {
    let mut run = store.agent_run(run_id)?;
    run.state = "pendingApproval".into();
    run.updated_at = now_iso();
    let saved_run = store.save_agent_run(run)?;
    emit_agent_run_record(app, &saved_run, None);
    Ok(())
}

fn append_waiting_for_approval_message(
    store: &AppStore,
    conversation_id: &str,
    server_id: &str,
    tool_name: &str,
) -> AppResult<ChatMessage> {
    store.append_message(ChatMessage::new(
        conversation_id.to_string(),
        "assistant",
        format!("下一步工具调用正在等待审批：{} · {}", server_id, tool_name),
        "desktop-agent",
    ))
}

fn approved_tool_execution_iteration(store: &AppStore, approval: &ToolApprovalRequest) -> u32 {
    approval
        .run_id
        .as_deref()
        .and_then(|run_id| store.agent_run(run_id).ok())
        .and_then(|run| workflow_graph_approval_iteration(run.workflow_graph.as_ref(), approval))
        .unwrap_or(1)
}

pub(super) fn workflow_graph_approval_iteration(
    graph: Option<&Value>,
    approval: &ToolApprovalRequest,
) -> Option<u32> {
    let graph = graph?;
    workflow_graph_matching_approval_iteration(graph, approval, workflow_approval_detail_id_matches)
        .or_else(|| {
            workflow_graph_matching_approval_iteration(
                graph,
                approval,
                workflow_approval_detail_target_matches,
            )
        })
}

fn workflow_graph_matching_approval_iteration(
    graph: &Value,
    approval: &ToolApprovalRequest,
    matches: fn(&Value, &ToolApprovalRequest) -> bool,
) -> Option<u32> {
    for collection_key in ["transitions", "nodes"] {
        let Some(items) = graph.get(collection_key).and_then(Value::as_array) else {
            continue;
        };
        for item in items.iter().rev() {
            let Some(detail) = item.get("detail") else {
                continue;
            };
            if matches(detail, approval) {
                if let Some(iteration) = workflow_detail_iteration(detail) {
                    return Some(iteration);
                }
            }
        }
    }
    None
}

fn workflow_approval_detail_id_matches(detail: &Value, approval: &ToolApprovalRequest) -> bool {
    workflow_detail_string(detail, &["approvalId", "approval_id"]) == Some(approval.id.as_str())
}

fn workflow_approval_detail_target_matches(detail: &Value, approval: &ToolApprovalRequest) -> bool {
    workflow_detail_string(detail, &["serverId", "server_id"]) == Some(approval.server_id.as_str())
        && workflow_detail_string(detail, &["toolName", "tool_name"])
            == Some(approval.tool_name.as_str())
}

fn workflow_detail_string<'a>(detail: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| detail.get(*key).and_then(Value::as_str))
}

fn workflow_detail_iteration(detail: &Value) -> Option<u32> {
    let iteration = detail.get("iteration").and_then(Value::as_u64)?;
    (iteration <= u32::MAX as u64).then_some(iteration as u32)
}

pub async fn approve_tool_call_and_resume(
    store: &AppStore,
    approval_id: String,
    timeout_seconds: Option<u64>,
    app: Option<&AppHandle>,
) -> AppResult<ToolApprovalRequest> {
    let approval = approve_tool_call_common(store, approval_id, timeout_seconds, app).await?;
    if approval.status == "approved" {
        continue_agent_run_after_approval(store, &approval, app).await?;
    }
    Ok(approval)
}

pub async fn approve_tool_call_always_and_resume(
    store: &AppStore,
    approval_id: String,
    timeout_seconds: Option<u64>,
    app: Option<&AppHandle>,
) -> AppResult<ToolApprovalRequest> {
    let approval = store.tool_approval(&approval_id)?;
    // Guard: only persist the trust pattern when the approval is actually pending.
    // Writing trust before the status check allows replaying a non-pending
    // approval_id to permanently escalate tool trust without executing the tool.
    if approval.status == "pending" {
        store.trust_tool_pattern(format!("{}.{}", approval.server_id, approval.tool_name))?;
    }
    approve_tool_call_and_resume(store, approval_id, timeout_seconds, app).await
}

pub async fn approve_tool_call_server_and_resume(
    store: &AppStore,
    approval_id: String,
    timeout_seconds: Option<u64>,
    app: Option<&AppHandle>,
) -> AppResult<ToolApprovalRequest> {
    let approval = store.tool_approval(&approval_id)?;
    if approval.status == "pending" {
        store.trust_tool_pattern(format!("{}.*", approval.server_id))?;
    }
    approve_tool_call_and_resume(store, approval_id, timeout_seconds, app).await
}

pub(super) async fn approve_tool_call_common(
    store: &AppStore,
    approval_id: String,
    timeout_seconds: Option<u64>,
    app: Option<&AppHandle>,
) -> AppResult<ToolApprovalRequest> {
    let approval = store.tool_approval(&approval_id)?;
    if approval.status != "pending" {
        return Ok(approval);
    }
    let run_id_for_interrupt = approval.run_id.clone();
    let run_started_at = Instant::now();
    let chat_config = store.config()?.chat;
    let run_timeout_seconds = chat_config.agent_run_timeout_seconds;
    let post_tool_quiet_timeout_seconds = chat_config.agent_post_tool_quiet_timeout_seconds;
    let workflow_executor = WorkflowDriver::new(WorkflowMode::ApprovalContinuation).executor();
    let executor_core = ExecutorCore::new(workflow_executor);
    let execution_iteration = approved_tool_execution_iteration(store, &approval);
    let result = if approval.server_id == "__internal" {
        let agent_id = approval
            .agent_id
            .as_deref()
            .ok_or_else(|| AppError::BadRequest("internal approval missing agentId".into()))?;
        let conversation_id = approval.conversation_id.as_deref().ok_or_else(|| {
            AppError::BadRequest("internal approval missing conversationId".into())
        })?;
        let run_id = approval
            .run_id
            .as_deref()
            .ok_or_else(|| AppError::BadRequest("internal approval missing runId".into()))?;
        let agent = store.agent(Some(agent_id))?;
        let replay_payload =
            approved_internal_tool_replay_payload(&approval.tool_name, approval.payload.clone());
        let execution_payload = replay_payload.clone();
        let tool_identity = workflow_executor.internal_tool(&approval.tool_name);
        executor_core.start_tool_execution(
            store,
            app,
            run_id,
            &tool_identity,
            &replay_payload,
            execution_iteration,
        )?;
        let future = executor_core.execute_internal_tool(
            store,
            ExecutorInternalToolExecutionContext {
                agent: &agent,
                conversation_id,
                run_id,
                tool_context: ToolExecutionContext::Interactive,
                app,
                approved_tool_call_replay: approval.tool_name == "tool_call",
            },
            &approval.tool_name,
            execution_payload,
        );
        match await_agent_run_interruptible(
            store,
            run_id,
            run_started_at,
            run_timeout_seconds,
            post_tool_quiet_timeout_seconds,
            app,
            future,
        )
        .await?
        {
            Some(Ok((stdout, event))) => {
                executor_core.record_tool_event(
                    store,
                    app,
                    conversation_id,
                    run_id,
                    event.clone(),
                )?;
                McpCallResult {
                    ok: true,
                    timed_out: false,
                    elapsed_ms: 0,
                    stdout,
                    stderr: String::new(),
                    error: None,
                }
            }
            Some(Err(error)) => {
                executor_core.record_tool_failed_with_iteration(
                    store,
                    app,
                    conversation_id,
                    run_id,
                    Some(execution_iteration),
                    &approval.tool_name,
                    &[],
                    &replay_payload,
                    &error,
                )?;
                McpCallResult {
                    ok: false,
                    timed_out: false,
                    elapsed_ms: 0,
                    stdout: String::new(),
                    stderr: error.to_string(),
                    error: Some(error.to_string()),
                }
            }
            None => {
                let error = AppError::BadRequest(
                    "Agent run was aborted before the approved tool completed.".into(),
                );
                executor_core.record_tool_failed_with_iteration(
                    store,
                    app,
                    conversation_id,
                    run_id,
                    Some(execution_iteration),
                    &approval.tool_name,
                    &[],
                    &replay_payload,
                    &error,
                )?;
                McpCallResult {
                    ok: false,
                    timed_out: false,
                    elapsed_ms: 0,
                    stdout: String::new(),
                    stderr: error.to_string(),
                    error: Some(error.to_string()),
                }
            }
        }
    } else {
        let replay_payload = approval.payload.clone();
        let execution_payload = strip_provider_tool_call_metadata(approval.payload.clone());
        let definition = ToolDefinition {
            name: format!("{}.{}", approval.server_id, approval.tool_name),
            display_name: approval.tool_name.clone(),
            description: String::new(),
            source: "mcp".into(),
            server_id: approval.server_id.clone(),
            tool_name: approval.tool_name.clone(),
            input_schema: json!({"type": "object"}),
            requires_approval: false,
        };
        let mcp_tools = vec![definition.clone()];
        if let Some(run_id) = approval.run_id.as_deref() {
            let tool_identity = workflow_executor.mcp_tool(
                &approval.tool_name,
                &approval.server_id,
                &approval.tool_name,
            );
            executor_core.start_tool_execution(
                store,
                app,
                run_id,
                &tool_identity,
                &replay_payload,
                execution_iteration,
            )?;
        }
        let future = call_mcp_tool_with_retry(
            store,
            approval.server_id.clone(),
            approval.tool_name.clone(),
            execution_payload,
            timeout_seconds,
            approval.run_id.as_deref(),
            0,
            0,
        );
        let result = if let Some(run_id) = run_id_for_interrupt.as_deref() {
            await_agent_run_interruptible(
                store,
                run_id,
                run_started_at,
                run_timeout_seconds,
                post_tool_quiet_timeout_seconds,
                app,
                future,
            )
            .await?
            .transpose()?
            .unwrap_or_else(|| McpCallResult {
                ok: false,
                timed_out: false,
                elapsed_ms: 0,
                stdout: String::new(),
                stderr: "Agent run was aborted before the approved MCP tool completed.".into(),
                error: Some("Agent run was aborted before the approved MCP tool completed.".into()),
            })
        } else {
            future.await?
        };
        if let (Some(conversation_id), Some(run_id)) = (
            approval.conversation_id.as_deref(),
            approval.run_id.as_deref(),
        ) {
            let mut event = mcp_result_to_tool_event(run_id, &definition, &result);
            event.raw = Some(redact_json_value(
                json!({"payload": replay_payload, "result": result.clone()}),
            ));
            executor_core.record_tool_event(store, app, conversation_id, run_id, event.clone())?;
            if !result.ok {
                let error = result
                    .error
                    .as_deref()
                    .filter(|error| !error.trim().is_empty())
                    .unwrap_or("approved MCP tool returned an unsuccessful result");
                executor_core.record_tool_failed_node(
                    store,
                    run_id,
                    Some(execution_iteration),
                    &approval.tool_name,
                    &mcp_tools,
                    error,
                )?;
            }
        }
        result
    };
    let updated = store.update_tool_approval(
        &approval_id,
        if result.ok { "approved" } else { "failed" },
        Some(json!(result)),
        result.error.clone(),
    )?;
    if updated.status != "approved" {
        if let Some(run_id) = updated.run_id.as_deref() {
            WorkflowDriver::new(WorkflowMode::ApprovalContinuation)
                .approval()
                .resolved(
                    store,
                    run_id,
                    &updated.server_id,
                    &updated.tool_name,
                    Some(updated.id.as_str()),
                    updated.status.as_str(),
                    Some(updated.reason.as_str()),
                    updated.error.as_deref(),
                )?;
        }
    }
    run_post_approval_response_hooks(store, &updated).await;
    Ok(updated)
}

fn approved_internal_tool_replay_payload(tool_name: &str, mut payload: Value) -> Value {
    if tool_name == "tool_call" {
        if let Some(object) = payload.as_object_mut() {
            object.insert(APPROVED_TOOL_CALL_REPLAY_KEY.into(), json!(true));
        }
    }
    payload
}

pub fn deny_tool_call_and_update_run(
    store: &AppStore,
    approval_id: String,
    reason: Option<String>,
    app: Option<&AppHandle>,
) -> AppResult<ToolApprovalRequest> {
    let approval = store.tool_approval(&approval_id)?;
    let denial_reason = reason
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Tool approval was denied by the user.")
        .to_string();
    let updated =
        store.update_tool_approval(&approval_id, "denied", None, Some(denial_reason.clone()))?;
    if let Some(run_id) = approval.run_id.as_deref() {
        WorkflowDriver::new(WorkflowMode::ApprovalContinuation)
            .approval()
            .resolved(
                store,
                run_id,
                &approval.server_id,
                &approval.tool_name,
                Some(approval.id.as_str()),
                updated.status.as_str(),
                Some(denial_reason.as_str()),
                updated.error.as_deref(),
            )?;
    }
    // Abort the run BEFORE firing post-approval hooks so that hooks reading
    // run.state see the terminal "aborted" state, not the stale "pendingApproval".
    if let Some(run_id) = approval.run_id.as_deref() {
        if let Ok(aborted) = store.abort_agent_run(
            run_id,
            Some(format!(
                "Tool approval denied for {}.{}: {}",
                approval.server_id, approval.tool_name, denial_reason
            )),
        ) {
            spawn_session_finished_hooks(
                store,
                aborted.clone(),
                json!({
                    "source": "approval_denied",
                    "server_id": approval.server_id,
                    "tool_name": approval.tool_name,
                    "reason": denial_reason,
                }),
            );
            if let Some(conversation_id) = approval.conversation_id.as_deref() {
                let assistant = store.append_message(ChatMessage::new(
                    conversation_id.to_string(),
                    "assistant",
                    format!(
                        "工具调用已拒绝，当前 agent run 已结束：{} · {}。\n\n原因：{}",
                        approval.server_id, approval.tool_name, denial_reason
                    ),
                    "desktop-agent-error",
                ))?;
                emit_agent_run_record(app, &aborted, Some(&assistant));
            } else {
                emit_agent_run_record(app, &aborted, None);
            }
        }
    }
    // Fire post-approval hooks after abort so hooks read the correct terminal state.
    spawn_post_approval_response_hooks(store, updated.clone());
    Ok(updated)
}
