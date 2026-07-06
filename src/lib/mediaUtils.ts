export type MediaSegment =
  | { kind: "text"; value: string }
  | { kind: "image"; path: string; mimeType: string }
  | { kind: "file"; path: string; mimeType: string };

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
