import {
  CircleSlash2,
  Code2,
  LoaderCircle,
  PackagePlus,
  PanelsTopLeft,
  Puzzle,
  RefreshCw,
  Search,
  Trash2,
  Wrench,
  X,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState, type FormEvent } from "react";
import { DesktopConnectionError } from "../../api/desktopConnection";
import { FileApiError, filesApi, type FilesApi } from "../../api/files";
import { McpApiError, mcpApi, type McpApi } from "../../api/mcp";
import {
  ProfileApiError,
  profilesApi,
  type ProfilesApi,
  type ProfileSummary,
} from "../../api/profiles";
import {
  SkillApiError,
  skillsApi,
  type Skill,
  type InstallSkillInput,
  type Operation,
  type SkillsApi,
  type VersionedSkillPage,
} from "../../api/skills";
import {
  ToolsetApiError,
  toolsetsApi,
  type Toolset,
  type ToolsetsApi,
  type VersionedToolsets,
} from "../../api/toolsets";
import { webApi, type WebApi } from "../../api/web";
import {
  readSkillOperationRuntimeConfig,
  type SkillOperationRuntimeConfig,
} from "../../config/runtimeConfig/skillOperations";
import { McpServersPanel } from "./McpServersPanel";
import { WebProviderPanel } from "./WebProviderPanel";
import "./tools.css";

type ProfileClient = Pick<
  ProfilesApi,
  | "getCapabilities"
  | "listProfiles"
  | "listSecretStatuses"
  | "putSecret"
  | "deleteSecret"
>;

type WorkspaceState =
  | { phase: "loading" }
  | { phase: "desktop-required" }
  | { phase: "unavailable" }
  | { phase: "error"; message: string }
  | { phase: "ready" };

type ToolsetState =
  | { phase: "idle" }
  | { phase: "loading" }
  | { phase: "error"; message: string }
  | { phase: "ready"; resource: VersionedToolsets };

type SkillState =
  | { phase: "idle" }
  | { phase: "loading" }
  | { phase: "error"; message: string }
  | {
    phase: "ready";
    resource: VersionedSkillPage;
    loadingMore: boolean;
    loadMoreError: string | null;
  };

interface ManagementCapabilities {
  toolsets: boolean;
  codeExecution: boolean;
  skillDiscovery: boolean;
  skillEnablement: boolean;
  skillManagement: boolean;
  webSearch: boolean;
  webExtract: boolean;
  browserAutomation: boolean;
  browserCdp: boolean;
  browserDownloads: boolean;
  mcpManagement: boolean;
  mcpStdio: boolean;
  mcpStreamableHttp: boolean;
  mcpSse: boolean;
}

const NO_MANAGEMENT_CAPABILITIES: ManagementCapabilities = {
  toolsets: false,
  codeExecution: false,
  skillDiscovery: false,
  skillEnablement: false,
  skillManagement: false,
  webSearch: false,
  webExtract: false,
  browserAutomation: false,
  browserCdp: false,
  browserDownloads: false,
  mcpManagement: false,
  mcpStdio: false,
  mcpStreamableHttp: false,
  mcpSse: false,
};
const SKILL_PAGE_LIMIT = 30;
const OPERATION_ID_PATTERN = /^op_[0-9a-f]{32}$/u;
const PROFILE_ID_PATTERN = /^(?:default|[a-z0-9_][a-z0-9_-]{0,63})$/u;
const PUBLIC_CODE_PATTERN = /^[a-z0-9_.:-]{1,80}$/u;
const PUBLIC_REQUEST_ID_PATTERN = /^[a-zA-Z0-9_.:-]{1,128}$/u;
const SKILL_OPERATION_STORAGE_PREFIX = "synthchat.skill-operation.v1";

type SkillInstallMode = "registry" | "url" | "file";
type SkillManagementAction =
  | { kind: "install" }
  | { kind: "uninstall"; skillId: string | null };
type PendingSkillOperation = {
  id: string;
  kind: "skillInstall" | "skillUninstall";
};

export interface ToolsWorkspaceProps {
  client?: ToolsetsApi;
  profileClient?: ProfileClient;
  skillsClient?: SkillsApi;
  filesClient?: FilesApi;
  mcpClient?: McpApi;
  webClient?: WebApi;
  skillOperationPolling?: SkillOperationRuntimeConfig;
}

function isAbortError(error: unknown): boolean {
  return error instanceof DOMException && error.name === "AbortError";
}

function errorMessage(error: unknown, fallback: string): string {
  if (
    error instanceof ToolsetApiError
    || error instanceof SkillApiError
    || error instanceof FileApiError
    || error instanceof McpApiError
    || error instanceof ProfileApiError
  ) {
    return error.requestId ? `${error.message} 请求 ID：${error.requestId}` : error.message;
  }
  if (error instanceof DesktopConnectionError) {
    return error.kind === "desktop_unavailable"
      ? "工具管理需要在 SynthChat Desktop 中使用。"
      : "本地 Rust 后端无法连接。";
  }
  return fallback;
}

function newIdempotencyKey(prefix: string): string {
  if (typeof globalThis.crypto?.randomUUID === "function") {
    return `${prefix}-${globalThis.crypto.randomUUID()}`;
  }
  const bytes = new Uint8Array(16);
  globalThis.crypto.getRandomValues(bytes);
  const suffix = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
  return `${prefix}-${suffix}`;
}

function backendOperationScope(values: readonly (string | null | undefined)[]): string {
  return encodeURIComponent(JSON.stringify(values.map((value) => value ?? null)));
}

function operationStorageKey(backendScope: string, profileId: string): string | null {
  if (!backendScope || !PROFILE_ID_PATTERN.test(profileId)) return null;
  return `${SKILL_OPERATION_STORAGE_PREFIX}:${backendScope}:${profileId}`;
}

function sessionStore(): Storage | null {
  try {
    return globalThis.sessionStorage ?? null;
  } catch {
    return null;
  }
}

function parsePendingSkillOperation(value: string): PendingSkillOperation | null {
  try {
    const parsed = JSON.parse(value) as unknown;
    if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) return null;
    const record = parsed as Record<string, unknown>;
    if (
      Object.keys(record).length !== 2
      || !("id" in record)
      || !("kind" in record)
      || typeof record.id !== "string"
      || !OPERATION_ID_PATTERN.test(record.id)
      || (record.kind !== "skillInstall" && record.kind !== "skillUninstall")
    ) return null;
    return { id: record.id, kind: record.kind };
  } catch {
    return null;
  }
}

