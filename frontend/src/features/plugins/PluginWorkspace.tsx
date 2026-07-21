import {
  useCallback,
  useEffect,
  useMemo,
  useState,
  type FormEvent,
} from "react";
import {
  Check,
  CircleSlash2,
  FolderPlus,
  LoaderCircle,
  PlugZap,
  Puzzle,
  RefreshCw,
  Search,
  Trash2,
  X,
} from "lucide-react";
import {
  PluginApiError,
  pluginsApi,
  type Plugin,
  type PluginsApi,
} from "../../api/plugins";
import "./plugins.css";

interface PluginWorkspaceProps {
  client?: PluginsApi;
}

type LoadState = "loading" | "ready" | "error";

function catalogError(error: unknown): string {
  if (!(error instanceof PluginApiError)) {
    return "插件目录连接失败。";
  }
  switch (error.code) {
    case "plugin_not_found":
      return "插件目录不存在或已经移除。";
    case "plugin_already_installed":
      return "该插件已经登记。";
    case "plugin_manifest_invalid":
      return "plugin.json 不符合本地清单契约。";
    case "plugin_catalog_limit":
      return "插件目录已达到数量限制。";
    case "plugin_catalog_unavailable":
      return "本地插件目录暂不可用。";
    case "revision_conflict":
      return "插件目录已发生变化，正在刷新。";
    default:
      return error.kind === "invalid_response"
        ? "插件服务返回了不兼容的数据。"
        : error.message || "插件操作失败。";
  }
}

function sortPlugins(items: Plugin[]): Plugin[] {
  return [...items].sort((left, right) => (
    left.name.localeCompare(right.name, "zh-CN") || left.id.localeCompare(right.id)
  ));
}

function matches(plugin: Plugin, query: string): boolean {
  if (!query) return true;
  return [
    plugin.id,
    plugin.name,
    plugin.author,
    plugin.description,
    ...plugin.providedTools,
    ...plugin.requiresEnv,
  ].some((value) => value.toLocaleLowerCase().includes(query));
}

function PluginState({
  failed,
  onRetry,
}: {
  failed: boolean;
  onRetry: () => void;
}) {
  const Icon = failed ? CircleSlash2 : LoaderCircle;
  return (
    <div
      aria-busy={failed ? undefined : true}
      className={failed ? "plugin-state is-error" : "plugin-state"}
      role="status"
    >
      <Icon aria-hidden="true" className={failed ? undefined : "spin"} size={25} />
      <strong>{failed ? "插件目录加载失败" : "正在加载插件目录"}</strong>
      {failed ? (
        <button className="plugin-secondary-button" onClick={onRetry} type="button">
          <RefreshCw aria-hidden="true" size={15} />
          重试
        </button>
      ) : null}
    </div>
  );
}

