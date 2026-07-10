import type { ChatMessage } from "./types";

export type MediaSegment =
  | { kind: "text"; value: string }
  | { kind: "image"; path: string; mimeType: string }
  | { kind: "file"; path: string; mimeType: string };

type MediaFileSegment = Exclude<MediaSegment, { kind: "text" }>;

export const MEDIA_MARKER = /\[media attached:\s*(?:"([^"]+)"|`([^`]+)`|([^\]\(]+?))\s*(?:\(([^)]+)\))?\]/gi;
export const MEDIA_TAG_MARKER = /`?MEDIA:\s*(?:"([^"\n]+)"|'([^'\n]+)'|`([^`\n]+)`|([A-Za-z]:[\\/][^\n]+|\/[^\n]+|~\/[^\n]+))`?/gi;

export function isImagePath(path: string): boolean {
  return /\.(png|jpe?g|webp|gif|bmp|svg)$/i.test(path);
}

export function imageMimeType(path: string): string {
  if (/\.gif$/i.test(path)) return "image/gif";
  if (/\.webp$/i.test(path)) return "image/webp";
  if (/\.jpe?g$/i.test(path)) return "image/jpeg";
  if (/\.bmp$/i.test(path)) return "image/bmp";
  if (/\.svg$/i.test(path)) return "image/svg+xml";
  return "image/png";
}

export function parseMediaTagSegments(text: string): MediaSegment[] {
  const segments: MediaSegment[] = [];
  let lastIndex = 0;
  let match: RegExpExecArray | null;
  MEDIA_TAG_MARKER.lastIndex = 0;
  while ((match = MEDIA_TAG_MARKER.exec(text)) !== null) {
    if (match.index > lastIndex) {
      segments.push({ kind: "text", value: text.slice(lastIndex, match.index) });
    }
    const path = (match[1] || match[2] || match[3] || match[4] || "").trim();
    const mimeType = isImagePath(path) ? imageMimeType(path) : "application/octet-stream";
    if (path) segments.push({ kind: isImagePath(path) ? "image" : "file", path, mimeType });
    lastIndex = match.index + match[0].length;
  }
  if (lastIndex < text.length) segments.push({ kind: "text", value: text.slice(lastIndex) });
  return segments;
}

export function parseMediaSegments(text: string): MediaSegment[] {
  const segments: MediaSegment[] = [];
  let lastIndex = 0;
  let match: RegExpExecArray | null;
  MEDIA_MARKER.lastIndex = 0;
  while ((match = MEDIA_MARKER.exec(text)) !== null) {
    if (match.index > lastIndex) {
      segments.push({ kind: "text", value: text.slice(lastIndex, match.index) });
    }
    const path = (match[1] || match[2] || match[3] || "").trim();
    const mimeType = (match[4] || (isImagePath(path) ? imageMimeType(path) : "application/octet-stream")).trim();
    if (path) segments.push({ kind: isImagePath(path) || mimeType.startsWith("image/") ? "image" : "file", path, mimeType });
    lastIndex = match.index + match[0].length;
  }
  if (lastIndex < text.length) segments.push({ kind: "text", value: text.slice(lastIndex) });
  return segments.flatMap((segment) => segment.kind === "text" ? parseMediaTagSegments(segment.value) : [segment]);
}

function recordValue(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function stringValue(record: Record<string, unknown>, keys: string[]) {
  for (const key of keys) {
    const value = record[key];
    if (typeof value === "string" && value.trim()) return value.trim();
  }
  return "";
}

function structuredMediaSegment(value: unknown): MediaFileSegment | null {
  const record = recordValue(value);
  if (!record) return null;
  const path = stringValue(record, [
    "path",
    "mediaPath",
    "media_path",
    "visiblePath",
    "visible_path",
    "localPath",
    "local_path",
    "url"
  ]);
  if (!path) return null;
  const mimeType = stringValue(record, ["mimeType", "mime_type", "contentType", "content_type"])
    || (isImagePath(path) ? imageMimeType(path) : "application/octet-stream");
  return {
    kind: isImagePath(path) || mimeType.startsWith("image/") ? "image" : "file",
    path,
    mimeType
  };
}

export function structuredMessageMedia(message: ChatMessage): MediaSegment[] {
  const root = recordValue(message.providerData);
  if (!root) return [];
  const segments: MediaSegment[] = [];
  const seen = new Set<string>();
  const push = (value: unknown) => {
    const segment = structuredMediaSegment(value);
    if (!segment) return;
    const key = segment.path.replace(/\\/g, "/").toLowerCase();
    if (seen.has(key)) return;
    seen.add(key);
    segments.push(segment);
  };
  if (
    root.type === "attachment"
    || typeof root.fileName === "string"
    || typeof root.file_name === "string"
  ) {
    push(root);
  }
  for (const key of [
    "attachments",
    "attachmentContexts",
    "attachment_contexts",
    "mediaFiles",
    "media_files"
  ]) {
    const values = root[key];
    if (!Array.isArray(values)) continue;
    for (const value of values) push(value);
  }
  return segments;
}

export function mergeMessageMediaSegments(
  contentSegments: MediaSegment[],
  structuredSegments: MediaSegment[]
) {
  const seen = new Set(
    contentSegments
      .filter((segment): segment is Exclude<MediaSegment, { kind: "text" }> => segment.kind !== "text")
      .map((segment) => segment.path.replace(/\\/g, "/").toLowerCase())
  );
  const supplements = structuredSegments.filter((segment) => {
    if (segment.kind === "text") return false;
    const key = segment.path.replace(/\\/g, "/").toLowerCase();
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
  return [...contentSegments, ...supplements];
}
