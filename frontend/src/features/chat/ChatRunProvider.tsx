import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useReducer,
  useRef,
  type ReactNode,
} from "react";
import { DesktopConnectionError } from "../../api/desktopConnection";
import {
  RunApiError,
  runsApi as defaultRunsApi,
  type ActiveRun,
  type ActionAccepted,
  type ApprovalDecision,
  type ClarificationAnswer,
  type CreateRunInput,
  type Run,
  type RunsApi,
} from "../../api/runs";
import {
  runEventsApi as defaultRunEventsApi,
  type RunEventsApi,
} from "../../api/sse";
import {
  sessionsApi as defaultSessionsApi,
  type SessionsApi,
} from "../../api/sessions";
import { readChatRuntimeConfig } from "../../config/runtimeConfig/chat";
import {
  chatRunFromAccepted,
  chatRunFromDiscovery,
  chatRunsReducer,
  hasPendingAsyncToolDeliveries,
  initialChatRunsState,
  type ChatRunsAction,
  type ChatRunsState,
  type ChatRunState,
} from "./runReducer";

interface CreateAttempt {
  fingerprint: string;
  idempotencyKey: string;
  promise?: Promise<ChatRunState>;
}

interface ActiveStream {
  controller: AbortController;
  promise: Promise<void>;
}

interface ActionAttempt {
  fingerprint: string;
  promise: Promise<ActionAccepted>;
}

export interface ChatRunProviderProps {
  children: ReactNode;
  runsApi?: RunsApi;
  runEventsApi?: RunEventsApi;
  sessionsApi?: Pick<SessionsApi, "listMessages">;
  reconnectDelayMs?: number;
  maxReconnectAttempts?: number;
  maxReconnectDelayMs?: number;
  runStatusPollIntervalMs?: number;
}

export interface ChatRunsContextValue {
  state: ChatRunsState;
  discoverActiveRuns(
    profileId: string,
    sessionId: string,
    options?: { signal?: AbortSignal },
  ): Promise<ChatRunState[]>;
  createRun(sessionId: string, input: CreateRunInput): Promise<ChatRunState>;
  cancelRun(runId: string): Promise<Run>;
  resolveApproval(
    runId: string,
    approvalId: string,
    decision: ApprovalDecision,
  ): Promise<ActionAccepted>;
  answerClarification(
    runId: string,
    requestId: string,
    answer: ClarificationAnswer,
  ): Promise<ActionAccepted>;
}

const ChatRunsContext = createContext<ChatRunsContextValue | null>(null);

function canonicalizeJson(value: unknown, ancestors: Set<object>): unknown {
  if (Array.isArray(value)) {
    if (ancestors.has(value)) throw new TypeError("Run input must not contain cycles.");
    ancestors.add(value);
    const result = value.map((item) => item === undefined
      ? null
      : canonicalizeJson(item, ancestors));
    ancestors.delete(value);
    return result;
  }
  if (value !== null && typeof value === "object") {
    if (ancestors.has(value)) throw new TypeError("Run input must not contain cycles.");
    ancestors.add(value);
    const record = value as Record<string, unknown>;
    const result: Record<string, unknown> = {};
    for (const key of Object.keys(record).sort()) {
      if (record[key] !== undefined) result[key] = canonicalizeJson(record[key], ancestors);
    }
    ancestors.delete(value);
    return result;
  }
  return value;
}

function inputFingerprint(input: unknown): string {
  return JSON.stringify(canonicalizeJson(input, new Set<object>()));
}

function actionAttemptKey(runId: string, action: NonNullable<Run["pendingAction"]>): string {
  return action.kind === "approval"
    ? `approval:${runId}:${action.approvalId}`
    : `clarification:${runId}:${action.requestId}`;
}

let fallbackKeySequence = 0;

