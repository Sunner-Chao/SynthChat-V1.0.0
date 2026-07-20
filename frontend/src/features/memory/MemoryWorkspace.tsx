import {
  Brain,
  CircleSlash2,
  LoaderCircle,
  Pencil,
  Plus,
  RefreshCw,
  Save,
  Search,
  ShieldAlert,
  Trash2,
  X,
} from "lucide-react";
import {
  useEffect,
  useRef,
  useState,
  type FormEvent,
} from "react";
import { DesktopConnectionError } from "../../api/desktopConnection";
import {
  memoriesApi,
  MemoryApiError,
  type MemoriesApi,
  type Memory,
  type MemoryTarget,
  type VersionedMemoryPage,
} from "../../api/memories";
import {
  ProfileApiError,
  profilesApi,
  type ProfilesApi,
  type ProfileSummary,
} from "../../api/profiles";
import "./memory.css";

type ProfileClient = Pick<ProfilesApi, "getCapabilities" | "listProfiles">;

type WorkspaceState =
  | { phase: "loading" }
  | { phase: "desktop-required" }
  | { phase: "unavailable" }
  | { phase: "error"; message: string }
  | { phase: "ready" };

type PageState =
  | { phase: "idle" }
  | { phase: "loading" }
  | { phase: "error"; message: string }
  | {
    phase: "ready";
    resource: VersionedMemoryPage;
    loadingMore: boolean;
    loadMoreError: string | null;
  };

type EditorState =
  | { mode: "create"; content: string }
  | { mode: "edit"; memoryId: string; content: string };

const PAGE_LIMIT = 30;

export interface MemoryWorkspaceProps {
  client?: MemoriesApi;
  profileClient?: ProfileClient;
}

function isAbortError(error: unknown): boolean {
  return error instanceof DOMException && error.name === "AbortError";
}

function errorMessage(error: unknown, fallback: string): string {
  if (error instanceof MemoryApiError || error instanceof ProfileApiError) {
    return error.requestId ? `${error.message} 请求 ID：${error.requestId}` : error.message;
  }
  if (error instanceof DesktopConnectionError) {
    return error.kind === "desktop_unavailable"
      ? "记忆管理需要在 SynthChat Desktop 中使用。"
      : "本地 Rust 后端无法连接。";
  }
  return fallback;
}

function newIdempotencyKey(): string {
  if (typeof globalThis.crypto?.randomUUID === "function") {
    return `memory-${globalThis.crypto.randomUUID()}`;
  }
  const bytes = new Uint8Array(16);
  globalThis.crypto.getRandomValues(bytes);
  return `memory-${Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("")}`;
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
  const Icon = loading ? LoaderCircle : failed ? CircleSlash2 : Brain;
  const title = loading
    ? "正在连接记忆服务"
    : state.phase === "desktop-required"
      ? "请在 SynthChat Desktop 中打开"
      : state.phase === "unavailable"
        ? "记忆管理暂不可用"
        : "记忆服务连接失败";
  const message = loading
    ? null
    : state.phase === "desktop-required"
      ? "Profile 记忆仅通过受保护的桌面后端连接提供。"
      : state.phase === "unavailable"
        ? "当前 Rust 后端未启用 Memory 写入能力。"
        : state.message;

  return (
    <div
      aria-busy={loading || undefined}
      className="memory-state"
      role={failed ? "alert" : "status"}
    >
      <Icon aria-hidden="true" className={loading ? "spin" : undefined} size={30} />
      <h2>{title}</h2>
      {message ? <p>{message}</p> : null}
      {failed ? (
        <button className="memory-secondary-button" onClick={onRetry} type="button">
          <RefreshCw aria-hidden="true" size={15} />
          重试
        </button>
      ) : null}
    </div>
  );
}

