import type { Message } from "../../api/sessions";
import type {
  ActiveRun,
  FileRef,
  PendingApprovalAction,
  PendingClarificationAction,
  ProblemDetails,
  Run,
  RunAccepted,
  Usage,
} from "../../api/runs";
import type { RunStreamEvent } from "../../api/sse";

export type ChatRunStreamStatus =
  | "idle"
  | "connecting"
  | "connected"
  | "reconnecting"
  | "closed"
  | "error";

export interface ChatDraftToolCall {
  callId: string;
  name: string;
  status: "running" | "completed" | "failed";
  inputSummary?: string;
  progressMessage?: string;
  progress?: number;
  resultSummary?: string;
  asyncDeliveryPending?: boolean;
  artifacts: FileRef[];
  error?: ProblemDetails;
}

export interface ChatPendingAsyncToolDelivery {
  callId: string;
  name: string;
}

export interface ChatAsyncToolDelivery {
  callId: string;
  processId: string;
  delivery: "completion" | "watch";
  status: "starting" | "running" | "exited" | "killed" | "lost" | "failed_start";
  exitCode?: number | null;
  matchedPatternCount?: number;
}

export interface ChatRunDraft {
  messageId: string;
  text: string;
  reasoning: string;
  tools: Record<string, ChatDraftToolCall>;
}

export interface ChatRunTerminal {
  kind: "completed" | "cancelled" | "failed";
  sequence: number | null;
  source: "event" | "rest";
  reason?: string;
  error?: ProblemDetails;
}

export interface ChatRunState {
  run: Run;
  disposition: RunAccepted["disposition"];
  queueItemId: string | null;
  sessionRevision: string;
  committedMessages: Message[];
  draft: ChatRunDraft | null;
  usage: Usage | null;
  pendingAction: PendingApprovalAction | PendingClarificationAction | null;
  lastSequence: number;
  serverLastSequence: number;
  recoveredAcrossGap: boolean;
  streamStatus: ChatRunStreamStatus;
  reconnectAttempt: number;
  streamError: string | null;
  protocolError: string | null;
  cancelPending: boolean;
  cancelError: string | null;
  pendingAsyncToolDeliveries: Record<string, ChatPendingAsyncToolDelivery>;
  asyncToolDeliveries: Record<string, ChatAsyncToolDelivery>;
  terminal: ChatRunTerminal | null;
}

export interface ChatRunsState {
  runs: Record<string, ChatRunState>;
  latestRunIdBySession: Record<string, string>;
}

export type ChatRunsAction =
  | { type: "run.accepted"; accepted: RunAccepted }
  | { type: "run.discovered"; activeRun: ActiveRun }
  | { type: "run.event"; runId: string; event: RunStreamEvent }
  | { type: "run.synced"; runId: string; run: Run }
  | { type: "run.reconciled"; runId: string; run: Run; messages: Message[] }
  | { type: "stream.connecting"; runId: string; reconnectAttempt: number }
  | { type: "stream.connected"; runId: string }
  | { type: "stream.closed"; runId: string }
  | { type: "stream.error"; runId: string; message: string }
  | { type: "cancel.requested"; runId: string }
  | { type: "cancel.failed"; runId: string; message: string };

export const initialChatRunsState: ChatRunsState = {
  runs: {},
  latestRunIdBySession: {},
};

export function hasPendingAsyncToolDeliveries(run: ChatRunState): boolean {
  return Object.keys(run.pendingAsyncToolDeliveries).length > 0;
}

function isTerminalStatus(status: Run["status"]): status is "completed" | "cancelled" | "failed" {
  return status === "completed" || status === "cancelled" || status === "failed";
}

function eventData<T>(event: RunStreamEvent): T {
  return event.payload.data as T;
}

function protocolFailure(run: ChatRunState, message: string): ChatRunState {
  return {
    ...run,
    streamStatus: "error",
    streamError: message,
    protocolError: message,
  };
}

function monotonicUsage(previous: Usage | null, next: Usage): Usage | null {
  if (
    previous
    && (
      next.promptTokens < previous.promptTokens
      || next.completionTokens < previous.completionTokens
      || next.totalTokens < previous.totalTokens
      || (typeof previous.cost === "number"
        && typeof next.cost === "number"
        && next.cost < previous.cost)
    )
  ) {
    return null;
  }
  return next;
}

