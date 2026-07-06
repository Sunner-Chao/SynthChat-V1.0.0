import { useEffect, useState } from "react";
import { ChevronRight, Plus, RefreshCw, Wand2 } from "lucide-react";
import { api } from "../../lib/api";
import { providerPresetLabel, providerPresetDefaults } from "../../lib/formatters";
import { useAppStore } from "../../lib/store";
import type {
  LlmProvider,
  ModelCatalogEntry,
  ModelCapabilities,
  TokenUsageStats
} from "../../lib/types";
import { BackBtn, SecretInput } from "./_shared";

type ModelCapabilityOverrideValue = "auto" | "on" | "off";

function formatTokenK(tokens: number) {
  if (tokens < 1000) return `${tokens}`;
  if (tokens >= 1_000_000) {
    const m = tokens / 1_000_000;
    return `${Number.isInteger(m) ? m.toFixed(0) : m.toFixed(1)}M`;
  }
  const value = tokens / 1000;
  return `${Number.isInteger(value) ? value.toFixed(0) : value.toFixed(1)}K`;
}

function providerPresetApiKeyEnv(id: string) {
  if (id === "synthapi") return "SYNTHAPI_API_KEY";
  return "SYNTHCHAT_LLM_API_KEY";
}

function normalizeProviderModels(value: LlmProvider["models"] | undefined): Record<string, Record<string, unknown>> {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return value as Record<string, Record<string, unknown>>;
}

function currentModelConfig(provider: LlmProvider | null): Record<string, unknown> {
  if (!provider || !provider.model?.trim()) return {};
  const models = normalizeProviderModels(provider.models);
  const config = models[provider.model.trim()];
  return config && typeof config === "object" && !Array.isArray(config) ? config : {};
}

function currentModelCapabilityOverrides(provider: LlmProvider | null): Record<string, unknown> {
  const config = currentModelConfig(provider);
  const capabilities = config.capabilities;
  return capabilities && typeof capabilities === "object" && !Array.isArray(capabilities)
    ? capabilities as Record<string, unknown>
    : {};
}

function capabilityOverrideValue(provider: LlmProvider | null, key: string): ModelCapabilityOverrideValue {
  const value = currentModelCapabilityOverrides(provider)[key];
  if (value === true) return "on";
  if (value === false) return "off";
  return "auto";
}

function setCapabilityOverride(
  provider: LlmProvider,
  key: string,
  next: ModelCapabilityOverrideValue
): LlmProvider {
  const model = provider.model.trim();
  if (!model) return provider;
  const models = { ...normalizeProviderModels(provider.models) };
  const currentConfig = currentModelConfig(provider);
  const nextConfig: Record<string, unknown> = { ...currentConfig };
  const currentCapabilities = currentModelCapabilityOverrides(provider);
  const nextCapabilities: Record<string, unknown> = { ...currentCapabilities };
  if (next === "auto") {
    delete nextCapabilities[key];
  } else {
    nextCapabilities[key] = next === "on";
  }
  if (Object.keys(nextCapabilities).length > 0) {
    nextConfig.capabilities = nextCapabilities;
  } else {
    delete nextConfig.capabilities;
  }
  if (Object.keys(nextConfig).length > 0) {
    models[model] = nextConfig;
  } else {
    delete models[model];
  }
  return { ...provider, models };
}

function providerThinkingEnabled(provider: LlmProvider | null | undefined): boolean {
  const models = normalizeProviderModels(provider?.models);
  return Boolean(models.__provider?.thinkingEnabled);
}

function setProviderThinkingEnabled(provider: LlmProvider, enabled: boolean): LlmProvider {
  const models = { ...normalizeProviderModels(provider.models) };
  const meta = {
    ...(models.__provider && typeof models.__provider === "object" && !Array.isArray(models.__provider)
      ? models.__provider
      : {})
  };
  if (enabled) {
    meta.thinkingEnabled = true;
  } else {
    delete meta.thinkingEnabled;
  }
  if (Object.keys(meta).length > 0) {
    models.__provider = meta;
  } else {
    delete models.__provider;
  }
  return { ...provider, models };
}

