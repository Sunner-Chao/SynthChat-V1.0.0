import { describe, it, expect } from "vitest";
import {
  imageMimeType,
  isImagePath,
  mergeMessageMediaSegments,
  parseMediaSegments,
  parseMediaTagSegments,
  structuredMessageMedia
} from "../mediaUtils";

describe("isImagePath", () => {
  it("returns true for image extensions", () => {
    expect(isImagePath("photo.png")).toBe(true);
    expect(isImagePath("photo.jpg")).toBe(true);
    expect(isImagePath("photo.jpeg")).toBe(true);
    expect(isImagePath("photo.webp")).toBe(true);
    expect(isImagePath("photo.gif")).toBe(true);
    expect(isImagePath("icon.svg")).toBe(true);
  });

  it("returns false for non-image extensions", () => {
    expect(isImagePath("document.pdf")).toBe(false);
    expect(isImagePath("video.mp4")).toBe(false);
    expect(isImagePath("data.json")).toBe(false);
    expect(isImagePath("archive.zip")).toBe(false);
  });

  it("is case-insensitive", () => {
    expect(isImagePath("photo.PNG")).toBe(true);
    expect(isImagePath("photo.JPG")).toBe(true);
  });
});

describe("imageMimeType", () => {
  it("returns correct MIME type for each extension", () => {
    expect(imageMimeType("photo.gif")).toBe("image/gif");
    expect(imageMimeType("photo.webp")).toBe("image/webp");
    expect(imageMimeType("photo.jpg")).toBe("image/jpeg");
    expect(imageMimeType("photo.jpeg")).toBe("image/jpeg");
    expect(imageMimeType("photo.bmp")).toBe("image/bmp");
    expect(imageMimeType("icon.svg")).toBe("image/svg+xml");
    expect(imageMimeType("photo.png")).toBe("image/png");
  });

  it("defaults to image/png for unknown extensions", () => {
    expect(imageMimeType("photo.unknown")).toBe("image/png");
  });
});

describe("parseMediaTagSegments", () => {
  it("returns single text segment for plain text", () => {
    const segments = parseMediaTagSegments("Hello world");
    expect(segments).toHaveLength(1);
    expect(segments[0]).toEqual({ kind: "text", value: "Hello world" });
  });

  it("parses a MEDIA: tag for an image", () => {
    const text = 'MEDIA: "path/to/image.png"';
    const segments = parseMediaTagSegments(text);
    expect(segments).toHaveLength(1);
    expect(segments[0].kind).toBe("image");
    if (segments[0].kind === "image") {
      expect(segments[0].path).toBe("path/to/image.png");
      expect(segments[0].mimeType).toBe("image/png");
    }
  });

  it("parses a MEDIA: tag for a non-image file", () => {
    const text = 'MEDIA: "path/to/file.pdf"';
    const segments = parseMediaTagSegments(text);
    expect(segments[0].kind).toBe("file");
  });

  it("splits text around MEDIA: tags", () => {
    const text = 'Before\nMEDIA: "photo.jpg"\nAfter';
    const segments = parseMediaTagSegments(text);
    const kinds = segments.map((s) => s.kind);
    expect(kinds).toContain("text");
    expect(kinds).toContain("image");
  });
});

describe("parseMediaSegments", () => {
  it("returns single text segment for plain text without markers", () => {
    const segments = parseMediaSegments("Just normal text");
    expect(segments).toHaveLength(1);
    expect(segments[0].kind).toBe("text");
  });

  it("parses [media attached: ...] marker for image", () => {
    const text = '[media attached: "photo.png"]';
    const segments = parseMediaSegments(text);
    expect(segments).toHaveLength(1);
    expect(segments[0].kind).toBe("image");
    if (segments[0].kind === "image") {
      expect(segments[0].path).toBe("photo.png");
    }
  });

  it("splits text around media markers", () => {
    const text = 'Caption: [media attached: "photo.jpg"] end';
    const segments = parseMediaSegments(text);
    const kinds = segments.map((s) => s.kind);
    expect(kinds).toContain("image");
  });
});

describe("structuredMessageMedia", () => {
  it("reads persisted attachment metadata even when message content has no media marker", () => {
    const segments = structuredMessageMedia({
      id: "message-1",
      conversationId: "conversation-1",
      role: "user",
      content: "请看这张图",
      createdAt: "2026-07-10T00:00:00.000Z",
      providerData: {
        attachments: [{
          fileName: "photo.png",
          mimeType: "image/png",
          path: "D:\\data\\photo.png"
        }]
      }
    });

    expect(segments).toEqual([{
      kind: "image",
      path: "D:\\data\\photo.png",
      mimeType: "image/png"
    }]);
  });

  it("does not duplicate structured attachments already represented by content markers", () => {
    const content = parseMediaSegments('[media attached: "D:\\data\\photo.png" (image/png)]');
    const structured = structuredMessageMedia({
      id: "message-1",
      conversationId: "conversation-1",
      role: "user",
      content: "",
      createdAt: "2026-07-10T00:00:00.000Z",
      providerData: {
        attachments: [{
          mime_type: "image/png",
          path: "d:/data/photo.png"
        }]
      }
    });

    expect(mergeMessageMediaSegments(content, structured)).toHaveLength(1);
  });
});
