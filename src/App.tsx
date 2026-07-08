import { ChangeEvent, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import {
  Bot,
  BookOpen,
  Brain,
  Camera,
  Check,
  ChevronRight,
  Compass,
  Edit3,
  Copy,
  ExternalLink,
  FolderOpen,
  Globe,
  Heart,
  Image,
  ImagePlus,
  Info,
  Pencil,
  Plus,
  MessageSquareText,
  Maximize2,
  Newspaper,
  Palette,
  PlugZap,
  Puzzle,
  Search,
  Send,
  Settings,
  Smartphone,
  Smile,
  Sparkles,
  Trash2,
  Upload,
  Wand2,
  Users,
  X
} from "lucide-react";
import { useAppStore } from "./lib/store";
import {
  maskSecret,
  formatTime,
  providerPresetLabel,
  providerPresetDefaults,
  imageProviderTypeLabel,
} from "./lib/formatters";
import { parseToolEvent } from "./lib/toolEventUtils";

import { api } from "./lib/api";
import { emit, emitTo, listen } from "@tauri-apps/api/event";
import { Avatar, MenuRow } from "./components/common";
import { PersonaMemoryManager } from "./components/PersonaMemoryManager";
import { resolvePersonaAgentBinding } from "./lib/personaAgentBinding";
import { SettingsPanel } from "./panels/SettingsPanel";
import { MemoryPanel, WorldbooksPanel, PluginsPanel } from "./panels/ToolPanels";
import { McpExtensionPanel } from "./panels/McpExtensionPanel";
import { SkillsCenterPanel } from "./panels/SkillsCenterPanel";
import { AgentsManagerPanel } from "./panels/AgentsManagerPanel";
import { MomentsPanel } from "./panels/MomentsPanel";
import { ContactsPanel } from "./panels/ContactsPanel";
import { DiscoverPanel } from "./panels/DiscoverPanel";
import { PersonaPanel } from "./panels/PersonaPanel";
import { ChatExperience } from "./panels/ChatExperience";
import { EnvironmentCheck } from "./panels/EnvironmentCheck";
import {
  PET_ACTIVE_CONTEXT_EVENT,
  PET_THINKING_STATE_EVENT,
  publishPetThinkingState,
  writeStoredPetActiveContext,
  type PetActiveContext,
  type PetThinkingState
} from "./lib/petContext";
import "./styles.css";
import "./panels-beautiful.css";
import type {
  AccountConfig,
  AgentConfig,
  AgentQueuedRequest,
  AgentRunEvent,
  AppSection,
  CapabilityAdapter,
  ChatMessage,
  EmojiGroup,
  ImageProvider,
  LlmProvider,
  ManagedProcessEvent,
  McpServer,
  MomentComment,
  MomentPost,
  Persona,
  ProfileConfig,
  SkillSummary,
  ThemeConfig,
  ToolEvent,
  ToolEventEnvelope,
  WechatConfig,
  WechatQrStartResult,
  WechatQrStatusResult,
  Worldbook
} from "./lib/types";

const navItems: Array<{ id: AppSection; label: string; icon: typeof MessageSquareText }> = [
  { id: "chat", label: "聊天", icon: MessageSquareText },
  { id: "contacts", label: "通讯录", icon: Users },
  { id: "discover", label: "发现", icon: Compass },
  { id: "personas", label: "角色", icon: Bot },
  { id: "moments", label: "朋友圈", icon: Newspaper },
  { id: "memory", label: "记忆", icon: Brain },
  { id: "worldbooks", label: "世界书", icon: BookOpen },
  { id: "plugins", label: "插件", icon: Puzzle },
  { id: "mcp", label: "工具", icon: PlugZap },
  { id: "agents", label: "智能体", icon: Sparkles },
  { id: "skills", label: "技能", icon: Wand2 },
  { id: "settings", label: "设置", icon: Settings }
];

const primaryNavItems = navItems.filter((item) =>
  ["chat", "contacts", "discover", "settings"].includes(item.id)
);



function isVisibleChatEventMessage(message: ChatMessage) {
  if (message.source === "desktop-agent-error") return false;
  const providerData = message.providerData;
  const record = providerData && typeof providerData === "object" && !Array.isArray(providerData)
    ? providerData as Record<string, unknown>
    : null;
  if (
    record?.silent === true
    && (message.source === "pet-vision" || record.source === "pet-vision" || record.visibility === "pet-only")
  ) {
    return false;
  }
  return !(message.role === "user" && message.source === "proactive-internal");
}

function isMemoryWriteToolEvent(event?: ToolEvent | null) {
  if (!event || event.ok === false || event.status === "running") return false;
  const toolName = event.toolName.trim();
  return ["remember_fact", "fact_store", "manage_memory", "memory"].includes(toolName);
}


const WECHAT_THINKING_MIN_VISIBLE_MS = 900;
const WECHAT_REPLY_INSERT_DEFER_MS = 750;


export function App() {
  const [envCheckDone, setEnvCheckDone] = useState(false);
  const config = useAppStore((state) => state.config);
  const activeSection = useAppStore((state) => state.activeSection);
  const setSection = useAppStore((state) => state.setSection);
  const bootstrap = useAppStore((state) => state.bootstrap);
  const refreshChatData = useAppStore((state) => state.refreshChatData);
  const refreshAgents = useAppStore((state) => state.refreshAgents);
  const refreshSkills = useAppStore((state) => state.refreshSkills);
  const refreshAgentQueue = useAppStore((state) => state.refreshAgentQueue);
  const refreshAgentRuns = useAppStore((state) => state.refreshAgentRuns);
  const refreshMemories = useAppStore((state) => state.refreshMemories);
  const setConversationProcessing = useAppStore((state) => state.setConversationProcessing);
  const incrementConversationUnread = useAppStore((state) => state.incrementConversationUnread);
  const handleAgentRunEvent = useAppStore((state) => state.handleAgentRunEvent);
  const handleManagedProcessEvent = useAppStore((state) => state.handleManagedProcessEvent);
  const upsertIncomingMessage = useAppStore((state) => state.upsertIncomingMessage);
  const clearStreamingAssistantMessages = useAppStore((state) => state.clearStreamingAssistantMessages);
  const refreshPersonasFromBackend = useCallback(async () => {
    const personas = await api.listPersonas();
    useAppStore.setState({ personas });
  }, []);
  const processingConversationIds = useAppStore((state) => state.processingConversationIds);
  const processingConversationCount = useAppStore((state) => state.processingConversationIds.length);
  const hasChatUnread = useAppStore((state) => Object.values(state.conversationUnreadCounts).some((count) => count > 0));
  const conversations = useAppStore((state) => state.conversations);
  const personas = useAppStore((state) => state.personas);
  const activeConversationId = useAppStore((state) => state.activeConversationId);
  const themes = useAppStore((state) => state.themes);
  const lastCountedMessageRef = useRef<Map<string, string>>(new Map());
  const processingStartedAtRef = useRef<Map<string, number>>(new Map());
  const activeWechatTurnRef = useRef<Set<string>>(new Set());
  const visibleWechatUserRef = useRef<Set<string>>(new Set());
  const deferredWechatMessagesRef = useRef<Map<string, Array<{ message: ChatMessage; personaId: string | null }>>>(new Map());
  const deferredWechatTimerRef = useRef<Map<string, number>>(new Map());
  const chatRefreshTimersRef = useRef<Map<string, number>>(new Map());
  const chatRefreshInFlightRef = useRef<Set<string>>(new Set());
  const pendingStreamMessagesRef = useRef<Map<string, {
    message: ChatMessage;
    streaming: boolean;
    final: boolean;
  }>>(new Map());
  const streamFlushTimerRef = useRef<number | null>(null);
  const agentQueueRefreshTimerRef = useRef<number | null>(null);
  const agentQueueRefreshInFlightRef = useRef(false);
  const agentRunsRefreshTimerRef = useRef<number | null>(null);
  const agentRunsRefreshInFlightRef = useRef(false);
  const memoriesRefreshTimerRef = useRef<number | null>(null);
  const memoriesRefreshInFlightRef = useRef(false);
  const bridgedProcessingIdsRef = useRef<Set<string>>(new Set());

  const scheduleChatRefresh = useCallback((
    conversationId?: string | null,
    personaId?: string | null,
    delayMs = 140
  ) => {
    const key = `${conversationId ?? ""}\u0000${personaId ?? ""}`;
    const existing = chatRefreshTimersRef.current.get(key);
    if (existing !== undefined) {
      window.clearTimeout(existing);
    }
    const timer = window.setTimeout(() => {
      chatRefreshTimersRef.current.delete(key);
      if (chatRefreshInFlightRef.current.has(key)) {
        scheduleChatRefresh(conversationId ?? null, personaId ?? null, delayMs);
        return;
      }
      chatRefreshInFlightRef.current.add(key);
      void refreshChatData(conversationId ?? null, personaId ?? null).finally(() => {
        chatRefreshInFlightRef.current.delete(key);
      });
    }, Math.max(0, delayMs));
    chatRefreshTimersRef.current.set(key, timer);
  }, [refreshChatData]);

  const scheduleAgentQueueRefresh = useCallback((delayMs = 180) => {
    if (agentQueueRefreshTimerRef.current !== null) {
      window.clearTimeout(agentQueueRefreshTimerRef.current);
    }
    agentQueueRefreshTimerRef.current = window.setTimeout(() => {
      agentQueueRefreshTimerRef.current = null;
      if (agentQueueRefreshInFlightRef.current) {
        scheduleAgentQueueRefresh(delayMs);
        return;
      }
      agentQueueRefreshInFlightRef.current = true;
      void refreshAgentQueue().finally(() => {
        agentQueueRefreshInFlightRef.current = false;
      });
    }, Math.max(0, delayMs));
  }, [refreshAgentQueue]);

  const scheduleAgentRunsRefresh = useCallback((delayMs = 220) => {
    if (agentRunsRefreshTimerRef.current !== null) {
      window.clearTimeout(agentRunsRefreshTimerRef.current);
    }
    agentRunsRefreshTimerRef.current = window.setTimeout(() => {
      agentRunsRefreshTimerRef.current = null;
      if (agentRunsRefreshInFlightRef.current) {
        scheduleAgentRunsRefresh(delayMs);
        return;
      }
      agentRunsRefreshInFlightRef.current = true;
      void refreshAgentRuns().finally(() => {
        agentRunsRefreshInFlightRef.current = false;
      });
    }, Math.max(0, delayMs));
  }, [refreshAgentRuns]);

  const scheduleMemoriesRefresh = useCallback((delayMs = 500) => {
    if (memoriesRefreshTimerRef.current !== null) {
      window.clearTimeout(memoriesRefreshTimerRef.current);
    }
    memoriesRefreshTimerRef.current = window.setTimeout(() => {
      memoriesRefreshTimerRef.current = null;
      if (memoriesRefreshInFlightRef.current) {
        scheduleMemoriesRefresh(delayMs);
        return;
      }
      memoriesRefreshInFlightRef.current = true;
      void refreshMemories().finally(() => {
        memoriesRefreshInFlightRef.current = false;
      });
    }, Math.max(0, delayMs));
  }, [refreshMemories]);

  const flushPendingStreamMessages = useCallback(() => {
    if (streamFlushTimerRef.current !== null) {
      window.clearTimeout(streamFlushTimerRef.current);
      streamFlushTimerRef.current = null;
    }
    const pending = Array.from(pendingStreamMessagesRef.current.values());
    pendingStreamMessagesRef.current.clear();
    for (const item of pending) {
      upsertIncomingMessage(item.message, {
        streaming: item.streaming && !item.final,
        final: item.final
      });
    }
  }, [upsertIncomingMessage]);

  const scheduleStreamMessageUpsert = useCallback((
    message: ChatMessage,
    options: { streaming?: boolean; final?: boolean },
    immediate = false
  ) => {
    const key = message.id.trim() || `${message.conversationId}:${message.role}`;
    const previous = pendingStreamMessagesRef.current.get(key);
    pendingStreamMessagesRef.current.set(key, {
      message,
      streaming: Boolean(options.streaming),
      final: Boolean(options.final || previous?.final)
    });
    if (immediate || options.final) {
      flushPendingStreamMessages();
      return;
    }
    if (streamFlushTimerRef.current !== null) return;
    streamFlushTimerRef.current = window.setTimeout(() => {
      flushPendingStreamMessages();
    }, 60);
  }, [flushPendingStreamMessages]);

  const discardPendingStreamMessagesForConversation = useCallback((conversationId: string) => {
    let removed = false;
    pendingStreamMessagesRef.current.forEach((item, key) => {
      if (item.message.conversationId === conversationId) {
        pendingStreamMessagesRef.current.delete(key);
        removed = true;
      }
    });
    if (removed && pendingStreamMessagesRef.current.size === 0 && streamFlushTimerRef.current !== null) {
      window.clearTimeout(streamFlushTimerRef.current);
      streamFlushTimerRef.current = null;
    }
  }, []);

  useEffect(() => () => {
    chatRefreshTimersRef.current.forEach((timer) => window.clearTimeout(timer));
    chatRefreshTimersRef.current.clear();
    chatRefreshInFlightRef.current.clear();
    if (streamFlushTimerRef.current !== null) window.clearTimeout(streamFlushTimerRef.current);
    streamFlushTimerRef.current = null;
    pendingStreamMessagesRef.current.clear();
    if (agentQueueRefreshTimerRef.current !== null) window.clearTimeout(agentQueueRefreshTimerRef.current);
    if (agentRunsRefreshTimerRef.current !== null) window.clearTimeout(agentRunsRefreshTimerRef.current);
    if (memoriesRefreshTimerRef.current !== null) window.clearTimeout(memoriesRefreshTimerRef.current);
    agentQueueRefreshTimerRef.current = null;
    agentRunsRefreshTimerRef.current = null;
    memoriesRefreshTimerRef.current = null;
    agentQueueRefreshInFlightRef.current = false;
    agentRunsRefreshInFlightRef.current = false;
    memoriesRefreshInFlightRef.current = false;
  }, []);

  const showConversationProcessing = useCallback((
    conversationId: string,
    personaId?: string | null,
    follow = false,
    switchSection = false
  ) => {
    if (!conversationId) return;
    processingStartedAtRef.current.set(conversationId, Date.now());
    if (switchSection) {
      setSection("chat");
    }
    setConversationProcessing(conversationId, true);
    if (follow) {
      scheduleChatRefresh(conversationId, personaId ?? null, 220);
    }
  }, [scheduleChatRefresh, setConversationProcessing, setSection]);

  const hideConversationProcessing = useCallback((conversationId: string) => {
    if (!conversationId) return;
    const startedAt = processingStartedAtRef.current.get(conversationId);
    const delay = startedAt
      ? Math.max(0, WECHAT_THINKING_MIN_VISIBLE_MS - (Date.now() - startedAt))
      : 0;
    window.setTimeout(() => {
      if (startedAt && processingStartedAtRef.current.get(conversationId) !== startedAt) return;
      processingStartedAtRef.current.delete(conversationId);
      setConversationProcessing(conversationId, false);
    }, delay);
  }, [setConversationProcessing]);

  const flushDeferredWechatMessages = useCallback((conversationId: string) => {
    const timer = deferredWechatTimerRef.current.get(conversationId);
    if (timer !== undefined) {
      window.clearTimeout(timer);
      deferredWechatTimerRef.current.delete(conversationId);
    }
    const pending = deferredWechatMessagesRef.current.get(conversationId);
    if (!pending || pending.length === 0) return;
    deferredWechatMessagesRef.current.delete(conversationId);
    pending.forEach((item) => upsertIncomingMessage(item.message));
  }, [upsertIncomingMessage]);

  const scheduleWechatFallbackRefresh = useCallback((conversationId: string, personaId?: string | null) => {
    const timer = deferredWechatTimerRef.current.get(conversationId);
    if (timer !== undefined) {
      window.clearTimeout(timer);
    }
    const nextTimer = window.setTimeout(() => {
      deferredWechatTimerRef.current.delete(conversationId);
      flushDeferredWechatMessages(conversationId);
      activeWechatTurnRef.current.delete(conversationId);
      visibleWechatUserRef.current.delete(conversationId);
      scheduleChatRefresh(conversationId, personaId ?? null);
    }, WECHAT_REPLY_INSERT_DEFER_MS);
    deferredWechatTimerRef.current.set(conversationId, nextTimer);
  }, [flushDeferredWechatMessages, scheduleChatRefresh]);

  const deferWechatTurnMessage = useCallback((
    conversationId: string,
    personaId: string | null | undefined,
    message: ChatMessage
  ) => {
    const pending = deferredWechatMessagesRef.current.get(conversationId) ?? [];
    const entry = { message, personaId: personaId ?? null };
    const existingIndex = pending.findIndex((item) => item.message.id === message.id);
    if (existingIndex >= 0) {
      pending[existingIndex] = entry;
    } else {
      pending.push(entry);
    }
    deferredWechatMessagesRef.current.set(conversationId, pending);
    scheduleWechatFallbackRefresh(conversationId, personaId ?? null);
  }, [scheduleWechatFallbackRefresh]);

  useEffect(() => {
    void bootstrap();
  }, [bootstrap]);

  useEffect(() => {
    if (!activeConversationId) return;
    const conversation = conversations.find((item) => item.id === activeConversationId);
    if (!conversation) return;
    const persona = conversation.personaId
      ? personas.find((item) => item.id === conversation.personaId) ?? null
      : null;
    const context: PetActiveContext = {
      conversationId: conversation.id,
      conversationTitle: conversation.title,
      personaId: conversation.personaId ?? persona?.id ?? null,
      personaName: persona?.name ?? null,
      agentId: conversation.agentId ?? persona?.agentId ?? null,
      updatedAt: new Date().toISOString(),
      source: "main"
    };
    writeStoredPetActiveContext(context);
    void emit(PET_ACTIVE_CONTEXT_EVENT, context);
  }, [activeConversationId, conversations, personas]);

  useEffect(() => {
    const previous = bridgedProcessingIdsRef.current;
    const next = new Set(processingConversationIds);
    next.forEach((conversationId) => {
      if (previous.has(conversationId)) return;
      const conversation = conversations.find((item) => item.id === conversationId);
      const payload: PetThinkingState = {
        source: "desktop",
        personaId: conversation?.personaId ?? null,
        conversationId,
        thinking: true,
        updatedAt: new Date().toISOString()
      };
      publishPetThinkingState(payload);
      void emit(PET_THINKING_STATE_EVENT, payload).catch(() => undefined);
      void emitTo("pet", PET_THINKING_STATE_EVENT, payload).catch(() => undefined);
      void emitTo("pet", "synthchat-pet-event", {
        type: "thinking_started",
        source: payload.source,
        personaId: payload.personaId,
        conversationId
      }).catch(() => undefined);
    });
    previous.forEach((conversationId) => {
      if (next.has(conversationId)) return;
      // The desktop chat row owns the exact hide timing, including its
      // minimum-visible delay and streaming overlap. Avoid clearing the pet
      // bubble here before ChatExperience publishes desktop-ui=false.
    });
    bridgedProcessingIdsRef.current = next;
  }, [conversations, processingConversationIds]);

  useEffect(() => {
    const tick = async () => {
      const due = await api.tickScheduledAgentJobs();
      if (due.length > 0) {
        await refreshChatData(null, null);
      }
    };
    void tick();
    const timer = window.setInterval(() => {
      void tick();
    }, 60_000);
    return () => window.clearInterval(timer);
  }, [refreshChatData]);

  useEffect(() => {
    if (activeSection !== "contacts") return;
    const timer = window.setInterval(() => {
      if (processingConversationCount > 0) return;
      void refreshChatData(null, null);
    }, 5000);
    return () => window.clearInterval(timer);
  }, [activeSection, processingConversationCount, refreshChatData]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listen<{
      type: string;
      source?: string;
      personaId?: string;
      conversationId?: string;
      message?: ChatMessage;
      delta?: string;
      isLast?: boolean;
      ok?: boolean;
      error?: string;
    }>("synthchat-chat-event", (event) => {
      const payload = event.payload;
      const eventSource = payload.source ?? "";
      const messageSource = payload.message?.source ?? eventSource;
      const isWechatEvent = eventSource === "wechat" || payload.message?.source === "wechat";
      const isActiveWechatTurn = Boolean(
        payload.conversationId && activeWechatTurnRef.current.has(payload.conversationId)
      );
      const isWechatTurnEvent = isWechatEvent || isActiveWechatTurn;
      const externalSource =
        messageSource !== "desktop"
        && messageSource !== "desktop-control"
        && messageSource !== "proactive-internal";

      // Authoritative thinking lifecycle: the backend emits turn_started /
      // turn_finished for EVERY user-facing turn (desktop, pet, wechat,
      // proactive), in order, from the single hub (run_chat_turn). The desktop
      // drives its "thinking" UI solely from this pair and auto-follows the
      // turn's conversation, so timing is uniform across all sources.
      if (payload.type === "turn_started" && payload.conversationId) {
        if (isWechatEvent) {
          const timer = deferredWechatTimerRef.current.get(payload.conversationId);
          if (timer !== undefined) {
            window.clearTimeout(timer);
            deferredWechatTimerRef.current.delete(payload.conversationId);
          }
          deferredWechatMessagesRef.current.delete(payload.conversationId);
          activeWechatTurnRef.current.add(payload.conversationId);
        }
        const shouldSwitchSection = externalSource && messageSource !== "pet" && messageSource !== "wechat";
        showConversationProcessing(
          payload.conversationId,
          payload.personaId ?? null,
          true,
          shouldSwitchSection
        );
        return;
      }
      if (payload.type === "turn_finished" && payload.conversationId) {
        hideConversationProcessing(payload.conversationId);
        if (payload.ok === false && !isWechatTurnEvent) {
          discardPendingStreamMessagesForConversation(payload.conversationId);
          clearStreamingAssistantMessages(payload.conversationId);
          scheduleChatRefresh(payload.conversationId ?? null, payload.personaId ?? null, 180);
          return;
        }
        if (isWechatEvent) {
          if (
            payload.message
            && isVisibleChatEventMessage(payload.message)
            && (payload.message.role === "assistant" || payload.message.role === "tool")
          ) {
            if (visibleWechatUserRef.current.has(payload.conversationId)) {
              upsertIncomingMessage(payload.message);
            } else {
              deferWechatTurnMessage(payload.conversationId, payload.personaId, payload.message);
            }
          }
          scheduleWechatFallbackRefresh(payload.conversationId, payload.personaId ?? null);
          return;
        }
        if (
          payload.message
          && isVisibleChatEventMessage(payload.message)
          && (payload.message.role === "assistant" || payload.message.role === "tool")
        ) {
          upsertIncomingMessage(payload.message, { final: payload.message.role === "assistant" });
        }
        scheduleChatRefresh(payload.conversationId ?? null, payload.personaId ?? null, 180);
        return;
      }
      const isMessageEvent =
        payload.type === "assistant_stream"
        || payload.type === "assistant_thinking_stream"
        || payload.type === "new_message"
        || payload.type === "tool_message"
        || payload.type === "assistant_message";
      const isVisibleMessageEvent =
        Boolean(payload.conversationId && payload.message && isVisibleChatEventMessage(payload.message));
      let appliedInlineVisibleMessage = false;
      if (isMessageEvent && payload.conversationId && payload.message && isVisibleMessageEvent) {
        if (payload.message.role === "user" && messageSource === "desktop") {
          // Desktop sends its own user messages and handles optimistic UI locally.
          return;
        }
        const isWechatUserMessage = payload.message.role === "user" && isWechatEvent;
        if (isWechatUserMessage) {
          visibleWechatUserRef.current.add(payload.conversationId);
        }
        const shouldDeferWechatTurnMessage =
          isWechatTurnEvent
          && !visibleWechatUserRef.current.has(payload.conversationId)
          && (payload.message.role === "assistant" || payload.message.role === "tool");
        if (shouldDeferWechatTurnMessage) {
          deferWechatTurnMessage(payload.conversationId, payload.personaId, payload.message);
          return;
        }
        if (payload.type === "assistant_message" || payload.type === "new_message") {
          const state = useAppStore.getState();
          const shouldMarkUnread =
            payload.conversationId !== state.activeConversationId
            || state.activeSection !== "chat";
          const messageKey = payload.message.id.trim() || `${payload.message.createdAt}:${payload.message.content.length}`;
          const previousKey = lastCountedMessageRef.current.get(payload.conversationId);
          if (shouldMarkUnread && previousKey !== messageKey) {
            lastCountedMessageRef.current.set(payload.conversationId, messageKey);
            incrementConversationUnread(payload.conversationId);
          }
        }
        const streamOptions = {
          streaming: (
            (payload.type === "assistant_stream" || payload.type === "assistant_thinking_stream")
            && payload.message.role === "assistant"
            && !payload.isLast
          ),
          final: (
            (payload.type === "assistant_message" || (payload.type === "assistant_stream" && payload.isLast))
            && payload.message.role === "assistant"
          )
        };
        if (payload.message.role === "assistant" && streamOptions.final) {
          scheduleStreamMessageUpsert(payload.message, streamOptions, true);
        } else if (
          payload.message.role === "assistant"
          && (payload.type === "assistant_stream" || payload.type === "assistant_thinking_stream")
        ) {
          scheduleStreamMessageUpsert(payload.message, streamOptions, Boolean(payload.isLast));
        } else {
          upsertIncomingMessage(payload.message, streamOptions);
        }
        appliedInlineVisibleMessage = true;
        if (isWechatUserMessage) {
          flushDeferredWechatMessages(payload.conversationId);
        }
      }
      if (payload.type === "assistant_stream" || payload.type === "assistant_thinking_stream") {
        return;
      }
      if (payload.type === "tool_message") {
        return;
      }
      if (
        appliedInlineVisibleMessage
        && (messageSource === "pet" || payload.message?.role === "assistant")
        && (payload.type === "new_message" || payload.type === "assistant_message")
      ) {
        // The live message has already been inserted. A same-tick refresh can
        // read a stale backend snapshot and temporarily wipe or downgrade the
        // streaming bubble before the final event catches up.
        return;
      }
      if (
        payload.type === "conversation_updated"
        && payload.conversationId
        && isWechatEvent
        && !visibleWechatUserRef.current.has(payload.conversationId)
      ) {
        scheduleWechatFallbackRefresh(payload.conversationId, payload.personaId ?? null);
        return;
      }
      if (payload.type === "new_message" || payload.type === "assistant_message" || payload.type === "conversation_updated") {
        scheduleChatRefresh(payload.conversationId ?? null, payload.personaId ?? null);
      }
    }).then((handler) => {
      unlisten = handler;
    });
    return () => {
      if (unlisten) unlisten();
      deferredWechatTimerRef.current.forEach((timer) => window.clearTimeout(timer));
      deferredWechatTimerRef.current.clear();
    };
  }, [
    deferWechatTurnMessage,
    discardPendingStreamMessagesForConversation,
    flushDeferredWechatMessages,
    hideConversationProcessing,
    incrementConversationUnread,
    refreshChatData,
    scheduleWechatFallbackRefresh,
    scheduleChatRefresh,
    scheduleStreamMessageUpsert,
    clearStreamingAssistantMessages,
    setConversationProcessing,
    showConversationProcessing,
    upsertIncomingMessage
  ]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listen<{
      type: string;
      personaId?: string;
      persona?: Persona;
    }>("synthchat-persona-event", (event) => {
      const payload = event.payload;
      if (payload.type !== "persona_updated") return;
      if (payload.persona) {
        useAppStore.setState((state) => ({
          personas: [payload.persona!, ...state.personas.filter((item) => item.id !== payload.persona!.id)]
            .sort((a, b) => a.name.localeCompare(b.name))
        }));
        return;
      }
      void refreshPersonasFromBackend();
    }).then((handler) => {
      unlisten = handler;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, [refreshPersonasFromBackend]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listen<AgentRunEvent>("synthchat-agent-run-event", (event) => {
      const payload = event.payload;
      handleAgentRunEvent(payload);
      if (payload.phase === "memory_write_observed" || isMemoryWriteToolEvent(payload.toolEvent)) {
        scheduleMemoriesRefresh();
      }
      // Processing visibility is owned by the authoritative turn_started /
      // turn_finished lifecycle (see synthchat-chat-event handler).
      if (payload.message) {
        upsertIncomingMessage(payload.message);
      }
      scheduleAgentQueueRefresh();
      if (payload.state === "completed" || payload.state === "failed" || payload.state === "aborted") {
        scheduleAgentRunsRefresh();
        scheduleChatRefresh(payload.conversationId, payload.personaId);
      }
    }).then((handler) => {
      unlisten = handler;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, [handleAgentRunEvent, scheduleAgentQueueRefresh, scheduleAgentRunsRefresh, scheduleChatRefresh, scheduleMemoriesRefresh, setConversationProcessing, upsertIncomingMessage]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listen<{ type?: string; item?: AgentQueuedRequest | null }>("synthchat-agent-queue-event", (event) => {
      const item = event.payload.item;
      if (item) {
        useAppStore.setState((state) => ({
          agentQueue: [item, ...state.agentQueue.filter((entry) => entry.id !== item.id)]
            .sort((a, b) => a.createdAt.localeCompare(b.createdAt))
        }));
      }
      scheduleAgentQueueRefresh(120);
    }).then((handler) => {
      unlisten = handler;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, [scheduleAgentQueueRefresh]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listen<{ type: string; conversationId?: string | null }>("synthchat-agent-goal-event", (event) => {
      scheduleAgentQueueRefresh();
      scheduleAgentRunsRefresh();
      scheduleChatRefresh(event.payload.conversationId ?? null, null);
    }).then((handler) => {
      unlisten = handler;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, [scheduleAgentQueueRefresh, scheduleAgentRunsRefresh, scheduleChatRefresh]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listen<ManagedProcessEvent>("synthchat-managed-process-event", (event) => {
      const payload = event.payload;
      handleManagedProcessEvent(payload);
      if (payload.conversationId) {
        scheduleChatRefresh(payload.conversationId, null);
      }
      scheduleAgentRunsRefresh();
      scheduleAgentQueueRefresh();
    }).then((handler) => {
      unlisten = handler;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, [handleManagedProcessEvent, scheduleAgentQueueRefresh, scheduleAgentRunsRefresh, scheduleChatRefresh]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listen("synthchat-skills-changed", () => {
      void refreshSkills();
      void refreshAgents();
    }).then((handler) => {
      unlisten = handler;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, [refreshAgents, refreshSkills]);

  useEffect(() => {
    const styleId = "synthchat-active-theme";
    let style = document.getElementById(styleId) as HTMLStyleElement | null;
    if (!style) {
      style = document.createElement("style");
      style.id = styleId;
      document.head.appendChild(style);
    }
    style.textContent = themes.filter((theme) => theme.active).map((theme) => theme.css).join("\n");
  }, [themes]);

  useEffect(() => {
    const rawMode = themes[0]?.mode ?? "light";
    const resolveMode = (m: string): "light" | "dark" => {
      if (m === "auto") {
        return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
      }
      return m as "light" | "dark";
    };
    document.documentElement.setAttribute("data-theme", resolveMode(rawMode));

    if (rawMode !== "auto") return;

    const mql = window.matchMedia("(prefers-color-scheme: dark)");
    const handler = () => {
      document.documentElement.setAttribute("data-theme", resolveMode("auto"));
    };
    mql.addEventListener("change", handler);
    return () => mql.removeEventListener("change", handler);
  }, [themes]);

  const skipEnvCheck = config?.chat?.skipEnvCheck ?? false;
  if (!envCheckDone && !skipEnvCheck) {
    return <EnvironmentCheck onComplete={() => setEnvCheckDone(true)} />;
  }

  return (
    <main className="app-shell">
      <aside className="sidebar">
        <div className="brand">
          <div className="brand-mark">
            <Sparkles size={18} />
          </div>
          <div>
            <strong>SynthChat</strong>
            <span>clean rebuild</span>
          </div>
        </div>

        <nav className="nav-list" aria-label="主导航">
          {primaryNavItems.map((item) => {
            const Icon = item.icon;
            return (
              <button
                className={activeSection === item.id ? "nav-item active" : "nav-item"}
                key={item.id}
                onClick={() => setSection(item.id)}
                type="button"
                title={item.label}
              >
                <Icon size={20} strokeWidth={activeSection === item.id ? 2.2 : 1.8} />
                <span>{item.label}</span>
                {item.id === "chat" && hasChatUnread ? <i aria-hidden="true" className="nav-item-dot" /> : null}
              </button>
            );
          })}
        </nav>

      </aside>

      <section className="workspace">
        <Header />
        <Content />
      </section>
    </main>
  );
}

function Header() {
  const activeSection = useAppStore((state) => state.activeSection);
  const title = navItems.find((item) => item.id === activeSection)?.label ?? "SynthChat";
  return (
    <header className="workspace-header">
      <div>
        <h1>{title}</h1>
      </div>
    </header>
  );
}

function Content() {
  const activeSection = useAppStore((state) => state.activeSection);
  return (
    <div className="workspace-content">
      <div
        aria-hidden={activeSection !== "chat"}
        className={activeSection === "chat" ? "workspace-panel active" : "workspace-panel hidden"}
      >
        <ChatExperience />
      </div>
      {activeSection !== "chat" ? (
        <div className="workspace-panel active" key={activeSection}>
          <ActivePanel section={activeSection} />
        </div>
      ) : null}
    </div>
  );
}

function ActivePanel({ section }: { section: AppSection }) {
  if (section === "contacts") return <ContactsPanel />;
  if (section === "discover") return <DiscoverPanel />;
  if (section === "moments") return <MomentsPanel />;
  if (section === "personas") return <PersonaPanel />;
  if (section === "mcp") return <McpExtensionPanel />;
  if (section === "settings") return <SettingsPanel />;
  if (section === "memory") return <MemoryPanel />;
  if (section === "worldbooks") return <WorldbooksPanel />;
  if (section === "plugins") return <PluginsPanel />;
  if (section === "agents") return <AgentsManagerPanel />;
  if (section === "skills") return <SkillsCenterPanel />;
  return null;
}

