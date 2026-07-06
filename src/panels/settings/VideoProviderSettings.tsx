import { useEffect, useState } from "react";
import { ChevronRight, Plus, Video } from "lucide-react";
import type { VideoProvider } from "../../lib/types";
import { SecretInput } from "./_shared";

export function VideoProviderSettings({
  onBack,
  providers,
  saveProviders,
}: {
  onBack?: () => void;
  providers: VideoProvider[];
  saveProviders: (providers: VideoProvider[]) => Promise<void>;
}) {
  const [selectedId, setSelectedId] = useState("");
  const [draft, setDraft] = useState<VideoProvider | null>(null);
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
    const provider: VideoProvider = {
      id: `video-provider-${crypto.randomUUID()}`,
      name: "视频生成服务",
      providerType: "http-json",
      baseUrl: "",
      apiKeyEnv: "SYNTHCHAT_VIDEO_API_KEY",
      apiKey: null,
      model: "",
      enabled: false,
      timeoutSeconds: 120,
      submitPath: "/generate",
      statusPath: "",
      idPath: "id",
      statusField: "status",
      resultPath: "video.url",
      completedStatuses: ["completed", "succeeded", "success", "ready", "done"],
      failedStatuses: ["failed", "error", "canceled", "cancelled"],
      pollIntervalSeconds: 3,
      maxPollSeconds: 300,
      downloadResult: false,
    };
    const nextProviders = [...providers, provider];
    setDraft({ ...provider });
    setSelectedId(provider.id);
    await saveProviders(nextProviders);
  };

  const remove = async () => {
    if (!draft) return;
    await saveProviders(providers.filter((p) => p.id !== draft.id));
    setSelectedId("");
    setDraft(null);
  };

  const setStatusList = (key: "completedStatuses" | "failedStatuses", value: string) => {
    setDraft((item) => item ? { ...item, [key]: value.split(",").map((p) => p.trim()).filter(Boolean) } : item);
  };

  const patch = <K extends keyof VideoProvider>(key: K, value: VideoProvider[K]) =>
    setDraft((item) => item ? { ...item, [key]: value } : item);

  return (
    <div className="primary-panel embedded-panel">
      <div className="panel-title action-title">
        {onBack && (
          <button className="icon-only-btn" onClick={onBack} title="返回" type="button">
            <ChevronRight size={19} style={{ transform: "rotate(180deg)" }} />
          </button>
        )}
        <div className="panel-title-text"><span>Video</span><strong>视频生成服务商</strong></div>
        <button className="icon-only-btn" onClick={() => void add()} title="添加视频生成服务商" type="button">
          <Plus size={19} />
        </button>
      </div>

      {!selectedId ? (
        providers.length === 0 ? (
          <div className="empty-state compact">
            <div className="empty-icon-wrap"><Video size={48} strokeWidth={1.5} /></div>
            <p>没有已配置的视频生成服务商</p>
            <button className="btn-primary" onClick={() => void add()} type="button">添加视频生成服务商</button>
          </div>
        ) : (
          <div className="provider-list">
            {providers.map((provider) => (
              <div className="card provider-item-card" key={provider.id}>
                <button className="provider-card-btn" onClick={() => selectProvider(provider.id)} type="button">
                  <div className="provider-card-header">
                    <div className="provider-card-left">
                      <span className="provider-type-badge image openai_image">Video</span>
                      <div className="provider-card-info">
                        <strong className="provider-card-name">{provider.name}</strong>
                        <span className="provider-card-model">{provider.model || provider.baseUrl || "未配置"}</span>
                      </div>
                    </div>
                    <div className="provider-card-right">
                      <label className="switch-wrap" onClick={(e) => e.stopPropagation()}>
                        <input checked={provider.enabled} onChange={() => toggleEnabled(provider.id)} type="checkbox" />
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
            <input checked={draft.enabled} type="checkbox" onChange={(e) => patch("enabled", e.target.checked)} />
            启用当前服务商
          </label>
          <label>名称<input value={draft.name} onChange={(e) => patch("name", e.target.value)} /></label>
          <div className="two-column">
            <label>类型<input value={draft.providerType} onChange={(e) => patch("providerType", e.target.value)} /></label>
            <label>模型<input value={draft.model} onChange={(e) => patch("model", e.target.value)} /></label>
          </div>
          <label>Base URL<input value={draft.baseUrl} onChange={(e) => patch("baseUrl", e.target.value)} /></label>
          <div className="two-column">
            <label>提交路径<input value={draft.submitPath} onChange={(e) => patch("submitPath", e.target.value)} /></label>
            <label>状态路径<input value={draft.statusPath} onChange={(e) => patch("statusPath", e.target.value)} /></label>
          </div>
          <div className="two-column">
            <label>任务 ID 路径<input value={draft.idPath} onChange={(e) => patch("idPath", e.target.value)} /></label>
            <label>结果 URL 路径<input value={draft.resultPath} onChange={(e) => patch("resultPath", e.target.value)} /></label>
          </div>
          <div className="two-column">
            <label>状态字段<input value={draft.statusField} onChange={(e) => patch("statusField", e.target.value)} /></label>
            <label>API Key 环境变量<input value={draft.apiKeyEnv} onChange={(e) => patch("apiKeyEnv", e.target.value)} /></label>
          </div>
          <label>API Key（可选）
            <SecretInput value={draft.apiKey ?? ""} onChange={(v) => patch("apiKey", v || null)} />
          </label>
          <label>完成状态<input value={draft.completedStatuses.join(", ")} onChange={(e) => setStatusList("completedStatuses", e.target.value)} /></label>
          <label>失败状态<input value={draft.failedStatuses.join(", ")} onChange={(e) => setStatusList("failedStatuses", e.target.value)} /></label>
          <div className="two-column">
            <label>请求超时秒数<input min={1} type="number" value={draft.timeoutSeconds} onChange={(e) => patch("timeoutSeconds", Number(e.target.value))} /></label>
            <label>最大轮询秒数<input min={1} type="number" value={draft.maxPollSeconds} onChange={(e) => patch("maxPollSeconds", Number(e.target.value))} /></label>
          </div>
          <label>轮询间隔秒数<input min={1} type="number" value={draft.pollIntervalSeconds} onChange={(e) => patch("pollIntervalSeconds", Number(e.target.value))} /></label>
          <label className="checkbox-row">
            <input checked={draft.downloadResult} type="checkbox" onChange={(e) => patch("downloadResult", e.target.checked)} />
            生成后下载视频到本地 artifact
          </label>
          <button className="btn-danger-outline" onClick={() => void remove()} type="button">删除服务商</button>
        </div>
      ) : null}
    </div>
  );
}
