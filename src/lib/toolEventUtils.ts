import type {
  AgentRuntimeEvent,
  ManagedProcessEvent,
  ToolEvent,
  ToolEventEnvelope
} from "./types";

const RUNNING_TOOL_STARTED_AT = "__runningToolStartedAt";

export function parseToolEvent(content: string): ToolEvent | null {
  try {
    const parsed = JSON.parse(content) as Partial<ToolEventEnvelope>;
    if (parsed?.type === "toolEvent" && parsed.event) return parsed.event;
  } catch {
    return null;
  }
  return null;
}

export function toolEventStartKey(event: Pick<ToolEvent, "callId" | "referenceId" | "serverId" | "toolName">) {
  if (event.callId) return `call:${event.callId}`;
  if (event.referenceId) return `ref:${event.referenceId}`;
  return `${event.serverId}.${event.toolName}`;
}

export function toolEventStartedAt(event: ToolEvent): string | null {
  const raw = event.raw as Record<string, unknown> | null | undefined;
  const value = raw?.[RUNNING_TOOL_STARTED_AT];
  return typeof value === "string" && value.trim() ? value.trim() : null;
}

export function withToolEventStartedAt(event: ToolEvent, startedAt?: string | null): ToolEvent {
  if (!startedAt || event.status !== "running" || toolEventStartedAt(event)) return event;
  const raw = event.raw && typeof event.raw === "object" && !Array.isArray(event.raw)
    ? event.raw as Record<string, unknown>
    : {};
  return {
    ...event,
    raw: {
      ...raw,
      [RUNNING_TOOL_STARTED_AT]: startedAt
    }
  };
}

export function parseManagedProcessEvent(content: string): ManagedProcessEvent | null {
  try {
    const parsed = JSON.parse(content) as { type?: string; event?: ManagedProcessEvent };
    if (parsed?.type === "managedProcessEvent" && parsed.event) return parsed.event;
  } catch {
    return null;
  }
  return null;
}

export function managedProcessEventLabel(type: string) {
  const labels: Record<string, string> = {
    completed: "进程完成",
    stopped: "进程已停止",
    watch_match: "进程输出匹配",
    watch_disabled: "进程观察已降级"
  };
  return labels[type] ?? type;
}

export function managedProcessEventText(event: ManagedProcessEvent) {
  const detail = event.detail ?? {};
  const parts = [
    event.label || event.processId,
    typeof detail.exitCode === "number" ? `exit ${detail.exitCode}` : "",
    typeof detail.pattern === "string" ? `匹配 ${detail.pattern}` : "",
    typeof detail.stream === "string" ? detail.stream : "",
    typeof detail.line === "string" ? detail.line : "",
    typeof detail.reason === "string" ? detail.reason : ""
  ].filter(Boolean);
  return parts.join(" · ");
}

export function runtimeEventTime(event: AgentRuntimeEvent) {
  return event.createdAt ?? event.created_at ?? "";
}

/**
 * workflowTextFn is an optional callback for resolving workflow-specific text.
 * ChatExperience passes workflowRuntimeEventText; callers without workflow context omit it.
 */
export function runtimeEventText(
  event: AgentRuntimeEvent,
  workflowTextFn?: (event: AgentRuntimeEvent) => string,
  shortRuntimeIdFn?: (value?: string | null) => string
) {
  const workflowText = workflowTextFn?.(event) ?? "";
  if (workflowText) return workflowText;
  const shortId = shortRuntimeIdFn ?? ((v?: string | null) => v?.trim() ?? "");
  const runId = event.runId ?? event.run_id;
  const queueItemId = event.queueItemId ?? event.queue_item_id;
  const taskId = event.taskId ?? event.task_id;
  const processId = event.processId ?? event.process_id;
  return [
    event.kind,
    event.status,
    taskId ? `task ${shortId(taskId)}` : "",
    runId ? `run ${shortId(runId)}` : "",
    queueItemId ? `queue ${shortId(queueItemId)}` : "",
    processId ? `process ${shortId(processId)}` : "",
    event.source
  ].filter(Boolean).join(" · ");
}

export function eventStatusLabel(event: ToolEvent) {
  if (event.status === "running") return "执行中...";
  if (event.ok) return "成功";
  return "失败";
}

export function isCanceledToolEvent(event: ToolEvent) {
  return event.status === "canceled" || event.status === "cancelled";
}

export function materializeToolEvent(event: ToolEvent, terminalRunState?: string | null): ToolEvent {
  // Inline terminal-state check to avoid circular dependency with agentRunUtils
  const isTerminal = terminalRunState === "completed"
    || terminalRunState === "failed"
    || terminalRunState === "aborted";
  if (event.status !== "running" || !isTerminal) return event;
  const summary = terminalRunState === "completed"
    ? "运行已完成，工具调用已结束"
    : terminalRunState === "aborted"
      ? "运行已取消，工具调用已结束"
      : "运行已失败，工具调用已结束";
  return {
    ...event,
    status: terminalRunState === "failed" ? "failed" : "canceled",
    ok: false,
    summary,
    error: summary
  };
}

export function toolEventRank(event: ToolEvent) {
  if (isCanceledToolEvent(event)) return -1;
  if (event.status === "running") return 0;
  if (!event.ok || event.status === "failed") return 2;
  return 3;
}

export function eventKey(event: ToolEvent, index: number) {
  if (event.callId) return `call:${event.callId}`;
  if (event.referenceId) return `ref:${event.referenceId}`;
  return `${event.serverId}:${event.toolName}:${event.elapsedMs}:${index}`;
}

export function toolEventMessageKey(event: ToolEvent) {
  if (event.callId) return `call:${event.callId}`;
  if (event.referenceId) return `ref:${event.referenceId}`;
  return `${event.serverId}.${event.toolName}`;
}

export function selectVisibleToolEvents(events: ToolEvent[]) {
  const selected = new Map<string, { index: number; event: ToolEvent }>();
  const suppressed = new Set<string>();
  events.forEach((event, index) => {
    const key = toolEventMessageKey(event);
    if (isCanceledToolEvent(event)) {
      selected.delete(key);
      suppressed.add(key);
      return;
    }
    suppressed.delete(key);
    const previous = selected.get(key);
    if (
      !previous
      || toolEventRank(event) > toolEventRank(previous.event)
      || (
        toolEventRank(event) === toolEventRank(previous.event)
        && index > previous.index
      )
    ) {
      selected.set(key, { index, event });
    }
  });
  return Array.from(selected.values())
    .filter((item) => !suppressed.has(toolEventMessageKey(item.event)))
    .sort((a, b) => a.index - b.index)
    .map((item) => item.event);
}
