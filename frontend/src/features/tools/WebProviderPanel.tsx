import {
  AlertCircle,
  CheckCircle2,
  FileText,
  Globe2,
  KeyRound,
  LoaderCircle,
  RefreshCw,
  Save,
  Search,
  Trash2,
} from "lucide-react";
import { useEffect, useRef, useState, type FormEvent } from "react";
import { DesktopConnectionError } from "../../api/desktopConnection";
import {
  ProfileApiError,
  profilesApi,
  type ProfilesApi,
  type SecretStatus,
} from "../../api/profiles";
import {
  WebApiError,
  webApi,
  type EffectiveWebProvider,
  type VersionedWebConfig,
  type WebApi,
  type WebConfigPatch,
  type WebProvider,
} from "../../api/web";

type SecretClient = Pick<
  ProfilesApi,
  "listSecretStatuses" | "putSecret" | "deleteSecret"
>;

type PanelState =
  | { phase: "idle" }
  | { phase: "loading" }
  | { phase: "error"; message: string }
  | {
    phase: "ready";
    profileId: string;
    providers: WebProvider[];
    config: VersionedWebConfig;
    secrets: SecretStatus[];
  };

type BusyAction =
  | "sharedProvider"
  | "searchProvider"
  | "extractProvider"
  | "extractCharLimit"
  | `putSecret:${string}`
  | `deleteSecret:${string}`;

export interface WebProviderPanelProps {
  profileId: string | null;
  searchAvailable: boolean;
  extractAvailable: boolean;
  client?: WebApi;
  profileClient?: SecretClient;
  mutationLocked?: boolean;
  onMutationStateChange?: (busy: boolean) => void;
}

const MIN_EXTRACT_CHAR_LIMIT = 2_000;
const MAX_EXTRACT_CHAR_LIMIT = 500_000;

function isAbortError(error: unknown): boolean {
  return error instanceof DOMException && error.name === "AbortError";
}

function errorMessage(error: unknown, fallback: string): string {
  if (error instanceof WebApiError || error instanceof ProfileApiError) {
    return error.requestId ? `${error.message} 请求 ID：${error.requestId}` : error.message;
  }
  if (error instanceof DesktopConnectionError) {
    return error.kind === "desktop_unavailable"
      ? "Web Provider 管理需要在 SynthChat Desktop 中使用。"
      : "本地 Rust 后端无法连接。";
  }
  return fallback;
}

function readinessLabel(
  available: boolean,
  effective: EffectiveWebProvider,
): string {
  if (!available || effective.status === "capabilityUnsupported") return "后端能力不可用";
  switch (effective.status) {
    case "ready":
      return "已就绪";
    case "unconfigured":
      return "未配置";
    case "missingSecret":
      return "缺少密钥";
    case "unsupported":
      return "Provider 不受支持";
  }
}

function ReadinessItem({
  available,
  effective,
  kind,
}: {
  available: boolean;
  effective: EffectiveWebProvider;
  kind: "search" | "extract";
}) {
  const ready = available && effective.status === "ready";
  const Icon = kind === "search" ? Search : FileText;
  return (
    <div className={ready ? "web-readiness-item is-ready" : "web-readiness-item"}>
      <span className="web-readiness-icon" aria-hidden="true">
        {ready ? <CheckCircle2 size={17} /> : <Icon size={17} />}
      </span>
      <span>
        <strong>{kind === "search" ? "Web Search" : "Web Extract"}</strong>
        <small>{readinessLabel(available, effective)}</small>
      </span>
      {available && effective.providerId ? <code>{effective.providerId}</code> : null}
      {available && effective.missingSecretNames.length > 0 ? (
        <p>缺少：{effective.missingSecretNames.join("、")}</p>
      ) : null}
    </div>
  );
}

function ProviderOptions({
  current,
  followLabel,
  providers,
  supports,
}: {
  current: string | null;
  followLabel: string;
  providers: WebProvider[];
  supports: "search" | "extract" | "both";
}) {
  const available = providers.filter((provider) => (
    supports === "both"
    || (supports === "search" && provider.supportsSearch)
    || (supports === "extract" && provider.supportsExtract)
  ));
  const currentSupported = current === null || available.some((provider) => provider.id === current);
  return (
    <>
      <option value="">{followLabel}</option>
      {!currentSupported && current ? (
        <option disabled value={current}>{current}（不受支持）</option>
      ) : null}
      {available.map((provider) => (
        <option key={provider.id} value={provider.id}>{provider.displayName}</option>
      ))}
    </>
  );
}

