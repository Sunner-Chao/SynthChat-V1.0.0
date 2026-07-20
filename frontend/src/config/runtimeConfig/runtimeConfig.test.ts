import { afterEach, describe, expect, it, vi } from "vitest";
import {
  DEFAULT_BACKEND_RUNTIME_CONFIG,
  readBackendRuntimeConfig,
} from "./backend";
import {
  DEFAULT_CHAT_RUNTIME_CONFIG,
  readChatRuntimeConfig,
} from "./chat";
import { FrontendRuntimeConfigError } from "./common";
import {
  DEFAULT_PET_RUNTIME_CONFIG,
  readPetRuntimeConfig,
} from "./pet";
import {
  DEFAULT_SKILL_OPERATION_RUNTIME_CONFIG,
  readSkillOperationRuntimeConfig,
} from "./skillOperations";

type RuntimeGlobal = typeof globalThis & {
  __SYNTHCHAT_RUNTIME_CONFIG__?: unknown;
};

const runtimeGlobal = globalThis as RuntimeGlobal;

afterEach(() => {
  delete runtimeGlobal.__SYNTHCHAT_RUNTIME_CONFIG__;
  vi.unstubAllEnvs();
});

describe("frontend runtime configuration", () => {
  it("uses generic defaults when no override is present", () => {
    expect(readBackendRuntimeConfig()).toEqual(DEFAULT_BACKEND_RUNTIME_CONFIG);
    expect(readChatRuntimeConfig()).toEqual(DEFAULT_CHAT_RUNTIME_CONFIG);
    expect(readSkillOperationRuntimeConfig()).toEqual(
      DEFAULT_SKILL_OPERATION_RUNTIME_CONFIG,
    );
    expect(readPetRuntimeConfig()).toEqual(DEFAULT_PET_RUNTIME_CONFIG);
  });

  it("prefers typed global injection over VITE build values per setting", () => {
    runtimeGlobal.__SYNTHCHAT_RUNTIME_CONFIG__ = {
      backend: { healthTimeoutMs: 6500 },
    };

    expect(readBackendRuntimeConfig({
      VITE_SYNTHCHAT_BACKEND_HEALTH_TIMEOUT_MS: "5000",
      VITE_SYNTHCHAT_BACKEND_STATUS_POLL_INTERVAL_MS: "20000",
    })).toEqual({
      healthTimeoutMs: 6500,
      statusPollIntervalMs: 20_000,
    });
  });

  it.each([
    { backend: { healthTimeoutMs: "5000" } },
    { backend: { healthTimeoutMs: 99 } },
    { backend: { statusPollIntervalMs: 999 } },
    { skillOperations: { maxPolls: 0 } },
    { skillOperations: { initialBackoffMs: 0 } },
  ])("rejects an invalid typed runtime value: %j", (runtimeConfig) => {
    runtimeGlobal.__SYNTHCHAT_RUNTIME_CONFIG__ = runtimeConfig;

    expect(() => {
      if ("backend" in runtimeConfig) readBackendRuntimeConfig();
      else readSkillOperationRuntimeConfig();
    }).toThrow(FrontendRuntimeConfigError);
  });

  it("rejects malformed VITE integers and an inverted backoff range", () => {
    expect(() => readSkillOperationRuntimeConfig({
      VITE_SYNTHCHAT_SKILL_OPERATION_MAX_POLLS: "2.5",
    })).toThrow(
      FrontendRuntimeConfigError,
    );

    runtimeGlobal.__SYNTHCHAT_RUNTIME_CONFIG__ = {
      skillOperations: { initialBackoffMs: 3000, maxBackoffMs: 2000 },
    };
    expect(() => readSkillOperationRuntimeConfig()).toThrowError(
      /maxBackoffMs.*initialBackoffMs/u,
    );
  });

  it("resolves Pet resources below Vite BASE_URL", () => {
    expect(readPetRuntimeConfig({}, { BASE_URL: "/desktop/" })).toEqual({
      frameUrl: "/desktop/pet/index.html",
      modelUrl: "/desktop/pet/model/Hiyori/Hiyori.model3.json",
      statusPollIntervalMs: 5_000,
    });
  });

  it("accepts relative Pet overrides and rejects cross-origin resources", () => {
    runtimeGlobal.__SYNTHCHAT_RUNTIME_CONFIG__ = {
      pet: {
        frameUrl: "pet/custom.html",
        modelUrl: "/models/custom.model3.json",
      },
    };
    expect(readPetRuntimeConfig({}, { BASE_URL: "/desktop/" })).toEqual({
      frameUrl: "/desktop/pet/custom.html",
      modelUrl: "/models/custom.model3.json",
      statusPollIntervalMs: 5_000,
    });

    runtimeGlobal.__SYNTHCHAT_RUNTIME_CONFIG__ = {
      pet: { frameUrl: "https://example.com/pet.html" },
    };
    expect(() => readPetRuntimeConfig()).toThrowError(/same-origin/u);
  });

  it("loads bounded Chat reconnect and Pet polling settings", () => {
    runtimeGlobal.__SYNTHCHAT_RUNTIME_CONFIG__ = {
      chat: {
        reconnectInitialDelayMs: 400,
        reconnectMaxAttempts: 45,
        reconnectMaxDelayMs: 12_000,
        runStatusPollIntervalMs: 3_500,
      },
      pet: { statusPollIntervalMs: 8_000 },
    };

    expect(readChatRuntimeConfig()).toEqual({
      reconnectInitialDelayMs: 400,
      reconnectMaxAttempts: 45,
      reconnectMaxDelayMs: 12_000,
      runStatusPollIntervalMs: 3_500,
    });
    expect(readPetRuntimeConfig().statusPollIntervalMs).toBe(8_000);
  });

  it("loads the Chat Run status polling interval from the VITE fallback", () => {
    expect(readChatRuntimeConfig({
      VITE_SYNTHCHAT_CHAT_RUN_STATUS_POLL_INTERVAL_MS: "4500",
    }).runStatusPollIntervalMs).toBe(4_500);
  });

  it("rejects inverted Chat reconnect backoff and too-fast Pet polling", () => {
    runtimeGlobal.__SYNTHCHAT_RUNTIME_CONFIG__ = {
      chat: { reconnectInitialDelayMs: 2_000, reconnectMaxDelayMs: 1_000 },
    };
    expect(() => readChatRuntimeConfig()).toThrowError(
      /reconnectMaxDelayMs.*reconnectInitialDelayMs/u,
    );

    runtimeGlobal.__SYNTHCHAT_RUNTIME_CONFIG__ = {
      chat: { runStatusPollIntervalMs: 499 },
    };
    expect(() => readChatRuntimeConfig()).toThrow(FrontendRuntimeConfigError);

    runtimeGlobal.__SYNTHCHAT_RUNTIME_CONFIG__ = {
      pet: { statusPollIntervalMs: 999 },
    };
    expect(() => readPetRuntimeConfig()).toThrow(FrontendRuntimeConfigError);
  });
});
