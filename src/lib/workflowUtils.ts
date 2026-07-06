import { queueStatusLabel, runtimePayloadRecord } from "./agentRunUtils";
import {
  WORKFLOW_RUNTIME_KIND_PREFIX,
  WORKFLOW_RUNTIME_KIND_SNAPSHOT,
  WORKFLOW_RUNTIME_KIND_TRANSITION,
  WORKFLOW_RUNTIME_NODE_KIND_PREFIX,
  WORKFLOW_RUNTIME_SOURCE,
  workflowGraphCurrentNodeValue,
  workflowGraphCurrentStatusValue,
  workflowGraphLastEventSequenceValue,
  workflowGraphRequestSourceValue,
  workflowGraphToolContextValue,
  workflowHumanGateValue,
  workflowNodeDisplayLabel,
  workflowNodeRoleLabel,
  workflowRuntimePayloadEventSequenceValue,
  workflowRuntimeSummaryCurrentNodeValue,
  workflowRuntimeSummaryCurrentStatusValue,
  workflowRuntimeSummaryNodeCountValue,
  workflowRuntimeSummaryRequestSourceValue,
  workflowRuntimeSummaryToolContextValue,
  workflowRuntimeSummaryToolOriginsValue,
  workflowRuntimeSummaryTransitionCountValue,
  workflowSnapshotRuntimeGraphValue,
  workflowSnapshotRuntimeSummaryValue,
  workflowStatusDisplayLabel,
  workflowTransitionReasonLabel,
  workflowTransitionSequenceValue
} from "./types";
import type {
  AgentRuntimeEvent,
  WorkflowGraph,
  WorkflowGraphTransition,
  WorkflowNodeRuntimePayload,
  WorkflowRuntimeSummary,
  WorkflowSnapshotRuntimePayload,
  WorkflowTransitionRuntimePayload
} from "./types";

export function workflowToolOriginSummaryText(origins: string[]) {
  return origins
    .map((origin) => {
      if (origin === "provider_native") return "provider native";
      if (origin === "planner_json") return "planner JSON";
      if (origin === "hermes_markup") return "Hermes markup";
      return origin.replace(/_/g, " ");
    })
    .join(", ");
}

export function workflowSummaryText(summary?: WorkflowRuntimeSummary | null) {
  if (!summary) return "";
  const current = workflowNodeDisplayLabel(workflowRuntimeSummaryCurrentNodeValue(summary));
  const currentStatus = workflowRuntimeSummaryCurrentStatusValue(summary);
  const status = currentStatus ? ` (${workflowStatusDisplayLabel(currentStatus)})` : "";
  const nodeCount = workflowRuntimeSummaryNodeCountValue(summary);
  const transitionCount = workflowRuntimeSummaryTransitionCountValue(summary);
  const requestSource = workflowRuntimeSummaryRequestSourceValue(summary);
  const toolContext = workflowRuntimeSummaryToolContextValue(summary);
  const toolOrigins = workflowToolOriginSummaryText(workflowRuntimeSummaryToolOriginsValue(summary));
  const humanGate = workflowDetailValueText("humanGate", summary.humanGate ?? summary.human_gate);
  const counts = [
    typeof nodeCount === "number" ? `${nodeCount} nodes` : "",
    typeof transitionCount === "number" ? `${transitionCount} edges` : "",
    requestSource ? `source ${requestSource}` : "",
    toolContext ? `context ${toolContext}` : "",
    humanGate ? `human ${humanGate}` : "",
    toolOrigins ? `origins ${toolOrigins}` : ""
  ].filter(Boolean).join(" · ");
  return [`current ${current}${status}`, counts].filter(Boolean).join(" · ");
}

