// @vitest-environment jsdom

import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { DesktopConnectionError } from "../../api/desktopConnection";
import type { Capabilities, ProfileSummary, ProfilesApi } from "../../api/profiles";
import {
  RunApiError,
  type ActionAccepted,
  type CreateRunInput,
  type Run,
  type RunAccepted,
  type RunEventName,
  type RunEventPayload,
  type RunsApi,
} from "../../api/runs";
import type { RunEventsApi, RunStreamEvent } from "../../api/sse";
import type { Message, Session, SessionsApi } from "../../api/sessions";
import { ChatRunProvider } from "./ChatRunProvider";
import { ChatWorkspace } from "./ChatWorkspace";

const NOW = "2026-07-16T08:00:00Z";
const SESSION: Session = {
  id: "session_1",
  profileId: "default",
  title: "Rust migration",
  preview: "",
  source: "synthchat",
  model: "lmstudio/test",
  messageCount: 0,
  archived: false,
  revision: "session_rev_1",
  createdAt: NOW,
  updatedAt: NOW,
  match: null,
};
const PROFILE: ProfileSummary = {
  id: "default",
  displayName: "Default",
  isDefault: true,
  isActive: true,
  color: null,
  avatarFileId: null,
  engineState: "running",
  configRevision: "config_rev_1",
  createdAt: null,
  updatedAt: NOW,
};
const OTHER_PROFILE: ProfileSummary = {
  ...PROFILE,
  id: "other",
  displayName: "Other",
  isDefault: false,
  isActive: false,
};
const OTHER_SESSION: Session = {
  ...SESSION,
  id: "session_2",
  profileId: OTHER_PROFILE.id,
  title: "Other session",
};
const CAPABILITIES: Capabilities = {
  contractVersion: "v1",
  backendVersion: "0.1.0",
  engine: {
    kind: "hermes-rust",
    available: true,
    version: "0.1.0",
    pinnedCommit: null,
    features: {
      runStreaming: true,
      reasoningStreaming: true,
      toolProgress: true,
      approvals: false,
      clarifications: false,
      asyncToolDelivery: false,
      profileManagement: true,
      skillManagement: false,
      memoryWrite: false,
      mcpManagement: false,
      oauthAccounts: false,
    },
  },
  sessionStorage: { available: true, schemaVersion: 6, hermesImportAvailable: true },
  sessionSearch: { mode: "fts5" },
  files: { maxBytes: 0, allowedMimeTypes: [] },
  extensions: {
    activeRunDiscovery: false,
    runQueue: false,
    toolsetManagement: true,
    toolExecution: true,
    codeExecution: true,
    workspaceManagement: true,
    skillDiscovery: true,
    skillEnablement: true,
    webSearch: true,
    webExtract: true,
    browserAutomation: false,
    browserCdp: false,
    browserDownloads: false,
    mcpStdio: false,
    mcpStreamableHttp: false,
    mcpSse: false,
  },
};

function message(id: string, sequence: number, role: Message["role"], text: string): Message {
  return {
    id,
    sessionId: SESSION.id,
    sequence,
    role,
    parts: [{ type: "text", text }],
    reasoning: null,
    toolCalls: [],
    usage: null,
    createdAt: NOW,
  };
}

function run(status: Run["status"] = "running"): Run {
  return {
    id: "run_1",
    sessionId: SESSION.id,
    profileId: "default",
    status,
    lastSequence: status === "completed" ? 5 : 1,
    messageId: status === "completed" ? "message_assistant" : null,
    usage: status === "completed"
      ? { promptTokens: 5, completionTokens: 3, totalTokens: 8, cost: null }
      : null,
    error: null,
    pendingAction: null,
    createdAt: NOW,
    updatedAt: NOW,
  };
}

function makeSessionClient(): SessionsApi {
  return {
    listSessions: vi.fn(async () => ({ items: [SESSION], nextCursor: null })),
    searchSessions: vi.fn(),
    createSession: vi.fn(async () => ({ value: SESSION, etag: '"session_rev_1"' })),
    getSession: vi.fn(async () => ({ value: SESSION, etag: '"session_rev_1"' })),
    updateSession: vi.fn(),
    deleteSession: vi.fn(),
    listMessages: vi.fn(async () => ({
      items: [],
      nextCursor: null,
      snapshotLastSequence: 0,
      firstSequence: null,
      lastSequence: null,
    })),
  };
}

function profileClient(capabilities = CAPABILITIES): Pick<ProfilesApi, "getCapabilities" | "listProfiles"> {
  return {
    getCapabilities: vi.fn(async () => capabilities),
    listProfiles: vi.fn(async () => [PROFILE]),
  };
}

function accepted(): Extract<RunAccepted, { disposition: "started" }> {
  const running = run();
  return {
    run: { ...running, status: "running" },
    disposition: "started",
    queueItemId: null,
    userMessage: message("message_user", 1, "user", "Hello Hermes"),
    sessionRevision: "session_rev_2",
  };
}