function replaceCommitted(messages: Message[], message: Message): Message[] {
  const existing = messages.findIndex((item) => item.id === message.id);
  if (existing === -1) return [...messages, message];
  return messages.map((item, index) => index === existing ? message : item);
}

export function chatRunFromAccepted(accepted: RunAccepted): ChatRunState {
  return {
    run: accepted.run,
    disposition: accepted.disposition,
    queueItemId: accepted.queueItemId,
    sessionRevision: accepted.sessionRevision,
    committedMessages: [accepted.userMessage],
    draft: null,
    usage: accepted.run.usage,
    pendingAction: accepted.run.pendingAction,
    // A newly attached client replays from sequence 1, even if POST observed a later server sequence.
    lastSequence: 0,
    serverLastSequence: accepted.run.lastSequence,
    recoveredAcrossGap: false,
    streamStatus: "idle",
    reconnectAttempt: 0,
    streamError: null,
    protocolError: null,
    cancelPending: false,
    cancelError: null,
    pendingAsyncToolDeliveries: {},
    asyncToolDeliveries: {},
    terminal: null,
  };
}

export function chatRunFromDiscovery(activeRun: ActiveRun): ChatRunState {
  return chatRunFromAccepted({
    run: activeRun.run,
    disposition: "replayed",
    queueItemId: activeRun.queueItemId,
    userMessage: activeRun.userMessage,
    sessionRevision: activeRun.sessionRevision,
  });
}