export function workflowGraphSnapshotText(graph?: WorkflowGraph | null) {
  if (!graph) return "";
  const currentNode = workflowGraphCurrentNodeValue(graph);
  const currentStatus = workflowGraphCurrentStatusValue(graph, currentNode);
  const lastEventSequence = workflowGraphLastEventSequenceValue(graph);
  const requestSource = workflowGraphRequestSourceValue(graph);
  const toolContext = workflowGraphToolContextValue(graph);
  const current = workflowNodeDisplayLabel(currentNode);
  const status = currentStatus ? ` (${workflowStatusDisplayLabel(currentStatus)})` : "";
  const counts = [
    `${graph.nodes?.length ?? 0} nodes`,
    `${graph.transitions?.length ?? 0} edges`,
    typeof lastEventSequence === "number" ? `seq ${lastEventSequence}` : "",
    requestSource ? `source ${requestSource}` : "",
    toolContext ? `context ${toolContext}` : ""
  ].filter(Boolean).join(" · ");
  return [`current ${current}${status}`, counts].filter(Boolean).join(" · ");
}

export function recentWorkflowGraphTransitions(graph?: WorkflowGraph | null): WorkflowGraphTransition[] {
  return (graph?.transitions ?? [])
    .slice()
    .sort((left, right) => (workflowTransitionSequenceValue(left) ?? 0) - (workflowTransitionSequenceValue(right) ?? 0))
    .slice(-3)
    .reverse();
}

export const WORKFLOW_DETAIL_VALUE_LABELS: Record<string, Record<string, string>> = {
  queueLifecycle: {
    dequeued_for_run: "dequeued for run",
    not_applicable: "not applicable",
    turn_completed: "turn completed",
    turn_failed: "turn failed",
    canceled: "canceled"
  },
  queueStatus: {
    claimed: "claimed",
    not_queued: "not queued",
    completed: "completed",
    failed: "failed",
    canceled: "canceled"
  },
  admission: {
    queued_turn: "queued turn",
    direct_turn: "direct turn"
  },
  kind: {
    resume_checkpoint: "resume checkpoint"
  },
  state: {
    resume_started: "resume started",
    resumed: "resumed",
    resume_failed: "resume failed"
  },
  errorKind: {
    context_compression: "context compression",
    iteration_budget_exhausted: "iteration budget exhausted",
    llm_error: "LLM error",
    llm_recovery_exhausted: "LLM recovery exhausted",
    no_final_answer: "no final answer",
    provider_turn_aborted: "provider turn aborted",
    tool_approval_required: "tool approval required",
    tool_schema_validation: "tool schema validation",
    tool_unavailable: "tool unavailable",
    tool_request: "tool request"
  },
  toolProtocol: {
    canonical_tool_call_v1: "canonical tool call v1"
  },
  toolOrigins: {
    provider_native: "provider native",
    planner_json: "planner JSON",
    hermes_markup: "Hermes markup"
  },
  bridgeStatus: {
    dispatch_ready: "dispatch ready",
    approval_required: "approval required",
    context_blocked: "context blocked",
    unavailable: "unavailable"
  }
};

