import {
  Bot,
  Check,
  CircleSlash2,
  HelpCircle,
  LoaderCircle,
  MessageSquarePlus,
  RefreshCw,
  Send,
  ShieldAlert,
  Square,
  TriangleAlert,
  Wrench,
  X,
} from "lucide-react";
import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type FormEvent,
} from "react";
import { DesktopConnectionError } from "../../api/desktopConnection";
import {
  ProfileApiError,
  profilesApi,
  type ProfilesApi,
  type ProfileSummary,
} from "../../api/profiles";
import {
  SessionApiError,
  sessionsApi,
  type Message,
  type Session,
  type SessionsApi,
} from "../../api/sessions";
import {
  RunApiError,
  type ApprovalDecision,
  type CreateRunInput,
  type PendingApprovalAction,
  type PendingClarificationAction,
} from "../../api/runs";
import { MarkdownContent } from "../../components/MarkdownContent";
import { SessionMessageTimeline } from "../sessions/SessionsWorkspace";
import { useChatRuns } from "./ChatRunProvider";
import type {
  ChatAsyncToolDelivery,
  ChatPendingAsyncToolDelivery,
} from "./runReducer";
import "./chat.css";

type ProfileClient = Pick<ProfilesApi, "getCapabilities" | "listProfiles">;

type WorkspaceState =
  | { phase: "loading" }
  | { phase: "desktop-required" }
  | { phase: "unavailable"; message: string }
  | { phase: "error"; message: string }
  | { phase: "ready" };

interface Continuation {
  id: string;
  title: string;
}

interface SendAttempt {
  sessionId: string;
  text: string;
  input: CreateRunInput;
}

interface ActionCapabilities {
  activeRunDiscovery: boolean;
  approvals: boolean;
  clarifications: boolean;
  asyncToolDelivery: boolean;
}

type ActiveRunDiscoveryState =
  | { key: null; phase: "idle" }
  | { key: string; phase: "loading" | "ready" }
  | { key: string; phase: "error"; message: string };

type PendingAction = PendingApprovalAction | PendingClarificationAction;

type ActionSubmission = {
  key: string;
  phase: "submitting" | "submitted" | "error";
  message: string | null;
} | null;

interface SessionPendingAsyncToolDelivery {
  key: string;
  delivery: ChatPendingAsyncToolDelivery;
}

interface SessionAsyncToolDelivery {
  key: string;
  delivery: ChatAsyncToolDelivery;
}

export interface ChatWorkspaceProps {
  continuation?: Continuation | null;
  client?: SessionsApi;
  profileClient?: ProfileClient;
}

function isAbortError(error: unknown): boolean {
  return error instanceof DOMException && error.name === "AbortError";
}

function errorMessage(error: unknown, fallback: string): string {
  if (error instanceof RunApiError) {
    switch (error.code) {
      case "session_busy": return "该会话已有回复正在生成。";
      case "session_archived": return "归档会话需要先恢复后才能继续。";
      case "engine_unavailable": return "当前 Profile 尚未配置可用的模型或密钥。";
      case "secret_storage_unavailable": return "系统密钥链暂时不可用。";
      case "capacity_exceeded": return "本地推理队列已满，请稍后重试。";
      default: return error.requestId ? `${error.message} 请求 ID：${error.requestId}` : error.message;
    }
  }
  if (error instanceof SessionApiError || error instanceof ProfileApiError) {
    return error.requestId ? `${error.message} 请求 ID：${error.requestId}` : error.message;
  }
  if (error instanceof DesktopConnectionError) {
    return error.kind === "desktop_unavailable"
      ? "受保护的聊天功能需要在 SynthChat Desktop 中使用。"
      : "本地 Rust 后端无法连接。";
  }
  return fallback;
}

function isRetryableSendError(error: unknown): boolean {
  return (error instanceof RunApiError && (error.kind === "network" || error.retryable))
    || (error instanceof DesktopConnectionError && error.kind === "network");
}

