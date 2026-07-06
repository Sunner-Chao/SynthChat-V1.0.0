import type { ImageProvider, LlmProvider } from "./types";

// ---------------------------------------------------------------------------
// Secret masking
// ---------------------------------------------------------------------------

/** Masks a sensitive string for display. Returns "未记录" when empty. */
export function maskSecret(value?: string | null): string {
  const text = value?.trim() ?? "";
  if (!text) return "未记录";
  if (text.length <= 10) return `${text.slice(0, 2)}***`;
  return `${text.slice(0, 6)}...${text.slice(-4)}`;
}

// ---------------------------------------------------------------------------
// Date / time
// ---------------------------------------------------------------------------

/** Formats an ISO date string using the locale. Falls back to the raw value. */
export function formatTime(value: string): string {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString();
}

// ---------------------------------------------------------------------------
// LLM provider presets
// ---------------------------------------------------------------------------

const SYNTHAPI_CHAT_BASE_URL = "https://synthapi.asia/v1";

const PROVIDER_PRESET_LABELS: Record<string, string> = {
  synthapi: "SynthAPI",
  openai: "OpenAI (GPT)",
  openaiResponses: "OpenAI Responses",
  anthropic: "Anthropic (Claude)",
  google: "Google (Gemini)",
  deepseek: "DeepSeek",
  siliconflow: "硅基流动",
  custom: "自定义",
};

export function providerPresetLabel(id: string): string {
  return PROVIDER_PRESET_LABELS[id] ?? id;
}

export interface ProviderPresetDefaults {
  providerType: string;
  baseUrl: string;
  appendChatPath: boolean;
}

const PROVIDER_PRESET_DEFAULTS: Record<string, ProviderPresetDefaults> = {
  synthapi: { providerType: "openai_compatible", baseUrl: SYNTHAPI_CHAT_BASE_URL, appendChatPath: true },
  openai: { providerType: "openai_compatible", baseUrl: "https://api.openai.com/v1", appendChatPath: true },
  openaiResponses: { providerType: "openai_responses", baseUrl: "https://api.openai.com/v1", appendChatPath: true },
  anthropic: { providerType: "anthropic", baseUrl: "https://api.anthropic.com/v1", appendChatPath: true },
  google: { providerType: "gemini", baseUrl: "https://generativelanguage.googleapis.com/v1beta", appendChatPath: true },
  deepseek: { providerType: "openai_compatible", baseUrl: "https://api.deepseek.com", appendChatPath: true },
  siliconflow: { providerType: "openai_compatible", baseUrl: "https://api.siliconflow.cn/v1", appendChatPath: true },
  custom: { providerType: "openai_compatible", baseUrl: "", appendChatPath: true },
};

export function providerPresetDefaults(id: string): ProviderPresetDefaults {
  return PROVIDER_PRESET_DEFAULTS[id] ?? PROVIDER_PRESET_DEFAULTS.custom;
}

// ---------------------------------------------------------------------------
// Image provider labels
// ---------------------------------------------------------------------------

const IMAGE_PROVIDER_LABELS: Record<string, string> = {
  openai_image: "OpenAI Image",
  gemini_image: "Gemini Image",
  novelai: "NovelAI",
};

export function imageProviderTypeLabel(id: string): string {
  return IMAGE_PROVIDER_LABELS[id] ?? id;
}
