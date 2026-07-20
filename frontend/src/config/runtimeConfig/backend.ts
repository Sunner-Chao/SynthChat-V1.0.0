import {
  readIntegerSetting,
  runtimeConfigSection,
} from "./common";

export interface BackendBuildEnvironment {
  VITE_SYNTHCHAT_BACKEND_HEALTH_TIMEOUT_MS?: unknown;
  VITE_SYNTHCHAT_BACKEND_STATUS_POLL_INTERVAL_MS?: unknown;
}

type BackendImportMeta = ImportMeta & { env?: BackendBuildEnvironment };

export interface BackendRuntimeConfig {
  healthTimeoutMs: number;
  statusPollIntervalMs: number;
}

export const DEFAULT_BACKEND_RUNTIME_CONFIG: Readonly<BackendRuntimeConfig> = Object.freeze({
  healthTimeoutMs: 4_000,
  statusPollIntervalMs: 15_000,
});

export function resolveBackendRuntimeConfig(
  runtime: Record<string, unknown> | undefined,
  build: BackendBuildEnvironment | undefined,
): BackendRuntimeConfig {
  return {
    healthTimeoutMs: readIntegerSetting({
      name: "backend.healthTimeoutMs",
      runtimeValue: runtime?.healthTimeoutMs,
      buildValue: build?.VITE_SYNTHCHAT_BACKEND_HEALTH_TIMEOUT_MS,
      defaultValue: DEFAULT_BACKEND_RUNTIME_CONFIG.healthTimeoutMs,
      minimum: 100,
      maximum: 120_000,
    }),
    statusPollIntervalMs: readIntegerSetting({
      name: "backend.statusPollIntervalMs",
      runtimeValue: runtime?.statusPollIntervalMs,
      buildValue: build?.VITE_SYNTHCHAT_BACKEND_STATUS_POLL_INTERVAL_MS,
      defaultValue: DEFAULT_BACKEND_RUNTIME_CONFIG.statusPollIntervalMs,
      minimum: 1_000,
      maximum: 3_600_000,
    }),
  };
}

export function readBackendRuntimeConfig(
  build = (import.meta as BackendImportMeta).env,
): BackendRuntimeConfig {
  return resolveBackendRuntimeConfig(runtimeConfigSection("backend"), build);
}