function reduceEvent(run: ChatRunState, event: RunStreamEvent): ChatRunState {
  const sequence = event.payload.sequence;
  if (event.payload.runId !== run.run.id || event.payload.sessionId !== run.run.sessionId) {
    return protocolFailure(run, "Run event belongs to a different Run or Session.");
  }
  if (sequence <= run.lastSequence) return run;
  if (sequence !== run.lastSequence + 1) {
    return protocolFailure(run, "Run event sequence is not continuous.");
  }
  if (run.protocolError) return run;
  if (run.terminal && event.event !== "tool.delivery") {
    return protocolFailure(run, "A Run event arrived after the terminal event.");
  }

  let next: ChatRunState = {
    ...run,
    run: {
      ...run.run,
      lastSequence: Math.max(run.run.lastSequence, sequence),
      updatedAt: event.payload.occurredAt,
    },
    lastSequence: sequence,
    serverLastSequence: Math.max(run.serverLastSequence, sequence),
    streamStatus: "connected",
    streamError: null,
  };

  switch (event.event) {
    case "run.queued": {
      const data = eventData<{ queueItemId: string }>(event);
      next = {
        ...next,
        queueItemId: data.queueItemId,
        run: { ...next.run, status: "queued" },
      };
      break;
    }
    case "run.started":
      next = {
        ...next,
        queueItemId: null,
        pendingAction: null,
        recoveredAcrossGap: false,
        run: { ...next.run, status: "running", pendingAction: null },
      };
      break;
    case "message.started": {
      const data = eventData<{ messageId: string }>(event);
      if (
        (next.draft && !next.recoveredAcrossGap)
        || next.committedMessages.some((message) => message.id === data.messageId)
      ) {
        return protocolFailure(next, "A second assistant draft started before the first completed.");
      }
      next = {
        ...next,
        recoveredAcrossGap: false,
        draft: {
          messageId: data.messageId,
          text: "",
          reasoning: "",
          tools: {},
        },
      };
      break;
    }
    case "message.delta": {
      const data = eventData<{ messageId: string; delta: string }>(event);
      const draft = next.draft;
      if (!draft || draft.messageId !== data.messageId) {
        if (next.recoveredAcrossGap) break;
        return protocolFailure(next, "Message delta arrived without its assistant draft.");
      }
      next = { ...next, draft: { ...draft, text: draft.text + data.delta } };
      break;
    }
    case "reasoning.delta": {
      const data = eventData<{ messageId: string; delta: string }>(event);
      const draft = next.draft;
      if (!draft || draft.messageId !== data.messageId) {
        if (next.recoveredAcrossGap) break;
        return protocolFailure(next, "Reasoning delta arrived without its assistant draft.");
      }
      next = {
        ...next,
        draft: { ...draft, reasoning: draft.reasoning + data.delta },
      };
      break;
    }
    case "tool.started": {
      const data = eventData<{ callId: string; name: string; inputSummary?: string }>(event);
      let draft = next.draft;
      if (!draft && next.recoveredAcrossGap) {
        draft = {
          messageId: `recovered:${next.run.id}:${sequence}`,
          text: "",
          reasoning: "",
          tools: {},
        };
      }
      if (!draft || draft.tools[data.callId]) {
        return protocolFailure(next, "Tool start arrived outside a valid assistant draft.");
      }
      next = {
        ...next,
        draft: {
          ...draft,
          tools: {
            ...draft.tools,
            [data.callId]: {
              callId: data.callId,
              name: data.name,
              status: "running",
              artifacts: [],
              ...(data.inputSummary === undefined ? {} : { inputSummary: data.inputSummary }),
            },
          },
        },
      };
      break;
    }
    case "tool.progress": {
      const data = eventData<{ callId: string; message?: string; progress?: number }>(event);
      const tool = next.draft?.tools[data.callId];
      if (!next.draft || !tool || tool.status !== "running") {
        if (next.recoveredAcrossGap) break;
        return protocolFailure(next, "Tool progress arrived without a running tool.");
      }
      next = {
        ...next,
        draft: {
          ...next.draft,
          tools: {
            ...next.draft.tools,
            [data.callId]: {
              ...tool,
              ...(data.message === undefined ? {} : { progressMessage: data.message }),
              ...(data.progress === undefined ? {} : { progress: data.progress }),
            },
          },
        },
      };
      break;
    }
    case "tool.completed": {
      const data = eventData<{
        callId: string;
        resultSummary?: string;
        artifacts: FileRef[];
        asyncDeliveryPending?: boolean;
      }>(event);
      const tool = next.draft?.tools[data.callId];
      if (!next.draft || !tool || tool.status !== "running") {
        if (next.recoveredAcrossGap) break;
        return protocolFailure(next, "Tool completion arrived without a running tool.");
      }
      next = {
        ...next,
        draft: {
          ...next.draft,
          tools: {
            ...next.draft.tools,
            [data.callId]: {
              ...tool,
              status: "completed",
              artifacts: data.artifacts,
              ...(data.resultSummary === undefined ? {} : { resultSummary: data.resultSummary }),
              ...(data.asyncDeliveryPending === true ? { asyncDeliveryPending: true } : {}),
            },
          },
        },
      };
      break;
    }
    case "tool.delivery": {
      const data = eventData<ChatAsyncToolDelivery>(event);
      let draft = next.draft;
      const tool = draft?.tools[data.callId];
      if (draft && tool) {
        const { asyncDeliveryPending: _pending, ...settledTool } = tool;
        draft = {
          ...draft,
          tools: { ...draft.tools, [data.callId]: settledTool },
        };
      }
      const { [data.callId]: _deliveryPending, ...pendingAsyncToolDeliveries } =
        next.pendingAsyncToolDeliveries;
      next = {
        ...next,
        ...(draft === next.draft ? {} : { draft }),
        pendingAsyncToolDeliveries,
        asyncToolDeliveries: {
          ...next.asyncToolDeliveries,
          [data.processId]: data,
        },
      };
      break;
    }
    case "tool.failed": {
      const data = eventData<{ callId: string; error: ProblemDetails }>(event);
      const tool = next.draft?.tools[data.callId];
      if (!next.draft || !tool || tool.status !== "running") {
        if (next.recoveredAcrossGap) break;
        return protocolFailure(next, "Tool failure arrived without a running tool.");
      }
      next = {
        ...next,
        draft: {
          ...next.draft,
          tools: {
            ...next.draft.tools,
            [data.callId]: { ...tool, status: "failed", error: data.error },
          },
        },
      };
      break;
    }
    case "approval.required": {
      const data = eventData<Omit<PendingApprovalAction, "kind">>(event);
      const tool = next.draft?.tools[data.callId];
      if (
        next.run.status !== "running"
        || next.pendingAction !== null
        || !tool
        || tool.status !== "running"
        || tool.name !== data.toolName
      ) {
        return protocolFailure(next, "Approval request arrived without a matching running tool.");
      }
      const pendingAction: PendingApprovalAction = { kind: "approval", ...data };
      next = {
        ...next,
        pendingAction,
        run: { ...next.run, status: "waitingApproval", pendingAction },
      };
      break;
    }
    case "approval.resolved": {
      const data = eventData<{
        approvalId: string;
        callId: string;
        decision: "once" | "session" | "always" | "deny";
        resolvedBy: "user" | "expiry" | "cancellation";
      }>(event);
      const pending = next.pendingAction;
      if (
        pending?.kind !== "approval"
        || pending.approvalId !== data.approvalId
        || pending.callId !== data.callId
        || !pending.choices.includes(data.decision)
      ) {
        return protocolFailure(next, "Approval resolution does not match the pending approval.");
      }
      next = {
        ...next,
        pendingAction: null,
        run: {
          ...next.run,
          status: data.resolvedBy === "cancellation" ? "cancelling" : "running",
          pendingAction: null,
        },
      };
      break;
    }
    case "clarification.required": {
      const data = eventData<Omit<PendingClarificationAction, "kind">>(event);
      if (next.run.status !== "running" || next.pendingAction !== null || !next.draft) {
        return protocolFailure(next, "Clarification request arrived outside an active Run draft.");
      }
      const pendingAction: PendingClarificationAction = { kind: "clarification", ...data };
      next = {
        ...next,
        pendingAction,
        run: { ...next.run, status: "waitingClarification", pendingAction },
      };
      break;
    }
    case "clarification.resolved": {
      const data = eventData<{
        requestId: string;
        resolvedBy: "user" | "cancellation" | "failure";
      }>(event);
      const pending = next.pendingAction;
      if (pending?.kind !== "clarification" || pending.requestId !== data.requestId) {
        return protocolFailure(next, "Clarification resolution does not match the pending request.");
      }
      next = {
        ...next,
        pendingAction: null,
        run: {
          ...next.run,
          status: data.resolvedBy === "cancellation" ? "cancelling" : "running",
          pendingAction: null,
        },
      };
      break;
    }
    case "usage.updated": {
      const usage = monotonicUsage(next.usage, eventData<Usage>(event));
      if (!usage) return protocolFailure(next, "Run usage counters moved backwards.");
      next = { ...next, usage, run: { ...next.run, usage } };
      break;
    }
    case "message.completed": {
      const data = eventData<{ message: Message; sessionRevision: string }>(event);
      if (
        (!next.draft || next.draft.messageId !== data.message.id)
        && !next.recoveredAcrossGap
      ) {
        return protocolFailure(next, "Completed message does not match the assistant draft.");
      }
      if (
        !next.recoveredAcrossGap
        && next.draft
        && Object.values(next.draft.tools).some((tool) => tool.status === "running")
      ) {
        return protocolFailure(next, "Completed message still has a running tool.");
      }
      const pendingAsyncToolDeliveries = next.draft
        ? Object.fromEntries(
          Object.values(next.draft.tools)
            .filter((tool) => tool.asyncDeliveryPending === true)
            .map((tool) => [tool.callId, { callId: tool.callId, name: tool.name }]),
        ) as Record<string, ChatPendingAsyncToolDelivery>
        : {};
      next = {
        ...next,
        committedMessages: replaceCommitted(next.committedMessages, data.message),
        draft: null,
        recoveredAcrossGap: false,
        sessionRevision: data.sessionRevision,
        pendingAsyncToolDeliveries: {
          ...next.pendingAsyncToolDeliveries,
          ...pendingAsyncToolDeliveries,
        },
        run: { ...next.run, messageId: data.message.id },
      };
      break;
    }
    case "run.completed": {
      const data = eventData<{ usage: Usage; messageId: string }>(event);
      const usage = monotonicUsage(next.usage, data.usage);
      if (
        !usage
        || (next.draft && !next.recoveredAcrossGap)
        || !next.committedMessages.some((message) => message.id === data.messageId)
      ) {
        return protocolFailure(next, "Run completed before its committed assistant message.");
      }
      next = {
        ...next,
        draft: null,
        recoveredAcrossGap: false,
        usage,
        pendingAction: null,
        cancelPending: false,
        cancelError: null,
        run: {
          ...next.run,
          status: "completed",
          messageId: data.messageId,
          usage,
          error: null,
          pendingAction: null,
        },
        terminal: { kind: "completed", sequence, source: "event" },
      };
      break;
    }
    case "run.cancelled": {
      const data = eventData<{ reason?: string }>(event);
      next = {
        ...next,
        recoveredAcrossGap: false,
        pendingAction: null,
        cancelPending: false,
        cancelError: null,
        run: { ...next.run, status: "cancelled", pendingAction: null },
        terminal: {
          kind: "cancelled",
          sequence,
          source: "event",
          ...(data.reason === undefined ? {} : { reason: data.reason }),
        },
      };
      break;
    }
    case "run.failed": {
      const data = eventData<{ error: ProblemDetails }>(event);
      next = {
        ...next,
        recoveredAcrossGap: false,
        pendingAction: null,
        cancelPending: false,
        cancelError: null,
        streamError: publicProblemMessage(data.error),
        run: { ...next.run, status: "failed", error: data.error, pendingAction: null },
        terminal: { kind: "failed", sequence, source: "event", error: data.error },
      };
      break;
    }
  }
  return next;
}