function MemoryEditor({
  busy,
  editor,
  onCancel,
  onChange,
  onSubmit,
}: {
  busy: boolean;
  editor: EditorState;
  onCancel: () => void;
  onChange: (content: string) => void;
  onSubmit: (event: FormEvent<HTMLFormElement>) => void;
}) {
  const creating = editor.mode === "create";
  const label = creating ? "新增记忆内容" : `编辑记忆内容 ${editor.memoryId}`;
  return (
    <form className="memory-editor" onSubmit={onSubmit}>
      <label>
        <span>{creating ? "新增内容" : "编辑内容"}</span>
        <textarea
          aria-label={label}
          autoFocus
          disabled={busy}
          maxLength={2_200}
          onChange={(event) => onChange(event.target.value)}
          rows={creating ? 5 : 4}
          value={editor.content}
        />
      </label>
      <div className="memory-editor-footer">
        <span>{Array.from(editor.content).length.toLocaleString()} / 2,200</span>
        <div>
          <button
            className="memory-secondary-button"
            disabled={busy}
            onClick={onCancel}
            type="button"
          >
            <X aria-hidden="true" size={15} />
            取消
          </button>
          <button
            className="memory-primary-button"
            disabled={busy || editor.content.length === 0}
            type="submit"
          >
            {busy
              ? <LoaderCircle aria-hidden="true" className="spin" size={15} />
              : <Save aria-hidden="true" size={15} />}
            {creating ? "添加" : "保存"}
          </button>
        </div>
      </div>
    </form>
  );
}

function MemoryRow({
  busy,
  canDelete,
  canUpdate,
  confirmingDelete,
  editing,
  memory,
  onCancelDelete,
  onConfirmDelete,
  onEdit,
  onRequestDelete,
}: {
  busy: boolean;
  canDelete: boolean;
  canUpdate: boolean;
  confirmingDelete: boolean;
  editing: boolean;
  memory: Memory;
  onCancelDelete: () => void;
  onConfirmDelete: () => void;
  onEdit: () => void;
  onRequestDelete: () => void;
}) {
  return (
    <article
      aria-busy={busy || undefined}
      className={editing ? "memory-row is-editing" : "memory-row"}
    >
      <div className="memory-row-heading">
        <code title={memory.id}>{memory.id}</code>
        <div className="memory-row-actions">
          <button
            aria-label={`编辑记忆 ${memory.id}`}
            className="memory-icon-button"
            disabled={!canUpdate || busy}
            onClick={onEdit}
            title={canUpdate ? "编辑" : "当前 provider 不支持编辑"}
            type="button"
          >
            <Pencil aria-hidden="true" size={15} />
          </button>
          <button
            aria-label={`删除记忆 ${memory.id}`}
            className="memory-icon-button is-danger"
            disabled={!canDelete || busy}
            onClick={onRequestDelete}
            title={canDelete ? "删除" : "当前 provider 不支持删除"}
            type="button"
          >
            <Trash2 aria-hidden="true" size={15} />
          </button>
        </div>
      </div>
      <p>{memory.content}</p>
      {confirmingDelete ? (
        <div className="memory-delete-confirm" role="alert">
          <span>确认删除这条记忆？</span>
          <button
            className="memory-secondary-button"
            disabled={busy}
            onClick={onCancelDelete}
            type="button"
          >
            取消
          </button>
          <button
            className="memory-danger-button"
            disabled={busy}
            onClick={onConfirmDelete}
            type="button"
          >
            {busy ? <LoaderCircle aria-hidden="true" className="spin" size={14} /> : null}
            确认删除
          </button>
        </div>
      ) : null}
    </article>
  );
}

