import type { components } from "./generated/openapi";

export const MAX_FILE_BYTES = 8 * 1024 * 1024;

export type FileMimeType = components["schemas"]["FileRef"]["mimeType"];

export const ALLOWED_FILE_MIME_TYPES = [
  "application/json",
  "application/octet-stream",
  "application/pdf",
  "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
  "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
  "application/x-zip-compressed",
  "application/yaml",
  "application/zip",
  "image/gif",
  "image/jpeg",
  "image/png",
  "image/webp",
  "text/csv",
  "text/markdown",
  "text/plain",
  "text/tab-separated-values",
  "text/yaml",
] as const satisfies readonly FileMimeType[];

type MissingFileMimeType = Exclude<
  FileMimeType,
  (typeof ALLOWED_FILE_MIME_TYPES)[number]
>;
const FILE_MIME_TYPES_ARE_EXHAUSTIVE: MissingFileMimeType extends never
  ? true
  : never = true;
void FILE_MIME_TYPES_ARE_EXHAUSTIVE;

const FILE_MIME_TYPE_SET: ReadonlySet<string> = new Set(ALLOWED_FILE_MIME_TYPES);

export function isFileMimeType(value: unknown): value is FileMimeType {
  return typeof value === "string" && FILE_MIME_TYPE_SET.has(value);
}
