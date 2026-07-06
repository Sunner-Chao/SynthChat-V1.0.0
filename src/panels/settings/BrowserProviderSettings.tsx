import { useEffect, useState } from "react";
import { ChevronRight, Globe, Plus } from "lucide-react";
import type { BrowserProvider } from "../../lib/types";
import { BackBtn, SecretInput } from "./_shared";

function browserProviderLabel(type: string): string {
  if (type === "browser-use") return "Browser Use";
  if (type === "browserbase") return "Browserbase";
  return type;
}

function browserProviderDefaults(providerType: string): Omit<BrowserProvider, "id"> {
  if (providerType === "browserbase") {
    return {
      name: "Browserbase",
      providerType,
      baseUrl: "https://api.browserbase.com",
      apiKeyEnv: "BROWSERBASE_API_KEY",
      apiKey: null,
      projectId: "",
      recordSessions: false,
      enabled: false,
      timeoutSeconds: 30,
    };
  }
  return {
    name: "Browser Use",
    providerType: "browser-use",
    baseUrl: "https://api.browser-use.com/api/v3",
    apiKeyEnv: "BROWSER_USE_API_KEY",
    apiKey: null,
    projectId: "",
    recordSessions: false,
    enabled: false,
    timeoutSeconds: 30,
  };
}