function syncRun(current: ChatRunState, run: Run): ChatRunState {
  if (run.id !== current.run.id || run.sessionId !== current.run.sessionId) {
    return protocolFailure(current, "REST Run state belongs to a different resource.");
  }
  if (current.terminal && !isTerminalStatus(run.status)) {
    return protocolFailure(current, "REST Run state moved backwards from a terminal status.");
  }
  if (current.terminal && isTerminalStatus(run.status) && current.terminal.kind !== run.status) {
    return protocolFailure(current, "REST Run terminal status conflicts with the existing terminal state.");
  }
  const terminal = isTerminalStatus(run.status)
    ? current.terminal ?? {
      kind: run.status,
      sequence: null,
      source: "rest" as const,
      ...(run.error ? { error: run.error } : {}),
    }
    : null;
  return {
    ...current,
    run,
    usage: run.usage,
    pendingAction: run.pendingAction,
    serverLastSequence: Math.max(current.serverLastSequence, run.lastSequence),
    cancelPending: false,
    cancelError: null,
    terminal,
    recoveredAcrossGap: terminal ? false : current.recoveredAcrossGap,
    streamStatus: terminal && !hasPendingAsyncToolDeliveries(current)
      ? "closed"
      : current.streamStatus,
    streamError: run.status === "failed" && run.error
      ? publicProblemMessage(run.error)
      : current.streamError,
  };
}

