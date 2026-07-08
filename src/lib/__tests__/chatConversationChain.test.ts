import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "../api";
import { __chatStoreTestUtils, useAppStore } from "../store";
import type { AgentDefinition, ChatMessage, SendChatRequest } from "../types";
import {
  deterministicChatResponse,
  testAgentRunEvent,
  testConversation,
  testMessage,
  TEST_NOW,
  testQueuedRequest,
  testToolEvent
} from "./chatTestHarness";

type StoreState = ReturnType<typeof useAppStore.getState>;

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

async function flushAsync() {
  await Promise.resolve();
  await Promise.resolve();
}

function stubRuntimeGlobals() {
  let uuid = 0;
  vi.stubGlobal("crypto", {
    randomUUID: vi.fn(() => `uuid-${++uuid}`)
  });
  vi.stubGlobal("window", {
    setTimeout: ((handler: TimerHandler, timeout?: number) => setTimeout(handler, timeout)) as typeof window.setTimeout,
    clearTimeout: ((handle?: number) => clearTimeout(handle)) as typeof window.clearTimeout
  });
}

function agent(id: string, isDefault = false): AgentDefinition {
  return {
    id,
    name: id,
    isDefault
  } as AgentDefinition;
}

function resetStore(overrides: Partial<StoreState> = {}) {
  __chatStoreTestUtils.resetPendingIncomingMessagesForTests();
  useAppStore.setState({
    activeSection: "chat",
    focusedAgentId: null,
    conversations: [testConversation()],
    activeConversationId: "conv-1",
    messages: [],
    conversationMessageLimits: {},
    processingConversationIds: [],
    conversationUnreadCounts: {},
    agents: [agent("agent-conv"), agent("agent-explicit", true)],
    personas: [{ id: "persona-1", name: "Persona", agentId: "agent-conv" } as any],
    agentQueue: [],
    agentRuns: [],
    activeAgentRuns: {},
    streamedAssistantIds: new Set<string>(),
    managedProcessEvents: [],
    ...overrides
  });
}

function mockRefreshBackends() {
  vi.spyOn(api, "listAgentQueue").mockResolvedValue([]);
  vi.spyOn(api, "listAgentRuns").mockResolvedValue([]);
  vi.spyOn(api, "listConversations").mockResolvedValue([testConversation()]);
  vi.spyOn(api, "listMessages").mockResolvedValue([]);
}

