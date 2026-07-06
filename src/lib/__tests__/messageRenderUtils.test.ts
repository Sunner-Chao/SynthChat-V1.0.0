import { describe, it, expect } from "vitest";
import {
  clampCount,
  previewText,
  composerErrorText,
  normalizeToolDetailText,
  estimateMessageTokens,
} from "../messageRenderUtils";

describe("clampCount", () => {
  it("returns fallback for undefined", () => {
    expect(clampCount(undefined, 50, 0, 200)).toBe(50);
  });

  it("clamps to minimum", () => {
    expect(clampCount(-5, 50, 0, 200)).toBe(0);
  });

  it("clamps to maximum", () => {
    expect(clampCount(999, 50, 0, 200)).toBe(200);
  });

  it("returns value when in range", () => {
    expect(clampCount(75, 50, 0, 200)).toBe(75);
  });

  it("floors floating point values", () => {
    expect(clampCount(7.9, 5, 0, 20)).toBe(7);
  });
});

describe("previewText", () => {
  it("returns text unchanged when within limit", () => {
    const text = "short text";
    expect(previewText(text, 100)).toBe(text);
  });

  it("truncates long text with notice", () => {
    const long = "A".repeat(500);
    const result = previewText(long, 50);
    expect(result.startsWith("A".repeat(50))).toBe(true);
    expect(result).toContain("内容过长");
    expect(result).toContain("50");
  });
});

describe("composerErrorText", () => {
  it("returns generic message for empty error", () => {
    expect(composerErrorText("")).toBe("发送失败。");
    expect(composerErrorText(null)).toBe("发送失败。");
  });

  it("extracts message from Error instance", () => {
    const result = composerErrorText(new Error("network timeout"));
    expect(result).toContain("network timeout");
    expect(result.startsWith("发送失败：")).toBe(true);
  });

  it("strips 'bad request:' prefix", () => {
    const result = composerErrorText("bad request: rate limit exceeded");
    expect(result).toContain("rate limit exceeded");
    expect(result).not.toContain("bad request:");
  });

  it("truncates long error messages", () => {
    const longMsg = "x".repeat(100);
    const result = composerErrorText(longMsg);
    expect(result.length).toBeLessThan(100 + "发送失败：".length + 5);
    expect(result.endsWith("...")).toBe(true);
  });
});

describe("normalizeToolDetailText", () => {
  it("trims whitespace and collapses spaces", () => {
    expect(normalizeToolDetailText("  hello  world  ")).toBe("hello world");
    expect(normalizeToolDetailText("a\nb\tc")).toBe("a b c");
  });

  it("returns empty string for blank input", () => {
    expect(normalizeToolDetailText("   ")).toBe("");
  });
});

describe("estimateMessageTokens", () => {
  it("returns 0 for empty string", () => {
    expect(estimateMessageTokens("")).toBe(0);
  });

  it("returns a positive value for non-empty text", () => {
    expect(estimateMessageTokens("Hello world")).toBeGreaterThan(0);
  });

  it("estimates more tokens for longer text", () => {
    const short = estimateMessageTokens("Hi");
    const long = estimateMessageTokens("Hello, this is a much longer sentence with many words.");
    expect(long).toBeGreaterThan(short);
  });

  it("handles Chinese characters", () => {
    const tokens = estimateMessageTokens("你好世界");
    expect(tokens).toBeGreaterThan(0);
  });
});
