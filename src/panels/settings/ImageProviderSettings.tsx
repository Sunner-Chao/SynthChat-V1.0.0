import { useEffect, useState } from "react";
import { ChevronRight, ImagePlus, Plus } from "lucide-react";
import { api } from "../../lib/api";
import { imageProviderTypeLabel } from "../../lib/formatters";
import type { ImageProvider, ModelCatalogEntry } from "../../lib/types";
import { BackBtn, SecretInput } from "./_shared";

const SYNTHAPI_IMAGE_GENERATIONS_URL = "https://synthapi.asia/v1/images/generations";

function imageProviderPresetLabel(id: string) {
  const labels: Record<string, string> = {
    synthapi_openai_image: "SynthAPI · OpenAI Image",
    synthapi_gemini_image: "SynthAPI · Gemini Image",
    openai_image: "OpenAI 官方 Image",
    gemini_image: "Google Gemini 原生",
    novelai: "NovelAI"
  };
  return labels[id] ?? imageProviderTypeLabel(id);
}

function imageProviderPresetDefaults(id: string): Omit<ImageProvider, "id" | "enabled"> {
  const defaults: Record<string, Omit<ImageProvider, "id" | "enabled">> = {
    synthapi_openai_image: {
      name: "SynthAPI · OpenAI Image",
      providerType: "openai_image",
      baseUrl: SYNTHAPI_IMAGE_GENERATIONS_URL,
      apiKeyEnv: "SYNTHAPI_IMAGE_API_KEY",
      apiKey: null,
      model: "gpt-image-2",
      timeoutSeconds: 300,
      useSystemProxy: true
    },
    synthapi_gemini_image: {
      name: "SynthAPI · Gemini Image",
      providerType: "gemini_image",
      baseUrl: SYNTHAPI_IMAGE_GENERATIONS_URL,
      apiKeyEnv: "SYNTHAPI_IMAGE_API_KEY",
      apiKey: null,
      model: "gemini-2.5-flash-image-preview",
      timeoutSeconds: 300,
      useSystemProxy: true
    },
    openai_image: {
      name: "OpenAI Image",
      providerType: "openai_image",
      baseUrl: "https://api.openai.com/v1/images/generations",
      apiKeyEnv: "OPENAI_API_KEY",
      apiKey: null,
      model: "gpt-image-1",
      timeoutSeconds: 300,
      useSystemProxy: true
    },
    gemini_image: {
      name: "Google Gemini Image",
      providerType: "gemini_image",
      baseUrl: "https://generativelanguage.googleapis.com/v1beta",
      apiKeyEnv: "GEMINI_API_KEY",
      apiKey: null,
      model: "gemini-2.5-flash-image-preview",
      timeoutSeconds: 300,
      useSystemProxy: true
    },
    novelai: {
      name: "NovelAI",
      providerType: "novelai",
      baseUrl: "",
      apiKeyEnv: "NOVELAI_API_KEY",
      apiKey: null,
      model: "",
      timeoutSeconds: 300,
      useSystemProxy: true
    }
  };
  return defaults[id] ?? defaults.synthapi_openai_image;
}

function imageProviderDefaultPresetForType(providerType: string) {
  if (providerType === "gemini_image") return "synthapi_gemini_image";
  if (providerType === "novelai") return "novelai";
  return "synthapi_openai_image";
}

function imageProviderBaseUrlOptions(providerType: string) {
  const options = [
    { value: SYNTHAPI_IMAGE_GENERATIONS_URL, label: SYNTHAPI_IMAGE_GENERATIONS_URL }
  ];
  if (providerType === "openai_image") {
    options.push({ value: "https://api.openai.com/v1/images/generations", label: "OpenAI /v1/images/generations" });
  } else if (providerType === "gemini_image") {
    options.push({ value: "https://generativelanguage.googleapis.com/v1beta", label: "Google Gemini native generateContent" });
  }
  return options;
}

