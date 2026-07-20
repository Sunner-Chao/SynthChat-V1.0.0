// @vitest-environment jsdom

import { act, cleanup, render, waitFor } from "@testing-library/react";
import { useEffect, type ReactNode } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { DesktopConnectionError } from "../../api/desktopConnection";
import type { Message, SessionsApi } from "../../api/sessions";
import {
  RunApiError,
  type ActiveRun,
  type ActionAccepted,
  type ApprovalDecision,
  type ClarificationAnswer,
  type CreateRunInput,
  type Run,
  type RunAccepted,
  type RunEventName,
  type RunEventPayload,
  type RunsApi,
  type Usage,
} from "../../api/runs";
import type {
  RunEventsApi,
  RunStreamEvent,
  StreamRunEventsOptions,
} from "../../api/sse";
import {
  ChatRunProvider,
  reconnectBackoffMs,
  useChatRuns,
  type ChatRunProviderProps,
  type ChatRunsContextValue,
} from "./ChatRunProvider";

describe("reconnectBackoffMs", () => {
  it("grows exponentially and remains capped", () => {
    expect([0, 1, 2, 3, 20].map((attempt) => (
      reconnectBackoffMs(250, 2_000, attempt)
    ))).toEqual([250, 500, 1_000, 2_000, 2_000]);
  });
});

const NOW = "2026-07-16T08:00:00Z";
const USAGE: Usage = {
  promptTokens: 8,
  completionTokens: 4,
  totalTokens: 12,
  cost: null,
};
const INPUT: CreateRunInput = {
  clientRequestId: "client-request-1",
  message: { text: "Hello", fileIds: [] },
  modelOverride: null,
  reasoningEffort: "medium",
};
const RUN: Run = {
  id: "run-1",
  sessionId: "session-1",
  profileId: "default",
  status: "running",
  lastSequence: 0,
  messageId: null,
  usage: null,
  error: null,
  pendingAction: null,
  createdAt: NOW,
  updatedAt: NOW,
};
const USER_MESSAGE: Message = {
  id: "user-1",
  sessionId: "session-1",
  sequence: 1,
  role: "user",
  parts: [{ type: "text", text: "Hello" }],
  reasoning: null,
  toolCalls: [],
  usage: null,
  createdAt: NOW,
};
const INTERRUPTED_PROBLEM = {
  type: "urn:synthchat:problem:run-interrupted",
  title: "Run interrupted",
  status: 500,
  code: "run_interrupted",
  requestId: "request-interrupted-1",
  retryable: false,
  detail: "The backend restarted before this Run completed.",
};
const ASSISTANT_MESSAGE: Message = {
  id: "assistant-1",
  sessionId: "session-1",
  sequence: 2,
  role: "assistant",
  parts: [{ type: "text", text: "Hello back" }],
  reasoning: "Prepared a concise answer.",
  toolCalls: [],
  usage: USAGE,
  createdAt: NOW,
};
const ACCEPTED: RunAccepted = {
  run: { ...RUN, status: "running" },
  disposition: "started",
  queueItemId: null,
  userMessage: USER_MESSAGE,
  sessionRevision: "session_rev_1",
};
const DISCOVERED: ActiveRun = {
  run: { ...RUN, lastSequence: 3 },
  queueItemId: null,
  userMessage: USER_MESSAGE,
  sessionRevision: "session_rev_3",
};

const APPROVAL_PENDING: Extract<NonNullable<Run["pendingAction"]>, { kind: "approval" }> = {
  kind: "approval",
  approvalId: "approval-1",
  callId: "call-1",
  toolName: "terminal",
  inputSummary: "Run a redacted command",
  choices: ["once", "deny"],
  expiresAt: "2026-07-16T08:05:00Z",
};

const CLARIFICATION_PENDING: Extract<NonNullable<Run["pendingAction"]>, { kind: "clarification" }> = {
  kind: "clarification",
  requestId: "clarification-1",
  question: "Which environment?",
  choices: ["staging", "production"],
};

function acceptedWithPending(pendingAction: NonNullable<Run["pendingAction"]>): RunAccepted {
  return {
    ...ACCEPTED,
    disposition: "replayed",
    run: {
      ...RUN,
      status: pendingAction.kind === "approval" ? "waitingApproval" : "waitingClarification",
      pendingAction,
    },
  };
}

function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, reject, resolve };
}

function streamEvent(event: RunEventName, sequence: number, data: unknown): RunStreamEvent {
  return {
    id: `run-1:${sequence}`,
    event,
    payload: {
      schemaVersion: 1,
      sequence,
      runId: "run-1",
      sessionId: "session-1",
      occurredAt: NOW,
      data,
    } as RunEventPayload,
  };
}