const APPROVAL_PENDING: Extract<NonNullable<Run["pendingAction"]>, { kind: "approval" }> = {
  kind: "approval",
  approvalId: "approval_1",
  callId: "call_1",
  toolName: "terminal",
  inputSummary: "Run command in the registered workspace",
  choices: ["once", "deny"],
  expiresAt: "2026-07-16T08:05:00Z",
};

const CLARIFICATION_PENDING: Extract<NonNullable<Run["pendingAction"]>, { kind: "clarification" }> = {
  kind: "clarification",
  requestId: "clarification_1",
  question: "Which environment should Hermes use?",
  choices: [],
};

function acceptedWithPending(pendingAction: NonNullable<Run["pendingAction"]>): RunAccepted {
  const base = accepted();
  return {
    ...base,
    disposition: "replayed",
    run: {
      ...base.run,
      status: pendingAction.kind === "approval" ? "waitingApproval" : "waitingClarification",
      pendingAction,
    },
  };
}

function withActionCapabilities(approvals: boolean, clarifications: boolean): Capabilities {
  return {
    ...CAPABILITIES,
    engine: {
      ...CAPABILITIES.engine,
      features: { ...CAPABILITIES.engine.features, approvals, clarifications },
    },
  };
}

function withAsyncToolDeliveryCapability(): Capabilities {
  return {
    ...CAPABILITIES,
    engine: {
      ...CAPABILITIES.engine,
      features: { ...CAPABILITIES.engine.features, asyncToolDelivery: true },
    },
  };
}

function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

function resolutionEvent(event: RunEventName, data: object): RunStreamEvent {
  return {
    id: "run_1:1",
    event,
    payload: {
      schemaVersion: 1,
      sequence: 1,
      runId: "run_1",
      sessionId: SESSION.id,
      occurredAt: NOW,
      data,
    } as RunEventPayload,
  };
}

function recoveryEvent(event: RunEventName, sequence: number, data: object): RunStreamEvent {
  return {
    id: `run_1:${sequence}`,
    event,
    payload: {
      schemaVersion: 1,
      sequence,
      runId: "run_1",
      sessionId: SESSION.id,
      occurredAt: NOW,
      data,
    } as RunEventPayload,
  };
}

function gatedEvents(gate: Promise<void>, event: RunStreamEvent): RunEventsApi {
  return {
    streamRunEvents: vi.fn((_runId, options = {}) => (async function* events() {
      await gate;
      yield event;
      const signal = options.signal;
      if (signal && !signal.aborted) {
        await new Promise<void>((resolve) => signal.addEventListener("abort", () => resolve(), {
          once: true,
        }));
      }
    })()),
  };
}

function idleEvents(): RunEventsApi {
  return {
    streamRunEvents: vi.fn((_runId, options = {}) => (async function* idle() {
      const signal = options.signal;
      if (signal && !signal.aborted) {
        await new Promise<void>((resolve) => signal.addEventListener("abort", () => resolve(), {
          once: true,
        }));
      }
    })()),
  };
}

function streamEvents(): RunStreamEvent[] {
  const envelope = (sequence: number, data: object) => ({
    schemaVersion: 1 as const,
    sequence,
    runId: "run_1",
    sessionId: SESSION.id,
    occurredAt: NOW,
    data,
  });
  const assistant = {
    ...message("message_assistant", 2, "assistant", "Hello from Rust"),
    usage: { promptTokens: 5, completionTokens: 3, totalTokens: 8, cost: null },
  };
  return [
    { id: "run_1:1", event: "run.started", payload: envelope(1, { profileId: "default" }) },
    { id: "run_1:2", event: "message.started", payload: envelope(2, { messageId: assistant.id, role: "assistant" }) },
    { id: "run_1:3", event: "message.delta", payload: envelope(3, { messageId: assistant.id, delta: "Hello from Rust" }) },
    { id: "run_1:4", event: "message.completed", payload: envelope(4, { message: assistant, sessionRevision: "session_rev_3" }) },
    { id: "run_1:5", event: "run.completed", payload: envelope(5, { usage: assistant.usage, messageId: assistant.id }) },
  ] as RunStreamEvent[];
}

afterEach(cleanup);