function imageProviderModelOptions(providerType: string) {
  if (providerType === "gemini_image") {
    return [
      { value: "gemini-2.5-flash-image-preview", label: "Gemini 2.5 Flash Image (Nano Banana)" },
      { value: "gemini-2.5-flash-image", label: "Gemini 2.5 Flash Image" },
      { value: "imagen-4.0-generate-001", label: "Imagen 4" },
      { value: "imagen-4.0-ultra-generate-001", label: "Imagen 4 Ultra" }
    ];
  }
  if (providerType === "novelai") {
    return [
      { value: "nai-diffusion-4-full", label: "NAI Diffusion 4 Full" },
      { value: "nai-diffusion-3", label: "NAI Diffusion 3" }
    ];
  }
  return [
    { value: "gpt-image-2", label: "gpt-image-2" },
    { value: "gpt-image2", label: "gpt-image2" },
    { value: "gpt-image-1", label: "gpt-image-1" },
    { value: "dall-e-3", label: "dall-e-3" }
  ];
}

export function ImageProviderSettings({
  onBack,
  providers,
  saveProviders
}: {
  onBack?: () => void;
  providers: ImageProvider[];
  saveProviders: (providers: ImageProvider[]) => Promise<void>;
}) {
  const [selectedId, setSelectedId] = useState("");
  const [draft, setDraft] = useState<ImageProvider | null>(null);
  const [showTypeSheet, setShowTypeSheet] = useState(false);
  const [imageCatalogModels, setImageCatalogModels] = useState<ModelCatalogEntry[]>([]);
  const [imageCatalogLoading, setImageCatalogLoading] = useState(false);
  const [imageCatalogSource, setImageCatalogSource] = useState("");
  const [imageCatalogBaseUrl, setImageCatalogBaseUrl] = useState("");
  const [imageCatalogError, setImageCatalogError] = useState("");
  const selected = providers.find((provider) => provider.id === selectedId);
  const fetchImageCatalogModels = async (provider: ImageProvider) => {
    setImageCatalogLoading(true);
    try {
      const result = await api.detectImageProviderModels(provider);
      setImageCatalogModels(result.models ?? []);
      setImageCatalogSource(result.source ?? "");
      setImageCatalogBaseUrl(result.baseUrl ?? "");
      setImageCatalogError(result.error ?? "");
    } catch (error) {
      setImageCatalogModels([]);
      setImageCatalogSource("");
      setImageCatalogBaseUrl("");
      setImageCatalogError(String(error));
    } finally {
      setImageCatalogLoading(false);
    }
  };
  useEffect(() => {
    if (selectedId && !selected) {
      setSelectedId("");
      setDraft(null);
    }
  }, [selected, selectedId]);
  useEffect(() => {
    if (draft) {
      void fetchImageCatalogModels(draft);
    } else {
      setImageCatalogModels([]);
      setImageCatalogSource("");
      setImageCatalogBaseUrl("");
      setImageCatalogError("");
    }
  }, [draft?.id, draft?.providerType, draft?.baseUrl, draft?.apiKeyEnv, draft?.apiKey]);
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
  const toggleEnabled = (id: string) => {
    void saveProviders(providers.map((p) => p.id === id ? { ...p, enabled: !p.enabled } : p));
  };
  const add = (preset = "synthapi_openai_image") => {
    const defaults = imageProviderPresetDefaults(preset);
    const provider: ImageProvider = {
      id: `image-provider-${crypto.randomUUID()}`,
      enabled: false,
      ...defaults,
      name: defaults.name || imageProviderPresetLabel(preset)
    };
    setDraft({ ...provider });
    setSelectedId(provider.id);
    void saveProviders([
      ...providers,
      provider
    ]);
    setShowTypeSheet(false);
  };
  const applyImageTypeDefaults = (provider: ImageProvider, providerType: string): ImageProvider => {
    const defaults = imageProviderPresetDefaults(imageProviderDefaultPresetForType(providerType));
    return {
      ...provider,
      providerType,
      baseUrl: defaults.baseUrl,
      model: defaults.model,
      apiKeyEnv: defaults.apiKeyEnv,
      timeoutSeconds: defaults.timeoutSeconds,
      useSystemProxy: defaults.useSystemProxy
    };
  };
  const remove = () => {
    if (!draft) return;
    void saveProviders(providers.filter((provider) => provider.id !== draft.id));
    setSelectedId("");
    setDraft(null);
  };
  const imageTypeBadgeClass = (t: string) => {
    if (t === "openai_image") return "openai_image";
    if (t === "gemini_image") return "gemini_image";
    if (t === "novelai") return "novelai";
    return "openai_image";
  };
  const imageTypeLabel = (t: string) => {
    if (t === "openai_image") return "DALL·E";
    if (t === "gemini_image") return "Gemini";
    if (t === "novelai") return "Nai";
    return t;
  };
  const draftBaseUrlOptions = draft ? imageProviderBaseUrlOptions(draft.providerType) : [];
  const draftBaseUrlSelectValue = draft && draftBaseUrlOptions.some((option) => option.value === draft.baseUrl)
    ? draft.baseUrl
    : "__custom";
  const fallbackModelOptions = draft ? imageProviderModelOptions(draft.providerType) : [];
  const detectedModelOptions = imageCatalogModels.map((model) => ({
    value: model.id,
    label: model.name && model.name !== model.id ? `${model.name} (${model.id})` : model.id
  }));
  const draftModelOptions = detectedModelOptions.length > 0 ? detectedModelOptions : fallbackModelOptions;
  const draftModelSelectValue = draft && draftModelOptions.some((option) => option.value === draft.model)
    ? draft.model
    : "__custom";
  return (
    <div className="primary-panel embedded-panel">
      <div className="panel-title action-title">
        <BackBtn onBack={onBack} />
        <div className="panel-title-text"><span>Image</span><strong>生图服务商</strong></div>
        <button className="icon-only-btn" onClick={() => setShowTypeSheet(true)} title="添加生图服务商" type="button"><Plus size={19} /></button>
      </div>
      {!selectedId ? (
        providers.length === 0 ? (
          <div className="empty-state compact">
            <div className="empty-icon-wrap"><ImagePlus size={48} strokeWidth={1.5} /></div>
            <p>没有已配置的生图服务商</p>
            <button className="btn-primary" onClick={() => setShowTypeSheet(true)} type="button">添加生图服务商</button>
          </div>
        ) : (
          <div className="provider-list">
            {providers.map((provider) => (
              <div className="card provider-item-card" key={provider.id}>
                <button className="provider-card-btn" onClick={() => selectProvider(provider.id)} type="button">
                  <div className="provider-card-header">
                    <div className="provider-card-left">
                      <span className={`provider-type-badge image ${imageTypeBadgeClass(provider.providerType)}`}>
                        {imageTypeLabel(provider.providerType)}
                      </span>
                      <div className="provider-card-info">
                        <strong className="provider-card-name">{provider.name}</strong>
                        <span className="provider-card-model">{provider.model || "未配置模型"}</span>
                      </div>
                    </div>
                    <div className="provider-card-right">
                      <label className="switch-wrap" onClick={(e) => e.stopPropagation()}>
                        <input
                          type="checkbox"
                          checked={provider.enabled}
                          onChange={() => toggleEnabled(provider.id)}
                        />
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
            <div className="panel-title action-title"><button className="icon-only-btn" onClick={() => { setSelectedId(""); setDraft(null); }} title="返回" type="button"><ChevronRight size={19} style={{ transform: "rotate(180deg)" }} /></button><div className="panel-title-text"><span>Edit</span><strong>{draft.name}</strong></div><button onClick={() => void saveDraft()} type="button">完成</button></div>
            <label className="checkbox-row"><input checked={draft.enabled} onChange={(event) => setDraft((d) => d ? { ...d, enabled: event.target.checked } : d)} type="checkbox" />启用当前服务商</label>
            <label>名称<input value={draft.name} onChange={(event) => setDraft((d) => d ? { ...d, name: event.target.value } : d)} /></label>
            <label>类型<select value={draft.providerType} onChange={(event) => setDraft((d) => d ? applyImageTypeDefaults(d, event.target.value) : d)}><option value="openai_image">OpenAI Image</option><option value="gemini_image">Gemini Image</option><option value="novelai">NovelAI</option></select></label>
            <label>Base URL
              <select
                value={draftBaseUrlSelectValue}
                onChange={(event) => {
                  const value = event.target.value;
                  if (value !== "__custom") {
                    setDraft((d) => d ? { ...d, baseUrl: value } : d);
                  }
                }}
              >
                {draftBaseUrlOptions.map((option) => (
                  <option key={option.value} value={option.value}>{option.label}</option>
                ))}
                <option value="__custom">自定义</option>
              </select>
              {draftBaseUrlSelectValue === "__custom" ? (
                <input value={draft.baseUrl} onChange={(event) => setDraft((d) => d ? { ...d, baseUrl: event.target.value } : d)} />
              ) : null}
            </label>
            <div className="two-column">
              <label>模型
                <select
                  value={draftModelSelectValue}
                  onChange={(event) => {
                    const value = event.target.value;
                    if (value !== "__custom") {
                      setDraft((d) => d ? { ...d, model: value } : d);
                    }
                  }}
                >
                  {draftModelOptions.map((option) => (
                    <option key={option.value} value={option.value}>{option.label}</option>
                  ))}
                  <option value="__custom">自定义模型</option>
                </select>
                {draftModelSelectValue === "__custom" ? (
                  <input value={draft.model} onChange={(event) => setDraft((d) => d ? { ...d, model: event.target.value } : d)} />
                ) : null}
                <button
                  className="model-refresh-btn"
                  disabled={imageCatalogLoading}
                  onClick={() => void fetchImageCatalogModels(draft)}
                  title="刷新生图模型目录"
                  type="button"
                >
                  {imageCatalogLoading ? "..." : "↻"}
                </button>
                {imageCatalogSource || imageCatalogError ? (
                  <small className="form-hint">
                    {imageCatalogSource === "live" ? `已从生图模型端点/API Key 拉取模型${imageCatalogBaseUrl ? `（${imageCatalogBaseUrl}）` : ""}` : "使用内置生图模型目录"}
                    {imageCatalogError ? `：${imageCatalogError}` : ""}
                  </small>
                ) : null}
              </label>
              <label>API Key 环境变量<input value={draft.apiKeyEnv} onChange={(event) => setDraft((d) => d ? { ...d, apiKeyEnv: event.target.value } : d)} /></label>
            </div>
            <label>API Key（可选）<SecretInput value={draft.apiKey ?? ""} onChange={(value) => setDraft((d) => d ? { ...d, apiKey: value || null } : d)} /></label>
            <label>超时秒数<input min={1} type="number" value={draft.timeoutSeconds} onChange={(event) => setDraft((d) => d ? { ...d, timeoutSeconds: Number(event.target.value) } : d)} /></label>
            <label className="checkbox-row"><input checked={draft.useSystemProxy ?? true} onChange={(event) => setDraft((d) => d ? { ...d, useSystemProxy: event.target.checked } : d)} type="checkbox" />使用系统/环境代理</label>
            <button className="btn-danger-outline" onClick={remove} type="button">删除服务商</button>
          </div>
        ) : (
          <div className="empty-state compact">
            <div className="empty-icon-wrap"><ImagePlus size={48} strokeWidth={1.5} /></div>
            <p>没有生图服务商</p>
            <button className="btn-primary" onClick={() => setShowTypeSheet(true)} type="button">添加</button>
          </div>
        )}
      {showTypeSheet ? (
        <div className="sheet-backdrop" onClick={() => setShowTypeSheet(false)}>
          <div className="action-sheet" onClick={(event) => event.stopPropagation()}>
            <div className="sheet-title">选择生图服务商类型</div>
            {["synthapi_openai_image", "synthapi_gemini_image", "openai_image", "gemini_image", "novelai"].map((preset) => (
              <button className="sheet-item" key={preset} onClick={() => add(preset)} type="button">{imageProviderPresetLabel(preset)}</button>
            ))}
            <button className="sheet-cancel" onClick={() => setShowTypeSheet(false)} type="button">取消</button>
          </div>
        </div>
      ) : null}
    </div>
  );
}
