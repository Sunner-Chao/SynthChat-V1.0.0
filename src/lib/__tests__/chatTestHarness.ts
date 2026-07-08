import type {
  AgentQueuedRequest,
  AgentRunEvent,
  ChatMessage,
  Conversation,
  SendChatRequest,
  ToolEvent
} from "../types";

export const TEST_NOW = "2026-07-08T04:00:00.000Z";

export function testMessage(
  partial: Partial<ChatMessage> & Pick<ChatMessage, "id" | "role" | "content">
): ChatMessage {
  return {
    conversationId: "conv-1",
    createdAt: TEST_NOW,
    accountId: null,
    source: "desktop",
    ...partial
  };
}

export function testConversation(partial: Partial<Conversation> = {}): Conversation {
  return {
    id: "conv-1",
    title: "Test conversation",
    personaId: "persona-1",
    agentId: "agent-conv",
    lastMessage: "",
    updatedAt: TEST_NOW,
    ...partial
  };
}

export function deterministicReplyText(content: string) {
  return `deterministic reply: ${content}`;
}

export function deterministicChatResponse(
  request: SendChatRequest,
  options: {
    userId?: string;
    assistantId?: string;
    assistantContent?: string;
    createdAt?: string;
  } = {}
): ChatMessage[] {
  const conversationId = request.conversationId ?? "conv-1";
  const createdAt = options.createdAt ?? TEST_NOW;
  const clientMessageId = request.providerData && typeof request.providerData === "object"
    ? (request.providerData as { clientMessageId?: unknown }).clientMessageId
    : undefined;
  return [
    testMessage({
      id: options.userId ?? "backend-user-1",
      conversationId,
      role: "user",
      content: request.content,
      createdAt,
      providerData: {
        source: "desktop",
        clientMessageId
      }
    }),
    testMessage({
      id: options.assistantId ?? "assistant-1",
      conversationId,
      role: "assistant",
      content: options.assistantContent ?? deterministicReplyText(request.content),
      createdAt,
      source: "desktop"
    })
  ];
}

export function testToolEvent(partial: Partial<ToolEvent> = {}): ToolEvent {
  return {
    eventType: "tool_completed",
    serverId: "__internal",
    toolName: "read_file",
    callId: "call-1",
    referenceId: "ref-1",
    runId: "run-1",
    checkpointId: null,
    status: "completed",
    ok: true,
    timedOut: false,
    elapsedMs: 42,
    kind: "read",
    title: "Read file",
    summary: "Read file completed",
    path: "README.md",
    text: "file contents",
    error: null,
    raw: { payload: { path: "README.md" } },
    ...partial
  };
}

export function testAgentRunEvent(partial: Partial<AgentRunEvent> = {}): AgentRunEvent {
  return {
    runId: "run-1",
    conversationId: "conv-1",
    personaId: "persona-1",
    agentId: "agent-conv",
    queueItemId: "queue-1",
    state: "running",
    updatedAt: TEST_NOW,
    lastActivityAt: TEST_NOW,
    lastActivityDesc: "Running",
    ...partial
  };
}

export function testQueuedRequest(partial: Partial<AgentQueuedRequest> = {}): AgentQueuedRequest {
  return {
    id: "queue-1",
    conversationId: "conv-1",
    personaId: "persona-1",
    userMessageId: "user-1",
    content: "test request",
    status: "pending",
    createdAt: TEST_NOW,
    updatedAt: TEST_NOW,
    startedAt: null,
    completedAt: null,
    error: null,
    ...partial
  };
}
