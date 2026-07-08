use crate::{
    error::{AppError, AppResult},
    models::{
        AgentDefinition, AgentRunRecord, LlmProvider, Persona, ToolApprovalRequest, ToolDefinition,
        ToolEvent,
    },
    store::AppStore,
};

use serde_json::Value;
use tauri::AppHandle;

use super::{
    append_tool_approval_request, apply_scheduled_approval_mode, apply_smart_approval_mode,
    apply_subagent_approval_override, execute_recovery_internal_tool, execute_recovery_mcp_tool,
    is_internal_tool, record_tool_event_for_run, record_tool_failed_for_run,
    record_tool_started_for_run, resolve_mcp_tool, run_pre_approval_request_hooks,
    tool_approval_reason,
    workflow_graph::{
        WorkflowExecutorApprovalPolicyStage, WorkflowExecutorApprovalWait, WorkflowExecutorNode,
        WorkflowExecutorRoute, WorkflowExecutorToolIdentity, WorkflowExecutorToolResolution,
    },
    PythonPluginBridgeContext, ToolExecutionContext,
};

#[derive(Debug, Clone, Copy)]
pub(super) struct ExecutorCore {
    workflow: WorkflowExecutorNode,
}

pub(super) struct ExecutorApprovalRequestContext<'a> {
    pub(super) conversation_id: &'a str,
    pub(super) persona_id: &'a str,
    pub(super) agent_id: &'a str,
    pub(super) run_id: &'a str,
    pub(super) tool_context: ToolExecutionContext,
}

pub(super) struct ExecutorApprovalRequestOutcome {
    pub(super) approval: ToolApprovalRequest,
    pub(super) route: WorkflowExecutorRoute,
}

pub(super) struct ExecutorInternalToolExecutionContext<'a> {
    pub(super) agent: &'a AgentDefinition,
    pub(super) conversation_id: &'a str,
    pub(super) run_id: &'a str,
    pub(super) tool_context: ToolExecutionContext,
    pub(super) app: Option<&'a AppHandle>,
    pub(super) approved_tool_call_replay: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecutorApprovalPolicyFlow {
    ChatTurn,
    ApprovalContinuation,
}

impl ExecutorApprovalPolicyFlow {
    fn includes_unattended_overrides(self) -> bool {
        matches!(self, Self::ChatTurn)
    }
}

pub(super) struct ExecutorApprovalPolicyContext<'a> {
    run_id: &'a str,
    providers: &'a [LlmProvider],
    persona: &'a Persona,
    tool_context: ToolExecutionContext,
    subagent_auto_approve: Option<bool>,
    flow: ExecutorApprovalPolicyFlow,
}

impl<'a> ExecutorApprovalPolicyContext<'a> {
    pub(super) fn chat_turn(
        run_id: &'a str,
        providers: &'a [LlmProvider],
        persona: &'a Persona,
        tool_context: ToolExecutionContext,
        subagent_auto_approve: Option<bool>,
    ) -> Self {
        Self {
            run_id,
            providers,
            persona,
            tool_context,
            subagent_auto_approve,
            flow: ExecutorApprovalPolicyFlow::ChatTurn,
        }
    }

    pub(super) fn approval_continuation(
        run_id: &'a str,
        providers: &'a [LlmProvider],
        persona: &'a Persona,
    ) -> Self {
        Self {
            run_id,
            providers,
            persona,
            tool_context: ToolExecutionContext::Interactive,
            subagent_auto_approve: None,
            flow: ExecutorApprovalPolicyFlow::ApprovalContinuation,
        }
    }
}

impl ExecutorCore {
    pub(super) fn new(workflow: WorkflowExecutorNode) -> Self {
        Self { workflow }
    }

    pub(super) fn resolve_tool(
        self,
        store: &AppStore,
        run_id: &str,
        iteration: u32,
        requested_name: &str,
        mcp_tools: &[ToolDefinition],
    ) -> AppResult<WorkflowExecutorToolResolution> {
        let resolution = self.workflow.resolve_tool(requested_name, mcp_tools);
        self.workflow
            .record_tool_resolution(store, run_id, iteration, &resolution)?;
        Ok(resolution)
    }

    pub(super) fn approval_wait(
        self,
        identity: &WorkflowExecutorToolIdentity,
        approval_id: Option<&str>,
        reason: Option<&str>,
    ) -> WorkflowExecutorApprovalWait {
        self.workflow.approval_wait(identity, approval_id, reason)
    }