export const WORKFLOW_DETAIL_ALIASES: Record<string, string[]> = {
  requestSource: ["request_source"],
  toolContext: ["tool_context"],
  queueItemId: ["queue_item_id"],
  queueStatus: ["queue_status"],
  queueLifecycle: ["queue_lifecycle"],
  preserveCurrent: ["preserve_current"],
  conversationKind: ["conversation_kind"],
  roomId: ["room_id"],
  channelId: ["channel_id"],
  chatId: ["chat_id"],
  threadId: ["thread_id"],
  groupId: ["group_id"],
  humanGate: ["human_gate"],
  approvalId: ["approval_id"],
  checkpointId: ["checkpoint_id"],
  checkpointScope: ["checkpoint_scope"],
  checkpointState: ["checkpoint_state"],
  checkpointSummary: ["checkpoint_summary"],
  checkpointIteration: ["checkpoint_iteration"],
  previousState: ["previous_state"],
  runState: ["run_state"],
  mutationKind: ["mutation_kind"],
  targetSummary: ["target_summary"],
  toolCount: ["tool_count"],
  toolProtocol: ["tool_protocol"],
  toolOrigins: ["tool_origins"],
  toolCallIds: ["tool_call_ids"],
  toolCalls: ["tool_calls"],
  providerNative: ["provider_native"],
  requestedName: ["requested_name"],
  serverId: ["server_id"],
  toolName: ["tool_name"],
  toolKind: ["tool_kind"],
  sourceLabel: ["source_label"],
  definitionName: ["definition_name"],
  requiresApproval: ["requires_approval"],
  directBridge: ["direct_bridge"],
  approvedToolCallReplay: ["approved_tool_call_replay"],
  bridgeStatus: ["bridge_status"],
  bridgeRejectionReason: ["bridge_rejection_reason"],
  bridgeStage: ["bridge_stage"],
  lastBridgeTarget: ["last_bridge_target"],
  messageId: ["message_id"],
  providerId: ["provider_id"],
  errorKind: ["error_kind"],
  timeoutSeconds: ["timeout_seconds"],
  requestedChildren: ["requested_children"],
  existingChildren: ["existing_children"],
  parentDepth: ["parent_depth"],
  childDepth: ["child_depth"],
  maxSubagents: ["max_subagents"],
  maxSubagentDepth: ["max_subagent_depth"],
  maxConcurrentChildren: ["max_concurrent_children"],
  orchestratorEnabled: ["orchestrator_enabled"],
  subagentAutoApprove: ["subagent_auto_approve"],
  inheritMcpToolsets: ["inherit_mcp_toolsets"],
  completedChildren: ["completed_children"],
  failedChildren: ["failed_children"],
  abortedChildren: ["aborted_children"],
  unknownChildren: ["unknown_children"],
  childIndex: ["child_index"],
  taskPreview: ["task_preview"],
  canDelegate: ["can_delegate"],
  maxIterations: ["max_iterations"],
  acpCommand: ["acp_command"],
  acpSessionMode: ["acp_session_mode"],
  childRunId: ["child_run_id"],
  childConversationId: ["child_conversation_id"],
  resultPreview: ["result_preview"],
  errorPreview: ["error_preview"],
  hasDiagnosticArtifact: ["has_diagnostic_artifact"]
};

export function workflowDetailRecordValue(record: Record<string, unknown>, key: string) {
  const keys = [key, ...(WORKFLOW_DETAIL_ALIASES[key] ?? [])];
  for (const candidate of keys) {
    if (record[candidate] !== undefined && record[candidate] !== null) return record[candidate];
  }
  return undefined;
}

export function workflowDetailValueText(key: string, value: unknown): string {
  if (key === "humanGate") {
    const gate = workflowHumanGateValue({ humanGate: value }) ?? runtimePayloadRecord(value);
    if (!gate) return "";
    const kind = workflowDetailValueText("kind", gate.kind);
    const status = workflowDetailValueText("status", gate.status);
    const target = [
      workflowDetailValueText("serverId", gate.serverId ?? gate.server_id),
      workflowDetailValueText("toolName", gate.toolName ?? gate.tool_name)
    ].filter(Boolean).join(".");
    const checkpoint = workflowDetailValueText("checkpointId", gate.checkpointId ?? gate.checkpoint_id);
    const approval = workflowDetailValueText("approvalId", gate.approvalId ?? gate.approval_id);
    const question = workflowDetailValueText("question", gate.question);
    return [
      kind,
      status,
      target,
      approval ? `approval ${approval}` : "",
      checkpoint ? `checkpoint ${checkpoint}` : "",
      question
    ].filter(Boolean).join(" · ");
  }
  if (typeof value === "string" && WORKFLOW_DETAIL_VALUE_LABELS[key]?.[value]) {
    return WORKFLOW_DETAIL_VALUE_LABELS[key][value];
  }
  if (typeof value === "string") return value.trim();
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  if (Array.isArray(value)) {
    return value
      .map((item) => {
        if (typeof item === "string" || typeof item === "number" || typeof item === "boolean") {
          return workflowDetailValueText(key, item);
        }
        const record = runtimePayloadRecord(item);
        if (!record) return "";
        const name = typeof record.name === "string" ? record.name : "";
        const origin = workflowDetailValueText("toolOrigins", record.origin);
        const id = typeof record.id === "string" ? record.id : "";
        return [name, origin, id].filter(Boolean).join(":");
      })
      .filter(Boolean)
      .join(", ");
  }
  const record = runtimePayloadRecord(value);
  if (record) {
    return [
      workflowDetailValueText("requestedName", workflowDetailRecordValue(record, "requestedName")),
      workflowDetailValueText("toolKind", workflowDetailRecordValue(record, "toolKind")),
      workflowDetailValueText("bridgeStatus", workflowDetailRecordValue(record, "bridgeStatus")),
      workflowDetailValueText("serverId", workflowDetailRecordValue(record, "serverId")),
      workflowDetailValueText("toolName", workflowDetailRecordValue(record, "toolName"))
    ].filter(Boolean).join(":");
  }
  return "";
}

