import {
  Archive,
  ArchiveRestore,
  Bot,
  CircleCheck,
  CircleSlash2,
  FileText,
  History,
  Import as ImportIcon,
  LoaderCircle,
  MessageSquareText,
  Plus,
  RefreshCw,
  Save,
  Search,
  Send,
  Trash2,
  TriangleAlert,
  UserRound,
  Wrench,
  X,
} from "lucide-react";
import {
  useEffect,
  useRef,
  useState,
  type FormEvent,
  type ReactNode,
} from "react";
import { DesktopConnectionError } from "../../api/desktopConnection";
import { MarkdownContent } from "../../components/MarkdownContent";
import {
  ProfileApiError,
  profilesApi,
  type ProfilesApi,
  type ProfileSummary,
} from "../../api/profiles";
import {
  SessionImportApiError,
  sessionImportsApi,
  type HermesImportConflict,
  type HermesImportPreview,
  type HermesImportResult,
  type HermesImportWarning,
  type SessionImportsApi,
} from "../../api/sessionImports";
import {
  SessionApiError,
  sessionsApi,
  type Message,
  type MessagePage,
  type Session,
  type SessionsApi,
  type VersionedSession,
} from "../../api/sessions";
import "./sessions.css";

type ProfileClient = Pick<ProfilesApi, "getCapabilities" | "listProfiles">;
type ImportClient = Pick<
  SessionImportsApi,
  "previewHermesV21Import" | "importHermesV21"
>;

type WorkspaceState =
  | { phase: "loading" }
  | { phase: "desktop-required" }
  | { phase: "storage-unavailable" }
  | { phase: "error"; message: string }
  | {
    phase: "ready";
    searchAvailable: boolean;
    searchMode: string;
    hermesImportAvailable: boolean;
  };

type ListState =
  | { phase: "idle" }
  | { phase: "loading"; requestKey: string }
  | { phase: "error"; message: string; requestKey: string }
  | {
    phase: "ready";
    requestKey: string;
    items: Session[];
    nextCursor: string | null;
    loadingMore: boolean;
    moreError: string | null;
  };

type DetailState =
  | { phase: "idle" }
  | { phase: "loading" }
  | { phase: "error"; message: string }
  | {
    phase: "ready";
    resource: VersionedSession;
    messages: Message[];
    nextCursor: string | null;
    snapshotLastSequence: number;
    loadingMore: boolean;
    moreError: string | null;
  };

interface CreateAttempt {
  fingerprint: string;
  idempotencyKey: string;
}

interface HermesImportAttempt {
  fingerprint: string;
  idempotencyKey: string;
}

type ReadyHermesImportPreview = HermesImportPreview & {
  state: "ready";
  schemaVersion: number;
  snapshotFingerprint: string;
  sessionCount: number;
  messageCount: number;
  modelUsageRowCount: number;
  attachmentCount: number;
  rewoundMessageCount: number;
};

type ImportState =
  | { phase: "closed" }
  | { phase: "previewing"; profileId: string }
  | { phase: "absent"; profileId: string }
  | {
    phase: "ready";
    profileId: string;
    preview: ReadyHermesImportPreview;
    allowAttachmentOmission: boolean;
  }
  | {
    phase: "importing";
    profileId: string;
    preview: ReadyHermesImportPreview;
    allowAttachmentOmission: boolean;
  }
  | { phase: "result"; profileId: string; result: HermesImportResult }
  | {
    phase: "error";
    profileId: string;
    stage: "preview";
    message: string;
  }
  | {
    phase: "error";
    profileId: string;
    stage: "import";
    message: string;
    retryable: boolean;
    sourceChanged: boolean;
    preview: ReadyHermesImportPreview;
    allowAttachmentOmission: boolean;
  }
  | {
    phase: "conflict";
    profileId: string;
    message: string;
    conflicts: HermesImportConflict[];
    conflictCount: number;
    conflictsDropped: number;
  };

export interface SessionsWorkspaceProps {
  client?: SessionsApi;
  importClient?: ImportClient;
  profileClient?: ProfileClient;
  onContinue?: (session: Session) => void;
}

function isAbortError(error: unknown): boolean {
  return error instanceof DOMException && error.name === "AbortError";
}

function safeErrorMessage(error: unknown, fallback: string): string {
  if (error instanceof SessionImportApiError) {
    const message = (() => {
      switch (error.code) {
        case "hermes_state_not_found": return "未找到可导入的 Hermes 历史。";
        case "hermes_import_source_changed": return "Hermes 历史在确认后发生了变化，请重新检查。";
        case "hermes_import_conflict": return "Hermes 历史与本地会话存在冲突。";
        case "hermes_import_too_large": return "Hermes 历史超出当前可导入的大小限制。";
        case "hermes_schema_unsupported": return "当前 Hermes 历史数据库版本不受支持。";
        case "hermes_import_source_invalid": return "Hermes 历史数据库无法安全读取。";
        case "hermes_attachments_require_policy": return "请确认附件忽略策略后再导入。";
        case "hermes_state_unavailable": return "Hermes 历史数据库暂时不可用。";
        default: return error.message;
      }
    })();
    return error.requestId ? `${message} 请求 ID：${error.requestId}` : message;
  }
  if (error instanceof SessionApiError || error instanceof ProfileApiError) {
    if (error instanceof SessionApiError && error.code === "session_storage_busy") {
      return "会话库正忙，请稍后重试。";
    }
    if (error instanceof SessionApiError && error.code === "session_search_unavailable") {
      return "当前后端不支持会话全文搜索。";
    }
    return error.requestId ? `${error.message} 请求 ID：${error.requestId}` : error.message;
  }
  if (error instanceof DesktopConnectionError && error.kind === "network") {
    return "本地 Rust 后端无法连接。";
  }
  return fallback;
}

function isReadyHermesImportPreview(
  preview: HermesImportPreview,
): preview is ReadyHermesImportPreview {
  return preview.state === "ready"
    && preview.schemaVersion !== null
    && preview.snapshotFingerprint !== null
    && preview.sessionCount !== null
    && preview.messageCount !== null
    && preview.modelUsageRowCount !== null
    && preview.attachmentCount !== null
    && preview.rewoundMessageCount !== null;
}

