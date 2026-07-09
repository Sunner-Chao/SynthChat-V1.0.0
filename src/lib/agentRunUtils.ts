import type { AgentDefinition, AgentRunPhase, AgentRunRecord } from "./types";
import { toolEventStartKey } from "./toolEventUtils";

export function formatTime(value?: string | number | null) {
  if (!value) return "";
  const date = typeof value === "number" ? new Date(value) : new Date(value);
  return Number.isNaN(date.getTime()) ? String(value) : date.toLocaleString();
}

export function formatDurationMs(value: number) {
  if (!Number.isFinite(value)) return "–";
  const ms = Math.max(0, Math.floor(value));
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
  const minutes = Math.floor(ms / 60_000);
  const seconds = Math.floor((ms % 60_000) / 1000);
  return `${minutes}m ${seconds.toString().padStart(2, "0")}s`;
}

export function runStateLabel(state: string) {
  const labels: Record<string, string> = {
    pending: "排队中",
    started: "任务已启动",
    planning: "正在规划",
    running: "正在思考",
    running_tool: "执行中...",
    tool_completed: "成功",
    pendingApproval: "等待审批",
    needsClarification: "等待澄清",
    finalizing: "正在整理",
    completed: "已完成",
    failed: "失败",
    aborted: "已停止"
  };
  return labels[state] ?? state;
}

export function runPhaseLabel(phase: string) {
  const labels: Record<string, string> = {
    planner_started: "开始规划",
    planner_decision: "规划决策",
    approval_required: "等待审批",
    tool_started: "执行中...",
    tool_message_recorded: "成功",
    tool_batch_started: "执行中...",
    tool_batch_completed: "成功",
    steer_injected: "用户补充已注入",
    subagent_started: "子任务启动",
    subagent_completed: "子任务完成",
    subagent_failed: "子任务失败",
    subagent_aborted: "子任务已停止",
    acp_session_update: "ACP 工具更新",
    acp_permission_decision: "ACP 权限决策",
    memory_delegation_observed: "委派观察记录",
    llm_retry: "模型请求重试",
    llm_failover: "模型故障切换",
    llm_recovery: "模型错误恢复",
    llm_preflight_compaction: "上下文预压缩",
    finalizing: "整理结果"
  };
  return labels[phase] ?? phase;
}

export function isTerminalRunState(state?: string | null) {
  return state === "completed" || state === "failed" || state === "aborted";
}

export function compactRunText(value?: string | null, limit = 120) {
  const text = value?.trim() ?? "";
  if (!text) return "";
  return text.length > limit ? `${text.slice(0, limit)}...` : text;
}

export function queueStatusLabel(status: string) {
  const labels: Record<string, string> = {
    pending: "排队中",
    running: "执行中",
    completed: "已完成",
    failed: "失败",
    canceled: "已取消"
  };
  return labels[status] ?? status;
}

export function shortRuntimeId(value?: string | null) {
  if (!value) return "";
  const text = value.trim();
  if (text.length <= 14) return text;
  const parts = text.split("-");
  const prefix = parts[0] || "id";
  return `${prefix}-${text.slice(-8)}`;
}

export function runtimePayloadRecord(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

export function subagentTitle(run: AgentRunRecord) {
  const index = typeof run.subagentIndex === "number" ? `#${run.subagentIndex}` : "";
  const role = run.subagentRole?.trim() || "subagent";
  return [index, role].filter(Boolean).join(" ");
}

export function runningToolStartTimesFromPhases(phases: AgentRunPhase[] | undefined | null) {
  const starts = new Map<string, string>();
  for (const phase of phases ?? []) {
    if (phase.phase !== "tool_started" && phase.phase !== "tool_batch_started") continue;
    const detail = phase.detail && typeof phase.detail === "object"
      ? phase.detail as Record<string, unknown>
      : {};
    const serverId = typeof detail.serverId === "string" ? detail.serverId : "";
    const toolName = typeof detail.toolName === "string" ? detail.toolName : "";
    const callId = typeof detail.callId === "string" ? detail.callId : "";
    const referenceId = typeof detail.referenceId === "string" ? detail.referenceId : "";
    if (!serverId || !toolName) continue;
    starts.set(toolEventStartKey({ callId, referenceId, serverId, toolName }), phase.updatedAt);
  }
  return starts;
}

export function agentLabel(agent: AgentDefinition | null | undefined) {
  if (!agent) return "Default Agent";
  return agent.name || agent.id || "Agent";
}