function publicProblemMessage(problem: ProblemDetails): string {
  switch (problem.code) {
    case "provider_configuration_invalid":
      return "当前 Profile 的 Provider、模型或 Base URL 配置无效。";
    case "provider_authentication_failed":
      return "模型服务拒绝了当前 API Key，请在 Profile 与密钥中重新保存。";
    case "provider_rate_limited":
      return "模型服务正在限流，请稍后重试。";
    case "provider_request_rejected":
      return "模型服务拒绝了当前模型或请求参数，请核对模型名与推理设置。";
    case "provider_stream_failed":
      return "模型服务在流式响应过程中返回错误，请重试。";
    case "provider_response_invalid":
      return "模型服务返回了不完整或不兼容的流式响应。";
    default:
      break;
  }
  const detail = problem.detail?.trim();
  return detail ? `${problem.title}: ${detail}` : problem.title;
}

function reconcileMessages(
  current: Message[],
  recovered: Message[],
  sessionId: string,
): Message[] | null {
  if (recovered.some((message) => message.sessionId !== sessionId)) return null;
  const byId = new Map(current.map((message) => [message.id, message]));
  for (const message of recovered) byId.set(message.id, message);
  const merged = Array.from(byId.values()).sort((left, right) => left.sequence - right.sequence);
  if (merged.some((message, index) => (
    index > 0 && message.sequence === merged[index - 1]!.sequence
  ))) return null;
  return merged;
}

function reconcileRun(
  current: ChatRunState,
  run: Run,
  messages: Message[],
): ChatRunState {
  if (
    run.lastSequence < current.lastSequence
    || run.lastSequence < current.serverLastSequence
  ) {
    return protocolFailure(current, "Recovered Run sequence moved backwards.");
  }
  const synced = syncRun(current, run);
  if (synced.protocolError) return synced;
  const committedMessages = reconcileMessages(
    synced.committedMessages,
    messages,
    current.run.sessionId,
  );
  if (!committedMessages) {
    return protocolFailure(synced, "Recovered messages conflict with the current Session history.");
  }
  const draftCommitted = synced.draft
    && committedMessages.some((message) => message.id === synced.draft?.messageId);
  return {
    ...synced,
    committedMessages,
    draft: draftCommitted || (synced.terminal && synced.terminal.kind !== "failed")
      ? null
      : synced.draft,
    lastSequence: run.lastSequence,
    serverLastSequence: run.lastSequence,
    recoveredAcrossGap: !synced.terminal && run.lastSequence > current.lastSequence,
    streamError: synced.run.status === "failed" && synced.run.error
      ? publicProblemMessage(synced.run.error)
      : null,
  };
}