export function BrowserProviderSettings({
  onBack,
  providers,
  saveProviders,
}: {
  onBack?: () => void;
  providers: BrowserProvider[];
  saveProviders: (providers: BrowserProvider[]) => Promise<void>;
}) {
  const [selectedId, setSelectedId] = useState("");
  const [draft, setDraft] = useState<BrowserProvider | null>(null);
  const [showTypeSheet, setShowTypeSheet] = useState(false);
  const selected = providers.find((p) => p.id === selectedId);

  useEffect(() => {
    if (selectedId && !selected) {
      setSelectedId("");
      setDraft(null);
    }
  }, [selected, selectedId]);

  const selectProvider = (id: string) => {
    const provider = providers.find((item) => item.id === id);
    setSelectedId(id);
    setDraft(provider ? { ...provider } : null);
  };

  const saveDraft = async () => {
    if (!draft) return;
    await saveProviders(providers.map((item) => (item.id === draft.id ? draft : item)));
    setSelectedId("");
    setDraft(null);
  };

  const toggleEnabled = (id: string) => {
    void saveProviders(providers.map((p) => p.id === id ? { ...p, enabled: !p.enabled } : p));
  };

  const add = async (providerType = "browser-use") => {
    const provider: BrowserProvider = {
      id: `browser-provider-${crypto.randomUUID()}`,
      ...browserProviderDefaults(providerType),
    };
    setDraft({ ...provider });
    setSelectedId(provider.id);
    await saveProviders([...providers, provider]);
    setShowTypeSheet(false);
  };

  const remove = async () => {
    if (!draft) return;
    await saveProviders(providers.filter((p) => p.id !== draft.id));
    setSelectedId("");
    setDraft(null);
  };

  const updateProviderType = (providerType: string) => {
    const defaults = browserProviderDefaults(providerType);
    setDraft((item) => item ? {
      ...item,
      providerType: defaults.providerType,
      name: item.name || defaults.name,
      baseUrl: item.baseUrl || defaults.baseUrl,
      apiKeyEnv: item.apiKeyEnv || defaults.apiKeyEnv,
      projectId: item.projectId ?? defaults.projectId,
    } : item);
  };

  const patchDraft = <K extends keyof BrowserProvider>(key: K, value: BrowserProvider[K]) =>
    setDraft((item) => item ? { ...item, [key]: value } : item);

  return (
    <div className="primary-panel embedded-panel">
      <div className="panel-title action-title">
        <BackBtn onBack={onBack} />
        <div className="panel-title-text"><span>Browser</span><strong>浏览器服务</strong></div>
        <button className="icon-only-btn" onClick={() => setShowTypeSheet(true)} title="添加浏览器服务" type="button">
          <Plus size={19} />
        </button>
      </div>

      {!selectedId ? (
        providers.length === 0 ? (
          <div className="empty-state compact">
            <div className="empty-icon-wrap"><Globe size={48} strokeWidth={1.5} /></div>
            <p>没有已配置的浏览器服务</p>
            <button className="btn-primary" onClick={() => setShowTypeSheet(true)} type="button">添加浏览器服务</button>
          </div>
        ) : (
          <div className="provider-list">
            {providers.map((provider) => (
              <div className="card provider-item-card" key={provider.id}>
                <button className="provider-card-btn" onClick={() => selectProvider(provider.id)} type="button">
                  <div className="provider-card-header">
                    <div className="provider-card-left">
                      <span className="provider-type-badge">{browserProviderLabel(provider.providerType)}</span>
                      <div className="provider-card-info">
                        <strong className="provider-card-name">{provider.name}</strong>
                        <span className="provider-card-model">{provider.baseUrl || "未配置地址"}</span>
                      </div>
                    </div>
                    <div className="provider-card-right">
                      <label className="switch-wrap" onClick={(e) => e.stopPropagation()}>
                        <input checked={provider.enabled} type="checkbox" onChange={() => toggleEnabled(provider.id)} />
                        <span className="switch-track" />
                      </label>
                      <ChevronRight size={16} className="provider-card-arrow" />
                    </div>
                  </div>
                </button>
              </div>
            ))}
          </div>
        )
      ) : draft ? (
        <div className="settings-form provider-card">
          <div className="panel-title action-title">
            <button className="icon-only-btn" onClick={() => { setSelectedId(""); setDraft(null); }} title="返回" type="button">
              <ChevronRight size={19} style={{ transform: "rotate(180deg)" }} />
            </button>
            <div className="panel-title-text"><span>Edit</span><strong>{draft.name}</strong></div>
            <button onClick={() => void saveDraft()} type="button">完成</button>
          </div>
          <label className="checkbox-row">
            <input checked={draft.enabled} type="checkbox" onChange={(e) => patchDraft("enabled", e.target.checked)} />
            启用当前浏览器服务
          </label>
          <label>名称<input value={draft.name} onChange={(e) => patchDraft("name", e.target.value)} /></label>
          <label>类型
            <select value={draft.providerType} onChange={(e) => updateProviderType(e.target.value)}>
              <option value="browser-use">Browser Use</option>
              <option value="browserbase">Browserbase</option>
            </select>
          </label>
          <label>Base URL
            <input value={draft.baseUrl}
              placeholder={draft.providerType === "browserbase" ? "https://api.browserbase.com" : "https://api.browser-use.com/api/v3"}
              onChange={(e) => patchDraft("baseUrl", e.target.value)} />
          </label>
          <div className="two-column">
            <label>API Key 环境变量
              <input value={draft.apiKeyEnv}
                placeholder={draft.providerType === "browserbase" ? "BROWSERBASE_API_KEY" : "BROWSER_USE_API_KEY"}
                onChange={(e) => patchDraft("apiKeyEnv", e.target.value)} />
            </label>
            <label>超时秒数
              <input min={1} type="number" value={draft.timeoutSeconds}
                onChange={(e) => patchDraft("timeoutSeconds", Number(e.target.value))} />
            </label>
          </div>
          <label>API Key（可选）
            <SecretInput value={draft.apiKey ?? ""} onChange={(v) => patchDraft("apiKey", v || null)} />
          </label>
          <label>Project ID
            <input value={draft.projectId ?? ""}
              placeholder={draft.providerType === "browserbase" ? "Browserbase project id" : "通常无需填写"}
              onChange={(e) => patchDraft("projectId", e.target.value)} />
          </label>
          <label className="checkbox-row">
            <input checked={Boolean(draft.recordSessions)} type="checkbox"
              onChange={(e) => patchDraft("recordSessions", e.target.checked)} />
            自动录制浏览器会话
          </label>
          <p className="form-hint">Agent 会优先使用静态页面快照、表单结构和请求线索；只有这些信息不足时才创建真实浏览器会话。</p>
          <button className="btn-danger-outline" onClick={() => void remove()} type="button">删除浏览器服务</button>
        </div>
      ) : null}

      {showTypeSheet ? (
        <div className="sheet-backdrop" onClick={() => setShowTypeSheet(false)}>
          <div className="action-sheet" onClick={(e) => e.stopPropagation()}>
            <div className="sheet-title">选择浏览器服务类型</div>
            <button className="sheet-item" type="button" onClick={() => void add("browser-use")}>Browser Use</button>
            <button className="sheet-item" type="button" onClick={() => void add("browserbase")}>Browserbase</button>
            <button className="sheet-cancel" type="button" onClick={() => setShowTypeSheet(false)}>取消</button>
          </div>
        </div>
      ) : null}
    </div>
  );
}
