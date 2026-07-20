import {
  AlertTriangle,
  CircleSlash2,
  Edit3,
  KeyRound,
  LoaderCircle,
  Network,
  Plus,
  RefreshCw,
  Save,
  Server,
  SquareTerminal,
  Trash2,
  X,
} from "lucide-react";
import { useEffect, useRef, useState, type FormEvent } from "react";
import { DesktopConnectionError } from "../../api/desktopConnection";
import {
  McpApiError,
  mcpApi,
  type CreateMcpServerInput,
  type McpApi,
  type McpServer,
  type McpServerPatch,
  type McpTransport,
  type VersionedMcpServers,
} from "../../api/mcp";

interface McpServersPanelProps {
  available: boolean;
  client?: McpApi;
  mutationLocked?: boolean;
  onMutationStateChange?: (busy: boolean) => void;
  profileId: string | null;
  transportRuntime?: Readonly<Record<McpTransport, boolean>>;
}

type ResourceState =
  | { phase: "idle" }
  | { phase: "loading" }
  | { phase: "error"; message: string }
  | { phase: "ready"; resource: VersionedMcpServers };

interface ServerDraft {
  name: string;
  transport: McpTransport;
  command: string;
  argsText: string;
  url: string;
  enabled: boolean;
  timeoutSeconds: string;
  secretNamesText: string;
}

const EMPTY_DRAFT: ServerDraft = {
  name: "",
  transport: "stdio",
  command: "",
  argsText: "",
  url: "",
  enabled: true,
  timeoutSeconds: "30",
  secretNamesText: "",
};

const TRANSPORT_LABELS: Record<McpTransport, string> = {
  stdio: "Standard I/O",
  streamableHttp: "Streamable HTTP",
  sse: "SSE",
};

const NO_TRANSPORT_RUNTIME: Readonly<Record<McpTransport, boolean>> = {
  stdio: false,
  streamableHttp: false,
  sse: false,
};

