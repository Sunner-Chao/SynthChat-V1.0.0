import { memo, useCallback, useDeferredValue, useEffect, useLayoutEffect, useMemo, useRef, useState, type DragEvent as ReactDragEvent, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { emit, emitTo, listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  AlertCircle,
  Bot,
  Brain,
  CheckCircle2,
  ChevronDown,
  ChevronRight,
  Circle,
  Clock,
  Code2,
  Copy,
  Eye,
  FileText,
  FolderOpen,
  Image as ImageIcon,
  Layers,
  Loader2,
  MessageSquareText,
  Mic,
  MicOff,
  Network,
  PanelRightClose,
  PanelRightOpen,
  Paperclip,
  Plus,
  RefreshCw,
  Search,
  SendHorizontal,
  Smile,
  Settings2,
  Sparkles,
  Square,
  Terminal,
  Trash2,
  Wrench,
  Zap,
  X
} from "lucide-react";
import { api, isTauri } from "../lib/api";
import {
  agentLabel,
  compactRunText,
  formatDurationMs,
  formatTime,
  isTerminalRunState,
  queueStatusLabel,
  runtimePayloadRecord,
  runningToolStartTimesFromPhases,
  runPhaseLabel,
  runStateLabel,
  shortRuntimeId,
  subagentTitle
} from "../lib/agentRunUtils";
import {
  eventKey,
  eventStatusLabel,
  isCanceledToolEvent,
  managedProcessEventLabel,
  managedProcessEventText,
  materializeToolEvent,
  parseManagedProcessEvent,
  parseToolEvent,
  runtimeEventText,
  runtimeEventTime,
  selectVisibleToolEvents,
  toolEventMessageKey,
  toolEventRank,
  toolEventStartedAt,
  toolEventStartKey,
  withToolEventStartedAt
} from "../lib/toolEventUtils";
import { displayTextForMessage, renderTextForMessage, speechTextForMessage, unwrapFinalAnswerEnvelope } from "../lib/messageText";
import { resolvePersonaAgentBinding, resolvePersonaBoundAgent } from "../lib/personaAgentBinding";
import { PET_THINKING_STATE_EVENT, publishPetThinkingState, type PetThinkingState } from "../lib/petContext";
import { useAppStore } from "../lib/store";
import {
  agentRunWorkflowGraph,
  workflowNodeDisplayLabel,
  workflowNodeRoleLabel,
  workflowStatusDisplayLabel,
  workflowTransitionSequenceValue,
  workflowTransitionReasonLabel
} from "../lib/types";
import {
  acpUpdateLinesFromDetail,
  phaseDetailText,
  recentWorkflowGraphTransitions,
  workflowGraphSnapshotText,
  workflowRuntimeEventText,
  workflowSummaryText
} from "../lib/workflowUtils";
import type {
  AgentControlCommand,
  AgentDefinition,
  AgentRunPhase,
  AgentRunRecord,
  AgentRuntimeEvent,
  ChatAttachment,
  ChatMessage,
  EmojiGroup,
  LlmProvider,
  ManagedProcessEvent,
  ModelCatalogEntry,
  ToolEvent,
  ToolEventEnvelope
} from "../lib/types";
import { Avatar } from "../components/common";
import {
  buildEmojiPathIndexes,
  fileNameFromLocalPath,
  fileNameFromPath,
  isEmojiAssetPath,
  normalizeEmojiPathKey,
  repairEmojiAssetPath,
  type EmojiPathIndexes
} from "../lib/emojiUtils";
import {
  artifactKind,
  arrayValue,
  clampCount,
  composerErrorText,
  estimateMessageTokens,
  extractArtifactPaths,
  formatTokenK,
  hasFileDragData,
  materializeMessageRenderItem,
  materializeMessageRenderItems,
  messageRenderItem,
  messageThinkingCards,
  normalizeToolDetailText,
  previewText,
  providerModelOptions,
  recordValue,
  stripThinkingCardsFromText,
  thinkingCardsFromProviderData,
  thinkingCardsSignature,
  visibleMessageText,
  type ArtifactTarget,
  type MessageRenderItem,
  type MessageRenderMode,
  type ThinkingCard
} from "../lib/messageRenderUtils";
import {
  isImagePath,
  imageMimeType,
  parseMediaSegments,
  parseMediaTagSegments,
  MEDIA_MARKER,
  MEDIA_TAG_MARKER,
  type MediaSegment
} from "../lib/mediaUtils";
import {
  compactSteps,
  toolEventReauthInfo,
  toolEventElapsedLabel,
  toolEventPathBadge,
  rawObject,
  rawString,
  rawNumber,
  parseTerminalOutput,
  toolEventPayload,
  firstTerminalParts,
  parseInlineTerminalCommand,
  terminalCommandLabel,
  type CompactStep,
  type TerminalOutputParts
} from "../lib/toolDisplayUtils";
import { ThinkingCards } from "./chat/ThinkingCards";
import { ChevronIcon, InlineImage, InlineFile } from "./chat/InlineMedia";
import { WelcomePanel } from "./chat/WelcomePanel";
import { EmojiPicker, STANDARD_EMOJIS, EMOJI_TAB_ID } from "./chat/EmojiPicker";
import { ImagePreviewModal } from "./chat/ImagePreviewModal";
import { ToolStep, TimelineStep } from "./chat/ToolSteps";
import { ToolMessage } from "./chat/ToolMessage";
import { ManagedProcessMessage } from "./chat/ManagedProcessMessage";
import { ArtifactPreview } from "./chat/ArtifactPreview";
import { MarkdownLite } from "./chat/MarkdownLite";
import { MessageRow, type ShortMemoryMessageStat } from "./chat/MessageRow";
import { MessageList } from "./chat/MessageList";

type ComposerAttachment = ChatAttachment & {
  preview: string | null;
  status: "ready" | "staging" | "error";
  error?: string;
};

type VoiceInputState = "idle" | "listening" | "recording" | "transcribing";

type SpeechRecognitionLike = {
  lang: string;
  continuous: boolean;
  interimResults: boolean;
  onresult: ((event: unknown) => void) | null;
  onerror: ((event: unknown) => void) | null;
  onend: (() => void) | null;
  start: () => void;
  stop: () => void;
  abort: () => void;
};

type SpeechRecognitionConstructor = new () => SpeechRecognitionLike;


const DEFAULT_RENDERED_MESSAGES = 180;
const DEFAULT_ARTIFACT_SCAN_LIMIT = 80;
const DEFAULT_MESSAGE_PREVIEW_CHARS = 12_000;
const DEFAULT_STREAM_CHARS_PER_SECOND = 36;
const DEFAULT_THINKING_MIN_VISIBLE_MS = 1800;
const DEFAULT_BOTTOM_FOLLOW_THRESHOLD_PX = 180;
const DEFAULT_ACTIVE_POLL_INTERVAL_MS = 1500;
const DEFAULT_IDLE_POLL_INTERVAL_MS = 3000;
type ConversationScrollMemory = {
  top: number;
  anchorMessageId?: string;
  anchorOffset?: number;
};

type NativeFileDropPayload = {
  type: "enter" | "over" | "drop" | "leave";
  paths?: string[];
  position?: { x: number; y: number };
  windowLabel?: string;
};


async function playVoiceArtifact(path: string) {
  if (!isTauri()) return false;
  try {
    await api.playChatAudio?.(path);
    return true;
  } catch (error) {
    console.warn("chat voice playback failed, falling back to web audio:", error);
    return false;
  }
}

function isUiPreviewMessage(message: ChatMessage) {
  const providerData = recordValue(message.providerData);
  const preview = recordValue(providerData?.uiPreview);
  return preview?.truncated === true;
}















export const ChatExperience = memo(function ChatExperience() {
  const conversationScrollPositionCacheRef = useRef(new Map<string, ConversationScrollMemory>());
  const activeConversationId = useAppStore((state) => state.activeConversationId);
  const conversations = useAppStore((state) => state.conversations);
  const messages = useAppStore((state) => state.messages);
  const processingConversationIds = useAppStore((state) => state.processingConversationIds);
  const activeSection = useAppStore((state) => state.activeSection);
  const conversationUnreadCounts = useAppStore((state) => state.conversationUnreadCounts);
  const activeAgentRuns = useAppStore((state) => state.activeAgentRuns);
  const agentQueue = useAppStore((state) => state.agentQueue);
  const agentRuns = useAppStore((state) => state.agentRuns);
  const managedProcessEvents = useAppStore((state) => state.managedProcessEvents);
  const personas = useAppStore((state) => state.personas);
  const agents = useAppStore((state) => state.agents);
  const agentConfig = useAppStore((state) => state.agentConfig);
  const chatConfig = useAppStore((state) => state.config?.chat);
  const llmProviders = useAppStore((state) => state.llmProviders);
  const emojiGroups = useAppStore((state) => state.emojiGroups);
  const mcpServers = useAppStore((state) => state.mcpServers);
  const skills = useAppStore((state) => state.skills);
  const profile = useAppStore((state) => state.profile);
  const createConversation = useAppStore((state) => state.createConversation);
  const deleteConversation = useAppStore((state) => state.deleteConversation);
  const refreshMemories = useAppStore((state) => state.refreshMemories);
  const selectConversation = useAppStore((state) => state.selectConversation);
  const sendMessage = useAppStore((state) => state.sendMessage);
  const setConversationProcessing = useAppStore((state) => state.setConversationProcessing);
  const incrementConversationUnread = useAppStore((state) => state.incrementConversationUnread);
  const markConversationRead = useAppStore((state) => state.markConversationRead);
  const setSection = useAppStore((state) => state.setSection);
  const setFocusedAgentId = useAppStore((state) => state.setFocusedAgentId);
  const setSkillsPanelMode = useAppStore((state) => state.setSkillsPanelMode);
  const setMcpPanelMode = useAppStore((state) => state.setMcpPanelMode);
  const refreshChatData = useAppStore((state) => state.refreshChatData);
  const loadOlderMessages = useAppStore((state) => state.loadOlderMessages);
  const refreshAgents = useAppStore((state) => state.refreshAgents);
  const refreshSkills = useAppStore((state) => state.refreshSkills);
  const refreshMcpServers = useAppStore((state) => state.refreshMcpServers);
  const refreshAgentQueue = useAppStore((state) => state.refreshAgentQueue);
  const refreshAgentRuns = useAppStore((state) => state.refreshAgentRuns);
  const savePersona = useAppStore((state) => state.savePersona);
  const [draft, setDraft] = useState("");
  const [composerError, setComposerError] = useState<string | null>(null);
  const [controlCommands, setControlCommands] = useState<AgentControlCommand[]>([]);
  const [selectedSlashCommandIndex, setSelectedSlashCommandIndex] = useState(0);
  const [query, setQuery] = useState("");
  const deferredQuery = useDeferredValue(query);
  const [selectedPersonaId, setSelectedPersonaId] = useState("");
  const [attachments, setAttachments] = useState<ComposerAttachment[]>([]);
  // Mirror of attachments kept in a ref for use in cleanup effects that need
  // to revoke blob preview URLs without capturing a stale closure value.
  const attachmentsRef = useRef<ComposerAttachment[]>([]);
  useEffect(() => { attachmentsRef.current = attachments; }, [attachments]);

  // Clear attachments (and revoke blob URLs) when the active conversation
  // switches so staged files from one conversation don't bleed into another.
  useEffect(() => {
    for (const a of attachmentsRef.current) {
      if (a.preview?.startsWith("blob:")) URL.revokeObjectURL(a.preview);
    }
    setAttachments([]);
    // Also reset the sending guard so a pending request in the old conversation
    // doesn't block the first send in the new one.
    sendingRef.current = false;
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeConversationId]);
  const [emojiPickerOpen, setEmojiPickerOpen] = useState(false);
  const [pickerEmojiGroups, setPickerEmojiGroups] = useState(emojiGroups);
  const [dragActive, setDragActive] = useState(false);
  const [voiceInputState, setVoiceInputState] = useState<VoiceInputState>("idle");
  const [voiceSupported, setVoiceSupported] = useState(true);
  const [previewTarget, setPreviewTarget] = useState<ArtifactTarget | null>(null);
  const [copiedMessageId, setCopiedMessageId] = useState<string | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const chatShellRef = useRef<HTMLElement>(null);
  const chatMainRef = useRef<HTMLDivElement>(null);
  const composerRef = useRef<HTMLElement>(null);
  const lastNativeDropRef = useRef<{ signature: string; at: number } | null>(null);
  const speechRecognitionRef = useRef<SpeechRecognitionLike | null>(null);
  const mediaRecorderRef = useRef<MediaRecorder | null>(null);
  const voiceChunksRef = useRef<Blob[]>([]);
  const voiceAudioRef = useRef<HTMLAudioElement | null>(null);
  const spokenAssistantMessageIdsRef = useRef<Set<string>>(new Set());
  const activeVoiceReplyRequestRef = useRef<string | null>(null);
  const sendingRef = useRef(false);
  const pollingRefreshInFlightRef = useRef(false);
  const lastPollingChatRefreshAtRef = useRef(0);
  const lastPollingRunRefreshAtRef = useRef(0);
  const lastRuntimeEventsPollAtRef = useRef(0);
  const runtimeCursorRef = useRef(0);
  const isStoppingRunRef = useRef(false);
  const copiedMessageTimerRef = useRef<number | null>(null);
  const postSubmitScrollTimerRef = useRef<number | null>(null);
  const [isNearBottom, setIsNearBottom] = useState(true);
  const [unreadCount, setUnreadCount] = useState(0);
  const [historyLoading, setHistoryLoading] = useState(false);
  const [historyExhausted, setHistoryExhausted] = useState(false);
  const seenMessageContentRef = useRef<Map<string, string>>(new Map());
  const [animatedMessageIds, setAnimatedMessageIds] = useState<Set<string>>(() => new Set());
  const [settlingConversationId, setSettlingConversationId] = useState<string | null>(null);
  // Sync ref mirror so deleteConversationWithMemorySettling can guard re-entry
  // without relying on the async React state update cycle.
  const settlingConversationIdRef = useRef<string | null>(null);
  const [executionPanelOpen, setExecutionPanelOpen] = useState(false);
  const [timelineCollapsed, setTimelineCollapsed] = useState(false);
  const [artifactsCollapsed, setArtifactsCollapsed] = useState(true);
  const [skillsCollapsed, setSkillsCollapsed] = useState(true);
  const [compactionTipVisible, setCompactionTipVisible] = useState(false);
  const [compactionRoundTokens, setCompactionRoundTokens] = useState(0);
  const [runtimeEvents, setRuntimeEvents] = useState<AgentRuntimeEvent[]>([]);
  const [runtimeCursor, setRuntimeCursor] = useState(0);
  const [catalogModels, setCatalogModels] = useState<ModelCatalogEntry[]>([]);

  useEffect(() => {
    void Promise.all([refreshAgents(), refreshSkills(), refreshMcpServers(), refreshAgentRuns(), refreshAgentQueue()]);
  }, [refreshAgentQueue, refreshAgentRuns, refreshAgents, refreshMcpServers, refreshSkills]);

  useEffect(() => {
    let cancelled = false;
    void api.listAgentControlCommands().then((commands) => {
      if (!cancelled) setControlCommands(commands);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    setPickerEmojiGroups(emojiGroups);
  }, [emojiGroups]);

  useEffect(() => {
    setRuntimeEvents([]);
    setRuntimeCursor(0);
    runtimeCursorRef.current = 0;
    pollingRefreshInFlightRef.current = false;
  }, [activeConversationId]);

  useEffect(() => {
    if (!emojiPickerOpen) return;
    let cancelled = false;
    void api.listEmojiGroups().then((groups) => {
      if (!cancelled) setPickerEmojiGroups(groups);
    });
    return () => {
      cancelled = true;
    };
  }, [emojiPickerOpen]);

  useEffect(() => {
    if (!selectedPersonaId && personas[0]) setSelectedPersonaId(personas[0].id);
  }, [personas, selectedPersonaId]);

  const activeConversation = useMemo(
    () => conversations.find((item) => item.id === activeConversationId) ?? null,
    [activeConversationId, conversations]
  );
  useEffect(() => {
    if (activeConversation?.personaId && activeConversation.personaId !== selectedPersonaId) {
      setSelectedPersonaId(activeConversation.personaId);
    }
  }, [activeConversation?.personaId, selectedPersonaId]);

  const personaById = useMemo(() => new Map(personas.map((persona) => [persona.id, persona])), [personas]);
  const visiblePersonas = personas;
  const selectedPersona = visiblePersonas.find((persona) => persona.id === selectedPersonaId) ?? visiblePersonas[0] ?? null;
  // Stable ref for selectedPersona?.id so the polling setInterval is not
  // recreated on every persona list change — recreation introduces a brief
  // polling gap while the new interval hasn't fired yet.
  const selectedPersonaIdRef = useRef<string | undefined>(selectedPersona?.id);
  useEffect(() => { selectedPersonaIdRef.current = selectedPersona?.id; }, [selectedPersona?.id]);
  const activeConversationPersona = useMemo(
    () => (activeConversation?.personaId ? personaById.get(activeConversation.personaId) ?? null : null),
    [activeConversation?.personaId, personaById]
  );
  const toolbarPersona = selectedPersona ?? activeConversationPersona;
  useEffect(() => {
    if (!selectedPersonaId && visiblePersonas[0]) {
      setSelectedPersonaId(visiblePersonas[0].id);
      return;
    }
    if (selectedPersonaId && !visiblePersonas.some((persona) => persona.id === selectedPersonaId)) {
      setSelectedPersonaId(visiblePersonas[0]?.id ?? "");
    }
  }, [selectedPersonaId, visiblePersonas]);
  const defaultAgent = useMemo(() => agents.find((agent) => agent.isDefault) ?? agents[0] ?? null, [agents]);
  const renderLimit = clampCount(chatConfig?.uiMessageLimit, DEFAULT_RENDERED_MESSAGES, 40, 1000);
  const artifactScanLimit = clampCount(chatConfig?.artifactScanLimit, DEFAULT_ARTIFACT_SCAN_LIMIT, 20, renderLimit);
  const previewCharLimit = clampCount(chatConfig?.uiMessagePreviewChars, DEFAULT_MESSAGE_PREVIEW_CHARS, 2000, 100_000);
  const streamCharsPerSecond = clampCount(chatConfig?.uiStreamCharsPerSecond, DEFAULT_STREAM_CHARS_PER_SECOND, 8, 160);
  const thinkingMinVisibleMs = clampCount(chatConfig?.thinkingMinVisibleMs, DEFAULT_THINKING_MIN_VISIBLE_MS, 0, 8000);
  const bottomFollowThresholdPx = clampCount(chatConfig?.bottomFollowThresholdPx, DEFAULT_BOTTOM_FOLLOW_THRESHOLD_PX, 24, 600);
  const activePollIntervalMs = clampCount(chatConfig?.activePollIntervalMs, DEFAULT_ACTIVE_POLL_INTERVAL_MS, 300, 30_000);
  const idlePollIntervalMs = clampCount(chatConfig?.idlePollIntervalMs, DEFAULT_IDLE_POLL_INTERVAL_MS, 1000, 120_000);

  useEffect(() => {
    setHistoryLoading(false);
    setHistoryExhausted(false);
    loadingHistoryRef.current = false;
    preserveTopOnHistoryLoadRef.current = null;
  }, [activeConversationId, renderLimit]);
  // Round-aware compaction tip: only count tokens/messages after the last summary boundary
  useEffect(() => {
    if (!activeConversationId) return;
    let cancelled = false;
    const timer = window.setTimeout(() => {
      if (cancelled) return;
      const dialogueMessages = messages.filter((m) => m.role === "user" || m.role === "assistant");
      if (dialogueMessages.length === 0) {
        setCompactionTipVisible(false);
        setCompactionRoundTokens(0);
        return;
      }
      const mode = chatConfig?.shortContextMode === "tokens" ? "tokens" : "messages";
      const budget = clampCount(chatConfig?.shortContextTokenBudget, 8000, 500, 500_000);
      const messageLimit = clampCount(chatConfig?.maxContextRounds, 10, 1, 500);
      api.getShortContextState(activeConversationId).then((state) => {
        if (cancelled) return;
        let startIndex = 0;
        const boundaryId = state?.boundaryId ?? null;
        if (boundaryId) {
          const idx = dialogueMessages.findIndex((m) => m.id === boundaryId);
          if (idx >= 0) startIndex = idx + 1;
        }
        const roundMessages = dialogueMessages.slice(startIndex);
        if (mode === "tokens") {
          const roundTokens = roundMessages.reduce((t, m) => t + estimateMessageTokens(visibleMessageText(m)), state?.summaryTokens ?? 0);
          if (roundTokens >= budget) {
            setCompactionTipVisible(true);
            setCompactionRoundTokens(roundTokens);
          } else {
            setCompactionTipVisible(false);
            setCompactionRoundTokens(0);
          }
        } else {
          const roundCount = roundMessages.length + (state?.summaryMessages ?? 0);
          if (roundCount >= messageLimit) {
            setCompactionTipVisible(true);
            setCompactionRoundTokens(roundCount);
          } else {
            setCompactionTipVisible(false);
            setCompactionRoundTokens(0);
          }
        }
      }).catch(() => {
        // fallback: full count (used when getShortContextState IPC call fails)
        if (cancelled) return;
        if (mode === "tokens") {
          const total = dialogueMessages.reduce((t, m) => t + estimateMessageTokens(visibleMessageText(m)), 0);
          if (total >= budget) {
            setCompactionTipVisible(true);
            setCompactionRoundTokens(total);
          } else {
            // Ensure the tip is hidden when below threshold even in the
            // fallback path — without this else, a prior true value sticks
            // if the API call fails on a subsequent poll.
            setCompactionTipVisible(false);
            setCompactionRoundTokens(0);
          }
        } else {
          if (dialogueMessages.length >= messageLimit) {
            setCompactionTipVisible(true);
            setCompactionRoundTokens(dialogueMessages.length);
          } else {
            setCompactionTipVisible(false);
            setCompactionRoundTokens(0);
          }
        }
      });
    }, 220);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [messages, activeConversationId, chatConfig?.shortContextMode, chatConfig?.shortContextTokenBudget, chatConfig?.maxContextRounds]);
  const shortContextNotice = useMemo(() => {
    if (!compactionTipVisible) return null;
    const mode = chatConfig?.shortContextMode === "tokens" ? "tokens" : "messages";
    if (mode === "tokens") {
      return `本轮短时记忆已达到 ${formatTokenK(compactionRoundTokens)} token 预算，旧片段已压缩为短时摘要。发送新消息后将开始新一轮对话。`;
    }
    return `本轮短时记忆已达到 ${compactionRoundTokens} 条消息窗口，旧片段已压缩为短时摘要。发送新消息后将开始新一轮对话。`;
  }, [compactionTipVisible, compactionRoundTokens, chatConfig?.shortContextMode]);
  const shortMemoryStats = useMemo(() => {
    const stats = new Map<string, ShortMemoryMessageStat>();
    const mode = chatConfig?.shortContextMode === "tokens" ? "tokens" : "messages";
    const messageLimit = clampCount(chatConfig?.maxContextRounds, 10, 1, 500);
    let dialogueCount = 0;
    for (const message of messages) {
      if (message.role !== "user" && message.role !== "assistant") continue;
      dialogueCount += 1;
      if (message.role !== "assistant" || message.source === "desktop-stream") continue;
      if (mode === "tokens") {
        stats.set(message.id, {
          label: `本轮回复约 ${estimateMessageTokens(visibleMessageText(message)).toLocaleString()} tokens`,
          tone: "tokens"
        });
      } else {
        const remaining = Math.max(0, messageLimit - dialogueCount);
        stats.set(message.id, {
          label: `短时记忆重置前剩余 ${remaining} 条消息`,
          tone: "messages"
        });
      }
    }
    return stats;
  }, [chatConfig?.maxContextRounds, chatConfig?.shortContextMode, messages]);
  const activeAgent = useMemo(() => {
    return resolvePersonaBoundAgent(toolbarPersona, agents, activeConversation?.agentId) ?? defaultAgent;
  }, [activeConversation?.agentId, agents, defaultAgent, toolbarPersona]);
  const activeToolIterationBudget = toolbarPersona?.toolPolicy?.maxIterations
    ?? selectedPersona?.toolPolicy?.maxIterations
    ?? activeAgent?.maxToolIterations
    ?? agentConfig?.maxToolIterations
    ?? "-";
  const activeRun = useMemo(
    () => Object.values(activeAgentRuns).find((run) => run.conversationId === activeConversationId && !run.parentRunId),
    [activeAgentRuns, activeConversationId]
  );
  const activeQueueItems = useMemo(() => agentQueue
    .filter((item) => item.conversationId === activeConversationId)
    .filter((item) => item.status !== "completed" && item.status !== "failed" && item.status !== "aborted")
    .sort((a, b) => a.createdAt.localeCompare(b.createdAt)), [activeConversationId, agentQueue]);
  const availableMcpServers = useMemo(
    () => mcpServers.filter((server) => server.enabled),
    [mcpServers]
  );
  const activeMcpServerIdSet = useMemo(() => {
    if (!activeAgent?.mcpEnabled) return new Set<string>();
    const configured = activeAgent.enabledMcpServers
      .map((serverId) => serverId.trim())
      .filter(Boolean);
    return new Set(configured.length > 0 ? configured : availableMcpServers.map((server) => server.id));
  }, [activeAgent?.enabledMcpServers, activeAgent?.mcpEnabled, availableMcpServers]);
  const activeSkills = useMemo(() => {
    if (!activeAgent?.skillsEnabled) return [];
    const enabledIds = new Set(
      activeAgent.enabledSkills
        .map((skillId) => skillId.trim())
        .filter(Boolean)
    );
    return skills.filter((skill) => enabledIds.has(skill.id));
  }, [activeAgent?.enabledSkills, activeAgent?.skillsEnabled, skills]);

  useEffect(() => {
    if (activeSection !== "chat" || !activeAgent?.id) return;
    setFocusedAgentId(activeAgent.id);
  }, [activeAgent?.id, activeSection, setFocusedAgentId]);

  const slashCommandQuery = useMemo(() => {
    const value = draft.trimStart();
    if (!value.startsWith("/") && !value.startsWith("／")) return null;
    const body = value.slice(1);
    if (/\s/.test(body)) return null;
    return body.toLowerCase();
  }, [draft]);
  const slashCommandSuggestions = useMemo(() => {
    if (slashCommandQuery === null) return [];
    return controlCommands
      .filter((command) => {
        if (!slashCommandQuery) return true;
        return command.name.toLowerCase().startsWith(slashCommandQuery)
          || command.aliases.some((alias) => alias.toLowerCase().startsWith(slashCommandQuery));
      })
      .slice(0, 8);
  }, [controlCommands, slashCommandQuery]);

  useEffect(() => {
    setSelectedSlashCommandIndex(0);
  }, [slashCommandQuery]);

  useEffect(() => {
    if (selectedSlashCommandIndex >= slashCommandSuggestions.length) {
      setSelectedSlashCommandIndex(Math.max(0, slashCommandSuggestions.length - 1));
    }
  }, [selectedSlashCommandIndex, slashCommandSuggestions.length]);
  const storedRun = useMemo(
    () => agentRuns.find((run) => run.conversationId === activeConversationId && !run.parentRunId),
    [activeConversationId, agentRuns]
  );
  const activeWorkflowGraph = agentRunWorkflowGraph(activeRun) ?? agentRunWorkflowGraph(storedRun);
  const runStates = useMemo(() => {
    const states = new Map<string, string>();
    for (const run of agentRuns) states.set(run.runId, run.state);
    for (const run of Object.values(activeAgentRuns)) states.set(run.runId, run.state);
    return states;
  }, [activeAgentRuns, agentRuns]);
  const runByQueueItemId = useMemo(() => {
    const entries = new Map<string, { runId: string; state: string }>();
    for (const run of agentRuns) {
      if (run.queueItemId) entries.set(run.queueItemId, { runId: run.runId, state: run.state });
    }
    for (const run of Object.values(activeAgentRuns)) {
      if (run.queueItemId) entries.set(run.queueItemId, { runId: run.runId, state: run.state });
    }
    return entries;
  }, [activeAgentRuns, agentRuns]);
  const visibleParentRunId = activeRun?.runId ?? storedRun?.runId ?? null;
  const activeChildRuns = useMemo(
    () => agentRuns
      .filter((run) => run.parentRunId === visibleParentRunId)
      .sort((a, b) => {
        const stateRank = Number(isTerminalRunState(a.state)) - Number(isTerminalRunState(b.state));
        if (stateRank !== 0) return stateRank;
        const indexRank = (a.subagentIndex ?? 0) - (b.subagentIndex ?? 0);
        if (indexRank !== 0) return indexRank;
        return new Date(b.updatedAt).getTime() - new Date(a.updatedAt).getTime();
      })
      .slice(0, 8),
    [agentRuns, visibleParentRunId]
  );
  const activeChildRunCount = activeChildRuns.length;
  const runningChildRunCount = activeChildRuns.filter((run) => !isTerminalRunState(run.state)).length;
  const activeRunActivityAt = activeRun?.lastActivityAt ?? storedRun?.lastActivityAt ?? activeRun?.updatedAt ?? storedRun?.updatedAt ?? null;
  const activeRunActivityDesc = activeRun?.lastActivityDesc ?? storedRun?.lastActivityDesc ?? null;
  const stoppableRun = activeRun ?? (storedRun && !["completed", "failed", "aborted"].includes(storedRun.state) ? storedRun : null);
  const activeRunPhases = activeRun?.accumulatedPhases
    ?? (activeRun?.phase ? [{ phase: activeRun.phase, detail: activeRun.detail, updatedAt: activeRun.updatedAt }] : storedRun?.phaseEvents ?? []);
  const activeToolStartTimes = useMemo(
    () => runningToolStartTimesFromPhases(activeRunPhases),
    [activeRunPhases]
  );
  const activeToolEvents: ToolEvent[] = (activeRun?.accumulatedToolEvents?.length
    ? activeRun.accumulatedToolEvents
    : activeRun?.toolEvent
      ? [activeRun.toolEvent]
      : []
  )
    .map((event) => withToolEventStartedAt(event, activeToolStartTimes.get(toolEventStartKey(event)) ?? activeRunActivityAt ?? activeRun?.updatedAt ?? null))
    .map((event) => materializeToolEvent(event, event.runId ? runStates.get(event.runId) : null));
  const activeProcessEvents = useMemo(
    () => managedProcessEvents
      .filter((event) => event.conversationId === activeConversationId || Boolean(activeRun?.runId && event.runId === activeRun.runId))
      .slice(0, 6),
    [activeConversationId, activeRun?.runId, managedProcessEvents]
  );
  const recentMessages = useMemo(() => messages.slice(-renderLimit), [messages, renderLimit]);
  const artifactMessages = useMemo(() => recentMessages.slice(-artifactScanLimit), [artifactScanLimit, recentMessages]);
  const messageToolEvents = useMemo(() => selectVisibleToolEvents(recentMessages
    .map((message) => {
      const event = message.role === "tool" ? parseToolEvent(message.content) : null;
      return event ? materializeToolEvent(withToolEventStartedAt(event, message.createdAt), event.runId ? runStates.get(event.runId) : null) : null;
    })
    .filter((event): event is ToolEvent => event !== null)), [recentMessages, runStates]);
  const graphEvents = activeToolEvents.length > 0 ? selectVisibleToolEvents(activeToolEvents) : messageToolEvents;
  const providerBinding = useMemo(
    () => resolvePersonaAgentBinding(toolbarPersona, agents, llmProviders, activeConversation?.agentId),
    [activeConversation?.agentId, agents, llmProviders, toolbarPersona]
  );
  const currentProvider = useMemo(() => {
    const providerId = providerBinding.providerId;
    return llmProviders.find((provider) => provider.id === providerId && provider.enabled) ?? null;
  }, [llmProviders, providerBinding.providerId]);
  const effectiveModelValue = providerBinding.model;
  useEffect(() => {
    if (!currentProvider) {
      setCatalogModels([]);
      return;
    }
    let cancelled = false;
    api.detectProviderModels(currentProvider).then((result) => {
      if (!cancelled) setCatalogModels(result.models ?? []);
    }).catch(() => {
      if (!cancelled) setCatalogModels([]);
    });
    return () => {
      cancelled = true;
    };
  }, [currentProvider]);
  const modelOptions = useMemo(() => {
    if (catalogModels.length > 0 && currentProvider) {
      const options = catalogModels.map((model) => ({
        key: `${currentProvider.id}::${model.id}`,
        providerId: currentProvider.id,
        model: model.id,
        label: model.id
      }));
      const currentModel = effectiveModelValue.trim();
      if (currentModel && !options.some((option) => option.model === currentModel)) {
        options.unshift({
          key: `${currentProvider.id}::${currentModel}`,
          providerId: currentProvider.id,
          model: currentModel,
          label: `${currentModel}（当前）`
        });
      }
      const defaultModel = currentProvider.model.trim();
      if (defaultModel && !options.some((option) => option.model === defaultModel)) {
        options.unshift({
          key: `${currentProvider.id}::${defaultModel}`,
          providerId: currentProvider.id,
          model: defaultModel,
          label: `${defaultModel}（默认）`
        });
      }
      return options;
    }
    if (!currentProvider) return [];
    const options = providerModelOptions([currentProvider]);
    const currentModel = effectiveModelValue.trim();
    if (currentModel && !options.some((option) => option.model === currentModel)) {
      options.unshift({
        key: `${currentProvider.id}::${currentModel}`,
        providerId: currentProvider.id,
        model: currentModel,
        label: `${currentModel}（当前）`
      });
    }
    return options;
  }, [catalogModels, currentProvider, effectiveModelValue]);
  const selectedModelKey = currentProvider && effectiveModelValue
    ? `${currentProvider.id}::${effectiveModelValue}`
    : "";
  const emojiPathIndexes = useMemo(() => buildEmojiPathIndexes(emojiGroups), [emojiGroups]);
  const artifacts = useMemo(() => {
    const results: ArtifactTarget[] = [];
    const seen = new Set<string>();
    const push = (target: ArtifactTarget) => {
      if (!target.path || seen.has(target.path)) return;
      seen.add(target.path);
      results.push(target);
    };
    for (const event of messageToolEvents) {
      if (event.path && event.exists) {
        push({
          path: event.path,
          title: event.title || fileNameFromPath(event.path),
          kind: artifactKind(event.path, event.mimeType),
          source: `${event.serverId}.${event.toolName}`
        });
      }
    }
    for (const message of artifactMessages) {
      for (const target of extractArtifactPaths(message.content)) push(target);
    }
    for (const attachment of attachments) {
      push({
        path: attachment.path,
        title: attachment.fileName,
        kind: artifactKind(attachment.path, attachment.mimeType),
        source: "attachment"
      });
    }
    return results;
  }, [artifactMessages, attachments, messageToolEvents]);
  const canStopRun = Boolean(stoppableRun);
  const activeConversationProcessing = Boolean(
    activeConversationId && processingConversationIds.includes(activeConversationId)
  );
  const isProcessing = canStopRun || activeConversationProcessing;
  const busyInputMode = (chatConfig?.busyInputMode ?? "queue").trim().toLowerCase();
  const hasReadyComposerPayload = Boolean(draft.trim()) || attachments.some((item) => item.status === "ready");
  const hasStagingAttachment = attachments.some((item) => item.status === "staging");
  const sendButtonStopsRun = canStopRun && !hasReadyComposerPayload;
  const busySubmitTitle = busyInputMode === "steer"
    ? "注入当前运行"
    : busyInputMode === "interrupt"
      ? "打断并发送"
      : "加入队列";
  const [showThinking, setShowThinking] = useState(false);
  // Keep the thinking row mounted through its exit animation so the
  // transition can play instead of the node being removed instantly.
  const [thinkingMounted, setThinkingMounted] = useState(false);
  const thinkingLeaveTimerRef = useRef<number | null>(null);
  const hasStreamingContent = useMemo(
    () => isProcessing && messages.some((m) => (
      m.conversationId === activeConversationId
      && m.source === "desktop-stream"
      && m.content.length > 0
    )),
    [activeConversationId, isProcessing, messages]
  );
  const [firstCharShown, setFirstCharShown] = useState(false);
  // Reset when streaming message disappears (new turn)
  useEffect(() => {
    if (!hasStreamingContent) setFirstCharShown(false);
  }, [hasStreamingContent]);
  const handleFirstStreamChar = useCallback(() => { setFirstCharShown(true); }, []);
  const processingEndedAtRef = useRef<number | null>(null);
  const wasHiddenRef = useRef(false);

  // Manage thinking animation visibility
  useEffect(() => {
    const currentRunState = activeRun?.state ?? storedRun?.state ?? null;
    if (currentRunState && isTerminalRunState(currentRunState) && !activeConversationProcessing) {
      processingEndedAtRef.current = null;
      setShowThinking(false);
      return;
    }
    // While processing or streaming, keep thinking visible
    if (isProcessing || hasStreamingContent) {
      if (isProcessing) processingEndedAtRef.current = null;
      setShowThinking(true);
      return;
    }
    // Both ended — start hide timer respecting minimum visible time
    if (processingEndedAtRef.current === null) processingEndedAtRef.current = Date.now();
    const elapsed = Date.now() - processingEndedAtRef.current;
    const delay = Math.max(0, thinkingMinVisibleMs - elapsed);
    const timer = window.setTimeout(() => {
      processingEndedAtRef.current = null;
      setShowThinking(false);
    }, delay);
    return () => window.clearTimeout(timer);
  }, [activeConversationProcessing, activeRun?.state, storedRun?.state, isProcessing, hasStreamingContent, thinkingMinVisibleMs, firstCharShown]);

  useEffect(() => {
    if (!activeConversationId) return;
    const state: PetThinkingState = {
      conversationId: activeConversationId,
      personaId: activeConversation?.personaId ?? selectedPersona?.id ?? null,
      source: "desktop-ui",
      thinking: showThinking,
      updatedAt: new Date().toISOString()
    };
    publishPetThinkingState(state);
    void emit(PET_THINKING_STATE_EVENT, state).catch(() => undefined);
    void emitTo("pet", PET_THINKING_STATE_EVENT, state).catch(() => undefined);
    void emitTo("pet", "synthchat-pet-event", {
      type: showThinking ? "thinking_started" : "thinking_finished",
      source: state.source,
      personaId: state.personaId,
      conversationId: activeConversationId,
      ok: !showThinking
    }).catch(() => undefined);
  }, [activeConversation?.personaId, activeConversationId, selectedPersona?.id, showThinking]);

  // Drive mount/unmount with an exit animation: mount immediately when
  // showThinking turns on; when it turns off keep the node mounted with the
  // leaving class long enough for the exit transition to finish, then unmount.
  const THINKING_LEAVE_MS = 200;
  useEffect(() => {
    if (showThinking) {
      if (thinkingLeaveTimerRef.current !== null) {
        window.clearTimeout(thinkingLeaveTimerRef.current);
        thinkingLeaveTimerRef.current = null;
      }
      setThinkingMounted(true);
      return;
    }
    if (!thinkingMounted) return;
    thinkingLeaveTimerRef.current = window.setTimeout(() => {
      setThinkingMounted(false);
      thinkingLeaveTimerRef.current = null;
    }, THINKING_LEAVE_MS);
    return () => {
      if (thinkingLeaveTimerRef.current !== null) {
        window.clearTimeout(thinkingLeaveTimerRef.current);
        thinkingLeaveTimerRef.current = null;
      }
    };
  }, [showThinking, thinkingMounted]);

  useEffect(() => {
    const isHidden = activeSection !== "chat";
    const previous = seenMessageContentRef.current;
    const next = new Map<string, string>();
    const changedAssistantIds: string[] = [];
    for (const message of messages) {
      const visibleContent = visibleMessageText(message);
      next.set(message.id, visibleContent);
      if (message.role !== "assistant" || !visibleContent.trim()) continue;
      if (previous.size > 0 && previous.get(message.id) !== visibleContent) {
        changedAssistantIds.push(message.id);
      }
    }
    if (isHidden) {
      seenMessageContentRef.current = next;
      wasHiddenRef.current = true;
      return;
    }
    if (wasHiddenRef.current) {
      wasHiddenRef.current = false;
      seenMessageContentRef.current = next;
      return;
    }
    seenMessageContentRef.current = next;
    if (changedAssistantIds.length === 0) return;
    setAnimatedMessageIds((current) => {
      // Prune ids for messages that no longer exist in the list (deleted, context-
      // trimmed, etc.) so orphan ids don't accumulate indefinitely in the Set.
      const liveIds = new Set(messages.map((m) => m.id));
      const updated = new Set<string>();
      for (const id of current) {
        if (liveIds.has(id)) updated.add(id);
      }
      for (const id of changedAssistantIds) updated.add(id);
      return updated;
    });
  }, [activeSection, messages]);

  useEffect(() => {
    if (activeSection === "chat") return;
    activeVoiceReplyRequestRef.current = null;
    for (const message of messages) {
      if (message.role === "assistant") {
        notifiedAssistantMessageIdsRef.current.add(message.id);
        spokenAssistantMessageIdsRef.current.add(message.id);
      }
    }
  }, [activeSection, messages]);

  const handleMessageAnimationDone = useCallback((messageId: string) => {
    setAnimatedMessageIds((current) => {
      if (!current.has(messageId)) return current;
      const updated = new Set(current);
      updated.delete(messageId);
      return updated;
    });
  }, []);

  const filteredConversations = useMemo(() => {
    const needle = deferredQuery.toLowerCase();
    if (!needle) return conversations;
    return conversations.filter((item) => {
      const lastMessage = unwrapFinalAnswerEnvelope(item.lastMessage ?? "");
      return `${item.title ?? ""} ${lastMessage}`.toLowerCase().includes(needle);
    });
  }, [conversations, deferredQuery]);
  const enabledMcpCount = useMemo(
    () => availableMcpServers.filter((server) => activeMcpServerIdSet.has(server.id)).length,
    [activeMcpServerIdSet, availableMcpServers]
  );
  const enabledSkillCount = useMemo(() => activeSkills.length, [activeSkills]);
  const agentReady = Boolean(activeAgent?.enabled && (activeAgent.allowShell || activeAgent.mcpEnabled || activeAgent.skillsEnabled));

  const scrollToBottom = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    const target = el.scrollHeight;
    if (target <= 0) return;
    el.scrollTop = target;
    // Double-RAF: wait for React commit + browser layout to settle
    window.requestAnimationFrame(() => {
      const el2 = scrollRef.current;
      if (!el2) return;
      const h = el2.scrollHeight;
      if (h > 0) el2.scrollTop = h;
    });
  }, []);

  // Ref-based scroll tracking (synchronous, not affected by React batching)
  const nearBottomRef = useRef(true);

  // Track the currently rendered conversation tail.
  const lastMessage = messages.length > 0 ? messages[messages.length - 1] : null;
  const latestMessageKey = messages.length > 0
    ? `${messages[messages.length - 1].id}:${messages[messages.length - 1].content.length}`
    : "";
  const prevConversationIdRef = useRef<string | null>(activeConversationId);
  const prevActiveSectionRef = useRef(activeSection);
  const scrollOnNextMessagesRef = useRef<"bottom" | "restore" | null>(null);
  const scrollRestoreTargetRef = useRef<{ conversationId: string; memory: ConversationScrollMemory } | null>(null);
  const conversationActivatedAtRef = useRef<number>(Date.now());
  const notifiedAssistantMessageIdsRef = useRef<Set<string>>(new Set());
  const loadingHistoryRef = useRef(false);
  const preserveTopOnHistoryLoadRef = useRef<{ scrollHeight: number; scrollTop: number } | null>(null);

  const stopVoicePlayback = useCallback(() => {
    if (isTauri()) {
      void api.stopChatAudio?.().catch((error: unknown) => {
        console.warn("chat native voice stop failed:", error);
      });
    }
    const audio = voiceAudioRef.current;
    if (audio) {
      audio.pause();
      audio.src = "";
      voiceAudioRef.current = null;
    }
  }, []);

  useEffect(() => () => stopVoicePlayback(), [stopVoicePlayback]);

  useEffect(() => () => {
    if (copiedMessageTimerRef.current !== null) {
      window.clearTimeout(copiedMessageTimerRef.current);
      copiedMessageTimerRef.current = null;
    }
    if (postSubmitScrollTimerRef.current !== null) {
      window.clearTimeout(postSubmitScrollTimerRef.current);
      postSubmitScrollTimerRef.current = null;
    }
  }, []);

  // Revoke all blob preview URLs when the component unmounts so the browser
  // can free the memory without waiting for GC. attachmentsRef always holds
  // the latest state, avoiding stale-closure capture.
  useEffect(() => () => {
    for (const a of attachmentsRef.current) {
      if (a.preview?.startsWith("blob:")) URL.revokeObjectURL(a.preview);
    }
  }, []);

  useEffect(() => {
    if (activeSection === "chat") return;
    activeVoiceReplyRequestRef.current = null;
    stopVoicePlayback();
  }, [activeSection, stopVoicePlayback]);

  const loadMoreHistory = useCallback(async () => {
    const element = scrollRef.current;
    if (!element || !activeConversationId || loadingHistoryRef.current || historyExhausted) return;
    loadingHistoryRef.current = true;
    setHistoryLoading(true);
    preserveTopOnHistoryLoadRef.current = {
      scrollHeight: element.scrollHeight,
      scrollTop: element.scrollTop
    };
    try {
      const beforeCount = messages.length;
      const result = await loadOlderMessages(activeConversationId, renderLimit);
      if (result.loadedCount <= beforeCount || !result.hasMore) {
        setHistoryExhausted(true);
      }
    } catch (error) {
      console.warn("load older messages failed", error);
      preserveTopOnHistoryLoadRef.current = null;
    } finally {
      loadingHistoryRef.current = false;
      setHistoryLoading(false);
    }
  }, [activeConversationId, historyExhausted, loadOlderMessages, messages.length, renderLimit]);

  useEffect(() => {
    if (activeConversationPersona?.voiceReply?.enabled) return;
    activeVoiceReplyRequestRef.current = null;
    stopVoicePlayback();
  }, [activeConversationPersona?.voiceReply?.enabled, stopVoicePlayback]);

  const getScrollAnchor = useCallback((element: HTMLDivElement): ConversationScrollMemory => {
    const base: ConversationScrollMemory = { top: element.scrollTop };
    const nodes = element.querySelectorAll<HTMLElement>("[data-message-id]");
    const containerTop = element.getBoundingClientRect().top;
    const containerBottom = containerTop + element.clientHeight;
    for (const node of Array.from(nodes)) {
      const messageId = node.dataset.messageId?.trim();
      if (!messageId) continue;
      const rect = node.getBoundingClientRect();
      if (rect.bottom <= containerTop) continue;
      if (rect.top >= containerBottom) break;
      return {
        top: element.scrollTop,
        anchorMessageId: messageId,
        anchorOffset: rect.top - containerTop
      };
    }
    return base;
  }, []);

  const canPersistScrollPosition = useCallback((element: HTMLDivElement | null) => {
    if (!element) return false;
    if (activeSection !== "chat") return false;
    return element.clientHeight > 0 && element.scrollHeight > 0;
  }, [activeSection]);

  const applyScrollMemory = useCallback((element: HTMLDivElement, memory: ConversationScrollMemory) => {
    let targetTop = memory.top;
    if (memory.anchorMessageId) {
      const anchor = Array.from(element.querySelectorAll<HTMLElement>("[data-message-id]"))
        .find((node) => node.dataset.messageId === memory.anchorMessageId);
      if (anchor) {
        targetTop = anchor.offsetTop - (memory.anchorOffset ?? 0);
      }
    }
    const maxTop = Math.max(0, element.scrollHeight - element.clientHeight);
    const nextTop = Math.min(Math.max(0, targetTop), maxTop);
    element.scrollTop = nextTop;
    return nextTop;
  }, []);

  const saveCurrentScrollPosition = useCallback((conversationId: string | null) => {
    const element = scrollRef.current;
    if (!element || !conversationId || !canPersistScrollPosition(element)) return;
    const cache = conversationScrollPositionCacheRef.current;
    // Cap at 500 entries: JS Maps are ordered by insertion, so the oldest (first) entries
    // are evicted first when the cache overflows. This prevents unbounded growth when
    // a user browses hundreds of conversations in a session.
    if (cache.size >= 500 && !cache.has(conversationId)) {
      const firstKey = cache.keys().next().value;
      if (firstKey !== undefined) cache.delete(firstKey);
    }
    cache.set(conversationId, getScrollAnchor(element));
  }, [canPersistScrollPosition, getScrollAnchor]);

  const restoreSavedScrollPosition = useCallback((conversationId: string | null) => {
    if (!conversationId) return () => {};
    const saved = conversationScrollPositionCacheRef.current.get(conversationId);
    let cancelled = false;
    let attempts = 0;
    const apply = () => {
      if (cancelled) return;
      const element = scrollRef.current;
      if (!element) {
        if (attempts < 8) {
          attempts += 1;
          window.requestAnimationFrame(apply);
        }
        return;
      }
      if (saved === undefined) {
        nearBottomRef.current = true;
        setIsNearBottom(true);
        setUnreadCount(0);
        markConversationRead(conversationId);
        scrollToBottom();
        return;
      }
      const nextTop = applyScrollMemory(element, saved);
      if (canPersistScrollPosition(element)) {
        conversationScrollPositionCacheRef.current.set(conversationId, getScrollAnchor(element));
      }
      const distanceFromBottom = element.scrollHeight - element.scrollTop - element.clientHeight;
      const near = distanceFromBottom <= bottomFollowThresholdPx;
      nearBottomRef.current = near;
      setIsNearBottom(near);
      const unread = conversationUnreadCounts[conversationId] ?? 0;
      setUnreadCount(near ? 0 : unread);
      if (near) {
        markConversationRead(conversationId);
      }
      if (attempts < 6 && Math.abs(element.scrollTop - nextTop) > 2) {
        attempts += 1;
        window.requestAnimationFrame(apply);
      }
    };
    window.requestAnimationFrame(apply);
    return () => {
      cancelled = true;
    };
  }, [applyScrollMemory, bottomFollowThresholdPx, conversationUnreadCounts, getScrollAnchor, markConversationRead, scrollToBottom]);

  const selectConversationWithScrollMemory = useCallback((conversationId: string) => {
    saveCurrentScrollPosition(activeConversationId);
    void selectConversation(conversationId);
  }, [activeConversationId, saveCurrentScrollPosition, selectConversation]);

  const deleteConversationWithMemorySettling = useCallback(async (conversationId: string) => {
    // Use a ref for the guard so double-clicks are rejected synchronously —
    // the state-based `settlingConversationId` can't guard re-entry because
    // setSettlingConversationId is asynchronous and the old value stays in the
    // closure until the next render.
    if (settlingConversationIdRef.current) return;
    settlingConversationIdRef.current = conversationId;
    setSettlingConversationId(conversationId);
    try {
      const result = await deleteConversation(conversationId);
      if (result.status === "failed") {
        console.warn("Conversation deleted, but memory settling failed:", result.reason);
      } else if (result.status === "scheduled") {
        window.setTimeout(() => void refreshMemories(), 1500);
      } else {
        void refreshMemories();
      }
    } finally {
      if (settlingConversationIdRef.current === conversationId) {
        settlingConversationIdRef.current = null;
      }
      setSettlingConversationId((current) => current === conversationId ? null : current);
    }
  }, [deleteConversation, refreshMemories]);

  // Mark conversation switch for instant scroll
  useEffect(() => {
    if (activeConversationId !== prevConversationIdRef.current) {
      prevConversationIdRef.current = activeConversationId;
      conversationActivatedAtRef.current = Date.now();
      activeVoiceReplyRequestRef.current = null;
      stopVoicePlayback();
      setUnreadCount(0);
      setIsNearBottom(true);
      nearBottomRef.current = true;
      // Check if we have a saved position for this conversation
      const savedPosition = activeConversationId ? conversationScrollPositionCacheRef.current.get(activeConversationId) : undefined;
      scrollOnNextMessagesRef.current = savedPosition ? "restore" : "bottom";
      scrollRestoreTargetRef.current = activeConversationId && savedPosition
        ? { conversationId: activeConversationId, memory: savedPosition }
        : null;
    }
  }, [activeConversationId, stopVoicePlayback]);

  useEffect(() => {
    const previousSection = prevActiveSectionRef.current;
    prevActiveSectionRef.current = activeSection;
    if (previousSection === "chat" && activeSection !== "chat") {
      saveCurrentScrollPosition(activeConversationId);
      return;
    }
    if (previousSection !== "chat" && activeSection === "chat") {
      return restoreSavedScrollPosition(activeConversationId);
    }
  }, [activeConversationId, activeSection, restoreSavedScrollPosition, saveCurrentScrollPosition]);

  // Instant scroll when messages load after conversation switch
  useEffect(() => {
    if (!scrollOnNextMessagesRef.current || messages.length === 0) return;
    const mode = scrollOnNextMessagesRef.current;
    const convId = activeConversationId;
    let cancelled = false;
    let attempts = 0;
    const attemptScroll = () => {
      if (cancelled) return true;
      const el = scrollRef.current;
      if (!el || el.scrollHeight <= 0) return false;
      if (mode === "restore" && convId) {
        const target = scrollRestoreTargetRef.current?.conversationId === convId
          ? scrollRestoreTargetRef.current.memory
          : conversationScrollPositionCacheRef.current.get(convId);
        if (target) {
          const appliedTop = applyScrollMemory(el, target);
          const dist = el.scrollHeight - el.scrollTop - el.clientHeight;
          nearBottomRef.current = dist <= bottomFollowThresholdPx;
          setIsNearBottom(nearBottomRef.current);
          attempts += 1;
          if (attempts >= 6 || Math.abs(el.scrollTop - appliedTop) <= 2) {
            scrollOnNextMessagesRef.current = null;
            scrollRestoreTargetRef.current = null;
            return true;
          }
          return false;
        }
      }
      el.scrollTop = el.scrollHeight;
      nearBottomRef.current = true;
      setIsNearBottom(true);
      scrollOnNextMessagesRef.current = null;
      scrollRestoreTargetRef.current = null;
      return true;
    };
    const retry = () => {
      if (!attemptScroll()) window.requestAnimationFrame(retry);
    };
    retry();
    return () => {
      cancelled = true;
    };
  }, [activeConversationId, applyScrollMemory, bottomFollowThresholdPx, messages]);

  useLayoutEffect(() => {
    const snapshot = preserveTopOnHistoryLoadRef.current;
    if (!snapshot) return;
    const element = scrollRef.current;
    preserveTopOnHistoryLoadRef.current = null;
    if (!element) return;
    const delta = element.scrollHeight - snapshot.scrollHeight;
    element.scrollTop = snapshot.scrollTop + Math.max(0, delta);
    if (activeConversationId && canPersistScrollPosition(element)) {
      conversationScrollPositionCacheRef.current.set(activeConversationId, getScrollAnchor(element));
    }
  }, [activeConversationId, getScrollAnchor, messages]);

  const handleScroll = useCallback(() => {
    const element = scrollRef.current;
    if (!element) return;
    const distanceFromBottom = element.scrollHeight - element.scrollTop - element.clientHeight;
    const near = distanceFromBottom <= bottomFollowThresholdPx;
    nearBottomRef.current = near;
    setIsNearBottom(near);
    if (scrollOnNextMessagesRef.current) return;
    if (element.scrollTop <= 48 && messages.length >= renderLimit && !historyLoading && !historyExhausted) {
      void loadMoreHistory();
    }
    // Save scroll position for current conversation — debounced to 150ms
    // to avoid triggering querySelectorAll DOM traversal on every scroll event.
    if (saveScrollPositionTimerRef.current !== null) window.clearTimeout(saveScrollPositionTimerRef.current);
    saveScrollPositionTimerRef.current = window.setTimeout(() => {
      saveScrollPositionTimerRef.current = null;
      saveCurrentScrollPosition(activeConversationId);
    }, 150);
    if (near) {
      setUnreadCount(0);
      if (activeConversationId) markConversationRead(activeConversationId);
    }
  }, [activeConversationId, bottomFollowThresholdPx, historyExhausted, historyLoading, loadMoreHistory, markConversationRead, messages.length, renderLimit, saveCurrentScrollPosition]);

  const handleScrollToBottom = useCallback(() => {
    setUnreadCount(0);
    setIsNearBottom(true);
    nearBottomRef.current = true;
    markConversationRead(activeConversationId ?? "");
    scrollToBottom();
  }, [activeConversationId, markConversationRead, scrollToBottom]);

  useEffect(() => {
    if (!activeConversationId || !lastMessage) return;
    if (activeSection !== "chat") return;
    if (scrollOnNextMessagesRef.current) return;
    if (lastMessage.role !== "assistant") return;
    if (notifiedAssistantMessageIdsRef.current.has(lastMessage.id)) return;
    const createdAt = new Date(lastMessage.createdAt).getTime();
    if (!Number.isFinite(createdAt) || createdAt < conversationActivatedAtRef.current) return;
    notifiedAssistantMessageIdsRef.current.add(lastMessage.id);
    if (nearBottomRef.current) {
      if (scrollRef.current) {
        const el = scrollRef.current;
        const dist = el.scrollHeight - el.scrollTop - el.clientHeight;
        if (dist <= bottomFollowThresholdPx) {
          markConversationRead(activeConversationId);
          scrollToBottom();
          return;
        }
      }
      incrementConversationUnread(activeConversationId);
      setUnreadCount((c) => c + 1);
    } else {
      incrementConversationUnread(activeConversationId);
      setUnreadCount((c) => c + 1);
    }
  }, [activeConversationId, activeSection, bottomFollowThresholdPx, incrementConversationUnread, lastMessage, markConversationRead, scrollToBottom]);

  useEffect(() => () => {
    saveCurrentScrollPosition(activeConversationId);
  }, [activeConversationId, saveCurrentScrollPosition]);

  // Throttle ref for streaming auto-scroll — one RAF per token causes
  // excessive layout reads and scroll jitter at high streaming rates.
  const autoScrollThrottleRef = useRef<number | null>(null);
  // Debounce ref for saveCurrentScrollPosition inside handleScroll.
  // querySelectorAll("[data-message-id]") traverses all message nodes on every
  // scroll event; debouncing limits it to at most once per 150 ms.
  const saveScrollPositionTimerRef = useRef<number | null>(null);

  useEffect(() => {
    if (activeSection !== "chat") return;
    if (!latestMessageKey) return;
    const element = scrollRef.current;
    if (!element) return;
    const distanceFromBottom = element.scrollHeight - element.scrollTop - element.clientHeight;
    if (!(nearBottomRef.current || distanceFromBottom <= bottomFollowThresholdPx)) return;
    if (autoScrollThrottleRef.current !== null) return;
    autoScrollThrottleRef.current = window.requestAnimationFrame(() => {
      autoScrollThrottleRef.current = null;
      scrollToBottom();
    });
  }, [activeSection, bottomFollowThresholdPx, latestMessageKey, scrollToBottom]);

  useEffect(() => {
    if (activeSection !== "chat") return;
    const interval = isProcessing ? activePollIntervalMs : idlePollIntervalMs;
    const timer = window.setInterval(() => {
      if (pollingRefreshInFlightRef.current) return;
      pollingRefreshInFlightRef.current = true;
      const now = Date.now();
      const chatRefreshFloor = isProcessing ? Math.max(activePollIntervalMs, 3000) : idlePollIntervalMs;
      const runRefreshFloor = isProcessing ? Math.max(activePollIntervalMs, 5000) : idlePollIntervalMs;
      const runtimePollFloor = isProcessing ? Math.max(activePollIntervalMs, 2500) : idlePollIntervalMs;
      const shouldRefreshChat =
        !hasStreamingContent
        && now - lastPollingChatRefreshAtRef.current >= chatRefreshFloor;
      const shouldRefreshRuns =
        now - lastPollingRunRefreshAtRef.current >= runRefreshFloor;
      const shouldPollRuntimeEvents =
        Boolean(activeConversationId)
        && (isProcessing || executionPanelOpen)
        && now - lastRuntimeEventsPollAtRef.current >= runtimePollFloor;
      if (shouldRefreshChat) lastPollingChatRefreshAtRef.current = now;
      if (shouldRefreshRuns) lastPollingRunRefreshAtRef.current = now;
      if (shouldPollRuntimeEvents) lastRuntimeEventsPollAtRef.current = now;
      const messageRefresh = shouldRefreshChat
        ? refreshChatData(activeConversationId, selectedPersonaIdRef.current)
        : Promise.resolve();
      // Capture the conversation id at dispatch time so we can discard the
      // response if the user switches conversations before it arrives.
      const runtimePollConversationId = activeConversationId;
      void Promise.allSettled([
        messageRefresh,
        shouldRefreshRuns ? refreshAgentRuns() : Promise.resolve(),
        shouldPollRuntimeEvents && runtimePollConversationId
          ? api.listAgentRuntimeEvents({ conversationId: runtimePollConversationId, since: runtimeCursorRef.current, limit: 80 })
              .then((stream) => {
                // Discard stale responses from a previous conversation to
                // prevent cursor/events leaking into the newly active one.
                // Using only conversationId equality as the guard — the previous
                // check also allowed `cursor !== 0`, which let in-flight responses
                // from the old conversation contaminate the new one.
                if (runtimePollConversationId === activeConversationId) {
                  runtimeCursorRef.current = stream.cursor;
                  setRuntimeCursor(stream.cursor);
                  if (stream.events.length > 0) {
                    setRuntimeEvents((current) => [...current, ...stream.events].slice(-80));
                  }
                }
              })
          : Promise.resolve()
      ]).finally(() => {
        pollingRefreshInFlightRef.current = false;
      });
    }, interval);
    return () => window.clearInterval(timer);
  }, [activeConversationId, activePollIntervalMs, activeSection, executionPanelOpen, hasStreamingContent, idlePollIntervalMs, isProcessing, refreshAgentRuns, refreshChatData]);

  const stageFiles = useCallback(async (files: FileList | File[]) => {
    const list = Array.from(files);
    if (list.length === 0) return;
    const MAX_ATTACHMENT_COUNT = 20;
    const MAX_ATTACHMENT_BYTES = 50 * 1024 * 1024; // 50 MB per file
    for (const file of list) {
      if (attachmentsRef.current.length >= MAX_ATTACHMENT_COUNT) break;
      if (file.size > MAX_ATTACHMENT_BYTES) {
        console.warn(`Attachment "${file.name}" skipped: size ${file.size} exceeds ${MAX_ATTACHMENT_BYTES} byte limit`);
        continue;
      }
      const temporaryId = crypto.randomUUID();
      const preview = file.type.startsWith("image/") ? URL.createObjectURL(file) : null;
      setAttachments((current) => [...current, {
        id: temporaryId,
        fileName: file.name,
        mimeType: file.type || "application/octet-stream",
        fileSize: file.size,
        path: "",
        preview,
        status: "staging"
      }]);
      try {
        const buffer = await file.arrayBuffer();
        const saved = await api.uploadChatAttachment(file.name, file.type || "application/octet-stream", Array.from(new Uint8Array(buffer)));
        setAttachments((current) => current.map((item) => item.id === temporaryId ? { ...saved, preview, status: "ready" } : item));
      } catch (error) {
        // Revoke the preview blob URL so it doesn't accumulate in error state
        if (preview) URL.revokeObjectURL(preview);
        setAttachments((current) => current.map((item) => item.id === temporaryId ? { ...item, preview: null, status: "error", error: String(error) } : item));
      }
    }
  }, []);

  const stageFilePaths = useCallback(async (paths: string[]) => {
    const list = paths.map((path) => path.trim()).filter(Boolean);
    if (list.length === 0) return;
    const MAX_ATTACHMENT_COUNT = 20;
    for (const path of list) {
      // Read count from ref (sync) rather than state (stale inside loop)
      if (attachmentsRef.current.length >= MAX_ATTACHMENT_COUNT) break;
      const temporaryId = crypto.randomUUID();
      setAttachments((current) => [...current, {
        id: temporaryId,
        fileName: fileNameFromLocalPath(path),
        mimeType: "application/octet-stream",
        fileSize: 0,
        path,
        preview: null,
        status: "staging"
      }]);
      try {
        const saved = await api.uploadChatAttachmentFromPath(path);
        const preview = saved.mimeType.startsWith("image/") ? api.convertFileSrc(saved.path) : null;
        setAttachments((current) => current.map((item) => item.id === temporaryId ? { ...saved, preview, status: "ready" } : item));
      } catch (error) {
        setAttachments((current) => current.map((item) => item.id === temporaryId ? { ...item, status: "error", error: String(error) } : item));
      }
    }
  }, []);

  const handleFileDragEnter = useCallback((event: ReactDragEvent<HTMLElement>) => {
    event.preventDefault();
    event.stopPropagation();
    event.dataTransfer.dropEffect = "copy";
    setDragActive(true);
  }, []);

  const handleFileDragOver = useCallback((event: ReactDragEvent<HTMLElement>) => {
    event.preventDefault();
    event.stopPropagation();
    event.dataTransfer.dropEffect = "copy";
    setDragActive(true);
  }, []);

  const handleFileDragLeave = useCallback((event: ReactDragEvent<HTMLElement>) => {
    event.stopPropagation();
    const nextTarget = event.relatedTarget as Node | null;
    if (!nextTarget || !event.currentTarget.contains(nextTarget)) {
      setDragActive(false);
    }
  }, []);

  const handleFileDrop = useCallback((event: ReactDragEvent<HTMLElement>) => {
    event.preventDefault();
    event.stopPropagation();
    setDragActive(false);
    if (event.dataTransfer.files.length > 0) {
      void stageFiles(event.dataTransfer.files);
    }
  }, [stageFiles]);

  const isPointInsideChatDropTarget = useCallback((x: number, y: number) => {
    return [chatShellRef.current, composerRef.current, chatMainRef.current].some((element) => {
      if (!element) return false;
      const rect = element.getBoundingClientRect();
      return x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom;
    });
  }, []);

  const isNativeDropInsideChatTarget = useCallback((position: NativeFileDropPayload["position"]) => {
    if (!position) return true;
    const pixelRatio = window.devicePixelRatio || 1;
    return isPointInsideChatDropTarget(position.x / pixelRatio, position.y / pixelRatio);
  }, [isPointInsideChatDropTarget]);

  const rememberFileDropSignature = useCallback((signature: string, windowMs = 1000) => {
    const now = Date.now();
    const previous = lastNativeDropRef.current;
    if (previous?.signature === signature && now - previous.at < windowMs) return false;
    lastNativeDropRef.current = { signature, at: now };
    return true;
  }, []);

  const rememberPathDrop = useCallback((paths: string[]) => {
    return rememberFileDropSignature(`paths:${paths.slice().sort().join("\n")}`);
  }, [rememberFileDropSignature]);

  const rememberDomDrop = useCallback((files: FileList) => {
    const signature = Array.from(files)
      .map((file) => `${file.name}:${file.size}:${file.lastModified}`)
      .sort()
      .join("\n");
    return rememberFileDropSignature(`files:${signature}`, 500);
  }, [rememberFileDropSignature]);

  const handleNativeFileDrop = useCallback((payload: NativeFileDropPayload) => {
    if (activeSection !== "chat") {
      setDragActive(false);
      return;
    }
    if (payload.type === "leave") {
      setDragActive(false);
      return;
    }
    if (!isNativeDropInsideChatTarget(payload.position)) {
      setDragActive(false);
      return;
    }
    if (payload.type === "enter" || payload.type === "over") {
      setDragActive(true);
      return;
    }
    setDragActive(false);
    const paths = (payload.paths ?? []).map((path) => path.trim()).filter(Boolean);
    if (paths.length === 0) return;
    if (!rememberPathDrop(paths)) return;
    void stageFilePaths(paths);
  }, [activeSection, isNativeDropInsideChatTarget, rememberPathDrop, stageFilePaths]);

  useEffect(() => {
    if (activeSection !== "chat") {
      setDragActive(false);
      return;
    }
    const handleDrag = (event: DragEvent) => {
      if (!hasFileDragData(event.dataTransfer)) return;
      event.preventDefault();
      event.stopPropagation();
      if (event.dataTransfer) event.dataTransfer.dropEffect = "copy";
      const inside = isPointInsideChatDropTarget(event.clientX, event.clientY);
      if (!inside) {
        setDragActive(false);
        return;
      }
      setDragActive(true);
    };
    const handleDragLeave = (event: DragEvent) => {
      if (!hasFileDragData(event.dataTransfer)) return;
      const nextTarget = event.relatedTarget as Node | null;
      if (nextTarget && (chatMainRef.current?.contains(nextTarget) || composerRef.current?.contains(nextTarget))) return;
      setDragActive(false);
    };
    const handleDrop = (event: DragEvent) => {
      if (!hasFileDragData(event.dataTransfer)) return;
      event.preventDefault();
      event.stopPropagation();
      const inside = isPointInsideChatDropTarget(event.clientX, event.clientY);
      if (!inside) {
        setDragActive(false);
        return;
      }
      setDragActive(false);
      if (event.dataTransfer && event.dataTransfer.files.length > 0 && rememberDomDrop(event.dataTransfer.files)) {
        void stageFiles(event.dataTransfer.files);
      }
    };
    window.addEventListener("dragenter", handleDrag, true);
    window.addEventListener("dragover", handleDrag, true);
    window.addEventListener("dragleave", handleDragLeave, true);
    window.addEventListener("drop", handleDrop, true);
    // dragend fires when the user cancels a drag (Esc key, drops outside the
    // window, etc.) and is not guaranteed to be followed by dragleave in Tauri
    // WebView. Without this, dragActive can be permanently stuck at true.
    const handleDragEnd = () => setDragActive(false);
    window.addEventListener("dragend", handleDragEnd, true);
    return () => {
      window.removeEventListener("dragenter", handleDrag, true);
      window.removeEventListener("dragover", handleDrag, true);
      window.removeEventListener("dragleave", handleDragLeave, true);
      window.removeEventListener("drop", handleDrop, true);
      window.removeEventListener("dragend", handleDragEnd, true);
    };
  }, [activeSection, isPointInsideChatDropTarget, rememberDomDrop, stageFiles]);

  useEffect(() => {
    if (!isTauri() || activeSection !== "chat") {
      setDragActive(false);
      return;
    }
    const unlisteners: Array<() => void> = [];
    let cancelled = false;
    const attach = (source: string, registration: Promise<() => void>) => {
      void registration.then((handler) => {
        if (cancelled) {
          handler();
        } else {
          unlisteners.push(handler);
        }
      }).catch((error) => {
        console.warn(`${source} file drop listener unavailable:`, error);
      });
    };
    attach("webview native", getCurrentWebview().onDragDropEvent((event) => handleNativeFileDrop(event.payload as NativeFileDropPayload)));
    attach("window native", getCurrentWindow().onDragDropEvent((event) => handleNativeFileDrop(event.payload as NativeFileDropPayload)));
    attach("window forwarded", listen<NativeFileDropPayload>("synthchat-file-drop-event", (event) => {
      if (event.payload.windowLabel && event.payload.windowLabel !== "main") return;
      handleNativeFileDrop(event.payload);
    }));
    return () => {
      cancelled = true;
      unlisteners.forEach((unlisten) => unlisten());
    };
  }, [activeSection, handleNativeFileDrop]);

  const removeAttachment = (id: string) => {
    setAttachments((current) => {
      // Revoke any blob preview URL created by URL.createObjectURL so the
      // browser can free the backing memory immediately instead of waiting
      // for GC or page unload.
      const item = current.find((a) => a.id === id);
      if (item?.preview?.startsWith("blob:")) {
        URL.revokeObjectURL(item.preview);
      }
      return current.filter((a) => a.id !== id);
    });
  };

  const appendVoiceTranscript = useCallback((text: string) => {
    const transcript = text.trim();
    if (!transcript) return;
    setDraft((current) => {
      const prefix = current.trimEnd();
      return prefix ? `${prefix}\n${transcript}` : transcript;
    });
    setComposerError(null);
  }, []);

  const blobToDataUrl = useCallback((blob: Blob) => new Promise<string>((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      if (typeof reader.result === "string") {
        resolve(reader.result);
      } else {
        reject(new Error("语音数据读取失败"));
      }
    };
    reader.onerror = () => reject(reader.error ?? new Error("语音数据读取失败"));
    reader.readAsDataURL(blob);
  }), []);

  const transcribeRecordedVoice = useCallback(async (blob: Blob) => {
    if (blob.size === 0) {
      setComposerError("没有录到语音内容。");
      return;
    }
    setVoiceInputState("transcribing");
    try {
      const dataUrl = await blobToDataUrl(blob);
      const result = await api.transcribeChatAudio(dataUrl, blob.type || "audio/webm");
      const transcript = String(result?.transcript ?? "").trim();
      if (transcript) {
        appendVoiceTranscript(transcript);
      } else {
        setComposerError("没有识别到语音内容。");
      }
    } catch (error) {
      setComposerError(composerErrorText(error));
    } finally {
      setVoiceInputState("idle");
    }
  }, [appendVoiceTranscript, blobToDataUrl]);

  const stopVoiceInput = useCallback(() => {
    const recognition = speechRecognitionRef.current;
    if (recognition) {
      recognition.stop();
      return;
    }
    const recorder = mediaRecorderRef.current;
    if (recorder && recorder.state !== "inactive") {
      recorder.stop();
    }
  }, []);

  // Stop any in-progress voice recording when the component unmounts so the
  // MediaRecorder and underlying microphone stream track are released.
  useEffect(() => () => stopVoiceInput(), [stopVoiceInput]);

  const startRecordedVoiceInput = useCallback(async () => {
    if (!navigator.mediaDevices?.getUserMedia || typeof MediaRecorder === "undefined") {
      setVoiceSupported(false);
      setComposerError("当前 WebView 不支持语音输入。");
      return;
    }
    // Re-entry guard: if a recording is already in progress (stream exists),
    // stop the old one before starting a new one to prevent mic stream leaks.
    if (mediaRecorderRef.current && mediaRecorderRef.current.state !== "inactive") {
      stopVoiceInput();
      return;
    }
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      const preferredMimeType = [
        "audio/webm;codecs=opus",
        "audio/webm",
        "audio/ogg;codecs=opus"
      ].find((mimeType) => MediaRecorder.isTypeSupported?.(mimeType));
      const recorder = preferredMimeType
        ? new MediaRecorder(stream, { mimeType: preferredMimeType })
        : new MediaRecorder(stream);
      voiceChunksRef.current = [];
      mediaRecorderRef.current = recorder;
      recorder.ondataavailable = (event) => {
        if (event.data.size > 0) voiceChunksRef.current.push(event.data);
      };
      recorder.onerror = () => {
        stream.getTracks().forEach((track) => track.stop());
        mediaRecorderRef.current = null;
        setVoiceInputState("idle");
        setComposerError("语音录制失败。");
      };
      recorder.onstop = () => {
        stream.getTracks().forEach((track) => track.stop());
        mediaRecorderRef.current = null;
        const blob = new Blob(voiceChunksRef.current, { type: recorder.mimeType || "audio/webm" });
        voiceChunksRef.current = [];
        void transcribeRecordedVoice(blob);
      };
      recorder.start();
      setVoiceInputState("recording");
      setComposerError(null);
    } catch (error) {
      setVoiceInputState("idle");
      setComposerError(composerErrorText(error));
    }
  }, [transcribeRecordedVoice]);

  const toggleVoiceInput = useCallback(() => {
    if (voiceInputState !== "idle") {
      stopVoiceInput();
      return;
    }
    const SpeechRecognitionCtor = (
      (window as unknown as { SpeechRecognition?: SpeechRecognitionConstructor }).SpeechRecognition
      ?? (window as unknown as { webkitSpeechRecognition?: SpeechRecognitionConstructor }).webkitSpeechRecognition
    );
    if (SpeechRecognitionCtor) {
      try {
        const recognition = new SpeechRecognitionCtor();
        speechRecognitionRef.current = recognition;
        recognition.lang = "zh-CN";
        recognition.continuous = false;
        recognition.interimResults = false;
        recognition.onresult = (event: unknown) => {
          const results = (event as { results?: ArrayLike<ArrayLike<{ transcript?: string }>> }).results;
          const transcript = results
            ? Array.from(results)
                .map((result) => result[0]?.transcript ?? "")
                .join("")
            : "";
          appendVoiceTranscript(transcript);
        };
        recognition.onerror = (event: unknown) => {
          const errorType = (event as { error?: string }).error ?? "";
          speechRecognitionRef.current = null;
          setVoiceInputState("idle");
          // Only fall back to MediaRecorder for transient errors.
          // For permission-denied or aborted, show a clear message instead
          // of silently starting a second getUserMedia that will also fail.
          if (errorType === "not-allowed" || errorType === "permission-denied") {
            setComposerError("麦克风权限被拒绝，请在浏览器设置中授权后重试。");
            return;
          }
          if (errorType === "aborted") {
            return;
          }
          if (errorType === "no-speech") {
            setComposerError("未检测到语音输入，请重试。");
            return;
          }
          void startRecordedVoiceInput();
        };
        recognition.onend = () => {
          speechRecognitionRef.current = null;
          setVoiceInputState((current) => current === "listening" ? "idle" : current);
        };
        recognition.start();
        setVoiceInputState("listening");
        setComposerError(null);
        setVoiceSupported(true);
        return;
      } catch {
        speechRecognitionRef.current = null;
      }
    }
    void startRecordedVoiceInput();
  }, [appendVoiceTranscript, startRecordedVoiceInput, stopVoiceInput, voiceInputState]);

  const switchAgentModel = async (key: string) => {
    if (!key) return;
    const option = modelOptions.find((item) => item.key === key);
    if (!option || !toolbarPersona || !currentProvider) return;
    const fixedProviderId = toolbarPersona.llmProvider.trim();
    if (!fixedProviderId || option.providerId !== fixedProviderId || currentProvider.id !== fixedProviderId) return;
    if (toolbarPersona.llmModel === option.model) return;
    const capturedConversationId = activeConversationId;
    const savedPersona = await savePersona({
      ...toolbarPersona,
      agentId: activeAgent?.id ?? toolbarPersona.agentId,
      llmProvider: fixedProviderId,
      llmModel: option.model
    });
    if (activeConversationId === capturedConversationId) {
      await refreshChatData(capturedConversationId, savedPersona.id);
    }
  };

  const switchConversationAgent = async (agentId: string) => {
    if (!agentId || activeAgent?.id === agentId) return;
    setFocusedAgentId(agentId);
    const capturedConversationId = activeConversationId;
    if (toolbarPersona) {
      const savedPersona = await savePersona({ ...toolbarPersona, agentId });
      if (activeConversationId === capturedConversationId) {
        await refreshChatData(capturedConversationId, savedPersona.id);
      }
      return;
    }
    if (!capturedConversationId) return;
    const conversation = await api.setConversationAgent(capturedConversationId, agentId);
    if (activeConversationId === capturedConversationId) {
      await refreshChatData((conversation as any)?.id || capturedConversationId, (conversation as any)?.personaId ?? activeConversation?.personaId);
    }
  };

  const submit = async () => {
    const content = draft.trim();
    const readyAttachments = attachments.filter((item) => item.status === "ready");
    if (!content && readyAttachments.length === 0) return;
    if (sendingRef.current) {
      setComposerError("上一条消息仍在提交，请稍后再发送。");
      return;
    }
    const submittedAttachments = attachments;
    sendingRef.current = true;
    setDraft("");
    setComposerError(null);
    setCompactionTipVisible(false);
    try {
      if (activeConversationPersona && activeAgent && activeConversationPersona.agentId !== activeAgent.id) {
        await savePersona({ ...activeConversationPersona, agentId: activeAgent.id });
      }
      const outboundPersonaId = activeConversation?.personaId ?? selectedPersona?.id;
      const attachmentContext = readyAttachments
        .map((file) => JSON.stringify({
          type: "attachment",
          id: file.id,
          fileName: file.fileName,
          mimeType: file.mimeType || "application/octet-stream",
          fileSize: file.fileSize,
          path: file.path,
          recommendedTool: file.mimeType?.startsWith("image/") ? "vision_analyze" : undefined
        }))
        .join("\n");
      const attachmentMarkers = readyAttachments
        .map((file) => `[media attached: "${file.path}" (${file.mimeType || "application/octet-stream"})] ${file.fileName}`)
        .join("\n");
      const outbound = [content, attachmentMarkers, attachmentContext].filter(Boolean).join("\n\n");
      await sendMessage(outbound, outboundPersonaId, activeAgent?.id);
      // Revoke preview blob URLs only after a successful send so that on failure
      // the restored submittedAttachments still have valid previews for retry.
      for (const a of attachments) {
        if (a.preview?.startsWith("blob:")) URL.revokeObjectURL(a.preview);
      }
      setAttachments([]);
    } catch (error) {
      console.error("submit message failed", error);
      setDraft((current) => current.trim() ? current : content);
      setAttachments((current) => current.length > 0 ? current : submittedAttachments);
      setComposerError(composerErrorText(error));
    } finally {
      sendingRef.current = false;
      // Delay scroll to let React commit the new message to DOM first.
      // Track the timer so it can be cancelled if the component unmounts.
      if (postSubmitScrollTimerRef.current !== null) window.clearTimeout(postSubmitScrollTimerRef.current);
      postSubmitScrollTimerRef.current = window.setTimeout(() => {
        postSubmitScrollTimerRef.current = null;
        scrollToBottom();
      }, 50);
    }
  };

  const stopActiveRun = async () => {
    if (!stoppableRun || isStoppingRunRef.current) return;
    isStoppingRunRef.current = true;
    try {
      await api.abortAgentRun(stoppableRun.runId, "Agent run stopped by user from chat.");
      setConversationProcessing(stoppableRun.conversationId, false);
      await Promise.all([
        refreshAgentRuns(),
        refreshAgentQueue(),
        refreshChatData(activeConversationId, selectedPersona?.id)
      ]);
    } finally {
      isStoppingRunRef.current = false;
    }
  };

  const cancellingQueueItemIdsRef = useRef(new Set<string>());

  const cancelQueuedItem = async (id: string) => {
    if (cancellingQueueItemIdsRef.current.has(id)) return;
    cancellingQueueItemIdsRef.current.add(id);
    try {
      await api.cancelAgentQueueItem(id);
      await Promise.all([
        refreshAgentQueue(),
        refreshAgentRuns(),
        refreshChatData(activeConversationId, selectedPersona?.id)
      ]);
    } finally {
      cancellingQueueItemIdsRef.current.delete(id);
    }
  };

  const copyMessage = async (message: ChatMessage) => {
    let sourceMessage = message;
    if (isUiPreviewMessage(message)) {
      try {
        const fullContent = await api.getMessageContent(message.conversationId, message.id);
        if (fullContent) {
          sourceMessage = { ...message, content: fullContent };
        }
      } catch (error) {
        console.warn("读取完整消息失败，复制当前预览内容。", error);
      }
    }
    const text = displayTextForMessage(visibleMessageText(sourceMessage));
    if (!text) return;
    try {
      await navigator.clipboard?.writeText(text);
    } catch (error) {
      setComposerError("复制失败：" + String(error));
      return;
    }
    if (copiedMessageTimerRef.current !== null) window.clearTimeout(copiedMessageTimerRef.current);
    setCopiedMessageId(message.id);
    copiedMessageTimerRef.current = window.setTimeout(() => {
      copiedMessageTimerRef.current = null;
      setCopiedMessageId(null);
    }, 1200);
  };

  const insertSkill = (skillName: string) => {
    const token = `/${skillName}  `;
    setDraft((current) => current.includes(token) ? current : `${token}${current}`);
  };

  const insertControlCommand = (command: AgentControlCommand) => {
    setDraft(`/${command.name}${command.argsHint ? " " : ""}`);
  };

  const handleComposerKeyDown = (event: ReactKeyboardEvent<HTMLTextAreaElement>) => {
    // Ignore key events fired during IME composition (e.g. Chinese/Japanese/Korean
    // candidate selection). Without this guard, pressing Enter to confirm a
    // candidate word triggers submit() — a CRITICAL input bug for CJK users.
    if (event.nativeEvent.isComposing) return;

    if (slashCommandSuggestions.length > 0) {
      if (event.key === "ArrowDown") {
        event.preventDefault();
        setSelectedSlashCommandIndex((current) => (current + 1) % slashCommandSuggestions.length);
        return;
      }
      if (event.key === "ArrowUp") {
        event.preventDefault();
        setSelectedSlashCommandIndex((current) => (current - 1 + slashCommandSuggestions.length) % slashCommandSuggestions.length);
        return;
      }
      if (event.key === "Tab" || (event.key === "Enter" && !event.shiftKey)) {
        event.preventDefault();
        insertControlCommand(slashCommandSuggestions[selectedSlashCommandIndex] ?? slashCommandSuggestions[0]);
        return;
      }
    }

    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      void submit();
    }
  };

  const sendEmojiImage = (path: string) => {
    const mime = imageMimeType(path);
    const marker = `[media attached: "${path}" (${mime})]`;
    setDraft((current) => [current.trim(), marker].filter(Boolean).join("\n\n"));
    setEmojiPickerOpen(false);
  };

  const insertEmoji = (emoji: string) => {
    setDraft((current) => `${current}${emoji}`);
  };

  return (
    <section className="claw-chat-shell">
      <aside className="claw-chat-sidebar">
        <div className="claw-side-head">
          <div>
            <span>Sessions</span>
            <strong>对话</strong>
          </div>
          <button onClick={() => void createConversation(selectedPersona?.id)} title="新建会话" type="button">
            <Plus size={16} />
          </button>
        </div>
        <label className="claw-search">
          <Search size={15} />
          <input aria-label="搜索会话" value={query} onChange={(event) => setQuery(event.target.value)} placeholder="搜索会话" />
        </label>
        <div className="claw-session-list">
          {filteredConversations.map((conversation) => {
            const persona = personaById.get(conversation.personaId || "");
            const lastMessage = displayTextForMessage(
              unwrapFinalAnswerEnvelope(conversation.lastMessage ?? "")
            ) || "暂无消息";
            return (
              <div className={[
                "claw-session",
                conversation.id === activeConversationId ? "active" : "",
                settlingConversationId === conversation.id ? "settling" : ""
              ].filter(Boolean).join(" ")} key={conversation.id}>
                <button disabled={settlingConversationId === conversation.id} onClick={() => selectConversationWithScrollMemory(conversation.id)} type="button">
                  <Avatar
                    name={persona?.name || conversation.title}
                    src={persona?.avatarPath || ""}
                  />
                  <span>
                    <strong>{persona?.name || conversation.title}</strong>
                    <small>{settlingConversationId === conversation.id ? "删除中，记忆稍后整理..." : lastMessage}</small>
                  </span>
                  {(() => {
                    const count = conversation.id === activeConversationId
                      ? Math.max(conversationUnreadCounts[conversation.id] ?? 0, unreadCount)
                      : (conversationUnreadCounts[conversation.id] ?? 0);
                    return count > 0
                      ? <span aria-label={`${count} 条未读消息`} className="claw-unread-badge" title={`${count} 条未读消息`} />
                      : null;
                  })()}
                </button>
                <button
                  className="claw-session-delete"
                  disabled={Boolean(settlingConversationId)}
                  onClick={() => void deleteConversationWithMemorySettling(conversation.id)}
                  title="整理会话记忆后删除会话"
                  type="button"
                >
                  {settlingConversationId === conversation.id ? <Loader2 className="spin" size={14} /> : <Trash2 size={14} />}
                </button>
                {settlingConversationId === conversation.id ? (
                  <div className="claw-memory-settling">
                    <span />
                  </div>
                ) : null}
              </div>
            );
          })}
          {filteredConversations.length === 0 ? (
            <div className="claw-empty-small">
              <MessageSquareText size={28} />
              <span>{deferredQuery ? "无匹配结果" : "还没有对话"}</span>
            </div>
          ) : null}
        </div>
      </aside>

      <article className="claw-chat-main" ref={chatShellRef}>
        <header className="claw-chat-toolbar">
          <div className="claw-toolbar-title">
            <Sparkles size={17} />
            <div>
              <span>{activeRun ? runStateLabel(activeRun.state) : agentReady ? "Agent runtime ready" : "Agent runtime disabled"}</span>
              <strong>{agentLabel(activeAgent)}</strong>
            </div>
          </div>
          <div className="claw-toolbar-actions">
            <label className="claw-select">
              <Bot size={14} />
              <select value={toolbarPersona?.id ?? selectedPersona?.id ?? ""} onChange={(event) => setSelectedPersonaId(event.target.value)}>
                {!toolbarPersona && visiblePersonas.length === 0 ? <option value="">无可用角色</option> : null}
                {visiblePersonas.map((persona) => <option key={persona.id} value={persona.id}>{persona.name}</option>)}
              </select>
            </label>
            <label className="claw-select">
              <Network size={14} />
              <select value={activeAgent?.id ?? ""} onChange={(event) => void switchConversationAgent(event.target.value)}>
                {agents.map((agent) => <option key={agent.id} value={agent.id}>{agent.name}</option>)}
              </select>
            </label>
            <label className="claw-select">
              <ChevronIcon />
              <select disabled={!currentProvider} value={selectedModelKey} onChange={(event) => void switchAgentModel(event.target.value)}>
                <option value="">{providerBinding.providerDisabled ? "服务商已停用" : currentProvider ? "选择模型" : "先在通讯录选择服务商"}</option>
                {modelOptions.map((option) => <option key={option.key} value={option.key}>{option.label}</option>)}
              </select>
            </label>
            <button onClick={() => void refreshChatData(activeConversationId, selectedPersona?.id)} title="刷新" type="button">
              <RefreshCw size={15} />
            </button>
            <button
              className={executionPanelOpen ? "claw-toolbar-btn-active" : ""}
              aria-pressed={executionPanelOpen}
              onClick={() => setExecutionPanelOpen((open) => !open)}
              title={executionPanelOpen ? "隐藏任务编排" : "显示任务编排"}
              type="button"
            >
              {executionPanelOpen ? <PanelRightClose size={15} /> : <PanelRightOpen size={15} />}
            </button>
          </div>
        </header>

        <div className="claw-runtime-strip">
          <button onClick={() => {
            if (activeAgent?.id) setFocusedAgentId(activeAgent.id);
            setSection("agents");
          }} type="button">
            <Bot size={14} />
            <span>Agents</span>
            <strong>{agents.length}</strong>
          </button>
          <button onClick={() => {
            if (activeAgent?.id) setFocusedAgentId(activeAgent.id);
            setMcpPanelMode("local");
            setSection("mcp");
          }} type="button">
            <Wrench size={14} />
            <span>MCP</span>
            <strong>{enabledMcpCount}/{availableMcpServers.length}</strong>
          </button>
          <button onClick={() => {
            if (activeAgent?.id) setFocusedAgentId(activeAgent.id);
            setSkillsPanelMode("local");
            setSection("skills");
          }} type="button">
            <Code2 size={14} />
            <span>Skills</span>
            <strong>{enabledSkillCount}/{skills.length}</strong>
          </button>
          <button onClick={() => setSection("personas")} type="button">
            <Settings2 size={14} />
            <span>Policy</span>
            <strong>{activeToolIterationBudget}</strong>
          </button>
          <button onClick={() => void refreshAgentQueue()} type="button" title="刷新队列">
            <Clock size={14} />
            <span>Queue</span>
            <strong>{activeQueueItems.length}</strong>
          </button>
        </div>

        <div
          className={[
            "claw-chat-body",
            dragActive ? "dragging" : "",
            executionPanelOpen ? "execution-open" : ""
          ].filter(Boolean).join(" ")}
          ref={chatMainRef}
          onDragEnter={handleFileDragEnter}
          onDragOver={handleFileDragOver}
          onDragLeave={handleFileDragLeave}
          onDrop={handleFileDrop}
        >
          <div className="claw-message-stream-wrap">
            <div
              className="claw-message-stream"
              ref={scrollRef}
              onScroll={handleScroll}
              aria-live="polite"
              aria-label="对话消息"
            >
              {messages.length === 0 ? (
                <WelcomePanel
                  disabled={!selectedPersona}
                  onPrompt={(text) => setDraft(text)}
                />
              ) : (
                <>
                  {messages.length >= renderLimit ? (
                    <div role="status" className={`claw-history-loader${historyLoading ? " is-loading" : ""}${historyExhausted ? " is-exhausted" : ""}`}>
                      {historyLoading ? (
                        <>
                          <Loader2 className="spin" size={14} />
                          <span>正在加载更早消息...</span>
                        </>
                      ) : historyExhausted ? (
                        <span>已到达当前会话最早消息</span>
                      ) : (
                        <span>继续向上滚动加载更早消息</span>
                      )}
                    </div>
                  ) : null}
                  <MessageList
                    messages={messages}
                    profileName={profile.name}
                    profileAvatar={profile.avatarPath ?? ""}
                    personaName={selectedPersona?.name ?? "assistant"}
                    personaAvatar={selectedPersona?.avatarPath ?? ""}
                    onFirstStreamChar={handleFirstStreamChar}
                    copiedMessageId={copiedMessageId}
                    onCopy={copyMessage}
                    previewCharLimit={previewCharLimit}
                    animatedMessageIds={animatedMessageIds}
                    streamCharsPerSecond={streamCharsPerSecond}
                    onMessageAnimationDone={handleMessageAnimationDone}
                    memoryStats={shortMemoryStats}
                    runStates={runStates}
                    emojiPathIndexes={emojiPathIndexes}
                  />
                </>
              )}
              {thinkingMounted ? (
                <div className={`claw-thinking-row${showThinking ? "" : " is-leaving"}`}>
                  <span className="claw-thinking-orbit" aria-hidden="true">
                    <i />
                    <i />
                    <i />
                  </span>
                  <span>{activeRun ? runStateLabel(activeRun.state) : "正在思考"}</span>
                </div>
              ) : null}
            </div>
            {unreadCount > 0 && !isNearBottom ? (
              <button className="claw-new-msg-bubble" onClick={handleScrollToBottom} type="button">
                <ChevronDown size={16} />
                <span>{unreadCount} 条新消息</span>
              </button>
            ) : null}
          </div>

          <aside
            className="claw-execution-panel"
            aria-hidden={!executionPanelOpen}
            // inert prevents keyboard Tab from reaching focusable children
            // inside a collapsed panel — aria-hidden alone does not block Tab.
            {...(!executionPanelOpen ? { inert: "" } : {})}
          >
            {activeQueueItems.length > 0 ? (
              <div className="claw-panel-card claw-panel-card--queue">
                <div className="claw-panel-head compact">
                  <div className="claw-panel-head-left">
                    <span className="claw-panel-icon claw-panel-icon--queue"><Clock size={14} /></span>
                    <div>
                      <span>Queue</span>
                      <strong>排队请求</strong>
                    </div>
                  </div>
                  <div className="claw-panel-head-right">
                    <small className="claw-count-badge">{activeQueueItems.length}</small>
                  </div>
                </div>
                <div className="claw-panel-body">
                  <div className="claw-agent-queue-list">
                    {activeQueueItems.slice(0, 6).map((item) => {
                      const linkedRun = runByQueueItemId.get(item.id);
                      return (
                      <div className={`claw-agent-queue-row is-${item.status}`} key={item.id}>
                        <div>
                          <span>{queueStatusLabel(item.status)}</span>
                          <small>
                            {formatTime(item.updatedAt || item.createdAt)}
                            {linkedRun ? ` · ${shortRuntimeId(linkedRun.runId)} · ${runStateLabel(linkedRun.state)}` : ` · ${shortRuntimeId(item.id)}`}
                          </small>
                        </div>
                        <p>{item.content}</p>
                        {item.error ? <em>{item.error}</em> : null}
                        {["pending", "running"].includes(item.status) ? (
                          <button onClick={() => void cancelQueuedItem(item.id)} title="取消排队请求" type="button">
                            <X size={12} />
                          </button>
                        ) : null}
                      </div>
                      );
                    })}
                  </div>
                </div>
              </div>
            ) : null}
            {/* ── Execution Graph Card ── */}
            <div className="claw-panel-card claw-panel-card--accent">
              <div className="claw-panel-head" onClick={() => setTimelineCollapsed((v) => !v)} role="button" tabIndex={0} onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); setTimelineCollapsed((v) => !v); } }}>
                <div className="claw-panel-head-left">
                  <span className="claw-panel-icon claw-panel-icon--primary"><Layers size={14} /></span>
                  <div>
                    <span>Execution Graph</span>
                    <strong>任务编排</strong>
                  </div>
                </div>
                <div className="claw-panel-head-right">
                  {activeRun ? <small className="claw-status-chip claw-status-chip--active">{runStateLabel(activeRun.state)}</small> : <small className="claw-status-chip">idle</small>}
                  <span className="claw-panel-chevron">{timelineCollapsed ? <ChevronRight size={14} /> : <ChevronDown size={14} />}</span>
                </div>
              </div>
              <div className={`claw-panel-body${timelineCollapsed ? " claw-panel-body--collapsed" : ""}`}>
                {activeRun?.error ? (
                  <div className="claw-run-error">
                    <AlertCircle size={15} />
                    <span>{activeRun.error}</span>
                  </div>
                ) : null}
                <div className="claw-timeline">
                  <div className="claw-tl-node claw-tl-node--done">
                    <div className="claw-tl-dot"><CheckCircle2 size={14} /></div>
                    <div className="claw-tl-content">
                      <div className="claw-tl-head">
                        <span className="claw-tl-title">接收用户目标</span>
                      </div>
                    </div>
                  </div>
                  {activeWorkflowGraph ? (
                    <div className="claw-tl-node claw-tl-node--phase">
                      <div className="claw-tl-dot"><Layers size={14} /></div>
                      <div className="claw-tl-content">
                        <div className="claw-tl-head">
                          <span className="claw-tl-title">Workflow Graph</span>
                          <small>{workflowGraphSnapshotText(activeWorkflowGraph)}</small>
                        </div>
                        <div className="claw-acp-updates">
                          {(activeWorkflowGraph.nodes ?? []).map((node) => (
                            <span className="claw-acp-update" key={`workflow-node-${node.node}`}>
                              {workflowNodeDisplayLabel(node.node)} · {workflowStatusDisplayLabel(node.status)} · {node.role ?? workflowNodeRoleLabel(node.node)}
                            </span>
                          ))}
                          {recentWorkflowGraphTransitions(activeWorkflowGraph).map((transition, index) => (
                            <span className="claw-acp-update" key={`workflow-edge-${workflowTransitionSequenceValue(transition) ?? index}`}>
                              {workflowNodeDisplayLabel(transition.from)}{" -> "}{workflowNodeDisplayLabel(transition.to)} · {workflowTransitionReasonLabel(transition.reason)}
                              {(transition.topologyEdgeSource ?? transition.topology_edge_source) ? ` · ${transition.topologyEdgeSource ?? transition.topology_edge_source}` : ""}
                              {((transition.topologyEdgeKnown ?? transition.topology_edge_known) === false) ? " · unknown edge" : ""}
                            </span>
                          ))}
                        </div>
                      </div>
                    </div>
                  ) : null}
                  {runtimeEvents.length > 0 ? (
                    <div className="claw-tl-node claw-tl-node--phase">
                      <div className="claw-tl-dot"><Network size={14} /></div>
                      <div className="claw-tl-content">
                        <div className="claw-tl-head">
                          <span className="claw-tl-title">Runtime Stream</span>
                          <small>{runtimeEvents.length} events · cursor {runtimeCursor}</small>
                        </div>
                        <div className="claw-acp-updates">
                          {runtimeEvents.slice(-5).map((event) => (
                            <span className="claw-acp-update" key={`${event.id}-${event.kind}-${runtimeEventTime(event)}`}>
                              {runtimeEventText(event, workflowRuntimeEventText, shortRuntimeId)}
                            </span>
                          ))}
                        </div>
                      </div>
                    </div>
                  ) : null}
                  {activeRunPhases.length > 0 ? (
                    activeRunPhases.slice(-8).map((phase, index) => {
                      const acpUpdateLines = acpUpdateLinesFromDetail(phase.detail).slice(-4);
                      return (
                        <div className="claw-tl-node claw-tl-node--phase" key={`${phase.phase}-${phase.updatedAt}-${index}`}>
                          <div className="claw-tl-dot"><Brain size={14} /></div>
                          <div className="claw-tl-content">
                            <div className="claw-tl-head">
                              <span className="claw-tl-title">{runPhaseLabel(phase.phase)}</span>
                              <small>{formatTime(phase.updatedAt)}</small>
                            </div>
                            {phaseDetailText(phase.detail) ? <p>{phaseDetailText(phase.detail)}</p> : null}
                            {acpUpdateLines.length > 0 ? (
                              <div className="claw-acp-updates">
                                {acpUpdateLines.map((line) => <span className="claw-acp-update" key={line}>{line}</span>)}
                              </div>
                            ) : null}
                          </div>
                        </div>
                      );
                    })
                  ) : null}
                  {activeChildRunCount > 0 ? (
                    <div className="claw-tl-node claw-tl-node--subagents">
                      <div className="claw-tl-dot"><Bot size={14} /></div>
                      <div className="claw-tl-content">
                        <div className="claw-tl-head">
                          <span className="claw-tl-title">子智能体</span>
                          <small>{runningChildRunCount > 0 ? `${runningChildRunCount} 个运行中` : `${activeChildRunCount} 个已结束`}</small>
                        </div>
                        <div className="claw-subagent-list">
                          {activeChildRuns.map((run) => {
                            const latestPhase = run.phaseEvents?.[run.phaseEvents.length - 1];
                            const acpUpdateLines = acpUpdateLinesFromDetail(latestPhase?.detail).slice(-3);
                            const activity = run.lastActivityDesc
                              || (latestPhase ? runPhaseLabel(latestPhase.phase) : "")
                              || run.error
                              || run.userRequest
                              || "";
                            const title = subagentTitle(run);
                            return (
                              <div className={`claw-subagent-row is-${run.state}`} key={run.runId}>
                                <div className="claw-subagent-row-head">
                                  <span>{title}</span>
                                  <small>{runStateLabel(run.state)}</small>
                                </div>
                                {compactRunText(run.subagentTask || run.userRequest) ? <p>{compactRunText(run.subagentTask || run.userRequest)}</p> : null}
                                {compactRunText(activity, 100) ? <em>{compactRunText(activity, 100)}</em> : null}
                                <div className="claw-subagent-row-meta">
                                  {typeof run.subagentDepth === "number" ? <span>depth {run.subagentDepth}</span> : null}
                                  {typeof run.subagentMaxIterations === "number" ? <span>max {run.subagentMaxIterations}</span> : null}
                                  {(run.subagentToolsets ?? []).slice(0, 4).map((toolset) => <span key={toolset}>{toolset}</span>)}
                                  <span>{formatTime(run.lastActivityAt || run.updatedAt)}</span>
                                </div>
                                {acpUpdateLines.length > 0 ? (
                                  <div className="claw-acp-updates claw-acp-updates--compact">
                                    {acpUpdateLines.map((line) => <span className="claw-acp-update" key={line}>{line}</span>)}
                                  </div>
                                ) : null}
                                {run.error ? <strong>{run.error}</strong> : null}
                              </div>
                            );
                          })}
                        </div>
                      </div>
                    </div>
                  ) : null}
                  {activeProcessEvents.length > 0 ? (
                    activeProcessEvents.map((event) => (
                      <div className="claw-tl-node claw-tl-node--phase" key={`${event.processId}-${event.type}-${event.createdAt}`}>
                        <div className="claw-tl-dot"><Zap size={14} /></div>
                        <div className="claw-tl-content">
                          <div className="claw-tl-head">
                            <span className="claw-tl-title">{managedProcessEventLabel(event.type)}</span>
                            <small>{formatTime(event.createdAt)}</small>
                          </div>
                          <p>{managedProcessEventText(event)}</p>
                        </div>
                      </div>
                    ))
                  ) : null}
                  {graphEvents.length > 0 ? (
                    compactSteps(graphEvents).map((step, index, arr) => (
                      <TimelineStep step={step} key={step.key} isLast={index === arr.length - 1} />
                    ))
                  ) : null}
                  {activeRun && activeRun.state !== "completed" && activeRun.state !== "failed" && activeRun.state !== "aborted" ? (
                    <div className="claw-tl-node claw-tl-node--phase">
                      <div className="claw-tl-dot"><Brain size={14} className="claw-tl-icon-spin" /></div>
                      <div className="claw-tl-content">
                        <div className="claw-tl-head">
                          <span className="claw-tl-title">{runStateLabel(activeRun.state)}</span>
                          {activeRunActivityAt ? <small>{formatTime(activeRunActivityAt)}</small> : null}
                        </div>
                        {activeRunActivityDesc ? <p>最近活动：{activeRunActivityDesc}</p> : null}
                      </div>
                    </div>
                  ) : null}
                  {graphEvents.length === 0 && activeProcessEvents.length === 0 && !activeRun ? (
                    <div className="claw-panel-hint-box">
                      <Network size={18} />
                      <p>复杂任务会在这里显示规划、工具调用、MCP 返回与最终整理过程。</p>
                    </div>
                  ) : null}
                </div>
              </div>
            </div>

            {/* ── Artifacts Card ── */}
            <div className="claw-panel-card">
              <div className="claw-panel-head compact" onClick={() => setArtifactsCollapsed((v) => !v)} role="button" tabIndex={0} onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); setArtifactsCollapsed((v) => !v); } }}>
                <div className="claw-panel-head-left">
                  <span className="claw-panel-icon claw-panel-icon--orange"><FolderOpen size={14} /></span>
                  <div>
                    <span>Artifacts</span>
                    <strong>文件与预览</strong>
                  </div>
                </div>
                <div className="claw-panel-head-right">
                  {artifacts.length > 0 ? <small className="claw-count-badge">{artifacts.length}</small> : null}
                  <span className="claw-panel-chevron">{artifactsCollapsed ? <ChevronRight size={14} /> : <ChevronDown size={14} />}</span>
                </div>
              </div>
              <div className={`claw-panel-body${artifactsCollapsed ? " claw-panel-body--collapsed" : ""}`}>
                <div className="claw-artifact-list">
                  {artifacts.slice(0, 8).map((artifact) => (
                    <button key={artifact.path} onClick={() => setPreviewTarget(artifact)} type="button">
                      {artifact.kind === "image" ? <ImageIcon size={14} /> : <FileText size={14} />}
                      <span>{artifact.title}</span>
                      <small>{artifact.source}</small>
                    </button>
                  ))}
                  {artifacts.length === 0 ? <div className="claw-panel-hint-box"><FolderOpen size={18} /><p>工具生成的截图、文档和附件会显示在这里。</p></div> : null}
                </div>
              </div>
            </div>

            {/* ── Quick Skills Card ── */}
            <div className="claw-panel-card">
              <div className="claw-panel-head compact" onClick={() => setSkillsCollapsed((v) => !v)} role="button" tabIndex={0} onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); setSkillsCollapsed((v) => !v); } }}>
                <div className="claw-panel-head-left">
                  <span className="claw-panel-icon claw-panel-icon--indigo"><Zap size={14} /></span>
                  <div>
                    <span>Quick Skills</span>
                    <strong>技能快捷调用</strong>
                  </div>
                </div>
                <div className="claw-panel-head-right">
                  {activeSkills.length > 0 ? <small className="claw-count-badge">{activeSkills.length}</small> : null}
                  <span className="claw-panel-chevron">{skillsCollapsed ? <ChevronRight size={14} /> : <ChevronDown size={14} />}</span>
                </div>
              </div>
              <div className={`claw-panel-body${skillsCollapsed ? " claw-panel-body--collapsed" : ""}`}>
                <div className="claw-skill-chips">
                  {activeSkills.slice(0, 8).map((skill) => (
                    <button key={skill.id} onClick={() => insertSkill(skill.name)} type="button" title={skill.description}>
                      /{skill.name}
                    </button>
                  ))}
                  {activeSkills.length === 0 ? <div className="claw-panel-hint-box"><Zap size={18} /><p>当前智能体暂无已启用技能，进入 Skills 或 Agents 配置。</p></div> : null}
                </div>
              </div>
            </div>
          </aside>
        </div>

        <footer
          className={`claw-composer${dragActive ? " dragging" : ""}`}
          ref={composerRef}
          onDragEnter={handleFileDragEnter}
          onDragOver={handleFileDragOver}
          onDragLeave={handleFileDragLeave}
          onDrop={handleFileDrop}
        >
          <input
            ref={fileInputRef}
            multiple
            type="file"
            onChange={(event) => {
              if (event.currentTarget.files) void stageFiles(event.currentTarget.files);
              event.currentTarget.value = "";
            }}
            hidden
          />
          <div className="claw-composer-main">
            {emojiPickerOpen ? (
              <EmojiPicker groups={pickerEmojiGroups} onEmoji={insertEmoji} onPick={sendEmojiImage} />
            ) : null}
            {shortContextNotice ? (
              <div className="claw-context-hint">
                <Sparkles size={14} />
                <span>{shortContextNotice}</span>
              </div>
            ) : null}
            {composerError ? (
              <div className="claw-composer-error">
                <AlertCircle size={14} />
                <span>{composerError}</span>
              </div>
            ) : null}
            {slashCommandSuggestions.length > 0 ? (
              <div className="claw-command-suggestions">
                {slashCommandSuggestions.map((command, index) => {
                  const primary = `/${command.name}${command.argsHint ? ` ${command.argsHint}` : ""}`;
                  const aliases = command.aliases.map((alias) => `/${alias}`).join(" ");
                  return (
                    <button
                      className={index === selectedSlashCommandIndex ? "selected" : ""}
                      key={command.name}
                      onClick={() => insertControlCommand(command)}
                      onMouseEnter={() => setSelectedSlashCommandIndex(index)}
                      type="button"
                    >
                      <span>{command.category}</span>
                      <strong>{primary}</strong>
                      <small>{command.description}</small>
                      {aliases ? <code>{aliases}</code> : null}
                    </button>
                  );
                })}
              </div>
            ) : null}
            {attachments.length > 0 ? (
              <div className="claw-attachment-row">
                {attachments.map((file) => (
                  <div className={`claw-attachment ${file.status}`} key={file.id}>
                    {file.preview ? <img src={file.preview} alt={file.fileName} /> : <FileText size={16} />}
                    <span>{file.fileName}</span>
                    {file.status === "staging" ? <Loader2 className="spin" size={13} /> : null}
                    {file.status === "error" ? <small>{file.error || "上传失败"}</small> : null}
                    <button onClick={() => removeAttachment(file.id)} title="移除附件" type="button"><X size={12} /></button>
                  </div>
                ))}
              </div>
            ) : null}
          <textarea
            rows={1}
            aria-label="消息输入框"
            value={draft}
            onChange={(event) => {
              setDraft(event.target.value);
              if (composerError) setComposerError(null);
            }}
            onPaste={(event) => {
              if (event.clipboardData.files.length > 0) void stageFiles(event.clipboardData.files);
            }}
            onKeyDown={handleComposerKeyDown}
            placeholder={agentReady ? "描述任务，Enter 发送，Shift+Enter 换行..." : "请先在 Agents / MCP / Skills 中启用运行时配置..."}
          />
          </div>
          <button
            className={`claw-attach-button${voiceInputState !== "idle" ? " is-recording" : ""}`}
            disabled={voiceInputState === "transcribing" || (!voiceSupported && voiceInputState === "idle")}
            onClick={toggleVoiceInput}
            title={voiceInputState === "idle" ? "语音输入" : voiceInputState === "transcribing" ? "正在识别语音" : "停止语音输入"}
            type="button"
          >
            {voiceInputState === "idle" ? <Mic size={17} /> : voiceInputState === "transcribing" ? <Loader2 className="spin" size={17} /> : <MicOff size={17} />}
          </button>
          <button className="claw-attach-button" onClick={() => setEmojiPickerOpen((open) => !open)} title="表情" type="button">
            <Smile size={17} />
          </button>
          <button className="claw-attach-button" onClick={() => fileInputRef.current?.click()} title="发送文件" type="button">
            <Paperclip size={17} />
          </button>
          <button
            disabled={sendButtonStopsRun ? false : (!hasReadyComposerPayload || hasStagingAttachment)}
            onClick={() => sendButtonStopsRun ? void stopActiveRun() : void submit()}
            title={sendButtonStopsRun ? "结束当前运行" : isProcessing ? busySubmitTitle : "发送"}
            type="button"
          >
            {sendButtonStopsRun ? <Square size={15} fill="currentColor" /> : <SendHorizontal size={17} />}
          </button>
        </footer>
        {dragActive ? (
          <div
            className="claw-file-drop-overlay"
            onDragEnter={handleFileDragEnter}
            onDragOver={handleFileDragOver}
            onDragLeave={handleFileDragLeave}
            onDrop={handleFileDrop}
          >
            <div className="claw-file-drop-message">
              <Paperclip size={24} />
              <strong>松开即可添加</strong>
              <span>文件会作为本轮消息附件上传</span>
            </div>
          </div>
        ) : null}
        {previewTarget ? <ArtifactPreview target={previewTarget} onClose={() => setPreviewTarget(null)} /> : null}
      </article>
    </section>
  );
});





