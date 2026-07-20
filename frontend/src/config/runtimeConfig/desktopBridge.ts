import { invoke, isTauri } from "@tauri-apps/api/core";
import {
  resolveBackendRuntimeConfig,
  type BackendRuntimeConfig,
} from "./backend";
import {
  resolveChatRuntimeConfig,
  type ChatRuntimeConfig,
} from "./chat";
import { FrontendRuntimeConfigError } from "./common";
import {
  resolvePetRuntimeConfig,
  type PetBuildEnvironment,
  type PetRuntimeConfig,
} from "./pet";
import {
  resolveSkillOperationRuntimeConfig,
  type SkillOperationRuntimeConfig,
} from "./skillOperations";

const FRONTEND_RUNTIME_CONFIG_COMMAND = "get_frontend_runtime_config";
const STARTUP_ERROR_MESSAGE =
  "Desktop startup configuration could not be loaded. Check the SYNTHCHAT_FRONTEND_* settings and restart SynthChat.";

type RuntimeConfigImportMeta = ImportMeta & { env?: PetBuildEnvironment };

type RuntimeConfigGlobal = typeof globalThis & {
  __SYNTHCHAT_RUNTIME_CONFIG__?: unknown;
};

export interface FrontendRuntimeConfigSnapshot {
  readonly backend: Readonly<BackendRuntimeConfig>;
  readonly chat: Readonly<ChatRuntimeConfig>;
  readonly skillOperations: Readonly<SkillOperationRuntimeConfig>;
  readonly pet: Readonly<PetRuntimeConfig>;
}

export class DesktopFrontendRuntimeConfigError extends Error {
  constructor() {
    super(STARTUP_ERROR_MESSAGE);
    this.name = "DesktopFrontendRuntimeConfigError";
  }
}

function strictRecord(
  value: unknown,
  name: string,
  expectedKeys: readonly string[],
): Record<string, unknown> {
  if (
    value === null
    || typeof value !== "object"
    || Array.isArray(value)
    || ![Object.prototype, null].includes(Object.getPrototypeOf(value))
    || Object.getOwnPropertySymbols(value).length !== 0
  ) {
    throw new FrontendRuntimeConfigError(`${name} must be a plain object.`);
  }

  const actualKeys = Object.getOwnPropertyNames(value);
  const expected = new Set(expectedKeys);
  if (
    actualKeys.length !== expectedKeys.length
    || actualKeys.some((key) => !expected.has(key))
  ) {
    throw new FrontendRuntimeConfigError(
      `${name} must contain exactly the reviewed runtime configuration fields.`,
    );
  }
  return value as Record<string, unknown>;
}

export function parseFrontendRuntimeConfig(
  value: unknown,
  build = (import.meta as RuntimeConfigImportMeta).env,
): FrontendRuntimeConfigSnapshot {
  const root = strictRecord(
    value,
    "Desktop runtime configuration",
    ["backend", "chat", "skillOperations", "pet"],
  );
  const backendSection = strictRecord(
    root.backend,
    "backend",
    ["healthTimeoutMs", "statusPollIntervalMs"],
  );
  const chatSection = strictRecord(
    root.chat,
    "chat",
    [
      "reconnectInitialDelayMs",
      "reconnectMaxAttempts",
      "reconnectMaxDelayMs",
      "runStatusPollIntervalMs",
    ],
  );
  const skillOperationSection = strictRecord(
    root.skillOperations,
    "skillOperations",
    ["maxPolls", "initialBackoffMs", "maxBackoffMs"],
  );
  const petSection = strictRecord(
    root.pet,
    "pet",
    ["frameUrl", "modelUrl", "statusPollIntervalMs"],
  );

  return Object.freeze({
    backend: Object.freeze(resolveBackendRuntimeConfig(backendSection, undefined)),
    chat: Object.freeze(resolveChatRuntimeConfig(chatSection, undefined)),
    skillOperations: Object.freeze(
      resolveSkillOperationRuntimeConfig(skillOperationSection, undefined),
    ),
    pet: Object.freeze(resolvePetRuntimeConfig(petSection, {}, build)),
  });
}

export function installFrontendRuntimeConfig(
  value: unknown,
  build = (import.meta as RuntimeConfigImportMeta).env,
): FrontendRuntimeConfigSnapshot {
  const snapshot = parseFrontendRuntimeConfig(value, build);
  (globalThis as RuntimeConfigGlobal).__SYNTHCHAT_RUNTIME_CONFIG__ = snapshot;
  return snapshot;
}

async function requestDesktopFrontendRuntimeConfig(): Promise<void> {
  try {
    installFrontendRuntimeConfig(await invoke(FRONTEND_RUNTIME_CONFIG_COMMAND));
  } catch {
    throw new DesktopFrontendRuntimeConfigError();
  }
}

let desktopLoadPromise: Promise<void> | undefined;

export function loadDesktopFrontendRuntimeConfig(): Promise<void> {
  if (!isTauri()) return Promise.resolve();
  desktopLoadPromise ??= requestDesktopFrontendRuntimeConfig();
  return desktopLoadPromise;
}