    pub(super) fn await_tool_approval(
        self,
        store: &AppStore,
        run_id: &str,
        iteration: u32,
        wait: &WorkflowExecutorApprovalWait,
    ) -> AppResult<WorkflowExecutorRoute> {
        self.workflow
            .await_tool_approval(store, run_id, iteration, wait)
    }

    pub(super) fn await_approval_request(
        self,
        store: &AppStore,
        run_id: &str,
        iteration: u32,
        identity: &WorkflowExecutorToolIdentity,
        approval: &ToolApprovalRequest,
    ) -> AppResult<WorkflowExecutorRoute> {
        let wait = self.approval_wait(
            identity,
            Some(approval.id.as_str()),
            Some(approval.reason.as_str()),
        );
        self.await_tool_approval(store, run_id, iteration, &wait)
    }

    pub(super) async fn request_approval(
        self,
        store: &AppStore,
        context: ExecutorApprovalRequestContext<'_>,
        iteration: u32,
        identity: &WorkflowExecutorToolIdentity,
        payload: Value,
        reason: String,
    ) -> AppResult<ExecutorApprovalRequestOutcome> {
        run_pre_approval_request_hooks(
            store,
            context.run_id,
            identity.server_id(),
            identity.tool_name(),
            &payload,
            &reason,
        )
        .await;
        let approval = append_tool_approval_request(
            store,
            context.conversation_id,
            context.persona_id,
            context.agent_id,
            context.run_id,
            identity.server_id(),
            identity.tool_name(),
            payload,
            reason,
            context.tool_context,
        )?;
        let route =
            self.await_approval_request(store, context.run_id, iteration, identity, &approval)?;
        Ok(ExecutorApprovalRequestOutcome { approval, route })
    }

    fn record_tool_started(
        self,
        store: &AppStore,
        app: Option<&AppHandle>,
        run_id: &str,
        identity: &WorkflowExecutorToolIdentity,
        payload: &Value,
        iteration: u32,
    ) -> AppResult<()> {
        record_tool_started_for_run(
            store,
            app,
            run_id,
            identity.server_id(),
            identity.tool_name(),
            payload,
            iteration,
        )
    }

    pub(super) fn start_tool_execution(
        self,
        store: &AppStore,
        app: Option<&AppHandle>,
        run_id: &str,
        identity: &WorkflowExecutorToolIdentity,
        payload: &Value,
        iteration: u32,
    ) -> AppResult<AgentRunRecord> {
        self.workflow
            .tool_started(store, run_id, iteration, identity)?;
        self.record_tool_started(store, app, run_id, identity, payload, iteration)?;
        store.agent_run(run_id)
    }

    pub(super) async fn execute_internal_tool(
        self,
        store: &AppStore,
        context: ExecutorInternalToolExecutionContext<'_>,
        tool_name: &str,
        payload: Value,
    ) -> AppResult<(String, ToolEvent)> {
        execute_recovery_internal_tool(
            store,
            context.agent,
            context.conversation_id,
            context.run_id,
            tool_name,
            payload,
            context.tool_context,
            context.app,
            context.approved_tool_call_replay,
        )
        .await
    }

    pub(super) async fn execute_mcp_tool(
        self,
        store: &AppStore,
        run_id: &str,
        definition: &ToolDefinition,
        payload: Value,
        plugin_bridge_context: Option<&PythonPluginBridgeContext<'_>>,
    ) -> AppResult<(String, ToolEvent)> {
        execute_recovery_mcp_tool(store, run_id, definition, payload, plugin_bridge_context).await
    }

    pub(super) fn record_tool_event(
        self,
        store: &AppStore,
        app: Option<&AppHandle>,
        conversation_id: &str,
        run_id: &str,
        event: ToolEvent,
    ) -> AppResult<AgentRunRecord> {
        record_tool_event_for_run(store, app, conversation_id, run_id, event)?;
        store.agent_run(run_id)
    }

    pub(super) fn record_tool_failed(
        self,
        store: &AppStore,
        app: Option<&AppHandle>,
        conversation_id: &str,
        run_id: &str,
        requested_tool_name: &str,
        mcp_tools: &[ToolDefinition],
        payload: &Value,
        error: &AppError,
    ) -> AppResult<()> {
        self.record_tool_failed_with_iteration(
            store,
            app,
            conversation_id,
            run_id,
            None,
            requested_tool_name,
            mcp_tools,
            payload,
            error,
        )
    }