export function workflowRuntimeDetailText(detail: unknown) {
  const record = runtimePayloadRecord(detail);
  if (!record) return "";
  const keys = [
    "queueLifecycle",
    "queueStatus",
    "queueItemId",
    "admission",
    "requestSource",
    "toolContext",
    "humanGate",
    "approvalId",
    "status",
    "serverId",
    "toolName",
    "requestedName",
    "toolKind",
    "sourceLabel",
    "definitionName",
    "directBridge",
    "approvedToolCallReplay",
    "bridgeStatus",
    "bridgeRejectionReason",
    "bridgeStage",
    "lastBridgeTarget",
    "checkpointId",
    "checkpointScope",
    "checkpointState",
    "checkpointIteration",
    "checkpointSummary",
    "previousState",
    "runState",
    "preserveCurrent",
    "mutationKind",
    "targetSummary",
    "kind",
    "state",
    "phase",
    "source",
    "conversationKind",
    "roomId",
    "channelId",
    "chatId",
    "threadId",
    "groupId",
    "strategy",
    "batch",
    "action",
    "toolCount",
    "tools",
    "toolProtocol",
    "toolOrigins",
    "toolCallIds",
    "toolCalls",
    "stage",
    "resolution",
    "requiresApproval",
    "messageId",
    "requestedChildren",
    "existingChildren",
    "completedChildren",
    "failedChildren",
    "abortedChildren",
    "unknownChildren",
    "parentDepth",
    "childDepth",
    "maxSubagents",
    "maxSubagentDepth",
    "maxConcurrentChildren",
    "ok",
    "orchestratorEnabled",
    "subagentAutoApprove",
    "inheritMcpToolsets",
    "summary",
    "aborted",
    "errorKind",
    "reason",
    "timeoutSeconds",
    "error"
  ];
  return keys
    .map((key) => {
      const text = workflowDetailValueText(key, workflowDetailRecordValue(record, key));
      return text ? `${key}=${text.length > 80 ? `${text.slice(0, 80)}...` : text}` : "";
    })
    .filter(Boolean)
    .slice(0, 4)
    .join(" · ");
}

