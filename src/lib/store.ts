import { create } from "zustand";
import { emit, emitTo } from "@tauri-apps/api/event";
import { api } from "./api";
import { forgetLocalImagePreview, rememberLocalImagePreview } from "./localImagePreview";
import {
  WORKFLOW_GRAPH_SCHEMA,
  WORKFLOW_PHASE_INITIALIZED,
  WORKFLOW_PHASE_NODE,
  WORKFLOW_PHASE_TRANSITION,
  agentRunWorkflowGraph,
  workflowNodeRoleLabel
} from "./types";
import type {
  AgentDefinition,
  AgentQueuedRequest,
  AgentRunEvent,
  AgentRunRecord,
  AppConfig,
  AgentConfig,
  AccountConfig,
  AppSection,
  BrowserProvider,
  CapabilityAdapter,
  ChatMessage,
  Conversation,
  ConversationDeleteMemorySettlingResult,
  EmojiGroup,
  EnhancedSkillSummary,
  ImageProvider,
  LlmProvider,
  MarketplaceSkill,
  ManagedProcessEvent,
  MemoryEntry,
  McpCallResult,
  McpListToolsResult,
  McpServer,
  MomentPost,
  Persona,
  PluginSummary,
  ProactiveStatus,
  ProfileConfig,
  SearchProvider,
  SkillBundle,
  ThemeConfig,
  ToolEvent,
  WorkflowGraph,
  WorkflowGraphNode,
  WorkflowGraphTransition,
  SkillSummary,
  VideoProvider,
  VisionProvider,
  Worldbook
} from "./types";

const DEFAULT_UI_MESSAGE_LIMIT = 180;
const MIN_UI_MESSAGE_LIMIT = 40;
const MAX_UI_MESSAGE_LIMIT = 1000;
const DEFAULT_UI_MESSAGE_PREVIEW_CHARS = 12_000;
const MAX_TOOL_EVENT_UI_PREVIEW_CHARS = 6_000;
const MAX_TOOL_EVENT_RAW_UI_PREVIEW_CHARS = 2_000;
const MAX_TOOL_EVENT_UI_JSON_DEPTH = 8;
const MAX_TOOL_EVENT_UI_ARRAY_ITEMS = 40;
const MAX_THINKING_CARD_UI_SUMMARY_CHARS = 6_000;
const BOOTSTRAP_CACHE_STORAGE_KEY = "synthchat.bootstrap.cache.v1";
const TERMINAL_AGENT_STATES = new Set(["completed", "failed", "aborted"]);
const ACTIVE_QUEUE_STATES = new Set(["pending", "running"]);

// Module-level ref for pending settings view (not in React state to avoid batching delays)
let pendingSettingsViewRef: string | null = null;
let profileMutationVersion = 0;
let personaMutationVersion = 0;

// Grace window guarding against a refresh clearing a "processing" flag that was
// just set (e.g. WeChat/pet emits a processing event before the user message is
// persisted, so a concurrent refresh still sees a stale assistant tail).
const PROCESSING_MARK_GRACE_MS = 1500;
const processingMarkedAtCache = new Map<string, number>();
const processingClearTimerCache = new Map<string, number>();
const pendingIncomingMessagesByConversation = new Map<string, ChatMessage[]>();
const refreshChatDataInFlight = new Map<string, Promise<void>>();

function scheduleBackgroundStoreRefresh(
  label: string,
  task: () => Promise<unknown>,
  delayMs = 180
) {
  const run = () => {
    void Promise.resolve()
      .then(task)
      .catch((error) => {
        console.warn(`${label} failed`, error);
      });
  };
  if (typeof window === "undefined") {
    run();
    return;
  }
  window.setTimeout(run, Math.max(0, delayMs));
}

function fileToDataUrl(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(typeof reader.result === "string" ? reader.result : "");
    reader.onerror = () => reject(reader.error ?? new Error("读取图片文件失败"));
    reader.readAsDataURL(file);
  });
}

function withinProcessingGrace(conversationId: string | null): boolean {
  if (!conversationId) return false;
  const markedAt = processingMarkedAtCache.get(conversationId);
  return markedAt !== undefined && Date.now() - markedAt < PROCESSING_MARK_GRACE_MS;
}

function emitPetThinkingEvent(
  type: "thinking_started" | "thinking_finished",
  conversationId: string | null | undefined,
  personaId: string | null | undefined,
  ok?: boolean,
  message?: ChatMessage | null
) {
  if (!conversationId) return;
  const payload = {
    type,
    source: "desktop",
    personaId: personaId ?? null,
    conversationId,
    ok,
    message: message ?? undefined
  };
  void emit("synthchat-pet-event", payload).catch(() => undefined);
  void emitTo("pet", "synthchat-pet-event", payload).catch(() => undefined);
}

export function consumePendingSettingsView(): string | null {
  const v = pendingSettingsViewRef;
  pendingSettingsViewRef = null;
  return v;
}

type BootstrapCacheSnapshot = {
  config: AppConfig | null;
  profile: ProfileConfig;
  llmProviders: LlmProvider[];
  imageProviders: ImageProvider[];
  videoProviders: VideoProvider[];
  searchProviders: SearchProvider[];
  visionProviders: VisionProvider[];
  browserProviders: BrowserProvider[];
  themes: ThemeConfig[];
  emojiGroups: EmojiGroup[];
  accounts: AccountConfig[];
  personas: Persona[];
};

function readBootstrapCache(): BootstrapCacheSnapshot | null {
  if (typeof window === "undefined") return null;
  try {
    const raw = window.localStorage.getItem(BOOTSTRAP_CACHE_STORAGE_KEY);
    if (!raw) return null;
    return JSON.parse(raw) as BootstrapCacheSnapshot;
  } catch {
    return null;
  }
}

function writeBootstrapCache(snapshot: BootstrapCacheSnapshot) {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(BOOTSTRAP_CACHE_STORAGE_KEY, JSON.stringify(snapshot));
  } catch {
    // ignore cache write failures
  }
}

const bootstrapCache = readBootstrapCache();

function withBootstrapTimeout<T>(promise: Promise<T>, fallback: T, label: string, timeoutMs = 5000): Promise<T> {
  let timeoutId: number | null = null;
  const timeoutPromise = new Promise<T>((resolve) => {
    timeoutId = window.setTimeout(() => {
      console.warn(`${label} timed out during bootstrap; using fallback`);
      resolve(fallback);
    }, timeoutMs);
  });
  return Promise.race([
    promise
      .then((value) => {
        if (timeoutId !== null) window.clearTimeout(timeoutId);
        return value;
      })
      .catch((error) => {
        if (timeoutId !== null) window.clearTimeout(timeoutId);
        console.warn(`${label} failed during bootstrap`, error);
        return fallback;
      }),
    timeoutPromise
  ]);
}

function uiMessageLimit(config: AppConfig | null) {
  const configured = config?.chat.uiMessageLimit ?? DEFAULT_UI_MESSAGE_LIMIT;
  if (!Number.isFinite(configured)) return DEFAULT_UI_MESSAGE_LIMIT;
  return Math.min(MAX_UI_MESSAGE_LIMIT, Math.max(MIN_UI_MESSAGE_LIMIT, Math.floor(configured)));
}

function conversationMessageLimit(config: AppConfig | null, conversationId: string | null | undefined, overrides: Record<string, number>) {
  const baseLimit = uiMessageLimit(config);
  const configured = conversationId ? overrides[conversationId] : undefined;
  if (typeof configured !== "number" || !Number.isFinite(configured)) return baseLimit;
  return Math.min(MAX_UI_MESSAGE_LIMIT, Math.max(baseLimit, Math.floor(configured)));
}

function uiMessagePreviewChars(config: AppConfig | null) {
  const configured = config?.chat.uiMessagePreviewChars ?? DEFAULT_UI_MESSAGE_PREVIEW_CHARS;
  if (!Number.isFinite(configured)) return DEFAULT_UI_MESSAGE_PREVIEW_CHARS;
  return Math.min(100_000, Math.max(2_000, Math.floor(configured)));
}

function limitMessages(messages: ChatMessage[], limit: number) {
  if (messages.length <= limit) return messages;
  if (limit <= 0) return [];
  const extra = messages.length - limit;
  const protectedUser = [...messages.slice(0, extra)].reverse().find((message) =>
    message.role === "user"
    && message.source !== "proactive-internal"
    && message.content.trim().length > 0
  );
  const visible = messages.slice(extra);
  if (!protectedUser || visible.some((message) => message.id === protectedUser.id)) {
    return visible;
  }
  if (visible.length >= limit) {
    const removeIndex = visible.findIndex((message) => message.role === "tool");
    visible.splice(removeIndex >= 0 ? removeIndex : 0, 1);
  }
  return sortMessagesForDisplay([...visible, protectedUser]);
}

function refreshChatDataKey(preferredConversationId?: string | null, preferredPersonaId?: string | null) {
  return `${preferredConversationId ?? ""}\u0000${preferredPersonaId ?? ""}`;
}

function messageDisplayRoleRank(message: ChatMessage) {
  if (message.role === "user") return 0;
  if (message.role === "tool") return 1;
  if (message.role === "assistant") return 2;
  return 3;
}

function sortMessagesForDisplay(messages: ChatMessage[]) {
  return messages
    .map((message, index) => ({ message, index }))
    .sort((left, right) => {
      const timeDelta = messageTime(left.message) - messageTime(right.message);
      if (timeDelta !== 0) return timeDelta;
      const leftRoleRank = messageDisplayRoleRank(left.message);
      const rightRoleRank = messageDisplayRoleRank(right.message);
      const roleDelta = leftRoleRank - rightRoleRank;
      return roleDelta === 0 ? left.index - right.index : roleDelta;
    })
    .map((item) => item.message);
}

function displayMessages(messages: ChatMessage[], limit: number) {
  return limitMessages(sortMessagesForDisplay(messages), limit);
}

