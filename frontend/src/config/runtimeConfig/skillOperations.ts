import {
  FrontendRuntimeConfigError,
  readIntegerSetting,
  runtimeConfigSection,
} from "./common";

export interface SkillOperationBuildEnvironment {
  VITE_SYNTHCHAT_SKILL_OPERATION_INITIAL_BACKOFF_MS?: unknown;
  VITE_SYNTHCHAT_SKILL_OPERATION_MAX_BACKOFF_MS?: unknown;
  VITE_SYNTHCHAT_SKILL_OPERATION_MAX_POLLS?: unknown;
}

type SkillOperationImportMeta = ImportMeta & { env?: SkillOperationBuildEnvironment };

export interface SkillOperationRuntimeConfig {
  maxPolls: number;
  initialBackoffMs: number;
  maxBackoffMs: number;
}

export const DEFAULT_SKILL_OPERATION_RUNTIME_CONFIG: Readonly<SkillOperationRuntimeConfig> =
  Object.freeze({
    maxPolls: 30,
    initialBackoffMs: 250,
    maxBackoffMs: 2_000,
  });

export function resolveSkillOperationRuntimeConfig(
  runtime: Record<string, unknown> | undefined,
  build: SkillOperationBuildEnvironment | undefined,
): SkillOperationRuntimeConfig {
  const config = {
    maxPolls: readIntegerSetting({
      name: "skillOperations.maxPolls",
      runtimeValue: runtime?.maxPolls,
      buildValue: build?.VITE_SYNTHCHAT_SKILL_OPERATION_MAX_POLLS,
      defaultValue: DEFAULT_SKILL_OPERATION_RUNTIME_CONFIG.maxPolls,
      minimum: 1,
      maximum: 1_000,
    }),
    initialBackoffMs: readIntegerSetting({
      name: "skillOperations.initialBackoffMs",
      runtimeValue: runtime?.initialBackoffMs,
      buildValue: build?.VITE_SYNTHCHAT_SKILL_OPERATION_INITIAL_BACKOFF_MS,
      defaultValue: DEFAULT_SKILL_OPERATION_RUNTIME_CONFIG.initialBackoffMs,
      minimum: 1,
      maximum: 60_000,
    }),
    maxBackoffMs: readIntegerSetting({
      name: "skillOperations.maxBackoffMs",
      runtimeValue: runtime?.maxBackoffMs,
      buildValue: build?.VITE_SYNTHCHAT_SKILL_OPERATION_MAX_BACKOFF_MS,
      defaultValue: DEFAULT_SKILL_OPERATION_RUNTIME_CONFIG.maxBackoffMs,
      minimum: 1,
      maximum: 300_000,
    }),
  };
  if (config.maxBackoffMs < config.initialBackoffMs) {
    throw new FrontendRuntimeConfigError(
      "skillOperations.maxBackoffMs must be greater than or equal to skillOperations.initialBackoffMs.",
    );
  }
  return config;
}

export function readSkillOperationRuntimeConfig(
  build = (import.meta as SkillOperationImportMeta).env,
): SkillOperationRuntimeConfig {
  return resolveSkillOperationRuntimeConfig(runtimeConfigSection("skillOperations"), build);
}
