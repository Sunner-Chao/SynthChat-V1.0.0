import { describe, expect, it } from "vitest";
import type { Message } from "../../api/sessions";
import type {
  ActiveRun,
  ProblemDetails,
  Run,
  RunAccepted,
  RunEventName,
  RunEventPayload,
  Usage,
} from "../../api/runs";
import type { RunStreamEvent } from "../../api/sse";
import {
  chatRunsReducer,
  initialChatRunsState,
  type ChatRunsState,
} from "./runReducer";

const NOW = "2026-07-16T08:00:00Z";
const USAGE: Usage = {
  promptTokens: 10,
  completionTokens: 5,
  totalTokens: 15,
  cost: 0.01,
};
const PROBLEM: ProblemDetails = {
  type: "urn:synthchat:error:tool_failed",
  title: "Tool failed",
  status: 500,
  detail: "The command failed.",
  instance: null,
  code: "tool_failed",
  requestId: "request-1",
  retryable: false,
};
const RUN: Run = {
  id: "run-1",
  sessionId: "session-1",
  profileId: "default",
  status: "running",
  lastSequence: 0,
  messageId: null,
  usage: null,
  error: null,
  pendingAction: null,
  createdAt: NOW,
  updatedAt: NOW,
};
const USER_MESSAGE: Message = {
  id: "user-1",
  sessionId: "session-1",
  sequence: 1,
  role: "user",
  parts: [{ type: "text", text: "Migrate the backend" }],
  reasoning: null,
  toolCalls: [],
  usage: null,
  createdAt: NOW,
};
const ASSISTANT_MESSAGE: Message = {
  id: "assistant-1",
  sessionId: "session-1",
  sequence: 2,
  role: "assistant",
  parts: [{ type: "text", text: "Done" }],
  reasoning: "Checked the contract.",
  toolCalls: [],
  usage: USAGE,
  createdAt: NOW,
};
const ACCEPTED: RunAccepted = {
  run: { ...RUN, status: "running" },
  disposition: "started",
  queueItemId: null,
  userMessage: USER_MESSAGE,
  sessionRevision: "session_rev_1",
};

function streamEvent(event: RunEventName, sequence: number, data: unknown): RunStreamEvent {
  return {
    id: `run-1:${sequence}`,
    event,
    payload: {
      schemaVersion: 1,
      sequence,
      runId: "run-1",
      sessionId: "session-1",
      occurredAt: NOW,
      data,
    } as RunEventPayload,
  };
}

function acceptedState(): ChatRunsState {
  return chatRunsReducer(initialChatRunsState, { type: "run.accepted", accepted: ACCEPTED });
}

function applyEvents(...events: RunStreamEvent[]): ChatRunsState {
  return events.reduce(
    (state, event) => chatRunsReducer(state, { type: "run.event", runId: "run-1", event }),
    acceptedState(),
  );
}