function newIdempotencyKey(): string {
  if (typeof globalThis.crypto?.randomUUID === "function") {
    return `mcp-create-${globalThis.crypto.randomUUID()}`;
  }
  return `mcp-create-${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
}

function errorMessage(error: unknown, fallback: string): string {
  if (error instanceof McpApiError) {
    return error.requestId ? `${error.message} 请求 ID：${error.requestId}` : error.message;
  }
  if (error instanceof DesktopConnectionError) {
    return error.kind === "desktop_unavailable"
      ? "MCP 配置需要在 SynthChat Desktop 中使用。"
      : "本地 Rust 后端无法连接。";
  }
  return fallback;
}

function splitLines(value: string): string[] {
  return value
    .split(/\r?\n/u)
    .map((item) => item.trim())
    .filter(Boolean);
}

function splitSecretNames(value: string): string[] {
  return value
    .split(/[\s,]+/u)
    .map((item) => item.trim())
    .filter(Boolean);
}

function inputFromDraft(draft: ServerDraft): CreateMcpServerInput {
  const timeoutSeconds = Number(draft.timeoutSeconds);
  if (draft.transport === "stdio") {
    return {
      name: draft.name.trim(),
      transport: "stdio",
      command: draft.command.trim(),
      args: splitLines(draft.argsText),
      enabled: draft.enabled,
      timeoutSeconds,
      envSecretNames: splitSecretNames(draft.secretNamesText),
    };
  }
  const bearerTokenSecretName = draft.secretNamesText.trim();
  return {
    name: draft.name.trim(),
    transport: draft.transport,
    url: draft.url.trim(),
    enabled: draft.enabled,
    timeoutSeconds,
    ...(bearerTokenSecretName ? { bearerTokenSecretName } : {}),
  } as CreateMcpServerInput;
}

function draftFromServer(server: McpServer): ServerDraft {
  return {
    name: server.name,
    transport: server.transport,
    command: server.command ?? "",
    argsText: server.args.join("\n"),
    url: server.url ?? "",
    enabled: server.enabled,
    timeoutSeconds: String(server.timeoutSeconds),
    secretNamesText: server.transport === "stdio"
      ? server.envSecretNames.join("\n")
      : server.bearerTokenSecretName ?? "",
  };
}

function patchFromDraft(draft: ServerDraft): McpServerPatch {
  const common: McpServerPatch = {
    name: draft.name.trim(),
    transport: draft.transport,
    enabled: draft.enabled,
    timeoutSeconds: Number(draft.timeoutSeconds),
  };
  if (draft.transport === "stdio") {
    return {
      ...common,
      command: draft.command.trim(),
      args: splitLines(draft.argsText),
      envSecretNames: splitSecretNames(draft.secretNamesText),
    };
  }
  return {
    ...common,
    url: draft.url.trim(),
    bearerTokenSecretName: draft.secretNamesText.trim() || null,
  };
}

function sortServers(servers: McpServer[]): McpServer[] {
  return [...servers].sort((left, right) => left.name.localeCompare(right.name));
}

function ServerFields({
  busy,
  draft,
  idPrefix,
  onChange,
  transportLocked = false,
}: {
  busy: boolean;
  draft: ServerDraft;
  idPrefix: string;
  onChange: (draft: ServerDraft) => void;
  transportLocked?: boolean;
}) {
  return (
    <div className="mcp-form-fields">
      <label htmlFor={`${idPrefix}-name`}>
        <span>名称</span>
        <input
          autoComplete="off"
          disabled={busy}
          id={`${idPrefix}-name`}
          maxLength={64}
          onChange={(event) => onChange({ ...draft, name: event.target.value })}
          pattern="[A-Za-z0-9][A-Za-z0-9_-]{0,63}"
          placeholder="local_tools"
          required
          value={draft.name}
        />
      </label>
      <label htmlFor={`${idPrefix}-transport`}>
        <span>Transport</span>
        <select
          disabled={busy || transportLocked}
          id={`${idPrefix}-transport`}
          onChange={(event) => onChange({
            ...draft,
            transport: event.target.value as McpTransport,
            command: "",
            argsText: "",
            url: "",
            secretNamesText: "",
          })}
          value={draft.transport}
        >
          <option value="stdio">Standard I/O</option>
          <option value="streamableHttp">Streamable HTTP</option>
          <option value="sse">SSE</option>
        </select>
      </label>

      {draft.transport === "stdio" ? (
        <>
          <label htmlFor={`${idPrefix}-command`}>
            <span>Executable</span>
            <input
              autoComplete="off"
              disabled={busy}
              id={`${idPrefix}-command`}
              maxLength={1024}
              onChange={(event) => onChange({ ...draft, command: event.target.value })}
              placeholder="npx"
              required
              value={draft.command}
            />
          </label>
          <label className="mcp-field-wide" htmlFor={`${idPrefix}-args`}>
            <span>Arguments（每行一个）</span>
            <textarea
              autoComplete="off"
              disabled={busy}
              id={`${idPrefix}-args`}
              onChange={(event) => onChange({ ...draft, argsText: event.target.value })}
              placeholder={"-y\n@example/mcp"}
              spellCheck={false}
              value={draft.argsText}
            />
          </label>
          <label className="mcp-field-wide" htmlFor={`${idPrefix}-secret-names`}>
            <span>环境变量密钥链名称引用（每行一个）</span>
            <textarea
              autoComplete="off"
              disabled={busy}
              id={`${idPrefix}-secret-names`}
              onChange={(event) => onChange({ ...draft, secretNamesText: event.target.value })}
              placeholder="MCP_TOKEN"
              spellCheck={false}
              value={draft.secretNamesText}
            />
          </label>
        </>
      ) : (
        <>
          <label className="mcp-field-wide" htmlFor={`${idPrefix}-url`}>
            <span>URL</span>
            <input
              autoComplete="off"
              disabled={busy}
              id={`${idPrefix}-url`}
              maxLength={2048}
              onChange={(event) => onChange({ ...draft, url: event.target.value })}
              placeholder="https://mcp.example.com/rpc"
              required
              type="url"
              value={draft.url}
            />
          </label>
          <label className="mcp-field-wide" htmlFor={`${idPrefix}-bearer-secret-name`}>
            <span>Bearer 密钥链名称引用（可选）</span>
            <input
              autoComplete="off"
              disabled={busy}
              id={`${idPrefix}-bearer-secret-name`}
              maxLength={128}
              onChange={(event) => onChange({ ...draft, secretNamesText: event.target.value })}
              pattern="[A-Z][A-Z0-9_]{0,127}"
              placeholder="MCP_BEARER_TOKEN"
              spellCheck={false}
              value={draft.secretNamesText}
            />
          </label>
        </>
      )}

      <label htmlFor={`${idPrefix}-timeout`}>
        <span>超时（秒）</span>
        <input
          disabled={busy}
          id={`${idPrefix}-timeout`}
          max={600}
          min={1}
          onChange={(event) => onChange({ ...draft, timeoutSeconds: event.target.value })}
          required
          type="number"
          value={draft.timeoutSeconds}
        />
      </label>
      <label className="mcp-checkbox-field" htmlFor={`${idPrefix}-enabled`}>
        <input
          checked={draft.enabled}
          disabled={busy}
          id={`${idPrefix}-enabled`}
          onChange={(event) => onChange({ ...draft, enabled: event.target.checked })}
          type="checkbox"
        />
        <span>启用配置</span>
      </label>
    </div>
  );
}

function MissingSecrets({ server }: { server: McpServer }) {
  if (server.missingSecretNames.length === 0) {
    return <span className="mcp-readiness is-ready">密钥引用就绪</span>;
  }
  return (
    <div className="mcp-missing-secrets" role="status">
      <AlertTriangle aria-hidden="true" size={14} />
      <span>密钥链缺少</span>
      {server.missingSecretNames.map((name) => <code key={name}>{name}</code>)}
    </div>
  );
}

export function McpServersPanel({
  available,
  client = mcpApi,
  mutationLocked = false,
  onMutationStateChange,
  profileId,
  transportRuntime = NO_TRANSPORT_RUNTIME,
}: McpServersPanelProps) {
  const [resource, setResource] = useState<ResourceState>({ phase: "idle" });
  const [resourceEpoch, setResourceEpoch] = useState(0);
  const [showCreate, setShowCreate] = useState(false);
  const [createDraft, setCreateDraft] = useState<ServerDraft>(EMPTY_DRAFT);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editDraft, setEditDraft] = useState<ServerDraft>(EMPTY_DRAFT);
  const [deleteArmedId, setDeleteArmedId] = useState<string | null>(null);
  const [busyAction, setBusyAction] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const createAttempt = useRef<{ fingerprint: string; key: string } | null>(null);

  useEffect(() => {
    onMutationStateChange?.(busyAction !== null);
  }, [busyAction, onMutationStateChange]);

  useEffect(() => () => onMutationStateChange?.(false), [onMutationStateChange]);

  useEffect(() => {
    setEditingId(null);
    setDeleteArmedId(null);
    setShowCreate(false);
    setCreateDraft(EMPTY_DRAFT);
    setActionError(null);
    createAttempt.current = null;
    if (!available || !profileId) {
      setResource({ phase: "idle" });
      return undefined;
    }
    const controller = new AbortController();
    setResource({ phase: "loading" });
    void client.listServers(profileId, { signal: controller.signal })
      .then((next) => {
        if (!controller.signal.aborted) {
          setResource({ phase: "ready", resource: { ...next, value: sortServers(next.value) } });
        }
      })
      .catch((error: unknown) => {
        if (!controller.signal.aborted) {
          setResource({ phase: "error", message: errorMessage(error, "无法加载 MCP server。") });
        }
      });
    return () => controller.abort();
  }, [available, client, profileId, resourceEpoch]);

  const reloadAfterConflict = async (targetProfileId: string) => {
    try {
      const latest = await client.listServers(targetProfileId);
      setResource({ phase: "ready", resource: { ...latest, value: sortServers(latest.value) } });
      setActionError("Profile 配置已变化，已重新加载 MCP server 最新状态。");
    } catch (reloadError) {
      setResource({
        phase: "error",
        message: errorMessage(reloadError, "配置已变化，但重新加载 MCP server 失败。"),
      });
    }
  };

  const handleMutationError = async (error: unknown, targetProfileId: string, fallback: string) => {
    if (error instanceof McpApiError && error.status === 409) {
      await reloadAfterConflict(targetProfileId);
      return;
    }
    setActionError(errorMessage(error, fallback));
  };

  const submitCreate = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (!profileId || resource.phase !== "ready" || mutationLocked || busyAction) return;
    const targetProfileId = profileId;
    const input = inputFromDraft(createDraft);
    const fingerprint = JSON.stringify(input);
    if (!createAttempt.current || createAttempt.current.fingerprint !== fingerprint) {
      createAttempt.current = { fingerprint, key: newIdempotencyKey() };
    }
    setBusyAction("create");
    setActionError(null);
    try {
      const created = await client.createServer(
        targetProfileId,
        input,
        createAttempt.current.key,
      );
      setResource((current) => current.phase === "ready"
        ? {
          phase: "ready",
          resource: {
            value: sortServers([
              ...current.resource.value.filter((server) => server.id !== created.value.id),
              created.value,
            ]),
            etag: created.etag,
          },
        }
        : current);
      setCreateDraft(EMPTY_DRAFT);
      setShowCreate(false);
      createAttempt.current = null;
    } catch (error) {
      await handleMutationError(error, targetProfileId, "创建 MCP server 失败。");
    } finally {
      setBusyAction(null);
    }
  };

  const updateServer = async (server: McpServer, patch: McpServerPatch, action: string) => {
    if (!profileId || resource.phase !== "ready" || mutationLocked || busyAction) return;
    const targetProfileId = profileId;
    const targetEtag = resource.resource.etag;
    setBusyAction(action);
    setActionError(null);
    try {
      const updated = await client.updateServer(
        targetProfileId,
        server.id,
        patch,
        targetEtag,
      );
      setResource((current) => current.phase === "ready"
        ? {
          phase: "ready",
          resource: {
            value: sortServers(current.resource.value.map((item) => (
              item.id === updated.value.id ? updated.value : item
            ))),
            etag: updated.etag,
          },
        }
        : current);
      setEditingId(null);
    } catch (error) {
      await handleMutationError(error, targetProfileId, "更新 MCP server 失败。");
    } finally {
      setBusyAction(null);
    }
  };

  const deleteServer = async (server: McpServer) => {
    if (!profileId || resource.phase !== "ready" || mutationLocked || busyAction) return;
    const targetProfileId = profileId;
    const targetEtag = resource.resource.etag;
    setBusyAction(`delete:${server.id}`);
    setActionError(null);
    try {
      const deleted = await client.deleteServer(targetProfileId, server.id, targetEtag);
      setResource((current) => current.phase === "ready"
        ? {
          phase: "ready",
          resource: {
            value: current.resource.value.filter((item) => item.id !== server.id),
            etag: deleted.etag,
          },
        }
        : current);
      setDeleteArmedId(null);
      if (editingId === server.id) setEditingId(null);
    } catch (error) {
      await handleMutationError(error, targetProfileId, "删除 MCP server 失败。");
    } finally {
      setBusyAction(null);
    }
  };

  const busy = mutationLocked || busyAction !== null;

  return (
    <section className="mcp-servers-panel" aria-labelledby="mcp-servers-title">
      <div className="tools-section-heading mcp-section-heading">
        <div>
          <span>MODEL CONTEXT PROTOCOL</span>
          <h2 id="mcp-servers-title">MCP Servers</h2>
        </div>
        {available && resource.phase === "ready" ? (
          <div className="mcp-heading-actions">
            <span className="mcp-server-count">{resource.resource.value.length}</span>
            <button
              aria-label={showCreate ? "关闭创建 MCP server" : "创建 MCP server"}
              className="tools-secondary-button"
              disabled={busy || !profileId}
              onClick={() => {
                setShowCreate((value) => !value);
                setActionError(null);
              }}
              type="button"
            >
              {showCreate ? <X aria-hidden="true" size={15} /> : <Plus aria-hidden="true" size={15} />}
              {showCreate ? "关闭" : "添加"}
            </button>
          </div>
        ) : null}
      </div>

      {!available ? (
        <div className="tools-inline-state" data-capability-state="disabled">
          <CircleSlash2 aria-hidden="true" size={20} />
          当前后端未启用 MCP 管理能力。
        </div>
      ) : !profileId ? (
        <div className="tools-inline-state">没有可用的 Profile。</div>
      ) : resource.phase === "loading" || resource.phase === "idle" ? (
        <div className="tools-inline-state" role="status" aria-busy="true">
          <LoaderCircle aria-hidden="true" className="spin" size={20} />
          正在加载 MCP servers
        </div>
      ) : resource.phase === "error" ? (
        <div className="tools-inline-state is-error">
          <p role="alert">{resource.message}</p>
          <button className="tools-secondary-button" onClick={() => setResourceEpoch((value) => value + 1)} type="button">
            <RefreshCw aria-hidden="true" size={15} />
            重新加载
          </button>
        </div>
      ) : (
        <>
          {showCreate ? (
            <form aria-label="创建 MCP server" className="mcp-editor" onSubmit={submitCreate}>
              <ServerFields
                busy={busy}
                draft={createDraft}
                idPrefix="mcp-create"
                onChange={(next) => {
                  setCreateDraft(next);
                  createAttempt.current = null;
                }}
              />
              <div className="mcp-editor-actions">
                <button className="tools-secondary-button" disabled={busy} type="submit">
                  {busyAction === "create"
                    ? <LoaderCircle aria-hidden="true" className="spin" size={15} />
                    : <Plus aria-hidden="true" size={15} />}
                  {busyAction === "create" ? "创建中" : "创建"}
                </button>
              </div>
            </form>
          ) : null}

          {actionError ? <p className="tools-action-error mcp-action-error" role="alert">{actionError}</p> : null}

          {resource.resource.value.length === 0 ? (
            <div className="tools-inline-state">
              <Server aria-hidden="true" size={20} />
              当前 Profile 没有 MCP server。
            </div>
          ) : (
            <div className="mcp-server-list">
              {resource.resource.value.map((server) => {
                const editing = editingId === server.id;
                const rowBusy = busyAction?.endsWith(server.id) ?? false;
                return (
                  <article aria-busy={rowBusy || undefined} className="mcp-server-row" key={server.id}>
                    <div className="mcp-server-summary">
                      <span className="mcp-transport-icon">
                        {server.transport === "stdio"
                          ? <SquareTerminal aria-hidden="true" size={17} />
                          : <Network aria-hidden="true" size={17} />}
                      </span>
                      <div className="mcp-server-copy">
                        <div>
                          <strong>{server.name}</strong>
                          <span className="mcp-transport-badge">{TRANSPORT_LABELS[server.transport]}</span>
                        </div>
                        <code>{server.id}</code>
                        <small>{server.command ?? server.url}</small>
                      </div>
                      <div className="mcp-server-controls">
                        <span className="mcp-row-spinner">
                          {rowBusy ? <LoaderCircle aria-hidden="true" className="spin" size={15} /> : null}
                        </span>
                        <label className="toolset-switch">
                          <input
                            aria-label={server.enabled
                              ? `停用 MCP server ${server.name}`
                              : `启用 MCP server ${server.name}`}
                            checked={server.enabled}
                            disabled={busy}
                            onChange={() => void updateServer(
                              server,
                              { enabled: !server.enabled },
                              `toggle:${server.id}`,
                            )}
                            role="switch"
                            type="checkbox"
                          />
                          <span aria-hidden="true" />
                        </label>
                        <button
                          aria-label={`编辑 MCP server ${server.name}`}
                          className="skills-icon-button"
                          disabled={busy}
                          onClick={() => {
                            if (editing) {
                              setEditingId(null);
                            } else {
                              setEditingId(server.id);
                              setEditDraft(draftFromServer(server));
                              setDeleteArmedId(null);
                            }
                          }}
                          title={editing ? "关闭编辑" : "编辑"}
                          type="button"
                        >
                          {editing ? <X aria-hidden="true" size={15} /> : <Edit3 aria-hidden="true" size={15} />}
                        </button>
                        <button
                          aria-label={deleteArmedId === server.id
                            ? `确认删除 MCP server ${server.name}`
                            : `删除 MCP server ${server.name}`}
                          className="skills-icon-button skill-uninstall-button"
                          disabled={busy}
                          onClick={() => {
                            if (deleteArmedId === server.id) void deleteServer(server);
                            else {
                              setDeleteArmedId(server.id);
                              setEditingId(null);
                            }
                          }}
                          title={deleteArmedId === server.id ? "确认删除" : "删除"}
                          type="button"
                        >
                          <Trash2 aria-hidden="true" size={15} />
                        </button>
                      </div>
                    </div>
                    <div className="mcp-server-status">
                      <span className={transportRuntime[server.transport] ? "mcp-readiness is-ready" : "mcp-readiness"}>
                        {transportRuntime[server.transport]
                          ? "运行时可用"
                          : "配置已保存，运行时未启用"}
                      </span>
                      <span className={server.enabled ? "mcp-readiness is-ready" : "mcp-readiness"}>
                        {server.enabled ? "已启用" : "已停用"}
                      </span>
                      <span>{server.timeoutSeconds} 秒</span>
                      {server.envSecretNames.map((name) => (
                        <span className="mcp-secret-reference" key={name}><KeyRound size={12} />{name}</span>
                      ))}
                      {server.bearerTokenSecretName ? (
                        <span className="mcp-secret-reference"><KeyRound size={12} />{server.bearerTokenSecretName}</span>
                      ) : null}
                      <MissingSecrets server={server} />
                    </div>
                    {editing ? (
                      <form
                        aria-label={`编辑 MCP server ${server.name}`}
                        className="mcp-editor is-inline"
                        onSubmit={(event) => {
                          event.preventDefault();
                          void updateServer(server, patchFromDraft(editDraft), `edit:${server.id}`);
                        }}
                      >
                        <ServerFields
                          busy={busy}
                          draft={editDraft}
                          idPrefix={`mcp-edit-${server.id}`}
                          onChange={setEditDraft}
                          transportLocked
                        />
                        <div className="mcp-editor-actions">
                          <button className="tools-secondary-button" disabled={busy} type="submit">
                            <Save aria-hidden="true" size={15} />
                            保存
                          </button>
                        </div>
                      </form>
                    ) : null}
                  </article>
                );
              })}
            </div>
          )}
        </>
      )}
    </section>
  );
}