    pub(super) fn record_tool_failed_with_iteration(
        self,
        store: &AppStore,
        app: Option<&AppHandle>,
        conversation_id: &str,
        run_id: &str,
        iteration: Option<u32>,
        requested_tool_name: &str,
        mcp_tools: &[ToolDefinition],
        payload: &Value,
        error: &AppError,
    ) -> AppResult<()> {
        self.record_tool_failed_node(
            store,
            run_id,
            iteration,
            requested_tool_name,
            mcp_tools,
            &error.to_string(),
        )?;
        record_tool_failed_for_run(
            store,
            app,
            conversation_id,
            run_id,
            requested_tool_name,
            mcp_tools,
            payload,
            error,
        )
    }

    pub(super) fn record_tool_failed_node(
        self,
        store: &AppStore,
        run_id: &str,
        iteration: Option<u32>,
        requested_tool_name: &str,
        mcp_tools: &[ToolDefinition],
        error: &str,
    ) -> AppResult<()> {
        let (server_id, tool_name) = workflow_failed_tool_target(requested_tool_name, mcp_tools);
        self.workflow.failed(
            store,
            run_id,
            iteration,
            requested_tool_name,
            &server_id,
            &tool_name,
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
        self.workflow
            .record_approval_policy(store, run_id, iteration, identity, reason)
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
        self.workflow
            .record_approval_policy_stage(store, run_id, iteration, identity, stage, reason)
    }

    pub(super) async fn resolve_approval_policy(
        self,
        store: &AppStore,
        context: ExecutorApprovalPolicyContext<'_>,
        iteration: u32,
        identity: &WorkflowExecutorToolIdentity,
        requested_tool_name: &str,
        payload: &Value,
        base_requires_approval: bool,
    ) -> AppResult<Option<String>> {
        let mut approval_reason = tool_approval_reason(
            store,
            identity.server_id(),
            identity.tool_name(),
            payload,
            base_requires_approval,
        )?;
        self.record_approval_policy_stage(
            store,
            context.run_id,
            iteration,
            identity,
            WorkflowExecutorApprovalPolicyStage::Base,
            approval_reason.as_deref(),
        )?;

        if context.flow.includes_unattended_overrides() {
            approval_reason = apply_scheduled_approval_mode(
                store,
                context.tool_context,
                approval_reason,
                requested_tool_name,
            )?;
            self.record_approval_policy_stage(
                store,
                context.run_id,
                iteration,
                identity,
                WorkflowExecutorApprovalPolicyStage::Scheduled,
                approval_reason.as_deref(),
            )?;
        }

        approval_reason = apply_smart_approval_mode(
            store,
            context.run_id,
            context.providers,
            context.persona,
            approval_reason,
            identity.tool_name(),
            payload,
        )
        .await?;
        self.record_approval_policy_stage(
            store,
            context.run_id,
            iteration,
            identity,
            WorkflowExecutorApprovalPolicyStage::Smart,
            approval_reason.as_deref(),
        )?;

        if context.flow.includes_unattended_overrides() {
            approval_reason = apply_subagent_approval_override(
                context.tool_context,
                context.subagent_auto_approve,
                approval_reason,
                requested_tool_name,
            )?;
            self.record_approval_policy_stage(
                store,
                context.run_id,
                iteration,
                identity,
                WorkflowExecutorApprovalPolicyStage::Subagent,
                approval_reason.as_deref(),
            )?;
        }

        self.record_approval_policy(
            store,
            context.run_id,
            iteration,
            identity,
            approval_reason.as_deref(),
        )?;
        Ok(approval_reason)
    }

    pub(super) fn continue_planning(
        self,
        store: &AppStore,
        run_id: &str,
        iteration: u32,
        tool_count: usize,
        parallel: Option<bool>,
    ) -> AppResult<WorkflowExecutorRoute> {
        self.workflow
            .continue_planning(store, run_id, iteration, tool_count, parallel)
    }

    pub(super) fn start_parallel_batch(
        self,
        store: &AppStore,
        run_id: &str,
        iteration: u32,
        tool_count: usize,
        tool_names: &[String],
    ) -> AppResult<()> {
        self.workflow
            .parallel_batch_started(store, run_id, iteration, tool_count, tool_names)
    }

    pub(super) fn complete_parallel_batch(
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
        self.workflow.parallel_batch_completed(
            store, run_id, iteration, tool_count, tool_names, succeeded, failed, halted,
        )
    }
}

fn workflow_failed_tool_target(
    requested_tool_name: &str,
    mcp_tools: &[ToolDefinition],
) -> (String, String) {
    if is_internal_tool(requested_tool_name) {
        return ("__internal".into(), requested_tool_name.to_string());
    }
    if let Some(definition) = resolve_mcp_tool(mcp_tools, requested_tool_name) {
        return (definition.server_id, definition.tool_name);
    }
    ("<missing>".into(), requested_tool_name.to_string())
}
