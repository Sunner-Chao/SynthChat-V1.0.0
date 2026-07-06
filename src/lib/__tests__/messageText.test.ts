import { describe, it, expect } from "vitest";
import {
  stripToolDirectiveBlocks,
  isAttachmentContextLine,
  isMediaDirectiveLine,
  renderTextForMessage,
  displayTextForMessage,
  sanitizeSpeechText,
} from "../messageText";

describe("stripToolDirectiveBlocks", () => {
  it("returns content unchanged when no tool blocks", () => {
    const input = "Hello world\nThis is normal text.";
    expect(stripToolDirectiveBlocks(input)).toBe(input);
  });

  it("strips content from <tool_call> tag onward", () => {
    const input = "Normal text\n<tool_call>some call</tool_call>";
    const result = stripToolDirectiveBlocks(input);
    expect(result).toBe("Normal text");
  });

  it("strips content from <tool_result> tag onward", () => {
    const input = "Before\n<tool_result>result</tool_result>";
    expect(stripToolDirectiveBlocks(input)).toBe("Before");
  });
});

describe("isAttachmentContextLine", () => {
  it("returns true for valid attachment JSON lines", () => {
    const line = JSON.stringify({ type: "attachment", url: "file://test.jpg" });
    expect(isAttachmentContextLine(line)).toBe(true);
  });

  it("returns false for normal text lines", () => {
    expect(isAttachmentContextLine("Hello world")).toBe(false);
    expect(isAttachmentContextLine("")).toBe(false);
  });

  it("returns false for JSON without type=attachment", () => {
    const line = JSON.stringify({ type: "text", content: "hello" });
    expect(isAttachmentContextLine(line)).toBe(false);
  });

  it("returns false for malformed JSON that contains attachment keyword", () => {
    expect(isAttachmentContextLine('{"attachment": true')).toBe(false);
  });
});

describe("isMediaDirectiveLine", () => {
  it("detects [media attached:...] pattern", () => {
    expect(isMediaDirectiveLine("[media attached: image.jpg]")).toBe(true);
  });

  it("detects MEDIA: directive pattern", () => {
    expect(isMediaDirectiveLine("MEDIA: \"video.mp4\"")).toBe(true);
  });

  it("returns false for normal text", () => {
    expect(isMediaDirectiveLine("This is a normal message")).toBe(false);
  });
});

describe("renderTextForMessage", () => {
  it("trims whitespace", () => {
    expect(renderTextForMessage("  hello  ")).toBe("hello");
  });

  it("filters out attachment context lines", () => {
    const attachmentLine = JSON.stringify({ type: "attachment", url: "x.jpg" });
    const input = `Hello\n${attachmentLine}\nWorld`;
    expect(renderTextForMessage(input)).toBe("Hello\nWorld");
  });
});

describe("displayTextForMessage", () => {
  it("filters out both attachment and media directive lines", () => {
    const attachment = JSON.stringify({ type: "attachment", url: "x.jpg" });
    const input = `Hello\n${attachment}\n[media attached: img]\nWorld`;
    expect(displayTextForMessage(input)).toBe("Hello\nWorld");
  });
});

describe("sanitizeSpeechText", () => {
  it("removes URLs", () => {
    const result = sanitizeSpeechText("Check https://example.com for details");
    expect(result).not.toContain("https://");
  });

  it("strips markdown formatting characters", () => {
    const result = sanitizeSpeechText("**bold** and _italic_");
    expect(result).not.toContain("**");
    expect(result).not.toContain("_");
  });

  it("clips text at the limit and prefers sentence boundaries", () => {
    const long = "这是第一句。这是第二句。".repeat(50);
    const result = sanitizeSpeechText(long, 50);
    expect(result.length).toBeLessThanOrEqual(50);
  });

  it("respects the default limit of 420", () => {
    const long = "x".repeat(500);
    expect(sanitizeSpeechText(long).length).toBeLessThanOrEqual(420);
  });
});
