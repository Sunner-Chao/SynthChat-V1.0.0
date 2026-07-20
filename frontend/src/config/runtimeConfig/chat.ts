import {
  FrontendRuntimeConfigError,
  readIntegerSetting,
  runtimeConfigSection,
} from "./common";

export interface ChatBuildEnvironment {
  VITE_SYNTHCHAT_CHAT_RECONNECT_INITIAL_DELAY_MS?: unknown;
  VITE_SYNTHCHAT_CHAT_RECONNECT_MAX_ATTEMPTS?: unknown;
  VITE_SYNTHCHAT_CHAT_RECONNECT_MAX_DELAY_MS?: unknown;
  VITE_SYNTHCHAT_CHAT_RUN_STATUS_POLL_INTERVAL_MS?: unknown;
}

type ChatImportMeta = ImportMeta & { env?: ChatBuildEnvironment };

export interface ChatRuntimeConfig {
  reconnectInitialDelayMs: number;
  reconnectMaxAttempts: number;
  reconnectMaxDelayMs: number;
  runStatusPollIntervalMs: number;
}

export const DEFAULT_CHAT_RUNTIME_CONFIG: Readonly<ChatRuntimeConfig> = Object.freeze({
  reconnectInitialDelayMs: 250,
  reconnectMaxAttempts: 30,
  reconnectMaxDelayMs: 8_000,
  runStatusPollIntervalMs: 2_000,
});

export function resolveChatRuntimeConfig(
  runtime: Record<string, unknown> | undefined,
  build: ChatBuildEnvironment | undefined,
): ChatRuntimeConfig {
  const config = {
    reconnectInitialDelayMs: readIntegerSetting({
      name: "chat.reconnectInitialDelayMs",
      runtimeValue: runtime?.reconnectInitialDelayMs,
      buildValue: build?.VITE_SYNTHCHAT_CHAT_RECONNECT_INITIAL_DELAY_MS,
      defaultValue: DEFAULT_CHAT_RUNTIME_CONFIG.reconnectInitialDelayMs,
      minimum: 10,
      maximum: 60_000,
    }),
    reconnectMaxAttempts: readIntegerSetting({
      name: "chat.reconnectMaxAttempts",
      runtimeValue: runtime?.reconnectMaxAttempts,
      buildValue: build?.VITE_SYNTHCHAT_CHAT_RECONNECT_MAX_ATTEMPTS,
      defaultValue: DEFAULT_CHAT_RUNTIME_CONFIG.reconnectMaxAttempts,
      minimum: 0,
      maximum: 10_000,
    }),
    reconnectMaxDelayMs: readIntegerSetting({
      name: "chat.reconnectMaxDelayMs",
      runtimeValue: runtime?.reconnectMaxDelayMs,
      buildValue: build?.VITE_SYNTHCHAT_CHAT_RECONNECT_MAX_DELAY_MS,
      defaultValue: DEFAULT_CHAT_RUNTIME_CONFIG.reconnectMaxDelayMs,
      minimum: 10,
      maximum: 300_000,
    }),
    runStatusPollIntervalMs: readIntegerSetting({
      name: "chat.runStatusPollIntervalMs",
      runtimeValue: runtime?.runStatusPollIntervalMs,
      buildValue: build?.VITE_SYNTHCHAT_CHAT_RUN_STATUS_POLL_INTERVAL_MS,
      defaultValue: DEFAULT_CHAT_RUNTIME_CONFIG.runStatusPollIntervalMs,
      minimum: 500,
      maximum: 60_000,
    }),
  };
  if (config.reconnectMaxDelayMs < config.reconnectInitialDelayMs) {
    throw new FrontendRuntimeConfigError(
      "chat.reconnectMaxDelayMs must be greater than or equal to chat.reconnectInitialDelayMs.",
    );
  }
  return config;
}

export function readChatRuntimeConfig(
  build = (import.meta as ChatImportMeta).env,
): ChatRuntimeConfig {
  return resolveChatRuntimeConfig(runtimeConfigSection("chat"), build);
}