describe("ChatWorkspace", () => {
  it("sends a Run and commits the streamed assistant message", async () => {
    const createRun = vi.fn(async (
      _sessionId: string,
      _input: Parameters<RunsApi["createRun"]>[1],
      _idempotencyKey: string,
    ) => accepted());
    const runsClient = {
      listActiveRuns: vi.fn(async () => ({ items: [] })),
      createRun,
      getRun: vi.fn(async () => run("completed")),
      cancelRun: vi.fn(async () => run("cancelled")),
      resolveApproval: vi.fn(),
      answerClarification: vi.fn(),
    } satisfies RunsApi;
    const eventClient: RunEventsApi = {
      async *streamRunEvents() {
        for (const event of streamEvents()) yield event;
      },
    };
    const user = userEvent.setup();
    render(
      <ChatRunProvider
        maxReconnectAttempts={0}
        reconnectDelayMs={0}
        runEventsApi={eventClient}
        runsApi={runsClient}
      >
        <ChatWorkspace client={makeSessionClient()} profileClient={profileClient()} />
      </ChatRunProvider>,
    );

    const composer = await screen.findByRole("textbox", { name: "消息" });
    await waitFor(() => expect((composer as HTMLTextAreaElement).disabled).toBe(false));
    await user.type(composer, "Hello Hermes");
    await user.click(screen.getByRole("button", { name: "发送消息" }));

    await waitFor(() => expect(createRun).toHaveBeenCalledTimes(1));
    expect(createRun).toHaveBeenCalledWith(
      SESSION.id,
      expect.objectContaining({ message: { text: "Hello Hermes", fileIds: [] } }),
      expect.any(String),
    );
    expect(await screen.findByText("Hello Hermes")).toBeTruthy();
    expect(await screen.findByText("Hello from Rust")).toBeTruthy();
    expect(screen.getAllByText("8 tokens")).toHaveLength(2);
  });

  it("renders only safe async terminal delivery status", async () => {
    const gate = deferred<void>();
    const privateOutput = "ASYNC_PRIVATE_OUTPUT";
    const watchPattern = "ASYNC_WATCH_PATTERN";
    const runsClient = {
      listActiveRuns: vi.fn(async () => ({ items: [] })),
      createRun: vi.fn(async () => accepted()),
      getRun: vi.fn(async () => run("completed")),
      cancelRun: vi.fn(async () => run("cancelled")),
      resolveApproval: vi.fn(),
      answerClarification: vi.fn(),
    } satisfies RunsApi;
    const assistant = {
      ...message("message_assistant", 2, "assistant", "Background task queued"),
      usage: { promptTokens: 5, completionTokens: 3, totalTokens: 8, cost: null },
    };
    const eventClient: RunEventsApi = {
      async *streamRunEvents() {
        yield recoveryEvent("run.started", 1, { profileId: "default" });
        yield recoveryEvent("message.started", 2, { messageId: assistant.id, role: "assistant" });
        yield recoveryEvent("tool.started", 3, { callId: "call-async", name: "terminal" });
        yield recoveryEvent("tool.completed", 4, {
          callId: "call-async",
          resultSummary: "Background process started",
          artifacts: [],
          asyncDeliveryPending: true,
          privateOutput,
          watchPatterns: [watchPattern],
        });
        yield recoveryEvent("message.completed", 5, {
          message: assistant,
          sessionRevision: "session_rev_3",
        });
        yield recoveryEvent("run.completed", 6, {
          usage: assistant.usage,
          messageId: assistant.id,
        });
        await gate.promise;
        yield recoveryEvent("tool.delivery", 7, {
          callId: "call-async",
          processId: "process_0123456789abcdef0123456789abcdef",
          delivery: "watch",
          status: "exited",
          matchedPatternCount: 1,
          privateOutput,
          watchPatterns: [watchPattern],
        });
      },
    };
    const user = userEvent.setup();
    render(
      <ChatRunProvider
        maxReconnectAttempts={0}
        reconnectDelayMs={0}
        runEventsApi={eventClient}
        runsApi={runsClient}
      >
        <ChatWorkspace
          client={makeSessionClient()}
          profileClient={profileClient(withAsyncToolDeliveryCapability())}
        />
      </ChatRunProvider>,
    );

    const composer = await screen.findByRole("textbox", { name: "消息" });
    await waitFor(() => expect((composer as HTMLTextAreaElement).disabled).toBe(false));
    await user.type(composer, "Watch background task");
    await user.click(screen.getByRole("button", { name: "发送消息" }));

    expect(await screen.findByText("后台终端任务")).toBeTruthy();
    expect(await screen.findByText("等待完成通知")).toBeTruthy();
    expect(screen.queryByText(privateOutput)).toBeNull();
    expect(screen.queryByText(watchPattern)).toBeNull();

    await act(async () => gate.resolve());
    expect(await screen.findByText("已匹配 1 个监测条件")).toBeTruthy();
    expect(screen.queryByText(privateOutput)).toBeNull();
    expect(screen.queryByText(watchPattern)).toBeNull();
  });

  it("keeps deduplicated async terminal notifications from every Run in the current session", async () => {
    const firstDelivery = deferred<void>();
    const secondDelivery = deferred<void>();
    const sharedCallId = "call-shared";
    const sharedProcessId = "process_0123456789abcdef0123456789abcdef";
    const privateCommand = "PRIVATE_COMMAND --secret credential";
    const privateOutput = "PRIVATE_BACKGROUND_OUTPUT";
    const acceptedFor = (runId: string, index: number, text: string): RunAccepted => {
      const base = accepted();
      return {
        ...base,
        run: { ...base.run, id: runId },
        userMessage: message(`message_user_${index}`, index * 2 - 1, "user", text),
      };
    };
    const eventFor = (
      runId: string,
      event: RunEventName,
      sequence: number,
      data: object,
    ): RunStreamEvent => ({
      id: `${runId}:${sequence}`,
      event,
      payload: {
        schemaVersion: 1,
        sequence,
        runId,
        sessionId: SESSION.id,
        occurredAt: NOW,
        data,
      } as RunEventPayload,
    });
    const runsClient = {
      listActiveRuns: vi.fn(async () => ({ items: [] })),
      createRun: vi.fn()
        .mockResolvedValueOnce(acceptedFor("run_1", 1, "Start first task"))
        .mockResolvedValueOnce(acceptedFor("run_2", 2, "Start second task")),
      getRun: vi.fn(async () => run("completed")),
      cancelRun: vi.fn(async () => run("cancelled")),
      resolveApproval: vi.fn(),
      answerClarification: vi.fn(),
    } satisfies RunsApi;
    const eventClient: RunEventsApi = {
      streamRunEvents: vi.fn((runId) => (async function* events() {
        const index = runId === "run_1" ? 1 : 2;
        const assistant = message(
          `message_assistant_${index}`,
          index * 2,
          "assistant",
          `Task ${index} queued`,
        );
        yield eventFor(runId, "run.started", 1, { profileId: "default" });
        yield eventFor(runId, "message.started", 2, {
          messageId: assistant.id,
          role: "assistant",
        });
        yield eventFor(runId, "tool.started", 3, {
          callId: sharedCallId,
          name: "terminal",
          inputSummary: privateCommand,
        });
        yield eventFor(runId, "tool.completed", 4, {
          callId: sharedCallId,
          resultSummary: "Background process started",
          artifacts: [],
          asyncDeliveryPending: true,
          privateOutput,
        });
        yield eventFor(runId, "message.completed", 5, {
          message: assistant,
          sessionRevision: `session_rev_${index + 2}`,
        });
        yield eventFor(runId, "run.completed", 6, {
          usage: { promptTokens: 1, completionTokens: 1, totalTokens: 2, cost: null },
          messageId: assistant.id,
        });
        await (runId === "run_1" ? firstDelivery.promise : secondDelivery.promise);
        yield eventFor(runId, "tool.delivery", 7, {
          callId: sharedCallId,
          processId: sharedProcessId,
          delivery: "completion",
          status: "exited",
          exitCode: 0,
          privateOutput,
          command: privateCommand,
        });
      })()),
    };
    const user = userEvent.setup();
    render(
      <ChatRunProvider
        maxReconnectAttempts={0}
        reconnectDelayMs={0}
        runEventsApi={eventClient}
        runsApi={runsClient}
      >
        <ChatWorkspace
          client={makeSessionClient()}
          profileClient={profileClient(withAsyncToolDeliveryCapability())}
        />
      </ChatRunProvider>,
    );

    const composer = await screen.findByRole("textbox", { name: "消息" });
    await waitFor(() => expect((composer as HTMLTextAreaElement).disabled).toBe(false));
    await user.type(composer, "Start first task");
    await user.click(screen.getByRole("button", { name: "发送消息" }));
    await screen.findByText("等待完成通知");

    await user.type(composer, "Start second task");
    await user.click(await screen.findByRole("button", { name: "发送消息" }));
    await waitFor(() => expect(screen.getAllByText("等待完成通知")).toHaveLength(2));

    await act(async () => firstDelivery.resolve());
    await waitFor(() => {
      expect(screen.getAllByText("等待完成通知")).toHaveLength(1);
      expect(screen.getAllByText("后台任务已完成，退出码 0")).toHaveLength(1);
    });

    await act(async () => secondDelivery.resolve());
    await waitFor(() => {
      expect(screen.queryByText("等待完成通知")).toBeNull();
      expect(screen.getAllByText("后台任务已完成，退出码 0")).toHaveLength(2);
    });
    expect(screen.queryByText(privateCommand)).toBeNull();
    expect(screen.queryByText(privateOutput)).toBeNull();
  });

  it("reuses one client request for a retry and creates a fresh request after success", async () => {
    const createRun = vi.fn()
      .mockRejectedValueOnce(new RunApiError("network", "offline", { retryable: true }))
      .mockResolvedValueOnce(accepted())
      .mockRejectedValueOnce(new RunApiError("http", "rejected", {
        status: 400,
        retryable: false,
      }));
    const runsClient = {
      listActiveRuns: vi.fn(async () => ({ items: [] })),
      createRun,
      getRun: vi.fn(async () => run("completed")),
      cancelRun: vi.fn(async () => run("cancelled")),
      resolveApproval: vi.fn(),
      answerClarification: vi.fn(),
    } satisfies RunsApi;
    const eventClient: RunEventsApi = {
      async *streamRunEvents() {
        for (const event of streamEvents()) yield event;
      },
    };
    const user = userEvent.setup();
    render(
      <ChatRunProvider
        maxReconnectAttempts={0}
        reconnectDelayMs={0}
        runEventsApi={eventClient}
        runsApi={runsClient}
      >
        <ChatWorkspace client={makeSessionClient()} profileClient={profileClient()} />
      </ChatRunProvider>,
    );

    const composer = await screen.findByRole("textbox", { name: "消息" });
    await waitFor(() => expect((composer as HTMLTextAreaElement).disabled).toBe(false));
    await user.type(composer, "Retry me");
    await user.click(screen.getByRole("button", { name: "发送消息" }));
    expect(await screen.findByText("offline")).toBeTruthy();

    await user.click(screen.getByRole("button", { name: "发送消息" }));
    await waitFor(() => expect(createRun).toHaveBeenCalledTimes(2));
    await waitFor(() => expect((composer as HTMLTextAreaElement).value).toBe(""));

    await user.type(composer, "Retry me");
    await user.click(screen.getByRole("button", { name: "发送消息" }));
    await waitFor(() => expect(createRun).toHaveBeenCalledTimes(3));

    const firstInput = createRun.mock.calls[0]![1] as CreateRunInput;
    const retryInput = createRun.mock.calls[1]![1] as CreateRunInput;
    const newInput = createRun.mock.calls[2]![1] as CreateRunInput;
    expect(retryInput.clientRequestId).toBe(firstInput.clientRequestId);
    expect(createRun.mock.calls[1]![2]).toBe(createRun.mock.calls[0]![2]);
    expect(newInput.clientRequestId).not.toBe(firstInput.clientRequestId);
    expect(createRun.mock.calls[2]![2]).not.toBe(createRun.mock.calls[0]![2]);
  });

  it("does not discover active Runs while the capability remains disabled", async () => {
    const runsClient = {
      listActiveRuns: vi.fn(async () => ({ items: [] })),
      createRun: vi.fn(async () => accepted()),
      getRun: vi.fn(async () => run()),
      cancelRun: vi.fn(async () => run("cancelled")),
      resolveApproval: vi.fn(),
      answerClarification: vi.fn(),
    } satisfies RunsApi;
    render(
      <ChatRunProvider runEventsApi={idleEvents()} runsApi={runsClient}>
        <ChatWorkspace client={makeSessionClient()} profileClient={profileClient()} />
      </ChatRunProvider>,
    );

    const composer = await screen.findByRole("textbox", { name: "消息" });
    await waitFor(() => expect((composer as HTMLTextAreaElement).disabled).toBe(false));
    expect(runsClient.listActiveRuns).not.toHaveBeenCalled();
  });

  it("restores a waiting approval and tool progress from active Run journal replay", async () => {
    const recoveredUser = message("message_recovered_user", 1, "user", "Recovered prompt");
    const waitingRun: Run = {
      ...run(),
      status: "waitingApproval",
      lastSequence: 5,
      pendingAction: APPROVAL_PENDING,
    };
    const listActiveRuns = vi.fn(async () => ({
      items: [{
        run: waitingRun,
        queueItemId: null,
        userMessage: recoveredUser,
        sessionRevision: "session_rev_5",
      }],
    }));
    const runsClient = {
      listActiveRuns,
      createRun: vi.fn(async () => accepted()),
      getRun: vi.fn(async () => waitingRun),
      cancelRun: vi.fn(async () => ({ ...waitingRun, status: "cancelled" as const })),
      resolveApproval: vi.fn(async () => ({ accepted: true as const })),
      answerClarification: vi.fn(),
    } satisfies RunsApi;
    const { kind: _kind, ...approval } = APPROVAL_PENDING;
    const streamRunEvents = vi.fn((
      _runId: string,
      options: Parameters<RunEventsApi["streamRunEvents"]>[1] = {},
    ): AsyncIterable<RunStreamEvent> => (async function* events() {
      yield recoveryEvent("run.started", 1, { profileId: PROFILE.id });
      yield recoveryEvent("message.started", 2, {
        messageId: "message_recovered_assistant",
        role: "assistant",
      });
      yield recoveryEvent("tool.started", 3, {
        callId: APPROVAL_PENDING.callId,
        name: APPROVAL_PENDING.toolName,
        inputSummary: APPROVAL_PENDING.inputSummary,
      });
      yield recoveryEvent("tool.progress", 4, {
        callId: APPROVAL_PENDING.callId,
        message: "Waiting for approval",
        progress: 0.5,
      });
      yield recoveryEvent("approval.required", 5, approval);
      if (options.signal && !options.signal.aborted) {
        await new Promise<void>((resolve) => options.signal?.addEventListener("abort", () => resolve(), {
          once: true,
        }));
      }
    })());
    const sessionsClient = makeSessionClient();
    sessionsClient.listMessages = vi.fn(async () => ({
      items: [recoveredUser],
      nextCursor: null,
      snapshotLastSequence: 1,
      firstSequence: 1,
      lastSequence: 1,
    }));
    const capabilities = withActionCapabilities(true, false);
    capabilities.extensions = { ...capabilities.extensions, activeRunDiscovery: true };
    render(
      <ChatRunProvider runEventsApi={{ streamRunEvents }} runsApi={runsClient}>
        <ChatWorkspace client={sessionsClient} profileClient={profileClient(capabilities)} />
      </ChatRunProvider>,
    );

    expect(await screen.findByRole("heading", { name: "需要确认工具调用" })).toBeTruthy();
    expect(screen.getAllByText("Recovered prompt")).toHaveLength(1);
    expect(screen.getByText("Waiting for approval")).toBeTruthy();
    expect(listActiveRuns).toHaveBeenCalledWith(
      PROFILE.id,
      { sessionId: SESSION.id },
      { signal: expect.any(AbortSignal) },
    );
    expect(streamRunEvents).toHaveBeenCalledWith("run_1", expect.objectContaining({
      lastSequence: undefined,
      sessionId: SESSION.id,
    }));
  });

  it("aborts stale discovery when switching Profile and discovers the new Session owner", async () => {
    const firstDiscovery = deferred<Awaited<ReturnType<RunsApi["listActiveRuns"]>>>();
    const listActiveRuns = vi.fn<RunsApi["listActiveRuns"]>((profileId) => (
      profileId === PROFILE.id ? firstDiscovery.promise : Promise.resolve({ items: [] })
    ));
    const runsClient = {
      listActiveRuns,
      createRun: vi.fn(async () => accepted()),
      getRun: vi.fn(async () => run()),
      cancelRun: vi.fn(async () => run("cancelled")),
      resolveApproval: vi.fn(),
      answerClarification: vi.fn(),
    } satisfies RunsApi;
    const streamRunEvents = vi.fn<RunEventsApi["streamRunEvents"]>();
    const sessionsClient = makeSessionClient();
    sessionsClient.listSessions = vi.fn(async (query) => ({
      items: [query.profileId === OTHER_PROFILE.id ? OTHER_SESSION : SESSION],
      nextCursor: null,
    }));
    const capabilities = withActionCapabilities(false, false);
    capabilities.extensions = { ...capabilities.extensions, activeRunDiscovery: true };
    const profilesClient: Pick<ProfilesApi, "getCapabilities" | "listProfiles"> = {
      getCapabilities: vi.fn(async () => capabilities),
      listProfiles: vi.fn(async () => [PROFILE, OTHER_PROFILE]),
    };
    const user = userEvent.setup();
    render(
      <ChatRunProvider runEventsApi={{ streamRunEvents }} runsApi={runsClient}>
        <ChatWorkspace client={sessionsClient} profileClient={profilesClient} />
      </ChatRunProvider>,
    );

    await waitFor(() => expect(listActiveRuns).toHaveBeenCalledTimes(1));
    const composer = screen.getByRole("textbox", { name: "消息" }) as HTMLTextAreaElement;
    expect(composer.disabled).toBe(true);
    await user.selectOptions(screen.getByRole("combobox", { name: "聊天 Profile" }), OTHER_PROFILE.id);
    await waitFor(() => expect(listActiveRuns).toHaveBeenCalledTimes(2));

    const firstSignal = listActiveRuns.mock.calls[0]![2]?.signal;
    expect(firstSignal?.aborted).toBe(true);
    expect(listActiveRuns.mock.calls[1]![0]).toBe(OTHER_PROFILE.id);
    expect(listActiveRuns.mock.calls[1]![1]).toEqual({ sessionId: OTHER_SESSION.id });
    await act(async () => {
      firstDiscovery.resolve({
        items: [{
          run: { ...run(), lastSequence: 1 },
          queueItemId: null,
          userMessage: message("message_stale", 1, "user", "Stale prompt"),
          sessionRevision: "session_rev_stale",
        }],
      });
      await firstDiscovery.promise;
    });

    await waitFor(() => expect(composer.disabled).toBe(false));
    expect((screen.getByRole("combobox", { name: "当前会话" }) as HTMLSelectElement).value).toBe(
      OTHER_SESSION.id,
    );
    expect(screen.queryByText("Stale prompt")).toBeNull();
    expect(streamRunEvents).not.toHaveBeenCalled();
  });

  it("keeps approval controls pending after REST 200 and removes them only after the SSE event", async () => {
    const actionResponse = deferred<ActionAccepted>();
    const eventGate = deferred<void>();
    const resolveApproval = vi.fn(() => actionResponse.promise);
    const runsClient = {
      listActiveRuns: vi.fn(async () => ({ items: [] })),
      createRun: vi.fn(async () => acceptedWithPending(APPROVAL_PENDING)),
      getRun: vi.fn(async () => run()),
      cancelRun: vi.fn(async () => run("cancelled")),
      resolveApproval,
      answerClarification: vi.fn(),
    } satisfies RunsApi;
    const user = userEvent.setup();
    render(
      <ChatRunProvider
        runEventsApi={gatedEvents(eventGate.promise, resolutionEvent("approval.resolved", {
          approvalId: APPROVAL_PENDING.approvalId,
          callId: APPROVAL_PENDING.callId,
          decision: "once",
          resolvedBy: "user",
        }))}
        runsApi={runsClient}
      >
        <ChatWorkspace
          client={makeSessionClient()}
          profileClient={profileClient(withActionCapabilities(true, false))}
        />
      </ChatRunProvider>,
    );

    const composer = await screen.findByRole("textbox", { name: "消息" });
    await waitFor(() => expect((composer as HTMLTextAreaElement).disabled).toBe(false));
    await user.type(composer, "Use the terminal");
    await user.click(screen.getByRole("button", { name: "发送消息" }));

    expect(await screen.findByRole("heading", { name: "需要确认工具调用" })).toBeTruthy();
    expect(screen.getByText("terminal")).toBeTruthy();
    expect(screen.getByText("Run command in the registered workspace")).toBeTruthy();
    const allowOnce = screen.getByRole("button", { name: "允许一次" });
    expect(screen.getByRole("button", { name: "拒绝" })).toBeTruthy();
    await user.dblClick(allowOnce);
    await waitFor(() => expect(resolveApproval).toHaveBeenCalledTimes(1));
    expect(resolveApproval).toHaveBeenCalledWith("run_1", "approval_1", {
      decision: "once",
      reason: null,
    });

    await act(async () => {
      actionResponse.resolve({ accepted: true });
      await actionResponse.promise;
    });
    expect(await screen.findByText("已提交，等待后端确认")).toBeTruthy();
    expect(screen.getByRole("heading", { name: "需要确认工具调用" })).toBeTruthy();
    expect((screen.getByRole("button", { name: "允许一次" }) as HTMLButtonElement).disabled).toBe(true);

    await act(async () => {
      eventGate.resolve();
      await Promise.resolve();
    });
    await waitFor(() => expect(screen.queryByRole("heading", { name: "需要确认工具调用" })).toBeNull());
    expect(resolveApproval).toHaveBeenCalledTimes(1);
  });

  it("retries free-text clarification after network, 409, and 422 errors and tolerates event-first completion", async () => {
    const actionResponse = deferred<ActionAccepted>();
    const eventGate = deferred<void>();
    const answerClarification = vi.fn()
      .mockRejectedValueOnce(new RunApiError("network", "offline", { retryable: true }))
      .mockRejectedValueOnce(new RunApiError("http", "conflict", { status: 409 }))
      .mockRejectedValueOnce(new RunApiError("http", "missing capability", { status: 422 }))
      .mockReturnValueOnce(actionResponse.promise);
    const runsClient = {
      listActiveRuns: vi.fn(async () => ({ items: [] })),
      createRun: vi.fn(async () => acceptedWithPending(CLARIFICATION_PENDING)),
      getRun: vi.fn(async () => run()),
      cancelRun: vi.fn(async () => run("cancelled")),
      resolveApproval: vi.fn(),
      answerClarification,
    } satisfies RunsApi;
    const user = userEvent.setup();
    render(
      <ChatRunProvider
        runEventsApi={gatedEvents(eventGate.promise, resolutionEvent("clarification.resolved", {
          requestId: CLARIFICATION_PENDING.requestId,
          resolvedBy: "user",
        }))}
        runsApi={runsClient}
      >
        <ChatWorkspace
          client={makeSessionClient()}
          profileClient={profileClient(withActionCapabilities(false, true))}
        />
      </ChatRunProvider>,
    );

    const composer = await screen.findByRole("textbox", { name: "消息" });
    await waitFor(() => expect((composer as HTMLTextAreaElement).disabled).toBe(false));
    await user.type(composer, "Deploy it");
    await user.click(screen.getByRole("button", { name: "发送消息" }));
    expect(await screen.findByText(CLARIFICATION_PENDING.question)).toBeTruthy();

    const answer = screen.getByRole("textbox", { name: "回答" });
    await user.type(answer, "  staging  ");
    const submit = screen.getByRole("button", { name: "提交回答" });

    await user.click(submit);
    expect(await screen.findByText("提交未送达本地后端，请重试或取消本次回复。")).toBeTruthy();
    await user.click(submit);
    expect(await screen.findByText("操作状态已发生变化，请重试；若仍失败可取消本次回复。")).toBeTruthy();
    await user.click(submit);
    expect(await screen.findByText("后端暂时无法处理此操作，请重试或取消本次回复。")).toBeTruthy();
    await user.click(submit);
    await waitFor(() => expect(answerClarification).toHaveBeenCalledTimes(4));
    expect(answerClarification).toHaveBeenLastCalledWith("run_1", "clarification_1", {
      answer: "staging",
    });

    await act(async () => {
      eventGate.resolve();
      await Promise.resolve();
    });
    await waitFor(() => expect(screen.queryByText(CLARIFICATION_PENDING.question)).toBeNull());
    await act(async () => {
      actionResponse.resolve({ accepted: true });
      await actionResponse.promise;
    });
    expect(screen.queryByText("已提交，等待后端确认")).toBeNull();
  });

  it("renders clarification choices as actions", async () => {
    const choicePending = { ...CLARIFICATION_PENDING, choices: ["staging", "production"] };
    const eventGate = deferred<void>();
    const answerClarification = vi.fn(async () => ({ accepted: true }) as ActionAccepted);
    const runsClient = {
      listActiveRuns: vi.fn(async () => ({ items: [] })),
      createRun: vi.fn(async () => acceptedWithPending(choicePending)),
      getRun: vi.fn(async () => run()),
      cancelRun: vi.fn(async () => run("cancelled")),
      resolveApproval: vi.fn(),
      answerClarification,
    } satisfies RunsApi;
    const user = userEvent.setup();
    render(
      <ChatRunProvider
        runEventsApi={gatedEvents(eventGate.promise, resolutionEvent("clarification.resolved", {
          requestId: choicePending.requestId,
          resolvedBy: "user",
        }))}
        runsApi={runsClient}
      >
        <ChatWorkspace
          client={makeSessionClient()}
          profileClient={profileClient(withActionCapabilities(false, true))}
        />
      </ChatRunProvider>,
    );

    const composer = await screen.findByRole("textbox", { name: "消息" });
    await waitFor(() => expect((composer as HTMLTextAreaElement).disabled).toBe(false));
    await user.type(composer, "Choose an environment");
    await user.click(screen.getByRole("button", { name: "发送消息" }));
    await user.click(await screen.findByRole("button", { name: "staging" }));
    await waitFor(() => expect(answerClarification).toHaveBeenCalledWith(
      "run_1",
      "clarification_1",
      { answer: "staging" },
    ));
    expect(screen.queryByRole("textbox", { name: "回答" })).toBeNull();

    await act(async () => {
      eventGate.resolve();
      await Promise.resolve();
    });
  });

  it("shows capability-disabled pending actions read-only and still permits cancellation", async () => {
    const resolveApproval = vi.fn();
    const cancelRun = vi.fn(async () => run("cancelled"));
    const runsClient = {
      listActiveRuns: vi.fn(async () => ({ items: [] })),
      createRun: vi.fn(async () => acceptedWithPending(APPROVAL_PENDING)),
      getRun: vi.fn(async () => run()),
      cancelRun,
      resolveApproval,
      answerClarification: vi.fn(),
    } satisfies RunsApi;
    const user = userEvent.setup();
    render(
      <ChatRunProvider runEventsApi={idleEvents()} runsApi={runsClient}>
        <ChatWorkspace client={makeSessionClient()} profileClient={profileClient()} />
      </ChatRunProvider>,
    );

    const composer = await screen.findByRole("textbox", { name: "消息" });
    await waitFor(() => expect((composer as HTMLTextAreaElement).disabled).toBe(false));
    await user.type(composer, "Use a gated tool");
    await user.click(screen.getByRole("button", { name: "发送消息" }));

    expect(await screen.findByText("当前后端未启用审批提交，可取消本次回复。")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "允许一次" })).toBeNull();
    expect(screen.queryByRole("button", { name: "拒绝" })).toBeNull();
    await user.click(screen.getByRole("button", { name: "取消本次回复" }));
    await waitFor(() => expect(cancelRun).toHaveBeenCalledWith("run_1"));
    expect(resolveApproval).not.toHaveBeenCalled();
  });

  it("keeps the composer gated when Run streaming is unavailable", async () => {
    const unavailable: Capabilities = {
      ...CAPABILITIES,
      engine: {
        ...CAPABILITIES.engine,
        available: false,
        features: { ...CAPABILITIES.engine.features, runStreaming: false },
      },
    };
    render(
      <ChatRunProvider>
        <ChatWorkspace client={makeSessionClient()} profileClient={profileClient(unavailable)} />
      </ChatRunProvider>,
    );
    expect(await screen.findByText("聊天引擎尚未就绪")).toBeTruthy();
    expect(screen.queryByRole("textbox", { name: "消息" })).toBeNull();
  });

  it("renders a non-retryable status when a Desktop session is required", async () => {
    const desktopOnlyClient: Pick<ProfilesApi, "getCapabilities" | "listProfiles"> = {
      getCapabilities: vi.fn(async () => {
        throw new DesktopConnectionError("desktop_unavailable", "desktop required");
      }),
      listProfiles: vi.fn(async () => [PROFILE]),
    };
    render(
      <ChatRunProvider>
        <ChatWorkspace client={makeSessionClient()} profileClient={desktopOnlyClient} />
      </ChatRunProvider>,
    );

    const status = await screen.findByRole("status");
    expect(status.textContent).toContain("请在 Desktop 中打开");
    expect(screen.queryByRole("button", { name: "重试" })).toBeNull();
    expect(screen.queryByRole("textbox", { name: "消息" })).toBeNull();
  });
});
