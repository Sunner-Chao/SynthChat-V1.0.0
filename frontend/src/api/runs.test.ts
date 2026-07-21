import { describe, expect, it, vi } from "vitest";
import type { DesktopTransport } from "./desktopConnection";
import {
  createRunsApi,
  parseActiveRunList,
  parseRun,
  parseRunAccepted,
  parseRunEventPayload,
  RunApiError,
  type ActiveRunList,
  type CreateRunInput,
  type ProblemDetails,
  type Run,
  type RunAccepted,
  type RunEventName,
} from "./runs";
import type { Message } from "./sessions";

const NOW = "2026-07-16T08:00:00Z";
const PROBLEM: ProblemDetails = {
  type: "urn:synthchat:error:session_busy",
  title: "Session busy",
  status: 409,
  detail: "Wait for the active Run.",
  instance: "/api/v1/sessions/session-1/runs",
  code: "session_busy",
  requestId: "request-1",
  retryable: false,
};
const USER_MESSAGE: Message = {
  id: "message-user-1",
  sessionId: "session-1",
  sequence: 3,
  role: "user",
  parts: [{ type: "text", text: "Explain the migration" }],
  reasoning: null,
  toolCalls: [],
  usage: null,
  createdAt: NOW,
};
const RUN: Run = {
  id: "run-1",
  sessionId: "session-1",
  profileId: "default",
  status: "running",
  lastSequence: 1,
  messageId: "message-assistant-1",
  usage: null,
  error: null,
  pendingAction: null,
  createdAt: NOW,
  updatedAt: NOW,
};
const ACCEPTED = {
  run: { ...RUN, status: "running" as const },
  disposition: "started",
  queueItemId: null,
  userMessage: USER_MESSAGE,
  sessionRevision: "session_rev_after_user_1",
} as RunAccepted;
const ACTIVE_RUN_LIST = {
  items: [{
    run: RUN,
    queueItemId: null,
    userMessage: USER_MESSAGE,
    sessionRevision: "session_rev_after_user_1",
  }],
} as ActiveRunList;
const CREATE_INPUT: CreateRunInput = {
  clientRequestId: "client-request-1",
  message: { text: "Explain the migration", fileIds: ["file-1"] },
  personaId: "persona_0123456789abcdef0123456789abcdef",
  modelOverride: null,
  reasoningEffort: "medium",
};

function jsonResponse(value: unknown, status = 200): Response {
  return new Response(JSON.stringify(value), {
    status,
    headers: {
      "Content-Type": status >= 400
        ? "application/problem+json"
        : "application/json; charset=utf-8",
    },
  });
}

function envelope(sequence: number, data: unknown, overrides: Record<string, unknown> = {}) {
  return {
    schemaVersion: 1,
    sequence,
    runId: "run-1",
    sessionId: "session-1",
    occurredAt: NOW,
    data,
    ...overrides,
  };
}

function expectInvalid(parser: () => unknown): void {
  expect(parser).toThrowError(
    expect.objectContaining<Partial<RunApiError>>({ kind: "invalid_response" }),
  );
}