function messageProviderDataRecord(message: ChatMessage): Record<string, unknown> | null {
  const value = message.providerData;
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function providerDataRecord(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function truncateTextForUi(text: string, maxChars: number) {
  if (text.length <= maxChars) return null;
  return `${text.slice(0, maxChars)}\n\n[内容过长，界面仅预览前 ${maxChars} 个字符；完整内容仍保存在本地数据中。]`;
}

function truncateObjectStringForUi(object: Record<string, unknown>, key: string, maxChars: number) {
  const value = object[key];
  if (typeof value !== "string") return false;
  const preview = truncateTextForUi(value, maxChars);
  if (!preview) return false;
  object[key] = preview;
  return true;
}

function truncateJsonStringsForUi(value: unknown, maxChars: number, depth = 0): boolean {
  if (depth > MAX_TOOL_EVENT_UI_JSON_DEPTH) return false;
  if (typeof value === "string") return false;
  if (Array.isArray(value)) {
    let changed = false;
    if (value.length > MAX_TOOL_EVENT_UI_ARRAY_ITEMS) {
      const omitted = value.length - MAX_TOOL_EVENT_UI_ARRAY_ITEMS;
      value.splice(MAX_TOOL_EVENT_UI_ARRAY_ITEMS);
      value.push(`[UI preview truncated: omitted ${omitted} array item(s)]`);
      changed = true;
    }
    for (let i = 0; i < value.length; i++) {
      const item = value[i];
      if (typeof item === "string") {
        const preview = truncateTextForUi(item, maxChars);
        if (preview) {
          value[i] = preview;
          changed = true;
        }
      } else {
        changed = truncateJsonStringsForUi(item, maxChars, depth + 1) || changed;
      }
    }
    return changed;
  }
  const object = providerDataRecord(value);
  if (!object) return false;
  if (depth === MAX_TOOL_EVENT_UI_JSON_DEPTH) {
    const omitted = Object.keys(object).length;
    if (omitted === 0) return false;
    for (const key of Object.keys(object)) delete object[key];
    object.uiPreviewTruncated = `depth limit reached; omitted ${omitted} field(s)`;
    return true;
  }
  let changed = false;
  for (const [key, item] of Object.entries(object)) {
    if (typeof item === "string") {
      const preview = truncateTextForUi(item, maxChars);
      if (preview) {
        object[key] = preview;
        changed = true;
      }
    } else {
      changed = truncateJsonStringsForUi(item, maxChars, depth + 1) || changed;
    }
  }
  return changed;
}

function omitToolEventRawForUi(value: unknown) {
  const root = providerDataRecord(value);
  if (root?.type !== "toolEvent") return false;
  const event = providerDataRecord(root.event);
  if (!event || !("raw" in event)) return false;
  event.raw = {
    uiPreviewTruncated: true,
    reason: "raw payload omitted from chat UI preview"
  };
  return true;
}

function truncateJsonMessageContentForUi(content: string, previewChars: number) {
  let parsed: unknown;
  try {
    parsed = JSON.parse(content);
  } catch {
    return null;
  }
  const root = providerDataRecord(parsed);
  const isToolEvent = root?.type === "toolEvent";
  const toolLimit = Math.min(MAX_TOOL_EVENT_UI_PREVIEW_CHARS, previewChars);
  const rawLimit = Math.min(MAX_TOOL_EVENT_RAW_UI_PREVIEW_CHARS, toolLimit);
  let changed = false;
  if (isToolEvent) {
    changed = truncateObjectStringForUi(root!, "modelSummary", toolLimit) || changed;
    const event = providerDataRecord(root!.event);
    if (event) {
      changed = truncateObjectStringForUi(event, "summary", toolLimit) || changed;
      changed = truncateObjectStringForUi(event, "text", toolLimit) || changed;
      changed = truncateObjectStringForUi(event, "error", toolLimit) || changed;
      if ("raw" in event) {
        changed = truncateJsonStringsForUi(event.raw, rawLimit) || changed;
      }
    }
  } else {
    changed = truncateJsonStringsForUi(parsed, rawLimit);
  }
  const hardJsonLimit = toolLimit * 3;
  if (content.length > hardJsonLimit) {
    changed = omitToolEventRawForUi(parsed) || changed;
  }
  if (!changed) return null;
  let rendered = JSON.stringify(parsed);
  if (isToolEvent && rendered.length > hardJsonLimit && omitToolEventRawForUi(parsed)) {
    rendered = JSON.stringify(parsed);
  }
  return rendered;
}

function markMessageUiPreview(message: ChatMessage, originalChars: number, previewChars: number): ChatMessage {
  const providerData = providerDataRecord(message.providerData);
  return {
    ...message,
    providerData: {
      ...(providerData ?? {}),
      uiPreview: {
        truncated: true,
        originalChars,
        previewChars
      }
    }
  };
}

function cloneJsonValue<T>(value: T): T | null {
  if (value === undefined || value === null) return null;
  try {
    return JSON.parse(JSON.stringify(value)) as T;
  } catch {
    return null;
  }
}

function truncateThinkingCardArrayForUi(value: unknown, maxChars: number) {
  if (!Array.isArray(value)) return false;
  let changed = false;
  for (const item of value) {
    const card = providerDataRecord(item);
    if (!card) continue;
    if (truncateObjectStringForUi(card, "summary", maxChars)) {
      card.uiPreviewTruncated = true;
      changed = true;
    }
  }
  return changed;
}

function truncateProviderDataForUi(providerData: unknown, previewChars: number) {
  const root = providerDataRecord(providerData);
  if (!root) return null;
  const cloned = cloneJsonValue(root);
  const clonedRoot = providerDataRecord(cloned);
  if (!clonedRoot) return null;
  const maxChars = Math.min(MAX_THINKING_CARD_UI_SUMMARY_CHARS, previewChars);
  let changed = truncateThinkingCardArrayForUi(clonedRoot.thinkingCards, maxChars);
  changed = truncateThinkingCardArrayForUi(providerDataRecord(clonedRoot.responses)?.thinkingCards, maxChars) || changed;
  changed = truncateThinkingCardArrayForUi(providerDataRecord(clonedRoot.anthropic)?.thinkingCards, maxChars) || changed;
  return changed ? clonedRoot : null;
}

function previewMessageForUi(message: ChatMessage, previewChars: number): ChatMessage {
  const originalChars = message.content.length;
  const content = message.role === "tool"
    ? truncateJsonMessageContentForUi(message.content, previewChars)
      ?? truncateTextForUi(message.content, Math.min(MAX_TOOL_EVENT_UI_PREVIEW_CHARS, previewChars))
    : truncateTextForUi(message.content, previewChars);
  const providerData = truncateProviderDataForUi(message.providerData, previewChars);
  if (!content && !providerData) return message;
  const nextMessage = {
    ...message,
    ...(content ? { content } : {}),
    ...(providerData ? { providerData } : {})
  };
  return markMessageUiPreview(nextMessage, originalChars, nextMessage.content.length);
}

function providerDataArray(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

function providerDataThinkingCards(providerData: unknown): unknown[] {
  const root = providerDataRecord(providerData);
  if (!root) return [];
  return [
    ...providerDataArray(root.thinkingCards),
    ...providerDataArray(providerDataRecord(root.responses)?.thinkingCards),
    ...providerDataArray(providerDataRecord(root.anthropic)?.thinkingCards)
  ];
}

function finalizedThinkingCards(cards: unknown[]) {
  return cards.map((card) => {
    const record = providerDataRecord(card);
    return record ? { ...record, streaming: false } : card;
  });
}

function preserveLiveThinkingCardsForFinalMessage(
  message: ChatMessage,
  previousMessage: ChatMessage | null,
  options?: IncomingMessageUpsertOptions
) {
  if (!options?.final || message.role !== "assistant") return message;
  if (providerDataThinkingCards(message.providerData).length > 0) return message;
  if (!previousMessage) return message;
  const liveCards = providerDataThinkingCards(previousMessage.providerData);
  if (liveCards.length === 0) return message;
  const root = providerDataRecord(message.providerData);
  return {
    ...message,
    providerData: {
      ...(root ?? {}),
      thinkingCards: finalizedThinkingCards(liveCards)
    }
  };
}

function isSilentPetOnlyMessage(message: ChatMessage) {
  const providerData = messageProviderDataRecord(message);
  return providerData?.silent === true
    && (message.source === "pet-vision" || providerData.source === "pet-vision" || providerData.visibility === "pet-only");
}

function isVisibleChatMessage(message: ChatMessage) {
  if (isSilentPetOnlyMessage(message)) return false;
  return !(message.role === "user" && message.source === "proactive-internal");
}

function isAgentErrorMessage(message: ChatMessage) {
  return message.source === "desktop-agent-error";
}

function visibleChatMessages(messages: ChatMessage[]) {
  return sortMessagesForDisplay(messages.filter(isVisibleChatMessage));
}

function rememberPendingIncomingMessage(message: ChatMessage) {
  if (!message.conversationId || !isVisibleChatMessage(message)) return;
  const pending = pendingIncomingMessagesByConversation.get(message.conversationId) ?? [];
  const next = pending.filter((item) => item.id !== message.id);
  next.push(message);
  pendingIncomingMessagesByConversation.set(message.conversationId, next.slice(-80));
}

function pendingIncomingMessagesForConversation(conversationId: string | null | undefined) {
  return conversationId ? pendingIncomingMessagesByConversation.get(conversationId) ?? [] : [];
}

function mergeUniqueMessagesById(messages: ChatMessage[]) {
  const byId = new Map<string, ChatMessage>();
  for (const message of messages) byId.set(message.id, message);
  return Array.from(byId.values());
}

function prunePendingIncomingMessages(conversationId: string | null | undefined, backendMessages: ChatMessage[]) {
  if (!conversationId) return;
  const pending = pendingIncomingMessagesByConversation.get(conversationId);
  if (!pending || pending.length === 0) return;
  const backendIds = new Set(backendMessages.map((message) => message.id));
  const unresolved = pending.filter((message) =>
    !backendIds.has(message.id)
    && !backendMessages.some((backend) => matchesPersistedUserMessage(message, backend))
  );
  if (unresolved.length > 0) {
    pendingIncomingMessagesByConversation.set(conversationId, unresolved.slice(-80));
  } else {
    pendingIncomingMessagesByConversation.delete(conversationId);
  }
}

function isLocalUiMessage(message: ChatMessage) {
  return message.id.startsWith("local-");
}

function isLocalStatusMessage(message: ChatMessage) {
  return message.source?.startsWith("desktop-local-") ?? false;
}

interface IncomingMessageUpsertOptions {
  streaming?: boolean;
  final?: boolean;
}

function messageTime(message: ChatMessage) {
  const timestamp = Date.parse(message.createdAt);
  return Number.isFinite(timestamp) ? timestamp : 0;
}

function normalizeMessageContentForMatch(content: string) {
  return content.replace(/\r\n/g, "\n").trim();
}

function providerDataString(value: unknown, keys: string[]) {
  const record = providerDataRecord(value);
  if (!record) return "";
  for (const key of keys) {
    const candidate = record[key];
    if (typeof candidate === "string" && candidate.trim()) return candidate.trim();
  }
  return "";
}

function messageClientMessageId(message: ChatMessage) {
  return providerDataString(message.providerData, ["clientMessageId", "client_message_id"]);
}

function matchesPersistedUserMessage(liveMessage: ChatMessage, backendMessage: ChatMessage) {
  if (liveMessage.role !== "user" || backendMessage.role !== "user") {
    return false;
  }
  const liveClientId = messageClientMessageId(liveMessage);
  const backendClientId = messageClientMessageId(backendMessage);
  if (liveClientId && backendClientId && liveClientId === backendClientId) {
    return true;
  }
  const liveContent = normalizeMessageContentForMatch(liveMessage.content);
  if (!liveContent || liveContent !== normalizeMessageContentForMatch(backendMessage.content)) {
    return false;
  }
  const liveSource = liveMessage.source?.trim() ?? "";
  const backendSource = backendMessage.source?.trim() ?? "";
  if (liveSource && backendSource && liveSource !== backendSource) {
    return false;
  }
  const liveAccountId = liveMessage.accountId ?? null;
  const backendAccountId = backendMessage.accountId ?? null;
  if (liveAccountId && backendAccountId && liveAccountId !== backendAccountId) {
    return false;
  }
  const liveCreatedAt = messageTime(liveMessage);
  const backendCreatedAt = messageTime(backendMessage);
  return !(
    liveCreatedAt > 0
    && backendCreatedAt > 0
    && Math.abs(backendCreatedAt - liveCreatedAt) > 120_000
  );
}

function matchesLocalUserReplacement(liveMessage: ChatMessage, backendMessage: ChatMessage) {
  if (!matchesPersistedUserMessage(liveMessage, backendMessage)) {
    return false;
  }
  const liveCreatedAt = messageTime(liveMessage);
  const backendCreatedAt = messageTime(backendMessage);
  return !(
    liveCreatedAt > 0
    && backendCreatedAt > 0
    && backendCreatedAt < liveCreatedAt - 5_000
  );
}

function isTrackedStreamingAssistantMessage(
  message: ChatMessage,
  conversationId: string | null,
  streamedAssistantIds: Set<string>
) {
  return Boolean(
    conversationId
    && message.conversationId === conversationId
    && message.role === "assistant"
    && streamedAssistantIds.has(message.id)
    && isVisibleChatMessage(message)
    && !isLocalUiMessage(message)
    && !isLocalStatusMessage(message)
  );
}

function shouldPreferLiveStreamingAssistant(
  liveMessage: ChatMessage,
  backendMessage: ChatMessage,
  conversationId: string | null,
  streamedAssistantIds: Set<string>
) {
  if (!isTrackedStreamingAssistantMessage(liveMessage, conversationId, streamedAssistantIds)) return false;
  if (backendMessage.role !== "assistant") return false;
  if (!backendMessage.content && liveMessage.content) return true;
  return (
    liveMessage.content.length > backendMessage.content.length
    && liveMessage.content.startsWith(backendMessage.content)
  );
}

function mergeLocalUiMessages(
  backendMessages: ChatMessage[],
  currentMessages: ChatMessage[],
  conversationId: string | null,
  limit: number,
  streamedAssistantIds: Set<string> = new Set()
) {
  if (!conversationId) return displayMessages(backendMessages, limit);
  const currentById = new Map(currentMessages.map((message) => [message.id, message]));
  const backendMessagesWithLiveStreams = backendMessages.map((backend) => {
    const live = currentById.get(backend.id);
    return live && shouldPreferLiveStreamingAssistant(live, backend, conversationId, streamedAssistantIds)
      ? live
      : backend;
  });
  const backendIds = new Set(backendMessagesWithLiveStreams.map((message) => message.id));
  const localMessages = currentMessages.filter((message) => {
    if (message.conversationId !== conversationId || !isLocalUiMessage(message) || backendIds.has(message.id)) {
      return false;
    }
    if (message.role === "user") {
      return !backendMessages.some((backend) => matchesLocalUserReplacement(message, backend));
    }
    if (isLocalStatusMessage(message)) {
      const localCreatedAt = messageTime(message);
      return !backendMessages.some((backend) => backend.role === "assistant" && messageTime(backend) >= localCreatedAt - 1000);
    }
    return false;
  });
  const streamingAssistantMessages = currentMessages.filter((message) =>
    isTrackedStreamingAssistantMessage(message, conversationId, streamedAssistantIds)
    && !backendIds.has(message.id)
  );
  const transientLiveMessages = currentMessages.filter((message) => {
    if (message.conversationId !== conversationId || backendIds.has(message.id)) {
      return false;
    }
    if (!isVisibleChatMessage(message) || isLocalUiMessage(message) || isLocalStatusMessage(message)) {
      return false;
    }
    if (message.role !== "user") {
      return false;
    }
    if (message.source !== "pet" && message.source !== "wechat") {
      return false;
    }
    return !backendMessages.some((backend) => matchesPersistedUserMessage(message, backend));
  });
  if (localMessages.length === 0 && transientLiveMessages.length === 0 && streamingAssistantMessages.length === 0) {
    return displayMessages(backendMessagesWithLiveStreams, limit);
  }
  return displayMessages(
    [...backendMessagesWithLiveStreams, ...localMessages, ...transientLiveMessages, ...streamingAssistantMessages],
    limit
  );
}

function mergeBackendMessagesWithLiveState(
  backendMessages: ChatMessage[],
  currentMessages: ChatMessage[],
  conversationId: string | null,
  limit: number,
  streamedAssistantIds: Set<string>
) {
  const pending = pendingIncomingMessagesForConversation(conversationId);
  const liveMessages = pending.length > 0
    ? mergeUniqueMessagesById([...currentMessages, ...pending])
    : currentMessages;
  let messages = mergeLocalUiMessages(
    backendMessages,
    liveMessages,
    conversationId,
    limit,
    streamedAssistantIds
  );
  if (pending.length > 0) {
    const messageIds = new Set(messages.map((message) => message.id));
    const backendIds = new Set(backendMessages.map((message) => message.id));
    const missingPending = pending.filter((message) =>
      !messageIds.has(message.id)
      && !backendIds.has(message.id)
      && !backendMessages.some((backend) => matchesPersistedUserMessage(message, backend))
    );
    if (missingPending.length > 0) {
      messages = displayMessages([...messages, ...missingPending], limit);
    }
  }
  prunePendingIncomingMessages(conversationId, backendMessages);
  return messages;
}

export const __chatStoreTestUtils = {
  mergeBackendMessagesWithLiveState,
  rememberPendingIncomingMessage,
  resetPendingIncomingMessagesForTests: () => pendingIncomingMessagesByConversation.clear()
};

function hasPendingAgentWork(state: AppState, conversationId: string | null) {
  if (!conversationId) return false;
  return Object.values(state.activeAgentRuns).some((run) =>
    run.conversationId === conversationId
    && !run.parentRunId
    && !TERMINAL_AGENT_STATES.has(run.state)
  )
    || state.agentQueue.some((item) =>
      item.conversationId === conversationId
      && ACTIVE_QUEUE_STATES.has(item.status)
    )
    || state.agentRuns.some((run) =>
      run.conversationId === conversationId
      && !run.parentRunId
      && !TERMINAL_AGENT_STATES.has(run.state)
    );
}

function sameConversations(left: Conversation[], right: Conversation[]) {
  return left.length === right.length && left.every((item, index) => {
    const other = right[index];
    return Boolean(other)
      && item.id === other.id
      && item.title === other.title
      && item.updatedAt === other.updatedAt
      && item.lastMessage === other.lastMessage
      && item.personaId === other.personaId
      && item.agentId === other.agentId
      && item.wechatAccountId === other.wechatAccountId;
  });
}

function normalizeFocusedAgentId(agents: AgentDefinition[], preferred?: string | null) {
  const trimmed = preferred?.trim() ?? "";
  if (trimmed && agents.some((agent) => agent.id === trimmed)) return trimmed;
  return agents.find((agent) => agent.isDefault)?.id ?? agents[0]?.id ?? null;
}

function upsertAgentPreservingOrder(agents: AgentDefinition[], agent: AgentDefinition) {
  const existingIndex = agents.findIndex((item) => item.id === agent.id);
  const normalized = agent.isDefault
    ? agents.map((item) => item.id === agent.id ? item : { ...item, isDefault: false })
    : agents;
  if (existingIndex >= 0) {
    return normalized.map((item, index) => index === existingIndex ? agent : item);
  }
  return [...normalized, agent];
}

function sortAgents(agents: AgentDefinition[]) {
  return agents
    .slice()
    .sort((a, b) => (b.isDefault ? 1 : 0) - (a.isDefault ? 1 : 0));
}

function sortPersonas(personas: Persona[]) {
  return personas
    .slice()
    .sort((a, b) => a.name.localeCompare(b.name));
}

function clampToolIterations(value: number | undefined | null) {
  const numeric = Number(value);
  if (!Number.isFinite(numeric)) return 90;
  return Math.min(90, Math.max(1, Math.round(numeric)));
}

function boundAgentId(persona: Persona) {
  return persona.agentId?.trim() ?? "";
}

function personaWithAgentRuntime(persona: Persona, agent: AgentDefinition): Persona {
  return {
    ...persona,
    llmProvider: agent.llmProvider,
    llmModel: agent.llmModel,
    toolPolicy: {
      ...persona.toolPolicy,
      maxIterations: clampToolIterations(agent.maxToolIterations)
    }
  };
}

function agentWithPersonaRuntime(agent: AgentDefinition, persona: Persona): AgentDefinition {
  return {
    ...agent,
    llmProvider: persona.llmProvider,
    llmModel: persona.llmModel,
    maxToolIterations: clampToolIterations(persona.toolPolicy?.maxIterations)
  };
}

function sameMessages(left: ChatMessage[], right: ChatMessage[]) {
  return left.length === right.length && left.every((item, index) => {
    const other = right[index];
    return Boolean(other)
      && item.id === other.id
      && item.role === other.role
      && item.content === other.content
      && item.createdAt === other.createdAt
      && item.source === other.source
      && item.accountId === other.accountId;
  });
}

function parseNewConversationCommand(content: string): string | null | undefined {
  const body = content
    .trim()
    .replace(/^[/／]/, "")
    .trim();
  if (!body) return undefined;
  const parts = body.split(/\s+/);
  const command = parts.shift()?.toLowerCase();
  if (command !== "new" && command !== "reset") return undefined;
  const title = parts
    .filter((part) => !["--confirm", "confirm", "确认", "--yes", "-y", "now"].includes(part.toLowerCase()))
    .join(" ")
    .trim();
  return title || null;
}

function parseSessionSwitchCommand(content: string): string | undefined {
  const body = content
    .trim()
    .replace(/^[/／]/, "")
    .trim();
  if (!body) return undefined;
  const parts = body.split(/\s+/);
  const command = parts.shift()?.toLowerCase();
  if (!["sessions", "session", "conversations"].includes(command ?? "")) return undefined;
  const selector = parts.join(" ").trim();
  if (!selector || selector.toLowerCase() === "list") return undefined;
  return selector;
}

function sameToolRun(left: AgentRunEvent["toolEvent"], right: AgentRunEvent["toolEvent"]) {
  if (!left || !right) return false;
  if (left.callId && right.callId) return left.callId === right.callId;
  if (left.callId || right.callId) return false;
  return left.serverId === right.serverId
    && left?.toolName === right?.toolName
    && left?.title === right?.title;
}

function mergeToolEventList(previousEvents: ToolEvent[], incoming: ToolEvent | null | undefined) {
  if (!incoming) return previousEvents;
  const events = [...previousEvents];
  const runningIndex = events.findIndex((item) => item.status === "running" && sameToolRun(item, incoming));
  if (runningIndex >= 0 && incoming.status !== "running") {
    events[runningIndex] = incoming;
    return events;
  }
  const duplicateIndex = events.findIndex((item) =>
    sameToolRun(item, incoming)
    && item.status === incoming.status
    && item.elapsedMs === incoming.elapsedMs
    && item.summary === incoming.summary
  );
  if (duplicateIndex >= 0) {
    events[duplicateIndex] = incoming;
    return events;
  }
  return [...events, incoming];
}

function mergeToolRunEvents(previous: AgentRunEvent | undefined, event: AgentRunEvent) {
  return mergeToolEventList(previous?.accumulatedToolEvents ?? [], event.toolEvent);
}

function mergeRunPhases(previous: AgentRunEvent | undefined, event: AgentRunEvent) {
  const phases = [...(previous?.accumulatedPhases ?? [])];
  if (!event.phase) return phases;
  const next = { phase: event.phase, detail: event.detail ?? null, updatedAt: event.updatedAt };
  const last = phases[phases.length - 1];
  if (last && last.phase === next.phase && JSON.stringify(last.detail ?? null) === JSON.stringify(next.detail ?? null)) {
    phases[phases.length - 1] = next;
    return phases;
  }
  phases.push(next);
  return phases.slice(-24);
}

function workflowRecord(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function workflowString(value: unknown): string | null {
  return typeof value === "string" && value.trim() ? value : null;
}

function workflowSequence(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function workflowBoolean(value: unknown): boolean | null {
  return typeof value === "boolean" ? value : null;
}

function workflowAliasString(record: Record<string, unknown>, camelCase: string, snakeCase: string): string | null {
  return workflowString(record[camelCase]) ?? workflowString(record[snakeCase]);
}

function workflowAliasBoolean(record: Record<string, unknown>, camelCase: string, snakeCase: string): boolean | null {
  return workflowBoolean(record[camelCase]) ?? workflowBoolean(record[snakeCase]);
}

function workflowAliasSequence(record: Record<string, unknown>, camelCase: string, snakeCase: string): number | null {
  return workflowSequence(record[camelCase]) ?? workflowSequence(record[snakeCase]);
}

function workflowEventSequence(record: Record<string, unknown>): number | null {
  return workflowAliasSequence(record, "eventSequence", "event_sequence");
}

const WORKFLOW_DETAIL_ALIAS_PAIRS: readonly [string, string][] = [
  ["requestSource", "request_source"],
  ["toolContext", "tool_context"],
  ["queueItemId", "queue_item_id"],
  ["queueStatus", "queue_status"],
  ["queueLifecycle", "queue_lifecycle"],
  ["preserveCurrent", "preserve_current"],
  ["conversationKind", "conversation_kind"],
  ["roomId", "room_id"],
  ["channelId", "channel_id"],
  ["chatId", "chat_id"],
  ["threadId", "thread_id"],
  ["groupId", "group_id"],
  ["approvalId", "approval_id"],
  ["checkpointId", "checkpoint_id"],
  ["checkpointScope", "checkpoint_scope"],
  ["checkpointState", "checkpoint_state"],
  ["checkpointSummary", "checkpoint_summary"],
  ["checkpointIteration", "checkpoint_iteration"],
  ["previousState", "previous_state"],
  ["runState", "run_state"],
  ["mutationKind", "mutation_kind"],
  ["targetSummary", "target_summary"],
  ["toolCount", "tool_count"],
  ["toolProtocol", "tool_protocol"],
  ["toolOrigins", "tool_origins"],
  ["toolCallIds", "tool_call_ids"],
  ["toolCalls", "tool_calls"],
  ["providerNative", "provider_native"],
  ["requestedName", "requested_name"],
  ["serverId", "server_id"],
  ["toolName", "tool_name"],
  ["toolKind", "tool_kind"],
  ["sourceLabel", "source_label"],
  ["definitionName", "definition_name"],
  ["requiresApproval", "requires_approval"],
  ["directBridge", "direct_bridge"],
  ["approvedToolCallReplay", "approved_tool_call_replay"],
  ["bridgeStatus", "bridge_status"],
  ["bridgeRejectionReason", "bridge_rejection_reason"],
  ["bridgeStage", "bridge_stage"],
  ["lastBridgeTarget", "last_bridge_target"],
  ["messageId", "message_id"],
  ["providerId", "provider_id"],
  ["errorKind", "error_kind"],
  ["timeoutSeconds", "timeout_seconds"],
  ["requestedChildren", "requested_children"],
  ["existingChildren", "existing_children"],
  ["parentDepth", "parent_depth"],
  ["childDepth", "child_depth"],
  ["maxSubagents", "max_subagents"],
  ["maxSubagentDepth", "max_subagent_depth"],
  ["maxConcurrentChildren", "max_concurrent_children"],
  ["orchestratorEnabled", "orchestrator_enabled"],
  ["subagentAutoApprove", "subagent_auto_approve"],
  ["inheritMcpToolsets", "inherit_mcp_toolsets"],
  ["completedChildren", "completed_children"],
  ["failedChildren", "failed_children"],
  ["abortedChildren", "aborted_children"],
  ["unknownChildren", "unknown_children"],
  ["childIndex", "child_index"],
  ["taskPreview", "task_preview"],
  ["canDelegate", "can_delegate"],
  ["maxIterations", "max_iterations"],
  ["acpCommand", "acp_command"],
  ["acpSessionMode", "acp_session_mode"],
  ["childRunId", "child_run_id"],
  ["childConversationId", "child_conversation_id"],
  ["resultPreview", "result_preview"],
  ["errorPreview", "error_preview"],
  ["hasDiagnosticArtifact", "has_diagnostic_artifact"]
];

function normalizeWorkflowDetailAliasPair(record: Record<string, unknown>, camelCase: string, snakeCase: string) {
  const camelValue = record[camelCase];
  const snakeValue = record[snakeCase];
  const value = camelValue !== undefined && camelValue !== null
    ? camelValue
    : snakeValue !== undefined && snakeValue !== null
      ? snakeValue
      : undefined;
  if (value === undefined) return;
  if (record[camelCase] === undefined || record[camelCase] === null) record[camelCase] = value;
  if (record[snakeCase] === undefined || record[snakeCase] === null) record[snakeCase] = value;
}

function normalizeWorkflowDetailAliases(value: unknown): unknown {
  if (Array.isArray(value)) {
    return value.map((item) => normalizeWorkflowDetailAliases(item));
  }
  const record = workflowRecord(value);
  if (!record) return value;
  const normalized: Record<string, unknown> = { ...record };
  for (const [key, child] of Object.entries(normalized)) {
    if (child && typeof child === "object") {
      normalized[key] = normalizeWorkflowDetailAliases(child);
    }
  }
  for (const [camelCase, snakeCase] of WORKFLOW_DETAIL_ALIAS_PAIRS) {
    normalizeWorkflowDetailAliasPair(normalized, camelCase, snakeCase);
  }
  return normalized;
}

function cloneWorkflowNodes(nodes: WorkflowGraph["nodes"]): WorkflowGraphNode[] {
  return (nodes ?? []).map((node) => {
    const record = node as unknown as Record<string, unknown>;
    const normalizedNode = workflowString(record.node) ?? String(node.node);
    const eventSequence = workflowAliasSequence(record, "eventSequence", "event_sequence") ?? node.eventSequence ?? null;
    const updatedAt = workflowAliasString(record, "updatedAt", "updated_at") ?? node.updatedAt ?? null;
    return {
      ...node,
      node: normalizedNode,
      role: workflowString(record.role) ?? workflowNodeRoleLabel(normalizedNode),
      status: workflowString(record.status) ?? node.status,
      detail: normalizeWorkflowDetailAliases(record.detail ?? node.detail),
      eventSequence,
      event_sequence: eventSequence,
      updatedAt,
      updated_at: updatedAt
    };
  });
}

function cloneWorkflowTransitions(transitions: WorkflowGraph["transitions"]): WorkflowGraphTransition[] {
  return (transitions ?? []).map((transition) => {
    const record = transition as unknown as Record<string, unknown>;
    const eventSequence = workflowAliasSequence(record, "eventSequence", "event_sequence") ?? transition.eventSequence ?? null;
    const updatedAt = workflowAliasString(record, "updatedAt", "updated_at") ?? transition.updatedAt ?? null;
    const topologyEdgeKnown = workflowAliasBoolean(record, "topologyEdgeKnown", "topology_edge_known") ?? transition.topologyEdgeKnown ?? null;
    const topologyReasonKnown = workflowAliasBoolean(record, "topologyReasonKnown", "topology_reason_known") ?? transition.topologyReasonKnown ?? null;
    const topologyEdgeSource = workflowAliasString(record, "topologyEdgeSource", "topology_edge_source") ?? transition.topologyEdgeSource ?? null;
    const topologyEdgeLabel = workflowAliasString(record, "topologyEdgeLabel", "topology_edge_label") ?? transition.topologyEdgeLabel ?? null;
    return {
      ...transition,
      from: workflowString(record.from) ?? transition.from ?? null,
      to: workflowString(record.to) ?? transition.to ?? null,
      reason: workflowString(record.reason) ?? transition.reason ?? null,
      topologyEdgeKnown,
      topology_edge_known: topologyEdgeKnown,
      topologyReasonKnown,
      topology_reason_known: topologyReasonKnown,
      topologyEdgeSource,
      topology_edge_source: topologyEdgeSource,
      topologyEdgeLabel,
      topology_edge_label: topologyEdgeLabel,
      detail: normalizeWorkflowDetailAliases(record.detail ?? transition.detail),
      eventSequence,
      event_sequence: eventSequence,
      updatedAt,
      updated_at: updatedAt
    };
  });
}

function cloneWorkflowGraph(graph: WorkflowGraph): WorkflowGraph {
  const record = graph as unknown as Record<string, unknown>;
  const nodes = cloneWorkflowNodes(graph.nodes);
  const currentNode = workflowAliasString(record, "currentNode", "current_node") ?? graph.currentNode ?? null;
  const currentStatus = graph.currentStatus
    ?? workflowAliasString(record, "currentStatus", "current_status")
    ?? nodes.find((node) => node.node === currentNode)?.status
    ?? null;
  const requestSource = workflowAliasString(record, "requestSource", "request_source") ?? graph.requestSource;
  const toolContext = workflowAliasString(record, "toolContext", "tool_context") ?? graph.toolContext;
  const lastEventSequence = workflowAliasSequence(record, "lastEventSequence", "last_event_sequence") ?? graph.lastEventSequence ?? null;
  const updatedAt = workflowAliasString(record, "updatedAt", "updated_at") ?? graph.updatedAt ?? null;
  return {
    ...graph,
    requestSource,
    request_source: requestSource,
    toolContext,
    tool_context: toolContext,
    currentNode,
    current_node: currentNode,
    currentStatus,
    current_status: currentStatus,
    lastEventSequence,
    last_event_sequence: lastEventSequence,
    updatedAt,
    updated_at: updatedAt,
    nodes,
    transitions: cloneWorkflowTransitions(graph.transitions)
  };
}

function initializedWorkflowGraph(detail: unknown): WorkflowGraph | null {
  const graph = workflowRecord(detail);
  if (!graph) return null;
  return cloneWorkflowGraph(graph as WorkflowGraph);
}

function ensureWorkflowGraph(previous: WorkflowGraph | null | undefined, updatedAt: string): WorkflowGraph {
  if (previous) {
    const graph = cloneWorkflowGraph(previous);
    return {
      ...graph,
      updatedAt,
      updated_at: updatedAt
    };
  }
  return {
    schema: WORKFLOW_GRAPH_SCHEMA,
    mode: "recovered",
    nodes: [],
    transitions: [],
    currentNode: null,
    current_node: null,
    currentStatus: null,
    current_status: null,
    lastEventSequence: 0,
    last_event_sequence: 0,
    updatedAt,
    updated_at: updatedAt
  };
}

function workflowNodeUpdateSetsCurrent(graph: WorkflowGraph, node: string, status: string, detail?: unknown): boolean {
  const detailRecord = workflowRecord(detail);
  if (detailRecord) {
    const preserveCurrent = detailRecord.preserveCurrent ?? detailRecord.preserve_current;
    if (preserveCurrent === true) return false;
  }
  if (status !== "skipped") return true;
  if (node === "reviewer") return true;
  return graph.currentNode === node;
}

function applyWorkflowGraphEvent(
  previous: WorkflowGraph | null | undefined,
  phase: string | null | undefined,
  detail: unknown,
  updatedAt: string
): WorkflowGraph | null {
  if (phase === WORKFLOW_PHASE_INITIALIZED) {
    return initializedWorkflowGraph(detail);
  }
  if (phase !== WORKFLOW_PHASE_NODE && phase !== WORKFLOW_PHASE_TRANSITION) {
    return previous ?? null;
  }
  const event = workflowRecord(detail);
  if (!event) return previous ?? null;
  const graph = ensureWorkflowGraph(previous, updatedAt);
  const eventSequence = workflowEventSequence(event);
  graph.lastEventSequence = eventSequence ?? graph.lastEventSequence ?? null;
  graph.last_event_sequence = graph.lastEventSequence;
  graph.updatedAt = updatedAt;
  graph.updated_at = updatedAt;

  if (phase === WORKFLOW_PHASE_NODE) {
    const node = workflowString(event.node);
    const status = workflowString(event.status);
    if (!node || !status) return graph;
    const nextNode: WorkflowGraphNode = {
      node,
      role: workflowString(event.role) ?? workflowNodeRoleLabel(node),
      status,
      detail: normalizeWorkflowDetailAliases(event.detail ?? {}),
      eventSequence,
      event_sequence: eventSequence,
      updatedAt,
      updated_at: updatedAt
    };
    const nodes = cloneWorkflowNodes(graph.nodes);
    const existingIndex = nodes.findIndex((item) => item.node === node);
    if (existingIndex >= 0) {
      nodes[existingIndex] = nextNode;
    } else {
      nodes.push(nextNode);
    }
    graph.nodes = nodes;
    if (workflowNodeUpdateSetsCurrent(graph, node, status, nextNode.detail)) {
      graph.currentNode = node;
      graph.current_node = node;
      graph.currentStatus = status;
      graph.current_status = status;
    }
    return graph;
  }

  const transition: WorkflowGraphTransition = {
    from: workflowString(event.from),
    to: workflowString(event.to),
    reason: workflowString(event.reason),
    topologyEdgeKnown: workflowAliasBoolean(event, "topologyEdgeKnown", "topology_edge_known"),
    topology_edge_known: workflowAliasBoolean(event, "topologyEdgeKnown", "topology_edge_known"),
    topologyReasonKnown: workflowAliasBoolean(event, "topologyReasonKnown", "topology_reason_known"),
    topology_reason_known: workflowAliasBoolean(event, "topologyReasonKnown", "topology_reason_known"),
    topologyEdgeSource: workflowAliasString(event, "topologyEdgeSource", "topology_edge_source"),
    topology_edge_source: workflowAliasString(event, "topologyEdgeSource", "topology_edge_source"),
    topologyEdgeLabel: workflowAliasString(event, "topologyEdgeLabel", "topology_edge_label"),
    topology_edge_label: workflowAliasString(event, "topologyEdgeLabel", "topology_edge_label"),
    detail: normalizeWorkflowDetailAliases(event.detail ?? {}),
    eventSequence,
    event_sequence: eventSequence,
    updatedAt,
    updated_at: updatedAt
  };
  graph.transitions = [...cloneWorkflowTransitions(graph.transitions), transition];
  if (transition.to) {
    graph.currentNode = transition.to;
    graph.current_node = transition.to;
    graph.currentStatus = graph.nodes?.find((node) => node.node === transition.to)?.status ?? null;
    graph.current_status = graph.currentStatus;
  }
  return graph;
}

function mergeWorkflowGraphFromRunEvent(
  previous: WorkflowGraph | null | undefined,
  event: AgentRunEvent
): WorkflowGraph | null {
  const snapshot = initializedWorkflowGraph(event.workflowGraph ?? event.workflow_graph);
  if (snapshot) return snapshot;
  return applyWorkflowGraphEvent(previous, event.phase, event.detail, event.updatedAt) ?? previous ?? null;
}

interface AppState {
  activeSection: AppSection;
  previousSection: AppSection | null;
  focusedAgentId: string | null;
  skillsPanelMode: "local" | "global";
  mcpPanelMode: "local" | "global";
  config: AppConfig | null;
  conversations: Conversation[];
  activeConversationId: string | null;
  messages: ChatMessage[];
  conversationMessageLimits: Record<string, number>;
  processingConversationIds: string[];
  conversationUnreadCounts: Record<string, number>;
  llmProviders: LlmProvider[];
  profile: ProfileConfig;
  accounts: AccountConfig[];
  imageProviders: ImageProvider[];
  videoProviders: VideoProvider[];
  searchProviders: SearchProvider[];
  visionProviders: VisionProvider[];
  browserProviders: BrowserProvider[];
  themes: ThemeConfig[];
  emojiGroups: EmojiGroup[];
  memories: MemoryEntry[];
  worldbooks: Worldbook[];
  agents: AgentDefinition[];
  agentQueue: AgentQueuedRequest[];
  agentRuns: AgentRunRecord[];
  activeAgentRuns: Record<string, AgentRunEvent>;
  managedProcessEvents: ManagedProcessEvent[];
  plugins: PluginSummary[];
  skillBundles: SkillBundle[];
  marketplaceSkills: MarketplaceSkill[];
  moments: MomentPost[];
  personas: Persona[];
  mcpServers: McpServer[];
  capabilityAdapters: CapabilityAdapter[];
  agentConfig: AgentConfig | null;
  skills: EnhancedSkillSummary[];
  proactiveStatuses: ProactiveStatus[];
  lastMcpResult: McpCallResult | null;
  lastMcpToolsResult: McpListToolsResult | null;
  streamedAssistantIds: Set<string>;
  loading: boolean;
  setSection: (section: AppSection, settingsView?: string) => void;
  setFocusedAgentId: (agentId: string | null) => void;
  setSkillsPanelMode: (mode: "local" | "global") => void;
  setMcpPanelMode: (mode: "local" | "global") => void;
  goBack: () => void;
  bootstrap: () => Promise<void>;
  refreshChatData: (preferredConversationId?: string | null, preferredPersonaId?: string | null) => Promise<void>;
  loadOlderMessages: (conversationId?: string | null, pageSize?: number) => Promise<{ loadedCount: number; hasMore: boolean }>;
  setConversationProcessing: (conversationId: string, processing: boolean) => void;
  incrementConversationUnread: (conversationId: string, amount?: number) => void;
  markConversationRead: (conversationId: string) => void;
  upsertIncomingMessage: (message: ChatMessage, options?: IncomingMessageUpsertOptions) => void;
  createConversation: (personaId?: string) => Promise<void>;
  openPersonaConversation: (personaId: string) => Promise<void>;
  deleteConversation: (conversationId: string) => Promise<ConversationDeleteMemorySettlingResult>;
  selectConversation: (conversationId: string) => Promise<void>;
  sendMessage: (content: string, personaId?: string, agentId?: string) => Promise<void>;
  deleteMessage: (messageId: string) => Promise<void>;
  saveLlmProviders: (providers: LlmProvider[]) => Promise<void>;
  saveProfile: (profile: ProfileConfig) => Promise<void>;
  uploadProfileAvatar: (file: File) => Promise<void>;
  clearProfileAvatar: () => Promise<void>;
  refreshAccounts: () => Promise<void>;
  saveAccounts: (accounts: AccountConfig[]) => Promise<void>;
  linkWechatAccount: (personaId: string, accountId: string) => Promise<void>;
  unlinkWechatAccount: (personaId: string) => Promise<void>;
  saveImageProviders: (providers: ImageProvider[]) => Promise<void>;
  saveVideoProviders: (providers: VideoProvider[]) => Promise<void>;
  saveSearchProviders: (providers: SearchProvider[]) => Promise<void>;
  saveVisionProviders: (providers: VisionProvider[]) => Promise<void>;
  saveBrowserProviders: (providers: BrowserProvider[]) => Promise<void>;
  saveThemes: (themes: ThemeConfig[]) => Promise<void>;
  importThemeCss: (file: File) => Promise<void>;
  saveEmojiGroups: (groups: EmojiGroup[]) => Promise<void>;
  uploadEmojiImage: (groupId: string, emotion: string, file: File) => Promise<void>;
  refreshMemories: (personaId?: string) => Promise<void>;
  saveMemory: (memory: Partial<MemoryEntry> & { personaId: string; summary: string; importance: number; target?: string }) => Promise<void>;
  deleteMemory: (id: string) => Promise<void>;
  saveWorldbook: (book: Worldbook) => Promise<void>;
  deleteWorldbook: (id: string) => Promise<void>;
  refreshMoments: () => Promise<void>;
  createMoment: (body: string) => Promise<void>;
  updateMomentText: (postId: string, body: string) => Promise<void>;
  addMomentComment: (postId: string, text: string) => Promise<void>;
  updateMomentComment: (postId: string, commentId: string, text: string) => Promise<void>;
  deleteMoment: (postId: string) => Promise<void>;
  deleteMomentComment: (postId: string, commentId: string) => Promise<void>;
  toggleMomentLike: (postId: string) => Promise<void>;
  uploadMomentCover: (postId: string, file: File) => Promise<void>;
  clearMomentCover: (postId: string) => Promise<void>;
  refreshMcpServers: () => Promise<void>;
  saveMcpServers: (servers: McpServer[]) => Promise<void>;
  refreshCapabilityAdapters: () => Promise<void>;
  saveCapabilityAdapters: (adapters: CapabilityAdapter[]) => Promise<void>;
  refreshAgentConfig: () => Promise<void>;
  saveAgentConfig: (config: AgentConfig) => Promise<void>;
  refreshAgents: () => Promise<void>;
  refreshAgentQueue: () => Promise<void>;
  refreshAgentRuns: () => Promise<void>;
  handleAgentRunEvent: (event: AgentRunEvent) => void;
  handleManagedProcessEvent: (event: ManagedProcessEvent) => void;
  saveAgent: (agent: AgentDefinition) => Promise<AgentDefinition>;
  deleteAgent: (id: string) => Promise<void>;
  refreshSkills: () => Promise<void>;
  refreshSkillsForAgent: (agentId: string) => Promise<void>;
  installBuiltinSkills: () => Promise<void>;
  saveSkillConfig: (agentId: string, skillId: string, config: Record<string, string>) => Promise<void>;
  refreshSkillBundles: () => Promise<void>;
  installSkillBundle: (bundleId: string, agentId?: string) => Promise<void>;
  refreshMarketplaceSkills: (query?: string, source?: string) => Promise<void>;
  installMarketplaceSkill: (skillId: string, agentId?: string) => Promise<void>;
  installExternalSkillUrl: (url: string, name?: string, category?: string, agentId?: string, force?: boolean) => Promise<void>;
  refreshProactiveStatuses: () => Promise<void>;
  triggerProactiveOnce: (personaId: string) => Promise<void>;
  listMcpTools: (serverId: string, timeoutSeconds?: number) => Promise<void>;
  callMcpTool: (serverId: string, toolName: string, payload: unknown, timeoutSeconds?: number) => Promise<void>;
  savePersona: (persona: Persona) => Promise<Persona>;
  deletePersona: (id: string) => Promise<void>;
  uploadPersonaAvatar: (personaId: string, file: File) => Promise<Persona>;
  clearPersonaAvatar: (personaId: string) => Promise<Persona>;
  saveConfig: (config: AppConfig) => Promise<void>;
  togglePlugin: (pluginId: string, enabled: boolean) => Promise<void>;
}

export const useAppStore = create<AppState>((set, get) => ({
  activeSection: "chat",
  previousSection: null,
  focusedAgentId: null,
  skillsPanelMode: "global",
  mcpPanelMode: "global",
  config: bootstrapCache?.config ?? null,
  conversations: [],
  activeConversationId: null,
  messages: [],
  conversationMessageLimits: {},
  processingConversationIds: [],
  conversationUnreadCounts: {},
  llmProviders: bootstrapCache?.llmProviders ?? [],
  profile: bootstrapCache?.profile ?? { name: "我", avatarPath: null },
  accounts: bootstrapCache?.accounts ?? [],
  imageProviders: bootstrapCache?.imageProviders ?? [],
  videoProviders: bootstrapCache?.videoProviders ?? [],
  searchProviders: bootstrapCache?.searchProviders ?? [],
  visionProviders: bootstrapCache?.visionProviders ?? [],
  browserProviders: bootstrapCache?.browserProviders ?? [],
  themes: bootstrapCache?.themes ?? [],
  emojiGroups: bootstrapCache?.emojiGroups ?? [],
  memories: [],
  worldbooks: [],
  agents: [],
  agentQueue: [],
  agentRuns: [],
  activeAgentRuns: {},
  managedProcessEvents: [],
  plugins: [],
  skillBundles: [],
  marketplaceSkills: [],
  moments: [],
  personas: bootstrapCache?.personas ?? [],
  mcpServers: [],
  capabilityAdapters: [],
  agentConfig: null,
  skills: [],
  proactiveStatuses: [],
  lastMcpResult: null,
  lastMcpToolsResult: null,
  streamedAssistantIds: new Set<string>(),
  loading: false,
  setSection: (activeSection, settingsView) => {
    if (settingsView) {
      pendingSettingsViewRef = settingsView;
    }
    set((state) => ({ activeSection, previousSection: state.activeSection }));
  },
  setFocusedAgentId: (agentId) => {
    set((state) => ({
      focusedAgentId: normalizeFocusedAgentId(state.agents, agentId)
    }));
  },
  setSkillsPanelMode: (skillsPanelMode) => set({ skillsPanelMode }),
  setMcpPanelMode: (mcpPanelMode) => set({ mcpPanelMode }),
  goBack: () => {
    const { previousSection } = get();
    if (previousSection) {
      set({ activeSection: previousSection, previousSection: null });
    }
  },
  bootstrap: async () => {
    const startedProfileMutationVersion = profileMutationVersion;
    const startedPersonaMutationVersion = personaMutationVersion;
    set({ loading: true });
    const config = await withBootstrapTimeout(
      api.getConfig(),
      get().config ?? bootstrapCache?.config ?? null,
      "config bootstrap",
      3000
    );
    const profile = await withBootstrapTimeout(
      api.getProfile(),
      get().profile,
      "profile bootstrap",
      3000
    );
    set((state) => ({
      config,
      profile: profileMutationVersion === startedProfileMutationVersion ? profile : state.profile,
      loading: false
    }));
    await api.cleanupHistoricalResources().catch((error) => {
      console.warn("historical resource cleanup failed", error);
    });
    const results = await Promise.allSettled([
      withBootstrapTimeout(api.listConversations(), [] as Conversation[], "conversations bootstrap"),
      withBootstrapTimeout(api.listMoments(), [] as MomentPost[], "moments bootstrap"),
      withBootstrapTimeout(api.listPersonas(), get().personas, "personas bootstrap"),
      withBootstrapTimeout(api.listMcpServers(), [] as McpServer[], "mcp servers bootstrap"),
      withBootstrapTimeout(api.listCapabilityAdapters(), [] as CapabilityAdapter[], "capability adapters bootstrap"),
      withBootstrapTimeout(api.getAgentConfig(), null as AgentConfig | null, "agent config bootstrap"),
      withBootstrapTimeout(api.listSkills(), [] as EnhancedSkillSummary[], "skills bootstrap"),
      withBootstrapTimeout(api.listProactiveStatuses(), [] as ProactiveStatus[], "proactive statuses bootstrap"),
      withBootstrapTimeout(api.listLlmProviders(), get().llmProviders, "llm providers bootstrap"),
      withBootstrapTimeout(api.listAccounts(), get().accounts, "accounts bootstrap"),
      withBootstrapTimeout(api.listImageProviders(), get().imageProviders, "image providers bootstrap"),
      withBootstrapTimeout(api.listVideoProviders(), get().videoProviders, "video providers bootstrap"),
      withBootstrapTimeout(api.listSearchProviders(), get().searchProviders, "search providers bootstrap"),
      withBootstrapTimeout(api.listVisionProviders(), get().visionProviders, "vision providers bootstrap"),
      withBootstrapTimeout(api.listBrowserProviders(), get().browserProviders, "browser providers bootstrap"),
      withBootstrapTimeout(api.listThemes(), get().themes, "themes bootstrap"),
      withBootstrapTimeout(api.listEmojiGroups(), get().emojiGroups, "emoji groups bootstrap"),
      withBootstrapTimeout(api.listMemories(), [] as MemoryEntry[], "memories bootstrap"),
      withBootstrapTimeout(api.listWorldbooks(), [] as Worldbook[], "worldbooks bootstrap"),
      withBootstrapTimeout(api.listPlugins(), [] as PluginSummary[], "plugins bootstrap"),
      withBootstrapTimeout(api.listAgents(), [] as AgentDefinition[], "agents bootstrap"),
      withBootstrapTimeout(api.listAgentRuns(), [] as AgentRunRecord[], "agent runs bootstrap"),
      withBootstrapTimeout(api.listAgentQueue(), [] as AgentQueuedRequest[], "agent queue bootstrap"),
      withBootstrapTimeout(api.listSkillBundles(), [] as SkillBundle[], "skill bundles bootstrap")
    ]);
    const pick = <T,>(index: number, fallback: T): T => {
      const result = results[index];
      if (result.status === "fulfilled") return result.value as T;
      console.warn(`bootstrap item ${index} failed`, result.reason);
      return fallback;
    };
    const conversations = pick<Conversation[]>(0, []);
    const moments = pick<MomentPost[]>(1, []);
    const personas = pick<Persona[]>(2, []);
    const mcpServers = pick<McpServer[]>(3, []);
    const capabilityAdapters = pick<CapabilityAdapter[]>(4, []);
    const agentConfig = pick<AgentConfig | null>(5, null);
    const skills = pick<EnhancedSkillSummary[]>(6, []);
    const proactiveStatuses = pick<ProactiveStatus[]>(7, []);
    const llmProviders = pick<LlmProvider[]>(8, []);
    const accounts = pick<AccountConfig[]>(9, []);
    const imageProviders = pick<ImageProvider[]>(10, []);
    const videoProviders = pick<VideoProvider[]>(11, []);
    const searchProviders = pick<SearchProvider[]>(12, []);
    const visionProviders = pick<VisionProvider[]>(13, []);
    const browserProviders = pick<BrowserProvider[]>(14, []);
    const themes = pick<ThemeConfig[]>(15, []);
    const emojiGroups = pick<EmojiGroup[]>(16, []);
    const memories = pick<MemoryEntry[]>(17, []);
    const worldbooks = pick<Worldbook[]>(18, []);
    const plugins = pick<PluginSummary[]>(19, []);
    const agents = pick<AgentDefinition[]>(20, []);
    const agentRuns = pick<AgentRunRecord[]>(21, []);
    const agentQueue = pick<AgentQueuedRequest[]>(22, []);
    const skillBundles = pick<SkillBundle[]>(23, []);
    const currentActive = get().activeConversationId;
    const activeConversationId = currentActive && conversations.some((item) => item.id === currentActive)
      ? currentActive
      : conversations[0]?.id ?? null;
    const messageLimit = conversationMessageLimit(config, activeConversationId, get().conversationMessageLimits);
    const previewChars = uiMessagePreviewChars(config);
    const backendMessages = activeConversationId
      ? visibleChatMessages(await api.listMessages(activeConversationId, messageLimit, previewChars).catch((error) => {
        console.warn("message bootstrap failed", error);
        return [];
      }))
      : [];
    const messages = mergeBackendMessagesWithLiveState(
      backendMessages,
      get().messages,
      activeConversationId,
      messageLimit,
      get().streamedAssistantIds
    );
    set({
      config,
      conversations,
      activeConversationId,
      focusedAgentId: normalizeFocusedAgentId(agents, get().focusedAgentId),
      messages,
      moments,
      personas: personaMutationVersion === startedPersonaMutationVersion ? personas : get().personas,
      mcpServers,
      capabilityAdapters,
      agentConfig,
      skills: skills as EnhancedSkillSummary[],
      proactiveStatuses,
      llmProviders,
      profile: profileMutationVersion === startedProfileMutationVersion ? profile : get().profile,
      accounts,
      imageProviders,
      videoProviders,
      searchProviders,
      visionProviders,
      browserProviders,
      themes,
      emojiGroups,
      memories,
      worldbooks,
      plugins,
      agents,
      agentQueue,
      agentRuns,
      skillBundles,
      activeAgentRuns: {},
      managedProcessEvents: [],
      processingConversationIds: [],
      conversationMessageLimits: activeConversationId
        ? {
          ...get().conversationMessageLimits,
          [activeConversationId]: messageLimit
        }
        : get().conversationMessageLimits,
      loading: false
    });
    writeBootstrapCache({
      config,
      profile,
      llmProviders,
      imageProviders,
      videoProviders,
      searchProviders,
      visionProviders,
      browserProviders,
      themes,
      emojiGroups,
      accounts,
      personas
    });
    void Promise.all([api.tickScheduledAgentJobs(), api.drainAgentQueue()]).then(async () => {
      const [nextRuns, nextQueue, nextConversations] = await Promise.all([
        api.listAgentRuns(),
        api.listAgentQueue(),
        api.listConversations()
      ]);
      const state = get();
      const messageLimit = conversationMessageLimit(state.config, state.activeConversationId, state.conversationMessageLimits);
      const previewChars = uiMessagePreviewChars(state.config);
      const backendMessages = state.activeConversationId
        ? visibleChatMessages(await api.listMessages(state.activeConversationId, messageLimit, previewChars))
        : [];
      const liveState = get();
      const nextMessages = mergeBackendMessagesWithLiveState(
        backendMessages,
        liveState.messages,
        state.activeConversationId,
        messageLimit,
        liveState.streamedAssistantIds
      );
      set({
        agentRuns: nextRuns,
        agentQueue: nextQueue,
        conversations: nextConversations,
        messages: nextMessages
      });
    }).catch((error) => {
      console.warn("agent scheduler bootstrap failed", error);
    });
  },
  refreshChatData: async (preferredConversationId, preferredPersonaId) => {
    const refreshKey = refreshChatDataKey(preferredConversationId, preferredPersonaId);
    const existingRefresh = refreshChatDataInFlight.get(refreshKey);
    if (existingRefresh) return existingRefresh;
    const refresh = (async () => {
    const [conversations, agentQueue] = await Promise.all([
      api.listConversations(),
      api.listAgentQueue()
    ]);
    const state = get();
    const currentActive = state.activeConversationId;
    const activeConversationId =
      (preferredConversationId && conversations.some((item) => item.id === preferredConversationId)
        ? preferredConversationId
        : null)
      ?? (currentActive && conversations.some((item) => item.id === currentActive)
        ? currentActive
        : null)
      ?? (preferredPersonaId
        ? conversations.find((item) => item.personaId === preferredPersonaId)?.id ?? null
        : null)
      ?? conversations[0]?.id
      ?? null;
    const messageLimit = conversationMessageLimit(state.config, activeConversationId, state.conversationMessageLimits);
    const previewChars = uiMessagePreviewChars(state.config);
    const backendMessages = activeConversationId
      ? visibleChatMessages(await api.listMessages(activeConversationId, messageLimit, previewChars))
      : [];
    const liveState = get();
    const messages = mergeBackendMessagesWithLiveState(
      backendMessages,
      liveState.messages,
      activeConversationId,
      messageLimit,
      liveState.streamedAssistantIds
    );
    const latestMessage = messages.at(-1);
    const shouldClearProcessing =
      Boolean(activeConversationId && latestMessage?.role === "assistant")
      && !withinProcessingGrace(activeConversationId);
    if (
      liveState.activeConversationId === activeConversationId
      && sameConversations(liveState.conversations, conversations)
      && sameMessages(liveState.messages, messages)
    ) {
      set((current) => ({
        agentQueue,
        processingConversationIds: shouldClearProcessing
          ? current.processingConversationIds.filter((id) => id !== activeConversationId)
          : current.processingConversationIds
      }));
      return;
    }
    set((current) => ({
      conversations,
      agentQueue,
      activeConversationId,
      messages,
      conversationUnreadCounts: current.conversationUnreadCounts,
      processingConversationIds: shouldClearProcessing
        ? current.processingConversationIds.filter((id) => id !== activeConversationId)
        : current.processingConversationIds
    }));
    })();
    refreshChatDataInFlight.set(refreshKey, refresh);
    try {
      await refresh;
    } finally {
      if (refreshChatDataInFlight.get(refreshKey) === refresh) {
        refreshChatDataInFlight.delete(refreshKey);
      }
    }
  },
  loadOlderMessages: async (conversationId, pageSize) => {
    const state = get();
    const targetConversationId = conversationId ?? state.activeConversationId;
    if (!targetConversationId) return { loadedCount: state.messages.length, hasMore: false };
    const baseLimit = uiMessageLimit(state.config);
    const increment = Math.max(baseLimit, pageSize ?? baseLimit);
    const currentLimit = conversationMessageLimit(state.config, targetConversationId, state.conversationMessageLimits);
    const nextLimit = Math.min(MAX_UI_MESSAGE_LIMIT, currentLimit + increment);
    if (nextLimit <= currentLimit && state.messages.length >= currentLimit) {
      return { loadedCount: state.messages.length, hasMore: false };
    }
    const previewChars = uiMessagePreviewChars(state.config);
    const backendMessages = visibleChatMessages(await api.listMessages(targetConversationId, nextLimit, previewChars));
    const messages = mergeBackendMessagesWithLiveState(
      backendMessages,
      state.messages,
      targetConversationId,
      nextLimit,
      state.streamedAssistantIds
    );
    if (get().activeConversationId === targetConversationId) {
      set((current) => ({
        conversationMessageLimits: {
          ...current.conversationMessageLimits,
          [targetConversationId]: nextLimit
        },
        messages
      }));
    } else {
      set((current) => ({
        conversationMessageLimits: {
          ...current.conversationMessageLimits,
          [targetConversationId]: nextLimit
        }
      }));
    }
    return { loadedCount: messages.length, hasMore: backendMessages.length >= nextLimit && nextLimit < MAX_UI_MESSAGE_LIMIT };
  },
  setConversationProcessing: (conversationId, processing) => {
    if (!conversationId) return;
    if (processing) {
      const clearTimer = processingClearTimerCache.get(conversationId);
      if (clearTimer !== undefined) {
        window.clearTimeout(clearTimer);
        processingClearTimerCache.delete(conversationId);
      }
      processingMarkedAtCache.set(conversationId, Date.now());
      set((state) => {
        if (state.processingConversationIds.includes(conversationId)) return state;
        return { processingConversationIds: [...state.processingConversationIds, conversationId] };
      });
      return;
    }
    const markedAt = processingMarkedAtCache.get(conversationId);
    const elapsed = markedAt === undefined ? PROCESSING_MARK_GRACE_MS : Date.now() - markedAt;
    const remaining = Math.max(0, PROCESSING_MARK_GRACE_MS - elapsed);
    const clearProcessing = () => {
      processingMarkedAtCache.delete(conversationId);
      processingClearTimerCache.delete(conversationId);
      set((state) => {
        if (!state.processingConversationIds.includes(conversationId)) return state;
        return {
          processingConversationIds: state.processingConversationIds.filter((id) => id !== conversationId)
        };
      });
    };
    const pendingTimer = processingClearTimerCache.get(conversationId);
    if (pendingTimer !== undefined) {
      window.clearTimeout(pendingTimer);
      processingClearTimerCache.delete(conversationId);
    }
    if (remaining <= 0) {
      clearProcessing();
      return;
    }
    const timer = window.setTimeout(clearProcessing, remaining);
    processingClearTimerCache.set(conversationId, timer);
  },
  incrementConversationUnread: (conversationId, amount = 1) => {
    if (!conversationId || amount <= 0) return;
    set((state) => ({
      conversationUnreadCounts: {
        ...state.conversationUnreadCounts,
        [conversationId]: (state.conversationUnreadCounts[conversationId] ?? 0) + amount
      }
    }));
  },
  markConversationRead: (conversationId) => {
    if (!conversationId) return;
    set((state) => {
      if (!(conversationId in state.conversationUnreadCounts)) return state;
      const unreadCounts = { ...state.conversationUnreadCounts };
      delete unreadCounts[conversationId];
      return { conversationUnreadCounts: unreadCounts };
    });
  },
  upsertIncomingMessage: (message, options) => {
    if (!isVisibleChatMessage(message)) return;
    set((state) => {
      const uiMessage = previewMessageForUi(message, uiMessagePreviewChars(state.config));
      if (state.activeConversationId && message.conversationId !== state.activeConversationId) {
        rememberPendingIncomingMessage(uiMessage);
        return state;
      }
      const index = state.messages.findIndex((item) => item.id === uiMessage.id);
      const messageLimit = conversationMessageLimit(state.config, uiMessage.conversationId, state.conversationMessageLimits);
      const previousMessage = index >= 0 ? state.messages[index] : null;
      const incomingMessage = preserveLiveThinkingCardsForFinalMessage(uiMessage, previousMessage, options);
      const shouldKeepStreamingPresentation =
        incomingMessage.role === "assistant"
        && !options?.final
        && (
          options?.streaming
          || (
            previousMessage?.source === "desktop-stream"
            && (state.streamedAssistantIds.has(incomingMessage.id) || options?.final)
            && (
              incomingMessage.content.startsWith(previousMessage.content)
              || previousMessage.content.startsWith(incomingMessage.content)
            )
          )
        );
      const nextMessage = shouldKeepStreamingPresentation
        ? { ...incomingMessage, source: options?.streaming ? "desktop-stream" : previousMessage?.source ?? "desktop-stream" }
        : incomingMessage;
      const messages = index >= 0
        ? state.messages.map((item) => (item.id === incomingMessage.id ? nextMessage : item))
        : [...state.messages, nextMessage];
      let streamedAssistantIds = state.streamedAssistantIds;
      if (incomingMessage.role === "assistant") {
        if (options?.streaming) {
          streamedAssistantIds = new Set(streamedAssistantIds);
          streamedAssistantIds.add(incomingMessage.id);
        } else if (options?.final && streamedAssistantIds.has(incomingMessage.id)) {
          streamedAssistantIds = new Set(streamedAssistantIds);
          streamedAssistantIds.delete(incomingMessage.id);
        }
      }
      return { messages: displayMessages(messages, messageLimit), streamedAssistantIds };
    });
  },
  createConversation: async (personaId) => {
    const persona = personaId ? get().personas.find((item) => item.id === personaId) : null;
    const conversation = await api.createConversation(persona?.name, personaId);
    const conversations = await api.listConversations();
    const messageLimit = uiMessageLimit(get().config);
    const previewChars = uiMessagePreviewChars(get().config);
    const backendMessages = visibleChatMessages(await api.listMessages(conversation.id, messageLimit, previewChars));
    const liveState = get();
    const messages = mergeBackendMessagesWithLiveState(
      backendMessages,
      liveState.messages,
      conversation.id,
      messageLimit,
      liveState.streamedAssistantIds
    );
    set((current) => ({
      conversations,
      activeConversationId: conversation.id,
      messages,
      conversationMessageLimits: {
        ...current.conversationMessageLimits,
        [conversation.id]: messageLimit
      }
    }));
  },
  openPersonaConversation: async (personaId) => {
    const persona = get().personas.find((item) => item.id === personaId);
    const conversation = await api.createConversation(persona?.name, personaId);
    const conversations = await api.listConversations();
    const messageLimit = uiMessageLimit(get().config);
    const previewChars = uiMessagePreviewChars(get().config);
    const backendMessages = visibleChatMessages(await api.listMessages(conversation.id, messageLimit, previewChars));
    const liveState = get();
    const messages = mergeBackendMessagesWithLiveState(
      backendMessages,
      liveState.messages,
      conversation.id,
      messageLimit,
      liveState.streamedAssistantIds
    );
    set((current) => ({
      conversations,
      activeConversationId: conversation.id,
      messages,
      conversationMessageLimits: {
        ...current.conversationMessageLimits,
        [conversation.id]: messageLimit
      }
    }));
  },
  deleteConversation: async (conversationId) => {
    const settling = await api.deleteConversation(conversationId);
    const state = get();
    const conversations = await api.listConversations();
    const activeConversationId = state.activeConversationId === conversationId
      ? conversations[0]?.id ?? null
      : state.activeConversationId;
    const { [conversationId]: _, ...conversationMessageLimits } = state.conversationMessageLimits;
    const messageLimit = conversationMessageLimit(state.config, activeConversationId, conversationMessageLimits);
    const previewChars = uiMessagePreviewChars(state.config);
    const backendMessages = activeConversationId ? visibleChatMessages(await api.listMessages(activeConversationId, messageLimit, previewChars)) : [];
    const messages = mergeBackendMessagesWithLiveState(
      backendMessages,
      state.messages,
      activeConversationId,
      messageLimit,
      state.streamedAssistantIds
    );
    const { [conversationId]: _unread, ...unreadCounts } = state.conversationUnreadCounts;
    set({ conversations, activeConversationId, messages, conversationUnreadCounts: unreadCounts, conversationMessageLimits });
    return settling;
  },
  selectConversation: async (conversationId) => {
    set((state) => {
      if (state.activeConversationId === conversationId) return state;
      const unreadCounts = { ...state.conversationUnreadCounts };
      delete unreadCounts[conversationId];
      return {
        activeConversationId: conversationId,
        messages: [],
        conversationUnreadCounts: unreadCounts
      };
    });
    const state = get();
    const messageLimit = conversationMessageLimit(state.config, conversationId, state.conversationMessageLimits);
    const previewChars = uiMessagePreviewChars(state.config);
    const backendMessages = visibleChatMessages(await api.listMessages(conversationId, messageLimit, previewChars));
    const liveState = get();
    const messages = mergeBackendMessagesWithLiveState(
      backendMessages,
      liveState.messages,
      conversationId,
      messageLimit,
      liveState.streamedAssistantIds
    );
    if (get().activeConversationId === conversationId) {
      set((current) => ({
        activeConversationId: conversationId,
        messages,
        conversationMessageLimits: {
          ...current.conversationMessageLimits,
          [conversationId]: messageLimit
        }
      }));
    }
  },
  sendMessage: async (content, personaId, agentId) => {
    const cleanContent = content.trim();
    if (!cleanContent) return;
    const state = get();
    const newConversationTitle = parseNewConversationCommand(cleanContent);
    if (newConversationTitle !== undefined) {
      const persona = personaId ? state.personas.find((item) => item.id === personaId) : null;
      const conversation = await api.createConversation(newConversationTitle ?? persona?.name, personaId);
      const conversations = await api.listConversations();
      const messageLimit = uiMessageLimit(state.config);
      const previewChars = uiMessagePreviewChars(state.config);
      const backendMessages = visibleChatMessages(await api.listMessages(conversation.id, messageLimit, previewChars));
      const liveState = get();
      const messages = mergeBackendMessagesWithLiveState(
        backendMessages,
        liveState.messages,
        conversation.id,
        messageLimit,
        liveState.streamedAssistantIds
      );
      set((current) => ({
        conversations,
        activeConversationId: conversation.id,
        messages,
        processingConversationIds: state.processingConversationIds.filter((id) => id !== state.activeConversationId),
        conversationMessageLimits: {
          ...current.conversationMessageLimits,
          [conversation.id]: messageLimit
        }
      }));
      return;
    }
    const sessionSelector = parseSessionSwitchCommand(cleanContent);
    if (sessionSelector) {
      const selector = sessionSelector.toLowerCase();
      const conversations = state.conversations.length > 0 ? state.conversations : await api.listConversations();
      const matches = conversations.filter((conversation) =>
        conversation.id.toLowerCase() === selector
        || conversation.id.toLowerCase().startsWith(selector)
        || conversation.title.toLowerCase() === selector
      );
      if (matches.length === 1) {
        const conversation = matches[0];
        const messageLimit = conversationMessageLimit(state.config, conversation.id, state.conversationMessageLimits);
        const previewChars = uiMessagePreviewChars(state.config);
        const unreadCounts = { ...state.conversationUnreadCounts };
        delete unreadCounts[conversation.id];
        const backendMessages = visibleChatMessages(await api.listMessages(conversation.id, messageLimit, previewChars));
        const liveState = get();
        const messages = mergeBackendMessagesWithLiveState(
          backendMessages,
          liveState.messages,
          conversation.id,
          messageLimit,
          liveState.streamedAssistantIds
        );
        set((current) => ({
          conversations,
          activeConversationId: conversation.id,
          messages,
          conversationUnreadCounts: unreadCounts,
          processingConversationIds: state.processingConversationIds.filter((id) => id !== state.activeConversationId),
          conversationMessageLimits: {
            ...current.conversationMessageLimits,
            [conversation.id]: messageLimit
          }
        }));
        return;
      }
    }
    let activeConversationId = state.activeConversationId;
    let activeConversation = state.conversations.find((item) => item.id === activeConversationId) ?? null;
    if (!activeConversationId || !activeConversation || (personaId && activeConversation.personaId !== personaId)) {
      const persona = personaId ? state.personas.find((item) => item.id === personaId) : null;
      const conversation = await api.createConversation(persona?.name, personaId);
      activeConversationId = conversation.id;
      activeConversation = conversation;
      const conversations = await api.listConversations();
      const messageLimit = uiMessageLimit(state.config);
      const previewChars = uiMessagePreviewChars(state.config);
      const backendMessages = visibleChatMessages(await api.listMessages(conversation.id, messageLimit, previewChars));
      const liveState = get();
      const messages = mergeBackendMessagesWithLiveState(
        backendMessages,
        liveState.messages,
        conversation.id,
        messageLimit,
        liveState.streamedAssistantIds
      );
      set((current) => ({
        conversations,
        activeConversationId,
        messages,
        conversationMessageLimits: {
          ...current.conversationMessageLimits,
          [conversation.id]: messageLimit
        }
      }));
    }
    const clientMessageId = `local-${crypto.randomUUID()}`;
    const temporaryMessage: ChatMessage = {
      id: clientMessageId,
      conversationId: activeConversationId ?? "",
      role: "user",
      content: cleanContent,
      createdAt: new Date().toISOString(),
      source: "desktop",
      accountId: null,
      providerData: {
        source: "desktop",
        clientMessageId
      }
    };
    set((current) => ({
      messages: displayMessages(
        [...current.messages, temporaryMessage],
        conversationMessageLimit(current.config, activeConversationId, current.conversationMessageLimits)
      ),
      processingConversationIds: current.processingConversationIds.includes(activeConversationId ?? "")
        ? current.processingConversationIds
        : [...current.processingConversationIds, activeConversationId ?? ""].filter(Boolean),
      conversations: current.conversations.map((conversation) =>
        conversation.id === activeConversationId
          ? { ...conversation, lastMessage: cleanContent, updatedAt: temporaryMessage.createdAt }
          : conversation
      )
    }));
    const conversationIdForSend = activeConversationId;
    const personaIdForSend = personaId ?? activeConversation?.personaId ?? null;
    const requestedAgentId = agentId ?? activeConversation?.agentId ?? null;
    const agentIdForSend = requestedAgentId && state.agents.some((item) => item.id === requestedAgentId)
      ? requestedAgentId
      : null;
    emitPetThinkingEvent("thinking_started", conversationIdForSend, personaIdForSend);
    // 异步发送消息，不阻塞 UI
    void (async () => {
      try {
        const responseMessages = await api.sendChatMessage({
          conversationId: conversationIdForSend,
          personaId: personaIdForSend,
          agentId: agentIdForSend,
          content: cleanContent,
          providerData: {
            source: "desktop",
            clientMessageId: messageClientMessageId(temporaryMessage),
            clientCreatedAt: temporaryMessage.createdAt
          }
        }, uiMessagePreviewChars(get().config));
        const refreshAgentRuntime = async () => {
          await Promise.allSettled([
            get().refreshAgentQueue(),
            get().refreshAgentRuns()
          ]);
        };
        const visibleResponseMessages = visibleChatMessages(responseMessages ?? []);
        const hasVisibleResponse = visibleResponseMessages.some((m) =>
          (m.role === "assistant" || m.role === "tool") && m.content.trim()
        );
        const hasAssistantReply = visibleResponseMessages.some((m) =>
          m.role === "assistant" && m.content.trim() && !isAgentErrorMessage(m)
        );
        const assistantReply = visibleResponseMessages
          .filter((m) => m.role === "assistant" && m.content.trim() && !isAgentErrorMessage(m))
          .at(-1) ?? null;
        if (assistantReply) {
          emitPetThinkingEvent("thinking_finished", conversationIdForSend, personaIdForSend, true, assistantReply);
        }
        if (visibleResponseMessages.length > 0) {
          const currentState = get();
          const messageLimit = conversationMessageLimit(currentState.config, conversationIdForSend, currentState.conversationMessageLimits);
          set((current) => {
            const withoutTemp = current.messages.filter((m) => {
              if (m.conversationId !== conversationIdForSend) return true;
              if (
                m.role === "user"
                && isLocalUiMessage(m)
                && visibleResponseMessages.some((backend) => matchesLocalUserReplacement(m, backend))
              ) {
                return false;
              }
              if (hasVisibleResponse && isLocalStatusMessage(m)) return false;
              return true;
            });
            const existingIds = new Set(withoutTemp.map((m) => m.id));
            const newMessages = visibleResponseMessages.filter((m) => !existingIds.has(m.id));
            const merged = [...withoutTemp, ...newMessages];
            return { messages: displayMessages(merged, messageLimit) };
          });
        }
        if (get().activeConversationId === conversationIdForSend) {
          if (hasVisibleResponse) {
            scheduleBackgroundStoreRefresh(
              "chat data refresh",
              () => get().refreshChatData(conversationIdForSend, personaIdForSend),
              700
            );
          } else {
            await get().refreshChatData(conversationIdForSend, personaIdForSend);
          }
        }
        if (hasVisibleResponse) {
          scheduleBackgroundStoreRefresh("agent runtime refresh", refreshAgentRuntime, 220);
        } else {
          await refreshAgentRuntime();
        }
        const current = get();
        const hasNewerLocalUser = conversationIdForSend
          ? current.messages.some((message) =>
            message.conversationId === conversationIdForSend
            && message.role === "user"
            && isLocalUiMessage(message)
            && messageTime(message) > messageTime(temporaryMessage)
          )
          : false;
        const hasVisibleAssistant = conversationIdForSend
          ? current.messages.some((message) =>
            message.conversationId === conversationIdForSend
            && message.role === "assistant"
            && message.content.trim()
            && messageTime(message) >= messageTime(temporaryMessage) - 1000
          )
          : false;
        const pendingWork = hasPendingAgentWork(current, conversationIdForSend);
        if (!hasNewerLocalUser && (hasAssistantReply || hasVisibleAssistant || !pendingWork)) {
          current.setConversationProcessing(conversationIdForSend ?? "", false);
        }
        if (!hasNewerLocalUser && !assistantReply && !pendingWork) {
          emitPetThinkingEvent("thinking_finished", conversationIdForSend, personaIdForSend, true);
        }
        if (!hasNewerLocalUser && !hasAssistantReply && !hasVisibleAssistant && !pendingWork) {
          current.setConversationProcessing(conversationIdForSend ?? "", false);
        }
      } catch (error) {
        console.error("发送消息失败:", error);
        scheduleBackgroundStoreRefresh(
          "agent runtime refresh",
          async () => {
            await Promise.allSettled([
              get().refreshAgentQueue(),
              get().refreshAgentRuns()
            ]);
          },
          0
        );
        const current = get();
        const hasNewerLocalUser = conversationIdForSend
          ? current.messages.some((message) =>
            message.conversationId === conversationIdForSend
            && message.role === "user"
            && isLocalUiMessage(message)
            && messageTime(message) > messageTime(temporaryMessage)
          )
          : false;
        if (!hasNewerLocalUser) {
          current.setConversationProcessing(conversationIdForSend ?? "", false);
        }
        emitPetThinkingEvent("thinking_finished", conversationIdForSend, personaIdForSend, false);
        // Keep transient transport errors out of the chat timeline. The backend
        // owns visible assistant/error messages through synthchat-chat-event,
        // and successful retries can still arrive as live stream events.
      }
    })();
  },
  deleteMessage: async (messageId) => {
    await api.deleteMessage(messageId);
    set((state) => ({ messages: state.messages.filter((message) => message.id !== messageId) }));
  },
  saveLlmProviders: async (llmProviders) => {
    await api.saveLlmProviders(llmProviders);
    set({ llmProviders });
  },
  saveProfile: async (profile) => {
    const saved = await api.saveProfile(profile);
    profileMutationVersion += 1;
    set({ profile: saved });
  },
  uploadProfileAvatar: async (file) => {
    const data = await fileToDataUrl(file);
    const profile = await api.uploadProfileAvatar(file.name, data);
    profileMutationVersion += 1;
    rememberLocalImagePreview(profile.avatarPath, data);
    set({ profile });
  },
  clearProfileAvatar: async () => {
    const previousPath = get().profile.avatarPath;
    const profile = await api.clearProfileAvatar();
    profileMutationVersion += 1;
    forgetLocalImagePreview(previousPath);
    set({ profile });
  },
  refreshAccounts: async () => {
    const accounts = await api.listAccounts();
    set({ accounts });
  },
  saveAccounts: async (accounts) => {
    await api.saveAccounts(accounts);
    set({ accounts });
  },
  linkWechatAccount: async (personaId, accountId) => {
    const accounts = await api.linkWechatAccount(personaId, accountId);
    set({ accounts });
  },
  unlinkWechatAccount: async (personaId) => {
    const accounts = await api.unlinkWechatAccount(personaId);
    set({ accounts });
  },
  saveImageProviders: async (imageProviders) => {
    await api.saveImageProviders(imageProviders);
    set({ imageProviders });
  },
  saveVideoProviders: async (videoProviders) => {
    await api.saveVideoProviders(videoProviders);
    set({ videoProviders });
  },
  saveSearchProviders: async (searchProviders) => {
    await api.saveSearchProviders(searchProviders);
    set({ searchProviders });
  },
  saveVisionProviders: async (visionProviders) => {
    await api.saveVisionProviders(visionProviders);
    set({ visionProviders });
  },
  saveBrowserProviders: async (browserProviders) => {
    await api.saveBrowserProviders(browserProviders);
    set({ browserProviders });
  },
  saveThemes: async (themes) => {
    await api.saveThemes(themes);
    set({ themes });
  },
  importThemeCss: async (file) => {
    const buffer = await file.arrayBuffer();
    const bytes = Array.from(new Uint8Array(buffer));
    const themes = await api.importThemeCss(file.name, bytes);
    set({ themes });
  },
  saveEmojiGroups: async (emojiGroups) => {
    await api.saveEmojiGroups(emojiGroups);
    set({ emojiGroups });
  },
  uploadEmojiImage: async (groupId, emotion, file) => {
    const buffer = await file.arrayBuffer();
    const bytes = Array.from(new Uint8Array(buffer));
    const emojiGroups = await api.uploadEmojiImage(groupId, emotion, file.name, bytes);
    set({ emojiGroups });
  },
  refreshMemories: async (personaId) => {
    const memories = await api.listMemories(personaId);
    set({ memories });
  },
  saveMemory: async (memory) => {
    const saved = await api.saveMemory(memory);
    set((state) => ({
      memories: [saved, ...state.memories.filter((item) => item.id !== saved.id)]
        .sort((a, b) => b.createdAt.localeCompare(a.createdAt))
    }));
  },
  deleteMemory: async (id) => {
    await api.deleteMemory(id);
    set((state) => ({ memories: state.memories.filter((memory) => memory.id !== id) }));
  },
  saveWorldbook: async (book) => {
    const saved = await api.saveWorldbook(book);
    set((state) => ({
      worldbooks: [saved, ...state.worldbooks.filter((item) => item.id !== saved.id)]
        .sort((a, b) => a.name.localeCompare(b.name))
    }));
  },
  deleteWorldbook: async (id) => {
    await api.deleteWorldbook(id);
    set((state) => ({ worldbooks: state.worldbooks.filter((book) => book.id !== id) }));
  },
  refreshMoments: async () => {
    const moments = await api.listMoments();
    set({ moments });
  },
  createMoment: async (body) => {
    const post = await api.createMoment(body);
    set((state) => ({ moments: [post, ...state.moments].sort((a, b) => b.createdAt.localeCompare(a.createdAt)) }));
  },
  updateMomentText: async (postId, body) => {
    const post = await api.updateMomentText(postId, body);
    set((state) => ({ moments: state.moments.map((item) => (item.id === post.id ? post : item)) }));
  },
  addMomentComment: async (postId, text) => {
    const post = await api.addMomentComment(postId, text);
    set((state) => ({ moments: state.moments.map((item) => (item.id === post.id ? post : item)) }));
  },
  updateMomentComment: async (postId, commentId, text) => {
    const post = await api.updateMomentComment(postId, commentId, text);
    set((state) => ({ moments: state.moments.map((item) => (item.id === post.id ? post : item)) }));
  },
  deleteMoment: async (postId) => {
    await api.deleteMoment(postId);
    set((state) => ({ moments: state.moments.filter((post) => post.id !== postId) }));
  },
  deleteMomentComment: async (postId, commentId) => {
    const post = await api.deleteMomentComment(postId, commentId);
    set((state) => ({ moments: state.moments.map((item) => (item.id === post.id ? post : item)) }));
  },
  toggleMomentLike: async (postId) => {
    const current = get().moments.find((post) => post.id === postId);
    const post = current?.likedBy.includes("user")
      ? await api.unlikeMoment(postId)
      : await api.likeMoment(postId);
    set((state) => ({ moments: state.moments.map((item) => (item.id === post.id ? post : item)) }));
  },
  uploadMomentCover: async (postId, file) => {
    const buffer = await file.arrayBuffer();
    const bytes = Array.from(new Uint8Array(buffer));
    const post = await api.uploadMomentCover(postId, file.name, bytes);
    set((state) => ({ moments: state.moments.map((item) => (item.id === post.id ? post : item)) }));
  },
  clearMomentCover: async (postId) => {
    const post = await api.clearMomentCover(postId);
    set((state) => ({ moments: state.moments.map((item) => (item.id === post.id ? post : item)) }));
  },
  refreshMcpServers: async () => {
    const mcpServers = await api.listMcpServers();
    set({ mcpServers });
  },
  saveMcpServers: async (mcpServers) => {
    const previous = get().mcpServers;
    set({ mcpServers });
    try {
      await api.saveMcpServers(mcpServers);
    } catch (error) {
      set({ mcpServers: previous });
      throw error;
    }
  },
  refreshCapabilityAdapters: async () => {
    const capabilityAdapters = await api.listCapabilityAdapters();
    set({ capabilityAdapters });
  },
  saveCapabilityAdapters: async (capabilityAdapters) => {
    await api.saveCapabilityAdapters(capabilityAdapters);
    set({ capabilityAdapters });
  },
  refreshAgentConfig: async () => {
    const agentConfig = await api.getAgentConfig();
    set({ agentConfig });
  },
  saveAgentConfig: async (agentConfig) => {
    const saved = await api.saveAgentConfig(agentConfig);
    set((state) => ({
      agentConfig: saved,
      agents: state.agents.map((agent) =>
        agent.isDefault || agent.id === "default"
          ? {
            ...agent,
            enabled: saved.enabled,
            mcpEnabled: saved.mcpEnabled,
            skillsEnabled: saved.skillsEnabled,
            allowShell: saved.allowShell,
            maxSubagents: saved.maxSubagents,
            maxSubagentDepth: saved.maxSubagentDepth,
            maxToolIterations: saved.maxToolIterations,
            skillsDir: saved.skillsDir,
            enabledSkills: saved.enabledSkills,
            enabledMcpServers: saved.enabledMcpServers,
            enabledToolsets: saved.enabledToolsets,
            disabledToolsets: saved.disabledToolsets
          }
          : agent
      )
    }));
  },
  refreshSkills: async () => {
    const skills = await api.listSkills();
    set({ skills: skills as EnhancedSkillSummary[] });
  },
  installBuiltinSkills: async () => {
    const skills = await api.installBuiltinSkills();
    set({ skills: skills as EnhancedSkillSummary[] });
  },
  refreshProactiveStatuses: async () => {
    const proactiveStatuses = await api.listProactiveStatuses();
    set({ proactiveStatuses });
  },
  triggerProactiveOnce: async (personaId) => {
    await api.triggerProactiveOnce(personaId);
    const proactiveStatuses = await api.listProactiveStatuses();
    set({ proactiveStatuses });
  },
  listMcpTools: async (serverId, timeoutSeconds) => {
    const lastMcpToolsResult = await api.listMcpTools(serverId, timeoutSeconds);
    set({ lastMcpToolsResult });
  },
  callMcpTool: async (serverId, toolName, payload, timeoutSeconds) => {
    const lastMcpResult = await api.callMcpTool(serverId, toolName, payload, timeoutSeconds);
    set({ lastMcpResult });
  },
  savePersona: async (persona) => {
    const saved = await api.savePersona(persona);
    personaMutationVersion += 1;
    const agentId = boundAgentId(saved);
    if (agentId) {
      const state = get();
      const agent = state.agents.find((item) => item.id === agentId)
        ?? (await api.listAgents()).find((item) => item.id === agentId);
      if (agent) {
        await api.saveAgent(agentWithPersonaRuntime(agent, saved));
      }
    }
    const [agents, personas, conversations] = await Promise.all([
      api.listAgents(),
      api.listPersonas(),
      api.listConversations()
    ]);
    set((state) => ({
      personas: sortPersonas(personas),
      agents: sortAgents(agents),
      focusedAgentId: normalizeFocusedAgentId(agents, state.focusedAgentId),
      conversations
    }));
    return saved;
  },
  deletePersona: async (id) => {
    await api.deletePersona(id);
    personaMutationVersion += 1;
    set((state) => ({ personas: state.personas.filter((persona) => persona.id !== id) }));
  },
  uploadPersonaAvatar: async (personaId, file) => {
    const data = await fileToDataUrl(file);
    const saved = await api.uploadPersonaAvatar(personaId, file.name, data);
    personaMutationVersion += 1;
    rememberLocalImagePreview(saved.avatarPath, data);
    set((state) => ({ personas: state.personas.map((item) => (item.id === saved.id ? saved : item)) }));
    return saved;
  },
  clearPersonaAvatar: async (personaId) => {
    const previousPath = get().personas.find((item) => item.id === personaId)?.avatarPath;
    const saved = await api.clearPersonaAvatar(personaId);
    personaMutationVersion += 1;
    forgetLocalImagePreview(previousPath);
    set((state) => ({ personas: state.personas.map((item) => (item.id === saved.id ? saved : item)) }));
    return saved;
  },
  saveConfig: async (config) => {
    await api.saveConfig(config);
    set({ config });
  },
  togglePlugin: async (pluginId, enabled) => {
    const plugins = await api.togglePlugin(pluginId, enabled);
    set({ plugins });
  },
  refreshAgents: async () => {
    const agents = await api.listAgents();
    set((state) => ({
      agents,
      focusedAgentId: normalizeFocusedAgentId(agents, state.focusedAgentId)
    }));
  },
  refreshAgentQueue: async () => {
    const agentQueue = await api.listAgentQueue();
    set({ agentQueue });
  },
  refreshAgentRuns: async () => {
    const agentRuns = await api.listAgentRuns();
    set({ agentRuns });
  },
  handleAgentRunEvent: (event) => {
    set((state) => {
      const terminal = event.state === "completed" || event.state === "failed" || event.state === "aborted";
      const activeAgentRuns = { ...state.activeAgentRuns };
      if (terminal || event.parentRunId) {
        delete activeAgentRuns[event.runId];
      } else {
        const prevRun = state.activeAgentRuns[event.runId];
        const workflowGraph = mergeWorkflowGraphFromRunEvent(agentRunWorkflowGraph(prevRun), event);
        activeAgentRuns[event.runId] = {
          ...event,
          accumulatedToolEvents: mergeToolRunEvents(prevRun, event),
          accumulatedPhases: mergeRunPhases(prevRun, event),
          workflowGraph,
          workflow_graph: workflowGraph
        };
      }
      const existingIndex = state.agentRuns.findIndex((run) => run.runId === event.runId);
      const fallbackWorkflowGraph = mergeWorkflowGraphFromRunEvent(null, event);
      const fallbackRun: AgentRunRecord = {
        runId: event.runId,
        conversationId: event.conversationId,
        personaId: event.personaId,
        agentId: event.agentId,
        parentRunId: event.parentRunId ?? null,
        subagentIndex: event.subagentIndex ?? null,
        subagentDepth: event.subagentDepth ?? null,
        subagentCanDelegate: event.subagentCanDelegate ?? null,
        subagentRole: event.subagentRole ?? null,
        subagentTask: event.subagentTask ?? null,
        subagentToolsets: event.subagentToolsets ?? [],
        subagentMaxIterations: event.subagentMaxIterations ?? null,
        queueItemId: event.queueItemId ?? null,
        userRequest: "",
        state: event.state,
        startedAt: event.updatedAt,
        updatedAt: event.updatedAt,
        lastActivityAt: event.lastActivityAt ?? event.updatedAt,
        lastActivityDesc: event.lastActivityDesc ?? null,
        completedAt: terminal ? event.updatedAt : null,
        error: event.error ?? null,
        toolEvents: mergeToolEventList([], event.toolEvent),
        phaseEvents: event.phase ? [{ phase: event.phase, detail: event.detail ?? null, updatedAt: event.updatedAt }] : [],
        checkpoints: [],
        workflowGraph: fallbackWorkflowGraph,
        workflow_graph: fallbackWorkflowGraph
      };
      const agentRuns = existingIndex >= 0
        ? state.agentRuns.map((run, index) => {
          if (index !== existingIndex) return run;
          const workflowGraph = mergeWorkflowGraphFromRunEvent(agentRunWorkflowGraph(run), event);
          return {
            ...run,
            parentRunId: event.parentRunId ?? run.parentRunId ?? null,
            subagentIndex: event.subagentIndex ?? run.subagentIndex ?? null,
            subagentDepth: event.subagentDepth ?? run.subagentDepth ?? null,
            subagentCanDelegate: event.subagentCanDelegate ?? run.subagentCanDelegate ?? null,
            subagentRole: event.subagentRole ?? run.subagentRole ?? null,
            subagentTask: event.subagentTask ?? run.subagentTask ?? null,
            subagentToolsets: event.subagentToolsets ?? run.subagentToolsets ?? [],
            subagentMaxIterations: event.subagentMaxIterations ?? run.subagentMaxIterations ?? null,
            queueItemId: event.queueItemId ?? run.queueItemId ?? null,
            state: event.state,
            updatedAt: event.updatedAt,
            lastActivityAt: event.lastActivityAt ?? run.lastActivityAt ?? event.updatedAt,
            lastActivityDesc: event.lastActivityDesc ?? run.lastActivityDesc ?? null,
            completedAt: terminal ? event.updatedAt : run.completedAt,
            error: event.error ?? run.error,
            toolEvents: mergeToolEventList(run.toolEvents, event.toolEvent),
            phaseEvents: event.phase
              ? [...(run.phaseEvents ?? []), { phase: event.phase, detail: event.detail ?? null, updatedAt: event.updatedAt }].slice(-200)
              : run.phaseEvents,
            workflowGraph,
            workflow_graph: workflowGraph
          };
        })
        : [fallbackRun, ...state.agentRuns];
      const agentQueue = event.queueItemId
        ? state.agentQueue.map((item) => item.id === event.queueItemId
          ? {
            ...item,
            status: terminal ? event.state : "running",
            updatedAt: event.updatedAt,
            startedAt: item.startedAt ?? event.updatedAt,
            completedAt: terminal ? event.updatedAt : item.completedAt,
            error: event.error ?? item.error
          }
          : item)
        : state.agentQueue;
      return { activeAgentRuns, agentRuns, agentQueue };
    });
  },
  handleManagedProcessEvent: (event) => {
    set((state) => {
      const withoutDuplicate = state.managedProcessEvents.filter((item) => {
        if (item.processId !== event.processId || item.type !== event.type) return true;
        if (item.createdAt !== event.createdAt) return true;
        return JSON.stringify(item.detail ?? null) !== JSON.stringify(event.detail ?? null);
      });
      return {
        managedProcessEvents: [event, ...withoutDuplicate]
          .sort((a, b) => b.createdAt.localeCompare(a.createdAt))
          .slice(0, 80)
      };
    });
  },
  saveAgent: async (agent) => {
    const optimisticId = agent.id.trim();
    if (optimisticId) {
      set((state) => ({
        agents: upsertAgentPreservingOrder(state.agents, agent),
        focusedAgentId: optimisticId
      }));
    }
    try {
      const saved = await api.saveAgent(agent);
      const personas = get().personas;
      const boundPersonas = personas.filter((persona) => boundAgentId(persona) === saved.id);
      if (boundPersonas.length > 0) {
        await Promise.all(
          boundPersonas.map((persona) => api.savePersona(personaWithAgentRuntime(persona, saved)))
        );
      }
      const [agents, refreshedPersonas] = await Promise.all([
        api.listAgents(),
        api.listPersonas()
      ]);
      set((state) => ({
        agents: sortAgents(agents),
        personas: sortPersonas(refreshedPersonas),
        focusedAgentId: normalizeFocusedAgentId(agents, saved.id)
      }));
      return agents.find((item) => item.id === saved.id) ?? saved;
    } catch (error) {
      const [agents, personas] = await Promise.all([
        api.listAgents().catch(() => null),
        api.listPersonas().catch(() => null)
      ]);
      if (agents) {
        set((state) => ({
          agents,
          personas: personas ? sortPersonas(personas) : state.personas,
          focusedAgentId: normalizeFocusedAgentId(agents, state.focusedAgentId)
        }));
      }
      throw error;
    }
  },
  deleteAgent: async (id) => {
    await api.deleteAgent(id);
    const state = get();
    const [agents, personas, conversations] = await Promise.all([
      api.listAgents(),
      api.listPersonas(),
      api.listConversations()
    ]);
    const currentActive = state.activeConversationId;
    const activeConversationId = currentActive && conversations.some((item) => item.id === currentActive)
      ? currentActive
      : conversations[0]?.id ?? null;
    const messageLimit = conversationMessageLimit(state.config, activeConversationId, state.conversationMessageLimits);
    const previewChars = uiMessagePreviewChars(state.config);
    const backendMessages = activeConversationId
      ? visibleChatMessages(await api.listMessages(activeConversationId, messageLimit, previewChars))
      : [];
    const messages = mergeBackendMessagesWithLiveState(
      backendMessages,
      state.messages,
      activeConversationId,
      messageLimit,
      state.streamedAssistantIds
    );
    set({
      agents,
      focusedAgentId: normalizeFocusedAgentId(agents, state.focusedAgentId === id ? null : state.focusedAgentId),
      personas,
      conversations,
      activeConversationId,
      messages
    });
  },
  refreshSkillsForAgent: async (agentId) => {
    const skills = await api.listSkillsForAgent(agentId);
    set({ skills: skills });
  },
  saveSkillConfig: async (agentId, skillId, config) => {
    await api.saveSkillConfig(agentId, skillId, config);
  },
  refreshSkillBundles: async () => {
    const skillBundles = await api.listSkillBundles();
    set({ skillBundles });
  },
  installSkillBundle: async (bundleId, agentId) => {
    const skills = await api.installSkillBundle(bundleId, agentId);
    set({ skills });
  },
  refreshMarketplaceSkills: async (query, source) => {
    const marketplaceSkills = source && source !== "local"
      ? await api.searchSkillMarketplace(query, source)
      : await api.listMarketplaceSkills(query);
    set({ marketplaceSkills });
  },
  installMarketplaceSkill: async (skillId, agentId) => {
    const skill = await api.installMarketplaceSkill(skillId, agentId);
    if (skill) {
      set((state) => ({ skills: [...state.skills.filter((item) => item.id !== skill.id), skill] }));
    }
  },
  installExternalSkillUrl: async (url, name, category, agentId, force) => {
    const skill = await api.installExternalSkillUrl(url, name, category, agentId, force);
    if (skill) {
      set((state) => ({
        skills: [...state.skills.filter((item) => item.id !== skill.id), skill as EnhancedSkillSummary]
      }));
    }
  }
}));