describe("chatRunsReducer", () => {
  it("indexes an accepted Run and ignores a duplicate sequence", () => {
    const initial = acceptedState();
    expect(initial.latestRunIdBySession).toEqual({ "session-1": "run-1" });
    expect(initial.runs["run-1"]?.committedMessages).toEqual([USER_MESSAGE]);

    const started = chatRunsReducer(initial, {
      type: "run.event",
      runId: "run-1",
      event: streamEvent("run.started", 1, { profileId: "default" }),
    });
    const duplicate = chatRunsReducer(started, {
      type: "run.event",
      runId: "run-1",
      event: streamEvent("run.started", 1, { profileId: "default" }),
    });

    expect(duplicate).toBe(started);
    expect(duplicate.runs["run-1"]?.lastSequence).toBe(1);
  });

  it("replays a discovered Run from sequence one without duplicating its user Message", () => {
    const activeRun: ActiveRun = {
      run: { ...RUN, lastSequence: 4 },
      queueItemId: null,
      userMessage: USER_MESSAGE,
      sessionRevision: "session_rev_4",
    };
    const discovered = chatRunsReducer(initialChatRunsState, {
      type: "run.discovered",
      activeRun,
    });
    const duplicate = chatRunsReducer(discovered, { type: "run.discovered", activeRun });
    const started = chatRunsReducer(duplicate, {
      type: "run.event",
      runId: RUN.id,
      event: streamEvent("run.started", 1, { profileId: "default" }),
    });
    const drafting = chatRunsReducer(started, {
      type: "run.event",
      runId: RUN.id,
      event: streamEvent("message.started", 2, {
        messageId: ASSISTANT_MESSAGE.id,
        role: "assistant",
      }),
    });
    const resumed = chatRunsReducer(drafting, {
      type: "run.event",
      runId: RUN.id,
      event: streamEvent("message.delta", 3, {
        messageId: ASSISTANT_MESSAGE.id,
        delta: "resumed",
      }),
    });

    expect(duplicate).toBe(discovered);
    expect(discovered.runs[RUN.id]).toMatchObject({
      disposition: "replayed",
      sessionRevision: "session_rev_4",
      lastSequence: 0,
      serverLastSequence: 4,
      recoveredAcrossGap: false,
      committedMessages: [USER_MESSAGE],
    });
    expect(resumed.runs[RUN.id]).toMatchObject({
      lastSequence: 3,
      protocolError: null,
      draft: { messageId: ASSISTANT_MESSAGE.id, text: "resumed" },
    });
  });

  it("does not render suffix-only deltas after an expired event-history reconciliation", () => {
    const reconciled = chatRunsReducer(acceptedState(), {
      type: "run.reconciled",
      runId: RUN.id,
      run: { ...RUN, lastSequence: 4 },
      messages: [USER_MESSAGE],
    });
    const suffix = chatRunsReducer(reconciled, {
      type: "run.event",
      runId: RUN.id,
      event: streamEvent("message.delta", 5, {
        messageId: ASSISTANT_MESSAGE.id,
        delta: "suffix only",
      }),
    });
    const completed = chatRunsReducer(suffix, {
      type: "run.event",
      runId: RUN.id,
      event: streamEvent("message.completed", 6, {
        message: ASSISTANT_MESSAGE,
        sessionRevision: "session_rev_6",
      }),
    });

    expect(reconciled.runs[RUN.id]?.recoveredAcrossGap).toBe(true);
    expect(suffix.runs[RUN.id]).toMatchObject({
      lastSequence: 5,
      recoveredAcrossGap: true,
      draft: null,
      protocolError: null,
    });
    expect(completed.runs[RUN.id]).toMatchObject({
      lastSequence: 6,
      recoveredAcrossGap: false,
      committedMessages: [USER_MESSAGE, ASSISTANT_MESSAGE],
      protocolError: null,
    });
  });

  it("rejects a sequence gap without advancing the resume cursor", () => {
    const state = applyEvents(streamEvent("message.started", 2, {
      messageId: "assistant-1",
      role: "assistant",
    }));

    expect(state.runs["run-1"]).toMatchObject({
      lastSequence: 0,
      streamStatus: "error",
      protocolError: "Run event sequence is not continuous.",
    });
  });

  it("accumulates text and reasoning, commits the Message, and records one terminal", () => {
    const state = applyEvents(
      streamEvent("message.started", 1, { messageId: "assistant-1", role: "assistant" }),
      streamEvent("message.delta", 2, { messageId: "assistant-1", delta: "Do" }),
      streamEvent("message.delta", 3, { messageId: "assistant-1", delta: "ne" }),
      streamEvent("reasoning.delta", 4, { messageId: "assistant-1", delta: "Checked " }),
      streamEvent("reasoning.delta", 5, { messageId: "assistant-1", delta: "the contract." }),
      streamEvent("usage.updated", 6, USAGE),
      streamEvent("message.completed", 7, {
        message: ASSISTANT_MESSAGE,
        sessionRevision: "session_rev_2",
      }),
      streamEvent("run.completed", 8, { usage: USAGE, messageId: "assistant-1" }),
    );

    expect(state.runs["run-1"]).toMatchObject({
      draft: null,
      sessionRevision: "session_rev_2",
      usage: USAGE,
      terminal: { kind: "completed", sequence: 8, source: "event" },
      run: { status: "completed", messageId: "assistant-1" },
    });
    expect(state.runs["run-1"]?.committedMessages).toEqual([
      USER_MESSAGE,
      ASSISTANT_MESSAGE,
    ]);
  });

  it("rejects usage counters that move backwards", () => {
    const state = applyEvents(
      streamEvent("usage.updated", 1, USAGE),
      streamEvent("usage.updated", 2, {
        promptTokens: 10,
        completionTokens: 4,
        totalTokens: 14,
        cost: 0.009,
      }),
    );

    expect(state.runs["run-1"]).toMatchObject({
      lastSequence: 2,
      usage: USAGE,
      protocolError: "Run usage counters moved backwards.",
    });
  });

  it("tracks completed and failed tools independently inside the draft", () => {
    const state = applyEvents(
      streamEvent("message.started", 1, { messageId: "assistant-1", role: "assistant" }),
      streamEvent("tool.started", 2, {
        callId: "call-1",
        name: "terminal",
        inputSummary: "cargo test",
      }),
      streamEvent("tool.progress", 3, {
        callId: "call-1",
        message: "running",
        progress: 0.5,
      }),
      streamEvent("tool.completed", 4, {
        callId: "call-1",
        resultSummary: "passed",
        artifacts: [{
          id: "file-1",
          name: "report.txt",
          mimeType: "text/plain",
          sizeBytes: 6,
          createdAt: NOW,
        }],
      }),
      streamEvent("tool.started", 5, { callId: "call-2", name: "browser" }),
      streamEvent("tool.failed", 6, { callId: "call-2", error: PROBLEM }),
    );

    expect(state.runs["run-1"]?.draft?.tools).toMatchObject({
      "call-1": {
        status: "completed",
        progressMessage: "running",
        progress: 0.5,
        resultSummary: "passed",
        artifacts: [{ id: "file-1" }],
      },
      "call-2": { status: "failed", error: PROBLEM },
    });
  });

  it("keeps a pending async terminal delivery through Run completion and accepts its later event", () => {
    const state = applyEvents(
      streamEvent("message.started", 1, { messageId: "assistant-1", role: "assistant" }),
      streamEvent("tool.started", 2, { callId: "call-async", name: "terminal" }),
      streamEvent("tool.completed", 3, {
        callId: "call-async",
        resultSummary: "Background process started",
        artifacts: [],
        asyncDeliveryPending: true,
      }),
      streamEvent("message.completed", 4, {
        message: ASSISTANT_MESSAGE,
        sessionRevision: "session_rev_2",
      }),
      streamEvent("run.completed", 5, { usage: USAGE, messageId: "assistant-1" }),
      streamEvent("tool.delivery", 6, {
        callId: "call-async",
        processId: "process_0123456789abcdef0123456789abcdef",
        delivery: "completion",
        status: "exited",
        exitCode: 0,
      }),
    );

    expect(state.runs["run-1"]).toMatchObject({
      terminal: { kind: "completed", sequence: 5 },
      lastSequence: 6,
      pendingAsyncToolDeliveries: {},
      asyncToolDeliveries: {
        process_0123456789abcdef0123456789abcdef: {
          callId: "call-async",
          delivery: "completion",
          status: "exited",
          exitCode: 0,
        },
      },
    });
    expect(state.runs["run-1"]?.protocolError).toBeNull();
  });

  it("clears a matching approval only after its durable resolution event", () => {
    const state = applyEvents(
      streamEvent("message.started", 1, { messageId: "assistant-1", role: "assistant" }),
      streamEvent("tool.started", 2, { callId: "call-1", name: "terminal" }),
      streamEvent("approval.required", 3, {
        approvalId: "approval-1",
        callId: "call-1",
        toolName: "terminal",
        inputSummary: "cargo test",
        choices: ["once", "deny"],
        expiresAt: NOW,
      }),
      streamEvent("approval.resolved", 4, {
        approvalId: "approval-1",
        callId: "call-1",
        decision: "once",
        resolvedBy: "user",
      }),
    );

    expect(state.runs["run-1"]).toMatchObject({
      lastSequence: 4,
      pendingAction: null,
      run: { status: "running", pendingAction: null },
      protocolError: null,
    });
  });

  it("rejects out-of-order, mismatched, and duplicate approval requests", () => {
    const approval = {
      approvalId: "approval-1",
      callId: "call-1",
      toolName: "terminal",
      inputSummary: "cargo test",
      choices: ["once", "deny"],
      expiresAt: NOW,
    };
    const withoutTool = applyEvents(
      streamEvent("message.started", 1, { messageId: "assistant-1", role: "assistant" }),
      streamEvent("approval.required", 2, approval),
    );
    expect(withoutTool.runs["run-1"]?.protocolError).toBe(
      "Approval request arrived without a matching running tool.",
    );

    const mismatched = applyEvents(
      streamEvent("message.started", 1, { messageId: "assistant-1", role: "assistant" }),
      streamEvent("tool.started", 2, { callId: "call-1", name: "browser" }),
      streamEvent("approval.required", 3, approval),
    );
    expect(mismatched.runs["run-1"]?.protocolError).toBe(
      "Approval request arrived without a matching running tool.",
    );

    const duplicate = applyEvents(
      streamEvent("message.started", 1, { messageId: "assistant-1", role: "assistant" }),
      streamEvent("tool.started", 2, { callId: "call-1", name: "terminal" }),
      streamEvent("approval.required", 3, approval),
      streamEvent("approval.required", 4, approval),
    );
    expect(duplicate.runs["run-1"]?.protocolError).toBe(
      "Approval request arrived without a matching running tool.",
    );
  });

  it("clears a clarification only after its matching durable resolution event", () => {
    const state = applyEvents(
      streamEvent("message.started", 1, { messageId: "assistant-1", role: "assistant" }),
      streamEvent("clarification.required", 2, {
        requestId: "question-1",
        question: "Continue?",
        choices: ["yes", "no"],
      }),
      streamEvent("clarification.resolved", 3, {
        requestId: "question-1",
        resolvedBy: "user",
      }),
    );

    expect(state.runs["run-1"]).toMatchObject({
      lastSequence: 3,
      pendingAction: null,
      run: { status: "running", pendingAction: null },
      protocolError: null,
    });
  });

  it("rejects clarification requests and resolutions outside the pending state", () => {
    const withoutDraft = applyEvents(streamEvent("clarification.required", 1, {
      requestId: "question-1",
      question: "Continue?",
      choices: [],
    }));
    expect(withoutDraft.runs["run-1"]?.protocolError).toBe(
      "Clarification request arrived outside an active Run draft.",
    );

    const wrongResolution = applyEvents(
      streamEvent("message.started", 1, { messageId: "assistant-1", role: "assistant" }),
      streamEvent("clarification.required", 2, {
        requestId: "question-1",
        question: "Continue?",
        choices: [],
      }),
      streamEvent("clarification.resolved", 3, {
        requestId: "question-2",
        resolvedBy: "user",
      }),
    );
    expect(wrongResolution.runs["run-1"]?.protocolError).toBe(
      "Clarification resolution does not match the pending request.",
    );
  });

  it("does not commit an assistant Message while one of its tools is running", () => {
    const state = applyEvents(
      streamEvent("message.started", 1, { messageId: "assistant-1", role: "assistant" }),
      streamEvent("tool.started", 2, { callId: "call-1", name: "terminal" }),
      streamEvent("message.completed", 3, {
        message: ASSISTANT_MESSAGE,
        sessionRevision: "session_rev_2",
      }),
    );

    expect(state.runs["run-1"]).toMatchObject({
      lastSequence: 3,
      protocolError: "Completed message still has a running tool.",
      draft: { tools: { "call-1": { status: "running" } } },
    });
  });

  it("keeps the first terminal outcome and flags a later terminal event", () => {
    const requested = chatRunsReducer(acceptedState(), {
      type: "cancel.requested",
      runId: "run-1",
    });
    const cancelled = chatRunsReducer(requested, {
      type: "run.event",
      runId: "run-1",
      event: streamEvent("run.cancelled", 1, { reason: "user" }),
    });
    const state = chatRunsReducer(cancelled, {
      type: "run.event",
      runId: "run-1",
      event: streamEvent("run.failed", 2, { error: PROBLEM }),
    });

    expect(state.runs["run-1"]).toMatchObject({
      lastSequence: 1,
      cancelPending: false,
      terminal: { kind: "cancelled", reason: "user" },
      run: { status: "cancelled" },
      protocolError: "A Run event arrived after the terminal event.",
    });
  });

  it("surfaces a failed Run problem while retaining its partial draft", () => {
    const started = chatRunsReducer(acceptedState(), {
      type: "run.event",
      runId: "run-1",
      event: streamEvent("message.started", 1, {
        messageId: "assistant-1",
        role: "assistant",
      }),
    });
    const partial = chatRunsReducer(started, {
      type: "run.event",
      runId: "run-1",
      event: streamEvent("message.delta", 2, {
        messageId: "assistant-1",
        delta: "Partial answer",
      }),
    });
    const failed = chatRunsReducer(partial, {
      type: "run.event",
      runId: "run-1",
      event: streamEvent("run.failed", 3, { error: PROBLEM }),
    });

    expect(failed.runs["run-1"]).toMatchObject({
      draft: { text: "Partial answer" },
      run: { status: "failed", error: PROBLEM },
      streamError: "Tool failed: The command failed.",
      terminal: { kind: "failed", source: "event", error: PROBLEM },
    });
  });

  it("syncs cancellation state from REST and rejects a conflicting terminal", () => {
    const requested = chatRunsReducer(acceptedState(), {
      type: "cancel.requested",
      runId: "run-1",
    });
    const cancelledRun: Run = { ...RUN, status: "cancelled", updatedAt: NOW };
    const cancelled = chatRunsReducer(requested, {
      type: "run.synced",
      runId: "run-1",
      run: cancelledRun,
    });
    const conflict = chatRunsReducer(cancelled, {
      type: "run.synced",
      runId: "run-1",
      run: { ...cancelledRun, status: "failed", error: PROBLEM },
    });

    expect(cancelled.runs["run-1"]).toMatchObject({
      cancelPending: false,
      streamStatus: "closed",
      terminal: { kind: "cancelled", sequence: null, source: "rest" },
    });
    expect(conflict.runs["run-1"]?.protocolError).toBe(
      "REST Run terminal status conflicts with the existing terminal state.",
    );
  });
});