function makeRunsApi(overrides: Partial<RunsApi> = {}): RunsApi {
  const actionAccepted: ActionAccepted = { accepted: true };
  const api: RunsApi = {
    listActiveRuns: vi.fn(async () => ({ items: [] })),
    createRun: vi.fn(async () => ACCEPTED),
    getRun: vi.fn(async () => RUN),
    cancelRun: vi.fn(async (): Promise<Run> => ({ ...RUN, status: "cancelling" })),
    resolveApproval: vi.fn(async (
      _runId: string,
      _approvalId: string,
      _decision: ApprovalDecision,
    ) => actionAccepted),
    answerClarification: vi.fn(async (
      _runId: string,
      _requestId: string,
      _answer: ClarificationAnswer,
    ) => actionAccepted),
  };
  return Object.assign(api, overrides);
}

function idleRunEventsApi(onSignal?: (signal: AbortSignal) => void): RunEventsApi {
  return {
    streamRunEvents: vi.fn((_runId: string, options: StreamRunEventsOptions = {}) => (
      (async function* idleStream() {
        const signal = options.signal;
        if (!signal) return;
        onSignal?.(signal);
        if (!signal.aborted) {
          await new Promise<void>((resolve) => signal.addEventListener("abort", () => resolve(), {
            once: true,
          }));
        }
      })()
    )),
  };
}

let latestContext: ChatRunsContextValue | null = null;

function ContextProbe({ onUnmount }: { onUnmount?: () => void }) {
  latestContext = useChatRuns();
  useEffect(() => () => onUnmount?.(), [onUnmount]);
  return null;
}

function ProviderHarness({
  children = <ContextProbe />,
  sessionsApi = {
    listMessages: async () => ({
      items: [],
      nextCursor: null,
      snapshotLastSequence: 0,
      firstSequence: null,
      lastSequence: null,
    }),
  },
  ...props
}: Omit<ChatRunProviderProps, "children"> & { children?: ReactNode }) {
  return <ChatRunProvider {...props} sessionsApi={sessionsApi}>{children}</ChatRunProvider>;
}

afterEach(() => {
  latestContext = null;
  cleanup();
});