describe("Run API runtime contract", () => {
  it("parses Run state, pending actions, and all RunAccepted dispositions", () => {
    expect(parseRun(RUN)).toEqual(RUN);
    expect(parseRun({
      ...RUN,
      status: "waitingApproval",
      pendingAction: {
        kind: "approval",
        approvalId: "approval-1",
        callId: "call-1",
        toolName: "terminal",
        inputSummary: null,
        choices: ["once", "deny"],
        expiresAt: NOW,
      },
    })).toMatchObject({ status: "waitingApproval", pendingAction: { kind: "approval" } });
    expect(parseRun({
      ...RUN,
      status: "waitingClarification",
      pendingAction: {
        kind: "clarification",
        requestId: "clarification-1",
        question: "Which branch?",
        choices: ["main", "release"],
      },
    })).toMatchObject({ status: "waitingClarification", pendingAction: { kind: "clarification" } });
    expect(parseRunAccepted(ACCEPTED)).toEqual(ACCEPTED);
    expect(parseRunAccepted({
      ...ACCEPTED,
      disposition: "queued",
      queueItemId: "queue-1",
      run: { ...RUN, status: "queued" },
    })).toMatchObject({ disposition: "queued", queueItemId: "queue-1" });
    expect(parseRunAccepted({
      ...ACCEPTED,
      disposition: "replayed",
      queueItemId: null,
      run: { ...RUN, status: "completed", usage: { promptTokens: 2, completionTokens: 3, totalTokens: 5, cost: null } },
    })).toMatchObject({ disposition: "replayed", run: { status: "completed" } });
  });

  it("strictly parses a bounded, ordered active Run recovery snapshot", () => {
    expect(parseActiveRunList(ACTIVE_RUN_LIST)).toEqual(ACTIVE_RUN_LIST);
    const queued = {
      ...ACTIVE_RUN_LIST.items[0],
      run: { ...RUN, id: "run-queued", status: "queued" as const },
      queueItemId: "queue_0123456789abcdef0123456789abcdef",
    };
    expect(parseActiveRunList({ items: [queued] })).toEqual({ items: [queued] });
  });

  it.each([
    ["unknown list field", { ...ACTIVE_RUN_LIST, token: "leak" }],
    ["more than sixteen", { items: Array(17).fill(ACTIVE_RUN_LIST.items[0]) }],
    ["terminal Run", { items: [{
      ...ACTIVE_RUN_LIST.items[0], run: { ...RUN, status: "completed" },
    }] }],
    ["queue ID outside queued", { items: [{
      ...ACTIVE_RUN_LIST.items[0], queueItemId: "queue_0123456789abcdef0123456789abcdef",
    }] }],
    ["queued without queue ID", { items: [{
      ...ACTIVE_RUN_LIST.items[0], run: { ...RUN, status: "queued" },
    }] }],
    ["malformed queue ID", { items: [{
      ...ACTIVE_RUN_LIST.items[0], run: { ...RUN, status: "queued" }, queueItemId: "queue-1",
    }] }],
    ["assistant origin Message", { items: [{
      ...ACTIVE_RUN_LIST.items[0], userMessage: { ...USER_MESSAGE, role: "assistant" },
    }] }],
    ["cross-Session Message", { items: [{
      ...ACTIVE_RUN_LIST.items[0], userMessage: { ...USER_MESSAGE, sessionId: "session-2" },
    }] }],
    ["bad revision", { items: [{
      ...ACTIVE_RUN_LIST.items[0], sessionRevision: "bad\"revision",
    }] }],
    ["unstable ordering", { items: [
      { ...ACTIVE_RUN_LIST.items[0], run: { ...RUN, id: "run-2" } },
      { ...ACTIVE_RUN_LIST.items[0], run: { ...RUN, id: "run-1" } },
    ] }],
  ])("rejects active discovery with %s", (_label, value) => {
    expectInvalid(() => parseActiveRunList(value));
  });

  it.each([
    ["unknown Run field", { ...RUN, token: "leak" }],
    ["fractional sequence", { ...RUN, lastSequence: 1.5 }],
    ["wrong pending action", { ...RUN, status: "waitingApproval", pendingAction: null }],
    ["pending action outside wait", { ...RUN, pendingAction: { kind: "clarification" } }],
    ["duplicate approval choices", {
      ...RUN,
      status: "waitingApproval",
      pendingAction: {
        kind: "approval",
        approvalId: "a",
        callId: "c",
        toolName: "t",
        inputSummary: null,
        choices: ["once", "once"],
        expiresAt: NOW,
      },
    }],
    ["invalid date", { ...RUN, updatedAt: "today" }],
  ])("rejects %s", (_label, value) => {
    expectInvalid(() => parseRun(value));
  });

  it.each([
    ["started with queue", { ...ACCEPTED, queueItemId: "queue-1" }],
    ["queued with running Run", { ...ACCEPTED, disposition: "queued", queueItemId: "queue-1" }],
    ["assistant userMessage", { ...ACCEPTED, userMessage: { ...USER_MESSAGE, role: "assistant" } }],
    ["cross-Session userMessage", { ...ACCEPTED, userMessage: { ...USER_MESSAGE, sessionId: "other" } }],
    ["invalid revision", { ...ACCEPTED, sessionRevision: "bad\"revision" }],
  ])("rejects invalid RunAccepted %s", (_label, value) => {
    expectInvalid(() => parseRunAccepted(value));
  });

  it("parses every event-specific SSE data shape", () => {
    const cases: Array<[RunEventName, unknown]> = [
      ["run.queued", { queueItemId: "queue-1" }],
      ["run.started", { profileId: "default" }],
      ["message.started", { messageId: "message-assistant-1", role: "assistant" }],
      ["message.delta", { messageId: "message-assistant-1", delta: "Hello" }],
      ["reasoning.delta", { messageId: "message-assistant-1", delta: "Think" }],
      ["tool.started", { callId: "call-1", name: "terminal", inputSummary: "cargo test" }],
      ["tool.progress", { callId: "call-1", message: "running", progress: 0.5 }],
      ["tool.completed", { callId: "call-1", resultSummary: "ok", artifacts: [{
        id: "file-1", name: "result.txt", mimeType: "text/plain", sizeBytes: 2, createdAt: NOW,
      }] }],
      ["tool.delivery", {
        callId: "call-1",
        processId: "process_0123456789abcdef0123456789abcdef",
        delivery: "watch",
        status: "running",
        matchedPatternCount: 1,
      }],
      ["tool.failed", { callId: "call-1", error: PROBLEM }],
      ["approval.required", {
        approvalId: "approval-1", callId: "call-1", toolName: "terminal",
        inputSummary: null, choices: ["once", "deny"], expiresAt: NOW,
      }],
      ["approval.resolved", {
        approvalId: "approval-1", callId: "call-1", decision: "once", resolvedBy: "user",
      }],
      ["clarification.required", { requestId: "question-1", question: "Continue?", choices: [] }],
      ["clarification.resolved", { requestId: "question-1", resolvedBy: "user" }],
      ["usage.updated", { promptTokens: 10, completionTokens: 2, totalTokens: 12, cost: null }],
      ["message.completed", {
        message: { ...USER_MESSAGE, id: "message-assistant-1", role: "assistant", sequence: 4 },
        sessionRevision: "session_rev_after_assistant_1",
      }],
      ["run.completed", {
        usage: { promptTokens: 10, completionTokens: 2, totalTokens: 12 },
        messageId: "message-assistant-1",
      }],
      ["run.cancelled", {}],
      ["run.failed", { error: PROBLEM }],
    ];
    for (const [event, data] of cases) {
      expect(parseRunEventPayload(event, envelope(1, data))).toMatchObject({
        schemaVersion: 1,
        sequence: 1,
        runId: "run-1",
        sessionId: "session-1",
        data,
      });
    }
  });

  it.each([
    ["unknown envelope field", "run.started", { ...envelope(1, { profileId: "default" }), token: "x" }],
    ["wrong schema", "run.started", envelope(1, { profileId: "default" }, { schemaVersion: 2 })],
    ["invalid sequence", "run.started", envelope(0, { profileId: "default" })],
    ["extra data field", "message.delta", envelope(1, { messageId: "m", delta: "x", html: "x" })],
    ["empty delta", "message.delta", envelope(1, { messageId: "m", delta: "" })],
    ["progress out of range", "tool.progress", envelope(1, { callId: "c", progress: 1.1 })],
    ["async delivery leaks unexpected field", "tool.delivery", envelope(1, {
      callId: "c",
      processId: "process_0123456789abcdef0123456789abcdef",
      delivery: "completion",
      status: "exited",
      output: "private",
    })],
    ["invalid async delivery process id", "tool.delivery", envelope(1, {
      callId: "c",
      processId: "process_bad",
      delivery: "completion",
      status: "exited",
    })],
    ["watch delivery missing match count", "tool.delivery", envelope(1, {
      callId: "c",
      processId: "process_0123456789abcdef0123456789abcdef",
      delivery: "watch",
      status: "running",
    })],
    ["unknown artifact MIME", "tool.completed", envelope(1, {
      callId: "c",
      resultSummary: "ok",
      artifacts: [{
        id: "file-1",
        name: "result.bin",
        mimeType: "application/x-untrusted",
        sizeBytes: 1,
        createdAt: NOW,
      }],
    })],
    ["oversized artifact", "tool.completed", envelope(1, {
      callId: "c",
      resultSummary: "ok",
      artifacts: [{
        id: "file-1",
        name: "result.txt",
        mimeType: "text/plain",
        sizeBytes: 8 * 1024 * 1024 + 1,
        createdAt: NOW,
      }],
    })],
    ["non-deny expiry", "approval.resolved", envelope(1, {
      approvalId: "a", callId: "c", decision: "once", resolvedBy: "expiry",
    })],
    ["unknown clarification resolver", "clarification.resolved", envelope(1, {
      requestId: "question-1", resolvedBy: "expiry",
    })],
    ["clarification answer leak", "clarification.resolved", envelope(1, {
      requestId: "question-1", resolvedBy: "user", answer: "private",
    })],
    ["wrong completed role", "message.completed", envelope(1, {
      message: USER_MESSAGE,
      sessionRevision: "session_rev_1",
    })],
  ] as const)("rejects invalid event %s", (_label, event, value) => {
    expectInvalid(() => parseRunEventPayload(event as RunEventName, value));
  });
});

