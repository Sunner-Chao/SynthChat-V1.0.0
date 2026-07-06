import { ChangeEvent, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import {
  AlertTriangle,
  Bot,
  CheckCircle2,
  ChevronRight,
  Edit3,
  Eye,
  EyeOff,
  Globe,
  ImagePlus,
  Info,
  Loader2,
  Plus,
  Puzzle,
  RefreshCw,
  Search,
  Settings,
  Smartphone,
  Smile,
  Sparkles,
  Terminal,
  Upload,
  Video,
  Wand2,
  Wifi,
  XCircle,
  Palette,
  PlugZap
} from "lucide-react";
import { api } from "../lib/api";
import { maskSecret, formatTime, providerPresetLabel, providerPresetDefaults, imageProviderTypeLabel } from "../lib/formatters";
import { filterSkillsByQuery } from "../lib/skillSearch";
import { useAppStore, consumePendingSettingsView } from "../lib/store";
import type {
  AccountConfig,
  AppBuildInfo,
  AppSection,
  AppUpdateCheck,
  AgentConfig,
  AgentDefinition,
  BrowserProvider,
  ChatConfig,
  EmojiGroup,
  ImageProvider,
  LlmProvider,
  ModelCatalogEntry,
  McpServer,
  ModelCapabilities,
  Persona,
  ProfileConfig,
  SearchProvider,
  SkillSummary,
  ThemeConfig,
  VideoProvider,
  VideoSummaryConfig,
  VisionProvider,
  WechatConfig,
  WechatQrStartResult,
  WechatQrStatusResult,
  TokenUsageStats
} from "../lib/types";
import { Avatar, MenuRow } from "../components/common";


import { BackBtn, SecretInput } from "./settings/_shared";
import { NetworkSettings } from "./settings/NetworkSettings";
import { ThemeSettings } from "./settings/ThemeSettings";
import { ReplySettings } from "./settings/ReplySettings";
import { InfoDocument } from "./settings/InfoDocument";
import { SearchProviderSettings } from "./settings/SearchProviderSettings";
import { VideoProviderSettings } from "./settings/VideoProviderSettings";
import { AboutSettings, readUpdateManifestUrl, writeUpdateManifestUrl } from "./settings/AboutSettings";
import { AgentSettingsRedirect } from "./settings/AgentSettingsRedirect";
import { BrowserProviderSettings } from "./settings/BrowserProviderSettings";
import { ProfileSettings } from "./settings/ProfileSettings";
import { EmojiSettings } from "./settings/EmojiSettings";
import { VideoSummarySettings, defaultVideoSummaryConfig } from "./settings/VideoSummarySettings";
import { VisionProviderSettings } from "./settings/VisionProviderSettings";
import { AgentSettings } from "./settings/AgentSettings";
import { AccountsSettings } from "./settings/AccountsSettings";
import { ProviderSettings } from "./settings/ProviderSettings";
import { ImageProviderSettings } from "./settings/ImageProviderSettings";



type SettingsView =
  | "menu"
  | "profile"
  | "accounts"
  | "providers"
  | "imageProviders"
  | "videoProviders"
  | "searchProviders"
  | "visionProviders"
  | "browserProviders"
  | "videoSummary"
  | "chat"
  | "reply"
  | "theme"
  | "emoji"
  | "agent"
  | "network"
  | "about"
  | "privacy"
  | "statement";

export function SettingsPanel() {
  const {
    config,
    llmProviders,
    mcpServers,
    agentConfig,
    agents,
    skills,
    plugins,
    profile,
    accounts,
    personas,
    imageProviders,
    videoProviders,
    searchProviders,
    visionProviders,
    browserProviders,
    themes,
    emojiGroups,
    saveLlmProviders,
    saveConfig,
    saveProfile,
    uploadProfileAvatar,
    clearProfileAvatar,
    refreshAccounts,
    saveAccounts,
    saveImageProviders,
    saveVideoProviders,
    saveSearchProviders,
    saveVisionProviders,
    saveBrowserProviders,
    saveThemes,
    importThemeCss,
    saveEmojiGroups,
    uploadEmojiImage,
    saveAgentConfig,
    installBuiltinSkills,
    refreshSkills,
    setSection,
    focusedAgentId,
    setSkillsPanelMode,
    setMcpPanelMode
  } = useAppStore();
  const [view, setView] = useState<SettingsView>("menu");
  const [menuAppVersion, setMenuAppVersion] = useState("");
  const focusedAgent = agents.find((agent) => agent.id === focusedAgentId) ?? agents.find((agent) => agent.isDefault) ?? agents[0] ?? null;

  useEffect(() => {
    void getVersion().then((v) => setMenuAppVersion(`v${v}`)).catch(() => {});
  }, []);

  // Read pendingSettingsView once on mount (before paint)
  useLayoutEffect(() => {
    const pending = consumePendingSettingsView();
    if (pending) {
      setView(pending as SettingsView);
    }
  }, []);

  if (!config) {
    return (
      <section className="simple-page">
        <div className="empty-state compact"><Settings size={32} /><p>设置加载中...</p></div>
      </section>
    );
  }

  const saveChat = async (patch: Partial<typeof config.chat>) => {
    await saveConfig({ ...config, chat: { ...config.chat, ...patch } });
  };
  const saveReply = async (patch: Partial<typeof config.reply>) => {
    await saveConfig({ ...config, reply: { ...config.reply, ...patch } });
  };
  const saveWeb = async (patch: Partial<typeof config.web>) => {
    await saveConfig({ ...config, web: { ...config.web, ...patch } });
  };

  if (view !== "menu") {
    const goBackToMenu = () => setView("menu");
    return (
      <section className="simple-page">
        {view === "profile" ? (
          <ProfileSettings
            onBack={goBackToMenu}
            clearAvatar={clearProfileAvatar}
            profile={profile}
            saveProfile={saveProfile}
            uploadAvatar={uploadProfileAvatar}
          />
        ) : null}
        {view === "accounts" ? (
          <AccountsSettings onBack={goBackToMenu} accounts={accounts} personas={personas} refreshAccounts={refreshAccounts} saveAccounts={saveAccounts} />
        ) : null}
        {view === "providers" ? (
          <ProviderSettings onBack={goBackToMenu} providers={llmProviders} saveProviders={saveLlmProviders} />
        ) : null}
        {view === "imageProviders" ? (
          <ImageProviderSettings onBack={goBackToMenu} providers={imageProviders} saveProviders={saveImageProviders} />
        ) : null}
        {view === "videoProviders" ? (
          <VideoProviderSettings onBack={goBackToMenu} providers={videoProviders} saveProviders={saveVideoProviders} />
        ) : null}
        {view === "searchProviders" ? (
          <SearchProviderSettings onBack={goBackToMenu} providers={searchProviders} saveProviders={saveSearchProviders} />
        ) : null}
        {view === "visionProviders" ? (
          <VisionProviderSettings onBack={goBackToMenu} providers={visionProviders} saveProviders={saveVisionProviders} />
        ) : null}
        {view === "browserProviders" ? (
          <BrowserProviderSettings onBack={goBackToMenu} providers={browserProviders} saveProviders={saveBrowserProviders} />
        ) : null}
        {view === "videoSummary" ? (
          <VideoSummarySettings onBack={goBackToMenu} config={config.videoSummary ?? defaultVideoSummaryConfig()} onSave={(patch) => saveConfig({ ...config, videoSummary: { ...(config.videoSummary ?? defaultVideoSummaryConfig()), ...patch } })} />
        ) : null}
        {view === "chat" ? (
          <ChatSettings onBack={goBackToMenu} config={config.chat} llmProviders={llmProviders} onSave={saveChat} />
        ) : null}
        {view === "reply" ? (
          <ReplySettings onBack={goBackToMenu} config={config.reply} onSave={saveReply} />
        ) : null}
        {view === "theme" ? (
          <ThemeSettings onBack={goBackToMenu} importThemeCss={importThemeCss} saveThemes={saveThemes} themes={themes} />
        ) : null}
        {view === "emoji" ? (
          <EmojiSettings onBack={goBackToMenu} groups={emojiGroups} saveGroups={saveEmojiGroups} uploadImage={uploadEmojiImage} />
        ) : null}
        {view === "agent" ? (
          <AgentSettingsRedirect
            agents={agents}
            serversCount={mcpServers.length}
            setSection={setSection}
            skillsCount={skills.length}
          />
        ) : null}
        {view === "network" ? (
          <NetworkSettings onBack={goBackToMenu} config={config.web} weather={config.weather} onSave={saveWeb} onSaveWeather={(patch) => saveConfig({ ...config, weather: { ...config.weather, ...patch } })} />
        ) : null}
        {view === "about" ? (
          <AboutSettings onBack={goBackToMenu} setView={setView} />
        ) : null}
        {view === "privacy" ? (
          <InfoDocument onBack={goBackToMenu}
            title="隐私说明及设置"
            body={[
              "SynthChat 原版包含匿名使用统计开关，用于判断版本使用情况和功能优先级。",
              "重构版默认关闭遥测；不会上传聊天记录、图片、文件、角色设定、世界书、记忆、API Key 或本机文件。",
              `当前状态：${config.telemetryEnabled ? "已开启" : "已关闭"}`
            ]}
          />
        ) : null}
        {view === "statement" ? (
          <InfoDocument onBack={goBackToMenu}
            title="软件声明"
            body={[
              "SynthChat 兼容 SillyTavern 角色卡和世界书格式，是为了方便迁移已有创作。",
              "导入、传播或商用他人角色卡、世界书、图片素材前，请自行确认授权。",
              "重构版目前以本地桌面端为主，公网面板、微信传输和自动更新会逐步补齐。"
            ]}
          />
        ) : null}
      </section>
    );
  }

  return (
    <section className="simple-page">
      <button className="status-card clickable-card" onClick={() => setView("profile")} type="button">
        <Avatar name={profile.name} src={profile.avatarPath ? api.assetUrl(profile.avatarPath) : ""} />
        <div className="info">
          <div className="app-name">{profile.name}</div>
          <div className="version">SynthChat {menuAppVersion}</div>
        </div>
        <ChevronRight size={18} />
      </button>

      <div className="menu-card">
        <MenuRow icon={Smartphone} label="微信账号" value={`${accounts.length} 个`} onClick={() => setView("accounts")} iconColor="green" />
        <MenuRow icon={Wand2} label="对话服务商" value={`${llmProviders.length} 个`} onClick={() => setView("providers")} iconColor="orange" />
        <MenuRow icon={ImagePlus} label="生图服务商" value={`${imageProviders.length} 个`} onClick={() => setView("imageProviders")} iconColor="peach" />
        <MenuRow icon={Video} label="视频生成服务商" value={`${videoProviders.length} 个`} onClick={() => setView("videoProviders")} iconColor="purple" />
        <MenuRow icon={Globe} label="搜索服务" value={`${searchProviders.length} 个`} onClick={() => setView("searchProviders")} iconColor="cyan" />
        <MenuRow icon={Bot} label="识图服务" value={`${visionProviders.length} 个`} onClick={() => setView("visionProviders")} iconColor="indigo" />
        <MenuRow icon={Globe} label="浏览器服务" value={`${browserProviders.length} 个`} onClick={() => setView("browserProviders")} iconColor="blue" />
        <MenuRow icon={Wand2} label="视频总结" value={config.videoSummary?.enabled === false ? "已关闭" : config.videoSummary?.transcriber || "auto"} onClick={() => setView("videoSummary")} iconColor="purple" />
        <MenuRow
          icon={Puzzle}
          label="MCP 扩展"
          value={mcpServers.length ? `${mcpServers.length} 个全局服务` : "未配置"}
          onClick={() => {
            setMcpPanelMode("global");
            setSection("mcp");
          }}
          iconColor="blue"
        />
        <MenuRow
          icon={Bot}
          label="Agent 配置"
          value={focusedAgent?.name || `${agents.length} 个智能体`}
          onClick={() => setSection("agents")}
          iconColor="indigo"
        />
        <MenuRow
          icon={Wand2}
          label="Skills 技能包"
          value={`${skills.length} 个全局技能`}
          onClick={() => {
            setSkillsPanelMode("global");
            setSection("skills");
          }}
          iconColor="purple"
        />
        <MenuRow icon={PlugZap} label="插件管理" value={`${plugins.length} 个`} onClick={() => setSection("plugins")} iconColor="red" />
        <MenuRow icon={Settings} label="对话设置" onClick={() => setView("chat")} iconColor="primary" />
        <MenuRow icon={Edit3} label="回复设置" onClick={() => setView("reply")} iconColor="indigo" />
        <MenuRow icon={Palette} label="主题" value={`${themes.filter((theme) => theme.active).length} 个已应用`} onClick={() => setView("theme")} iconColor="purple" />
        <MenuRow icon={Smile} label="表情包管理" value={`${emojiGroups.length} 个分组`} onClick={() => setView("emoji")} iconColor="yellow" />
        <MenuRow icon={Globe} label="网络设置" onClick={() => setView("network")} iconColor="cyan" />
        <MenuRow
          icon={Terminal}
          label="环境检查"
          value={config.chat?.skipEnvCheck ? "已跳过" : "启动时检查"}
          onClick={async () => {
            const newValue = !(config.chat?.skipEnvCheck ?? false);
            await saveChat({ skipEnvCheck: newValue });
          }}
          iconColor="cyan"
        />
        <MenuRow icon={Info} label="关于 SynthChat" onClick={() => setView("about")} iconColor="neutral" />
      </div>
    </section>
  );
}

function settingsIcon(view: SettingsView) {
  const icons: Partial<Record<SettingsView, typeof Smartphone>> = {
    profile: Smartphone,
    accounts: Smartphone,
    providers: Wand2,
    imageProviders: ImagePlus,
    videoProviders: Video,
    searchProviders: Globe,
    visionProviders: Bot,
    browserProviders: Globe,
    videoSummary: Wand2,
    chat: Settings,
    reply: Edit3,
    theme: Palette,
    emoji: Smile,
    agent: Bot,
    network: Globe,
    about: Info,
    privacy: Info,
    statement: Info
  };
  return icons[view];
}

function settingsTitle(view: SettingsView) {
  const titles: Record<SettingsView, string> = {
    menu: "设置",
    profile: "个人资料",
    accounts: "微信账号",
    providers: "对话服务商",
    imageProviders: "生图服务商",
    videoProviders: "视频生成服务商",
    searchProviders: "搜索服务",
    visionProviders: "识图服务",
    browserProviders: "浏览器服务",
    videoSummary: "视频总结",
    chat: "对话设置",
    reply: "回复设置",
    theme: "主题",
    emoji: "表情包管理",
    agent: "Agent 与 Skills",
    network: "网络设置",
    about: "关于 SynthChat",
    privacy: "隐私说明及设置",
    statement: "软件声明"
  };
  return titles[view];
}



// ---------------------------------------------------------------------------
// ChatSettings local form state — single source of truth for all form fields.
// Adding a new config field: update ChatFormState + formStateFromConfig only.
// ---------------------------------------------------------------------------

interface ChatFormState {
  wait: number;
  dedupEnabled: boolean;
  dedupWindow: number;
  runTimeout: number;
  busyInputMode: string;
  shortContextAbortOnSummaryFailure: boolean;
  shortContextSummaryProviderId: string;
  shortContextSummaryModel: string;
  autoTitleEnabled: boolean;
  uiLimit: number;
  artifactLimit: number;
  previewChars: number;
  streamChars: number;
  thinkingMs: number;
  petCloudDurationSeconds: number;
  bottomThreshold: number;
  activePollMs: number;
  idlePollMs: number;
  intentMode: string;
  intentProviderId: string;
  intentModel: string;
  intentEmbeddingConfidence: number;
  intentLlmConfidence: number;
  intentLlmTimeout: number;
  intentLlmMaxTokens: number;
  intentLlmPrompt: string;
  routerEnabled: boolean;
  routerConfidence: number;
  routerTimeout: number;
  routerMaxTokens: number;
  routerPrompt: string;
  toolUseEnforcement: string;
  toolParallelEnabled: boolean;
  toolParallelLimit: number;
  sendMessageToolEnabled: boolean;
  toolApprovalMode: string;
  trustedToolPatterns: string[];
  trustedToolPatternDraft: string;
  trustedCommandPatterns: string[];
  trustedCommandPatternDraft: string;
  llmCredentialPoolStrategy: string;
  toolEnvPassthroughDraft: string;
  llmRetryCount: number;
  llmRetryBackoff: number;
  responsesReasoningReplayEnabled: boolean;
  toolRetryCount: number;
  toolRetryBackoff: number;
  guardWarningsEnabled: boolean;
  guardHardStopEnabled: boolean;
  guardExactWarnAfter: number;
  guardSameToolWarnAfter: number;
  guardNoProgressWarnAfter: number;
  guardExactLimit: number;
  guardSameToolLimit: number;
  guardNoProgressLimit: number;
  backgroundSkillReviewEnabled: boolean;
  backgroundSkillReviewAutoCreateEnabled: boolean;
  backgroundSkillCuratorEnabled: boolean;
  backgroundSkillCuratorIntervalHours: number;
  skillHotReloadEnabled: boolean;
  skillHotReloadInterval: number;
  cleanupEnabled: boolean;
  retentionDays: number;
  storedMessages: number;
  storedRuns: number;
  storedTraces: number;
}

function formStateFromConfig(cfg: ChatConfig): ChatFormState {
  return {
    wait: cfg.queueWaitSeconds,
    dedupEnabled: cfg.messageDedupEnabled !== false,
    dedupWindow: cfg.messageDedupWindowSeconds ?? 30,
    runTimeout: cfg.agentRunTimeoutSeconds ?? 600,
    busyInputMode: cfg.busyInputMode ?? "queue",
    shortContextAbortOnSummaryFailure: cfg.shortContextAbortOnSummaryFailure === true,
    shortContextSummaryProviderId: cfg.shortContextSummaryProviderId ?? "",
    shortContextSummaryModel: cfg.shortContextSummaryModel ?? "",
    autoTitleEnabled: cfg.autoTitleEnabled !== false,
    uiLimit: cfg.uiMessageLimit ?? 180,
    artifactLimit: cfg.artifactScanLimit ?? 80,
    previewChars: cfg.uiMessagePreviewChars ?? 12000,
    streamChars: cfg.uiStreamCharsPerSecond ?? 36,
    thinkingMs: cfg.thinkingMinVisibleMs ?? 1800,
    petCloudDurationSeconds: cfg.petCloudDurationSeconds ?? 10,
    bottomThreshold: cfg.bottomFollowThresholdPx ?? 180,
    activePollMs: cfg.activePollIntervalMs ?? 1500,
    idlePollMs: cfg.idlePollIntervalMs ?? 3000,
    intentMode: cfg.intentAnalyzerMode ?? "llm",
    intentProviderId: cfg.intentAnalyzerProviderId ?? "",
    intentModel: cfg.intentAnalyzerModel ?? "",
    intentEmbeddingConfidence: cfg.intentEmbeddingMinConfidence ?? 0.62,
    intentLlmConfidence: cfg.intentLlmMinConfidence ?? 0.65,
    intentLlmTimeout: cfg.intentLlmTimeoutSeconds ?? 10,
    intentLlmMaxTokens: cfg.intentLlmMaxTokens ?? 384,
    intentLlmPrompt: cfg.intentLlmPrompt ?? "",
    routerEnabled: cfg.toolRouterLlmEnabled !== false,
    routerConfidence: cfg.toolRouterLlmMinConfidence ?? 0.72,
    routerTimeout: cfg.toolRouterLlmTimeoutSeconds ?? 15,
    routerMaxTokens: cfg.toolRouterLlmMaxTokens ?? 2048,
    routerPrompt: cfg.toolRouterLlmPrompt ?? "",
    toolUseEnforcement: cfg.toolUseEnforcement ?? "auto",
    toolParallelEnabled: cfg.toolParallelEnabled !== false,
    toolParallelLimit: cfg.toolParallelLimit ?? 4,
    sendMessageToolEnabled: cfg.sendMessageToolEnabled === true,
    toolApprovalMode: cfg.toolApprovalMode ?? "risky",
    trustedToolPatterns: cfg.trustedToolPatterns ?? [],
    trustedToolPatternDraft: "",
    trustedCommandPatterns: cfg.trustedCommandPatterns ?? [],
    trustedCommandPatternDraft: "",
    llmCredentialPoolStrategy: cfg.llmCredentialPoolStrategy ?? "fill_first",
    toolEnvPassthroughDraft: (cfg.toolEnvPassthrough ?? []).join("\n"),
    llmRetryCount: cfg.llmRetryCount ?? 2,
    llmRetryBackoff: cfg.llmRetryBackoffMs ?? 800,
    responsesReasoningReplayEnabled: cfg.responsesReasoningReplayEnabled !== false,
    toolRetryCount: cfg.toolCallRetryCount ?? 1,
    toolRetryBackoff: cfg.toolCallRetryBackoffMs ?? 300,
    guardWarningsEnabled: cfg.toolGuardrailWarningsEnabled !== false,
    guardHardStopEnabled: cfg.toolGuardrailHardStopEnabled === true,
    guardExactWarnAfter: cfg.toolGuardrailExactFailureWarnAfter ?? 2,
    guardSameToolWarnAfter: cfg.toolGuardrailSameToolFailureWarnAfter ?? 3,
    guardNoProgressWarnAfter: cfg.toolGuardrailNoProgressWarnAfter ?? 2,
    guardExactLimit: cfg.toolGuardrailExactFailureLimit ?? 5,
    guardSameToolLimit: cfg.toolGuardrailSameToolFailureLimit ?? 8,
    guardNoProgressLimit: cfg.toolGuardrailNoProgressLimit ?? 5,
    backgroundSkillReviewEnabled: cfg.backgroundSkillReviewEnabled !== false,
    backgroundSkillReviewAutoCreateEnabled: cfg.backgroundSkillReviewAutoCreateEnabled === true,
    backgroundSkillCuratorEnabled: cfg.backgroundSkillCuratorEnabled !== false,
    backgroundSkillCuratorIntervalHours: cfg.backgroundSkillCuratorIntervalHours ?? 168,
    skillHotReloadEnabled: cfg.skillHotReloadEnabled !== false,
    skillHotReloadInterval: cfg.skillHotReloadIntervalSeconds ?? 3,
    cleanupEnabled: cfg.historyCleanupEnabled !== false,
    retentionDays: cfg.historyRetentionDays ?? 14,
    storedMessages: cfg.maxStoredMessagesPerConversation ?? 300,
    storedRuns: cfg.maxStoredAgentRuns ?? 50,
    storedTraces: cfg.maxStoredToolTraces ?? 100,
  };
}

function ChatSettings({
  onBack,
  config,
  llmProviders,
  onSave
}: {
  onBack?: () => void;
  config: ChatConfig;
  llmProviders: LlmProvider[];
  onSave: (patch: Partial<ChatConfig>) => Promise<void>;
}) {
  // Single state object — all form fields live here.
  const [form, setForm] = useState<ChatFormState>(() => formStateFromConfig(config));
  const patchForm = useCallback(
    <K extends keyof ChatFormState>(key: K, value: ChatFormState[K]) =>
      setForm(prev => ({ ...prev, [key]: value })),
    [],
  );

  // Destructure for JSX compatibility — all reads stay unchanged.
  const {
    wait, dedupEnabled, dedupWindow, runTimeout, busyInputMode,
    shortContextAbortOnSummaryFailure, shortContextSummaryProviderId, shortContextSummaryModel,
    autoTitleEnabled, uiLimit, artifactLimit, previewChars, streamChars, thinkingMs,
    petCloudDurationSeconds, bottomThreshold, activePollMs, idlePollMs,
    intentMode, intentProviderId, intentModel, intentEmbeddingConfidence, intentLlmConfidence,
    intentLlmTimeout, intentLlmMaxTokens, intentLlmPrompt,
    routerEnabled, routerConfidence, routerTimeout, routerMaxTokens, routerPrompt,
    toolUseEnforcement, toolParallelEnabled, toolParallelLimit, sendMessageToolEnabled, toolApprovalMode,
    trustedToolPatterns, trustedToolPatternDraft, trustedCommandPatterns, trustedCommandPatternDraft,
    llmCredentialPoolStrategy, toolEnvPassthroughDraft,
    llmRetryCount, llmRetryBackoff, responsesReasoningReplayEnabled,
    toolRetryCount, toolRetryBackoff,
    guardWarningsEnabled, guardHardStopEnabled, guardExactWarnAfter, guardSameToolWarnAfter,
    guardNoProgressWarnAfter, guardExactLimit, guardSameToolLimit, guardNoProgressLimit,
    backgroundSkillReviewEnabled, backgroundSkillReviewAutoCreateEnabled,
    backgroundSkillCuratorEnabled, backgroundSkillCuratorIntervalHours,
    skillHotReloadEnabled, skillHotReloadInterval,
    cleanupEnabled, retentionDays, storedMessages, storedRuns, storedTraces,
  } = form;

  // Thin setter wrappers — all existing call-sites in JSX stay unchanged.
  const setWait = (v: number) => patchForm("wait", v);
  const setDedupEnabled = (v: boolean) => patchForm("dedupEnabled", v);
  const setDedupWindow = (v: number) => patchForm("dedupWindow", v);
  const setRunTimeout = (v: number) => patchForm("runTimeout", v);
  const setBusyInputMode = (v: string) => patchForm("busyInputMode", v);
  const setShortContextAbortOnSummaryFailure = (v: boolean) => patchForm("shortContextAbortOnSummaryFailure", v);
  const setShortContextSummaryProviderId = (v: string) => patchForm("shortContextSummaryProviderId", v);
  const setShortContextSummaryModel = (v: string) => patchForm("shortContextSummaryModel", v);
  const setAutoTitleEnabled = (v: boolean) => patchForm("autoTitleEnabled", v);
  const setUiLimit = (v: number) => patchForm("uiLimit", v);
  const setArtifactLimit = (v: number) => patchForm("artifactLimit", v);
  const setPreviewChars = (v: number) => patchForm("previewChars", v);
  const setStreamChars = (v: number) => patchForm("streamChars", v);
  const setThinkingMs = (v: number) => patchForm("thinkingMs", v);
  const setPetCloudDurationSeconds = (v: number) => patchForm("petCloudDurationSeconds", v);
  const setBottomThreshold = (v: number) => patchForm("bottomThreshold", v);
  const setActivePollMs = (v: number) => patchForm("activePollMs", v);
  const setIdlePollMs = (v: number) => patchForm("idlePollMs", v);
  const setIntentMode = (v: string) => patchForm("intentMode", v);
  const setIntentProviderId = (v: string) => patchForm("intentProviderId", v);
  const setIntentModel = (v: string) => patchForm("intentModel", v);
  const setIntentEmbeddingConfidence = (v: number) => patchForm("intentEmbeddingConfidence", v);
  const setIntentLlmConfidence = (v: number) => patchForm("intentLlmConfidence", v);
  const setIntentLlmTimeout = (v: number) => patchForm("intentLlmTimeout", v);
  const setIntentLlmMaxTokens = (v: number) => patchForm("intentLlmMaxTokens", v);
  const setIntentLlmPrompt = (v: string) => patchForm("intentLlmPrompt", v);
  const setRouterEnabled = (v: boolean) => patchForm("routerEnabled", v);
  const setRouterConfidence = (v: number) => patchForm("routerConfidence", v);
  const setRouterTimeout = (v: number) => patchForm("routerTimeout", v);
  const setRouterMaxTokens = (v: number) => patchForm("routerMaxTokens", v);
  const setRouterPrompt = (v: string) => patchForm("routerPrompt", v);
  const setToolUseEnforcement = (v: string) => patchForm("toolUseEnforcement", v);
  const setToolParallelEnabled = (v: boolean) => patchForm("toolParallelEnabled", v);
  const setToolParallelLimit = (v: number) => patchForm("toolParallelLimit", v);
  const setSendMessageToolEnabled = (v: boolean) => patchForm("sendMessageToolEnabled", v);
  const setToolApprovalMode = (v: string) => patchForm("toolApprovalMode", v);
  const setTrustedToolPatterns = (v: string[]) => patchForm("trustedToolPatterns", v);
  const setTrustedToolPatternDraft = (v: string) => patchForm("trustedToolPatternDraft", v);
  const setTrustedCommandPatterns = (v: string[]) => patchForm("trustedCommandPatterns", v);
  const setTrustedCommandPatternDraft = (v: string) => patchForm("trustedCommandPatternDraft", v);
  const setLlmCredentialPoolStrategy = (v: string) => patchForm("llmCredentialPoolStrategy", v);
  const setToolEnvPassthroughDraft = (v: string) => patchForm("toolEnvPassthroughDraft", v);
  const setLlmRetryCount = (v: number) => patchForm("llmRetryCount", v);
  const setLlmRetryBackoff = (v: number) => patchForm("llmRetryBackoff", v);
  const setResponsesReasoningReplayEnabled = (v: boolean) => patchForm("responsesReasoningReplayEnabled", v);
  const setToolRetryCount = (v: number) => patchForm("toolRetryCount", v);
  const setToolRetryBackoff = (v: number) => patchForm("toolRetryBackoff", v);
  const setGuardWarningsEnabled = (v: boolean) => patchForm("guardWarningsEnabled", v);
  const setGuardHardStopEnabled = (v: boolean) => patchForm("guardHardStopEnabled", v);
  const setGuardExactWarnAfter = (v: number) => patchForm("guardExactWarnAfter", v);
  const setGuardSameToolWarnAfter = (v: number) => patchForm("guardSameToolWarnAfter", v);
  const setGuardNoProgressWarnAfter = (v: number) => patchForm("guardNoProgressWarnAfter", v);
  const setGuardExactLimit = (v: number) => patchForm("guardExactLimit", v);
  const setGuardSameToolLimit = (v: number) => patchForm("guardSameToolLimit", v);
  const setGuardNoProgressLimit = (v: number) => patchForm("guardNoProgressLimit", v);
  const setBackgroundSkillReviewEnabled = (v: boolean) => patchForm("backgroundSkillReviewEnabled", v);
  const setBackgroundSkillReviewAutoCreateEnabled = (v: boolean) => patchForm("backgroundSkillReviewAutoCreateEnabled", v);
  const setBackgroundSkillCuratorEnabled = (v: boolean) => patchForm("backgroundSkillCuratorEnabled", v);
  const setBackgroundSkillCuratorIntervalHours = (v: number) => patchForm("backgroundSkillCuratorIntervalHours", v);
  const setSkillHotReloadEnabled = (v: boolean) => patchForm("skillHotReloadEnabled", v);
  const setSkillHotReloadInterval = (v: number) => patchForm("skillHotReloadInterval", v);
  const setCleanupEnabled = (v: boolean) => patchForm("cleanupEnabled", v);
  const setRetentionDays = (v: number) => patchForm("retentionDays", v);
  const setStoredMessages = (v: number) => patchForm("storedMessages", v);
  const setStoredRuns = (v: number) => patchForm("storedRuns", v);
  const setStoredTraces = (v: number) => patchForm("storedTraces", v);

  // Reset form state whenever the config prop changes atomically.
  useEffect(() => { setForm(formStateFromConfig(config)); }, [config]);

  const save = () => void onSave({
    busyInputMode: busyInputMode,
    messageDedupEnabled: dedupEnabled,
    messageDedupWindowSeconds: dedupWindow,
    shortContextAbortOnSummaryFailure: shortContextAbortOnSummaryFailure,
    shortContextSummaryProviderId: shortContextSummaryProviderId,
    shortContextSummaryModel: shortContextSummaryModel,
    autoTitleEnabled: autoTitleEnabled,
    queueWaitSeconds: wait,
    agentRunTimeoutSeconds: runTimeout,
    uiMessageLimit: uiLimit,
    artifactScanLimit: artifactLimit,
    uiMessagePreviewChars: previewChars,
    uiStreamCharsPerSecond: streamChars,
    thinkingMinVisibleMs: thinkingMs,
    petCloudDurationSeconds: petCloudDurationSeconds,
    bottomFollowThresholdPx: bottomThreshold,
    activePollIntervalMs: activePollMs,
    idlePollIntervalMs: idlePollMs,
    intentAnalyzerMode: intentMode === "llm" ? "llm" : "embedding",
    intentAnalyzerProviderId: intentProviderId,
    intentAnalyzerModel: intentModel,
    intentEmbeddingMinConfidence: intentEmbeddingConfidence,
    intentLlmMinConfidence: intentLlmConfidence,
    intentLlmTimeoutSeconds: intentLlmTimeout,
    intentLlmMaxTokens: intentLlmMaxTokens,
    intentLlmPrompt: intentLlmPrompt,
    toolRouterMode: "llm_unified",
    toolRouterLlmEnabled: routerEnabled,
    toolRouterLlmMinConfidence: routerConfidence,
    toolRouterLlmTimeoutSeconds: routerTimeout,
    toolRouterLlmMaxTokens: routerMaxTokens,
    toolRouterLlmPrompt: routerPrompt,
    toolUseEnforcement: toolUseEnforcement,
    toolParallelEnabled: toolParallelEnabled,
    toolParallelLimit: toolParallelLimit,
    sendMessageToolEnabled: sendMessageToolEnabled,
    toolApprovalMode: toolApprovalMode,
    trustedToolPatterns: trustedToolPatterns,
    trustedCommandPatterns: trustedCommandPatterns,
    llmCredentialPoolStrategy: llmCredentialPoolStrategy,
    toolEnvPassthrough: toolEnvPassthroughDraft
      .split(/[\n,;]/)
      .map((item) => item.trim())
      .filter(Boolean),
    llmRetryCount: llmRetryCount,
    llmRetryBackoffMs: llmRetryBackoff,
    responsesReasoningReplayEnabled: responsesReasoningReplayEnabled,
    toolCallRetryCount: toolRetryCount,
    toolCallRetryBackoffMs: toolRetryBackoff,
    toolGuardrailWarningsEnabled: guardWarningsEnabled,
    toolGuardrailHardStopEnabled: guardHardStopEnabled,
    toolGuardrailExactFailureWarnAfter: guardExactWarnAfter,
    toolGuardrailSameToolFailureWarnAfter: guardSameToolWarnAfter,
    toolGuardrailNoProgressWarnAfter: guardNoProgressWarnAfter,
    toolGuardrailExactFailureLimit: guardExactLimit,
    toolGuardrailSameToolFailureLimit: guardSameToolLimit,
    toolGuardrailNoProgressLimit: guardNoProgressLimit,
    backgroundSkillReviewEnabled: backgroundSkillReviewEnabled,
    backgroundSkillReviewAutoCreateEnabled: backgroundSkillReviewAutoCreateEnabled,
    backgroundSkillCuratorEnabled: backgroundSkillCuratorEnabled,
    backgroundSkillCuratorIntervalHours: backgroundSkillCuratorIntervalHours,
    skillHotReloadEnabled: skillHotReloadEnabled,
    skillHotReloadIntervalSeconds: skillHotReloadInterval,
    historyCleanupEnabled: cleanupEnabled,
    historyRetentionDays: retentionDays,
    maxStoredMessagesPerConversation: storedMessages,
    maxStoredAgentRuns: storedRuns,
    maxStoredToolTraces: storedTraces
  });

  const addTrustedToolPattern = async () => {
    const pattern = trustedToolPatternDraft.trim();
    if (!pattern) return;
    const next = await api.addTrustedToolPattern(pattern);
    setTrustedToolPatterns(next.chat.trustedToolPatterns ?? []);
    setTrustedToolPatternDraft("");
  };

  const removeTrustedToolPattern = async (pattern: string) => {
    const next = await api.removeTrustedToolPattern(pattern);
    setTrustedToolPatterns(next.chat.trustedToolPatterns ?? []);
  };

  const addTrustedCommandPattern = () => {
    const pattern = trustedCommandPatternDraft.trim();
    if (!pattern) return;
    setTrustedCommandPatterns(Array.from(new Set([...trustedCommandPatterns, pattern])));
    setTrustedCommandPatternDraft("");
  };

  const removeTrustedCommandPattern = (pattern: string) => {
    setTrustedCommandPatterns(trustedCommandPatterns.filter((item) => item !== pattern));
  };

  const approvalModeHint = {
    risky: "只拦截工具声明需要审批、非只读 HTTP、命令、写入、删除、提交、发送等高风险调用；可信规则会跳过审批。",
    smart: "高风险调用先由辅助 LLM 评估；明显安全时自动放行，明显危险时阻断，不确定时等待人工审批。",
    always: "所有外部工具调用都先暂停等待确认；可信规则仍可直接放行。",
    never: "默认直接执行外部工具调用；仍保留运行记录和工具轨迹，适合本机可信环境。"
  }[toolApprovalMode] ?? "未知审批模式。";

  return (
    <div className="primary-panel embedded-panel">
      <div className="panel-title action-title">
        <BackBtn onBack={onBack} />
        <div className="panel-title-text"><span>Chat</span><strong>对话设置</strong></div>
        <button className="btn-primary" onClick={save} type="button">保存</button>
      </div>
      <div className="card" style={{ margin: "0 16px 12px" }}>
        <div className="card-header">消息队列</div>
        <div className="form-group">
          <div className="form-row">
            <label>忙碌时输入</label>
            <select value={busyInputMode} onChange={(event) => setBusyInputMode(event.target.value)}>
              <option value="queue">加入队列</option>
              <option value="steer">注入规划</option>
              <option value="interrupt">中止并重开</option>
            </select>
          </div>
          <div className="form-row">
            <label>自动标题</label>
            <input checked={autoTitleEnabled} onChange={(event) => setAutoTitleEnabled(event.target.checked)} type="checkbox" />
          </div>
          <div className="form-row">
            <label>队列等待时间（秒）</label>
            <div className="stepper">
              <button onClick={() => setWait(Math.max(0, wait - 1))} type="button">−</button>
              <span className="stepper-val">{wait}</span>
              <button onClick={() => setWait(wait + 1)} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>单轮最长运行（秒）</label>
            <div className="stepper">
              <button onClick={() => setRunTimeout(Math.max(0, runTimeout - 30))} type="button">−</button>
              <span className="stepper-val">{runTimeout}</span>
              <button onClick={() => setRunTimeout(runTimeout + 30)} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>重复请求拦截</label>
            <input checked={dedupEnabled} onChange={(event) => setDedupEnabled(event.target.checked)} type="checkbox" />
          </div>
          <div className="form-row" style={{ opacity: dedupEnabled ? 1 : 0.5 }}>
            <label>重复窗口（秒）</label>
            <div className="stepper">
              <button onClick={() => setDedupWindow(Math.max(5, dedupWindow - 5))} type="button">−</button>
              <span className="stepper-val">{dedupWindow}</span>
              <button onClick={() => setDedupWindow(dedupWindow + 5)} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>压缩失败冻结</label>
            <input checked={shortContextAbortOnSummaryFailure} onChange={(event) => setShortContextAbortOnSummaryFailure(event.target.checked)} type="checkbox" />
          </div>
          <div className="form-row">
            <label>摘要服务商</label>
            <select value={shortContextSummaryProviderId} onChange={(event) => setShortContextSummaryProviderId(event.target.value)}>
              <option value="">跟随当前模型</option>
              {llmProviders.map((provider) => (
                <option key={provider.id} value={provider.id}>{provider.name || provider.id}</option>
              ))}
            </select>
          </div>
          <div className="form-row">
            <label>摘要模型</label>
            <input
              value={shortContextSummaryModel}
              onChange={(event) => setShortContextSummaryModel(event.target.value)}
              placeholder="留空使用服务商默认模型"
            />
          </div>
        </div>
        <div className="form-hint">控制 agent 正在运行时新消息的默认处理方式；单轮最长运行设为 0 时不自动超时。开启压缩失败冻结后，摘要模型失败不会丢弃旧历史，会暂停 agent 直到 /compact 成功。摘要服务商/模型为空时跟随当前主模型，辅助摘要失败会自动回退主模型一次。</div>
      </div>
      <div className="card" style={{ margin: "0 16px 12px" }}>
        <div className="card-header">意图分析</div>
        <div className="form-group">
          <div className="form-row">
            <label>分析方法</label>
            <select value={intentMode} onChange={(event) => setIntentMode(event.target.value)}>
              <option value="embedding">Embedding 分类器</option>
              <option value="llm">LLM 原生理解</option>
            </select>
          </div>
          <div className="form-row" style={{ opacity: intentMode === "embedding" ? 1 : 0.45, pointerEvents: intentMode === "embedding" ? "auto" : "none" }}>
            <label>Embedding 置信度</label>
            <div className="stepper">
              <button onClick={() => setIntentEmbeddingConfidence(Math.max(0, Number((intentEmbeddingConfidence - 0.05).toFixed(2))))} type="button">−</button>
              <span className="stepper-val">{intentEmbeddingConfidence.toFixed(2)}</span>
              <button onClick={() => setIntentEmbeddingConfidence(Math.min(1, Number((intentEmbeddingConfidence + 0.05).toFixed(2))))} type="button">+</button>
            </div>
          </div>
          <div style={{ opacity: intentMode === "llm" ? 1 : 0.45, pointerEvents: intentMode === "llm" ? "auto" : "none" }}>
            <div className="form-row">
              <label>绑定服务商</label>
              <select value={intentProviderId} onChange={(event) => setIntentProviderId(event.target.value)}>
                <option value="">跟随当前角色</option>
                {llmProviders.map((provider) => (
                  <option key={provider.id} value={provider.id}>{provider.name || provider.id}</option>
                ))}
              </select>
            </div>
            <div className="form-row">
              <label>绑定模型</label>
              <input value={intentModel} onChange={(event) => setIntentModel(event.target.value)} placeholder="留空使用服务商默认模型" />
            </div>
            <div className="form-row">
              <label>LLM 置信度</label>
              <div className="stepper">
                <button onClick={() => setIntentLlmConfidence(Math.max(0, Number((intentLlmConfidence - 0.05).toFixed(2))))} type="button">−</button>
                <span className="stepper-val">{intentLlmConfidence.toFixed(2)}</span>
                <button onClick={() => setIntentLlmConfidence(Math.min(1, Number((intentLlmConfidence + 0.05).toFixed(2))))} type="button">+</button>
              </div>
            </div>
            <div className="form-row">
              <label>LLM 超时（秒）</label>
              <div className="stepper">
                <button onClick={() => setIntentLlmTimeout(Math.max(3, intentLlmTimeout - 1))} type="button">−</button>
                <span className="stepper-val">{intentLlmTimeout}</span>
                <button onClick={() => setIntentLlmTimeout(Math.min(120, intentLlmTimeout + 1))} type="button">+</button>
              </div>
            </div>
            <div className="form-row">
              <label>LLM 最大 token</label>
              <div className="stepper">
                <button onClick={() => setIntentLlmMaxTokens(Math.max(64, intentLlmMaxTokens - 64))} type="button">−</button>
                <span className="stepper-val">{intentLlmMaxTokens}</span>
                <button onClick={() => setIntentLlmMaxTokens(Math.min(4096, intentLlmMaxTokens + 64))} type="button">+</button>
              </div>
            </div>
            <div className="form-row vertical">
              <label>LLM 意图提示词</label>
              <textarea value={intentLlmPrompt} onChange={(event) => setIntentLlmPrompt(event.target.value)} />
            </div>
          </div>
        </div>
        <div className="form-hint">生产路径只使用当前选择的方法；LLM 模式不可用时返回 unknown，不切回旧规则。</div>
      </div>
      <div className="card" style={{ margin: "0 16px 12px" }}>
        <div className="card-header">工具路由</div>
        <div className="form-group">
          <div className="form-row">
            <label>统一 LLM 路由</label>
            <input checked={routerEnabled} onChange={(event) => setRouterEnabled(event.target.checked)} type="checkbox" />
          </div>
          <div className="form-row">
            <label>最低置信度</label>
            <div className="stepper">
              <button onClick={() => setRouterConfidence(Math.max(0, Number((routerConfidence - 0.05).toFixed(2))))} type="button">−</button>
              <span className="stepper-val">{routerConfidence.toFixed(2)}</span>
              <button onClick={() => setRouterConfidence(Math.min(1, Number((routerConfidence + 0.05).toFixed(2))))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>分类超时（秒）</label>
            <div className="stepper">
              <button onClick={() => setRouterTimeout(Math.max(3, routerTimeout - 1))} type="button">−</button>
              <span className="stepper-val">{routerTimeout}</span>
              <button onClick={() => setRouterTimeout(Math.min(120, routerTimeout + 1))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>分类最大 token</label>
            <div className="stepper">
              <button onClick={() => setRouterMaxTokens(Math.max(64, routerMaxTokens - 64))} type="button">−</button>
              <span className="stepper-val">{routerMaxTokens}</span>
              <button onClick={() => setRouterMaxTokens(Math.min(4096, routerMaxTokens + 64))} type="button">+</button>
            </div>
          </div>
          <div className="form-row vertical">
            <label>分类提示词</label>
            <textarea value={routerPrompt} onChange={(event) => setRouterPrompt(event.target.value)} />
          </div>
          <div className="form-row">
            <label>工具协议修复</label>
            <select value={toolUseEnforcement} onChange={(event) => setToolUseEnforcement(event.target.value)}>
              <option value="auto">自动修复</option>
              <option value="off">关闭</option>
            </select>
          </div>
          <div className="form-row">
            <label>并行工具</label>
            <input checked={toolParallelEnabled} onChange={(event) => setToolParallelEnabled(event.target.checked)} type="checkbox" />
          </div>
          <div className="form-row">
            <label>send_message 工具</label>
            <input checked={sendMessageToolEnabled} onChange={(event) => setSendMessageToolEnabled(event.target.checked)} type="checkbox" />
          </div>
          <div className="form-hint">关闭时 Agent 不会看到或调用 Hermes send_message；扫码微信通道不依赖它。</div>
          <div className="form-row">
            <label>并行上限</label>
            <div className="stepper">
              <button onClick={() => setToolParallelLimit(Math.max(1, toolParallelLimit - 1))} type="button">−</button>
              <span className="stepper-val">{toolParallelLimit}</span>
              <button onClick={() => setToolParallelLimit(Math.min(64, toolParallelLimit + 1))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>工具审批模式</label>
            <select value={toolApprovalMode} onChange={(event) => setToolApprovalMode(event.target.value)}>
              <option value="risky">仅高风险</option>
              <option value="smart">智能审批</option>
              <option value="always">全部审批</option>
              <option value="never">默认允许（never）</option>
            </select>
          </div>
          <div className="form-hint">{approvalModeHint}</div>
          <div className="form-row">
            <label>凭据池策略</label>
            <select value={llmCredentialPoolStrategy} onChange={(event) => setLlmCredentialPoolStrategy(event.target.value)}>
              <option value="fill_first">优先第一个</option>
              <option value="round_robin">轮询</option>
              <option value="least_used">最少使用</option>
              <option value="random">随机</option>
            </select>
          </div>
          <div className="form-row vertical">
            <label>命令环境放行</label>
            <textarea
              rows={3}
              value={toolEnvPassthroughDraft}
              onChange={(event) => setToolEnvPassthroughDraft(event.target.value)}
              placeholder="每行一个变量名，例如 NOTION_TOKEN"
            />
            <div className="form-hint">terminal、process、execute_code 默认移除 API_KEY/TOKEN/SECRET 等敏感环境变量；这里列出的变量会保留。</div>
          </div>
          <div className="form-row vertical">
            <label>可信工具规则</label>
            <div className="inline-actions">
              <input
                value={trustedToolPatternDraft}
                onChange={(event) => setTrustedToolPatternDraft(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") {
                    event.preventDefault();
                    void addTrustedToolPattern();
                  }
                }}
                placeholder="server.tool、server.* 或 *"
              />
              <button className="btn-primary" onClick={() => void addTrustedToolPattern()} type="button">
                添加
              </button>
            </div>
            <div className="form-hint">命中规则的工具调用会跳过审批；`*` 表示信任全部外部工具。</div>
            {trustedToolPatterns.length > 0 ? (
              <div className="adapter-list" style={{ marginTop: 8 }}>
                {trustedToolPatterns.map((pattern) => (
                  <div className="adapter-row trace-row" key={pattern}>
                    <span className="status-badge enabled">trusted</span>
                    <div className="adapter-info">
                      <strong>{pattern}</strong>
                      <small>匹配后续工具调用时直接执行</small>
                    </div>
                    <button className="btn-secondary" onClick={() => void removeTrustedToolPattern(pattern)} type="button">
                      移除
                    </button>
                  </div>
                ))}
              </div>
            ) : null}
          </div>
          <div className="form-row vertical">
            <label>可信命令规则</label>
            <div className="inline-actions">
              <input
                value={trustedCommandPatternDraft}
                onChange={(event) => setTrustedCommandPatternDraft(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") {
                    event.preventDefault();
                    addTrustedCommandPattern();
                  }
                }}
                placeholder='例如 npm run build 或 git status*'
              />
              <button className="btn-primary" onClick={addTrustedCommandPattern} type="button">
                添加
              </button>
            </div>
            <div className="form-hint">匹配 terminal、process start/run、execute_code 的命令文本时跳过审批；支持 `*` 通配。hardline 风险不会被绕过。</div>
            {trustedCommandPatterns.length > 0 ? (
              <div className="adapter-list" style={{ marginTop: 8 }}>
                {trustedCommandPatterns.map((pattern) => (
                  <div className="adapter-row trace-row" key={pattern}>
                    <span className="status-badge enabled">trusted</span>
                    <div className="adapter-info">
                      <strong>{pattern}</strong>
                      <small>匹配后续命令调用时直接执行</small>
                    </div>
                    <button className="btn-secondary" onClick={() => removeTrustedCommandPattern(pattern)} type="button">
                      移除
                    </button>
                  </div>
                ))}
              </div>
            ) : null}
          </div>
          <div className="form-row">
            <label>模型重试次数</label>
            <div className="stepper">
              <button onClick={() => setLlmRetryCount(Math.max(0, llmRetryCount - 1))} type="button">−</button>
              <span className="stepper-val">{llmRetryCount}</span>
              <button onClick={() => setLlmRetryCount(Math.min(5, llmRetryCount + 1))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>模型退避（ms）</label>
            <div className="stepper">
              <button onClick={() => setLlmRetryBackoff(Math.max(100, llmRetryBackoff - 100))} type="button">−</button>
              <span className="stepper-val">{llmRetryBackoff}</span>
              <button onClick={() => setLlmRetryBackoff(Math.min(60000, llmRetryBackoff + 100))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>Responses reasoning 回放</label>
            <input checked={responsesReasoningReplayEnabled} onChange={(event) => setResponsesReasoningReplayEnabled(event.target.checked)} type="checkbox" />
          </div>
          <div className="form-hint">关闭后不会向 Responses 请求附带历史 encrypted reasoning item，也不会请求新的 encrypted reasoning 内容。</div>
          <div className="form-row">
            <label>工具重试次数</label>
            <div className="stepper">
              <button onClick={() => setToolRetryCount(Math.max(0, toolRetryCount - 1))} type="button">−</button>
              <span className="stepper-val">{toolRetryCount}</span>
              <button onClick={() => setToolRetryCount(Math.min(5, toolRetryCount + 1))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>重试退避（ms）</label>
            <div className="stepper">
              <button onClick={() => setToolRetryBackoff(Math.max(0, toolRetryBackoff - 100))} type="button">−</button>
              <span className="stepper-val">{toolRetryBackoff}</span>
              <button onClick={() => setToolRetryBackoff(Math.min(10000, toolRetryBackoff + 100))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>循环提醒</label>
            <input checked={guardWarningsEnabled} onChange={(event) => setGuardWarningsEnabled(event.target.checked)} type="checkbox" />
          </div>
          <div className="form-row">
            <label>循环硬停止</label>
            <input checked={guardHardStopEnabled} onChange={(event) => setGuardHardStopEnabled(event.target.checked)} type="checkbox" />
          </div>
          <div className="form-row">
            <label>同参失败提醒</label>
            <div className="stepper">
              <button onClick={() => setGuardExactWarnAfter(Math.max(1, guardExactWarnAfter - 1))} type="button">−</button>
              <span className="stepper-val">{guardExactWarnAfter}</span>
              <button onClick={() => setGuardExactWarnAfter(Math.min(12, guardExactWarnAfter + 1))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>同工具失败提醒</label>
            <div className="stepper">
              <button onClick={() => setGuardSameToolWarnAfter(Math.max(1, guardSameToolWarnAfter - 1))} type="button">−</button>
              <span className="stepper-val">{guardSameToolWarnAfter}</span>
              <button onClick={() => setGuardSameToolWarnAfter(Math.min(16, guardSameToolWarnAfter + 1))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>无进展提醒</label>
            <div className="stepper">
              <button onClick={() => setGuardNoProgressWarnAfter(Math.max(1, guardNoProgressWarnAfter - 1))} type="button">−</button>
              <span className="stepper-val">{guardNoProgressWarnAfter}</span>
              <button onClick={() => setGuardNoProgressWarnAfter(Math.min(12, guardNoProgressWarnAfter + 1))} type="button">+</button>
            </div>
          </div>
          <div className="form-row" style={{ opacity: guardHardStopEnabled ? 1 : 0.5 }}>
            <label>同参失败停止</label>
            <div className="stepper">
              <button onClick={() => setGuardExactLimit(Math.max(1, guardExactLimit - 1))} type="button">−</button>
              <span className="stepper-val">{guardExactLimit}</span>
              <button onClick={() => setGuardExactLimit(Math.min(12, guardExactLimit + 1))} type="button">+</button>
            </div>
          </div>
          <div className="form-row" style={{ opacity: guardHardStopEnabled ? 1 : 0.5 }}>
            <label>同工具失败停止</label>
            <div className="stepper">
              <button onClick={() => setGuardSameToolLimit(Math.max(1, guardSameToolLimit - 1))} type="button">−</button>
              <span className="stepper-val">{guardSameToolLimit}</span>
              <button onClick={() => setGuardSameToolLimit(Math.min(16, guardSameToolLimit + 1))} type="button">+</button>
            </div>
          </div>
          <div className="form-row" style={{ opacity: guardHardStopEnabled ? 1 : 0.5 }}>
            <label>无进展停止</label>
            <div className="stepper">
              <button onClick={() => setGuardNoProgressLimit(Math.max(1, guardNoProgressLimit - 1))} type="button">−</button>
              <span className="stepper-val">{guardNoProgressLimit}</span>
              <button onClick={() => setGuardNoProgressLimit(Math.min(12, guardNoProgressLimit + 1))} type="button">+</button>
            </div>
          </div>
        </div>
        <div className="form-hint">统一 LLM 路由已完全替换三级漏斗；模型与工具重试只处理超时、连接中断、429/5xx 等瞬时失败，参数或路径错误会交给失败重规划。循环保护默认只向 planner 注入提醒；开启硬停止后才会自动终止重复工具路径。</div>
      </div>
      <div className="card" style={{ margin: "0 16px 12px" }}>
        <div className="card-header">后台复盘</div>
        <div className="form-group">
          <div className="form-row">
            <label>技能建议复盘</label>
            <input checked={backgroundSkillReviewEnabled} onChange={(event) => setBackgroundSkillReviewEnabled(event.target.checked)} type="checkbox" />
          </div>
          <div className="form-row">
            <label>自动创建技能</label>
            <input checked={backgroundSkillReviewAutoCreateEnabled} onChange={(event) => setBackgroundSkillReviewAutoCreateEnabled(event.target.checked)} type="checkbox" />
          </div>
          <div className="form-row">
            <label>自动整理报告</label>
            <input checked={backgroundSkillCuratorEnabled} onChange={(event) => setBackgroundSkillCuratorEnabled(event.target.checked)} type="checkbox" />
          </div>
          <div className="form-row">
            <label>整理间隔（小时）</label>
            <div className="stepper">
              <button onClick={() => setBackgroundSkillCuratorIntervalHours(Math.max(1, backgroundSkillCuratorIntervalHours - 24))} type="button">−</button>
              <span className="stepper-val">{backgroundSkillCuratorIntervalHours}</span>
              <button onClick={() => setBackgroundSkillCuratorIntervalHours(Math.min(2160, backgroundSkillCuratorIntervalHours + 24))} type="button">+</button>
            </div>
          </div>
        </div>
        <div className="form-hint">完成一次 agent run 后异步检查技能库改进建议；自动整理报告按间隔生成 curator dry-run 报告，不会自动归档。记忆复盘请在通讯录的记忆管理中配置。</div>
      </div>
      <div className="card" style={{ margin: "0 16px 12px" }}>
        <div className="card-header">界面性能</div>
        <div className="form-group">
          <div className="form-row">
            <label>页面加载消息数</label>
            <div className="stepper">
              <button onClick={() => setUiLimit(Math.max(40, uiLimit - 20))} type="button">−</button>
              <span className="stepper-val">{uiLimit}</span>
              <button onClick={() => setUiLimit(Math.min(1000, uiLimit + 20))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>附件扫描消息数</label>
            <div className="stepper">
              <button onClick={() => setArtifactLimit(Math.max(20, artifactLimit - 10))} type="button">−</button>
              <span className="stepper-val">{artifactLimit}</span>
              <button onClick={() => setArtifactLimit(Math.min(uiLimit, artifactLimit + 10))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>单条消息预览字符</label>
            <div className="stepper">
              <button onClick={() => setPreviewChars(Math.max(2000, previewChars - 1000))} type="button">−</button>
              <span className="stepper-val">{previewChars}</span>
              <button onClick={() => setPreviewChars(Math.min(100000, previewChars + 1000))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>回复显现速度（字/秒）</label>
            <div className="stepper">
              <button onClick={() => setStreamChars(Math.max(8, streamChars - 4))} type="button">−</button>
              <span className="stepper-val">{streamChars}</span>
              <button onClick={() => setStreamChars(Math.min(160, streamChars + 4))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>Thinking 最短展示（ms）</label>
            <div className="stepper">
              <button onClick={() => setThinkingMs(Math.max(0, thinkingMs - 200))} type="button">−</button>
              <span className="stepper-val">{thinkingMs}</span>
              <button onClick={() => setThinkingMs(Math.min(8000, thinkingMs + 200))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>底部跟随容忍距离</label>
            <div className="stepper">
              <button onClick={() => setBottomThreshold(Math.max(24, bottomThreshold - 24))} type="button">−</button>
              <span className="stepper-val">{bottomThreshold}px</span>
              <button onClick={() => setBottomThreshold(Math.min(600, bottomThreshold + 24))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>桌宠气泡时长（秒）</label>
            <div className="stepper">
              <button onClick={() => setPetCloudDurationSeconds(Math.max(1, petCloudDurationSeconds - 1))} type="button">−</button>
              <span className="stepper-val">{petCloudDurationSeconds}</span>
              <button onClick={() => setPetCloudDurationSeconds(Math.min(120, petCloudDurationSeconds + 1))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>活跃刷新间隔（ms）</label>
            <div className="stepper">
              <button onClick={() => setActivePollMs(Math.max(300, activePollMs - 100))} type="button">−</button>
              <span className="stepper-val">{activePollMs}</span>
              <button onClick={() => setActivePollMs(Math.min(30000, activePollMs + 100))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>空闲刷新间隔（ms）</label>
            <div className="stepper">
              <button onClick={() => setIdlePollMs(Math.max(1000, idlePollMs - 500))} type="button">−</button>
              <span className="stepper-val">{idlePollMs}</span>
              <button onClick={() => setIdlePollMs(Math.min(120000, idlePollMs + 500))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>Skill 热更新</label>
            <input checked={skillHotReloadEnabled} onChange={(event) => setSkillHotReloadEnabled(event.target.checked)} type="checkbox" />
          </div>
          <div className="form-row">
            <label>Skill 扫描间隔（秒）</label>
            <div className="stepper">
              <button onClick={() => setSkillHotReloadInterval(Math.max(1, skillHotReloadInterval - 1))} type="button">−</button>
              <span className="stepper-val">{skillHotReloadInterval}</span>
              <button onClick={() => setSkillHotReloadInterval(Math.min(3600, skillHotReloadInterval + 1))} type="button">+</button>
            </div>
          </div>
        </div>
        <div className="form-hint">限制一次加载数量，并控制对话页的显现节奏、Thinking 停留、底部跟随、事件刷新和 Skill 热更新。</div>
      </div>
      <div className="card" style={{ margin: "0 16px 12px" }}>
        <div className="card-header">历史资源清理</div>
        <div className="form-group">
          <div className="form-row">
            <label>自动清理</label>
            <input checked={cleanupEnabled} onChange={(event) => setCleanupEnabled(event.target.checked)} type="checkbox" />
          </div>
          <div className="form-row">
            <label>保留天数</label>
            <div className="stepper">
              <button onClick={() => setRetentionDays(Math.max(1, retentionDays - 1))} type="button">−</button>
              <span className="stepper-val">{retentionDays}</span>
              <button onClick={() => setRetentionDays(Math.min(3650, retentionDays + 1))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>每会话最多消息</label>
            <div className="stepper">
              <button onClick={() => setStoredMessages(Math.max(50, storedMessages - 50))} type="button">−</button>
              <span className="stepper-val">{storedMessages}</span>
              <button onClick={() => setStoredMessages(Math.min(20000, storedMessages + 50))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>最多运行记录</label>
            <div className="stepper">
              <button onClick={() => setStoredRuns(Math.max(5, storedRuns - 5))} type="button">−</button>
              <span className="stepper-val">{storedRuns}</span>
              <button onClick={() => setStoredRuns(Math.min(5000, storedRuns + 5))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>最多工具 traces</label>
            <div className="stepper">
              <button onClick={() => setStoredTraces(Math.max(5, storedTraces - 10))} type="button">−</button>
              <span className="stepper-val">{storedTraces}</span>
              <button onClick={() => setStoredTraces(Math.min(10000, storedTraces + 10))} type="button">+</button>
            </div>
          </div>
        </div>
        <div className="form-hint">启动后自动清理过期会话、运行记录、工具 traces、state/workspace 快照，并裁剪超出上限的历史消息</div>
      </div>
    </div>
  );
}