describe("ChatRunProvider", () => {
  it("requires consumers to be mounted below the Provider", () => {
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    const preventJsdomReport = (event: ErrorEvent) => event.preventDefault();
    window.addEventListener("error", preventJsdomReport);
    try {
      expect(() => render(<ContextProbe />)).toThrowError(
        "useChatRuns must be used within a ChatRunProvider.",
      );
    } finally {
      window.removeEventListener("error", preventJsdomReport);
      consoleError.mockRestore();
    }
  });

  it("returns the same Promise for concurrent input and reuses its key after a network retry", async () => {
    let resolveAccepted!: (accepted: RunAccepted) => void;
    const pendingAccepted = new Promise<RunAccepted>((resolve) => {
      resolveAccepted = resolve;
    });
    const createRun = vi.fn()
      .mockReturnValueOnce(pendingAccepted)
      .mockRejectedValueOnce(new RunApiError("network", "offline", { retryable: true }))
      .mockResolvedValueOnce(ACCEPTED);
    const runsApi = makeRunsApi({ createRun });
    const eventsApi = idleRunEventsApi();
    const view = render(<ProviderHarness runsApi={runsApi} runEventsApi={eventsApi} />);

    const first = latestContext!.createRun("session-1", INPUT);
    const concurrent = latestContext!.createRun("session-1", {
      reasoningEffort: "medium",
      modelOverride: null,
      message: { fileIds: [], text: "Hello" },
      clientRequestId: "client-request-1",
    });
    expect(concurrent).toBe(first);
    expect(createRun).toHaveBeenCalledTimes(1);

    await act(async () => resolveAccepted(ACCEPTED));
    await expect(first).resolves.toMatchObject({ run: { id: "run-1" } });
    view.unmount();

    const retryEventsApi = idleRunEventsApi();
    render(<ProviderHarness runsApi={runsApi} runEventsApi={retryEventsApi} />);
    await expect(latestContext!.createRun("session-1", INPUT)).rejects.toMatchObject({
      kind: "network",
    });
    await act(async () => {
      await latestContext!.createRun("session-1", INPUT);
    });

    expect(createRun).toHaveBeenCalledTimes(3);
    expect(createRun.mock.calls[1]![2]).toBe(createRun.mock.calls[2]![2]);
    expect(createRun.mock.calls[0]![2]).not.toBe(createRun.mock.calls[1]![2]);
  });

  it("discovers one owner-scoped Run, keeps its snapshot fields, and deduplicates its stream", async () => {
    let streamSignal: AbortSignal | undefined;
    const listActiveRuns = vi.fn(async () => ({ items: [DISCOVERED] }));
    const cancelledRun: Run = {
      ...DISCOVERED.run,
      status: "cancelled",
      updatedAt: NOW,
    };
    const runsApi = makeRunsApi({
      listActiveRuns,
      cancelRun: vi.fn(async () => cancelledRun),
    });
    const streamRunEvents = vi.fn((
      _runId: string,
      options: StreamRunEventsOptions = {},
    ): AsyncIterable<RunStreamEvent> => (async function* idle() {
      streamSignal = options.signal;
      if (options.signal && !options.signal.aborted) {
        await new Promise<void>((resolve) => options.signal?.addEventListener("abort", () => resolve(), {
          once: true,
        }));
      }
    })());
    render(<ProviderHarness runsApi={runsApi} runEventsApi={{ streamRunEvents }} />);

    await act(async () => {
      await latestContext!.discoverActiveRuns("default", "session-1");
      await latestContext!.discoverActiveRuns("default", "session-1");
    });
    await waitFor(() => expect(streamSignal).toBeDefined());

    expect(listActiveRuns).toHaveBeenCalledTimes(2);
    expect(listActiveRuns).toHaveBeenLastCalledWith(
      "default",
      { sessionId: "session-1" },
      { signal: undefined },
    );
    expect(streamRunEvents).toHaveBeenCalledTimes(1);
    expect(streamRunEvents).toHaveBeenCalledWith("run-1", expect.objectContaining({
      sessionId: "session-1",
      lastSequence: undefined,
      signal: expect.any(AbortSignal),
    }));
    expect(latestContext!.state.runs["run-1"]).toMatchObject({
      disposition: "replayed",
      sessionRevision: "session_rev_3",
      lastSequence: 0,
      serverLastSequence: 3,
      recoveredAcrossGap: false,
      committedMessages: [USER_MESSAGE],
    });

    await act(async () => {
      await latestContext!.cancelRun("run-1");
    });
    expect(streamSignal?.aborted).toBe(true);
    expect(latestContext!.state.runs["run-1"]?.terminal?.kind).toBe("cancelled");
  });

  it("resumes a discovered Run from its cursor and reconnects without duplicating messages", async () => {
    const streamRunEvents = vi.fn((
      _runId: string,
      _options: StreamRunEventsOptions = {},
    ): AsyncIterable<RunStreamEvent> => {
      const callNumber = streamRunEvents.mock.calls.length;
      return (async function* events() {
        if (callNumber === 1) {
          yield streamEvent("run.started", 1, { profileId: "default" });
          yield streamEvent("message.started", 2, {
            messageId: ASSISTANT_MESSAGE.id,
            role: "assistant",
          });
          yield streamEvent("message.delta", 3, {
            messageId: ASSISTANT_MESSAGE.id,
            delta: "Hello ",
          });
          throw new RunApiError("http", "backend restarting", {
            retryable: true,
            status: 503,
          });
        }
        yield streamEvent("message.delta", 4, {
          messageId: ASSISTANT_MESSAGE.id,
          delta: "back",
        });
        yield streamEvent("message.completed", 5, {
          message: ASSISTANT_MESSAGE,
          sessionRevision: "session_rev_5",
        });
        yield streamEvent("run.completed", 6, {
          usage: USAGE,
          messageId: ASSISTANT_MESSAGE.id,
        });
      })();
    });
    render(
      <ProviderHarness
        maxReconnectAttempts={1}
        reconnectDelayMs={0}
        runEventsApi={{ streamRunEvents }}
        runsApi={makeRunsApi({
          getRun: vi.fn(async () => ({ ...RUN, lastSequence: 3 })),
          listActiveRuns: vi.fn(async () => ({ items: [DISCOVERED] })),
        })}
      />,
    );

    await act(async () => {
      await latestContext!.discoverActiveRuns("default", "session-1");
    });
    await waitFor(() => expect(latestContext!.state.runs["run-1"]?.terminal?.kind).toBe(
      "completed",
    ));

    expect(streamRunEvents).toHaveBeenCalledTimes(2);
    expect(streamRunEvents.mock.calls[0]![1]).toMatchObject({ lastSequence: undefined });
    expect(streamRunEvents.mock.calls[1]![1]).toMatchObject({ lastSequence: 3 });
    expect(latestContext!.state.runs["run-1"]).toMatchObject({
      lastSequence: 6,
      sessionRevision: "session_rev_5",
      protocolError: null,
      streamStatus: "closed",
    });
    expect(latestContext!.state.runs["run-1"]?.committedMessages).toEqual([
      USER_MESSAGE,
      ASSISTANT_MESSAGE,
    ]);
  });

  it("replays tool progress and a waiting approval from the complete persisted journal", async () => {
    const waiting: ActiveRun = {
      ...DISCOVERED,
      run: {
        ...DISCOVERED.run,
        status: "waitingApproval",
        lastSequence: 5,
        pendingAction: APPROVAL_PENDING,
      },
    };
    const streamRunEvents = vi.fn((
      _runId: string,
      options: StreamRunEventsOptions = {},
    ): AsyncIterable<RunStreamEvent> => (async function* events() {
      yield streamEvent("run.started", 1, { profileId: "default" });
      yield streamEvent("message.started", 2, {
        messageId: ASSISTANT_MESSAGE.id,
        role: "assistant",
      });
      yield streamEvent("tool.started", 3, {
        callId: APPROVAL_PENDING.callId,
        name: APPROVAL_PENDING.toolName,
        inputSummary: APPROVAL_PENDING.inputSummary,
      });
      yield streamEvent("tool.progress", 4, {
        callId: APPROVAL_PENDING.callId,
        message: "Waiting for approval",
        progress: 0.5,
      });
      const { kind: _kind, ...approval } = APPROVAL_PENDING;
      yield streamEvent("approval.required", 5, approval);
      if (options.signal && !options.signal.aborted) {
        await new Promise<void>((resolve) => options.signal?.addEventListener("abort", () => resolve(), {
          once: true,
        }));
      }
    })());
    render(
      <ProviderHarness
        runEventsApi={{ streamRunEvents }}
        runsApi={makeRunsApi({ listActiveRuns: vi.fn(async () => ({ items: [waiting] })) })}
      />,
    );

    await act(async () => {
      await latestContext!.discoverActiveRuns("default", "session-1");
    });
    await waitFor(() => expect(latestContext!.state.runs["run-1"]?.lastSequence).toBe(5));

    expect(streamRunEvents).toHaveBeenCalledWith("run-1", expect.objectContaining({
      lastSequence: undefined,
      sessionId: "session-1",
    }));
    expect(latestContext!.state.runs["run-1"]).toMatchObject({
      run: { status: "waitingApproval", pendingAction: APPROVAL_PENDING },
      pendingAction: APPROVAL_PENDING,
      protocolError: null,
      draft: {
        messageId: ASSISTANT_MESSAGE.id,
        tools: {
          [APPROVAL_PENDING.callId]: {
            status: "running",
            progress: 0.5,
            progressMessage: "Waiting for approval",
          },
        },
      },
    });
  });

  it("reconciles an interrupted terminal Run before reopening its event stream", async () => {
    const streamRunEvents = vi.fn((
      _runId: string,
      _options: StreamRunEventsOptions = {},
    ): AsyncIterable<RunStreamEvent> => (
      async function* events() {
        yield streamEvent("run.started", 1, { profileId: "default" });
        yield streamEvent("message.started", 2, {
          messageId: ASSISTANT_MESSAGE.id,
          role: "assistant",
        });
        yield streamEvent("message.delta", 3, {
          messageId: ASSISTANT_MESSAGE.id,
          delta: "Partial response",
        });
        throw new DesktopConnectionError(
          "desktop_unavailable",
          "managed backend restarting",
        );
      }
    )());
    const getRun = vi.fn(async (): Promise<Run> => ({
      ...RUN,
      status: "failed",
      lastSequence: 4,
      error: INTERRUPTED_PROBLEM,
    }));
    const listMessages = vi.fn(async () => ({
      items: [USER_MESSAGE],
      nextCursor: null,
      snapshotLastSequence: 1,
      firstSequence: 1,
      lastSequence: 1,
    }));

    render(
      <ProviderHarness
        maxReconnectAttempts={1}
        reconnectDelayMs={0}
        runEventsApi={{ streamRunEvents }}
        runsApi={makeRunsApi({ getRun })}
        sessionsApi={{ listMessages }}
      />,
    );

    await act(async () => {
      await latestContext!.createRun("session-1", INPUT);
    });
    await waitFor(() => expect(latestContext!.state.runs["run-1"]?.terminal?.kind).toBe(
      "failed",
    ));

    expect(streamRunEvents).toHaveBeenCalledTimes(1);
    expect(getRun).toHaveBeenCalledWith("run-1", { signal: expect.any(AbortSignal) });
    expect(listMessages).toHaveBeenCalledWith(
      "session-1",
      { limit: 100 },
      { signal: expect.any(AbortSignal) },
    );
    expect(latestContext!.state.runs["run-1"]).toMatchObject({
      streamStatus: "closed",
      streamError: "Run interrupted: The backend restarted before this Run completed.",
      draft: { text: "Partial response" },
      terminal: { kind: "failed", source: "rest" },
    });
  });

  it("rejects an Active Run owner mismatch before mutating state or opening SSE", async () => {
    const streamRunEvents = vi.fn<RunEventsApi["streamRunEvents"]>();
    const foreign: ActiveRun = {
      ...DISCOVERED,
      run: { ...DISCOVERED.run, profileId: "other" },
    };
    render(
      <ProviderHarness
        runEventsApi={{ streamRunEvents }}
        runsApi={makeRunsApi({ listActiveRuns: vi.fn(async () => ({ items: [foreign] })) })}
      />,
    );

    await expect(latestContext!.discoverActiveRuns("default", "session-1")).rejects.toMatchObject({
      kind: "invalid_response",
    });
    expect(latestContext!.state).toEqual({ runs: {}, latestRunIdBySession: {} });
    expect(streamRunEvents).not.toHaveBeenCalled();
  });

  it("resumes after the last applied sequence and preserves draft, usage, and terminal state", async () => {
    const streamRunEvents = vi.fn((
      _runId: string,
      _options: StreamRunEventsOptions = {},
    ): AsyncIterable<RunStreamEvent> => {
      const callNumber = streamRunEvents.mock.calls.length;
      return (async function* events() {
        if (callNumber === 1) {
          yield streamEvent("message.started", 1, {
            messageId: "assistant-1",
            role: "assistant",
          });
          yield streamEvent("message.delta", 2, {
            messageId: "assistant-1",
            delta: "Hello ",
          });
          yield streamEvent("usage.updated", 3, {
            promptTokens: 8,
            completionTokens: 1,
            totalTokens: 9,
            cost: null,
          });
          throw new DesktopConnectionError(
            "desktop_unavailable",
            "managed backend restarting",
          );
        }
        yield streamEvent("message.delta", 4, {
          messageId: "assistant-1",
          delta: "back",
        });
        yield streamEvent("reasoning.delta", 5, {
          messageId: "assistant-1",
          delta: "Prepared a concise answer.",
        });
        yield streamEvent("message.completed", 6, {
          message: ASSISTANT_MESSAGE,
          sessionRevision: "session_rev_2",
        });
        yield streamEvent("run.completed", 7, {
          usage: USAGE,
          messageId: "assistant-1",
        });
      })();
    });
    const eventsApi: RunEventsApi = { streamRunEvents };
    render(
      <ProviderHarness
        runsApi={makeRunsApi({
          getRun: vi.fn(async () => ({ ...RUN, lastSequence: 3 })),
        })}
        runEventsApi={eventsApi}
        reconnectDelayMs={0}
        maxReconnectAttempts={1}
      />,
    );

    await act(async () => {
      await latestContext!.createRun("session-1", INPUT);
    });
    await waitFor(() => expect(latestContext!.state.runs["run-1"]?.terminal?.kind).toBe(
      "completed",
    ));

    expect(streamRunEvents).toHaveBeenCalledTimes(2);
    expect(streamRunEvents.mock.calls[0]![1]).toMatchObject({
      sessionId: "session-1",
      lastSequence: undefined,
      signal: expect.any(AbortSignal),
    });
    expect(streamRunEvents.mock.calls[1]![1]).toMatchObject({
      sessionId: "session-1",
      lastSequence: 3,
      signal: expect.any(AbortSignal),
    });
    expect(latestContext!.state.runs["run-1"]).toMatchObject({
      streamStatus: "closed",
      lastSequence: 7,
      draft: null,
      usage: USAGE,
      terminal: { kind: "completed", sequence: 7, source: "event" },
      run: { status: "completed", messageId: "assistant-1" },
    });
    expect(latestContext!.state.runs["run-1"]?.committedMessages).toEqual([
      USER_MESSAGE,
      ASSISTANT_MESSAGE,
    ]);
  });

  it("keeps SSE attached after Run completion until its pending async tool delivery arrives", async () => {
    const gate = deferred<void>();
    const streamRunEvents = vi.fn((
      _runId: string,
      _options: StreamRunEventsOptions = {},
    ): AsyncIterable<RunStreamEvent> => (async function* events() {
      yield streamEvent("message.started", 1, { messageId: ASSISTANT_MESSAGE.id, role: "assistant" });
      yield streamEvent("tool.started", 2, { callId: "call-async", name: "terminal" });
      yield streamEvent("tool.completed", 3, {
        callId: "call-async",
        resultSummary: "Background process started",
        artifacts: [],
        asyncDeliveryPending: true,
      });
      yield streamEvent("message.completed", 4, {
        message: ASSISTANT_MESSAGE,
        sessionRevision: "session_rev_2",
      });
      yield streamEvent("run.completed", 5, { usage: USAGE, messageId: ASSISTANT_MESSAGE.id });
      await gate.promise;
      yield streamEvent("tool.delivery", 6, {
        callId: "call-async",
        processId: "process_0123456789abcdef0123456789abcdef",
        delivery: "completion",
        status: "exited",
        exitCode: 0,
      });
    })());
    render(<ProviderHarness runsApi={makeRunsApi()} runEventsApi={{ streamRunEvents }} />);

    await act(async () => {
      await latestContext!.createRun("session-1", INPUT);
    });
    await waitFor(() => expect(latestContext!.state.runs["run-1"]?.terminal?.kind).toBe("completed"));
    expect(latestContext!.state.runs["run-1"]).toMatchObject({
      streamStatus: "connected",
      pendingAsyncToolDeliveries: { "call-async": { name: "terminal" } },
    });
    expect(streamRunEvents).toHaveBeenCalledTimes(1);

    await act(async () => gate.resolve());
    await waitFor(() => expect(latestContext!.state.runs["run-1"]?.streamStatus).toBe("closed"));
    expect(latestContext!.state.runs["run-1"]?.asyncToolDeliveries).toMatchObject({
      process_0123456789abcdef0123456789abcdef: { status: "exited", exitCode: 0 },
    });
  });

  it("reconnects when a completed Run stream closes before its async delivery", async () => {
    const streamRunEvents = vi.fn((
      _runId: string,
      options: StreamRunEventsOptions = {},
    ): AsyncIterable<RunStreamEvent> => {
      const callNumber = streamRunEvents.mock.calls.length;
      return (async function* events() {
        if (callNumber === 1) {
          yield streamEvent("message.started", 1, {
            messageId: ASSISTANT_MESSAGE.id,
            role: "assistant",
          });
          yield streamEvent("tool.started", 2, { callId: "call-async", name: "terminal" });
          yield streamEvent("tool.completed", 3, {
            callId: "call-async",
            resultSummary: "Background process started",
            artifacts: [],
            asyncDeliveryPending: true,
          });
          yield streamEvent("message.completed", 4, {
            message: ASSISTANT_MESSAGE,
            sessionRevision: "session_rev_2",
          });
          yield streamEvent("run.completed", 5, {
            usage: USAGE,
            messageId: ASSISTANT_MESSAGE.id,
          });
          return;
        }

        expect(options.lastSequence).toBe(5);
        yield streamEvent("tool.delivery", 6, {
          callId: "call-async",
          processId: "process_0123456789abcdef0123456789abcdef",
          delivery: "completion",
          status: "killed",
        });
      })();
    });
    const completedRun: Run = {
      ...RUN,
      status: "completed",
      lastSequence: 5,
      messageId: ASSISTANT_MESSAGE.id,
      usage: USAGE,
    };
    render(
      <ProviderHarness
        maxReconnectAttempts={1}
        reconnectDelayMs={0}
        runEventsApi={{ streamRunEvents }}
        runsApi={makeRunsApi({ getRun: vi.fn(async () => completedRun) })}
      />,
    );

    await act(async () => {
      await latestContext!.createRun("session-1", INPUT);
    });
    await waitFor(() => expect(latestContext!.state.runs["run-1"]?.streamStatus).toBe("closed"));

    expect(streamRunEvents).toHaveBeenCalledTimes(2);
    expect(latestContext!.state.runs["run-1"]?.pendingAsyncToolDeliveries).toEqual({});
    expect(latestContext!.state.runs["run-1"]?.asyncToolDeliveries).toMatchObject({
      process_0123456789abcdef0123456789abcdef: { status: "killed" },
    });
  });

  it("reconciles Run and Message state after the SSE replay window expires", async () => {
    const recoveredRun: Run = {
      ...RUN,
      lastSequence: 4,
    };
    const getRun = vi.fn(async () => recoveredRun);
    const listMessages = vi.fn<Pick<SessionsApi, "listMessages">["listMessages"]>(async () => ({
      items: [USER_MESSAGE],
      nextCursor: null,
      snapshotLastSequence: 1,
      firstSequence: 1,
      lastSequence: 1,
    }));
    const streamRunEvents = vi.fn((
      _runId: string,
      _options: StreamRunEventsOptions = {},
    ): AsyncIterable<RunStreamEvent> => {
      const callNumber = streamRunEvents.mock.calls.length;
      return (async function* events() {
        if (callNumber === 1) {
          throw new RunApiError("http", "Event history expired", {
            status: 409,
            code: "event_history_expired",
          });
        }
        yield streamEvent("message.delta", 5, {
          messageId: ASSISTANT_MESSAGE.id,
          delta: "back",
        });
        yield streamEvent("message.completed", 6, {
          message: ASSISTANT_MESSAGE,
          sessionRevision: "session_rev_2",
        });
        yield streamEvent("run.completed", 7, {
          usage: USAGE,
          messageId: ASSISTANT_MESSAGE.id,
        });
      })();
    });

    render(
      <ProviderHarness
        reconnectDelayMs={0}
        maxReconnectAttempts={0}
        runEventsApi={{ streamRunEvents }}
        runsApi={makeRunsApi({ getRun })}
        sessionsApi={{ listMessages }}
      />,
    );
    await act(async () => {
      await latestContext!.createRun("session-1", INPUT);
    });
    await waitFor(() => expect(latestContext!.state.runs["run-1"]?.terminal?.kind).toBe(
      "completed",
    ));

    expect(getRun).toHaveBeenCalledWith("run-1", { signal: expect.any(AbortSignal) });
    expect(listMessages).toHaveBeenCalledWith(
      "session-1",
      { limit: 100 },
      { signal: expect.any(AbortSignal) },
    );
    expect(streamRunEvents).toHaveBeenCalledTimes(2);
    expect(streamRunEvents.mock.calls[1]![1]).toMatchObject({ lastSequence: 4 });
    expect(latestContext!.state.runs["run-1"]).toMatchObject({
      lastSequence: 7,
      recoveredAcrossGap: false,
      protocolError: null,
      streamStatus: "closed",
    });
    expect(latestContext!.state.runs["run-1"]?.committedMessages).toEqual([
      USER_MESSAGE,
      ASSISTANT_MESSAGE,
    ]);
  });

  it("keeps approval state event-authoritative and deduplicates before and after REST acceptance", async () => {
    const acceptedAction: ActionAccepted = { accepted: true };
    const rest = deferred<ActionAccepted>();
    const resolutionEvent = deferred<void>();
    const resolveApproval = vi.fn(() => rest.promise);
    const streamRunEvents = vi.fn((
      _runId: string,
      options: StreamRunEventsOptions = {},
    ): AsyncIterable<RunStreamEvent> => (async function* approvalEvents() {
      await resolutionEvent.promise;
      yield streamEvent("approval.resolved", 1, {
        approvalId: APPROVAL_PENDING.approvalId,
        callId: APPROVAL_PENDING.callId,
        decision: "once",
        resolvedBy: "user",
      });
      const signal = options.signal;
      if (signal && !signal.aborted) {
        await new Promise<void>((resolve) => signal.addEventListener("abort", () => resolve(), {
          once: true,
        }));
      }
    })());
    render(
      <ProviderHarness
        runEventsApi={{ streamRunEvents }}
        runsApi={makeRunsApi({
          createRun: vi.fn(async () => acceptedWithPending(APPROVAL_PENDING)),
          resolveApproval,
        })}
      />,
    );

    await act(async () => {
      await latestContext!.createRun("session-1", INPUT);
    });
    expect(latestContext!.state.runs["run-1"]?.pendingAction).toEqual(APPROVAL_PENDING);

    const first = latestContext!.resolveApproval("run-1", "approval-1", {
      decision: "once",
      reason: null,
    });
    const duplicate = latestContext!.resolveApproval("run-1", "approval-1", {
      reason: null,
      decision: "once",
    });
    expect(duplicate).toBe(first);
    await waitFor(() => expect(resolveApproval).toHaveBeenCalledTimes(1));

    rest.resolve(acceptedAction);
    await expect(first).resolves.toEqual(acceptedAction);
    expect(latestContext!.state.runs["run-1"]?.pendingAction).toEqual(APPROVAL_PENDING);
    expect(latestContext!.resolveApproval("run-1", "approval-1", {
      decision: "once",
      reason: null,
    })).toBe(first);

    resolutionEvent.resolve();
    await waitFor(() => expect(latestContext!.state.runs["run-1"]?.pendingAction).toBeNull());
    expect(resolveApproval).toHaveBeenCalledTimes(1);
  });

  it("accepts an event-first clarification resolution and permits retry after REST errors", async () => {
    const acceptedAction: ActionAccepted = { accepted: true };
    const rest = deferred<ActionAccepted>();
    const resolutionEvent = deferred<void>();
    const answerClarification = vi.fn()
      .mockRejectedValueOnce(new RunApiError("network", "offline", { retryable: true }))
      .mockReturnValueOnce(rest.promise);
    const streamRunEvents = vi.fn((
      _runId: string,
      options: StreamRunEventsOptions = {},
    ): AsyncIterable<RunStreamEvent> => (async function* clarificationEvents() {
      await resolutionEvent.promise;
      yield streamEvent("clarification.resolved", 1, {
        requestId: CLARIFICATION_PENDING.requestId,
        resolvedBy: "user",
      });
      const signal = options.signal;
      if (signal && !signal.aborted) {
        await new Promise<void>((resolve) => signal.addEventListener("abort", () => resolve(), {
          once: true,
        }));
      }
    })());
    render(
      <ProviderHarness
        runEventsApi={{ streamRunEvents }}
        runsApi={makeRunsApi({
          answerClarification,
          createRun: vi.fn(async () => acceptedWithPending(CLARIFICATION_PENDING)),
        })}
      />,
    );

    await act(async () => {
      await latestContext!.createRun("session-1", INPUT);
    });
    await expect(latestContext!.answerClarification("run-1", "clarification-1", {
      answer: "staging",
    })).rejects.toMatchObject({ kind: "network" });

    const retry = latestContext!.answerClarification("run-1", "clarification-1", {
      answer: "staging",
    });
    await waitFor(() => expect(answerClarification).toHaveBeenCalledTimes(2));
    resolutionEvent.resolve();
    await waitFor(() => expect(latestContext!.state.runs["run-1"]?.pendingAction).toBeNull());

    rest.resolve(acceptedAction);
    await expect(retry).resolves.toEqual(acceptedAction);
    expect(latestContext!.state.runs["run-1"]?.pendingAction).toBeNull();
    await expect(latestContext!.answerClarification("run-1", "clarification-1", {
      answer: "staging",
    })).rejects.toThrow("not waiting");
    expect(answerClarification).toHaveBeenCalledTimes(2);
  });

  it("cancels through REST, closes terminal Runs, and aborts their stream", async () => {
    let streamSignal: AbortSignal | undefined;
    const cancelledRun: Run = { ...RUN, status: "cancelled", updatedAt: NOW };
    const runsApi = makeRunsApi({ cancelRun: vi.fn(async () => cancelledRun) });
    render(
      <ProviderHarness
        runsApi={runsApi}
        runEventsApi={idleRunEventsApi((signal) => {
          streamSignal = signal;
        })}
      />,
    );
    await act(async () => {
      await latestContext!.createRun("session-1", INPUT);
    });
    await waitFor(() => expect(streamSignal).toBeDefined());

    await act(async () => {
      await latestContext!.cancelRun("run-1");
    });

    expect(runsApi.cancelRun).toHaveBeenCalledWith("run-1");
    expect(streamSignal?.aborted).toBe(true);
    expect(latestContext!.state.runs["run-1"]).toMatchObject({
      cancelPending: false,
      cancelError: null,
      streamStatus: "closed",
      terminal: { kind: "cancelled", sequence: null, source: "rest" },
    });
  });

  it("keeps Run state when a consumer unmounts and aborts only when the Provider unmounts", async () => {
    let streamSignal: AbortSignal | undefined;
    const eventsApi = idleRunEventsApi((signal) => {
      streamSignal = signal;
    });
    const props = { runsApi: makeRunsApi(), runEventsApi: eventsApi };
    const view = render(<ProviderHarness {...props}><ContextProbe /></ProviderHarness>);
    await act(async () => {
      await latestContext!.createRun("session-1", INPUT);
    });
    await waitFor(() => expect(streamSignal).toBeDefined());

    view.rerender(<ProviderHarness {...props}>{null}</ProviderHarness>);
    expect(streamSignal?.aborted).toBe(false);
    latestContext = null;
    view.rerender(<ProviderHarness {...props}><ContextProbe /></ProviderHarness>);

    expect(latestContext!.state.runs["run-1"]?.run.id).toBe("run-1");
    expect(latestContext!.state.latestRunIdBySession["session-1"]).toBe("run-1");
    view.unmount();
    expect(streamSignal?.aborted).toBe(true);
  });
});
