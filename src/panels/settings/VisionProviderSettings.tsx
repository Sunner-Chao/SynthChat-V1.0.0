import { useEffect, useState } from "react";
import { Bot, ChevronRight, Plus } from "lucide-react";
import type { VisionProvider } from "../../lib/types";
import { BackBtn, SecretInput } from "./_shared";

function visionTypeBadgeClass(t: string): string {
  if (t === "ollama") return "ollama";
  if (t === "openai_compatible") return "openai_vision";
  return "ollama";
}

function visionTypeLabel(t: string): string {
  if (t === "ollama") return "Ollama";
  if (t === "openai_compatible") return "OpenAI";
  return t;
}

export function VisionProviderSettings({
  onBack,
  providers,
  saveProviders,
}: {
  onBack?: () => void;
  providers: VisionProvider[];
  saveProviders: (providers: VisionProvider[]) => Promise<void>;
}) {
  const [selectedId, setSelectedId] = useState("");
  const [draft, setDraft] = useState<VisionProvider | null>(null);
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

  const add = async (providerType = "ollama") => {
    const provider: VisionProvider = {
      id: `vision-provider-${crypto.randomUUID()}`,
      name: providerType === "ollama" ? "Ollama Qwen2.5-VL 本地识图" : "OpenAI Compatible 识图",
      providerType,
      baseUrl: providerType === "ollama" ? "http://127.0.0.1:11434" : "",
      apiKeyEnv: "SYNTHCHAT_VISION_API_KEY",
      apiKey: null,
      model: providerType === "ollama" ? "qwen2.5vl:7b" : "",
      enabled: false,
      timeoutSeconds: 60,
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

  const patchDraft = <K extends keyof VisionProvider>(key: K, value: VisionProvider[K]) =>
    setDraft((item) => item ? { ...item, [key]: value } : item);

  return (
    <div className="primary-panel embedded-panel">
      <div className="panel-title action-title">
        <BackBtn onBack={onBack} />
        <div className="panel-title-text"><span>Vision</span><strong>识图服务</strong></div>
        <button className="icon-only-btn" onClick={() => setShowTypeSheet(true)} title="添加识图服务" type="button">
          <Plus size={19} />
        </button>
      </div>

      {!selectedId ? (
        providers.length === 0 ? (
          <div className="empty-state compact">
            <div className="empty-icon-wrap"><Bot size={48} strokeWidth={1.5} /></div>
            <p>没有已配置的识图服务</p>
            <button className="btn-primary" onClick={() => setShowTypeSheet(true)} type="button">添加识图服务</button>
          </div>
        ) : (
          <div className="provider-list">
            {providers.map((provider) => (
              <div className="card provider-item-card" key={provider.id}>
                <button className="provider-card-btn" onClick={() => selectProvider(provider.id)} type="button">
                  <div className="provider-card-header">
                    <div className="provider-card-left">
                      <span className={`provider-type-badge vision ${visionTypeBadgeClass(provider.providerType)}`}>
                        {visionTypeLabel(provider.providerType)}
                      </span>
                      <div className="provider-card-info">
                        <strong className="provider-card-name">{provider.name}</strong>
                        <span className="provider-card-model">{provider.model || "未配置模型"}</span>
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
            启用当前识图服务
          </label>
          <label>名称<input value={draft.name} onChange={(e) => patchDraft("name", e.target.value)} /></label>
          <label>类型
            <select value={draft.providerType} onChange={(e) => patchDraft("providerType", e.target.value)}>
              <option value="ollama">Ollama</option>
              <option value="openai_compatible">OpenAI Compatible</option>
            </select>
          </label>
          <label>Base URL
            <input value={draft.baseUrl}
              placeholder={draft.providerType === "ollama" ? "http://127.0.0.1:11434" : "https://api.example.com/v1"}
              onChange={(e) => patchDraft("baseUrl", e.target.value)} />
          </label>
          <div className="two-column">
            <label>模型
              <input value={draft.model}
                placeholder={draft.providerType === "ollama" ? "qwen2.5vl:7b" : "gpt-4o-mini"}
                onChange={(e) => patchDraft("model", e.target.value)} />
            </label>
            <label>超时秒数
              <input min={1} type="number" value={draft.timeoutSeconds}
                onChange={(e) => patchDraft("timeoutSeconds", Number(e.target.value))} />
            </label>
          </div>
          <label>API Key 环境变量
            <input value={draft.apiKeyEnv} onChange={(e) => patchDraft("apiKeyEnv", e.target.value)} />
          </label>
          <label>API Key（可选）
            <SecretInput value={draft.apiKey ?? ""} onChange={(v) => patchDraft("apiKey", v || null)} />
          </label>
          <button className="btn-danger-outline" onClick={() => void remove()} type="button">删除识图服务</button>
        </div>
      ) : null}

      {showTypeSheet ? (
        <div className="sheet-backdrop" onClick={() => setShowTypeSheet(false)}>
          <div className="action-sheet" onClick={(e) => e.stopPropagation()}>
            <div className="sheet-title">选择识图服务类型</div>
            <button className="sheet-item" type="button" onClick={() => void add("ollama")}>Ollama（本地模型）</button>
            <button className="sheet-item" type="button" onClick={() => void add("openai_compatible")}>OpenAI Compatible（云端API）</button>
            <button className="sheet-cancel" type="button" onClick={() => setShowTypeSheet(false)}>取消</button>
          </div>
        </div>
      ) : null}
    </div>
  );
}