function pendingActionErrorMessage(error: unknown): string {
  if (error instanceof RunApiError) {
    if (error.kind === "network") return "提交未送达本地后端，请重试或取消本次回复。";
    if (error.status === 409) return "操作状态已发生变化，请重试；若仍失败可取消本次回复。";
    if (error.status === 422) return "后端暂时无法处理此操作，请重试或取消本次回复。";
  }
  return errorMessage(error, "操作提交失败，请重试或取消本次回复。");
}

function actionKey(runId: string, action: PendingAction): string {
  return action.kind === "approval"
    ? `approval:${runId}:${action.approvalId}`
    : `clarification:${runId}:${action.requestId}`;
}

function approvalLabel(decision: ApprovalDecision["decision"]): string {
  switch (decision) {
    case "once": return "允许一次";
    case "session": return "本会话允许";
    case "always": return "始终允许";
    case "deny": return "拒绝";
  }
}

function asyncDeliveryLabel(delivery: {
  delivery: "completion" | "watch";
  status: "starting" | "running" | "exited" | "killed" | "lost" | "failed_start";
  exitCode?: number | null;
  matchedPatternCount?: number;
}): string {
  if (delivery.delivery === "watch") {
    return `已匹配 ${delivery.matchedPatternCount ?? 1} 个监测条件`;
  }
  switch (delivery.status) {
    case "exited": return delivery.exitCode === undefined || delivery.exitCode === null
      ? "后台任务已完成"
      : `后台任务已完成，退出码 ${delivery.exitCode}`;
    case "killed": return "后台任务已停止";
    case "lost": return "后台任务状态已丢失";
    case "failed_start": return "后台任务未能启动";
    case "starting": return "后台任务正在启动";
    case "running": return "后台任务仍在运行";
  }
}

function PendingActionPanel({
  action,
  actionEnabled,
  cancelPending,
  onAnswer,
  onCancel,
  onDecision,
  submission,
}: {
  action: PendingAction;
  actionEnabled: boolean;
  cancelPending: boolean;
  onAnswer: (answer: string) => void;
  onCancel: () => void;
  onDecision: (decision: ApprovalDecision["decision"]) => void;
  submission: Exclude<ActionSubmission, null> | null;
}) {
  const [answer, setAnswer] = useState("");
  const busy = submission?.phase === "submitting";
  const submitted = submission?.phase === "submitted";
  const controlsDisabled = !actionEnabled || busy || submitted;
  const headingId = action.kind === "approval"
    ? `approval-${action.approvalId}-heading`
    : `clarification-${action.requestId}-heading`;

  return (
    <article
      aria-busy={busy || undefined}
      aria-labelledby={headingId}
      className="chat-pending-action"
    >
      <header>
        {action.kind === "approval"
          ? <ShieldAlert aria-hidden="true" size={17} />
          : <HelpCircle aria-hidden="true" size={17} />}
        <div>
          <h3 id={headingId}>
            {action.kind === "approval" ? "需要确认工具调用" : "Hermes 需要补充信息"}
          </h3>
          <span>{action.kind === "approval" ? action.toolName : "澄清问题"}</span>
        </div>
      </header>

      {action.kind === "approval" ? (
        <div className="chat-action-detail">
          <span>调用摘要</span>
          <p>{action.inputSummary ?? "未提供参数摘要"}</p>
        </div>
      ) : <p className="chat-clarification-question">{action.question}</p>}

      {!actionEnabled ? (
        <p className="chat-action-readonly" role="status">
          当前后端未启用{action.kind === "approval" ? "审批" : "澄清"}提交，可取消本次回复。
        </p>
      ) : action.kind === "approval" ? (
        <fieldset className="chat-action-fieldset" disabled={controlsDisabled}>
          <legend>选择处理方式</legend>
          <div className="chat-action-choices">
            {action.choices.map((decision) => (
              <button
                className={decision === "deny" ? "is-danger" : "is-primary"}
                disabled={controlsDisabled}
                key={decision}
                onClick={() => onDecision(decision)}
                type="button"
              >
                {decision === "deny"
                  ? <X aria-hidden="true" size={15} />
                  : <Check aria-hidden="true" size={15} />}
                {approvalLabel(decision)}
              </button>
            ))}
          </div>
        </fieldset>
      ) : action.choices.length > 0 ? (
        <fieldset className="chat-action-fieldset" disabled={controlsDisabled}>
          <legend>选择回答</legend>
          <div className="chat-action-choices">
            {action.choices.map((choice) => (
              <button
                className="is-primary"
                disabled={controlsDisabled}
                key={choice}
                onClick={() => onAnswer(choice)}
                type="button"
              >
                {choice}
              </button>
            ))}
          </div>
        </fieldset>
      ) : (
        <form
          className="chat-clarification-form"
          onSubmit={(event) => {
            event.preventDefault();
            const value = answer.trim();
            if (value && !controlsDisabled) onAnswer(value);
          }}
        >
          <label>
            <span>回答</span>
            <textarea
              disabled={controlsDisabled}
              maxLength={10_000}
              onChange={(event) => setAnswer(event.target.value)}
              rows={3}
              value={answer}
            />
          </label>
          <button className="chat-action-submit" disabled={controlsDisabled || !answer.trim()} type="submit">
            <Send aria-hidden="true" size={15} />
            提交回答
          </button>
        </form>
      )}

      {submission?.phase === "error" ? (
        <p className="chat-action-error" role="alert">
          <TriangleAlert aria-hidden="true" size={14} />
          <span>{submission.message}</span>
        </p>
      ) : submission ? (
        <p className="chat-action-progress" role="status">
          <LoaderCircle aria-hidden="true" className="spin" size={14} />
          {busy ? "正在提交" : "已提交，等待后端确认"}
        </p>
      ) : null}

      <button
        className="chat-action-cancel"
        disabled={cancelPending}
        onClick={onCancel}
        type="button"
      >
        <Square aria-hidden="true" fill="currentColor" size={11} />
        取消本次回复
      </button>
    </article>
  );
}