describe("Run REST client", () => {
  it("creates a Run with encoded Session binding, idempotency, and AbortSignal", async () => {
    const signal = new AbortController().signal;
    const request = vi.fn(async (_path: string, _init?: RequestInit, _options?: unknown) => (
      jsonResponse(ACCEPTED, 202)
    ));
    const api = createRunsApi({ request } as DesktopTransport);

    await expect(api.createRun("session-1", CREATE_INPUT, "idem-key-123", { signal }))
      .resolves.toEqual(ACCEPTED);
    expect(request).toHaveBeenCalledWith(
      "/api/v1/sessions/session-1/runs",
      {
        method: "POST",
        headers: {
          Accept: "application/json",
          "Content-Type": "application/json",
          "Idempotency-Key": "idem-key-123",
        },
        body: JSON.stringify(CREATE_INPUT),
      },
      { signal },
    );
  });

  it("gets and cancels only the requested Run", async () => {
    const request = vi.fn(async (path: string, _init?: RequestInit) => jsonResponse(
      path.endsWith("/cancel") ? { ...RUN, status: "cancelling" } : RUN,
      path.endsWith("/cancel") ? 202 : 200,
    ));
    const api = createRunsApi({ request } as DesktopTransport);

    await expect(api.getRun("run-1")).resolves.toEqual(RUN);
    await expect(api.cancelRun("run-1")).resolves.toMatchObject({ status: "cancelling" });
    expect(request.mock.calls.map(([path, init]) => [path, (init as RequestInit).method])).toEqual([
      ["/api/v1/runs/run-1", "GET"],
      ["/api/v1/runs/run-1/cancel", "POST"],
    ]);
  });

  it("lists active Runs with fixed state and optional Session owner", async () => {
    const sessionId = "session_0123456789abcdef0123456789abcdef";
    const response = {
      items: [{
        ...ACTIVE_RUN_LIST.items[0],
        run: { ...RUN, sessionId },
        userMessage: { ...USER_MESSAGE, sessionId },
      }],
    } as ActiveRunList;
    const request = vi.fn(async () => jsonResponse(response));
    const api = createRunsApi({ request } as DesktopTransport);

    await expect(api.listActiveRuns("default", { sessionId })).resolves.toEqual(response);
    expect(request).toHaveBeenCalledWith(
      `/api/v1/runs?profileId=default&state=active&sessionId=${sessionId}`,
      { method: "GET", headers: { Accept: "application/json" } },
      {},
    );
  });

  it("rejects invalid discovery owners and mismatched responses", async () => {
    const request = vi.fn(async () => jsonResponse({
      items: [{
        ...ACTIVE_RUN_LIST.items[0],
        run: { ...RUN, profileId: "other" },
      }],
    }));
    const api = createRunsApi({ request } as DesktopTransport);

    await expect(api.listActiveRuns("Default")).rejects.toMatchObject({ kind: "invalid_request" });
    await expect(api.listActiveRuns("default", { sessionId: "../secret" }))
      .rejects.toMatchObject({ kind: "invalid_request" });
    expect(request).not.toHaveBeenCalled();
    await expect(api.listActiveRuns("default"))
      .rejects.toMatchObject({ kind: "invalid_response" });
  });

  it("posts approval and clarification actions with strict bodies", async () => {
    const request = vi.fn(async (_path: string, _init?: RequestInit) => jsonResponse({ accepted: true }));
    const api = createRunsApi({ request } as DesktopTransport);

    await expect(api.resolveApproval("run/1", "approval/1", { decision: "once", reason: null }))
      .resolves.toEqual({ accepted: true });
    await expect(api.answerClarification("run/1", "question/1", { answer: "main" }))
      .resolves.toEqual({ accepted: true });
    expect(request.mock.calls.map(([path, init]) => [path, JSON.parse(String((init as RequestInit).body))])).toEqual([
      ["/api/v1/runs/run%2F1/approvals/approval%2F1", { decision: "once", reason: null }],
      ["/api/v1/runs/run%2F1/clarifications/question%2F1", { answer: "main" }],
    ]);
  });

  it("returns sanitized HTTP metadata and rejects malformed success envelopes", async () => {
    const request = vi.fn()
      .mockResolvedValueOnce(jsonResponse(PROBLEM, 409))
      .mockResolvedValueOnce(jsonResponse({ ...RUN, id: "other-run" }));
    const api = createRunsApi({ request } as DesktopTransport);

    await expect(api.createRun("session-1", CREATE_INPUT, "idem-key-123")).rejects.toMatchObject({
      kind: "http",
      status: 409,
      code: "session_busy",
      requestId: "request-1",
      retryable: false,
    });
    await expect(api.getRun("run-1")).rejects.toMatchObject({ kind: "invalid_response" });
  });

  it.each([
    ["short idempotency", CREATE_INPUT, "short"],
    ["unknown input", { ...CREATE_INPUT, secret: true }, "idem-key-123"],
    ["bad Persona ID", { ...CREATE_INPUT, personaId: "persona-invalid" }, "idem-key-123"],
    ["too many files", { ...CREATE_INPUT, message: { text: "x", fileIds: Array(21).fill("f") } }, "idem-key-123"],
    ["bad effort", { ...CREATE_INPUT, reasoningEffort: "extreme" }, "idem-key-123"],
    ["unsafe model URL", { ...CREATE_INPUT, modelOverride: {
      provider: "custom", model: "m", baseUrl: "https://user:pass@example.test/v1",
    } }, "idem-key-123"],
  ])("rejects invalid request %s before transport", async (_label, input, key) => {
    const request = vi.fn();
    const api = createRunsApi({ request } as unknown as DesktopTransport);
    await expect(api.createRun("session-1", input as CreateRunInput, key)).rejects.toMatchObject({
      kind: "invalid_request",
    });
    expect(request).not.toHaveBeenCalled();
  });
});