export function MemoryWorkspace({
  client = memoriesApi,
  profileClient = profilesApi,
}: MemoryWorkspaceProps) {
  const [workspace, setWorkspace] = useState<WorkspaceState>({ phase: "loading" });
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [profileId, setProfileId] = useState<string | null>(null);
  const [target, setTarget] = useState<MemoryTarget>("memory");
  const [page, setPage] = useState<PageState>({ phase: "idle" });
  const [searchInput, setSearchInput] = useState("");
  const [query, setQuery] = useState("");
  const [editor, setEditor] = useState<EditorState | null>(null);
  const [deleteConfirmId, setDeleteConfirmId] = useState<string | null>(null);
  const [busyAction, setBusyAction] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [workspaceEpoch, setWorkspaceEpoch] = useState(0);
  const [pageEpoch, setPageEpoch] = useState(0);
  const paginationController = useRef<AbortController | null>(null);

  useEffect(() => {
    const controller = new AbortController();
    setWorkspace({ phase: "loading" });
    setActionError(null);
    void (async () => {
      try {
        const capabilities = await profileClient.getCapabilities({ signal: controller.signal });
        if (controller.signal.aborted) return;
        if (!capabilities.engine.features.memoryWrite) {
          setProfiles([]);
          setProfileId(null);
          setWorkspace({ phase: "unavailable" });
          return;
        }
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
            message: errorMessage(error, "无法加载记忆服务。"),
          });
        }
      }
    })();
    return () => controller.abort();
  }, [profileClient, workspaceEpoch]);

  useEffect(() => {
    paginationController.current?.abort();
    paginationController.current = null;
    if (workspace.phase !== "ready" || !profileId) {
      setPage({ phase: "idle" });
      return undefined;
    }
    const controller = new AbortController();
    setPage({ phase: "loading" });
    void client.listMemories(
      profileId,
      {
        target,
        ...(query ? { query } : {}),
        limit: PAGE_LIMIT,
      },
      { signal: controller.signal },
    )
      .then((resource) => {
        if (!controller.signal.aborted) {
          setPage({ phase: "ready", resource, loadingMore: false, loadMoreError: null });
        }
      })
      .catch((error: unknown) => {
        if (!controller.signal.aborted && !isAbortError(error)) {
          setPage({
            phase: "error",
            message: error instanceof MemoryApiError && error.status === 422
              ? "当前 Profile 未使用 builtin 记忆存储。"
              : errorMessage(error, "无法加载记忆列表。"),
          });
        }
      });
    return () => controller.abort();
  }, [client, pageEpoch, profileId, query, target, workspace.phase]);

  const refreshPage = () => {
    paginationController.current?.abort();
    paginationController.current = null;
    setPageEpoch((value) => value + 1);
  };

  const selectProfile = (nextProfileId: string) => {
    paginationController.current?.abort();
    setProfileId(nextProfileId || null);
    setSearchInput("");
    setQuery("");
    setEditor(null);
    setDeleteConfirmId(null);
    setActionError(null);
  };

  const selectTarget = (nextTarget: MemoryTarget) => {
    if (nextTarget === target || busyAction) return;
    paginationController.current?.abort();
    setTarget(nextTarget);
    setSearchInput("");
    setQuery("");
    setEditor(null);
    setDeleteConfirmId(null);
    setActionError(null);
  };

  const submitSearch = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (busyAction || page.phase !== "ready" || !page.resource.value.capabilities.search) return;
    const nextQuery = searchInput.trim();
    setSearchInput(nextQuery);
    setQuery(nextQuery);
    refreshPage();
  };

  const clearSearch = () => {
    if (busyAction) return;
    setSearchInput("");
    setQuery("");
    refreshPage();
  };

  const staleRevision = (error: unknown): boolean => (
    error instanceof MemoryApiError && (error.status === 409 || error.status === 412)
  );

  const submitEditor = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (!profileId || !editor || page.phase !== "ready" || busyAction) return;
    const targetEditor = editor;
    const targetProfileId = profileId;
    const etag = page.resource.etag;
    const capabilities = page.resource.value.capabilities;
    if (
      (targetEditor.mode === "create" && !capabilities.create)
      || (targetEditor.mode === "edit" && !capabilities.update)
    ) return;
    setBusyAction(targetEditor.mode === "create" ? "create" : `edit:${targetEditor.memoryId}`);
    setActionError(null);
    try {
      if (targetEditor.mode === "create") {
        await client.createMemory(
          targetProfileId,
          { target, content: targetEditor.content },
          etag,
          newIdempotencyKey(),
        );
      } else {
        await client.updateMemory(
          targetProfileId,
          targetEditor.memoryId,
          { content: targetEditor.content },
          etag,
        );
      }
      setEditor(null);
      refreshPage();
    } catch (error) {
      if (staleRevision(error)) {
        setEditor(null);
        setActionError("记忆已在其他窗口更新，已重新加载最新版本，请确认后重试。");
        refreshPage();
      } else {
        setActionError(errorMessage(error, "无法保存记忆。"));
      }
    } finally {
      setBusyAction(null);
    }
  };

  const deleteMemory = async (memory: Memory) => {
    if (
      !profileId
      || page.phase !== "ready"
      || !page.resource.value.capabilities.delete
      || busyAction
    ) return;
    setBusyAction(`delete:${memory.id}`);
    setActionError(null);
    try {
      await client.deleteMemory(profileId, memory.id, page.resource.etag);
      setDeleteConfirmId(null);
      refreshPage();
    } catch (error) {
      if (staleRevision(error)) {
        setDeleteConfirmId(null);
        setActionError("记忆已在其他窗口更新，已重新加载最新版本，请确认后重试。");
        refreshPage();
      } else {
        setActionError(errorMessage(error, "无法删除记忆。"));
      }
    } finally {
      setBusyAction(null);
    }
  };

  const loadMore = async () => {
    if (
      !profileId
      || page.phase !== "ready"
      || page.loadingMore
      || busyAction
      || !page.resource.value.nextCursor
    ) return;
    const resource = page.resource;
    const cursor = resource.value.nextCursor;
    if (!cursor) return;
    const controller = new AbortController();
    paginationController.current?.abort();
    paginationController.current = controller;
    setPage((current) => current.phase === "ready"
      ? { ...current, loadingMore: true, loadMoreError: null }
      : current);
    try {
      const next = await client.listMemories(
        profileId,
        {
          target,
          ...(query ? { query } : {}),
          cursor,
          limit: PAGE_LIMIT,
        },
        { signal: controller.signal },
      );
      if (controller.signal.aborted) return;
      if (next.etag !== resource.etag || next.value.revision !== resource.value.revision) {
        setActionError("记忆在分页期间发生变化，已重新加载最新版本。");
        refreshPage();
        return;
      }
      setPage((current) => {
        if (current.phase !== "ready" || current.resource.etag !== resource.etag) return current;
        const knownIds = new Set(current.resource.value.items.map((item) => item.id));
        return {
          phase: "ready",
          resource: {
            etag: next.etag,
            value: {
              ...next.value,
              items: [
                ...current.resource.value.items,
                ...next.value.items.filter((item) => !knownIds.has(item.id)),
              ],
            },
          },
          loadingMore: false,
          loadMoreError: null,
        };
      });
    } catch (error) {
      if (!controller.signal.aborted && !isAbortError(error)) {
        setPage((current) => current.phase === "ready"
          ? {
            ...current,
            loadingMore: false,
            loadMoreError: errorMessage(error, "无法加载更多记忆。"),
          }
          : current);
      }
    } finally {
      if (paginationController.current === controller) paginationController.current = null;
    }
  };

  if (workspace.phase !== "ready") {
    return (
      <WorkspaceStateView
        onRetry={() => setWorkspaceEpoch((value) => value + 1)}
        state={workspace}
      />
    );
  }

  const capabilities = page.phase === "ready" ? page.resource.value.capabilities : null;
  const usage = page.phase === "ready" ? page.resource.value : null;

  return (
    <div className="memory-panel">
      <header className="memory-toolbar">
        <label>
          <span>Profile</span>
          <select
            aria-label="记忆 Profile"
            disabled={profiles.length === 0 || busyAction !== null}
            onChange={(event) => selectProfile(event.target.value)}
            value={profileId ?? ""}
          >
            {profiles.length === 0 ? <option value="">暂无 Profile</option> : null}
            {profiles.map((profile) => (
              <option key={profile.id} value={profile.id}>{profile.displayName}</option>
            ))}
          </select>
        </label>
        {usage ? (
          <div className="memory-usage" aria-label="记忆用量">
            <div>
              <span>用量</span>
              <strong>{usage.charsUsed.toLocaleString()} / {usage.charLimit.toLocaleString()}</strong>
            </div>
            <progress max={Math.max(1, usage.charLimit)} value={usage.charsUsed} />
          </div>
        ) : null}
        <button
          className="memory-primary-button"
          disabled={!profileId || !capabilities?.create || busyAction !== null}
          onClick={() => {
            setDeleteConfirmId(null);
            setEditor({ mode: "create", content: "" });
            setActionError(null);
          }}
          title={capabilities && !capabilities.create ? "当前 provider 不支持新增" : "新增记忆"}
          type="button"
        >
          <Plus aria-hidden="true" size={16} />
          新增
        </button>
      </header>

      <div className="memory-controls">
        <div aria-label="记忆目标" className="memory-tabs" role="tablist">
          <button
            aria-selected={target === "memory"}
            disabled={busyAction !== null}
            onClick={() => selectTarget("memory")}
            role="tab"
            type="button"
          >
            长期记忆
          </button>
          <button
            aria-selected={target === "user"}
            disabled={busyAction !== null}
            onClick={() => selectTarget("user")}
            role="tab"
            type="button"
          >
            用户信息
          </button>
        </div>

        <form aria-label="记忆搜索" className="memory-search" onSubmit={submitSearch} role="search">
          <label>
            <span className="sr-only">搜索记忆</span>
            <Search aria-hidden="true" size={15} />
            <input
              disabled={busyAction !== null || capabilities?.search === false}
              maxLength={500}
              onChange={(event) => setSearchInput(event.target.value)}
              placeholder={capabilities?.search === false ? "当前 provider 不支持搜索" : "搜索当前目标"}
              type="search"
              value={searchInput}
            />
          </label>
          {searchInput || query ? (
            <button
              aria-label="清除记忆搜索"
              className="memory-icon-button"
              disabled={busyAction !== null}
              onClick={clearSearch}
              title="清除搜索"
              type="button"
            >
              <X aria-hidden="true" size={15} />
            </button>
          ) : null}
          <button
            className="memory-secondary-button"
            disabled={busyAction !== null || capabilities?.search === false}
            type="submit"
          >
            <Search aria-hidden="true" size={15} />
            搜索
          </button>
        </form>
      </div>

      {usage?.promptSafety === "blocked" ? (
        <div className="memory-safety-alert" role="alert">
          <ShieldAlert aria-hidden="true" size={17} />
          <span>当前记忆未通过提示安全检查，Run 不会注入这些内容。</span>
        </div>
      ) : null}

      {actionError ? <p className="memory-action-error" role="alert">{actionError}</p> : null}

      {editor?.mode === "create" ? (
        <MemoryEditor
          busy={busyAction === "create"}
          editor={editor}
          onCancel={() => setEditor(null)}
          onChange={(content) => setEditor({ mode: "create", content })}
          onSubmit={(event) => void submitEditor(event)}
        />
      ) : null}

      <section className="memory-catalog" aria-labelledby="memory-catalog-title">
        <div className="memory-section-heading">
          <div>
            <span>BUILTIN MEMORY</span>
            <h2 id="memory-catalog-title">{target === "memory" ? "长期记忆" : "用户信息"}</h2>
          </div>
          {page.phase === "ready" ? (
            <div className="memory-count" aria-live="polite">
              <strong>{page.resource.value.items.length}</strong>
              <span>已加载</span>
            </div>
          ) : null}
        </div>

        {!profileId ? (
          <div className="memory-inline-state">没有可用的 Profile。</div>
        ) : page.phase === "loading" || page.phase === "idle" ? (
          <div aria-busy="true" className="memory-inline-state" role="status">
            <LoaderCircle aria-hidden="true" className="spin" size={20} />
            正在加载记忆
          </div>
        ) : page.phase === "error" ? (
          <div className="memory-inline-state is-error">
            <p role="alert">{page.message}</p>
            <button className="memory-secondary-button" onClick={refreshPage} type="button">
              <RefreshCw aria-hidden="true" size={15} />
              重新加载
            </button>
          </div>
        ) : page.resource.value.items.length === 0 ? (
          <div className="memory-inline-state">
            {query ? "没有匹配的记忆。" : "当前目标还没有记忆。"}
          </div>
        ) : (
          <ul className="memory-list">
            {page.resource.value.items.map((memory) => (
              <li className="memory-item-shell" key={memory.id}>
                <MemoryRow
                  busy={busyAction === `delete:${memory.id}` || busyAction === `edit:${memory.id}`}
                  canDelete={page.resource.value.capabilities.delete}
                  canUpdate={page.resource.value.capabilities.update}
                  confirmingDelete={deleteConfirmId === memory.id}
                  editing={editor?.mode === "edit" && editor.memoryId === memory.id}
                  memory={memory}
                  onCancelDelete={() => setDeleteConfirmId(null)}
                  onConfirmDelete={() => void deleteMemory(memory)}
                  onEdit={() => {
                    setDeleteConfirmId(null);
                    setEditor({ mode: "edit", memoryId: memory.id, content: memory.content });
                    setActionError(null);
                  }}
                  onRequestDelete={() => {
                    setEditor(null);
                    setDeleteConfirmId(memory.id);
                    setActionError(null);
                  }}
                />
                {editor?.mode === "edit" && editor.memoryId === memory.id ? (
                  <MemoryEditor
                    busy={busyAction === `edit:${memory.id}`}
                    editor={editor}
                    onCancel={() => setEditor(null)}
                    onChange={(content) => setEditor({
                      mode: "edit",
                      memoryId: memory.id,
                      content,
                    })}
                    onSubmit={(event) => void submitEditor(event)}
                  />
                ) : null}
              </li>
            ))}
          </ul>
        )}

        {page.phase === "ready" && page.resource.value.nextCursor ? (
          <div className="memory-pagination">
            {page.loadMoreError ? <p role="alert">{page.loadMoreError}</p> : null}
            <button
              className="memory-secondary-button"
              disabled={page.loadingMore || busyAction !== null}
              onClick={() => void loadMore()}
              type="button"
            >
              {page.loadingMore
                ? <LoaderCircle aria-hidden="true" className="spin" size={15} />
                : <Plus aria-hidden="true" size={15} />}
              加载更多
            </button>
          </div>
        ) : null}
      </section>
    </div>
  );
}