describe("chat conversation store chain", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date(TEST_NOW));
    stubRuntimeGlobals();
    resetStore();
    mockRefreshBackends();
  });

  afterEach(() => {
    vi.clearAllTimers();
    vi.useRealTimers();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it("sends a deterministic chat request, shows a local user message, then merges backend user and assistant", async () => {
    const response = deferred<ChatMessage[]>();
    const sendSpy = vi.spyOn(api, "sendChatMessage").mockReturnValue(response.promise);

    await useAppStore.getState().sendMessage("  hello agent  ", "persona-1", "agent-explicit");

    const localMessages = useAppStore.getState().messages;
    expect(localMessages).toHaveLength(1);
    expect(localMessages[0]).toMatchObject({
      id: "local-uuid-1",
      role: "user",
      content: "hello agent",
      source: "desktop",
      providerData: {
        source: "desktop",
        clientMessageId: "local-uuid-1"
      }
    });
    expect(useAppStore.getState().processingConversationIds).toEqual(["conv-1"]);

    const request = sendSpy.mock.calls[0][0] as SendChatRequest;
    expect(request).toMatchObject({
      conversationId: "conv-1",
      personaId: "persona-1",
      agentId: "agent-explicit",
      content: "hello agent"
    });
    expect((request.providerData as any).clientMessageId).toBe("local-uuid-1");

    response.resolve(deterministicChatResponse(request));
    await flushAsync();

    const finalMessages = useAppStore.getState().messages;
    expect(finalMessages.map((item) => [item.id, item.role, item.content])).toEqual([
      ["backend-user-1", "user", "hello agent"],
      ["assistant-1", "assistant", "deterministic reply: hello agent"]
    ]);
    expect(finalMessages.some((item) => item.id === "local-uuid-1")).toBe(false);
    expect(useAppStore.getState().processingConversationIds).toEqual([]);
  });

  it("filters invalid explicit agent ids and falls back to the conversation agent when no explicit agent is provided", async () => {
    const cases: Array<{ label: string; agentId?: string; expected: string | null }> = [
      { label: "invalid explicit", agentId: "missing-agent", expected: null },
      { label: "undefined fallback", agentId: undefined, expected: "agent-conv" }
    ];

    for (const entry of cases) {
      resetStore();
      const response = deferred<ChatMessage[]>();
      const sendSpy = vi.spyOn(api, "sendChatMessage").mockReturnValue(response.promise);

      await useAppStore.getState().sendMessage(entry.label, "persona-1", entry.agentId);

      const request = sendSpy.mock.calls[0][0] as SendChatRequest;
      expect(request.agentId).toBe(entry.expected);
      response.resolve(deterministicChatResponse(request));
      await flushAsync();
      sendSpy.mockRestore();
    }
  });

  it("keeps transport failures out of the chat timeline, refreshes runtime state, and allows a retry to converge", async () => {
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    const queueSpy = vi.spyOn(useAppStore.getState(), "refreshAgentQueue");
    const runsSpy = vi.spyOn(useAppStore.getState(), "refreshAgentRuns");
    const sendSpy = vi.spyOn(api, "sendChatMessage")
      .mockRejectedValueOnce(new Error("provider offline"))
      .mockImplementationOnce(async (request: unknown) => deterministicChatResponse(request as SendChatRequest, {
        userId: "backend-user-retry",
        assistantId: "assistant-retry"
      }));

    await useAppStore.getState().sendMessage("please retry", "persona-1", "agent-explicit");
    await flushAsync();
    await vi.runOnlyPendingTimersAsync();

    const failedMessages = useAppStore.getState().messages;
    expect(failedMessages).toHaveLength(1);
    expect(failedMessages[0]).toMatchObject({
      id: "local-uuid-1",
      role: "user",
      content: "please retry"
    });
    expect(failedMessages.some((item) => item.role === "assistant" || item.source === "desktop-agent-error")).toBe(false);
    expect(useAppStore.getState().processingConversationIds).toEqual([]);
    expect(queueSpy).toHaveBeenCalled();
    expect(runsSpy).toHaveBeenCalled();

    await useAppStore.getState().sendMessage("please retry", "persona-1", "agent-explicit");
    await flushAsync();

    const retryRequest = sendSpy.mock.calls[1][0] as SendChatRequest;
    expect((retryRequest.providerData as any).clientMessageId).toBe("local-uuid-2");
    expect(useAppStore.getState().messages.map((item) => [item.id, item.role])).toEqual([
      ["backend-user-retry", "user"],
      ["assistant-retry", "assistant"]
    ]);
    expect(useAppStore.getState().messages.some((item) => item.id.startsWith("local-"))).toBe(false);
  });

  it("preserves an active assistant stream through a stale chat refresh while processing is active", async () => {
    useAppStore.getState().upsertIncomingMessage(
      testMessage({
        id: "assistant-active-stream",
        role: "assistant",
        content: "Partial active response",
        source: "desktop"
      }),
      { streaming: true }
    );
    useAppStore.setState({ processingConversationIds: ["conv-1"] });
    vi.spyOn(api, "listMessages").mockResolvedValueOnce([
      testMessage({
        id: "backend-user-active-stream",
        role: "user",
        content: "Prompt before active stream",
        createdAt: "2026-07-08T03:59:59.000Z"
      })
    ]);

    await useAppStore.getState().refreshChatData("conv-1", "persona-1");

    const state = useAppStore.getState();
    expect(state.messages.map((item) => item.id)).toEqual([
      "backend-user-active-stream",
      "assistant-active-stream"
    ]);
    expect(state.messages.find((item) => item.id === "assistant-active-stream")).toMatchObject({
      role: "assistant",
      source: "desktop-stream"
    });
    expect(state.streamedAssistantIds.has("assistant-active-stream")).toBe(true);
  });

  it("drops an orphan assistant stream during chat refresh when no active work remains", async () => {
    useAppStore.getState().upsertIncomingMessage(
      testMessage({
        id: "assistant-orphan-refresh-stream",
        role: "assistant",
        content: "Partial response from an interrupted turn",
        source: "desktop"
      }),
      { streaming: true }
    );
    vi.spyOn(api, "listMessages").mockResolvedValueOnce([
      testMessage({
        id: "backend-user-orphan-refresh",
        role: "user",
        content: "Prompt before interruption",
        createdAt: "2026-07-08T03:59:59.000Z"
      })
    ]);

    await useAppStore.getState().refreshChatData("conv-1", "persona-1");

    const state = useAppStore.getState();
    expect(state.messages.map((item) => item.id)).toEqual(["backend-user-orphan-refresh"]);
    expect(state.messages.some((item) => item.id === "assistant-orphan-refresh-stream")).toBe(false);
    expect(state.streamedAssistantIds.has("assistant-orphan-refresh-stream")).toBe(false);
  });

  it("merges agent run events, mock tool events, queue state, and abort/cancel terminal state into the UI store", () => {
    useAppStore.setState({
      agentQueue: [
        testQueuedRequest(),
        testQueuedRequest({ id: "queue-abort", content: "cancel me" })
      ]
    });

    useAppStore.getState().handleAgentRunEvent(testAgentRunEvent({
      state: "running",
      toolEvent: testToolEvent({ status: "running", ok: true, summary: "Reading README.md" }),
      phase: "tool_started",
      detail: { toolName: "read_file" }
    }));

    let state = useAppStore.getState();
    expect(state.activeAgentRuns["run-1"]).toMatchObject({
      runId: "run-1",
      state: "running",
      accumulatedToolEvents: [{ status: "running", toolName: "read_file" }]
    });
    expect(state.agentRuns[0]).toMatchObject({
      runId: "run-1",
      state: "running",
      toolEvents: [{ status: "running", toolName: "read_file" }]
    });
    expect(state.agentQueue.find((item) => item.id === "queue-1")).toMatchObject({
      status: "running",
      startedAt: TEST_NOW
    });

    useAppStore.getState().handleAgentRunEvent(testAgentRunEvent({
      state: "completed",
      toolEvent: testToolEvent({ status: "completed", ok: true, summary: "README.md loaded" }),
      phase: "completed",
      lastActivityDesc: "Done"
    }));

    state = useAppStore.getState();
    expect(state.activeAgentRuns["run-1"]).toBeUndefined();
    expect(state.agentRuns.find((run) => run.runId === "run-1")).toMatchObject({
      state: "completed",
      completedAt: TEST_NOW,
      toolEvents: [{ status: "completed", summary: "README.md loaded" }]
    });
    expect(state.agentQueue.find((item) => item.id === "queue-1")).toMatchObject({
      status: "completed",
      completedAt: TEST_NOW
    });

    useAppStore.getState().handleAgentRunEvent(testAgentRunEvent({
      runId: "run-abort",
      queueItemId: "queue-abort",
      state: "aborted",
      error: "Agent run stopped by user from chat.",
      toolEvent: testToolEvent({
        callId: "call-abort",
        status: "canceled",
        ok: false,
        summary: "Tool canceled",
        error: "Tool canceled"
      })
    }));

    state = useAppStore.getState();
    expect(state.activeAgentRuns["run-abort"]).toBeUndefined();
    expect(state.agentRuns.find((run) => run.runId === "run-abort")).toMatchObject({
      state: "aborted",
      error: "Agent run stopped by user from chat.",
      toolEvents: [{ status: "canceled", ok: false }]
    });
    expect(state.agentQueue.find((item) => item.id === "queue-abort")).toMatchObject({
      status: "aborted",
      error: "Agent run stopped by user from chat."
    });
  });
});
