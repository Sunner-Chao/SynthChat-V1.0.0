use crate::{
    error::AppResult,
    models::{ToolApprovalRequest, ToolDefinition},
    store::AppStore,
};

use serde_json::Value;
use tauri::AppHandle;

use super::{
    append_tool_approval_request, record_tool_started_for_run, run_pre_approval_request_hooks,
    workflow_graph::{
        WorkflowExecutorApprovalWait, WorkflowExecutorNode, WorkflowExecutorRoute,
        WorkflowExecutorToolIdentity, WorkflowExecutorToolResolution,
    },
    ToolExecutionContext,
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

    pub(super) fn record_tool_started(
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
        stage: &str,
        reason: Option<&str>,
    ) -> AppResult<()> {
        self.workflow
            .record_approval_policy_stage(store, run_id, iteration, identity, stage, reason)
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
}