function newRequestId(): string {
  if (typeof globalThis.crypto?.randomUUID === "function") {
    return globalThis.crypto.randomUUID();
  }
  const bytes = new Uint8Array(16);
  globalThis.crypto.getRandomValues(bytes);
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

function mergeMessages(history: Message[], additions: Message[]): Message[] {
  const messages = new Map(history.map((message) => [message.id, message]));
  for (const message of additions) messages.set(message.id, message);
  return Array.from(messages.values()).sort((left, right) => left.sequence - right.sequence);
}

function ChatState({
  state,
  continuation,
  onRetry,
}: {
  state: Exclude<WorkspaceState, { phase: "ready" }>;
  continuation: Continuation | null;
  onRetry: () => void;
}) {
  const loading = state.phase === "loading";
  const Icon = loading ? LoaderCircle : state.phase === "error" ? TriangleAlert : CircleSlash2;
  const title = loading
    ? "正在连接聊天服务"
    : state.phase === "desktop-required"
      ? "请在 Desktop 中打开"
      : state.phase === "unavailable"
        ? "聊天引擎尚未就绪"
        : "聊天服务连接失败";
  const message = loading
    ? ""
    : state.phase === "desktop-required"
      ? "受保护的 Run 与 SSE 接口只接受桌面应用签发的会话令牌。"
      : state.message;
  return (
    <div
      aria-busy={loading || undefined}
      className="chat-state"
      role={state.phase === "error" ? "alert" : "status"}
    >
      <Icon className={loading ? "spin" : undefined} aria-hidden="true" size={28} />
      <h2>{title}</h2>
      {message ? <p>{message}</p> : null}
      {continuation ? <p>已选择会话“{continuation.title}”（{continuation.id}）</p> : null}
      {!loading && state.phase !== "desktop-required" ? (
        <button className="chat-secondary-button" onClick={onRetry} type="button">
          <RefreshCw aria-hidden="true" size={15} />
          重试
        </button>
      ) : null}
    </div>
  );
}

export function ChatWorkspace({
  continuation = null,
  client = sessionsApi,
  profileClient = profilesApi,
}: ChatWorkspaceProps) {
  const {
    state: runRegistry,
    discoverActiveRuns,
    createRun,
    cancelRun,
    resolveApproval,
    answerClarification,
  } = useChatRuns();
  const [workspace, setWorkspace] = useState<WorkspaceState>({ phase: "loading" });
  const [actionCapabilities, setActionCapabilities] = useState<ActionCapabilities>({
    activeRunDiscovery: false,
    approvals: false,
    clarifications: false,
    asyncToolDelivery: false,
  });
  const [activeRunDiscovery, setActiveRunDiscovery] = useState<ActiveRunDiscoveryState>({
    key: null,
    phase: "idle",
  });
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [profileId, setProfileId] = useState<string | null>(null);
  const [sessions, setSessions] = useState<Session[]>([]);
  const [sessionId, setSessionId] = useState<string | null>(continuation?.id ?? null);
  const [history, setHistory] = useState<Message[]>([]);
  const [historyLoading, setHistoryLoading] = useState(false);
  const [draft, setDraft] = useState("");
  const [actionError, setActionError] = useState<string | null>(null);
  const [actionSubmission, setActionSubmission] = useState<ActionSubmission>(null);
  const [sending, setSending] = useState(false);
  const [creatingSession, setCreatingSession] = useState(false);
  const [epoch, setEpoch] = useState(0);
  const timelineRef = useRef<HTMLDivElement>(null);
  const sendAttemptRef = useRef<SendAttempt | null>(null);
  const latestRunId = sessionId ? runRegistry.latestRunIdBySession[sessionId] : undefined;
  const activeRun = latestRunId ? runRegistry.runs[latestRunId] : undefined;
  const pendingAction = activeRun?.pendingAction ?? null;
  const pendingActionKey = activeRun && pendingAction
    ? actionKey(activeRun.run.id, pendingAction)
    : null;
  const runBusy = activeRun
    ? !["completed", "cancelled", "failed"].includes(activeRun.run.status)
    : false;
  const { pendingAsyncDeliveries, deliveredAsyncTools } = useMemo(() => {
    const pending = new Map<string, SessionPendingAsyncToolDelivery>();
    const delivered = new Map<string, SessionAsyncToolDelivery>();

    if (sessionId) {
      for (const runState of Object.values(runRegistry.runs)) {
        if (runState.run.sessionId !== sessionId) continue;
        for (const delivery of Object.values(runState.pendingAsyncToolDeliveries)) {
          const key = `${runState.run.id}:${delivery.callId}`;
          pending.set(key, { key, delivery });
        }
        for (const delivery of Object.values(runState.asyncToolDeliveries)) {
          const key = `${runState.run.id}:${delivery.processId}`;
          delivered.set(key, { key, delivery });
        }
      }
    }

    return {
      pendingAsyncDeliveries: [...pending.values()],
      deliveredAsyncTools: [...delivered.values()],
    };
  }, [runRegistry.runs, sessionId]);
  const activeRunDiscoveryKey = actionCapabilities.activeRunDiscovery
    && workspace.phase === "ready"
    && profileId
    && sessionId
    ? JSON.stringify([profileId, sessionId])
    : null;
  const activeRunDiscoveryReady = activeRunDiscoveryKey === null
    || (activeRunDiscovery.key === activeRunDiscoveryKey && activeRunDiscovery.phase === "ready");
  const activeRunDiscoveryPending = activeRunDiscoveryKey !== null
    && (
      activeRunDiscovery.key !== activeRunDiscoveryKey
      || activeRunDiscovery.phase === "loading"
    );
  const activeRunDiscoveryError = activeRunDiscoveryKey !== null
    && activeRunDiscovery.key === activeRunDiscoveryKey
    && activeRunDiscovery.phase === "error"
    ? activeRunDiscovery.message
    : null;
  const displayedMessages = useMemo(
    () => mergeMessages(history, activeRun?.committedMessages ?? []),
    [activeRun?.committedMessages, history],
  );

  useEffect(() => {
    if (!continuation) return;
    setSessionId(continuation.id);
  }, [continuation]);

  useEffect(() => {
    sendAttemptRef.current = null;
  }, [sessionId]);

  useEffect(() => {
    setActionSubmission(null);
  }, [pendingActionKey]);

  useEffect(() => {
    const controller = new AbortController();
    setWorkspace({ phase: "loading" });
    setActionCapabilities({
      activeRunDiscovery: false,
      approvals: false,
      clarifications: false,
      asyncToolDelivery: false,
    });
    setActionError(null);
    void (async () => {
      try {
        const [capabilities, availableProfiles, continuedSession] = await Promise.all([
          profileClient.getCapabilities({ signal: controller.signal }),
          profileClient.listProfiles({ signal: controller.signal }),
          continuation
            ? client.getSession(continuation.id, { signal: controller.signal })
            : Promise.resolve(null),
        ]);
        if (controller.signal.aborted) return;
        setActionCapabilities({
          activeRunDiscovery: capabilities.extensions.activeRunDiscovery,
          approvals: capabilities.engine.features.approvals,
          clarifications: capabilities.engine.features.clarifications,
          asyncToolDelivery: capabilities.engine.features.asyncToolDelivery,
        });
        if (!capabilities.engine.features.runStreaming) {
          setWorkspace({ phase: "unavailable", message: "Rust Run/SSE 能力当前未启用。" });
          return;
        }
        const selectedProfile = continuedSession
          ? availableProfiles.find((profile) => profile.id === continuedSession.value.profileId)
          : undefined;
        const activeProfile = selectedProfile
          ?? availableProfiles.find((profile) => profile.isActive)
          ?? availableProfiles[0]
          ?? null;
        setProfiles(availableProfiles);
        setProfileId((current) => current && availableProfiles.some((profile) => profile.id === current)
          ? current
          : activeProfile?.id ?? null);
        setWorkspace({ phase: "ready" });
      } catch (error) {
        if (controller.signal.aborted || isAbortError(error)) return;
        if (error instanceof DesktopConnectionError && error.kind === "desktop_unavailable") {
          setWorkspace({ phase: "desktop-required" });
        } else {
          setWorkspace({ phase: "error", message: errorMessage(error, "无法连接聊天服务。") });
        }
      }
    })();
    return () => controller.abort();
  }, [client, continuation, epoch, profileClient]);

  useEffect(() => {
    if (workspace.phase !== "ready" || !profileId) {
      setSessions([]);
      return undefined;
    }
    const controller = new AbortController();
    void (async () => {
      try {
        const page = await client.listSessions(
          { profileId, archived: false, limit: 50 },
          { signal: controller.signal },
        );
        if (controller.signal.aborted) return;
        setSessions(page.items);
        setSessionId((current) => {
          if (continuation?.id) return continuation.id;
          return current && page.items.some((session) => session.id === current)
            ? current
            : page.items[0]?.id ?? null;
        });
      } catch (error) {
        if (controller.signal.aborted || isAbortError(error)) return;
        setActionError(errorMessage(error, "无法加载会话。"));
      }
    })();
    return () => controller.abort();
  }, [client, continuation?.id, profileId, workspace.phase]);

  useEffect(() => {
    if (!activeRunDiscoveryKey || !profileId || !sessionId) {
      setActiveRunDiscovery({ key: null, phase: "idle" });
      return undefined;
    }
    const controller = new AbortController();
    const key = activeRunDiscoveryKey;
    setActiveRunDiscovery({ key, phase: "loading" });
    void discoverActiveRuns(profileId, sessionId, { signal: controller.signal })
      .then(() => {
        if (!controller.signal.aborted) setActiveRunDiscovery({ key, phase: "ready" });
      })
      .catch((error: unknown) => {
        if (!controller.signal.aborted && !isAbortError(error)) {
          setActiveRunDiscovery({
            key,
            phase: "error",
            message: errorMessage(error, "无法恢复进行中的对话。"),
          });
        }
      });
    return () => controller.abort();
  }, [activeRunDiscoveryKey, discoverActiveRuns, profileId, sessionId]);

  useEffect(() => {
    if (!sessionId || workspace.phase !== "ready") {
      setHistory([]);
      return undefined;
    }
    const controller = new AbortController();
    setHistoryLoading(true);
    setActionError(null);
    void client.listMessages(sessionId, { limit: 100 }, { signal: controller.signal })
      .then((page) => {
        if (!controller.signal.aborted) setHistory(page.items);
      })
      .catch((error: unknown) => {
        if (!controller.signal.aborted && !isAbortError(error)) {
          setActionError(errorMessage(error, "无法加载会话消息。"));
        }
      })
      .finally(() => {
        if (!controller.signal.aborted) setHistoryLoading(false);
      });
    return () => controller.abort();
  }, [client, sessionId, workspace.phase]);

  useEffect(() => {
    const frame = globalThis.requestAnimationFrame(() => {
      const timeline = timelineRef.current;
      if (timeline) timeline.scrollTop = timeline.scrollHeight;
    });
    return () => globalThis.cancelAnimationFrame(frame);
  }, [
    activeRun?.draft?.reasoning,
    activeRun?.draft?.text,
    deliveredAsyncTools.length,
    displayedMessages.length,
    pendingAsyncDeliveries.length,
  ]);

  useEffect(() => {
    if (!activeRun?.terminal || activeRun.committedMessages.length === 0) return;
    setHistory((current) => mergeMessages(current, activeRun.committedMessages));
  }, [activeRun?.committedMessages, activeRun?.terminal]);

  const handleCreateSession = async () => {
    if (!profileId || creatingSession) return;
    setCreatingSession(true);
    setActionError(null);
    try {
      const created = await client.createSession({ profileId }, newRequestId());
      setSessions((current) => [created.value, ...current]);
      setSessionId(created.value.id);
      setHistory([]);
    } catch (error) {
      setActionError(errorMessage(error, "无法创建会话。"));
    } finally {
      setCreatingSession(false);
    }
  };

  const handleSend = async (event: FormEvent) => {
    event.preventDefault();
    const text = draft.trim();
    if (!sessionId || !text || sending || runBusy || !activeRunDiscoveryReady) return;
    let attempt = sendAttemptRef.current;
    if (!attempt || attempt.sessionId !== sessionId || attempt.text !== text) {
      attempt = {
        sessionId,
        text,
        input: {
          clientRequestId: newRequestId(),
          message: { text, fileIds: [] },
        },
      };
      sendAttemptRef.current = attempt;
    }
    setSending(true);
    setActionError(null);
    try {
      await createRun(sessionId, attempt.input);
      if (sendAttemptRef.current === attempt) sendAttemptRef.current = null;
      setDraft("");
    } catch (error) {
      if (!isRetryableSendError(error) && sendAttemptRef.current === attempt) {
        sendAttemptRef.current = null;
      }
      setActionError(errorMessage(error, "无法发起对话。"));
    } finally {
      setSending(false);
    }
  };

  const handleApproval = async (decision: ApprovalDecision["decision"]) => {
    const action = activeRun?.pendingAction;
    if (!activeRun || action?.kind !== "approval" || !actionCapabilities.approvals) return;
    const key = actionKey(activeRun.run.id, action);
    if (
      actionSubmission?.key === key
      && (actionSubmission.phase === "submitting" || actionSubmission.phase === "submitted")
    ) return;
    setActionSubmission({ key, phase: "submitting", message: null });
    try {
      await resolveApproval(activeRun.run.id, action.approvalId, { decision, reason: null });
      setActionSubmission((current) => current?.key === key
        ? { key, phase: "submitted", message: null }
        : current);
    } catch (error) {
      setActionSubmission((current) => current?.key === key
        ? { key, phase: "error", message: pendingActionErrorMessage(error) }
        : current);
    }
  };

  const handleClarification = async (answer: string) => {
    const action = activeRun?.pendingAction;
    if (!activeRun || action?.kind !== "clarification" || !actionCapabilities.clarifications) return;
    const key = actionKey(activeRun.run.id, action);
    if (
      actionSubmission?.key === key
      && (actionSubmission.phase === "submitting" || actionSubmission.phase === "submitted")
    ) return;
    setActionSubmission({ key, phase: "submitting", message: null });
    try {
      await answerClarification(activeRun.run.id, action.requestId, { answer });
      setActionSubmission((current) => current?.key === key
        ? { key, phase: "submitted", message: null }
        : current);
    } catch (error) {
      setActionSubmission((current) => current?.key === key
        ? { key, phase: "error", message: pendingActionErrorMessage(error) }
        : current);
    }
  };

  const handleCancelActiveRun = () => {
    if (!activeRun) return;
    void cancelRun(activeRun.run.id).catch((error: unknown) => {
      setActionError(errorMessage(error, "无法取消当前回复。"));
    });
  };

  if (workspace.phase !== "ready") {
    return (
      <ChatState
        continuation={continuation}
        state={workspace}
        onRetry={() => setEpoch((value) => value + 1)}
      />
    );
  }

  return (
    <div className="chat-panel">
      <header className="chat-toolbar">
        <label>
          <span>Profile</span>
          <select
            aria-label="聊天 Profile"
            disabled={runBusy}
            onChange={(event) => {
              setProfileId(event.target.value);
              setSessionId(null);
              setHistory([]);
            }}
            value={profileId ?? ""}
          >
            {profiles.map((profile) => <option key={profile.id} value={profile.id}>{profile.displayName}</option>)}
          </select>
        </label>
        <label className="chat-session-select">
          <span>会话</span>
          <select
            aria-label="当前会话"
            disabled={runBusy || sessions.length === 0}
            onChange={(event) => setSessionId(event.target.value || null)}
            value={sessionId ?? ""}
          >
            {sessions.length === 0 ? <option value="">暂无会话</option> : null}
            {sessions.map((session) => <option key={session.id} value={session.id}>{session.title}</option>)}
          </select>
        </label>
        <button
          aria-busy={creatingSession || undefined}
          aria-label={creatingSession ? "正在新建会话" : "新建会话"}
          className="chat-icon-button"
          disabled={!profileId || creatingSession || runBusy}
          onClick={() => void handleCreateSession()}
          title="新建会话"
          type="button"
        >
          {creatingSession
            ? <LoaderCircle aria-hidden="true" className="spin" size={17} />
            : <MessageSquarePlus aria-hidden="true" size={17} />}
        </button>
        <div className="chat-run-status" aria-live="polite">
          <span className={runBusy || activeRunDiscoveryPending ? "is-live" : ""} />
          {activeRun?.streamStatus === "reconnecting"
            ? "正在重连"
            : activeRun?.run.status === "waitingApproval"
              ? "等待审批"
              : activeRun?.run.status === "waitingClarification"
                ? "等待回答"
                : runBusy
                  ? "正在生成"
                  : activeRunDiscoveryPending
                    ? "正在恢复"
                    : activeRunDiscoveryError
                      ? "恢复失败"
                      : "就绪"}
        </div>
      </header>

      <div className="chat-timeline" ref={timelineRef}>
        {historyLoading ? (
          <div className="chat-inline-state"><LoaderCircle className="spin" size={18} />加载消息</div>
        ) : <SessionMessageTimeline messages={displayedMessages} />}

        {activeRun?.draft ? (
          <article className="chat-stream-message" aria-live="polite">
            <header><Bot aria-hidden="true" size={16} /><strong>Hermes</strong><span>流式响应</span></header>
            {activeRun.draft.reasoning ? (
              <details className="chat-reasoning" open={!activeRun.draft.text}>
                <summary>推理过程</summary>
                <p>{activeRun.draft.reasoning}</p>
              </details>
            ) : null}
            {activeRun.draft.text ? (
              <MarkdownContent className="chat-stream-text">{activeRun.draft.text}</MarkdownContent>
            ) : <div className="chat-stream-text"><span className="chat-caret">正在思考</span></div>}
            {Object.values(activeRun.draft.tools).length > 0 ? (
              <div className="chat-tool-list">
                {Object.values(activeRun.draft.tools).map((tool) => (
                  <div key={tool.callId}><Wrench size={13} /><strong>{tool.name}</strong><span>{tool.progressMessage ?? tool.status}</span></div>
                ))}
              </div>
            ) : null}
          </article>
        ) : null}

        {actionCapabilities.asyncToolDelivery
        && (pendingAsyncDeliveries.length > 0 || deliveredAsyncTools.length > 0) ? (
          <section className="chat-async-deliveries" aria-live="polite">
            {pendingAsyncDeliveries.map(({ delivery, key }) => (
              <div className="chat-async-delivery" key={`pending:${key}`}>
                <Wrench aria-hidden="true" size={14} />
                <span>后台终端任务</span>
                <span>等待完成通知</span>
              </div>
            ))}
            {deliveredAsyncTools.map(({ delivery, key }) => (
              <div className="chat-async-delivery" key={`delivered:${key}`}>
                <Wrench aria-hidden="true" size={14} />
                <span>后台终端任务</span>
                <span>{asyncDeliveryLabel(delivery)}</span>
              </div>
            ))}
          </section>
        ) : null}

        {activeRun && pendingAction && pendingActionKey ? (
          <PendingActionPanel
            action={pendingAction}
            actionEnabled={pendingAction.kind === "approval"
              ? actionCapabilities.approvals
              : actionCapabilities.clarifications}
            cancelPending={activeRun.cancelPending}
            key={pendingActionKey}
            onAnswer={(answer) => void handleClarification(answer)}
            onCancel={handleCancelActiveRun}
            onDecision={(decision) => void handleApproval(decision)}
            submission={actionSubmission?.key === pendingActionKey ? actionSubmission : null}
          />
        ) : null}
      </div>

      <footer className="chat-composer-band">
        {activeRunDiscoveryError || actionError || activeRun?.streamError || activeRun?.cancelError ? (
          <div className="chat-error" role="alert">
            <TriangleAlert aria-hidden="true" size={14} />
            <span>{activeRunDiscoveryError ?? actionError ?? activeRun?.cancelError ?? activeRun?.streamError}</span>
          </div>
        ) : null}
        <form className="chat-composer" onSubmit={(event) => void handleSend(event)}>
          <textarea
            aria-label="消息"
            disabled={!sessionId || sending || !activeRunDiscoveryReady}
            maxLength={1_000_000}
            onChange={(event) => {
              const value = event.target.value;
              if (sendAttemptRef.current && value.trim() !== sendAttemptRef.current.text) {
                sendAttemptRef.current = null;
              }
              setDraft(value);
            }}
            onKeyDown={(event) => {
              if (event.key === "Enter" && !event.shiftKey) {
                event.preventDefault();
                event.currentTarget.form?.requestSubmit();
              }
            }}
            placeholder={sessionId ? "输入消息" : "新建或选择一个会话"}
            rows={2}
            value={draft}
          />
          {runBusy && activeRun ? (
            <button
              aria-label="停止生成"
              className="chat-send-button is-stop"
              disabled={activeRun.cancelPending}
              onClick={handleCancelActiveRun}
              title="停止生成"
              type="button"
            >
              <Square fill="currentColor" size={13} />
            </button>
          ) : (
            <button
              aria-label="发送消息"
              className="chat-send-button"
              disabled={!sessionId || !draft.trim() || sending || !activeRunDiscoveryReady}
              title="发送"
              type="submit"
            >
              {sending ? <LoaderCircle className="spin" size={17} /> : <Send size={17} />}
            </button>
          )}
        </form>
        <div className="chat-composer-meta">
          <span>{activeRun?.usage ? `${activeRun.usage.totalTokens.toLocaleString("zh-CN")} tokens` : "本地 Run/SSE"}</span>
          <span>{draft.length.toLocaleString("zh-CN")} / 1,000,000</span>
        </div>
      </footer>
    </div>
  );
}