export function PluginWorkspace({ client = pluginsApi }: PluginWorkspaceProps) {
  const [loadState, setLoadState] = useState<LoadState>("loading");
  const [items, setItems] = useState<Plugin[]>([]);
  const [etag, setEtag] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [sourcePath, setSourcePath] = useState("");
  const [busyId, setBusyId] = useState<string | null>(null);
  const [removeId, setRemoveId] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [reload, setReload] = useState(0);

  const refresh = useCallback(() => {
    setReload((value) => value + 1);
  }, []);

  useEffect(() => {
    const controller = new AbortController();
    let active = true;
    setLoadState("loading");
    setActionError(null);
    void client.listPlugins({ signal: controller.signal }).then((result) => {
      if (!active) return;
      setItems(sortPlugins(result.value.items));
      setEtag(result.etag);
      setLoadState("ready");
    }).catch((error: unknown) => {
      if (!active || controller.signal.aborted) return;
      setActionError(catalogError(error));
      setLoadState("error");
    });
    return () => {
      active = false;
      controller.abort();
    };
  }, [client, reload]);

  const filtered = useMemo(() => {
    const normalized = query.trim().toLocaleLowerCase();
    return items.filter((plugin) => matches(plugin, normalized));
  }, [items, query]);

  const enabledCount = items.filter((plugin) => plugin.enabled).length;
  const mutating = busyId !== null;

  function fail(error: unknown): void {
    setMessage(null);
    setActionError(catalogError(error));
    if (error instanceof PluginApiError && error.code === "revision_conflict") {
      refresh();
    }
  }

  async function install(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    const path = sourcePath.trim();
    if (!path || mutating) return;
    setBusyId("$install");
    setActionError(null);
    setMessage(null);
    try {
      const result = await client.installPlugin({ sourcePath: path });
      setItems((current) => sortPlugins([
        ...current.filter((plugin) => plugin.id !== result.value.id),
        result.value,
      ]));
      setEtag(result.etag);
      setSourcePath("");
      setMessage(`已登记 ${result.value.name}，当前保持停用。`);
    } catch (error) {
      fail(error);
    } finally {
      setBusyId(null);
    }
  }

  async function toggle(plugin: Plugin): Promise<void> {
    if (!etag || mutating) return;
    setBusyId(plugin.id);
    setActionError(null);
    setMessage(null);
    try {
      const result = await client.updatePlugin(
        plugin.id,
        { enabled: !plugin.enabled },
        etag,
      );
      setItems((current) => current.map((item) => (
        item.id === plugin.id ? result.value : item
      )));
      setEtag(result.etag);
      setMessage(`${result.value.name} 已${result.value.enabled ? "启用" : "停用"}。`);
    } catch (error) {
      fail(error);
    } finally {
      setBusyId(null);
    }
  }

  async function remove(plugin: Plugin): Promise<void> {
    if (!etag || mutating) return;
    setBusyId(plugin.id);
    setActionError(null);
    setMessage(null);
    try {
      const result = await client.uninstallPlugin(plugin.id, etag);
      setItems((current) => current.filter((item) => item.id !== plugin.id));
      setEtag(result.etag);
      setRemoveId(null);
      setMessage(`已移除 ${plugin.name} 的登记。`);
    } catch (error) {
      fail(error);
    } finally {
      setBusyId(null);
    }
  }

  return (
    <section className="product-page plugin-workspace" aria-label="插件管理">
      <header className="plugin-toolbar">
        <div>
          <small>PLUGINS</small>
          <h2>插件管理</h2>
        </div>
        <div className="plugin-toolbar__actions">
          <label className="plugin-search">
            <Search aria-hidden="true" size={15} />
            <input
              aria-label="搜索插件"
              onChange={(event) => setQuery(event.target.value)}
              placeholder="搜索插件"
              type="search"
              value={query}
            />
          </label>
          <button
            aria-label="刷新插件目录"
            disabled={loadState === "loading" || mutating}
            onClick={refresh}
            title="刷新"
            type="button"
          >
            <RefreshCw aria-hidden="true" className={loadState === "loading" ? "spin" : undefined} size={16} />
          </button>
        </div>
      </header>

      <form className="plugin-install" onSubmit={(event) => void install(event)}>
        <span className="plugin-install__mark"><FolderPlus aria-hidden="true" size={18} /></span>
        <label>
          <span>本地插件目录</span>
          <input
            aria-label="本地插件目录"
            disabled={loadState !== "ready" || mutating}
            onChange={(event) => setSourcePath(event.target.value)}
            placeholder="local-tools"
            spellCheck={false}
            value={sourcePath}
          />
        </label>
        <button
          className="plugin-primary-button"
          disabled={loadState !== "ready" || !sourcePath.trim() || mutating}
          type="submit"
        >
          {busyId === "$install"
            ? <LoaderCircle aria-hidden="true" className="spin" size={16} />
            : <FolderPlus aria-hidden="true" size={16} />}
          登记
        </button>
      </form>

      {message ? <p className="plugin-message" role="status">{message}</p> : null}
      {actionError ? <p className="plugin-message is-error" role="alert">{actionError}</p> : null}

      {loadState !== "ready" ? (
        <PluginState failed={loadState === "error"} onRetry={refresh} />
      ) : (
        <div className="plugin-catalog">
          <div className="plugin-summary" aria-live="polite">
            <span>{items.length} 个插件</span>
            <span>{enabledCount} 个已启用</span>
            <span>本地清单</span>
          </div>

          {filtered.length === 0 ? (
            <div className="plugin-state is-empty" role="status">
              <Puzzle aria-hidden="true" size={25} />
              <strong>{items.length === 0 ? "暂无已登记插件" : "没有匹配的插件"}</strong>
            </div>
          ) : (
            <ul className="plugin-list">
              {filtered.map((plugin) => {
                const busy = busyId === plugin.id;
                const confirming = removeId === plugin.id;
                return (
                  <li aria-busy={busy || undefined} className="plugin-row" key={plugin.id}>
                    <span className="plugin-row__mark" aria-hidden="true"><PlugZap size={17} /></span>
                    <div className="plugin-row__main">
                      <header>
                        <strong>{plugin.name}</strong>
                        <code>{plugin.id}</code>
                        <span>v{plugin.version}</span>
                      </header>
                      <p>{plugin.description || "暂无说明"}</p>
                      <div className="plugin-meta">
                        <span>{plugin.author}</span>
                        <span>{plugin.providedTools.length} 个工具</span>
                        <span>{plugin.requiresEnv.length} 个环境变量</span>
                      </div>
                      {plugin.providedTools.length > 0 || plugin.requiresEnv.length > 0 ? (
                        <details>
                          <summary>清单详情</summary>
                          {plugin.providedTools.length > 0 ? (
                            <div><strong>工具</strong>{plugin.providedTools.map((tool) => <code key={tool}>{tool}</code>)}</div>
                          ) : null}
                          {plugin.requiresEnv.length > 0 ? (
                            <div><strong>环境变量</strong>{plugin.requiresEnv.map((name) => <code key={name}>{name}</code>)}</div>
                          ) : null}
                        </details>
                      ) : null}
                    </div>

                    <div className="plugin-row__actions">
                      <div className="plugin-enable-control">
                        {busy && !confirming ? <LoaderCircle aria-hidden="true" className="spin" size={15} /> : null}
                        <label className="plugin-switch">
                          <input
                            aria-label={plugin.enabled ? `停用插件 ${plugin.name}` : `启用插件 ${plugin.name}`}
                            checked={plugin.enabled}
                            disabled={mutating}
                            onChange={() => void toggle(plugin)}
                            role="switch"
                            type="checkbox"
                          />
                          <span aria-hidden="true" />
                        </label>
                        <small>{plugin.enabled ? "已启用" : "已停用"}</small>
                      </div>

                      {confirming ? (
                        <div className="plugin-remove-confirm" role="group" aria-label={`确认移除 ${plugin.name}`}>
                          <span>移除登记？</span>
                          <button
                            aria-label={`确认移除插件 ${plugin.name}`}
                            disabled={mutating}
                            onClick={() => void remove(plugin)}
                            title="确认移除"
                            type="button"
                          >
                            {busy
                              ? <LoaderCircle aria-hidden="true" className="spin" size={14} />
                              : <Check aria-hidden="true" size={14} />}
                          </button>
                          <button
                            aria-label={`取消移除插件 ${plugin.name}`}
                            disabled={mutating}
                            onClick={() => setRemoveId(null)}
                            title="取消"
                            type="button"
                          >
                            <X aria-hidden="true" size={14} />
                          </button>
                        </div>
                      ) : (
                        <button
                          aria-label={`移除插件 ${plugin.name}`}
                          className="plugin-remove-button"
                          disabled={mutating}
                          onClick={() => setRemoveId(plugin.id)}
                          title="移除登记"
                          type="button"
                        >
                          <Trash2 aria-hidden="true" size={15} />
                        </button>
                      )}
                    </div>
                  </li>
                );
              })}
            </ul>
          )}
        </div>
      )}
    </section>
  );
}