function readPendingSkillOperation(key: string): PendingSkillOperation | null {
  const storage = sessionStore();
  if (!storage) return null;
  try {
    const value = storage.getItem(key);
    if (value === null) return null;
    const pending = parsePendingSkillOperation(value);
    if (!pending) storage.removeItem(key);
    return pending;
  } catch {
    return null;
  }
}

function persistPendingSkillOperation(key: string, operation: Operation): boolean {
  if (
    !OPERATION_ID_PATTERN.test(operation.id)
    || (operation.kind !== "skillInstall" && operation.kind !== "skillUninstall")
  ) return false;
  const storage = sessionStore();
  if (!storage) return false;
  try {
    storage.setItem(key, JSON.stringify({ id: operation.id, kind: operation.kind }));
    return true;
  } catch {
    return false;
  }
}

function clearPendingSkillOperation(key: string, operationId: string): void {
  const storage = sessionStore();
  if (!storage) return;
  try {
    const value = storage.getItem(key);
    if (value === null) return;
    const pending = parsePendingSkillOperation(value);
    if (!pending || pending.id === operationId) storage.removeItem(key);
  } catch {
    // Recovery storage is best-effort when the browser denies access.
  }
}

function expectedOperationKind(action: SkillManagementAction): PendingSkillOperation["kind"] {
  return action.kind === "install" ? "skillInstall" : "skillUninstall";
}

function assertTrackedOperation(
  operation: Operation,
  expectedId: string | null,
  expectedKind: PendingSkillOperation["kind"],
): void {
  if (
    !OPERATION_ID_PATTERN.test(operation.id)
    || (expectedId !== null && operation.id !== expectedId)
    || operation.kind !== expectedKind
  ) {
    throw new SkillApiError(
      "invalid_response",
      "Skill operation did not match the accepted operation.",
    );
  }
}

function operationAction(kind: PendingSkillOperation["kind"]): SkillManagementAction {
  return kind === "skillInstall"
    ? { kind: "install" }
    : { kind: "uninstall", skillId: null };
}

function cleanupFailureMessage(error: unknown): string {
  const code = error instanceof FileApiError && error.code && PUBLIC_CODE_PATTERN.test(error.code)
    ? error.code
    : "file_cleanup_failed";
  const requestId = error instanceof FileApiError
    && error.requestId
    && PUBLIC_REQUEST_ID_PATTERN.test(error.requestId)
    ? ` 请求 ID：${error.requestId}`
    : "";
  return `临时文件清理失败（${code}）。${requestId}`;
}

function waitForPoll(delayMs: number, signal: AbortSignal): Promise<void> {
  if (signal.aborted) return Promise.reject(new DOMException("aborted", "AbortError"));
  if (delayMs === 0) return Promise.resolve();
  return new Promise((resolve, reject) => {
    const timeout = globalThis.setTimeout(() => {
      signal.removeEventListener("abort", abort);
      resolve();
    }, delayMs);
    const abort = () => {
      globalThis.clearTimeout(timeout);
      reject(new DOMException("aborted", "AbortError"));
    };
    signal.addEventListener("abort", abort, { once: true });
  });
}

async function pollSkillOperation(
  initial: Operation,
  client: SkillsApi,
  signal: AbortSignal,
  polling: SkillOperationRuntimeConfig,
): Promise<Operation> {
  let operation = initial;
  for (let attempt = 0; attempt < polling.maxPolls; attempt += 1) {
    if (!["queued", "running"].includes(operation.status)) return operation;
    const delayMs = attempt === 0
      ? 0
      : Math.min(polling.initialBackoffMs * (2 ** (attempt - 1)), polling.maxBackoffMs);
    await waitForPoll(delayMs, signal);
    operation = await client.getOperation(operation.id, { signal });
  }
  if (!["queued", "running"].includes(operation.status)) return operation;
  throw new SkillApiError(
    "http",
    "Skill 操作仍在处理中，请稍后刷新。",
    { retryable: true },
  );
}

function operationFailureMessage(operation: Operation): string {
  if (operation.status === "cancelled") return "Skill 操作已取消。";
  const problem = operation.error;
  if (!problem) return "Skill 操作失败。";
  const code = PUBLIC_CODE_PATTERN.test(problem.code) ? `（${problem.code}）` : "";
  const requestId = PUBLIC_REQUEST_ID_PATTERN.test(problem.requestId)
    ? ` 请求 ID：${problem.requestId}`
    : "";
  return `Skill 操作失败${code}。${requestId}`;
}

function WorkspaceStateView({
  onRetry,
  state,
}: {
  onRetry: () => void;
  state: Exclude<WorkspaceState, { phase: "ready" }>;
}) {
  const loading = state.phase === "loading";
  const failed = state.phase === "error";
  const Icon = loading ? LoaderCircle : failed ? CircleSlash2 : Wrench;
  const title = loading
    ? "正在连接扩展服务"
    : state.phase === "desktop-required"
      ? "请在 SynthChat Desktop 中打开"
      : state.phase === "unavailable"
        ? "工具与 Skills 暂不可用"
        : "扩展服务连接失败";
  const message = loading
    ? null
    : state.phase === "desktop-required"
      ? "Profile 工具集与 Skills 仅通过受保护的桌面后端连接提供。"
      : state.phase === "unavailable"
        ? "当前 Rust 后端尚未启用 Toolset 或 Skills 目录能力。"
        : state.message;

  return (
    <div
      aria-busy={loading || undefined}
      className="tools-state"
      role={failed ? "alert" : "status"}
    >
      <Icon aria-hidden="true" className={loading ? "spin" : undefined} size={30} />
      <h2>{title}</h2>
      {message ? <p>{message}</p> : null}
      {failed ? (
        <button className="tools-secondary-button" onClick={onRetry} type="button">
          <RefreshCw aria-hidden="true" size={15} />
          重试
        </button>
      ) : null}
    </div>
  );
}