export function workflowRuntimeEventText(event: AgentRuntimeEvent) {
  if (event.source !== WORKFLOW_RUNTIME_SOURCE && !event.kind.startsWith(WORKFLOW_RUNTIME_KIND_PREFIX)) return "";
  const payload = runtimePayloadRecord(event.payload);
  if (event.kind === WORKFLOW_RUNTIME_KIND_SNAPSHOT) {
    const snapshot = (payload ?? {}) as WorkflowSnapshotRuntimePayload;
    const summary = workflowSnapshotRuntimeSummaryValue(snapshot);
    const graph = workflowSnapshotRuntimeGraphValue(snapshot);
    return ["workflow snapshot", workflowSummaryText(summary) || workflowGraphSnapshotText(graph)].filter(Boolean).join(" · ");
  }
  if (event.kind.startsWith(WORKFLOW_RUNTIME_NODE_KIND_PREFIX)) {
    const nodePayload = (payload ?? {}) as WorkflowNodeRuntimePayload;
    const node = typeof nodePayload.node === "string" ? nodePayload.node : "";
    const role = typeof nodePayload.role === "string" ? nodePayload.role : workflowNodeRoleLabel(node);
    const status = typeof nodePayload.status === "string" ? nodePayload.status : event.status;
    const eventSequence = workflowRuntimePayloadEventSequenceValue(nodePayload);
    const sequence = typeof eventSequence === "number" ? `seq ${eventSequence}` : "";
    const detail = workflowRuntimeDetailText(nodePayload.detail);
    return ["workflow node", workflowNodeDisplayLabel(node), role, workflowStatusDisplayLabel(status), detail, sequence].filter(Boolean).join(" · ");
  }
  if (event.kind === WORKFLOW_RUNTIME_KIND_TRANSITION) {
    const transition = (payload ?? {}) as WorkflowTransitionRuntimePayload;
    const from = typeof transition.from === "string" ? transition.from : null;
    const to = typeof transition.to === "string" ? transition.to : null;
    const reason = workflowTransitionReasonLabel(
      typeof transition.reason === "string" ? transition.reason : event.status
    );
    const eventSequence = workflowRuntimePayloadEventSequenceValue(transition);
    const sequence = typeof eventSequence === "number" ? `seq ${eventSequence}` : "";
    const detail = workflowRuntimeDetailText(transition.detail);
    const topologySource = typeof transition.topologyEdgeSource === "string"
      ? transition.topologyEdgeSource
      : typeof transition.topology_edge_source === "string"
        ? transition.topology_edge_source
        : "";
    const topologyKnown = typeof transition.topologyEdgeKnown === "boolean"
      ? transition.topologyEdgeKnown
      : typeof transition.topology_edge_known === "boolean"
        ? transition.topology_edge_known
        : null;
    const topology = topologySource || topologyKnown === false
      ? `topology ${topologySource || "unknown"}`
      : "";
    return ["workflow edge", `${workflowNodeDisplayLabel(from)} -> ${workflowNodeDisplayLabel(to)}`, reason, topology, detail, sequence].filter(Boolean).join(" · ");
  }
  return "";
}

export function phaseDetailText(detail: unknown) {
  if (!detail || typeof detail !== "object") return "";
  const data = detail as Record<string, unknown>;
  const serverTool = typeof data.serverId === "string" && typeof data.toolName === "string"
    ? `${data.serverId}.${data.toolName}`
    : "";
  const acpUpdates = (Array.isArray(data.acpSessionUpdates) ? data.acpSessionUpdates.length : 0) + (data.update ? 1 : 0);
  const permissionDecisions = (Array.isArray(data.permissionDecisions) ? data.permissionDecisions.length : 0) + (data.decision ? 1 : 0);
  const parts = [
    typeof data.iteration === "number" ? `#${data.iteration}` : "",
    serverTool,
    typeof data.tool === "string" ? data.tool : "",
    typeof data.providerId === "string" ? data.providerId : "",
    typeof data.kind === "string" ? data.kind : "",
    typeof data.status === "string" ? data.status : "",
    typeof data.count === "number" ? `${data.count} calls` : "",
    typeof data.observationCount === "number" ? `${data.observationCount} observations` : "",
    acpUpdates > 0 ? `${acpUpdates} ACP updates` : "",
    permissionDecisions > 0 ? `${permissionDecisions} permissions` : "",
    typeof data.note === "string" ? data.note : "",
    typeof data.message === "string" ? data.message : "",
    typeof data.summaryTokens === "number" ? `${data.summaryTokens} tokens` : ""
  ].filter(Boolean);
  return parts.join(" · ");
}

