import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const tauri = vi.hoisted(() => ({
  invoke: vi.fn(),
  isTauri: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => tauri);

type RuntimeGlobal = typeof globalThis & {
  __SYNTHCHAT_RUNTIME_CONFIG__?: unknown;
};

const runtimeGlobal = globalThis as RuntimeGlobal;

function validDesktopConfig(): Record<string, unknown> {
  return {
    backend: {
      healthTimeoutMs: 4_000,
      statusPollIntervalMs: 15_000,
    },
    chat: {
      reconnectInitialDelayMs: 250,
      reconnectMaxAttempts: 30,
      reconnectMaxDelayMs: 8_000,
      runStatusPollIntervalMs: 2_000,
    },
    skillOperations: {
      maxPolls: 30,
      initialBackoffMs: 250,
      maxBackoffMs: 2_000,
    },
    pet: {
      frameUrl: "pet/index.html",
      modelUrl: "pet/model/Hiyori/Hiyori.model3.json",
      statusPollIntervalMs: 5_000,
    },
  };
}

beforeEach(() => {
  vi.resetModules();
  tauri.invoke.mockReset();
  tauri.isTauri.mockReset();
  delete runtimeGlobal.__SYNTHCHAT_RUNTIME_CONFIG__;
});

afterEach(() => {
  delete runtimeGlobal.__SYNTHCHAT_RUNTIME_CONFIG__;
});

describe("desktop frontend runtime configuration bridge", () => {
  it("does not invoke Tauri or replace a preset browser config outside Desktop", async () => {
    const preset = { backend: { healthTimeoutMs: 6_000 } };
    runtimeGlobal.__SYNTHCHAT_RUNTIME_CONFIG__ = preset;
    tauri.isTauri.mockReturnValue(false);
    const { loadDesktopFrontendRuntimeConfig } = await import("./desktopBridge");

    await loadDesktopFrontendRuntimeConfig();

    expect(tauri.invoke).not.toHaveBeenCalled();
    expect(runtimeGlobal.__SYNTHCHAT_RUNTIME_CONFIG__).toBe(preset);
  });

  it("invokes the fixed command once and installs an immutable typed snapshot", async () => {
    tauri.isTauri.mockReturnValue(true);
    tauri.invoke.mockResolvedValue(validDesktopConfig());
    const { loadDesktopFrontendRuntimeConfig } = await import("./desktopBridge");

    await Promise.all([
      loadDesktopFrontendRuntimeConfig(),
      loadDesktopFrontendRuntimeConfig(),
    ]);

    expect(tauri.invoke).toHaveBeenCalledTimes(1);
    expect(tauri.invoke).toHaveBeenCalledWith("get_frontend_runtime_config");
    const snapshot = runtimeGlobal.__SYNTHCHAT_RUNTIME_CONFIG__ as Record<string, unknown>;
    expect(Object.isFrozen(snapshot)).toBe(true);
    expect(Object.isFrozen(snapshot.backend)).toBe(true);
    expect(Object.isFrozen(snapshot.chat)).toBe(true);
    expect(Object.isFrozen(snapshot.skillOperations)).toBe(true);
    expect(Object.isFrozen(snapshot.pet)).toBe(true);
  });

  it("strictly rejects missing, extra, and invalid fields before installation", async () => {
    const { installFrontendRuntimeConfig } = await import("./desktopBridge");
    const missing = validDesktopConfig();
    delete (missing.backend as Record<string, unknown>).healthTimeoutMs;
    expect(() => installFrontendRuntimeConfig(missing)).toThrowError(
      /reviewed runtime configuration fields/u,
    );

    const extra = validDesktopConfig();
    (extra as Record<string, unknown>).token = "must-not-be-accepted";
    expect(() => installFrontendRuntimeConfig(extra)).toThrowError(
      /reviewed runtime configuration fields/u,
    );

    const invalidBackoff = validDesktopConfig();
    (invalidBackoff.chat as Record<string, unknown>).reconnectInitialDelayMs = 9_000;
    expect(() => installFrontendRuntimeConfig(invalidBackoff)).toThrowError(
      /reconnectMaxDelayMs.*reconnectInitialDelayMs/u,
    );
    expect(runtimeGlobal.__SYNTHCHAT_RUNTIME_CONFIG__).toBeUndefined();
  });

  it("keeps Pet resource validation in the frontend", async () => {
    const { installFrontendRuntimeConfig } = await import("./desktopBridge");
    const config = validDesktopConfig();
    (config.pet as Record<string, unknown>).frameUrl = "https://example.com/pet.html";

    expect(() => installFrontendRuntimeConfig(config)).toThrowError(/same-origin/u);
    expect(runtimeGlobal.__SYNTHCHAT_RUNTIME_CONFIG__).toBeUndefined();
  });

  it("returns a fixed startup error without echoing rejected values", async () => {
    const secretMarker = "runtime-secret-marker";
    tauri.isTauri.mockReturnValue(true);
    tauri.invoke.mockRejectedValue(new Error(secretMarker));
    const {
      DesktopFrontendRuntimeConfigError,
      loadDesktopFrontendRuntimeConfig,
    } = await import("./desktopBridge");

    const error = await loadDesktopFrontendRuntimeConfig().catch((cause: unknown) => cause);

    expect(error).toBeInstanceOf(DesktopFrontendRuntimeConfigError);
    expect((error as Error).message).not.toContain(secretMarker);
    expect((error as Error).message).toContain("SYNTHCHAT_FRONTEND_*");
  });
});
