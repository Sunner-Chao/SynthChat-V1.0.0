// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const tauri = vi.hoisted(() => ({
  invoke: vi.fn(),
  isTauri: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => tauri);

import { PetWindow } from "./PetWindow";

describe("PetWindow", () => {
  afterEach(cleanup);

  beforeEach(() => {
    tauri.isTauri.mockReturnValue(true);
    tauri.invoke.mockReset();
  });

  it("shows an explicit unavailable state outside Desktop", () => {
    tauri.isTauri.mockReturnValue(false);

    render(<PetWindow />);

    expect(screen.getByText("桌宠仅在 Desktop 应用中可用")).toBeTruthy();
  });

  it("renders Rust-backed profile and session state and routes drag through the bridge", async () => {
    const bridge = {
      open: vi.fn(async () => undefined),
      toggle: vi.fn(async () => true),
      startDragging: vi.fn(async () => undefined),
      setIgnoreCursorEvents: vi.fn(async () => undefined),
    };
    const runtimeApis = {
      profilesApi: {
        getCapabilities: vi.fn(async () => ({
          engine: { available: true },
          extensions: { activeRunDiscovery: true },
        })),
        listProfiles: vi.fn(async () => [{
          id: "default",
          displayName: "本地 Hermes",
          isDefault: true,
          isActive: true,
        }]),
      },
      sessionsApi: {
        listSessions: vi.fn(async () => ({
          items: [{ title: "迁移验证", id: "session_1" }],
          nextCursor: null,
        })),
      },
      runsApi: { listActiveRuns: vi.fn(async () => ({ items: [] })) },
      runEventsApi: { streamRunEvents: vi.fn() },
    };

    render(
      <PetWindow
        bridge={bridge}
        pollIntervalMs={0}
        runtimeApis={runtimeApis as never}
      />,
    );

    expect(await screen.findByText("准备就绪")).toBeTruthy();
    expect(screen.getByText("最近会话：迁移验证")).toBeTruthy();
    fireEvent.pointerDown(screen.getByRole("button", { name: "拖动桌宠" }));
    expect(bridge.startDragging).toHaveBeenCalledTimes(1);
  });

  it("accepts controlled Pet resource overrides through props", () => {
    render(
      <PetWindow
        frameUrl="/runtime/pet-frame.html"
        modelUrl="/runtime/model.model3.json"
        pollIntervalMs={0}
      />,
    );

    const frame = screen.getByTitle("SynthChat Live2D 桌宠") as HTMLIFrameElement;
    expect(frame.getAttribute("src")).toBe("/runtime/pet-frame.html");

    const postMessage = vi.spyOn(frame.contentWindow!, "postMessage");
    fireEvent.load(frame);
    expect(postMessage).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "load",
        url: "/runtime/model.model3.json",
      }),
      window.location.origin,
    );
  });

  it("sends through the Rust Run API and renders streamed deltas", async () => {
    const runtimeApis = {
      profilesApi: {
        getCapabilities: vi.fn(async () => ({
          engine: { available: true },
          extensions: { activeRunDiscovery: true },
        })),
        listProfiles: vi.fn(async () => [{
          id: "default",
          displayName: "本地 Hermes",
          isDefault: true,
          isActive: true,
        }]),
      },
      sessionsApi: {
        listSessions: vi.fn(async () => ({
          items: [{ id: "session_1", profileId: "default", title: "Pet 对话", archived: false }],
          nextCursor: null,
        })),
      },
      runsApi: { listActiveRuns: vi.fn(async () => ({ items: [] })) },
      runEventsApi: { streamRunEvents: vi.fn(async function* () {
        yield {
          id: "run_1:1",
          event: "message.delta",
          payload: { sequence: 1, data: { delta: "你好，" } },
        };
        yield {
          id: "run_1:2",
          event: "run.completed",
          payload: { sequence: 2, data: { messageId: "message_2" } },
        };
      }) },
    };
    const interactionApis = {
      profilesApi: {
        getProfileConfig: vi.fn(async () => ({
          etag: '"1"',
          value: {
            revision: "1",
            model: { provider: "openai-api", model: "gpt-5.5", baseUrl: null, reasoningEffort: null },
            codeExecution: { mode: "strict", timeoutSeconds: 300, maxToolCalls: 50 },
            toolsets: {},
            skills: {},
            memoryProvider: "none",
            platforms: {},
            extensions: {},
          },
        })),
        updateProfileConfig: vi.fn(),
      },
      sessionsApi: {
        createSession: vi.fn(),
        listMessages: vi.fn(async () => ({ items: [], nextCursor: null, snapshotLastSequence: 0, firstSequence: null, lastSequence: null })),
      },
      runsApi: {
        createRun: vi.fn(async () => ({
          run: { id: "run_1", sessionId: "session_1", profileId: "default", status: "running", lastSequence: 0 },
          userMessage: {},
          disposition: "started",
          queueItemId: null,
          sessionRevision: "2",
        })),
        getRun: vi.fn(),
        cancelRun: vi.fn(),
      },
      runEventsApi: {
        streamRunEvents: vi.fn(async function* () {
          yield {
            id: "run_1:1",
            event: "message.delta",
            payload: { sequence: 1, data: { delta: "你好，" } },
          };
          yield {
            id: "run_1:2",
            event: "run.completed",
            payload: { sequence: 2, data: { messageId: "message_2" } },
          };
        }),
      },
    };

    render(
      <PetWindow
        interactionApis={interactionApis as never}
        pollIntervalMs={0}
        runtimeApis={runtimeApis as never}
      />,
    );

    const input = await screen.findByRole("textbox", { name: "给 Pet 发送消息" });
    fireEvent.change(input, { target: { value: "介绍一下 Hermes" } });
    fireEvent.submit(input.closest("form")!);

    await waitFor(() => expect(interactionApis.runsApi.createRun).toHaveBeenCalledWith(
      "session_1",
      expect.objectContaining({ message: { text: "介绍一下 Hermes", fileIds: [] } }),
      expect.stringMatching(/^pet-/u),
    ));
    expect(await screen.findByText("你好，")).toBeTruthy();
    expect(screen.getByText("回复完成")).toBeTruthy();
  });

  it("loads and saves the selected Profile model and switches trusted pet assets", async () => {
    const runtimeApis = {
      profilesApi: {
        getCapabilities: vi.fn(async () => ({ engine: { available: true }, extensions: { activeRunDiscovery: true } })),
        listProfiles: vi.fn(async () => [
          { id: "default", displayName: "Default", isDefault: true, isActive: true },
          { id: "work", displayName: "Work", isDefault: false, isActive: false },
        ]),
      },
      sessionsApi: { listSessions: vi.fn(async () => ({ items: [], nextCursor: null })) },
      runsApi: { listActiveRuns: vi.fn(async () => ({ items: [] })) },
      runEventsApi: { streamRunEvents: vi.fn() },
    };
    const config = {
      etag: '"7"',
      value: {
        revision: "7",
        model: { provider: "openai-api", model: "gpt-5.5", baseUrl: null, reasoningEffort: null },
        codeExecution: { mode: "strict", timeoutSeconds: 300, maxToolCalls: 50 },
        toolsets: {}, skills: {}, memoryProvider: "none", platforms: {}, extensions: {},
      },
    };
    const getProfileConfig = vi.fn(async () => config);
    const updateProfileConfig = vi.fn(async (_id: string, patch: unknown) => ({
      ...config,
      etag: '"8"',
      value: { ...config.value, model: { ...config.value.model, ...(patch as { model: object }).model } },
    }));
    const interactionApis = {
      profilesApi: { getProfileConfig, updateProfileConfig },
      sessionsApi: { createSession: vi.fn(), listMessages: vi.fn() },
      runsApi: { createRun: vi.fn(), getRun: vi.fn(), cancelRun: vi.fn() },
      runEventsApi: { streamRunEvents: vi.fn() },
    };

    render(
      <PetWindow
        interactionApis={interactionApis as never}
        pollIntervalMs={0}
        runtimeApis={runtimeApis as never}
      />,
    );

    const frame = screen.getByTitle("SynthChat Live2D 桌宠") as HTMLIFrameElement;
    const postMessage = vi.spyOn(frame.contentWindow!, "postMessage");
    fireEvent.click(screen.getByRole("button", { name: "打开桌宠设置" }));
    const modelInput = await screen.findByRole("textbox", { name: "Pet 推理模型" });
    await waitFor(() => expect(getProfileConfig).toHaveBeenCalledWith("default", expect.anything()));
    fireEvent.change(modelInput, { target: { value: "gpt-5.6" } });
    fireEvent.submit(modelInput.closest("form")!);
    await waitFor(() => expect(updateProfileConfig).toHaveBeenCalledWith(
      "default",
      { model: { model: "gpt-5.6" } },
      '"7"',
    ));

    fireEvent.click(screen.getByRole("button", { name: "Tororo" }));
    expect(postMessage).toHaveBeenCalledWith(
      expect.objectContaining({ type: "load", url: "/pet/model/Tororo/tororo.model3.json" }),
      window.location.origin,
    );
  });

  it("cancels an active Pet Run through the Rust API", async () => {
    const runtimeApis = {
      profilesApi: {
        getCapabilities: vi.fn(async () => ({ engine: { available: true }, extensions: { activeRunDiscovery: true } })),
        listProfiles: vi.fn(async () => [{ id: "default", displayName: "Default", isDefault: true, isActive: true }]),
      },
      sessionsApi: {
        listSessions: vi.fn(async () => ({ items: [{ id: "session_1", profileId: "default", title: "Pet", archived: false }], nextCursor: null })),
      },
      runsApi: { listActiveRuns: vi.fn(async () => ({ items: [] })) },
      runEventsApi: { streamRunEvents: vi.fn() },
    };
    const cancelRun = vi.fn(async () => ({ id: "run_1", sessionId: "session_1", profileId: "default", status: "cancelled", lastSequence: 1 }));
    const interactionApis = {
      profilesApi: { getProfileConfig: vi.fn(), updateProfileConfig: vi.fn() },
      sessionsApi: { createSession: vi.fn(), listMessages: vi.fn() },
      runsApi: {
        createRun: vi.fn(async () => ({
          run: { id: "run_1", sessionId: "session_1", profileId: "default", status: "running", lastSequence: 0 },
          userMessage: {}, disposition: "started", queueItemId: null, sessionRevision: "2",
        })),
        getRun: vi.fn(),
        cancelRun,
      },
      runEventsApi: {
        streamRunEvents: vi.fn(async function* (_runId: string, options: { signal?: AbortSignal } = {}) {
          yield { id: "run_1:1", event: "message.delta", payload: { sequence: 1, data: { delta: "处理中" } } };
          await new Promise<void>((resolve) => {
            if (options.signal?.aborted) resolve();
            else options.signal?.addEventListener("abort", () => resolve(), { once: true });
          });
        }),
      },
    };

    render(<PetWindow interactionApis={interactionApis as never} pollIntervalMs={0} runtimeApis={runtimeApis as never} />);
    const input = await screen.findByRole("textbox", { name: "给 Pet 发送消息" });
    fireEvent.change(input, { target: { value: "停止测试" } });
    fireEvent.submit(input.closest("form")!);

    fireEvent.click(await screen.findByRole("button", { name: "停止 Pet 回复" }));
    await waitFor(() => expect(cancelRun).toHaveBeenCalledWith("run_1"));
    expect(await screen.findByText("已停止回复")).toBeTruthy();
  });

  it("offers refresh recovery after an SSE disconnect", async () => {
    const runtimeApis = {
      profilesApi: {
        getCapabilities: vi.fn(async () => ({ engine: { available: true }, extensions: { activeRunDiscovery: true } })),
        listProfiles: vi.fn(async () => [{ id: "default", displayName: "Default", isDefault: true, isActive: true }]),
      },
      sessionsApi: {
        listSessions: vi.fn(async () => ({ items: [{ id: "session_1", profileId: "default", title: "Pet", archived: false }], nextCursor: null })),
      },
      runsApi: { listActiveRuns: vi.fn(async () => ({ items: [] })) },
      runEventsApi: { streamRunEvents: vi.fn() },
    };
    const getRun = vi.fn(async () => ({ id: "run_1", sessionId: "session_1", profileId: "default", status: "completed", lastSequence: 2, error: null }));
    const interactionApis = {
      profilesApi: { getProfileConfig: vi.fn(), updateProfileConfig: vi.fn() },
      sessionsApi: {
        createSession: vi.fn(),
        listMessages: vi.fn(async () => ({
          items: [{
            id: "message_2", sessionId: "session_1", sequence: 2, role: "assistant",
            parts: [{ type: "text", text: "断线后恢复的回复" }], reasoning: null, toolCalls: [], usage: null,
            createdAt: "2026-07-20T00:00:00Z",
          }],
          nextCursor: null, snapshotLastSequence: 2, firstSequence: 2, lastSequence: 2,
        })),
      },
      runsApi: {
        createRun: vi.fn(async () => ({
          run: { id: "run_1", sessionId: "session_1", profileId: "default", status: "running", lastSequence: 0 },
          userMessage: {}, disposition: "started", queueItemId: null, sessionRevision: "2",
        })),
        getRun,
        cancelRun: vi.fn(),
      },
      runEventsApi: { streamRunEvents: vi.fn(async function* () { throw new Error("SSE disconnected"); }) },
    };

    render(<PetWindow interactionApis={interactionApis as never} pollIntervalMs={0} runtimeApis={runtimeApis as never} />);
    const input = await screen.findByRole("textbox", { name: "给 Pet 发送消息" });
    fireEvent.change(input, { target: { value: "断线测试" } });
    fireEvent.submit(input.closest("form")!);
    await waitFor(() => expect(screen.getByText("实时回复已断开")).toBeTruthy());

    fireEvent.click(screen.getByRole("button", { name: "刷新桌宠状态" }));
    await waitFor(() => expect(getRun).toHaveBeenCalledWith("run_1"));
    expect(await screen.findByText("断线后恢复的回复")).toBeTruthy();
  });
});