function ToolsetRow({
  busy,
  disabled,
  onToggle,
  toolset,
}: {
  busy: boolean;
  disabled: boolean;
  onToggle: () => void;
  toolset: Toolset;
}) {
  return (
    <li aria-busy={busy || undefined} className="toolset-row">
      <div className="toolset-main">
        <header>
          <span className="toolset-mark" aria-hidden="true">
            <Wrench size={16} />
          </span>
          <span className="toolset-title">
            <strong>{toolset.displayName}</strong>
            <code>{toolset.id}</code>
          </span>
          <span className={toolset.configured ? "toolset-badge is-configured" : "toolset-badge"}>
            {toolset.configured ? "已配置" : "未配置"}
          </span>
        </header>
        <p>{toolset.description || "暂无说明"}</p>
        <details className="toolset-members">
          <summary>{toolset.tools.length} 个工具</summary>
          {toolset.tools.length > 0 ? (
            <ul>
              {toolset.tools.map((tool) => <li key={tool}>{tool}</li>)}
            </ul>
          ) : <span>无工具成员</span>}
        </details>
      </div>

      <div className="toolset-control">
        <span className="toolset-busy-slot">
          {busy ? <LoaderCircle aria-hidden="true" className="spin" size={16} /> : null}
        </span>
        <label className="toolset-switch">
          <input
            aria-label={toolset.enabled
              ? `停用 ${toolset.displayName} (${toolset.id})`
              : `启用 ${toolset.displayName} (${toolset.id})`}
            checked={toolset.enabled}
            disabled={disabled}
            onChange={onToggle}
            role="switch"
            type="checkbox"
          />
        </label>
        <small aria-live="polite">
          {busy ? "保存中" : toolset.enabled ? "已启用" : "已停用"}
        </small>
      </div>
    </li>
  );
}

const SKILL_SOURCE_LABELS: Record<Skill["source"], string> = {
  bundled: "内置",
  local: "本地",
  registry: "Registry",
  url: "URL",
  file: "文件",
};

function SkillRow({
  busy,
  disabled,
  enablementAvailable,
  onUninstall,
  onToggle,
  skill,
  uninstallBusy,
  uninstallDisabled,
}: {
  busy: boolean;
  disabled: boolean;
  enablementAvailable: boolean;
  onUninstall?: () => void;
  onToggle: () => void;
  skill: Skill;
  uninstallBusy: boolean;
  uninstallDisabled: boolean;
}) {
  return (
    <li aria-busy={busy || uninstallBusy || undefined} className="toolset-row skill-row">
      <div className="toolset-main">
        <header>
          <span className="toolset-mark skill-mark" aria-hidden="true">
            <Puzzle size={16} />
          </span>
          <span className="toolset-title">
            <strong>{skill.name}</strong>
            <code>{skill.id}</code>
          </span>
          <span className="skill-source">{SKILL_SOURCE_LABELS[skill.source]}</span>
          {skill.configurable ? <span className="skill-configurable">可配置</span> : null}
        </header>
        <p>{skill.description || "暂无说明"}</p>
        <div className="skill-meta">
          <span>{skill.version ? `v${skill.version}` : "未标注版本"}</span>
        </div>
      </div>

      <div className="skill-row-actions">
        <div className="toolset-control">
          <span className="toolset-busy-slot">
            {busy ? <LoaderCircle aria-hidden="true" className="spin" size={16} /> : null}
          </span>
          <label className="toolset-switch">
            <input
              aria-label={skill.enabled
                ? `停用 Skill ${skill.name} (${skill.id})`
                : `启用 Skill ${skill.name} (${skill.id})`}
              checked={skill.enabled}
              disabled={disabled}
              onChange={onToggle}
              role="switch"
              type="checkbox"
            />
          </label>
          <small aria-live="polite">
            {busy
              ? "保存中"
              : !enablementAvailable
                ? "只读"
                : skill.enabled
                  ? "已启用"
                  : "已停用"}
          </small>
        </div>
        {onUninstall ? (
          <button
            aria-label={`卸载 Skill ${skill.name} (${skill.id})`}
            className="skills-icon-button skill-uninstall-button"
            disabled={uninstallDisabled}
            onClick={onUninstall}
            title="卸载 Skill"
            type="button"
          >
            {uninstallBusy
              ? <LoaderCircle aria-hidden="true" className="spin" size={15} />
              : <Trash2 aria-hidden="true" size={15} />}
          </button>
        ) : null}
      </div>
    </li>
  );
}