function formatCapabilitySource(source?: string) {
  if (!source) return "unknown";
  if (source === "configured") return "手动覆盖";
  if (source === "live") return "实时模型端点";
  if (source === "models.dev") return "models.dev";
  if (source === "curated") return "内置兼容规则";
  if (source === "heuristic") return "名称兜底推断";
  return source;
}

function capabilityFlag(capabilities: ModelCapabilities | null | undefined, snakeKey: keyof ModelCapabilities, camelKey: keyof ModelCapabilities): boolean {
  if (!capabilities) return false;
  return Boolean(capabilities[snakeKey] ?? capabilities[camelKey]);
}

function capabilityList(capabilities: ModelCapabilities | null | undefined, snakeKey: keyof ModelCapabilities, camelKey: keyof ModelCapabilities): string[] {
  if (!capabilities) return [];
  const value = capabilities[snakeKey] ?? capabilities[camelKey];
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === "string" && item.trim().length > 0) : [];
}

export function ProviderSettings({
  onBack,
  providers,
  saveProviders
}: {
  onBack?: () => void;
  providers: LlmProvider[];
  saveProviders: (providers: LlmProvider[]) => Promise<void>;
}) {
  const messages = useAppStore((state) => state.messages);
  const [selectedId, setSelectedId] = useState("");
  const [draft, setDraft] = useState<LlmProvider | null>(null);
  const [showTypeSheet, setShowTypeSheet] = useState(false);
  const selected = providers.find((provider) => provider.id === selectedId);
  const [tokenStats, setTokenStats] = useState<Record<string, TokenUsageStats>>({});
  const emptyTokenStats: TokenUsageStats = { promptTokens: 0, completionTokens: 0, totalTokens: 0, callCount: 0 };
  useEffect(() => {
    api.getTokenUsageStats().then((response) => setTokenStats(response.byProvider ?? {})).catch(() => {});
  }, [messages]);
  const resetTokenStats = async (providerId?: string) => {
    await api.resetTokenUsage(providerId).catch(() => {});
    const response = await api.getTokenUsageStats().catch(() => ({ byProvider: {} }));
    setTokenStats(response.byProvider ?? {});
  };
  const [catalogModels, setCatalogModels] = useState<ModelCatalogEntry[]>([]);
  const [catalogLoading, setCatalogLoading] = useState(false);
  const [catalogSource, setCatalogSource] = useState("");
  const [catalogBaseUrl, setCatalogBaseUrl] = useState("");
  const [catalogError, setCatalogError] = useState("");
  const [draftCapabilities, setDraftCapabilities] = useState<ModelCapabilities | null>(null);
  const [capabilityLoading, setCapabilityLoading] = useState(false);
  const [capabilityProbeLoading, setCapabilityProbeLoading] = useState(false);
  const [capabilityProbeMessage, setCapabilityProbeMessage] = useState("");
  const fetchCatalogModels = async (provider: LlmProvider) => {
    setCatalogLoading(true);
    try {
      const result = await api.detectProviderModels(provider);
      setCatalogModels(result.models ?? []);
      setCatalogSource(result.source ?? "");
      setCatalogBaseUrl(result.baseUrl ?? "");
      setCatalogError(result.error ?? "");
    } catch (error) {
      setCatalogModels([]);
      setCatalogSource("");
      setCatalogBaseUrl("");
      setCatalogError(String(error));
    } finally {
      setCatalogLoading(false);
    }
  };
  const fetchDraftCapabilities = async (provider: LlmProvider) => {
    if (!provider.model?.trim()) {
      setDraftCapabilities(null);
      return;
    }
    setCapabilityLoading(true);
    try {
      const result = await api.inferProviderModelCapabilities(provider);
      setDraftCapabilities(result);
    } catch {
      setDraftCapabilities(null);
    } finally {
      setCapabilityLoading(false);
    }
  };
  const probeDraftVisionCapability = async () => {
    if (!draft || !draft.model?.trim()) return;
    setCapabilityProbeLoading(true);
    setCapabilityProbeMessage("");
    try {
      const result = await api.probeProviderVisionCapability(draft);
      if (result.capabilities) setDraftCapabilities(result.capabilities);
      if (result.supported) {
        const nextDraft = setCapabilityOverride(draft, "supportsVision", "on");
        setDraft(nextDraft);
        setCapabilityProbeMessage("探测成功，已将原生识图覆盖为强制开启。");
      } else {
        setCapabilityProbeMessage(`探测未通过：${result.error || "模型未接受图片输入"}`);
      }
    } catch (error) {
      setCapabilityProbeMessage(`探测失败：${String(error)}`);
    } finally {
      setCapabilityProbeLoading(false);
    }
  };
  useEffect(() => {
    if (draft) void fetchCatalogModels(draft);
  }, [draft?.id, draft?.providerType, draft?.baseUrl, draft?.apiKeyEnv, draft?.apiKey]);
  useEffect(() => {
    if (draft) {
      void fetchDraftCapabilities(draft);
    } else {
      setDraftCapabilities(null);
    }
  }, [draft?.id, draft?.providerType, draft?.baseUrl, draft?.apiKeyEnv, draft?.apiKey, draft?.model, JSON.stringify(draft?.models ?? {})]);
  useEffect(() => {
    if (selectedId && !selected) {
      setSelectedId("");
      setDraft(null);
    }
  }, [selected, selectedId]);
  const selectProvider = (id: string) => {
    const provider = providers.find((p) => p.id === id);
    setSelectedId(id);
    setDraft(provider ? { ...provider } : null);
  };
  const saveDraft = async () => {
    if (!draft) return;
    await saveProviders(providers.map((item) => (item.id === draft.id ? draft : item)));
    setSelectedId("");
    setDraft(null);
  };
  const addProvider = async (preset = "custom") => {
    const defaults = providerPresetDefaults(preset);
    const provider = {
      id: `provider-${crypto.randomUUID()}`,
      name: preset === "custom" ? "自定义服务商" : providerPresetLabel(preset),
      providerType: defaults.providerType,
      preset,
      baseUrl: defaults.baseUrl,
      appendChatPath: defaults.appendChatPath,
      apiKeyEnv: providerPresetApiKeyEnv(preset),
      apiKey: null,
      model: "",
      enabled: false,
      timeoutSeconds: 60,
      promptCacheMode: "auto",
      promptCacheTtl: "5m",
      promptCacheLayout: "auto"
    };
    setDraft({ ...provider });
    setSelectedId(provider.id);
    await saveProviders([
      ...providers,
      provider
    ]);
    setShowTypeSheet(false);
  };
  const remove = async (provider: LlmProvider) => {
    await saveProviders(providers.filter((item) => item.id !== provider.id));
    setSelectedId("");
    setDraft(null);
  };
  return (
    <div className="primary-panel embedded-panel">
      <div className="panel-title action-title">
        <BackBtn onBack={onBack} />
        <div className="panel-title-text"><span>Providers</span><strong>对话服务商</strong></div>
        <button className="icon-only-btn" onClick={() => setShowTypeSheet(true)} title="添加对话服务商" type="button"><Plus size={19} /></button>
      </div>
      {!selectedId ? (
        providers.length === 0 ? (
          <div className="empty-state compact">
            <div className="empty-icon-wrap"><Wand2 size={48} strokeWidth={1.5} /></div>
            <p>没有已配置的对话服务商</p>
            <button className="btn-primary" onClick={() => setShowTypeSheet(true)} type="button">添加对话服务商</button>
          </div>
        ) : (
          <div className="provider-list">
            {providers.map((provider) => {
              const stats = tokenStats[provider.id] ?? emptyTokenStats;
              const typeLabel = provider.providerType === "anthropic" ? "Anthropic" :
                provider.providerType === "gemini" ? "Gemini" :
                provider.providerType === "openai_responses" ? "Responses" : "OpenAI";
              return (
                <div className="card provider-item-card" key={provider.id}>
                  <button className="provider-card-btn" onClick={() => selectProvider(provider.id)} type="button">
                    <div className="provider-card-header">
                      <div className="provider-card-left">
                        <div className="provider-badge-stack">
                          <span className={`provider-type-badge ${provider.providerType ?? "openai_compatible"}`}>
                            {typeLabel}
                          </span>
                        </div>
                        <div className="provider-card-info">
                          <strong className="provider-card-name">{provider.name}</strong>
                          <span className="provider-card-model">{provider.model || "未配置模型"}</span>
                        </div>
                      </div>
                      <div className="provider-card-right">
                        <span className={`provider-status-dot ${provider.enabled ? "enabled" : "disabled"}`} />
                        <ChevronRight size={16} className="provider-card-arrow" />
                      </div>
                    </div>
                    {stats.totalTokens > 0 ? (
                      <div className="provider-card-stats">
                        <div className="provider-stats-row">
                          <div className="provider-stat-chip">
                            <span className="provider-stat-icon tokens">T</span>
                            <span className="provider-stat-num">{formatTokenK(stats.totalTokens)}</span>
                          </div>
                          <div className="provider-stat-chip">
                            <span className="provider-stat-icon prompt">P</span>
                            <span className="provider-stat-num">{formatTokenK(stats.promptTokens)}</span>
                          </div>
                          <div className="provider-stat-chip">
                            <span className="provider-stat-icon completion">C</span>
                            <span className="provider-stat-num">{formatTokenK(stats.completionTokens)}</span>
                          </div>
                          <div className="provider-stat-chip">
                            <span className="provider-stat-icon calls">#</span>
                            <span className="provider-stat-num">{stats.callCount}</span>
                          </div>
                          <button
                            className="token-reset-btn"
                            onClick={(e) => { e.stopPropagation(); void resetTokenStats(provider.id); }}
                            title="重置计数"
                            type="button"
                          >
                            <RefreshCw size={11} />
                          </button>
                        </div>
                        <div className="provider-stats-bar">
                          <div
                            className="provider-stats-fill"
                            style={{ width: `${Math.min(100, Math.max(3, Math.round(Math.log10(stats.totalTokens + 1) * 18)))}%` }}
                          />
                        </div>
                      </div>
                    ) : (
                      <div className="provider-card-empty-stats">
                        <span>暂无 Token 消耗记录</span>
                      </div>
                    )}
                  </button>
                </div>
              );
            })}
          </div>
        )
      ) : draft ? (
          <div className="settings-form provider-card">
            <div className="panel-title action-title"><button className="icon-only-btn" onClick={() => { setSelectedId(""); setDraft(null); }} title="返回" type="button"><ChevronRight size={19} style={{ transform: "rotate(180deg)" }} /></button><div className="panel-title-text"><span>Edit</span><strong>{draft.name}</strong></div><button onClick={() => void saveDraft()} type="button">完成</button></div>
            <label className="checkbox-row"><input checked={draft.enabled} onChange={(event) => setDraft((d) => d ? { ...d, enabled: event.target.checked } : d)} type="checkbox" />启用当前服务商</label>
            <label className="checkbox-row"><input checked={providerThinkingEnabled(draft)} onChange={(event) => setDraft((d) => d ? setProviderThinkingEnabled(d, event.target.checked) : d)} type="checkbox" />启用思考卡片</label>
            <small className="form-hint">该服务商被通讯录/角色实际选中时，才会请求 reasoning/thinking，并把返回内容展示为折叠卡片。</small>
            <label>名称<input value={draft.name} onChange={(event) => setDraft((d) => d ? { ...d, name: event.target.value } : d)} /></label>
            <label>类型<select value={draft.providerType ?? "openai_compatible"} onChange={(event) => setDraft((d) => d ? { ...d, providerType: event.target.value } : d)}>
              <option value="openai_compatible">OpenAI Compatible</option>
              <option value="openai_responses">OpenAI Responses</option>
              <option value="anthropic">Anthropic</option>
              <option value="gemini">Google Gemini</option>
            </select></label>
            <label>Base URL<input value={draft.baseUrl} onChange={(event) => setDraft((d) => d ? { ...d, baseUrl: event.target.value } : d)} placeholder={(draft.providerType ?? "openai_compatible") === "gemini" ? "https://generativelanguage.googleapis.com/v1beta" : "https://api.example.com/v1"} /></label>
            {(draft.providerType ?? "openai_compatible") === "openai_compatible" ? (
              <label className="checkbox-row"><input checked={draft.appendChatPath ?? true} onChange={(event) => setDraft((d) => d ? { ...d, appendChatPath: event.target.checked } : d)} type="checkbox" />拼接 /chat/completions</label>
            ) : (draft.providerType ?? "openai_compatible") === "openai_responses" ? (
              <label className="checkbox-row"><input checked={draft.appendChatPath ?? true} onChange={(event) => setDraft((d) => d ? { ...d, appendChatPath: event.target.checked } : d)} type="checkbox" />拼接 /responses</label>
            ) : null}
            <label>模型
              <div className="model-select-row">
                {catalogModels.length > 0 ? (
                  <select
                    value={catalogModels.some((model) => model.id === draft.model) ? draft.model : ""}
                    onChange={(event) => {
                      const value = event.target.value;
                      if (value) setDraft((d) => d ? { ...d, model: value } : d);
                    }}
                  >
                    <option value="">{catalogLoading ? "加载中..." : "从目录选择模型"}</option>
                    {catalogModels.map((model) => (
                      <option key={model.id} value={model.id}>{model.name || model.id}{model.family ? ` (${model.family})` : ""}</option>
                    ))}
                  </select>
                ) : null}
                <input
                  value={draft.model}
                  onChange={(event) => setDraft((d) => d ? { ...d, model: event.target.value } : d)}
                  placeholder={catalogModels.length > 0 ? "或手动输入模型 ID" : "模型 ID"}
                />
                <button
                  className="model-refresh-btn"
                  disabled={catalogLoading}
                  onClick={() => void fetchCatalogModels(draft)}
                  title="刷新模型目录"
                  type="button"
                >
                  {catalogLoading ? "..." : "↻"}
                </button>
              </div>
              {catalogSource || catalogError ? (
                <small className="form-hint">
                  {catalogSource === "live" ? `已通过模型目录端点/API Key 检测模型${catalogBaseUrl ? `（${catalogBaseUrl}）` : ""}` : "使用内置模型目录"}
                  {catalogError ? `：${catalogError}` : ""}
                </small>
              ) : null}
            </label>
            <div className="two-column">
              <label>API Key 环境变量<input value={draft.apiKeyEnv} onChange={(event) => setDraft((d) => d ? { ...d, apiKeyEnv: event.target.value } : d)} /></label>
              <label>超时秒数<input min={1} type="number" value={draft.timeoutSeconds} onChange={(event) => setDraft((d) => d ? { ...d, timeoutSeconds: Number(event.target.value) } : d)} /></label>
            </div>
            <div className="two-column">
              <label>Prompt Cache<select value={draft.promptCacheMode ?? "auto"} onChange={(event) => setDraft((d) => d ? { ...d, promptCacheMode: event.target.value } : d)}>
                <option value="auto">自动</option>
                <option value="on">强制开启</option>
                <option value="off">关闭</option>
              </select></label>
              <label>Cache TTL<select value={draft.promptCacheTtl ?? "5m"} onChange={(event) => setDraft((d) => d ? { ...d, promptCacheTtl: event.target.value } : d)}>
                <option value="5m">5 分钟</option>
                <option value="1h">1 小时</option>
              </select></label>
            </div>
            <label>Cache Layout<select value={draft.promptCacheLayout ?? "auto"} onChange={(event) => setDraft((d) => d ? { ...d, promptCacheLayout: event.target.value } : d)}>
              <option value="auto">自动匹配服务商</option>
              <option value="native">Anthropic native content block</option>
              <option value="envelope">OpenAI/OpenRouter envelope</option>
            </select></label>
            <label>API Key（可选）<SecretInput value={draft.apiKey ?? ""} onChange={(value) => setDraft((d) => d ? { ...d, apiKey: value || null } : d)} /></label>
            {draft.model?.trim() ? (
              <div className="settings-subpanel">
                <div className="settings-subpanel-head">
                  <strong>模型能力覆盖</strong>
                  <div className="settings-subpanel-actions">
                    <small>{capabilityLoading ? "检测中..." : `当前来源：${formatCapabilitySource(draftCapabilities?.source)}`}</small>
                    <button
                      disabled={capabilityProbeLoading || !draft.model?.trim()}
                      onClick={() => void probeDraftVisionCapability()}
                      type="button"
                    >
                      {capabilityProbeLoading ? "探测中..." : "探测识图"}
                    </button>
                  </div>
                </div>
                <div className="two-column">
                  <label>原生识图
                    <select
                      value={capabilityOverrideValue(draft, "supportsVision")}
                      onChange={(event) => setDraft((d) => d ? setCapabilityOverride(d, "supportsVision", event.target.value as ModelCapabilityOverrideValue) : d)}
                    >
                      <option value="auto">自动判断</option>
                      <option value="on">强制开启</option>
                      <option value="off">强制关闭</option>
                    </select>
                  </label>
                  <label>推理模式
                    <select
                      value={capabilityOverrideValue(draft, "supportsReasoning")}
                      onChange={(event) => setDraft((d) => d ? setCapabilityOverride(d, "supportsReasoning", event.target.value as ModelCapabilityOverrideValue) : d)}
                    >
                      <option value="auto">自动判断</option>
                      <option value="on">强制开启</option>
                      <option value="off">强制关闭</option>
                    </select>
                  </label>
                </div>
                <div className="two-column">
                  <label>工具调用
                    <select
                      value={capabilityOverrideValue(draft, "supportsTools")}
                      onChange={(event) => setDraft((d) => d ? setCapabilityOverride(d, "supportsTools", event.target.value as ModelCapabilityOverrideValue) : d)}
                    >
                      <option value="auto">自动判断</option>
                      <option value="on">强制开启</option>
                      <option value="off">强制关闭</option>
                    </select>
                  </label>
                  <label>结构化输出
                    <select
                      value={capabilityOverrideValue(draft, "supportsStructuredOutput")}
                      onChange={(event) => setDraft((d) => d ? setCapabilityOverride(d, "supportsStructuredOutput", event.target.value as ModelCapabilityOverrideValue) : d)}
                    >
                      <option value="auto">自动判断</option>
                      <option value="on">强制开启</option>
                      <option value="off">强制关闭</option>
                    </select>
                  </label>
                </div>
                {draftCapabilities ? (
                  <small className="form-hint">
                    当前生效：识图 {capabilityFlag(draftCapabilities, "supports_vision", "supportsVision") ? "开启" : "关闭"} ·
                    工具 {capabilityFlag(draftCapabilities, "supports_tools", "supportsTools") ? "开启" : "关闭"} ·
                    推理 {capabilityFlag(draftCapabilities, "supports_reasoning", "supportsReasoning") ? "开启" : "关闭"} ·
                    结构化 {capabilityFlag(draftCapabilities, "supports_structured_output", "supportsStructuredOutput") ? "开启" : "关闭"}
                    {capabilityList(draftCapabilities, "input_modalities", "inputModalities").length ? ` · 输入 ${capabilityList(draftCapabilities, "input_modalities", "inputModalities").join("/")}` : ""}
                  </small>
                ) : (
                  <small className="form-hint">选择模型后可对当前模型单独指定能力；留在"自动判断"时将使用后端发现结果。</small>
                )}
                {capabilityProbeMessage ? <small className="form-hint">{capabilityProbeMessage}</small> : null}
              </div>
            ) : null}
            <button className="btn-danger-outline" onClick={() => void remove(draft)} type="button">删除服务商</button>
          </div>
        ) : (
          <div className="empty-state compact">
            <div className="empty-icon-wrap"><Wand2 size={48} strokeWidth={1.5} /></div>
            <p>没有对话服务商</p>
            <button className="btn-primary" onClick={() => setShowTypeSheet(true)} type="button">添加</button>
          </div>
        )}
      {showTypeSheet ? (
        <div className="sheet-backdrop" onClick={() => setShowTypeSheet(false)}>
          <div className="action-sheet" onClick={(event) => event.stopPropagation()}>
            <div className="sheet-title">选择对话服务商类型</div>
            {["synthapi", "openai", "openaiResponses", "anthropic", "google", "deepseek", "siliconflow", "custom"].map((preset) => (
              <button className="sheet-item" key={preset} onClick={() => void addProvider(preset)} type="button">{providerPresetLabel(preset)}</button>
            ))}
            <button className="sheet-cancel" onClick={() => setShowTypeSheet(false)} type="button">取消</button>
          </div>
        </div>
      ) : null}
    </div>
  );
}
