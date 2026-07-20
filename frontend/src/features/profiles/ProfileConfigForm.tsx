import { RefreshCw, Save, X } from "lucide-react";
import { useEffect, useMemo, useState, type FormEvent } from "react";
import {
  ProfileApiError,
  type ProfileConfig,
  type ProfileConfigPatch,
  type ProfileMetadata,
  type ProfilePatch,
  type Provider,
  type Versioned,
} from "../../api/profiles";

function formErrorMessage(error: unknown): string {
  if (error instanceof ProfileApiError) {
    if (error.code === "revision_conflict" || error.status === 409) {
      return "内容已在其他窗口更新，请重新加载后再保存。";
    }
    return error.requestId ? `${error.message} 请求 ID：${error.requestId}` : error.message;
  }
  return "保存失败，请重试。";
}

export function ProfileConfigForm({
  metadata,
  config,
  providers,
  onSaveMetadata,
  onSaveConfig,
  onReload,
}: {
  metadata: Versioned<ProfileMetadata>;
  config: Versioned<ProfileConfig>;
  providers: Provider[];
  onSaveMetadata: (patch: ProfilePatch, metadataEtag: string) => Promise<void>;
  onSaveConfig: (patch: ProfileConfigPatch, configEtag: string) => Promise<void>;
  onReload: () => void;
}) {
  const [displayName, setDisplayName] = useState(metadata.value.displayName);
  const [color, setColor] = useState(metadata.value.color ?? "");
  const [provider, setProvider] = useState(config.value.model.provider);
  const [model, setModel] = useState(config.value.model.model);
  const [baseUrl, setBaseUrl] = useState(config.value.model.baseUrl ?? "");
  const [reasoningEffort, setReasoningEffort] = useState<
    "" | NonNullable<ProfileConfig["model"]["reasoningEffort"]>
  >(
    config.value.model.reasoningEffort ?? "",
  );
  const [codeExecutionMode, setCodeExecutionMode] = useState<
    ProfileConfig["codeExecution"]["mode"]
  >(config.value.codeExecution.mode);
  const [codeExecutionTimeout, setCodeExecutionTimeout] = useState(
    String(config.value.codeExecution.timeoutSeconds),
  );
  const [codeExecutionMaxToolCalls, setCodeExecutionMaxToolCalls] = useState(
    String(config.value.codeExecution.maxToolCalls),
  );
  const [metadataBusy, setMetadataBusy] = useState(false);
  const [configBusy, setConfigBusy] = useState(false);
  const [metadataError, setMetadataError] = useState<string | null>(null);
  const [configError, setConfigError] = useState<string | null>(null);

  useEffect(() => {
    setDisplayName(metadata.value.displayName);
    setColor(metadata.value.color ?? "");
    setMetadataError(null);
  }, [metadata.etag, metadata.value.color, metadata.value.displayName]);

  useEffect(() => {
    setProvider(config.value.model.provider);
    setModel(config.value.model.model);
    setBaseUrl(config.value.model.baseUrl ?? "");
    setReasoningEffort(config.value.model.reasoningEffort ?? "");
    setCodeExecutionMode(config.value.codeExecution.mode);
    setCodeExecutionTimeout(String(config.value.codeExecution.timeoutSeconds));
    setCodeExecutionMaxToolCalls(String(config.value.codeExecution.maxToolCalls));
    setConfigError(null);
  }, [
    config.etag,
    config.value.codeExecution.maxToolCalls,
    config.value.codeExecution.mode,
    config.value.codeExecution.timeoutSeconds,
    config.value.model.baseUrl,
    config.value.model.model,
    config.value.model.provider,
    config.value.model.reasoningEffort,
  ]);

  const providerOptions = useMemo(() => {
    if (providers.some((item) => item.id === provider)) return providers;
    return [
      {
        id: provider,
        displayName: provider,
        defaultBaseUrl: null,
        requiresSecret: false,
        secretNames: [],
        supportsModelDiscovery: false,
      } satisfies Provider,
      ...providers,
    ];
  }, [provider, providers]);

  const normalizedDisplayName = displayName.trim();
  const normalizedColor = color.trim() || null;
  const metadataDirty = normalizedDisplayName !== metadata.value.displayName
    || normalizedColor !== (metadata.value.color ?? null);
  const normalizedBaseUrl = baseUrl.trim() || null;
  const normalizedReasoning: ProfileConfig["model"]["reasoningEffort"] = reasoningEffort || null;
  const configDirty = provider !== config.value.model.provider
    || model.trim() !== config.value.model.model
    || normalizedBaseUrl !== config.value.model.baseUrl
    || normalizedReasoning !== (config.value.model.reasoningEffort ?? null)
    || codeExecutionMode !== config.value.codeExecution.mode
    || codeExecutionTimeout !== String(config.value.codeExecution.timeoutSeconds)
    || codeExecutionMaxToolCalls !== String(config.value.codeExecution.maxToolCalls);

  const saveMetadata = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (!metadataDirty || metadataBusy) return;
    if (!normalizedDisplayName || Array.from(normalizedDisplayName).length > 80) {
      setMetadataError("显示名称必须包含 1 到 80 个字符。");
      return;
    }
    if (normalizedColor && !/^#[0-9a-f]{6}$/iu.test(normalizedColor)) {
      setMetadataError("颜色必须使用 #RRGGBB 格式。");
      return;
    }
    const patch: ProfilePatch = {};
    if (normalizedDisplayName !== metadata.value.displayName) {
      patch.displayName = normalizedDisplayName;
    }
    if (normalizedColor !== (metadata.value.color ?? null)) patch.color = normalizedColor;

    setMetadataBusy(true);
    setMetadataError(null);
    try {
      await onSaveMetadata(patch, metadata.etag);
    } catch (error) {
      setMetadataError(formErrorMessage(error));
    } finally {
      setMetadataBusy(false);
    }
  };

  const saveConfig = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (!configDirty || configBusy) return;
    const normalizedModel = model.trim();
    if (!provider) {
      setConfigError("Provider 不能为空。");
      return;
    }
    if (normalizedBaseUrl) {
      try {
        const url = new URL(normalizedBaseUrl);
        if (
          (url.protocol !== "http:" && url.protocol !== "https:")
          || url.username.length > 0
          || url.password.length > 0
          || url.search.length > 0
          || url.hash.length > 0
        ) throw new Error("unsafe URL");
      } catch {
        setConfigError("Base URL 必须是有效 URL。");
        return;
      }
    }
    const normalizedCodeExecutionTimeout = Number(codeExecutionTimeout);
    if (
      !Number.isInteger(normalizedCodeExecutionTimeout)
      || normalizedCodeExecutionTimeout < 1
      || normalizedCodeExecutionTimeout > 600
    ) {
      setConfigError("代码执行超时必须是 1 到 600 秒之间的整数。");
      return;
    }
    const normalizedCodeExecutionMaxToolCalls = Number(codeExecutionMaxToolCalls);
    if (
      !Number.isInteger(normalizedCodeExecutionMaxToolCalls)
      || normalizedCodeExecutionMaxToolCalls < 1
      || normalizedCodeExecutionMaxToolCalls > 100
    ) {
      setConfigError("工具调用上限必须是 1 到 100 之间的整数。");
      return;
    }

    const modelPatch: NonNullable<ProfileConfigPatch["model"]> = {};
    if (provider !== config.value.model.provider) modelPatch.provider = provider;
    if (normalizedModel !== config.value.model.model) modelPatch.model = normalizedModel;
    if (normalizedBaseUrl !== config.value.model.baseUrl) modelPatch.baseUrl = normalizedBaseUrl;
    if (normalizedReasoning !== (config.value.model.reasoningEffort ?? null)) {
      modelPatch.reasoningEffort = normalizedReasoning;
    }
    const codeExecutionPatch: NonNullable<ProfileConfigPatch["codeExecution"]> = {};
    if (codeExecutionMode !== config.value.codeExecution.mode) {
      codeExecutionPatch.mode = codeExecutionMode;
    }
    if (normalizedCodeExecutionTimeout !== config.value.codeExecution.timeoutSeconds) {
      codeExecutionPatch.timeoutSeconds = normalizedCodeExecutionTimeout;
    }
    if (normalizedCodeExecutionMaxToolCalls !== config.value.codeExecution.maxToolCalls) {
      codeExecutionPatch.maxToolCalls = normalizedCodeExecutionMaxToolCalls;
    }
    const patch: ProfileConfigPatch = {};
    if (Object.keys(modelPatch).length > 0) patch.model = modelPatch;
    if (Object.keys(codeExecutionPatch).length > 0) patch.codeExecution = codeExecutionPatch;

    setConfigBusy(true);
    setConfigError(null);
    try {
      await onSaveConfig(patch, config.etag);
    } catch (error) {
      setConfigError(formErrorMessage(error));
    } finally {
      setConfigBusy(false);
    }
  };

  return (
    <div className="profile-forms">
      <section className="profile-form-section" aria-labelledby="profile-metadata-title">
        <div className="profile-section-heading">
          <div>
            <p>PROFILE METADATA</p>
            <h2 id="profile-metadata-title">Profile 信息</h2>
          </div>
          <button className="icon-button" onClick={onReload} title="重新加载" type="button">
            <RefreshCw aria-hidden="true" size={16} />
            <span className="sr-only">重新加载</span>
          </button>
        </div>
        <form className="profile-form-grid" onSubmit={(event) => void saveMetadata(event)}>
          <label>
            <span>显示名称</span>
            <input
              disabled={metadataBusy}
              onChange={(event) => setDisplayName(event.target.value)}
              value={displayName}
            />
          </label>
          <label>
            <span>标识</span>
            <input disabled readOnly value={metadata.value.id} />
          </label>
          <div className="profile-color-field">
            <span>颜色</span>
            <div>
              <input
                aria-label="Profile 颜色"
                disabled={metadataBusy}
                onChange={(event) => setColor(event.target.value)}
                type="color"
                value={color || "#087f9d"}
              />
              <output>{color || "默认"}</output>
              <button
                aria-label="清除 Profile 颜色"
                className="icon-button"
                disabled={metadataBusy || !color}
                onClick={() => setColor("")}
                title="清除颜色"
                type="button"
              >
                <X aria-hidden="true" size={15} />
              </button>
            </div>
          </div>
          <div className="form-actions">
            {metadataError ? <p className="form-error" role="alert">{metadataError}</p> : <span />}
            <button className="primary-button" disabled={!metadataDirty || metadataBusy} type="submit">
              <Save aria-hidden="true" size={16} />
              保存信息
            </button>
          </div>
        </form>
      </section>

      <section className="profile-form-section" aria-labelledby="profile-model-title">
        <div className="profile-section-heading">
          <div>
            <p>MODEL CONFIG</p>
            <h2 id="profile-model-title">模型配置</h2>
          </div>
        </div>
        <form
          className="profile-form-grid"
          noValidate
          onSubmit={(event) => void saveConfig(event)}
        >
          <label>
            <span>Provider</span>
            <select
              disabled={configBusy}
              onChange={(event) => setProvider(event.target.value)}
              value={provider}
            >
              {providerOptions.map((item) => (
                <option key={item.id} value={item.id}>{item.displayName}</option>
              ))}
            </select>
          </label>
          <label>
            <span>模型</span>
            <input
              disabled={configBusy}
              onChange={(event) => setModel(event.target.value)}
              value={model}
            />
          </label>
          <label className="span-two">
            <span>Base URL</span>
            <input
              disabled={configBusy}
              inputMode="url"
              onChange={(event) => setBaseUrl(event.target.value)}
              placeholder="Provider 默认"
              value={baseUrl}
            />
          </label>
          <label>
            <span>推理强度</span>
            <select
              disabled={configBusy}
              onChange={(event) => setReasoningEffort(
                event.target.value as "" | NonNullable<ProfileConfig["model"]["reasoningEffort"]>,
              )}
              value={reasoningEffort}
            >
              <option value="">Provider 默认</option>
              <option value="minimal">Minimal</option>
              <option value="low">Low</option>
              <option value="medium">Medium</option>
              <option value="high">High</option>
              <option value="xhigh">XHigh</option>
            </select>
          </label>
          <fieldset className="profile-code-execution span-two">
            <legend>代码执行</legend>
            <label>
              <span>执行模式</span>
              <select
                disabled={configBusy}
                onChange={(event) => setCodeExecutionMode(
                  event.target.value as ProfileConfig["codeExecution"]["mode"],
                )}
                value={codeExecutionMode}
              >
                <option value="project">Project（Workspace）</option>
                <option value="strict">Strict（私有暂存目录）</option>
              </select>
            </label>
            <label>
              <span>超时（秒）</span>
              <input
                disabled={configBusy}
                inputMode="numeric"
                max={600}
                min={1}
                onChange={(event) => setCodeExecutionTimeout(event.target.value)}
                step={1}
                type="number"
                value={codeExecutionTimeout}
              />
            </label>
            <label>
              <span>工具调用上限</span>
              <input
                disabled={configBusy}
                inputMode="numeric"
                max={100}
                min={1}
                onChange={(event) => setCodeExecutionMaxToolCalls(event.target.value)}
                step={1}
                type="number"
                value={codeExecutionMaxToolCalls}
              />
            </label>
          </fieldset>
          <div className="form-actions span-two">
            {configError ? <p className="form-error" role="alert">{configError}</p> : <span />}
            <button className="primary-button" disabled={!configDirty || configBusy} type="submit">
              <Save aria-hidden="true" size={16} />
              保存配置
            </button>
          </div>
        </form>
      </section>
    </div>
  );
}
