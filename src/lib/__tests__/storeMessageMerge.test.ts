import { describe, expect, it, beforeEach } from "vitest";
import { __chatStoreTestUtils, useAppStore } from "../store";
import type { ChatMessage } from "../types";
import { testMessage } from "./chatTestHarness";

function message(partial: Partial<ChatMessage> & Pick<ChatMessage, "id" | "role" | "content">): ChatMessage {
  return {
    conversationId: "conv-1",
    createdAt: "2026-07-07T08:00:00.000Z",
    accountId: null,
    ...partial
  };
}

function merge(
  backendMessages: ChatMessage[],
  currentMessages: ChatMessage[],
  limit = 20
) {
  return __chatStoreTestUtils.mergeBackendMessagesWithLiveState(
    backendMessages,
    currentMessages,
    "conv-1",
    limit,
    new Set<string>()
  );
}

describe("chat store message merge", () => {
  beforeEach(() => {
    __chatStoreTestUtils.resetPendingIncomingMessagesForTests();
    useAppStore.setState({
      activeConversationId: "conv-1",
      messages: [],
      streamedAssistantIds: new Set<string>(),
      conversationMessageLimits: {}
    });
  });

  it("keeps a local desktop user message when a refresh returns a stale backend snapshot", () => {
    const localUser = message({
      id: "local-user-1",
      role: "user",
      content: "帮我整理今天新闻",
      source: "desktop",
      providerData: { clientMessageId: "local-user-1" }
    });
    const oldAssistant = message({
      id: "assistant-old",
      role: "assistant",
      content: "上一轮回复",
      source: "desktop",
      createdAt: "2026-07-07T07:59:00.000Z"
    });

    const merged = merge([oldAssistant], [localUser]);

    expect(merged.map((item) => item.id)).toEqual(["assistant-old", "local-user-1"]);
  });

  it("replaces a local user echo once the persisted backend user message arrives", () => {
    const localUser = message({
      id: "local-user-1",
      role: "user",
      content: "生成一张海边图片",
      source: "desktop",
      providerData: { clientMessageId: "client-1" }
    });
    const backendUser = message({
      id: "backend-user-1",
      role: "user",
      content: "生成一张海边图片",
      source: "desktop",
      providerData: { clientMessageId: "client-1" },
      createdAt: "2026-07-07T08:00:01.000Z"
    });
    const assistant = message({
      id: "assistant-1",
      role: "assistant",
      content: "图片已生成。",
      source: "desktop",
      createdAt: "2026-07-07T08:00:02.000Z"
    });

    const merged = merge([backendUser, assistant], [localUser]);

    expect(merged.map((item) => item.id)).toEqual(["backend-user-1", "assistant-1"]);
  });

  it("keeps a live wechat user message while backend persistence is still catching up", () => {
    const wechatUser = message({
      id: "wechat-live-user",
      role: "user",
      content: "整理新闻",
      source: "wechat",
      accountId: "wechat-account",
      providerData: { source: "wechat", accountId: "wechat-account", userId: "wechat-user" }
    });
    const oldAssistant = message({
      id: "assistant-old",
      role: "assistant",
      content: "上一轮微信回复",
      source: "wechat",
      createdAt: "2026-07-07T07:58:00.000Z"
    });

    const merged = merge([oldAssistant], [wechatUser]);

    expect(merged.map((item) => item.id)).toEqual(["assistant-old", "wechat-live-user"]);
  });

  it("carries pending inactive-conversation messages into the active conversation refresh and prunes after backend confirmation", () => {
    const pendingWechatUser = message({
      id: "wechat-pending-user",
      role: "user",
      content: "帮我查一下天气",
      source: "wechat",
      accountId: "wechat-account",
      providerData: { source: "wechat", accountId: "wechat-account", userId: "wechat-user" }
    });
    __chatStoreTestUtils.rememberPendingIncomingMessage(pendingWechatUser);

    const staleMerge = merge([], []);
    expect(staleMerge.map((item) => item.id)).toEqual(["wechat-pending-user"]);

    const backendUser = message({
      id: "backend-wechat-user",
      role: "user",
      content: "帮我查一下天气",
      source: "wechat",
      accountId: "wechat-account",
      providerData: { source: "wechat", accountId: "wechat-account", userId: "wechat-user" },
      createdAt: "2026-07-07T08:00:01.000Z"
    });
    const confirmedMerge = merge([backendUser], []);
    expect(confirmedMerge.map((item) => item.id)).toEqual(["backend-wechat-user"]);
  });

  it("keeps a live assistant stream through stale refresh and converges on the final message", () => {
    const stream = testMessage({
      id: "assistant-stream-1",
      role: "assistant",
      content: "Partial",
      source: "desktop"
    });
    useAppStore.getState().upsertIncomingMessage(stream, { streaming: true });

    const streamingState = useAppStore.getState();
    expect(streamingState.messages).toHaveLength(1);
    expect(streamingState.messages[0]).toMatchObject({
      id: "assistant-stream-1",
      content: "Partial",
      source: "desktop-stream"
    });
    expect(streamingState.streamedAssistantIds.has("assistant-stream-1")).toBe(true);

    const staleBackend = [
      testMessage({
        id: "backend-user-1",
        role: "user",
        content: "Prompt",
        createdAt: "2026-07-08T03:59:59.000Z"
      })
    ];
    const staleMergeResult = __chatStoreTestUtils.mergeBackendMessagesWithLiveStateResult(
      staleBackend,
      streamingState.messages,
      "conv-1",
      20,
      streamingState.streamedAssistantIds,
      { preserveStreamingAssistantMessages: true }
    );
    const staleMerged = staleMergeResult.messages;
    expect(staleMerged.map((item) => item.id)).toEqual(["backend-user-1", "assistant-stream-1"]);
    expect(staleMerged.find((item) => item.id === "assistant-stream-1")?.source).toBe("desktop-stream");
    expect(staleMergeResult.streamedAssistantIds.has("assistant-stream-1")).toBe(true);

    useAppStore.getState().upsertIncomingMessage(
      testMessage({
        id: "assistant-stream-1",
        role: "assistant",
        content: "Partial final answer",
        source: "desktop"
      }),
      { final: true }
    );

    const finalState = useAppStore.getState();
    expect(finalState.messages.filter((item) => item.id === "assistant-stream-1")).toHaveLength(1);
    expect(finalState.messages[0]).toMatchObject({
      id: "assistant-stream-1",
      content: "Partial final answer",
      source: "desktop"
    });
    expect(finalState.streamedAssistantIds.has("assistant-stream-1")).toBe(false);
  });

  it("drops an orphan live assistant stream during stale refresh when no active work remains", () => {
    useAppStore.getState().upsertIncomingMessage(
      testMessage({
        id: "assistant-orphan-stream",
        role: "assistant",
        content: "Partial answer from an interrupted run",
        source: "desktop"
      }),
      { streaming: true }
    );

    const streamingState = useAppStore.getState();
    expect(streamingState.messages).toMatchObject([
      {
        id: "assistant-orphan-stream",
        role: "assistant",
        source: "desktop-stream"
      }
    ]);
    expect(streamingState.streamedAssistantIds.has("assistant-orphan-stream")).toBe(true);

    const staleBackend = [
      testMessage({
        id: "backend-user-orphan",
        role: "user",
        content: "Prompt before interruption",
        createdAt: "2026-07-08T03:59:59.000Z"
      })
    ];
    const mergeResult = __chatStoreTestUtils.mergeBackendMessagesWithLiveStateResult(
      staleBackend,
      streamingState.messages,
      "conv-1",
      20,
      streamingState.streamedAssistantIds,
      { preserveStreamingAssistantMessages: false }
    );
    useAppStore.setState({
      messages: mergeResult.messages,
      streamedAssistantIds: mergeResult.streamedAssistantIds
    });

    const refreshedState = useAppStore.getState();
    expect(refreshedState.messages.map((item) => item.id)).toEqual(["backend-user-orphan"]);
    expect(refreshedState.messages.some((item) => item.id === "assistant-orphan-stream")).toBe(false);
    expect(refreshedState.streamedAssistantIds.has("assistant-orphan-stream")).toBe(false);
  });

  it("clears a failed assistant stream before stale refresh can preserve a ghost bubble", () => {
    useAppStore.getState().upsertIncomingMessage(
      testMessage({
        id: "assistant-failed-stream",
        role: "assistant",
        content: "Partial answer before provider failure",
        source: "desktop"
      }),
      { streaming: true }
    );

    const streamingState = useAppStore.getState();
    expect(streamingState.messages).toMatchObject([
      {
        id: "assistant-failed-stream",
        role: "assistant",
        source: "desktop-stream"
      }
    ]);
    expect(streamingState.streamedAssistantIds.has("assistant-failed-stream")).toBe(true);

    useAppStore.getState().clearStreamingAssistantMessages("conv-1");

    const clearedState = useAppStore.getState();
    expect(clearedState.messages.some((item) => item.id === "assistant-failed-stream")).toBe(false);
    expect(clearedState.streamedAssistantIds.has("assistant-failed-stream")).toBe(false);

    const staleMerged = __chatStoreTestUtils.mergeBackendMessagesWithLiveState(
      [],
      clearedState.messages,
      "conv-1",
      20,
      clearedState.streamedAssistantIds
    );
    expect(staleMerged.some((item) => item.id === "assistant-failed-stream")).toBe(false);
  });

  it("clears a failed pending assistant stream before inactive conversation refresh", () => {
    useAppStore.setState({ activeConversationId: "other-conv" });
    useAppStore.getState().upsertIncomingMessage(
      testMessage({
        id: "assistant-pending-failed-stream",
        role: "assistant",
        content: "Partial inactive answer before provider failure",
        source: "desktop"
      }),
      { streaming: true }
    );

    const pendingState = useAppStore.getState();
    expect(pendingState.messages).toHaveLength(0);
    expect(pendingState.streamedAssistantIds.has("assistant-pending-failed-stream")).toBe(true);
    expect(
      __chatStoreTestUtils.mergeBackendMessagesWithLiveState(
        [],
        pendingState.messages,
        "conv-1",
        20,
        pendingState.streamedAssistantIds
      ).map((item) => [item.id, item.source])
    ).toEqual([["assistant-pending-failed-stream", "desktop-stream"]]);

    useAppStore.getState().clearStreamingAssistantMessages("conv-1");

    const clearedState = useAppStore.getState();
    expect(clearedState.streamedAssistantIds.has("assistant-pending-failed-stream")).toBe(false);
    expect(
      __chatStoreTestUtils.mergeBackendMessagesWithLiveState(
        [],
        clearedState.messages,
        "conv-1",
        20,
        clearedState.streamedAssistantIds
      )
    ).toEqual([]);
  });
});