export function acpUpdateLinesFromDetail(detail: unknown) {
  if (!detail || typeof detail !== "object") return [];
  const data = detail as Record<string, unknown>;
  const updates = [
    ...(Array.isArray(data.acpSessionUpdates) ? data.acpSessionUpdates : []),
    data.update
  ].filter(Boolean);
  const permissions = [
    ...(Array.isArray(data.permissionDecisions) ? data.permissionDecisions : []),
    data.decision
  ].filter(Boolean);
  const updateLines = updates.map((item) => {
    if (!item || typeof item !== "object") return "";
    const update = item as Record<string, unknown>;
    const kind = typeof update.sessionUpdate === "string"
      ? update.sessionUpdate
      : typeof update.session_update === "string"
        ? update.session_update
        : "update";
    if (kind === "tool_call" || kind === "tool_call_update") {
      const title = typeof update.title === "string" && update.title.trim() ? update.title.trim() : "tool";
      const status = typeof update.status === "string" && update.status.trim() ? update.status.trim() : (kind === "tool_call" ? "started" : "updated");
      const rawCallId = typeof update.toolCallId === "string"
        ? update.toolCallId
        : typeof update.tool_call_id === "string"
          ? update.tool_call_id
          : "";
      const callId = rawCallId.trim() ? ` · ${rawCallId.trim()}` : "";
      return `${kind === "tool_call" ? "ACP 工具启动" : "ACP 工具更新"} · ${title} · ${status}${callId}`;
    }
    if (kind === "plan") {
      const entries = Array.isArray(update.entries) ? update.entries : [];
      const active = entries
        .map((entry) => entry && typeof entry === "object" ? entry as Record<string, unknown> : null)
        .filter((entry) => entry && entry.status !== "completed")
        .slice(0, 2)
        .map((entry) => typeof entry?.content === "string" ? entry.content : "")
        .filter(Boolean);
      return `ACP 计划更新 · ${entries.length} 项${active.length ? ` · ${active.join(" / ")}` : ""}`;
    }
    if (kind === "available_commands_update") {
      const count = typeof update.availableCommandCount === "number" ? update.availableCommandCount : 0;
      return `ACP 可用命令 · ${count}`;
    }
    if (kind === "queue_update") {
      const status = typeof update.status === "string" && update.status.trim() ? queueStatusLabel(update.status.trim()) : "队列更新";
      const queueId = typeof update.queueId === "string"
        ? update.queueId
        : typeof update.queue_id === "string"
          ? update.queue_id
          : "";
      const position = typeof update.position === "number" && update.position > 0 ? `#${update.position}` : "";
      const pendingCount = typeof update.pendingCount === "number"
        ? `${update.pendingCount} pending`
        : typeof update.pending_count === "number"
          ? `${update.pending_count} pending`
          : "";
      const activeRunId = typeof update.activeRunId === "string"
        ? update.activeRunId
        : typeof update.active_run_id === "string"
          ? update.active_run_id
          : "";
      return ["ACP 队列", status, position, pendingCount, activeRunId, queueId].filter(Boolean).join(" · ");
    }
    return `ACP ${kind}`;
  }).filter(Boolean);
  const permissionLines = permissions.map((item) => {
    if (!item || typeof item !== "object") return "";
    const decision = item as Record<string, unknown>;
    const outcome = typeof decision.outcome === "string" ? decision.outcome : "";
    const optionId = typeof decision.optionId === "string" ? decision.optionId : "";
    const params = decision.params && typeof decision.params === "object" ? decision.params as Record<string, unknown> : {};
    const toolCall = (
      (params.toolCall && typeof params.toolCall === "object" ? params.toolCall : null) ||
      (params.tool_call && typeof params.tool_call === "object" ? params.tool_call : null)
    ) as Record<string, unknown> | null;
    const rawInput = toolCall?.rawInput && typeof toolCall.rawInput === "object"
      ? toolCall.rawInput as Record<string, unknown>
      : toolCall?.raw_input && typeof toolCall.raw_input === "object"
        ? toolCall.raw_input as Record<string, unknown>
        : null;
    const title = typeof toolCall?.title === "string" && toolCall.title.trim()
      ? toolCall.title.trim()
      : typeof rawInput?.command === "string" && rawInput.command.trim()
        ? rawInput.command.trim()
        : typeof rawInput?.description === "string" && rawInput.description.trim()
          ? rawInput.description.trim()
          : "";
    const label = decision.decision === "approved" ? "ACP 权限自动允许" : "ACP 权限自动取消";
    return [label, title, outcome, optionId].filter(Boolean).join(" · ");
  }).filter(Boolean);
  return [...updateLines, ...permissionLines];
}