function isRetryableImportError(error: unknown): boolean {
  return (error instanceof SessionImportApiError && error.retryable)
    || (error instanceof DesktopConnectionError && error.kind === "network");
}

function conflictLabel(code: HermesImportConflict["code"]): string {
  switch (code) {
    case "sourceRemoved": return "来源会话已移除";
    case "sourceChanged": return "来源会话已变化";
    case "sourceExtended": return "来源会话新增了消息";
    case "targetDeleted": return "本地映射已删除";
    case "targetModified": return "本地会话已修改";
  }
}

function warningCount(warnings: HermesImportWarning[]): number {
  return warnings.reduce((total, warning) => total + warning.count, 0);
}

function newIdempotencyKey(): string {
  if (typeof globalThis.crypto?.randomUUID === "function") {
    return globalThis.crypto.randomUUID();
  }
  const bytes = new Uint8Array(16);
  globalThis.crypto.getRandomValues(bytes);
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

function formatTimestamp(value: string): string {
  return new Intl.DateTimeFormat("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(value));
}

function roleLabel(role: Message["role"]): string {
  switch (role) {
    case "user": return "你";
    case "assistant": return "Hermes";
    case "system": return "系统";
    case "tool": return "工具";
  }
}

function toolStatusLabel(status: Message["toolCalls"][number]["status"]): string {
  switch (status) {
    case "unknown": return "历史状态未知";
    case "running": return "运行中";
    case "completed": return "已完成";
    case "failed": return "失败";
    case "cancelled": return "已取消";
  }
}

function RoleIcon({ role }: { role: Message["role"] }) {
  if (role === "user") return <UserRound aria-hidden="true" size={15} />;
  if (role === "assistant") return <Bot aria-hidden="true" size={15} />;
  return <Wrench aria-hidden="true" size={15} />;
}

function MessageParts({ message }: { message: Message }) {
  return (
    <>
      {message.parts.map((part, index) => part.type === "text" ? (
        message.role === "assistant" ? (
          <MarkdownContent className="session-message-markdown" key={`${message.id}:text:${index}`}>
            {part.text}
          </MarkdownContent>
        ) : (
          <p className="session-message-text" key={`${message.id}:text:${index}`}>{part.text}</p>
        )
      ) : (
        <div className="session-file-part" key={`${message.id}:file:${part.fileId}`}>
          <FileText aria-hidden="true" size={15} />
          <span>{part.name}</span>
          <small>{part.mimeType}</small>
        </div>
      ))}
      {message.reasoning ? (
        <details className="session-reasoning">
          <summary>推理过程</summary>
          <p>{message.reasoning}</p>
        </details>
      ) : null}
      {message.toolCalls.length > 0 ? (
        <div className="session-tool-calls" aria-label="工具调用">
          {message.toolCalls.map((tool) => (
            <div className={`session-tool-call is-${tool.status}`} key={tool.callId}>
              <Wrench aria-hidden="true" size={13} />
              <strong>{tool.name}</strong>
              <span>{toolStatusLabel(tool.status)}</span>
              {tool.resultSummary ? <small>{tool.resultSummary}</small> : null}
            </div>
          ))}
        </div>
      ) : null}
    </>
  );
}

export function SessionMessageTimeline({ messages }: { messages: Message[] }) {
  if (messages.length === 0) {
    return (
      <div className="session-empty-messages">
        <MessageSquareText aria-hidden="true" size={24} />
        <strong>尚无消息</strong>
        <span>此会话还没有已提交的消息。</span>
      </div>
    );
  }

  return (
    <ol className="session-message-list">
      {messages.map((message) => (
        <li className={`session-message role-${message.role}`} key={message.id}>
          <header>
            <span><RoleIcon role={message.role} />{roleLabel(message.role)}</span>
            <time dateTime={message.createdAt}>#{message.sequence} · {formatTimestamp(message.createdAt)}</time>
          </header>
          <div className="session-message-body">
            <MessageParts message={message} />
          </div>
          {message.usage ? (
            <footer>{message.usage.totalTokens.toLocaleString("zh-CN")} tokens</footer>
          ) : null}
        </li>
      ))}
    </ol>
  );
}

function StatePanel({
  icon,
  title,
  children,
  busy = false,
}: {
  icon: ReactNode;
  title: string;
  children?: ReactNode;
  busy?: boolean;
}) {
  return (
    <div aria-busy={busy || undefined} className="session-state">
      {icon}
      <h2>{title}</h2>
      {children}
    </div>
  );
}

function ImportWarnings({
  warnings,
  dropped,
}: {
  warnings: HermesImportWarning[];
  dropped: number;
}) {
  if (warnings.length === 0 && dropped === 0) return null;
  return (
    <details className="session-import-details">
      <summary>{warningCount(warnings)} 条导入警告</summary>
      <ul>
        {warnings.map((warning) => (
          <li key={warning.code}>
            <code>{warning.code}</code>
            <span>{warning.count} 条</span>
          </li>
        ))}
      </ul>
      {dropped > 0 ? <p>另有 {dropped} 条警告未返回。</p> : null}
    </details>
  );
}

function importResultTitle(disposition: HermesImportResult["disposition"]): string {
  switch (disposition) {
    case "imported": return "导入完成";
    case "unchanged": return "Hermes 历史已是最新";
    case "replayed": return "已恢复上次导入结果";
  }
}

function SessionImportPanel({
  state,
  profileName,
  actionBusy,
  onAllowAttachmentOmission,
  onClose,
  onConfirm,
  onPreview,
  onRetryImport,
}: {
  state: ImportState;
  profileName: string;
  actionBusy: boolean;
  onAllowAttachmentOmission: (allow: boolean) => void;
  onClose: () => void;
  onConfirm: () => void;
  onPreview: () => void;
  onRetryImport: () => void;
}) {
  if (state.phase === "closed") return null;
  const busy = state.phase === "previewing" || state.phase === "importing";

  return (
    <section
      aria-label="Hermes 历史导入"
      className={`session-import-panel is-${state.phase}`}
    >
      <header className="session-import-header">
        <span>
          <ImportIcon aria-hidden="true" size={15} />
          <strong>Hermes 历史</strong>
          <small>{profileName}</small>
        </span>
        {state.phase !== "importing" ? (
          <button
            aria-label="关闭 Hermes 历史导入"
            className="session-import-close"
            onClick={onClose}
            title="关闭"
            type="button"
          >
            <X aria-hidden="true" size={14} />
          </button>
        ) : null}
      </header>

      {state.phase === "previewing" ? (
        <div aria-busy="true" className="session-import-status">
          <LoaderCircle aria-hidden="true" className="spin" size={17} />
          <span>正在检查可导入历史</span>
        </div>
      ) : state.phase === "absent" ? (
        <div className="session-import-status" role="status">
          <CircleSlash2 aria-hidden="true" size={17} />
          <span>未发现可导入的 Hermes 历史。</span>
          <button className="session-secondary-button" onClick={onPreview} type="button">
            <RefreshCw aria-hidden="true" size={14} />
            重新检查
          </button>
        </div>
      ) : state.phase === "ready" ? (
        <>
          <dl className="session-import-metrics">
            <div><dt>会话</dt><dd>{state.preview.sessionCount.toLocaleString("zh-CN")}</dd></div>
            <div><dt>消息</dt><dd>{state.preview.messageCount.toLocaleString("zh-CN")}</dd></div>
            <div><dt>用量记录</dt><dd>{state.preview.modelUsageRowCount.toLocaleString("zh-CN")}</dd></div>
            <div><dt>历史消息</dt><dd>{state.preview.rewoundMessageCount.toLocaleString("zh-CN")}</dd></div>
          </dl>
          <ImportWarnings
            dropped={state.preview.warningsDropped}
            warnings={state.preview.warnings}
          />
          {state.preview.attachmentCount > 0 ? (
            <label className="session-import-checkbox">
              <input
                checked={state.allowAttachmentOmission}
                disabled={actionBusy}
                onChange={(event) => onAllowAttachmentOmission(event.target.checked)}
                type="checkbox"
              />
              <span>
                忽略 {state.preview.attachmentCount.toLocaleString("zh-CN")} 个附件引用并继续
              </span>
            </label>
          ) : null}
          <p className="session-import-note">发生冲突时整批不会写入。</p>
          <div className="session-import-actions">
            <button className="session-secondary-button" onClick={onClose} type="button">
              取消
            </button>
            <button
              className="session-primary-button"
              disabled={
                actionBusy
                || (state.preview.attachmentCount > 0 && !state.allowAttachmentOmission)
              }
              onClick={onConfirm}
              type="button"
            >
              <ImportIcon aria-hidden="true" size={14} />
              确认导入
            </button>
          </div>
        </>
      ) : state.phase === "importing" ? (
        <div aria-busy="true" className="session-import-status">
          <LoaderCircle aria-hidden="true" className="spin" size={17} />
          <span>正在导入，完成前请保持应用运行</span>
        </div>
      ) : state.phase === "result" ? (
        <div className="session-import-result" role="status">
          <div className="session-import-result-title">
            <CircleCheck aria-hidden="true" size={17} />
            <strong>{importResultTitle(state.result.disposition)}</strong>
          </div>
          <dl className="session-import-metrics">
            <div><dt>会话</dt><dd>{state.result.importedSessionCount.toLocaleString("zh-CN")}</dd></div>
            <div><dt>消息</dt><dd>{state.result.importedMessageCount.toLocaleString("zh-CN")}</dd></div>
            <div><dt>用量记录</dt><dd>{state.result.importedModelUsageRowCount.toLocaleString("zh-CN")}</dd></div>
            <div><dt>忽略附件</dt><dd>{state.result.omittedAttachmentCount.toLocaleString("zh-CN")}</dd></div>
          </dl>
          <ImportWarnings
            dropped={state.result.warningsDropped}
            warnings={state.result.warnings}
          />
        </div>
      ) : state.phase === "conflict" ? (
        <div className="session-import-problem" role="alert">
          <div className="session-import-problem-title">
            <TriangleAlert aria-hidden="true" size={17} />
            <strong>存在 {state.conflictCount} 个冲突，整批未写入</strong>
          </div>
          <p>{state.message}</p>
          <details className="session-import-details" open>
            <summary>冲突项</summary>
            <ul className="session-import-conflicts">
              {state.conflicts.map((conflict) => (
                <li key={`${conflict.sourceKeyDigest}:${conflict.code}`}>
                  <strong>{conflictLabel(conflict.code)}</strong>
                  <code>来源 {conflict.sourceKeyDigest.slice(0, 12)}</code>
                  {conflict.targetSessionId ? <code>{conflict.targetSessionId}</code> : null}
                </li>
              ))}
            </ul>
            {state.conflictsDropped > 0 ? (
              <p>另有 {state.conflictsDropped} 个冲突未返回。</p>
            ) : null}
          </details>
          <div className="session-import-actions">
            <button className="session-secondary-button" onClick={onPreview} type="button">
              <RefreshCw aria-hidden="true" size={14} />
              重新检查
            </button>
          </div>
        </div>
      ) : (
        <div className="session-import-problem" role="alert">
          <div className="session-import-problem-title">
            <TriangleAlert aria-hidden="true" size={17} />
            <strong>{state.stage === "preview" ? "检查失败" : "导入失败"}</strong>
          </div>
          <p>{state.message}</p>
          <div className="session-import-actions">
            {state.stage === "import" && state.retryable && !state.sourceChanged ? (
              <button className="session-primary-button" onClick={onRetryImport} type="button">
                <RefreshCw aria-hidden="true" size={14} />
                重试导入
              </button>
            ) : null}
            <button className="session-secondary-button" onClick={onPreview} type="button">
              <RefreshCw aria-hidden="true" size={14} />
              {state.stage === "preview" ? "重试检查" : "重新检查"}
            </button>
          </div>
        </div>
      )}
      {busy ? <span className="sr-only" aria-live="polite">操作进行中</span> : null}
    </section>
  );
}

export function SessionsWorkspace({
  client = sessionsApi,
  importClient = sessionImportsApi,
  profileClient = profilesApi,
  onContinue,
}: SessionsWorkspaceProps) {
  const [workspace, setWorkspace] = useState<WorkspaceState>({ phase: "loading" });
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [selectedProfileId, setSelectedProfileId] = useState<string | null>(null);
  const [archived, setArchived] = useState(false);
  const [queryDraft, setQueryDraft] = useState("");
  const [appliedQuery, setAppliedQuery] = useState("");
  const [list, setList] = useState<ListState>({ phase: "idle" });
  const [selectedSessionId, setSelectedSessionId] = useState<string | null>(null);
  const [detail, setDetail] = useState<DetailState>({ phase: "idle" });
  const [titleDraft, setTitleDraft] = useState("");
  const [showCreate, setShowCreate] = useState(false);
  const [createTitle, setCreateTitle] = useState("");
  const [createError, setCreateError] = useState<string | null>(null);
  const [actionBusy, setActionBusy] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [deleteArmed, setDeleteArmed] = useState(false);
  const [importState, setImportState] = useState<ImportState>({ phase: "closed" });
  const [workspaceEpoch, setWorkspaceEpoch] = useState(0);
  const [listEpoch, setListEpoch] = useState(0);
  const [detailEpoch, setDetailEpoch] = useState(0);
  const createAttemptRef = useRef<CreateAttempt | null>(null);
  const importAttemptRef = useRef<HermesImportAttempt | null>(null);
  const importControllerRef = useRef<AbortController | null>(null);
  const importEpochRef = useRef(0);
  const listRequestKeyRef = useRef("");
  const importing = importState.phase === "importing";
  const controlsBusy = actionBusy || importing;
  const hermesImportAvailable = workspace.phase === "ready"
    && workspace.hermesImportAvailable;

  useEffect(() => () => {
    importEpochRef.current += 1;
    importControllerRef.current?.abort();
  }, []);

  useEffect(() => {
    if (hermesImportAvailable) return;
    importEpochRef.current += 1;
    importControllerRef.current?.abort();
    importControllerRef.current = null;
    importAttemptRef.current = null;
    setImportState({ phase: "closed" });
  }, [hermesImportAvailable]);

  useEffect(() => {
    const controller = new AbortController();
    setWorkspace({ phase: "loading" });
    setActionError(null);
    void (async () => {
      try {
        const [capabilities, profileList] = await Promise.all([
          profileClient.getCapabilities({ signal: controller.signal }),
          profileClient.listProfiles({ signal: controller.signal }),
        ]);
        if (controller.signal.aborted) return;
        setProfiles(profileList);
        setSelectedProfileId((current) => (
          current && profileList.some((profile) => profile.id === current)
            ? current
            : profileList.find((profile) => profile.isActive)?.id ?? profileList[0]?.id ?? null
        ));
        if (!capabilities.sessionStorage.available) {
          setWorkspace({ phase: "storage-unavailable" });
          return;
        }
        setWorkspace({
          phase: "ready",
          searchAvailable: capabilities.sessionSearch.mode !== "unavailable",
          searchMode: capabilities.sessionSearch.mode,
          hermesImportAvailable: capabilities.sessionStorage.hermesImportAvailable,
        });
      } catch (error) {
        if (controller.signal.aborted || isAbortError(error)) return;
        if (error instanceof DesktopConnectionError && error.kind === "desktop_unavailable") {
          setWorkspace({ phase: "desktop-required" });
          return;
        }
        setWorkspace({
          phase: "error",
          message: safeErrorMessage(error, "无法连接会话服务。"),
        });
      }
    })();
    return () => controller.abort();
  }, [profileClient, workspaceEpoch]);

  useEffect(() => {
    if (workspace.phase !== "ready") {
      setList({ phase: "idle" });
      return undefined;
    }
    if (!selectedProfileId) {
      setList({
        phase: "ready",
        requestKey: "no-profile",
        items: [],
        nextCursor: null,
        loadingMore: false,
        moreError: null,
      });
      setSelectedSessionId(null);
      return undefined;
    }

    const controller = new AbortController();
    const requestKey = JSON.stringify([
      selectedProfileId,
      archived,
      appliedQuery,
      listEpoch,
    ]);
    listRequestKeyRef.current = requestKey;
    setList({ phase: "loading", requestKey });
    void (async () => {
      try {
        const page = appliedQuery
          ? await client.searchSessions({
            profileId: selectedProfileId,
            query: appliedQuery,
            archived,
            limit: 30,
          }, { signal: controller.signal })
          : await client.listSessions({
            profileId: selectedProfileId,
            archived,
            limit: 30,
          }, { signal: controller.signal });
        if (controller.signal.aborted || listRequestKeyRef.current !== requestKey) return;
        setList({
          phase: "ready",
          requestKey,
          items: page.items,
          nextCursor: page.nextCursor,
          loadingMore: false,
          moreError: null,
        });
        setSelectedSessionId((current) => (
          current && page.items.some((session) => session.id === current)
            ? current
            : page.items[0]?.id ?? null
        ));
      } catch (error) {
        if (
          controller.signal.aborted
          || isAbortError(error)
          || listRequestKeyRef.current !== requestKey
        ) return;
        setList({
          phase: "error",
          requestKey,
          message: safeErrorMessage(error, "无法加载会话列表。"),
        });
      }
    })();
    return () => controller.abort();
  }, [appliedQuery, archived, client, listEpoch, selectedProfileId, workspace.phase]);

  useEffect(() => {
    setDeleteArmed(false);
    if (!selectedSessionId) {
      setDetail({ phase: "idle" });
      setTitleDraft("");
      return undefined;
    }
    const controller = new AbortController();
    setDetail({ phase: "loading" });
    void (async () => {
      try {
        const [resource, messages] = await Promise.all([
          client.getSession(selectedSessionId, { signal: controller.signal }),
          client.listMessages(selectedSessionId, { limit: 50 }, { signal: controller.signal }),
        ]);
        if (controller.signal.aborted) return;
        setTitleDraft(resource.value.title);
        setDetail({
          phase: "ready",
          resource,
          messages: messages.items,
          nextCursor: messages.nextCursor,
          snapshotLastSequence: messages.snapshotLastSequence,
          loadingMore: false,
          moreError: null,
        });
      } catch (error) {
        if (controller.signal.aborted || isAbortError(error)) return;
        setDetail({
          phase: "error",
          message: safeErrorMessage(error, "无法加载会话详情。"),
        });
      }
    })();
    return () => controller.abort();
  }, [client, detailEpoch, selectedSessionId]);

  const refreshSessionData = () => {
    listRequestKeyRef.current = "";
    setListEpoch((value) => value + 1);
    setDetailEpoch((value) => value + 1);
  };

  const closeHermesImport = () => {
    importEpochRef.current += 1;
    importControllerRef.current?.abort();
    importControllerRef.current = null;
    importAttemptRef.current = null;
    setImportState({ phase: "closed" });
  };

  const previewHermesImport = async (profileId: string | null = selectedProfileId) => {
    if (!profileId || !hermesImportAvailable || actionBusy || importing) return;
    importEpochRef.current += 1;
    const epoch = importEpochRef.current;
    importControllerRef.current?.abort();
    const controller = new AbortController();
    importControllerRef.current = controller;
    importAttemptRef.current = null;
    setImportState({ phase: "previewing", profileId });
    try {
      const preview = await importClient.previewHermesV21Import(
        profileId,
        { signal: controller.signal },
      );
      if (controller.signal.aborted || importEpochRef.current !== epoch) return;
      if (preview.state === "absent") {
        setImportState({ phase: "absent", profileId });
      } else if (isReadyHermesImportPreview(preview)) {
        setImportState({
          phase: "ready",
          profileId,
          preview,
          allowAttachmentOmission: false,
        });
      } else {
        setImportState({
          phase: "error",
          profileId,
          stage: "preview",
          message: "Hermes 历史预检结果不完整。",
        });
      }
    } catch (error) {
      if (
        controller.signal.aborted
        || isAbortError(error)
        || importEpochRef.current !== epoch
      ) return;
      setImportState({
        phase: "error",
        profileId,
        stage: "preview",
        message: safeErrorMessage(error, "无法检查 Hermes 历史。"),
      });
    } finally {
      if (importEpochRef.current === epoch) importControllerRef.current = null;
    }
  };

  const importHermesPreview = async (
    profileId: string,
    preview: ReadyHermesImportPreview,
    allowAttachmentOmission: boolean,
  ) => {
    if (actionBusy || importing || importControllerRef.current) return;
    const input = {
      expectedSnapshotFingerprint: preview.snapshotFingerprint,
      allowAttachmentOmission,
    };
    const fingerprint = JSON.stringify([profileId, input]);
    if (!importAttemptRef.current || importAttemptRef.current.fingerprint !== fingerprint) {
      importAttemptRef.current = {
        fingerprint,
        idempotencyKey: newIdempotencyKey(),
      };
    }
    const idempotencyKey = importAttemptRef.current.idempotencyKey;
    importEpochRef.current += 1;
    const epoch = importEpochRef.current;
    const controller = new AbortController();
    importControllerRef.current = controller;
    setImportState({
      phase: "importing",
      profileId,
      preview,
      allowAttachmentOmission,
    });
    try {
      const result = await importClient.importHermesV21(
        profileId,
        input,
        idempotencyKey,
        { signal: controller.signal },
      );
      if (controller.signal.aborted || importEpochRef.current !== epoch) return;
      importAttemptRef.current = null;
      setImportState({ phase: "result", profileId, result });
      refreshSessionData();
    } catch (error) {
      if (
        controller.signal.aborted
        || isAbortError(error)
        || importEpochRef.current !== epoch
      ) return;
      const message = safeErrorMessage(error, "导入 Hermes 历史失败。");
      if (
        error instanceof SessionImportApiError
        && error.code === "hermes_import_conflict"
      ) {
        importAttemptRef.current = null;
        setImportState({
          phase: "conflict",
          profileId,
          message,
          conflicts: error.conflicts,
          conflictCount: error.conflictCount,
          conflictsDropped: error.conflictsDropped,
        });
      } else {
        const sourceChanged = error instanceof SessionImportApiError
          && error.code === "hermes_import_source_changed";
        if (sourceChanged) importAttemptRef.current = null;
        setImportState({
          phase: "error",
          profileId,
          stage: "import",
          message,
          retryable: isRetryableImportError(error),
          sourceChanged,
          preview,
          allowAttachmentOmission,
        });
      }
    } finally {
      if (importEpochRef.current === epoch) importControllerRef.current = null;
    }
  };

  const confirmHermesImport = () => {
    if (importState.phase !== "ready") return;
    void importHermesPreview(
      importState.profileId,
      importState.preview,
      importState.allowAttachmentOmission,
    );
  };

  const retryHermesImport = () => {
    if (importState.phase !== "error" || importState.stage !== "import") return;
    void importHermesPreview(
      importState.profileId,
      importState.preview,
      importState.allowAttachmentOmission,
    );
  };

  const submitSearch = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (workspace.phase !== "ready" || !workspace.searchAvailable || controlsBusy) return;
    listRequestKeyRef.current = "";
    setSelectedSessionId(null);
    setAppliedQuery(queryDraft.trim());
  };

  const clearSearch = () => {
    listRequestKeyRef.current = "";
    setQueryDraft("");
    setSelectedSessionId(null);
    setAppliedQuery("");
  };

  const switchArchiveView = (nextArchived: boolean) => {
    listRequestKeyRef.current = "";
    setSelectedSessionId(null);
    setArchived(nextArchived);
  };

  const loadMoreSessions = async () => {
    if (
      list.phase !== "ready"
      || !list.nextCursor
      || list.loadingMore
      || !selectedProfileId
      || controlsBusy
    ) return;
    const cursor = list.nextCursor;
    const requestKey = list.requestKey;
    setList((current) => current.phase === "ready"
      ? { ...current, loadingMore: true, moreError: null }
      : current);
    try {
      const page = appliedQuery
        ? await client.searchSessions({
          profileId: selectedProfileId,
          query: appliedQuery,
          archived,
          cursor,
          limit: 30,
        })
        : await client.listSessions({
          profileId: selectedProfileId,
          archived,
          cursor,
          limit: 30,
        });
      setList((current) => {
        if (
          listRequestKeyRef.current !== requestKey
          || current.phase !== "ready"
          || current.requestKey !== requestKey
        ) return current;
        const seen = new Set(current.items.map((session) => session.id));
        return {
          ...current,
          items: [...current.items, ...page.items.filter((session) => !seen.has(session.id))],
          nextCursor: page.nextCursor,
          loadingMore: false,
          moreError: null,
        };
      });
    } catch (error) {
      setList((current) => current.phase === "ready"
        && current.requestKey === requestKey
        && listRequestKeyRef.current === requestKey
        ? {
          ...current,
          loadingMore: false,
          moreError: safeErrorMessage(error, "无法加载更多会话。"),
        }
        : current);
    }
  };

  const createSession = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (!selectedProfileId || controlsBusy) return;
    const title = createTitle.trim();
    const input = { profileId: selectedProfileId, title: title || null };
    const fingerprint = JSON.stringify(input);
    if (!createAttemptRef.current || createAttemptRef.current.fingerprint !== fingerprint) {
      createAttemptRef.current = { fingerprint, idempotencyKey: newIdempotencyKey() };
    }
    setActionBusy(true);
    setCreateError(null);
    try {
      const created = await client.createSession(input, createAttemptRef.current.idempotencyKey);
      createAttemptRef.current = null;
      setCreateTitle("");
      setShowCreate(false);
      setQueryDraft("");
      setAppliedQuery("");
      setArchived(false);
      setSelectedSessionId(created.value.id);
      listRequestKeyRef.current = "";
      setListEpoch((value) => value + 1);
    } catch (error) {
      setCreateError(safeErrorMessage(error, "创建会话失败。"));
    } finally {
      setActionBusy(false);
    }
  };

  const updateSelected = async (patch: { title?: string; archived?: boolean }) => {
    if (detail.phase !== "ready" || controlsBusy) return;
    const currentId = detail.resource.value.id;
    setActionBusy(true);
    setActionError(null);
    try {
      const updated = await client.updateSession(
        currentId,
        patch,
        detail.resource.etag,
      );
      if ("archived" in patch) {
        setSelectedSessionId(null);
        listRequestKeyRef.current = "";
        setListEpoch((value) => value + 1);
      } else {
        setDetail((current) => current.phase === "ready"
          ? { ...current, resource: updated }
          : current);
        setList((current) => current.phase === "ready"
          ? {
            ...current,
            items: current.items.map((session) => session.id === currentId
              ? updated.value
              : session),
          }
          : current);
        setTitleDraft(updated.value.title);
      }
    } catch (error) {
      setActionError(safeErrorMessage(error, "更新会话失败。"));
      if (error instanceof SessionApiError && error.status === 409) refreshSessionData();
    } finally {
      setActionBusy(false);
    }
  };

  const saveTitle = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (detail.phase !== "ready") return;
    const title = titleDraft.trim();
    if (!title || title === detail.resource.value.title) return;
    await updateSelected({ title });
  };

  const deleteSelected = async () => {
    if (detail.phase !== "ready" || controlsBusy || !deleteArmed) return;
    setActionBusy(true);
    setActionError(null);
    try {
      await client.deleteSession(detail.resource.value.id, detail.resource.etag);
      setSelectedSessionId(null);
      setDeleteArmed(false);
      listRequestKeyRef.current = "";
      setListEpoch((value) => value + 1);
    } catch (error) {
      setActionError(safeErrorMessage(error, "删除会话失败。"));
      if (error instanceof SessionApiError && error.status === 409) refreshSessionData();
    } finally {
      setActionBusy(false);
    }
  };

  const loadMoreMessages = async () => {
    if (
      detail.phase !== "ready"
      || !detail.nextCursor
      || detail.loadingMore
      || controlsBusy
    ) return;
    const sessionId = detail.resource.value.id;
    const cursor = detail.nextCursor;
    const snapshot = detail.snapshotLastSequence;
    setDetail((current) => current.phase === "ready"
      ? { ...current, loadingMore: true, moreError: null }
      : current);
    try {
      const page: MessagePage = await client.listMessages(sessionId, { cursor, limit: 50 });
      setDetail((current) => {
        if (current.phase !== "ready" || current.resource.value.id !== sessionId) return current;
        const firstSequence = current.messages[0]?.sequence ?? (snapshot + 1);
        if (
          page.snapshotLastSequence !== snapshot
          || (page.lastSequence !== null && page.lastSequence >= firstSequence)
        ) {
          return {
            ...current,
            loadingMore: false,
            moreError: "消息分页快照已变化，请重新加载会话。",
          };
        }
        const seen = new Set(current.messages.map((message) => message.id));
        return {
          ...current,
          messages: [...page.items.filter((message) => !seen.has(message.id)), ...current.messages],
          nextCursor: page.nextCursor,
          loadingMore: false,
          moreError: null,
        };
      });
    } catch (error) {
      setDetail((current) => current.phase === "ready"
        && current.resource.value.id === sessionId
        ? {
          ...current,
          loadingMore: false,
          moreError: safeErrorMessage(error, "无法加载更多消息。"),
        }
        : current);
    }
  };

  if (workspace.phase === "loading") {
    return (
      <StatePanel
        busy
        icon={<LoaderCircle aria-hidden="true" className="spin" size={28} />}
        title="正在连接会话服务"
      />
    );
  }

  if (workspace.phase === "desktop-required") {
    return (
      <StatePanel
        icon={<History aria-hidden="true" size={30} />}
        title="请在 SynthChat Desktop 中打开"
      >
        <p>会话历史通过受保护的本地 Rust 后端提供，浏览器模式不会接收桌面令牌。</p>
      </StatePanel>
    );
  }

  if (workspace.phase === "storage-unavailable") {
    return (
      <StatePanel
        icon={<CircleSlash2 aria-hidden="true" size={30} />}
        title="会话存储不可用"
      >
        <p>本地 Rust 会话数据库未成功初始化；Profile 与其他桌面功能仍可独立使用。</p>
        <button
          className="session-secondary-button"
          onClick={() => setWorkspaceEpoch((value) => value + 1)}
          type="button"
        >
          <RefreshCw aria-hidden="true" size={15} />
          重试
        </button>
      </StatePanel>
    );
  }

  if (workspace.phase === "error") {
    return (
      <StatePanel
        icon={<CircleSlash2 aria-hidden="true" size={30} />}
        title="会话服务连接失败"
      >
        <p role="alert">{workspace.message}</p>
        <button
          className="session-secondary-button"
          onClick={() => setWorkspaceEpoch((value) => value + 1)}
          type="button"
        >
          <RefreshCw aria-hidden="true" size={15} />
          重试
        </button>
      </StatePanel>
    );
  }

  const selectedSession = detail.phase === "ready" ? detail.resource.value : null;
  const importProfileId = importState.phase === "closed"
    ? selectedProfileId
    : importState.profileId;
  const importProfileName = profiles.find((profile) => profile.id === importProfileId)
    ?.displayName ?? importProfileId ?? "Profile";

  return (
    <div className="workspace-panel sessions-panel">
      <aside className="session-list-pane" aria-label="会话列表">
        <div className="session-list-toolbar">
          <label>
            <span>PROFILE</span>
            <select
              aria-label="按 Profile 筛选"
              disabled={controlsBusy}
              onChange={(event) => {
                closeHermesImport();
                listRequestKeyRef.current = "";
                setSelectedSessionId(null);
                setSelectedProfileId(event.target.value || null);
              }}
              value={selectedProfileId ?? ""}
            >
              {profiles.length === 0 ? <option value="">无 Profile</option> : null}
              {profiles.map((profile) => (
                <option key={profile.id} value={profile.id}>
                  {profile.displayName}{profile.isActive ? " · 活动" : ""}
                </option>
              ))}
            </select>
          </label>
          <div className="session-list-toolbar-actions">
            {workspace.hermesImportAvailable ? (
              <button
                aria-label="导入 Hermes 历史"
                aria-pressed={importState.phase !== "closed"}
                className="session-icon-button"
                disabled={!selectedProfileId || controlsBusy}
                onClick={() => void previewHermesImport()}
                title={importState.phase === "closed" ? "导入 Hermes 历史" : "重新检查 Hermes 历史"}
                type="button"
              >
                <ImportIcon aria-hidden="true" size={17} />
              </button>
            ) : null}
            <button
              aria-label={showCreate ? "关闭创建会话" : "创建会话"}
              className="session-icon-button"
              disabled={!selectedProfileId || controlsBusy}
              onClick={() => {
                setShowCreate((value) => !value);
                setCreateError(null);
              }}
              title={showCreate ? "关闭" : "创建会话"}
              type="button"
            >
              {showCreate ? <X aria-hidden="true" size={17} /> : <Plus aria-hidden="true" size={17} />}
            </button>
          </div>
          <SessionImportPanel
            actionBusy={actionBusy}
            onAllowAttachmentOmission={(allow) => {
              setImportState((current) => current.phase === "ready"
                ? { ...current, allowAttachmentOmission: allow }
                : current);
            }}
            onClose={closeHermesImport}
            onConfirm={confirmHermesImport}
            onPreview={() => {
              if (importState.phase !== "closed") {
                void previewHermesImport(importState.profileId);
              }
            }}
            onRetryImport={retryHermesImport}
            profileName={importProfileName}
            state={importState}
          />
        </div>

        {showCreate ? (
          <form className="session-create-form" onSubmit={(event) => void createSession(event)}>
            <label>
              <span>标题（可选）</span>
              <input
                disabled={controlsBusy}
                onChange={(event) => setCreateTitle(event.target.value)}
                placeholder="新会话"
                value={createTitle}
              />
            </label>
            {createError ? <p role="alert">{createError}</p> : null}
            <button className="session-primary-button" disabled={controlsBusy} type="submit">
              <Plus aria-hidden="true" size={15} />
              创建
            </button>
          </form>
        ) : null}

        <form className="session-search" onSubmit={submitSearch} role="search">
          <Search aria-hidden="true" size={15} />
          <input
            aria-label="搜索会话"
            disabled={!workspace.searchAvailable || controlsBusy}
            onChange={(event) => setQueryDraft(event.target.value)}
            placeholder={workspace.searchAvailable ? "搜索标题、ID 或消息" : "当前后端未启用搜索"}
            value={queryDraft}
          />
          <button aria-label="执行搜索" disabled={!workspace.searchAvailable || controlsBusy} title="搜索" type="submit">
            <Search aria-hidden="true" size={14} />
          </button>
          <button
            aria-label="清除搜索"
            className={queryDraft || appliedQuery ? "" : "is-hidden"}
            disabled={controlsBusy || (!queryDraft && !appliedQuery)}
            onClick={clearSearch}
            title="清除搜索"
            type="button"
          >
            <X aria-hidden="true" size={14} />
          </button>
        </form>

        <div className="session-view-switch" aria-label="会话状态筛选" role="group">
          <button
            aria-pressed={!archived}
            className={!archived ? "active" : ""}
            disabled={controlsBusy}
            onClick={() => switchArchiveView(false)}
            type="button"
          >
            当前
          </button>
          <button
            aria-pressed={archived}
            className={archived ? "active" : ""}
            disabled={controlsBusy}
            onClick={() => switchArchiveView(true)}
            type="button"
          >
            已归档
          </button>
          <small>{workspace.searchMode.toUpperCase()}</small>
        </div>

        <div className="session-list-scroll">
          {list.phase === "loading" || list.phase === "idle" ? (
            <div className="session-list-state" aria-busy="true">
              <LoaderCircle aria-hidden="true" className="spin" size={20} />
              <span>加载会话</span>
            </div>
          ) : list.phase === "error" ? (
            <div className="session-list-state">
              <p role="alert">{list.message}</p>
              <button className="session-secondary-button" onClick={() => setListEpoch((value) => value + 1)} type="button">
                重试
              </button>
            </div>
          ) : list.items.length === 0 ? (
            <div className="session-list-state">
              <History aria-hidden="true" size={22} />
              <strong>{appliedQuery ? "没有搜索结果" : archived ? "没有已归档会话" : "还没有会话"}</strong>
              <span>{appliedQuery ? "当前关键词没有命中。" : "当前筛选范围内没有会话。"}</span>
            </div>
          ) : (
            <ul className="session-list">
              {list.items.map((session) => (
                <li key={session.id}>
                  <button
                    aria-current={session.id === selectedSessionId ? "true" : undefined}
                    className={session.id === selectedSessionId ? "session-list-item active" : "session-list-item"}
                    disabled={controlsBusy}
                    onClick={() => {
                      setActionError(null);
                      setSelectedSessionId(session.id);
                    }}
                    type="button"
                  >
                    <span className="session-list-title">
                      <strong>{session.title}</strong>
                      <time dateTime={session.updatedAt}>{formatTimestamp(session.updatedAt)}</time>
                    </span>
                    <span className="session-list-preview">
                      {session.match?.snippet || session.preview || "空会话"}
                    </span>
                    <span className="session-list-meta">
                      <small>{session.messageCount} 条消息</small>
                      <small>{session.model || session.source}</small>
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          )}
          {list.phase === "ready" && list.nextCursor ? (
            <button
              className="session-load-more"
              disabled={list.loadingMore || controlsBusy}
              onClick={() => void loadMoreSessions()}
              type="button"
            >
              {list.loadingMore ? <LoaderCircle aria-hidden="true" className="spin" size={14} /> : null}
              加载更多会话
            </button>
          ) : null}
          {list.phase === "ready" && list.moreError ? <p className="session-inline-error" role="alert">{list.moreError}</p> : null}
        </div>
      </aside>

      <section className="session-detail-pane" aria-label="会话详情">
        {detail.phase === "idle" ? (
          <StatePanel icon={<MessageSquareText aria-hidden="true" size={28} />} title="选择一个会话">
            <p>当前未选择会话。</p>
          </StatePanel>
        ) : detail.phase === "loading" ? (
          <StatePanel
            busy
            icon={<LoaderCircle aria-hidden="true" className="spin" size={26} />}
            title="正在加载消息"
          />
        ) : detail.phase === "error" ? (
          <StatePanel icon={<CircleSlash2 aria-hidden="true" size={28} />} title="会话加载失败">
            <p role="alert">{detail.message}</p>
            <button
              className="session-secondary-button"
              onClick={() => {
                setActionError(null);
                setDetailEpoch((value) => value + 1);
              }}
              type="button"
            >
              重新加载
            </button>
          </StatePanel>
        ) : (
          <>
            <header className="session-detail-toolbar">
              <form onSubmit={(event) => void saveTitle(event)}>
                <label className="sr-only" htmlFor="selected-session-title">会话标题</label>
                <input
                  disabled={controlsBusy}
                  id="selected-session-title"
                  onChange={(event) => setTitleDraft(event.target.value)}
                  value={titleDraft}
                />
                <button
                  aria-label="保存会话标题"
                  className="session-icon-button"
                  disabled={controlsBusy || !titleDraft.trim() || titleDraft.trim() === detail.resource.value.title}
                  title="保存标题"
                  type="submit"
                >
                  <Save aria-hidden="true" size={16} />
                </button>
              </form>
              <div className="session-toolbar-actions">
                <button
                  className="session-secondary-button"
                  disabled={controlsBusy}
                  onClick={() => void updateSelected({ archived: !detail.resource.value.archived })}
                  type="button"
                >
                  {detail.resource.value.archived
                    ? <ArchiveRestore aria-hidden="true" size={15} />
                    : <Archive aria-hidden="true" size={15} />}
                  {detail.resource.value.archived ? "恢复" : "归档"}
                </button>
                <button
                  aria-label="删除会话"
                  aria-pressed={deleteArmed}
                  className="session-icon-button danger"
                  disabled={controlsBusy}
                  onClick={() => setDeleteArmed((value) => !value)}
                  title={deleteArmed ? "取消删除" : "删除会话"}
                  type="button"
                >
                  {deleteArmed ? <X aria-hidden="true" size={16} /> : <Trash2 aria-hidden="true" size={16} />}
                </button>
                {deleteArmed ? (
                  <button
                    className="session-danger-button"
                    disabled={controlsBusy}
                    onClick={() => void deleteSelected()}
                    type="button"
                  >
                    确认删除
                  </button>
                ) : null}
                <button
                  className="session-primary-button"
                  disabled={detail.resource.value.archived || controlsBusy || !onContinue}
                  onClick={() => onContinue?.(detail.resource.value)}
                  type="button"
                >
                  <Send aria-hidden="true" size={15} />
                  继续对话
                </button>
              </div>
            </header>
            <div className="session-detail-meta">
              <span>{selectedSession?.messageCount ?? 0} 条消息</span>
              <span>{selectedSession?.model || "未指定模型"}</span>
              <span>{selectedSession?.source || "desktop"}</span>
              <time dateTime={selectedSession?.updatedAt}>更新于 {selectedSession ? formatTimestamp(selectedSession.updatedAt) : "-"}</time>
            </div>
            {actionError ? <p className="session-action-error" role="alert">{actionError}</p> : null}
            <div className="session-message-scroll">
              <SessionMessageTimeline messages={detail.messages} />
              {detail.nextCursor ? (
                <button
                  className="session-load-more message-page"
                  disabled={detail.loadingMore || controlsBusy}
                  onClick={() => void loadMoreMessages()}
                  type="button"
                >
                  {detail.loadingMore ? <LoaderCircle aria-hidden="true" className="spin" size={14} /> : null}
                  加载更多消息
                </button>
              ) : null}
              {detail.moreError ? <p className="session-inline-error" role="alert">{detail.moreError}</p> : null}
            </div>
          </>
        )}
      </section>
    </div>
  );
}