export function ToolsWorkspace({
  client = toolsetsApi,
  filesClient = filesApi,
  mcpClient = mcpApi,
  profileClient = profilesApi,
  skillOperationPolling,
  skillsClient = skillsApi,
  webClient = webApi,
}: ToolsWorkspaceProps) {
  const effectiveSkillOperationPolling = useMemo(
    () => skillOperationPolling ?? readSkillOperationRuntimeConfig(),
    [skillOperationPolling],
  );
  const [workspace, setWorkspace] = useState<WorkspaceState>({ phase: "loading" });
  const [management, setManagement] = useState<ManagementCapabilities>(
    NO_MANAGEMENT_CAPABILITIES,
  );
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [profileId, setProfileId] = useState<string | null>(null);
  const [toolsets, setToolsets] = useState<ToolsetState>({ phase: "idle" });
  const [skills, setSkills] = useState<SkillState>({ phase: "idle" });
  const [busyId, setBusyId] = useState<string | null>(null);
  const [busySkillId, setBusySkillId] = useState<string | null>(null);
  const [mcpMutationBusy, setMcpMutationBusy] = useState(false);
  const [webMutationBusy, setWebMutationBusy] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [skillActionError, setSkillActionError] = useState<string | null>(null);
  const [skillCleanupError, setSkillCleanupError] = useState<string | null>(null);
  const [skillManagementAction, setSkillManagementAction] = useState<SkillManagementAction | null>(
    null,
  );
  const [skillOperationBackendScope, setSkillOperationBackendScope] = useState<string | null>(null);
  const [skillInstallMode, setSkillInstallMode] = useState<SkillInstallMode>("registry");
  const [skillRegistryId, setSkillRegistryId] = useState("");
  const [skillUrl, setSkillUrl] = useState("");
  const [skillFile, setSkillFile] = useState<File | null>(null);
  const [skillSearchInput, setSkillSearchInput] = useState("");
  const [skillQuery, setSkillQuery] = useState("");
  const [workspaceEpoch, setWorkspaceEpoch] = useState(0);
  const [resourceEpoch, setResourceEpoch] = useState(0);
  const [skillEpoch, setSkillEpoch] = useState(0);
  const paginationController = useRef<AbortController | null>(null);
  const operationController = useRef<AbortController | null>(null);
  const recoveryAttempt = useRef<string | null>(null);
  const skillFileInput = useRef<HTMLInputElement | null>(null);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      paginationController.current?.abort();
      const operation = operationController.current;
      operationController.current = null;
      operation?.abort();
    };
  }, []);

  useEffect(() => {
    const controller = new AbortController();
    setWorkspace({ phase: "loading" });
    setManagement(NO_MANAGEMENT_CAPABILITIES);
    setSkillOperationBackendScope(null);
    recoveryAttempt.current = null;
    operationController.current?.abort();
    operationController.current = null;
    setSkillManagementAction(null);
    setActionError(null);
    setSkillActionError(null);
    void (async () => {
      try {
        const capabilities = await profileClient.getCapabilities({ signal: controller.signal });
        if (controller.signal.aborted) return;
        const operationScope = backendOperationScope([
          capabilities.contractVersion,
          capabilities.backendVersion,
          capabilities.engine.kind,
          capabilities.engine.version,
          capabilities.engine.pinnedCommit,
        ]);
        const available: ManagementCapabilities = {
          toolsets: capabilities.extensions.toolsetManagement === true,
          codeExecution: capabilities.extensions.codeExecution === true,
          skillDiscovery: capabilities.extensions.skillDiscovery === true,
          skillEnablement: capabilities.extensions.skillEnablement === true,
          skillManagement: capabilities.engine.features.skillManagement === true,
          webSearch: capabilities.extensions.webSearch === true,
          webExtract: capabilities.extensions.webExtract === true,
          browserAutomation: capabilities.extensions.browserAutomation === true,
          browserCdp: capabilities.extensions.browserCdp === true,
          browserDownloads: capabilities.extensions.browserDownloads === true,
          mcpManagement: capabilities.engine.features.mcpManagement === true,
          mcpStdio: capabilities.extensions.mcpStdio === true,
          mcpStreamableHttp: capabilities.extensions.mcpStreamableHttp === true,
          mcpSse: capabilities.extensions.mcpSse === true,
        };
        if (
          !available.toolsets
          && !available.codeExecution
          && !available.skillDiscovery
          && !available.skillManagement
          && !available.webSearch
          && !available.webExtract
          && !available.browserAutomation
          && !available.browserCdp
          && !available.browserDownloads
          && !available.mcpManagement
        ) {
          setWorkspace({ phase: "unavailable" });
          return;
        }
        setManagement(available);
        setSkillOperationBackendScope(operationScope);
        const availableProfiles = await profileClient.listProfiles({ signal: controller.signal });
        if (controller.signal.aborted) return;
        setProfiles(availableProfiles);
        setProfileId((current) => (
          current && availableProfiles.some((profile) => profile.id === current)
            ? current
            : availableProfiles.find((profile) => profile.isActive)?.id
              ?? availableProfiles[0]?.id
              ?? null
        ));
        setWorkspace({ phase: "ready" });
      } catch (error) {
        if (controller.signal.aborted || isAbortError(error)) return;
        if (error instanceof DesktopConnectionError && error.kind === "desktop_unavailable") {
          setWorkspace({ phase: "desktop-required" });
        } else {
          setWorkspace({
            phase: "error",
            message: errorMessage(error, "无法加载工具与 Skills 服务。"),
          });
        }
      }
    })();
    return () => controller.abort();
  }, [profileClient, workspaceEpoch]);

  useEffect(() => {
    if (workspace.phase !== "ready" || !profileId || !management.toolsets) {
      setToolsets({ phase: "idle" });
      return undefined;
    }
    const controller = new AbortController();
    setToolsets({ phase: "loading" });
    setActionError(null);
    void client.listToolsets(profileId, { signal: controller.signal })
      .then((resource) => {
        if (!controller.signal.aborted) setToolsets({ phase: "ready", resource });
      })
      .catch((error: unknown) => {
        if (!controller.signal.aborted && !isAbortError(error)) {
          setToolsets({
            phase: "error",
            message: errorMessage(error, "无法加载工具列表。"),
          });
        }
      });
    return () => controller.abort();
  }, [client, management.toolsets, profileId, resourceEpoch, workspace.phase]);

  useEffect(() => {
    paginationController.current?.abort();
    paginationController.current = null;
    if (workspace.phase !== "ready" || !profileId || !management.skillDiscovery) {
      setSkills({ phase: "idle" });
      return undefined;
    }
    const controller = new AbortController();
    setSkills({ phase: "loading" });
    setSkillActionError(null);
    void skillsClient.listSkills(
      profileId,
      {
        ...(skillQuery ? { query: skillQuery } : {}),
        limit: SKILL_PAGE_LIMIT,
      },
      { signal: controller.signal },
    )
      .then((resource) => {
        if (!controller.signal.aborted) {
          setSkills({
            phase: "ready",
            resource,
            loadingMore: false,
            loadMoreError: null,
          });
        }
      })
      .catch((error: unknown) => {
        if (!controller.signal.aborted && !isAbortError(error)) {
          setSkills({
            phase: "error",
            message: errorMessage(error, "无法加载 Skills 列表。"),
          });
        }
      });
    return () => {
      controller.abort();
      paginationController.current?.abort();
      paginationController.current = null;
    };
  }, [
    management.skillDiscovery,
    profileId,
    skillEpoch,
    skillQuery,
    skillsClient,
    workspace.phase,
  ]);

  const toggleToolset = async (toolset: Toolset) => {
    if (
      !profileId
      || busyId
      || busySkillId
      || skillManagementAction
      || webMutationBusy
      || toolsets.phase !== "ready"
    ) return;
    const targetProfileId = profileId;
    const targetEtag = toolsets.resource.etag;
    setBusyId(toolset.id);
    setActionError(null);
    try {
      const updated = await client.updateToolset(
        targetProfileId,
        toolset.id,
        { enabled: !toolset.enabled },
        targetEtag,
      );
      setToolsets((current) => current.phase === "ready"
        ? {
          phase: "ready",
          resource: {
            etag: updated.etag,
            value: current.resource.value.map((item) => (
              item.id === updated.value.id ? updated.value : item
            )),
          },
        }
        : current);
    } catch (error) {
      if (error instanceof ToolsetApiError && error.status === 409) {
        setActionError("工具配置已在其他窗口更新，已重新加载最新状态，请确认后重试。");
        try {
          const refreshed = await client.listToolsets(targetProfileId);
          setToolsets({ phase: "ready", resource: refreshed });
        } catch (refreshError) {
          setToolsets({
            phase: "error",
            message: errorMessage(refreshError, "配置冲突后无法重新加载工具列表。"),
          });
        }
      } else {
        setActionError(errorMessage(error, "无法更新工具状态。"));
      }
    } finally {
      setBusyId(null);
    }
  };

  const submitSkillSearch = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (!profileId || busyId || busySkillId || skillManagementAction || webMutationBusy) return;
    paginationController.current?.abort();
    const nextQuery = skillSearchInput.trim();
    setSkillSearchInput(nextQuery);
    setSkillQuery(nextQuery);
    setSkillEpoch((value) => value + 1);
  };

  const clearSkillSearch = () => {
    if (busyId || busySkillId || skillManagementAction || webMutationBusy) return;
    paginationController.current?.abort();
    setSkillSearchInput("");
    setSkillQuery("");
    setSkillEpoch((value) => value + 1);
  };

  const toggleSkill = async (skill: Skill) => {
    if (
      !profileId
      || !management.skillEnablement
      || busyId
      || busySkillId
      || skillManagementAction
      || webMutationBusy
      || skills.phase !== "ready"
      || skills.loadingMore
    ) return;
    const targetProfileId = profileId;
    const targetQuery = skillQuery;
    const targetEtag = skills.resource.etag;
    setBusySkillId(skill.id);
    setSkillActionError(null);
    try {
      const updated = await skillsClient.updateSkill(
        targetProfileId,
        skill.id,
        !skill.enabled,
        targetEtag,
      );
      setSkills((current) => current.phase === "ready"
        ? {
          ...current,
          resource: {
            etag: updated.etag,
            value: {
              ...current.resource.value,
              items: current.resource.value.items.map((item) => (
                item.id === updated.value.id ? updated.value : item
              )),
            },
          },
        }
        : current);
    } catch (error) {
      if (error instanceof SkillApiError && error.status === 409) {
        setSkillActionError(
          "Skills 状态已在其他窗口更新，已重新加载最新目录，请确认后重试。",
        );
        try {
          const refreshed = await skillsClient.listSkills(targetProfileId, {
            ...(targetQuery ? { query: targetQuery } : {}),
            limit: SKILL_PAGE_LIMIT,
          });
          setSkills({
            phase: "ready",
            resource: refreshed,
            loadingMore: false,
            loadMoreError: null,
          });
        } catch (refreshError) {
          setSkills({
            phase: "error",
            message: errorMessage(refreshError, "配置冲突后无法重新加载 Skills 目录。"),
          });
        }
      } else {
        setSkillActionError(errorMessage(error, "无法更新 Skill 状态。"));
      }
    } finally {
      setBusySkillId(null);
    }
  };

  const loadMoreSkills = async () => {
    if (
      !profileId
      || busyId
      || busySkillId
      || skillManagementAction
      || webMutationBusy
      || skills.phase !== "ready"
      || skills.loadingMore
      || !skills.resource.value.nextCursor
    ) return;
    const targetProfileId = profileId;
    const targetQuery = skillQuery;
    const targetEtag = skills.resource.etag;
    const cursor = skills.resource.value.nextCursor;
    const controller = new AbortController();
    paginationController.current?.abort();
    paginationController.current = controller;
    setSkills((current) => current.phase === "ready"
      ? { ...current, loadingMore: true, loadMoreError: null }
      : current);
    try {
      const next = await skillsClient.listSkills(
        targetProfileId,
        {
          ...(targetQuery ? { query: targetQuery } : {}),
          cursor,
          limit: SKILL_PAGE_LIMIT,
        },
        { signal: controller.signal },
      );
      if (controller.signal.aborted) return;
      if (next.etag !== targetEtag) {
        const refreshed = await skillsClient.listSkills(
          targetProfileId,
          {
            ...(targetQuery ? { query: targetQuery } : {}),
            limit: SKILL_PAGE_LIMIT,
          },
          { signal: controller.signal },
        );
        if (controller.signal.aborted) return;
        setSkillActionError("Skills 目录在分页期间发生变化，已重新加载最新状态。");
        setSkills({
          phase: "ready",
          resource: refreshed,
          loadingMore: false,
          loadMoreError: null,
        });
        return;
      }
      setSkills((current) => {
        if (current.phase !== "ready" || current.resource.etag !== targetEtag) return current;
        const knownIds = new Set(current.resource.value.items.map((item) => item.id));
        return {
          phase: "ready",
          resource: {
            etag: next.etag,
            value: {
              items: [
                ...current.resource.value.items,
                ...next.value.items.filter((item) => !knownIds.has(item.id)),
              ],
              nextCursor: next.value.nextCursor,
            },
          },
          loadingMore: false,
          loadMoreError: null,
        };
      });
    } catch (error) {
      if (!controller.signal.aborted && !isAbortError(error)) {
        setSkills((current) => current.phase === "ready"
          ? {
            ...current,
            loadingMore: false,
            loadMoreError: errorMessage(error, "无法加载更多 Skills。"),
          }
          : current);
      }
    } finally {
      if (paginationController.current === controller) paginationController.current = null;
    }
  };

  const refreshFirstSkillPage = useCallback(async (
    targetProfileId: string,
    targetQuery: string,
    signal: AbortSignal,
  ): Promise<void> => {
    const refreshed = await skillsClient.listSkills(
      targetProfileId,
      {
        ...(targetQuery ? { query: targetQuery } : {}),
        limit: SKILL_PAGE_LIMIT,
      },
      { signal },
    );
    if (signal.aborted) return;
    setSkills({
      phase: "ready",
      resource: refreshed,
      loadingMore: false,
      loadMoreError: null,
    });
  }, [skillsClient]);

  const monitorSkillManagementOperation = useCallback(async (
    action: SkillManagementAction,
    start: (signal: AbortSignal) => Promise<Operation>,
    expectedId: string | null,
    storageKey: string,
    targetProfileId: string,
    targetQuery: string,
    controller: AbortController,
    fallback: string,
    persistAccepted: boolean,
    onCompleted?: () => void,
  ): Promise<void> => {
    let operationCompleted = false;
    try {
      const initial = await start(controller.signal);
      assertTrackedOperation(initial, expectedId, expectedOperationKind(action));
      if (persistAccepted) persistPendingSkillOperation(storageKey, initial);
      const operation = await pollSkillOperation(
        initial,
        skillsClient,
        controller.signal,
        effectiveSkillOperationPolling,
      );
      if (controller.signal.aborted) return;
      clearPendingSkillOperation(storageKey, operation.id);
      if (operation.status !== "completed") {
        if (mounted.current) setSkillActionError(operationFailureMessage(operation));
        return;
      }
      operationCompleted = true;
      await refreshFirstSkillPage(targetProfileId, targetQuery, controller.signal);
      if (!controller.signal.aborted && mounted.current) onCompleted?.();
    } catch (error) {
      if (expectedId !== null && error instanceof SkillApiError && error.kind === "invalid_response") {
        clearPendingSkillOperation(storageKey, expectedId);
      }
      if (!controller.signal.aborted && !isAbortError(error) && mounted.current) {
        setSkillActionError(errorMessage(
          error,
          operationCompleted ? "Skill 操作已完成，但无法刷新列表。" : fallback,
        ));
      }
    } finally {
      if (operationController.current === controller) {
        operationController.current = null;
        if (mounted.current) setSkillManagementAction(null);
      }
    }
  }, [effectiveSkillOperationPolling, refreshFirstSkillPage, skillsClient]);

  const runSkillManagementOperation = async (
    action: SkillManagementAction,
    start: (signal: AbortSignal) => Promise<Operation>,
    fallback: string,
    onCompleted?: () => void,
  ): Promise<void> => {
    if (
      !profileId
      || !management.skillManagement
      || busyId
      || busySkillId
      || skillManagementAction
      || webMutationBusy
      || (management.skillDiscovery && skills.phase !== "ready")
      || (skills.phase === "ready" && skills.loadingMore)
    ) return;
    const targetProfileId = profileId;
    const targetQuery = skillQuery;
    const storageKey = skillOperationBackendScope
      ? operationStorageKey(skillOperationBackendScope, targetProfileId)
      : null;
    if (!storageKey) {
      setSkillActionError("Skill 操作恢复状态不可用（operation_recovery_unavailable）。");
      return;
    }
    const controller = new AbortController();
    operationController.current?.abort();
    operationController.current = controller;
    setSkillManagementAction(action);
    setSkillActionError(null);
    await monitorSkillManagementOperation(
      action,
      start,
      null,
      storageKey,
      targetProfileId,
      targetQuery,
      controller,
      fallback,
      true,
      onCompleted,
    );
  };

  useEffect(() => {
    if (
      workspace.phase !== "ready"
      || !management.skillManagement
      || !profileId
      || !skillOperationBackendScope
      || operationController.current
      || (management.skillDiscovery && skills.phase !== "ready")
    ) return undefined;
    const storageKey = operationStorageKey(skillOperationBackendScope, profileId);
    if (!storageKey) return undefined;
    const pending = readPendingSkillOperation(storageKey);
    if (!pending) return undefined;
    const attemptKey = `${storageKey}:${pending.id}`;
    if (recoveryAttempt.current === attemptKey) return undefined;
    recoveryAttempt.current = attemptKey;
    const controller = new AbortController();
    const action = operationAction(pending.kind);
    operationController.current = controller;
    setSkillManagementAction(action);
    setSkillActionError(null);
    void monitorSkillManagementOperation(
      action,
      (signal) => skillsClient.getOperation(pending.id, { signal }),
      pending.id,
      storageKey,
      profileId,
      skillQuery,
      controller,
      "无法恢复 Skill 操作。",
      false,
    );
    return () => {
      if (operationController.current === controller) controller.abort();
    };
  }, [
    management.skillDiscovery,
    management.skillManagement,
    monitorSkillManagementOperation,
    profileId,
    skillOperationBackendScope,
    skillQuery,
    skills.phase,
    skillsClient,
    workspace.phase,
  ]);

  const installSkill = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (!profileId || !management.skillManagement || skillManagementAction) return;
    const targetProfileId = profileId;
    const registryId = skillRegistryId.trim();
    const url = skillUrl.trim();
    let input: InstallSkillInput | null = null;
    if (skillInstallMode === "registry") {
      if (!registryId) return;
      input = { registryId };
    } else if (skillInstallMode === "url") {
      if (!url) return;
      input = { url };
    } else if (!skillFile) {
      return;
    }
    const selectedFile = skillInstallMode === "file" ? skillFile : null;
    const uploadKey = newIdempotencyKey("skill-file");
    const installKey = newIdempotencyKey("skill-install");
    setSkillCleanupError(null);
    void (async () => {
      let uploadedFileId: string | null = null;
      await runSkillManagementOperation(
        { kind: "install" },
        async (signal) => {
          let installInput: InstallSkillInput | null = input;
          if (selectedFile) {
            const uploaded = await filesClient.uploadFile(selectedFile, uploadKey, { signal });
            uploadedFileId = uploaded.id;
            installInput = { fileId: uploaded.id };
          }
          return skillsClient.installSkill(
            targetProfileId,
            installInput!,
            installKey,
            { signal },
          );
        },
        "无法安装 Skill。",
        () => {
          setSkillRegistryId("");
          setSkillUrl("");
          setSkillFile(null);
          if (skillFileInput.current) skillFileInput.current.value = "";
        },
      );
      if (uploadedFileId) {
        try {
          await filesClient.deleteFile(uploadedFileId);
        } catch (error) {
          if (mounted.current) setSkillCleanupError(cleanupFailureMessage(error));
        }
      }
    })();
  };

  const uninstallSkill = (skill: Skill) => {
    if (
      !profileId
      || !management.skillManagement
      || !skill.uninstallable
      || skillManagementAction
    ) return;
    if (!window.confirm(`确定卸载 Skill “${skill.name}” (${skill.id})？`)) return;
    const targetProfileId = profileId;
    const uninstallKey = newIdempotencyKey("skill-uninstall");
    void runSkillManagementOperation(
      { kind: "uninstall", skillId: skill.id },
      (signal) => skillsClient.uninstallSkill(
        targetProfileId,
        skill.id,
        uninstallKey,
        { signal },
      ),
      "无法卸载 Skill。",
    );
  };

  if (workspace.phase !== "ready") {
    return (
      <WorkspaceStateView
        onRetry={() => setWorkspaceEpoch((value) => value + 1)}
        state={workspace}
      />
    );
  }

  const enabledCount = toolsets.phase === "ready"
    ? toolsets.resource.value.filter((toolset) => toolset.enabled).length
    : 0;
  const nonMcpMutationBusy = busyId !== null
    || busySkillId !== null
    || skillManagementAction !== null
    || webMutationBusy;
  const mutationBusy = nonMcpMutationBusy || mcpMutationBusy;
  const skillInstallReady = skillInstallMode === "registry"
    ? skillRegistryId.trim().length > 0
    : skillInstallMode === "url"
      ? skillUrl.trim().length > 0
      : skillFile !== null;
  const skillManagementLocked = mutationBusy
    || (management.skillDiscovery && skills.phase !== "ready")
    || (skills.phase === "ready" && skills.loadingMore);

  return (
    <div className="tools-panel">
      <header className="tools-toolbar">
        <label>
          <span>Profile</span>
          <select
            aria-label="工具 Profile"
            disabled={profiles.length === 0 || mutationBusy}
            onChange={(event) => {
              paginationController.current?.abort();
              operationController.current?.abort();
              operationController.current = null;
              recoveryAttempt.current = null;
              setSkillManagementAction(null);
              setProfileId(event.target.value || null);
              if (management.skillDiscovery) setSkills({ phase: "loading" });
              setSkillSearchInput("");
              setSkillQuery("");
              setSkillActionError(null);
              setSkillCleanupError(null);
            }}
            value={profileId ?? ""}
          >
            {profiles.length === 0 ? <option value="">暂无 Profile</option> : null}
            {profiles.map((profile) => (
              <option key={profile.id} value={profile.id}>{profile.displayName}</option>
            ))}
          </select>
        </label>
        <div className="tools-summary" aria-live="polite">
          {management.toolsets ? (
            <>
              <strong>{toolsets.phase === "ready" ? toolsets.resource.value.length : 0}</strong>
              <span>个工具集</span>
              <strong>{enabledCount}</strong>
              <span>个已启用</span>
            </>
          ) : <span>Toolset 未启用</span>}
        </div>
      </header>

      {actionError ? <p className="tools-action-error" role="alert">{actionError}</p> : null}

      <section className="tools-catalog" aria-labelledby="toolset-catalog-title">
        <div className="tools-section-heading">
          <div>
            <span>DYNAMIC CATALOG</span>
            <h2 id="toolset-catalog-title">工具集</h2>
          </div>
        </div>

        {!management.toolsets ? (
          <div className="tools-inline-state">当前后端未启用 Toolset 管理能力。</div>
        ) : !profileId ? (
          <div className="tools-inline-state">没有可用的 Profile。</div>
        ) : toolsets.phase === "loading" || toolsets.phase === "idle" ? (
          <div className="tools-inline-state" role="status" aria-busy="true">
            <LoaderCircle aria-hidden="true" className="spin" size={20} />
            正在加载工具列表
          </div>
        ) : toolsets.phase === "error" ? (
          <div className="tools-inline-state is-error">
            <p role="alert">{toolsets.message}</p>
            <button
              className="tools-secondary-button"
              onClick={() => setResourceEpoch((value) => value + 1)}
              type="button"
            >
              <RefreshCw aria-hidden="true" size={15} />
              重新加载
            </button>
          </div>
        ) : toolsets.resource.value.length === 0 ? (
          <div className="tools-inline-state">当前 Profile 没有可用的工具集。</div>
        ) : (
          <ul className="toolset-list">
            {toolsets.resource.value.map((toolset) => (
              <ToolsetRow
                busy={busyId === toolset.id}
                disabled={mutationBusy}
                key={toolset.id}
                onToggle={() => void toggleToolset(toolset)}
                toolset={toolset}
              />
            ))}
          </ul>
        )}
      </section>

      <WebProviderPanel
        client={webClient}
        extractAvailable={management.webExtract}
        mutationLocked={busyId !== null || busySkillId !== null || skillManagementAction !== null || mcpMutationBusy}
        onMutationStateChange={setWebMutationBusy}
        profileClient={profileClient}
        profileId={profileId}
        searchAvailable={management.webSearch}
      />

      <McpServersPanel
        available={management.mcpManagement}
        client={mcpClient}
        mutationLocked={nonMcpMutationBusy}
        onMutationStateChange={setMcpMutationBusy}
        profileId={profileId}
        transportRuntime={{
          stdio: management.mcpStdio,
          streamableHttp: management.mcpStreamableHttp,
          sse: management.mcpSse,
        }}
      />

      <section className="skills-catalog" aria-labelledby="skills-catalog-title">
        <div className="tools-section-heading skills-section-heading">
          <div>
            <span>PROFILE SKILLS</span>
            <h2 id="skills-catalog-title">Skills</h2>
          </div>
          {management.skillDiscovery && skills.phase === "ready" ? (
            <div className="skills-count" aria-live="polite">
              <strong>{skills.resource.value.items.length}</strong>
              <span>已加载</span>
            </div>
          ) : null}
        </div>

        {management.skillManagement && profileId ? (
          <form
            aria-busy={skillManagementAction?.kind === "install" || undefined}
            aria-label="Skill 安装"
            className="skill-install"
            onSubmit={installSkill}
          >
            <div aria-label="Skill 安装来源" className="skill-install-modes" role="radiogroup">
              {(["registry", "url", "file"] as const).map((mode) => (
                <button
                  aria-checked={skillInstallMode === mode}
                  className={skillInstallMode === mode ? "is-active" : undefined}
                  disabled={skillManagementLocked}
                  key={mode}
                  onClick={() => setSkillInstallMode(mode)}
                  role="radio"
                  type="button"
                >
                  {mode === "registry" ? "Registry" : mode === "url" ? "URL" : "文件"}
                </button>
              ))}
            </div>

            {skillInstallMode === "registry" ? (
              <label className="skill-install-field" key="registry">
                <span>Registry ID</span>
                <input
                  aria-label="Registry Skill ID"
                  disabled={skillManagementLocked}
                  maxLength={2048}
                  onChange={(event) => setSkillRegistryId(event.target.value)}
                  required
                  type="text"
                  value={skillRegistryId}
                />
              </label>
            ) : skillInstallMode === "url" ? (
              <label className="skill-install-field" key="url">
                <span>URL</span>
                <input
                  aria-label="Skill URL"
                  disabled={skillManagementLocked}
                  maxLength={2048}
                  onChange={(event) => setSkillUrl(event.target.value)}
                  required
                  type="url"
                  value={skillUrl}
                />
              </label>
            ) : (
              <label className="skill-install-field skill-install-file" key="file">
                <span>文件</span>
                <input
                  aria-label="Skill 文件"
                  disabled={skillManagementLocked}
                  onChange={(event) => setSkillFile(event.target.files?.[0] ?? null)}
                  ref={skillFileInput}
                  type="file"
                />
              </label>
            )}

            <button
              className="tools-secondary-button skill-install-submit"
              disabled={skillManagementLocked || !skillInstallReady}
              type="submit"
            >
              {skillManagementAction?.kind === "install" ? (
                <LoaderCircle aria-hidden="true" className="spin" size={15} />
              ) : (
                <PackagePlus aria-hidden="true" size={15} />
              )}
              {skillManagementAction?.kind === "install" ? "安装中" : "安装"}
            </button>
          </form>
        ) : null}

        {skillActionError ? (
          <p className="tools-action-error skills-action-error" role="alert">
            {skillActionError}
          </p>
        ) : null}
        {skillCleanupError ? (
          <p className="tools-action-error skills-action-error" role="alert">
            {skillCleanupError}
          </p>
        ) : null}

        {!management.skillDiscovery ? (
          <div className="tools-inline-state">当前后端未启用 Skills 发现能力。</div>
        ) : !profileId ? (
          <div className="tools-inline-state">没有可用的 Profile。</div>
        ) : (
          <>
            <form
              aria-label="Skills 搜索"
              className="skills-search"
              onSubmit={submitSkillSearch}
              role="search"
            >
              <label className="skills-search-field">
                <span className="sr-only">搜索 Skills</span>
                <Search aria-hidden="true" size={15} />
                <input
                  disabled={mutationBusy}
                  maxLength={500}
                  onChange={(event) => setSkillSearchInput(event.target.value)}
                  placeholder="搜索名称、ID 或说明"
                  type="search"
                  value={skillSearchInput}
                />
              </label>
              {skillSearchInput || skillQuery ? (
                <button
                  aria-label="清除 Skills 搜索"
                  className="skills-icon-button"
                  disabled={mutationBusy}
                  onClick={clearSkillSearch}
                  title="清除搜索"
                  type="button"
                >
                  <X aria-hidden="true" size={15} />
                </button>
              ) : null}
              <button
                className="tools-secondary-button"
                disabled={mutationBusy}
                type="submit"
              >
                <Search aria-hidden="true" size={15} />
                搜索
              </button>
            </form>

            {skills.phase === "loading" || skills.phase === "idle" ? (
              <div className="tools-inline-state" role="status" aria-busy="true">
                <LoaderCircle aria-hidden="true" className="spin" size={20} />
                正在加载 Skills
              </div>
            ) : skills.phase === "error" ? (
              <div className="tools-inline-state is-error">
                <p role="alert">{skills.message}</p>
                <button
                  className="tools-secondary-button"
                  onClick={() => setSkillEpoch((value) => value + 1)}
                  type="button"
                >
                  <RefreshCw aria-hidden="true" size={15} />
                  重新加载 Skills
                </button>
              </div>
            ) : skills.resource.value.items.length === 0 ? (
              <div className="tools-inline-state">
                {skillQuery ? "没有匹配的 Skills。" : "当前 Profile 没有可用的 Skills。"}
              </div>
            ) : (
              <>
                <ul className="toolset-list skill-list">
                  {skills.resource.value.items.map((skill) => (
                    <SkillRow
                      busy={busySkillId === skill.id}
                      disabled={
                        !management.skillEnablement
                        || mutationBusy
                        || skills.loadingMore
                      }
                      enablementAvailable={management.skillEnablement}
                      key={skill.id}
                      onUninstall={management.skillManagement && skill.uninstallable
                        ? () => uninstallSkill(skill)
                        : undefined}
                      onToggle={() => void toggleSkill(skill)}
                      skill={skill}
                      uninstallBusy={
                        skillManagementAction?.kind === "uninstall"
                        && skillManagementAction.skillId === skill.id
                      }
                      uninstallDisabled={mutationBusy || skills.loadingMore}
                    />
                  ))}
                </ul>
                <div className="skills-pagination">
                  {skills.loadMoreError ? (
                    <p role="alert">{skills.loadMoreError}</p>
                  ) : null}
                  {skills.resource.value.nextCursor ? (
                    <button
                      className="tools-secondary-button"
                      disabled={mutationBusy || skills.loadingMore}
                      onClick={() => void loadMoreSkills()}
                      type="button"
                    >
                      {skills.loadingMore ? (
                        <LoaderCircle aria-hidden="true" className="spin" size={15} />
                      ) : null}
                      {skills.loadingMore ? "正在加载" : "加载更多"}
                    </button>
                  ) : (
                    <span>已加载全部</span>
                  )}
                </div>
              </>
            )}
          </>
        )}
      </section>

      <section className="tools-deferred" aria-labelledby="tools-deferred-title">
        <div className="tools-section-heading">
          <div>
            <span>CAPABILITY STATUS</span>
            <h2 id="tools-deferred-title">扩展能力</h2>
          </div>
        </div>
        <div className="tools-deferred-list">
          <div>
            <Code2 aria-hidden="true" size={17} />
            <strong>代码执行</strong>
            <span>{management.codeExecution ? "可用" : "不可用"}</span>
          </div>
          <div>
            <PanelsTopLeft aria-hidden="true" size={17} />
            <strong>
              {management.browserAutomation
                ? "Browser 自动化可用"
                : "Browser 自动化不可用"}
            </strong>
            <span>
              {management.browserCdp || management.browserDownloads
                ? "部分扩展可用"
                : "CDP 与下载未启用"}
            </span>
          </div>
        </div>
      </section>
    </div>
  );
}
