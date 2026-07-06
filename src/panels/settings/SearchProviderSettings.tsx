import { useEffect, useState } from "react";
import { ChevronRight, Globe, Plus } from "lucide-react";
import type { SearchProvider } from "../../lib/types";
import { BackBtn, SecretInput } from "./_shared";

function defaultSearchApiKeyEnv(providerType: string): string {
  if (providerType === "firecrawl") return "FIRECRAWL_API_KEY";
  if (providerType === "tavily") return "TAVILY_API_KEY";
  if (providerType === "exa") return "EXA_API_KEY";
  if (providerType === "brave-free") return "BRAVE_SEARCH_API_KEY";
  if (providerType === "parallel") return "PARALLEL_API_KEY";
  return "";
}

function defaultSearchBaseUrl(providerType: string): string {
  if (providerType === "searxng") return "http://127.0.0.1:8080";
  if (providerType === "firecrawl") return "https://api.firecrawl.dev";
  if (providerType === "tavily") return "https://api.tavily.com";
  if (providerType === "exa") return "https://api.exa.ai";
  if (providerType === "brave-free") return "https://api.search.brave.com/res/v1/web/search";
  return "";
}

function searchTypeBadgeClass(t: string): string {
  if (t === "searxng") return "searxng";
  if (t === "ddgs" || t === "duckduckgo_html") return "duckduckgo";
  if (t === "brave-free") return "brave";
  return "searxng";
}

function searchTypeLabel(t: string): string {
  if (t === "searxng") return "SearXNG";
  if (t === "firecrawl") return "Firecrawl";
  if (t === "tavily") return "Tavily";
  if (t === "exa") return "Exa";
  if (t === "brave-free") return "Brave";
  if (t === "parallel") return "Parallel";
  if (t === "ddgs" || t === "duckduckgo_html") return "DDGS";
  return t;
}

export function SearchProviderSettings({
  onBack,
  providers,
  saveProviders,
}: {
  onBack?: () => void;
  providers: SearchProvider[];
  saveProviders: (providers: SearchProvider[]) => Promise<void>;
}) {
  const [selectedId, setSelectedId] = useState("");
  const [draft, setDraft] = useState<SearchProvider | null>(null);
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

  const add = async () => {
    const provider: SearchProvider = {
      id: `search-provider-${crypto.randomUUID()}`,
      name: "SearXNG 搜索",
      providerType: "searxng",
      baseUrl: "http://127.0.0.1:8080",
      apiKeyEnv: "",
      apiKey: null,
      enabled: false,
      timeoutSeconds: 10,
    };
    setDraft({ ...provider });
    setSelectedId(provider.id);
    await saveProviders([...providers, provider]);
  };

  const remove = async () => {
    if (!draft) return;
    await saveProviders(providers.filter((p) => p.id !== draft.id));
    setSelectedId("");
    setDraft(null);
  };

  const updateProviderType = (providerType: string) => {
    setDraft((item) => item ? {
      ...item,
      providerType,
      baseUrl: item.baseUrl || defaultSearchBaseUrl(providerType),
      apiKeyEnv: item.apiKeyEnv || defaultSearchApiKeyEnv(providerType),
    } : item);
  };

  const patchDraft = <K extends keyof SearchProvider>(key: K, value: SearchProvider[K]) =>
    setDraft((item) => item ? { ...item, [key]: value } : item);

  return (
    <div className="primary-panel embedded-panel">
      <div className="panel-title action-title">
        <BackBtn onBack={onBack} />
        <div className="panel-title-text"><span>Search</span><strong>搜索服务</strong></div>
        <button className="icon-only-btn" onClick={() => void add()} title="添加搜索服务" type="button">
          <Plus size={19} />
        </button>
      </div>

      {!selectedId ? (
        providers.length === 0 ? (
          <div className="empty-state compact">
            <div className="empty-icon-wrap"><Globe size={48} strokeWidth={1.5} /></div>
            <p>没有已配置的搜索服务</p>
            <button className="btn-primary" onClick={() => void add()} type="button">添加搜索服务</button>
          </div>
        ) : (
          <div className="provider-list">
            {providers.map((provider) => (
              <div className="card provider-item-card" key={provider.id}>
                <button className="provider-card-btn" onClick={() => selectProvider(provider.id)} type="button">
                  <div className="provider-card-header">
                    <div className="provider-card-left">
                      <span className={`provider-type-badge search ${searchTypeBadgeClass(provider.providerType)}`}>
                        {searchTypeLabel(provider.providerType)}
                      </span>
                      <div className="provider-card-info">
                        <strong className="provider-card-name">{provider.name}</strong>
                        <span className="provider-card-model">{provider.baseUrl || "未配置地址"}</span>
                      </div>
                    </div>
                    <div className="provider-card-right">
                      <label className="switch-wrap" onClick={(e) => e.stopPropagation()}>
                        <input checked={provider.enabled} type="checkbox"
                          onChange={() => toggleEnabled(provider.id)} />
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
            启用当前搜索服务
          </label>
          <label>名称<input value={draft.name} onChange={(e) => patchDraft("name", e.target.value)} /></label>
          <label>类型
            <select value={draft.providerType} onChange={(e) => updateProviderType(e.target.value)}>
              <option value="searxng">SearXNG</option>
              <option value="firecrawl">Firecrawl</option>
              <option value="tavily">Tavily</option>
              <option value="exa">Exa</option>
              <option value="brave-free">Brave Search</option>
              <option value="parallel">Parallel</option>
              <option value="ddgs">DDGS</option>
            </select>
          </label>
          <label>Base URL
            <input value={draft.baseUrl} placeholder="http://127.0.0.1:8080"
              onChange={(e) => patchDraft("baseUrl", e.target.value)} />
          </label>
          <label>API Key 环境变量
            <input value={draft.apiKeyEnv || ""}
              placeholder={defaultSearchApiKeyEnv(draft.providerType)}
              onChange={(e) => patchDraft("apiKeyEnv", e.target.value)} />
          </label>
          <label>API Key（可选）
            <SecretInput value={draft.apiKey ?? ""} onChange={(v) => patchDraft("apiKey", v || null)} />
          </label>
          <label>超时秒数
            <input min={1} type="number" value={draft.timeoutSeconds}
              onChange={(e) => patchDraft("timeoutSeconds", Number(e.target.value))} />
          </label>
          <button className="btn-danger-outline" onClick={() => void remove()} type="button">删除搜索服务</button>
        </div>
      ) : null}
    </div>
  );
}