function updateRun(
  state: ChatRunsState,
  runId: string,
  update: (run: ChatRunState) => ChatRunState,
): ChatRunsState {
  const current = state.runs[runId];
  if (!current) return state;
  const next = update(current);
  if (next === current) return state;
  return { ...state, runs: { ...state.runs, [runId]: next } };
}

export function chatRunsReducer(state: ChatRunsState, action: ChatRunsAction): ChatRunsState {
  switch (action.type) {
    case "run.accepted": {
      const id = action.accepted.run.id;
      const existing = state.runs[id];
      const run = existing
        ? {
          ...existing,
          run: action.accepted.run,
          disposition: action.accepted.disposition,
          queueItemId: action.accepted.queueItemId,
          sessionRevision: action.accepted.sessionRevision,
          committedMessages: replaceCommitted(existing.committedMessages, action.accepted.userMessage),
          serverLastSequence: Math.max(existing.serverLastSequence, action.accepted.run.lastSequence),
        }
        : chatRunFromAccepted(action.accepted);
      if (existing && existing.run.sessionId !== action.accepted.run.sessionId) {
        return updateRun(state, id, (current) => protocolFailure(
          current,
          "Idempotent Run replay changed its Session binding.",
        ));
      }
      return {
        runs: { ...state.runs, [id]: run },
        latestRunIdBySession: {
          ...state.latestRunIdBySession,
          [action.accepted.run.sessionId]: id,
        },
      };
    }
    case "run.discovered": {
      const { activeRun } = action;
      const id = activeRun.run.id;
      const existing = state.runs[id];
      if (existing) {
        const existingUserMessage = existing.committedMessages.find(
          (message) => message.id === activeRun.userMessage.id,
        );
        if (
          existing.run.sessionId !== activeRun.run.sessionId
          || existing.run.profileId !== activeRun.run.profileId
          || !existingUserMessage
          || existingUserMessage.sessionId !== activeRun.userMessage.sessionId
          || existingUserMessage.sequence !== activeRun.userMessage.sequence
        ) {
          return updateRun(state, id, (current) => protocolFailure(
            current,
            "Active Run discovery conflicts with the existing Run owner or user Message.",
          ));
        }
        if (state.latestRunIdBySession[activeRun.run.sessionId] === id) return state;
        return {
          ...state,
          latestRunIdBySession: {
            ...state.latestRunIdBySession,
            [activeRun.run.sessionId]: id,
          },
        };
      }
      return {
        runs: { ...state.runs, [id]: chatRunFromDiscovery(activeRun) },
        latestRunIdBySession: {
          ...state.latestRunIdBySession,
          [activeRun.run.sessionId]: id,
        },
      };
    }
    case "run.event":
      return updateRun(state, action.runId, (run) => reduceEvent(run, action.event));
    case "run.synced":
      return updateRun(state, action.runId, (run) => syncRun(run, action.run));
    case "run.reconciled":
      return updateRun(state, action.runId, (run) => reconcileRun(
        run,
        action.run,
        action.messages,
      ));
    case "stream.connecting":
      return updateRun(state, action.runId, (run) => ({
        ...run,
        streamStatus: action.reconnectAttempt === 0 ? "connecting" : "reconnecting",
        reconnectAttempt: action.reconnectAttempt,
        streamError: null,
      }));
    case "stream.connected":
      return updateRun(state, action.runId, (run) => ({
        ...run,
        streamStatus: "connected",
        streamError: null,
      }));
    case "stream.closed":
      return updateRun(state, action.runId, (run) => ({
        ...run,
        streamStatus: "closed",
        streamError: run.terminal?.kind === "failed" ? run.streamError : null,
        pendingAsyncToolDeliveries: {},
      }));
    case "stream.error":
      return updateRun(state, action.runId, (run) => ({
        ...run,
        streamStatus: "error",
        streamError: action.message,
      }));
    case "cancel.requested":
      return updateRun(state, action.runId, (run) => ({
        ...run,
        cancelPending: true,
        cancelError: null,
      }));
    case "cancel.failed":
      return updateRun(state, action.runId, (run) => ({
        ...run,
        cancelPending: false,
        cancelError: action.message,
      }));
  }
}