function newIdempotencyKey(): string {
  const uuid = globalThis.crypto?.randomUUID?.();
  if (uuid) return `run-${uuid}`;
  fallbackKeySequence += 1;
  return `run-${Date.now().toString(36)}-${fallbackKeySequence.toString(36)}`;
}

function isNetworkError(error: unknown): boolean {
  return (error instanceof RunApiError || error instanceof DesktopConnectionError)
    && error.kind === "network";
}

function isRetryableStreamError(error: unknown): boolean {
  return isNetworkError(error)
    || (error instanceof RunApiError && error.retryable)
    || (error instanceof DesktopConnectionError && error.kind === "desktop_unavailable");
}

function isRetryableCreateError(error: unknown): boolean {
  return isNetworkError(error) || (error instanceof RunApiError && error.retryable);
}

function isEventHistoryExpired(error: unknown): boolean {
  return error instanceof RunApiError
    && error.kind === "http"
    && error.status === 409
    && error.code === "event_history_expired";
}

function isTerminalRun(run: Run): boolean {
  return run.status === "completed" || run.status === "cancelled" || run.status === "failed";
}

function isAbortError(error: unknown): boolean {
  return error instanceof DOMException
    ? error.name === "AbortError"
    : Boolean(error && typeof error === "object" && "name" in error
      && (error as { name?: unknown }).name === "AbortError");
}

function abortError(): DOMException {
  return new DOMException("The request was aborted.", "AbortError");
}

function validateDiscoveryOwner(
  activeRun: ActiveRun,
  profileId: string,
  sessionId: string,
): void {
  if (
    activeRun.run.profileId !== profileId
    || activeRun.run.sessionId !== sessionId
    || activeRun.userMessage.role !== "user"
    || activeRun.userMessage.sessionId !== sessionId
  ) {
    throw new RunApiError(
      "invalid_response",
      "Active Run discovery returned a resource owned by another Profile or Session.",
    );
  }
}

function errorMessage(error: unknown, fallback: string): string {
  return error instanceof Error && error.message.length > 0 ? error.message : fallback;
}

function reconnectDelay(delayMs: number, signal: AbortSignal): Promise<void> {
  if (delayMs <= 0 || signal.aborted) return Promise.resolve();
  return new Promise((resolve) => {
    const timer = globalThis.setTimeout(finish, delayMs);
    function finish() {
      globalThis.clearTimeout(timer);
      signal.removeEventListener("abort", finish);
      resolve();
    }
    signal.addEventListener("abort", finish, { once: true });
  });
}

export function reconnectBackoffMs(
  initialDelayMs: number,
  maximumDelayMs: number,
  attempt: number,
): number {
  const exponent = Math.min(31, Math.max(0, Math.floor(attempt)));
  return Math.min(maximumDelayMs, initialDelayMs * (2 ** exponent));
}