export function WebProviderPanel({
  profileId,
  searchAvailable,
  extractAvailable,
  client = webApi,
  profileClient = profilesApi,
  mutationLocked = false,
  onMutationStateChange,
}: WebProviderPanelProps) {
  const enabled = searchAvailable || extractAvailable;
  const [panel, setPanel] = useState<PanelState>({ phase: "idle" });
  const [loadEpoch, setLoadEpoch] = useState(0);
  const [busyAction, setBusyAction] = useState<BusyAction | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [extractCharDraft, setExtractCharDraft] = useState("15000");
  const [secretDrafts, setSecretDrafts] = useState<Record<string, string>>({});
  const mutationController = useRef<AbortController | null>(null);

  useEffect(() => {
    const busy = busyAction !== null;
    onMutationStateChange?.(busy);
    return () => {
      if (busy) onMutationStateChange?.(false);
    };
  }, [busyAction, onMutationStateChange]);

  useEffect(() => {
    mutationController.current?.abort();
    mutationController.current = null;
    setBusyAction(null);
    setActionError(null);
    setSecretDrafts({});

    if (!enabled || !profileId) {
      setPanel({ phase: "idle" });
      return undefined;
    }

    const controller = new AbortController();
    setPanel({ phase: "loading" });
    void Promise.all([
      client.listProviders({ signal: controller.signal }),
      client.getWebConfig(profileId, { signal: controller.signal }),
      profileClient.listSecretStatuses(profileId, { signal: controller.signal }),
    ])
      .then(([providers, config, secrets]) => {
        if (controller.signal.aborted) return;
        setExtractCharDraft(String(config.value.extractCharLimit));
        setPanel({ phase: "ready", profileId, providers, config, secrets });
      })
      .catch((error: unknown) => {
        if (!controller.signal.aborted && !isAbortError(error)) {
          setPanel({
            phase: "error",
            message: errorMessage(error, "无法加载 Web Provider 配置。"),
          });
        }
      });

    return () => {
      controller.abort();
      mutationController.current?.abort();
      mutationController.current = null;
    };
  }, [client, enabled, loadEpoch, profileClient, profileId]);

  const beginMutation = (action: BusyAction): AbortController | null => {
    if (
      !profileId
      || panel.phase !== "ready"
      || panel.profileId !== profileId
      || busyAction
      || mutationLocked
    ) return null;
    mutationController.current?.abort();
    const controller = new AbortController();
    mutationController.current = controller;
    setBusyAction(action);
    setActionError(null);
    return controller;
  };

  const finishMutation = (controller: AbortController) => {
    if (mutationController.current !== controller) return;
    mutationController.current = null;
    setBusyAction(null);
  };

  const replaceConfig = (config: VersionedWebConfig) => {
    setExtractCharDraft(String(config.value.extractCharLimit));
    setPanel((current) => current.phase === "ready"
      ? { ...current, config }
      : current);
  };

  const updateConfig = async (patch: WebConfigPatch, action: BusyAction) => {
    if (!profileId || panel.phase !== "ready" || panel.profileId !== profileId) return;
    const source = panel.config;
    const targetProfileId = profileId;
    const controller = beginMutation(action);
    if (!controller) return;
    try {
      const updated = await client.updateWebConfig(
        targetProfileId,
        patch,
        source.etag,
        { signal: controller.signal },
      );
      if (!controller.signal.aborted) replaceConfig(updated);
    } catch (error) {
      if (controller.signal.aborted || isAbortError(error)) return;
      if (error instanceof WebApiError && error.status === 409) {
        setActionError(
          "Web 配置已在其他窗口更新，已重新加载最新状态，请确认后重试。",
        );
        try {
          const refreshed = await client.getWebConfig(targetProfileId, {
            signal: controller.signal,
          });
          if (!controller.signal.aborted) replaceConfig(refreshed);
        } catch (refreshError) {
          if (!controller.signal.aborted && !isAbortError(refreshError)) {
            setPanel({
              phase: "error",
              message: errorMessage(refreshError, "配置冲突后无法重新加载 Web 配置。"),
            });
          }
        }
      } else {
        setActionError(errorMessage(error, "无法更新 Web Provider 配置。"));
      }
    } finally {
      finishMutation(controller);
    }
  };

  const refreshReadiness = async (
    targetProfileId: string,
    controller: AbortController,
  ) => {
    const refreshed = await client.getWebConfig(targetProfileId, {
      signal: controller.signal,
    });
    if (!controller.signal.aborted) replaceConfig(refreshed);
  };

  const putSecret = async (secretName: string) => {
    if (!profileId || panel.phase !== "ready" || panel.profileId !== profileId) return;
    const value = secretDrafts[secretName] ?? "";
    if (!value) {
      setActionError(`请输入 ${secretName}。`);
      return;
    }
    const targetProfileId = profileId;
    const controller = beginMutation(`putSecret:${secretName}`);
    if (!controller) return;
    try {
      const status = await profileClient.putSecret(
        targetProfileId,
        secretName,
        value,
        { signal: controller.signal },
      );
      if (controller.signal.aborted) return;
      setSecretDrafts((current) => ({ ...current, [secretName]: "" }));
      setPanel((current) => {
        if (current.phase !== "ready") return current;
        const known = current.secrets.some((item) => item.name === status.name);
        return {
          ...current,
          secrets: known
            ? current.secrets.map((item) => item.name === status.name ? status : item)
            : [...current.secrets, status],
        };
      });
      await refreshReadiness(targetProfileId, controller);
    } catch (error) {
      if (!controller.signal.aborted && !isAbortError(error)) {
        setActionError(errorMessage(error, `无法保存 ${secretName}。`));
      }
    } finally {
      finishMutation(controller);
    }
  };

  const deleteSecret = async (secretName: string) => {
    if (!profileId || panel.phase !== "ready" || panel.profileId !== profileId) return;
    const targetProfileId = profileId;
    const controller = beginMutation(`deleteSecret:${secretName}`);
    if (!controller) return;
    try {
      await profileClient.deleteSecret(targetProfileId, secretName, {
        signal: controller.signal,
      });
      if (controller.signal.aborted) return;
      setSecretDrafts((current) => ({ ...current, [secretName]: "" }));
      setPanel((current) => current.phase === "ready"
        ? {
          ...current,
          secrets: current.secrets.map((item) => item.name === secretName
            ? { ...item, configured: false, updatedAt: null }
            : item),
        }
        : current);
      await refreshReadiness(targetProfileId, controller);
    } catch (error) {
      if (!controller.signal.aborted && !isAbortError(error)) {
        setActionError(errorMessage(error, `无法删除 ${secretName}。`));
      }
    } finally {
      finishMutation(controller);
    }
  };

  const submitExtractLimit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const next = Number(extractCharDraft);
    if (
      !Number.isInteger(next)
      || next < MIN_EXTRACT_CHAR_LIMIT
      || next > MAX_EXTRACT_CHAR_LIMIT
    ) {
      setActionError("提取字符上限必须是 2,000 到 500,000 之间的整数。");
      return;
    }
    void updateConfig({ extractCharLimit: next }, "extractCharLimit");
  };

  const controlsDisabled = busyAction !== null || mutationLocked;

  return (
    <section className="web-provider-panel" aria-labelledby="web-provider-title">
      <div className="tools-section-heading web-section-heading">
        <div>
          <span>WEB PROVIDERS</span>
          <h2 id="web-provider-title">Web Search 与提取</h2>
        </div>
        <Globe2 aria-hidden="true" size={19} />
      </div>

      {!enabled ? (
        <div className="tools-inline-state web-inline-state">
          当前后端未启用 Web Search 或 Web Extract 能力。
        </div>
      ) : !profileId ? (
        <div className="tools-inline-state web-inline-state">没有可用的 Profile。</div>
      ) : panel.phase === "loading"
        || panel.phase === "idle"
        || (panel.phase === "ready" && panel.profileId !== profileId) ? (
        <div className="tools-inline-state web-inline-state" role="status" aria-busy="true">
          <LoaderCircle aria-hidden="true" className="spin" size={20} />
          正在加载 Web Provider 配置
        </div>
      ) : panel.phase === "error" ? (
        <div className="tools-inline-state web-inline-state is-error">
          <p role="alert">{panel.message}</p>
          <button
            className="tools-secondary-button"
            onClick={() => setLoadEpoch((value) => value + 1)}
            type="button"
          >
            <RefreshCw aria-hidden="true" size={15} />
            重新加载 Web 配置
          </button>
        </div>
      ) : (
        <div className="web-provider-content">
          <div className="web-readiness" aria-label="Web 能力状态">
            <ReadinessItem
              available={searchAvailable}
              effective={panel.config.value.effectiveSearch}
              kind="search"
            />
            <ReadinessItem
              available={extractAvailable}
              effective={panel.config.value.effectiveExtract}
              kind="extract"
            />
          </div>

          {actionError ? (
            <p className="tools-action-error web-action-error" role="alert">
              <AlertCircle aria-hidden="true" size={15} />
              {actionError}
            </p>
          ) : null}

          <div className="web-settings" aria-busy={busyAction !== null || undefined}>
            <label>
              <span>共享 Provider</span>
              <select
                aria-label="共享 Web Provider"
                disabled={controlsDisabled}
                onChange={(event) => void updateConfig(
                  { sharedProvider: event.target.value ? "tavily" : null },
                  "sharedProvider",
                )}
                value={panel.config.value.sharedProvider ?? ""}
              >
                <ProviderOptions
                  current={panel.config.value.sharedProvider}
                  followLabel="自动选择可用 Provider"
                  providers={panel.providers}
                  supports="both"
                />
              </select>
            </label>

            <label>
              <span>Search Provider</span>
              <select
                aria-label="Web Search Provider"
                disabled={controlsDisabled || !searchAvailable}
                onChange={(event) => void updateConfig(
                  { searchProvider: event.target.value ? "tavily" : null },
                  "searchProvider",
                )}
                value={panel.config.value.searchProvider ?? ""}
              >
                <ProviderOptions
                  current={panel.config.value.searchProvider}
                  followLabel="跟随共享设置"
                  providers={panel.providers}
                  supports="search"
                />
              </select>
              {!searchAvailable ? <small>当前后端能力不可用</small> : null}
            </label>

            <label>
              <span>Extract Provider</span>
              <select
                aria-label="Web Extract Provider"
                disabled={controlsDisabled || !extractAvailable}
                onChange={(event) => void updateConfig(
                  { extractProvider: event.target.value ? "tavily" : null },
                  "extractProvider",
                )}
                value={panel.config.value.extractProvider ?? ""}
              >
                <ProviderOptions
                  current={panel.config.value.extractProvider}
                  followLabel="跟随共享设置"
                  providers={panel.providers}
                  supports="extract"
                />
              </select>
              {!extractAvailable ? <small>当前后端能力不可用</small> : null}
            </label>

            <form className="web-char-limit" onSubmit={submitExtractLimit}>
              <label>
                <span>单页提取字符上限</span>
                <input
                  aria-label="Web Extract 字符上限"
                  disabled={controlsDisabled || !extractAvailable}
                  inputMode="numeric"
                  max={MAX_EXTRACT_CHAR_LIMIT}
                  min={MIN_EXTRACT_CHAR_LIMIT}
                  onChange={(event) => setExtractCharDraft(event.target.value)}
                  step={1}
                  type="number"
                  value={extractCharDraft}
                />
              </label>
              <button
                className="tools-secondary-button"
                disabled={
                  controlsDisabled
                  || !extractAvailable
                  || extractCharDraft === String(panel.config.value.extractCharLimit)
                }
                type="submit"
              >
                {busyAction === "extractCharLimit"
                  ? <LoaderCircle aria-hidden="true" className="spin" size={15} />
                  : <Save aria-hidden="true" size={15} />}
                保存字符上限
              </button>
            </form>
          </div>

          <div className="web-secrets">
            <div className="web-subheading">
              <KeyRound aria-hidden="true" size={16} />
              <h3>Provider 密钥</h3>
              <span>OS 密钥链</span>
            </div>
            {panel.providers.length === 0 ? (
              <p className="web-empty-note">当前没有可配置的 Web Provider。</p>
            ) : panel.providers.flatMap((provider) => provider.secretNames).map((secretName) => {
              const status = panel.secrets.find((item) => item.name === secretName);
              const configured = status?.configured === true;
              const saving = busyAction === `putSecret:${secretName}`;
              const deleting = busyAction === `deleteSecret:${secretName}`;
              return (
                <div className="web-secret-row" key={secretName}>
                  <span className="web-secret-status">
                    <strong>{secretName}</strong>
                    <small>{configured ? "已存储于系统密钥链" : "未配置"}</small>
                  </span>
                  <input
                    aria-label={`输入 ${secretName}`}
                    autoComplete="new-password"
                    disabled={controlsDisabled}
                    maxLength={2560}
                    onChange={(event) => setSecretDrafts((current) => ({
                      ...current,
                      [secretName]: event.target.value,
                    }))}
                    placeholder={configured ? "输入新值以替换" : "输入密钥"}
                    type="password"
                    value={secretDrafts[secretName] ?? ""}
                  />
                  <button
                    aria-label={`保存 ${secretName}`}
                    className="web-icon-button"
                    disabled={controlsDisabled || !(secretDrafts[secretName] ?? "")}
                    onClick={() => void putSecret(secretName)}
                    title={`保存 ${secretName}`}
                    type="button"
                  >
                    {saving
                      ? <LoaderCircle aria-hidden="true" className="spin" size={16} />
                      : <Save aria-hidden="true" size={16} />}
                  </button>
                  <button
                    aria-label={`删除 ${secretName}`}
                    className="web-icon-button is-danger"
                    disabled={controlsDisabled || !configured}
                    onClick={() => void deleteSecret(secretName)}
                    title={`删除 ${secretName}`}
                    type="button"
                  >
                    {deleting
                      ? <LoaderCircle aria-hidden="true" className="spin" size={16} />
                      : <Trash2 aria-hidden="true" size={16} />}
                  </button>
                </div>
              );
            })}
          </div>
        </div>
      )}
    </section>
  );
}
