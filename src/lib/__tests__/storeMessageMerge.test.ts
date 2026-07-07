import { describe, expect, it, beforeEach } from "vitest";
import { __chatStoreTestUtils } from "../store";
import type { ChatMessage } from "../types";

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
});
