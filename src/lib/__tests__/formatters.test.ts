import { describe, it, expect } from "vitest";
import {
  maskSecret,
  formatTime,
  providerPresetLabel,
  providerPresetDefaults,
  imageProviderTypeLabel,
} from "../formatters";

describe("maskSecret", () => {
  it("returns 未记录 for empty string", () => {
    expect(maskSecret("")).toBe("未记录");
    expect(maskSecret(null)).toBe("未记录");
    expect(maskSecret(undefined)).toBe("未记录");
    expect(maskSecret("   ")).toBe("未记录");
  });

  it("masks short secrets (≤10 chars)", () => {
    const result = maskSecret("abc");
    expect(result).toBe("ab***");
  });

  it("masks long secrets with prefix and suffix", () => {
    const result = maskSecret("sk-abcdefghijklmnop");
    expect(result).toMatch(/^sk-abc\.\.\.mnop$/);
  });
});

describe("formatTime", () => {
  it("formats a valid ISO date string", () => {
    const iso = "2026-01-15T10:30:00.000Z";
    const result = formatTime(iso);
    expect(result).not.toBe(iso);
    expect(typeof result).toBe("string");
  });

  it("returns the raw value for invalid date strings", () => {
    expect(formatTime("not-a-date")).toBe("not-a-date");
    expect(formatTime("")).toBe("");
  });
});

describe("providerPresetLabel", () => {
  it("returns display label for known providers", () => {
    expect(providerPresetLabel("openai")).toBe("OpenAI (GPT)");
    expect(providerPresetLabel("anthropic")).toBe("Anthropic (Claude)");
    expect(providerPresetLabel("google")).toBe("Google (Gemini)");
    expect(providerPresetLabel("deepseek")).toBe("DeepSeek");
    expect(providerPresetLabel("siliconflow")).toBe("硅基流动");
    expect(providerPresetLabel("synthapi")).toBe("SynthAPI");
    expect(providerPresetLabel("custom")).toBe("自定义");
  });

  it("returns the raw id for unknown providers", () => {
    expect(providerPresetLabel("unknown-provider")).toBe("unknown-provider");
  });
});

describe("providerPresetDefaults", () => {
  it("returns correct defaults for anthropic", () => {
    const defaults = providerPresetDefaults("anthropic");
    expect(defaults.providerType).toBe("anthropic");
    expect(defaults.baseUrl).toBe("https://api.anthropic.com/v1");
    expect(defaults.appendChatPath).toBe(true);
  });

  it("returns custom defaults for unknown ids", () => {
    const defaults = providerPresetDefaults("unknown");
    expect(defaults.providerType).toBe("openai_compatible");
    expect(defaults.baseUrl).toBe("");
  });

  it("includes synthapi preset", () => {
    const defaults = providerPresetDefaults("synthapi");
    expect(defaults.baseUrl).toContain("synthapi.asia");
  });
});

describe("imageProviderTypeLabel", () => {
  it("returns display label for known types", () => {
    expect(imageProviderTypeLabel("openai_image")).toBe("OpenAI Image");
    expect(imageProviderTypeLabel("gemini_image")).toBe("Gemini Image");
    expect(imageProviderTypeLabel("novelai")).toBe("NovelAI");
  });

  it("returns the raw id for unknown types", () => {
    expect(imageProviderTypeLabel("comfyui")).toBe("comfyui");
  });
});