export function ChatRunProvider({
  children,
  runsApi = defaultRunsApi,
  runEventsApi = defaultRunEventsApi,
  sessionsApi = defaultSessionsApi,
  reconnectDelayMs,
  maxReconnectAttempts,
  maxReconnectDelayMs,
  runStatusPollIntervalMs,
}: ChatRunProviderProps) {
  const runtimeConfig = readChatRuntimeConfig();
  const configuredReconnectDelayMs = reconnectDelayMs
    ?? runtimeConfig.reconnectInitialDelayMs;
  const configuredMaxReconnectAttempts = maxReconnectAttempts
    ?? runtimeConfig.reconnectMaxAttempts;
  const configuredMaxReconnectDelayMs = maxReconnectDelayMs
    ?? runtimeConfig.reconnectMaxDelayMs;
  const configuredRunStatusPollIntervalMs = runStatusPollIntervalMs
    ?? runtimeConfig.runStatusPollIntervalMs;
  const [state, dispatch] = useReducer(chatRunsReducer, initialChatRunsState);
  const stateRef = useRef(state);
  const mountedRef = useRef(true);
  const attemptsRef = useRef(new Map<string, CreateAttempt>());
  const actionAttemptsRef = useRef(new Map<string, ActionAttempt>());
  const streamsRef = useRef(new Map<string, ActiveStream>());

  // Async stream iterations may be batched into one render, so keep a synchronous reducer mirror.
  const apply = useCallback((action: ChatRunsAction): ChatRunsState => {
    const previous = stateRef.current;
    stateRef.current = chatRunsReducer(previous, action);
    for (const [runId, previousRun] of Object.entries(previous.runs)) {
      const previousPending = previousRun.pendingAction;
      if (!previousPending) continue;
      const nextPending = stateRef.current.runs[runId]?.pendingAction;
      if (!nextPending || actionAttemptKey(runId, nextPending) !== actionAttemptKey(runId, previousPending)) {
        actionAttemptsRef.current.delete(actionAttemptKey(runId, previousPending));
      }
    }
    if (mountedRef.current) dispatch(action);
    return stateRef.current;
  }, []);

  const startStream = useCallback((runId: string, sessionId: string): void => {
    if (!mountedRef.current || streamsRef.current.has(runId)) return;

    const controller = new AbortController();
    const maxAttempts = Number.isFinite(configuredMaxReconnectAttempts)
      ? Math.max(0, Math.floor(configuredMaxReconnectAttempts))
      : runtimeConfig.reconnectMaxAttempts;
    const initialDelayMs = Number.isFinite(configuredReconnectDelayMs)
      ? Math.max(0, configuredReconnectDelayMs)
      : runtimeConfig.reconnectInitialDelayMs;
    const maximumDelayMs = Number.isFinite(configuredMaxReconnectDelayMs)
      ? Math.max(initialDelayMs, configuredMaxReconnectDelayMs)
      : Math.max(initialDelayMs, runtimeConfig.reconnectMaxDelayMs);
    const statusPollIntervalMs = Number.isFinite(configuredRunStatusPollIntervalMs)
      ? Math.max(500, configuredRunStatusPollIntervalMs)
      : runtimeConfig.runStatusPollIntervalMs;
    let active!: ActiveStream;

    const reconcileFromServer = async (
      terminalOnly = false,
    ): Promise<ChatRunState | undefined> => {
      const recoveredRun = await runsApi.getRun(runId, { signal: controller.signal });
      if (terminalOnly && !isTerminalRun(recoveredRun)) {
        return stateRef.current.runs[runId];
      }
      const recoveredMessages = await sessionsApi.listMessages(
        sessionId,
        { limit: 100 },
        { signal: controller.signal },
      );
      if (controller.signal.aborted || !mountedRef.current) return undefined;
      return apply({
        type: "run.reconciled",
        runId,
        run: recoveredRun,
        messages: recoveredMessages.items,
      }).runs[runId];
    };

    const streamPromise = (async () => {
      let reconnectAttempt = 0;

      while (!controller.signal.aborted && mountedRef.current) {
        const beforeConnect = stateRef.current.runs[runId];
        if (
          !beforeConnect
          || (beforeConnect.terminal && !hasPendingAsyncToolDeliveries(beforeConnect))
          || beforeConnect.protocolError
        ) return;
        apply({ type: "stream.connecting", runId, reconnectAttempt });

        if (reconnectAttempt > 0) {
          try {
            const recovered = await reconcileFromServer();
            if (!recovered || recovered.protocolError) return;
            if (recovered.terminal && !hasPendingAsyncToolDeliveries(recovered)) {
              apply({ type: "stream.closed", runId });
              return;
            }
          } catch (recoveryError) {
            if (
              controller.signal.aborted
              || !mountedRef.current
              || isAbortError(recoveryError)
            ) return;
            if (!isRetryableStreamError(recoveryError)) {
              apply({
                type: "stream.error",
                runId,
                message: errorMessage(recoveryError, "Run recovery failed."),
              });
              return;
            }
          }
        }

        try {
          const lastSequence = stateRef.current.runs[runId]?.lastSequence ?? 0;
          for await (const event of runEventsApi.streamRunEvents(runId, {
            sessionId,
            lastSequence: lastSequence || undefined,
            signal: controller.signal,
          })) {
            if (controller.signal.aborted || !mountedRef.current) return;
            const next = apply({ type: "run.event", runId, event }).runs[runId];
            if (!next || next.protocolError) return;
            if (next.terminal && !hasPendingAsyncToolDeliveries(next)) {
              apply({ type: "stream.closed", runId });
              return;
            }
          }

          const afterClose = stateRef.current.runs[runId];
          if (!afterClose || afterClose.protocolError) {
            return;
          }
          if (afterClose.terminal && !hasPendingAsyncToolDeliveries(afterClose)) {
            apply({ type: "stream.closed", runId });
            return;
          }
        } catch (error) {
          if (controller.signal.aborted || !mountedRef.current || isAbortError(error)) return;
          if (isEventHistoryExpired(error)) {
            try {
              const recovered = await reconcileFromServer();
              if (!recovered || recovered.protocolError) return;
              if (recovered.terminal && !hasPendingAsyncToolDeliveries(recovered)) {
                apply({ type: "stream.closed", runId });
                return;
              }
              reconnectAttempt = 0;
              continue;
            } catch (recoveryError) {
              if (
                controller.signal.aborted
                || !mountedRef.current
                || isAbortError(recoveryError)
              ) return;
              if (!isRetryableStreamError(recoveryError)) {
                apply({
                  type: "stream.error",
                  runId,
                  message: errorMessage(recoveryError, "Run recovery failed."),
                });
                return;
              }
            }
          } else if (!isRetryableStreamError(error)) {
            apply({
              type: "stream.error",
              runId,
              message: errorMessage(error, "Run event stream failed."),
            });
            return;
          }
        }

        if (reconnectAttempt >= maxAttempts) {
          apply({
            type: "stream.error",
            runId,
            message: "Run event stream disconnected and could not be resumed.",
          });
          return;
        }
        const delayMs = reconnectBackoffMs(
          initialDelayMs,
          maximumDelayMs,
          reconnectAttempt,
        );
        reconnectAttempt += 1;
        await reconnectDelay(delayMs, controller.signal);
      }
    })();

    const statusPollPromise = (async () => {
      while (!controller.signal.aborted && mountedRef.current) {
        await reconnectDelay(statusPollIntervalMs, controller.signal);
        if (controller.signal.aborted || !mountedRef.current) return;
        const current = stateRef.current.runs[runId];
        if (
          !current
          || (current.terminal && !hasPendingAsyncToolDeliveries(current))
          || current.protocolError
        ) return;

        try {
          const recovered = await reconcileFromServer(true);
          if (!recovered || recovered.protocolError) return;
          if (recovered.terminal && !hasPendingAsyncToolDeliveries(recovered)) {
            apply({ type: "stream.closed", runId });
            return;
          }
        } catch (error) {
          if (controller.signal.aborted || !mountedRef.current || isAbortError(error)) return;
          if (!isRetryableStreamError(error)) {
            apply({
              type: "stream.error",
              runId,
              message: errorMessage(error, "Run status recovery failed."),
            });
            return;
          }
        }
      }
    })();

    const promise = (async () => {
      try {
        await Promise.race([streamPromise, statusPollPromise]);
      } finally {
        controller.abort();
        await Promise.allSettled([streamPromise, statusPollPromise]);
      }
    })().finally(() => {
      if (streamsRef.current.get(runId) === active) streamsRef.current.delete(runId);
    });

    active = { controller, promise };
    streamsRef.current.set(runId, active);
  }, [
    apply,
    configuredMaxReconnectAttempts,
    configuredMaxReconnectDelayMs,
    configuredReconnectDelayMs,
    configuredRunStatusPollIntervalMs,
    runEventsApi,
    runsApi,
    runtimeConfig.reconnectInitialDelayMs,
    runtimeConfig.reconnectMaxAttempts,
    runtimeConfig.reconnectMaxDelayMs,
    runtimeConfig.runStatusPollIntervalMs,
    sessionsApi,
  ]);

  const discoverActiveRuns = useCallback(async (
    profileId: string,
    sessionId: string,
    options: { signal?: AbortSignal } = {},
  ): Promise<ChatRunState[]> => {
    if (options.signal?.aborted) throw abortError();
    const discovered = await runsApi.listActiveRuns(
      profileId,
      { sessionId },
      { signal: options.signal },
    );
    if (options.signal?.aborted) throw abortError();

    for (const activeRun of discovered.items) {
      validateDiscoveryOwner(activeRun, profileId, sessionId);
      const existing = stateRef.current.runs[activeRun.run.id];
      if (
        existing
        && (
          existing.run.profileId !== profileId
          || existing.run.sessionId !== sessionId
          || !existing.committedMessages.some((message) => (
            message.id === activeRun.userMessage.id
            && message.sessionId === sessionId
            && message.sequence === activeRun.userMessage.sequence
          ))
        )
      ) {
        throw new RunApiError(
          "invalid_response",
          "Active Run discovery conflicts with an existing Run owner or user Message.",
        );
      }
    }

    const recovered: ChatRunState[] = [];
    for (const activeRun of discovered.items) {
      if (options.signal?.aborted) throw abortError();
      if (!mountedRef.current) {
        recovered.push(chatRunFromDiscovery(activeRun));
        continue;
      }
      const next = apply({ type: "run.discovered", activeRun }).runs[activeRun.run.id];
      if (!next || next.protocolError) {
        throw new RunApiError(
          "invalid_response",
          "Active Run discovery could not be merged into the current chat state.",
        );
      }
      recovered.push(next);
      startStream(activeRun.run.id, activeRun.run.sessionId);
    }
    return recovered;
  }, [apply, runsApi, startStream]);

  const createRun = useCallback((
    sessionId: string,
    input: CreateRunInput,
  ): Promise<ChatRunState> => {
    const fingerprint = inputFingerprint(input);
    let attempt = attemptsRef.current.get(sessionId);
    if (!attempt || attempt.fingerprint !== fingerprint) {
      attempt = { fingerprint, idempotencyKey: newIdempotencyKey() };
      attemptsRef.current.set(sessionId, attempt);
    }
    if (attempt.promise) return attempt.promise;

    const currentAttempt = attempt;
    const promise = runsApi.createRun(
      sessionId,
      input,
      currentAttempt.idempotencyKey,
    ).then((accepted) => {
      if (!mountedRef.current) return chatRunFromAccepted(accepted);
      const next = apply({ type: "run.accepted", accepted });
      startStream(accepted.run.id, accepted.run.sessionId);
      return next.runs[accepted.run.id] ?? chatRunFromAccepted(accepted);
    }).catch((error: unknown) => {
      if (!isRetryableCreateError(error) && attemptsRef.current.get(sessionId) === currentAttempt) {
        attemptsRef.current.delete(sessionId);
      }
      throw error;
    }).finally(() => {
      if (attemptsRef.current.get(sessionId) === currentAttempt) {
        currentAttempt.promise = undefined;
      }
    });

    currentAttempt.promise = promise;
    return promise;
  }, [apply, runsApi, startStream]);

  const cancelRun = useCallback(async (runId: string): Promise<Run> => {
    if (!stateRef.current.runs[runId]) throw new Error(`Unknown Run: ${runId}`);
    apply({ type: "cancel.requested", runId });
    try {
      const run = await runsApi.cancelRun(runId);
      if (mountedRef.current) {
        const next = apply({ type: "run.synced", runId, run }).runs[runId];
        if (next?.terminal && !hasPendingAsyncToolDeliveries(next)) {
          streamsRef.current.get(runId)?.controller.abort();
        }
      }
      return run;
    } catch (error) {
      if (mountedRef.current) {
        apply({
          type: "cancel.failed",
          runId,
          message: errorMessage(error, "Run cancellation failed."),
        });
      }
      throw error;
    }
  }, [apply, runsApi]);

  const resolveApproval = useCallback((
    runId: string,
    approvalId: string,
    decision: ApprovalDecision,
  ): Promise<ActionAccepted> => {
    const pending = stateRef.current.runs[runId]?.pendingAction;
    if (pending?.kind !== "approval" || pending.approvalId !== approvalId) {
      return Promise.reject(new Error("The Run is not waiting for this approval."));
    }
    if (!pending.choices.includes(decision.decision)) {
      return Promise.reject(new Error("The approval decision is not currently available."));
    }

    const key = actionAttemptKey(runId, pending);
    const fingerprint = inputFingerprint(decision);
    const active = actionAttemptsRef.current.get(key);
    if (active) {
      return active.fingerprint === fingerprint
        ? active.promise
        : Promise.reject(new Error("A different decision is already being submitted."));
    }

    let attempt!: ActionAttempt;
    const promise = Promise.resolve()
      .then(() => runsApi.resolveApproval(runId, approvalId, decision))
      .catch((error: unknown) => {
        if (actionAttemptsRef.current.get(key) === attempt) actionAttemptsRef.current.delete(key);
        throw error;
      });
    attempt = { fingerprint, promise };
    actionAttemptsRef.current.set(key, attempt);
    return promise;
  }, [runsApi]);

  const answerClarification = useCallback((
    runId: string,
    requestId: string,
    answer: ClarificationAnswer,
  ): Promise<ActionAccepted> => {
    const pending = stateRef.current.runs[runId]?.pendingAction;
    if (pending?.kind !== "clarification" || pending.requestId !== requestId) {
      return Promise.reject(new Error("The Run is not waiting for this clarification."));
    }
    if (pending.choices.length > 0 && !pending.choices.includes(answer.answer)) {
      return Promise.reject(new Error("The clarification answer is not currently available."));
    }

    const key = actionAttemptKey(runId, pending);
    const fingerprint = inputFingerprint(answer);
    const active = actionAttemptsRef.current.get(key);
    if (active) {
      return active.fingerprint === fingerprint
        ? active.promise
        : Promise.reject(new Error("A different answer is already being submitted."));
    }

    let attempt!: ActionAttempt;
    const promise = Promise.resolve()
      .then(() => runsApi.answerClarification(runId, requestId, answer))
      .catch((error: unknown) => {
        if (actionAttemptsRef.current.get(key) === attempt) actionAttemptsRef.current.delete(key);
        throw error;
      });
    attempt = { fingerprint, promise };
    actionAttemptsRef.current.set(key, attempt);
    return promise;
  }, [runsApi]);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      for (const stream of streamsRef.current.values()) stream.controller.abort();
      streamsRef.current.clear();
      attemptsRef.current.clear();
      actionAttemptsRef.current.clear();
    };
  }, []);

  const value = useMemo<ChatRunsContextValue>(() => ({
    state,
    discoverActiveRuns,
    createRun,
    cancelRun,
    resolveApproval,
    answerClarification,
  }), [
    answerClarification,
    cancelRun,
    createRun,
    discoverActiveRuns,
    resolveApproval,
    state,
  ]);

  return <ChatRunsContext.Provider value={value}>{children}</ChatRunsContext.Provider>;
}

export function useChatRuns(): ChatRunsContextValue {
  const context = useContext(ChatRunsContext);
  if (!context) throw new Error("useChatRuns must be used within a ChatRunProvider.");
  return context;
}
