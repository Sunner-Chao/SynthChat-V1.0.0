// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
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
});
