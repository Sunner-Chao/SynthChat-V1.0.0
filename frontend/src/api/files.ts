import type { components } from "./generated/openapi";
import {
  desktopTransport,
  type DesktopRequestOptions,
  type DesktopTransport,
} from "./desktopConnection";
import { isFileMimeType, MAX_FILE_BYTES } from "./fileContract";

export type FileRef = components["schemas"]["FileRef"];

export type FileApiErrorKind = "http" | "invalid_request" | "invalid_response";

export class FileApiError extends Error {
  readonly kind: FileApiErrorKind;
  readonly status?: number;
  readonly code?: string;
  readonly requestId?: string;
  readonly retryable: boolean;

  constructor(
    kind: FileApiErrorKind,
    message: string,
    options: {
      status?: number;
      code?: string;
      requestId?: string;
      retryable?: boolean;
    } = {},
  ) {
    super(message);
    this.name = "FileApiError";
    this.kind = kind;
    this.status = options.status;
    this.code = options.code;
    this.requestId = options.requestId;
    this.retryable = options.retryable ?? false;
  }
}

export interface FilesApi {
  uploadFile(
    file: File,
    idempotencyKey: string,
    options?: DesktopRequestOptions,
  ): Promise<FileRef>;
  deleteFile(
    fileId: string,
    options?: DesktopRequestOptions,
  ): Promise<void>;
}

interface ProblemDetails {
  title: string;
  status: number;
  code: string;
  requestId: string;
  retryable: boolean;
}

const IDEMPOTENCY_KEY_PATTERN = /^[\x21-\x7e]{8,128}$/u;
const FILE_ID_PATTERN = /^file_[0-9a-f]{32}$/u;
const RFC3339_PATTERN = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/u;

function invalidResponse(context: string): never {
  throw new FileApiError(
    "invalid_response",
    `${context} did not match the API v1 contract.`,
  );
}

function invalidRequest(message: string): never {
  throw new FileApiError("invalid_request", message);
}

function asRecord(value: unknown, context: string): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    return invalidResponse(context);
  }
  return value as Record<string, unknown>;
}

function exactKeys(
  record: Record<string, unknown>,
  required: readonly string[],
  optional: readonly string[],
  context: string,
): void {
  const allowed = new Set([...required, ...optional]);
  if (
    required.some((key) => !(key in record))
    || Object.keys(record).some((key) => !allowed.has(key))
  ) {
    invalidResponse(context);
  }
}

function stringValue(value: unknown, context: string): string {
  if (typeof value !== "string") return invalidResponse(context);
  return value;
}

function nonEmptyString(value: unknown, context: string): string {
  const result = stringValue(value, context);
  if (!result) return invalidResponse(context);
  return result;
}

function dateTime(value: unknown, context: string): string {
  const result = stringValue(value, context);
  if (!RFC3339_PATTERN.test(result) || Number.isNaN(Date.parse(result))) {
    return invalidResponse(context);
  }
  return result;
}

export function parseFileRef(value: unknown): FileRef {
  const record = asRecord(value, "FileRef");
  exactKeys(record, ["id", "name", "mimeType", "sizeBytes", "createdAt"], [], "FileRef");
  if (
    !Number.isSafeInteger(record.sizeBytes)
    || (record.sizeBytes as number) < 0
    || (record.sizeBytes as number) > MAX_FILE_BYTES
  ) {
    return invalidResponse("FileRef.sizeBytes");
  }
  if (!isFileMimeType(record.mimeType)) {
    return invalidResponse("FileRef.mimeType");
  }
  const id = nonEmptyString(record.id, "FileRef.id");
  if (!FILE_ID_PATTERN.test(id)) return invalidResponse("FileRef.id");
  return {
    id,
    name: stringValue(record.name, "FileRef.name"),
    mimeType: record.mimeType,
    sizeBytes: record.sizeBytes as number,
    createdAt: dateTime(record.createdAt, "FileRef.createdAt"),
  };
}

function parseProblemDetails(value: unknown): ProblemDetails {
  const record = asRecord(value, "Problem details");
  exactKeys(
    record,
    ["type", "title", "status", "code", "requestId", "retryable"],
    ["detail", "instance"],
    "Problem details",
  );
  if (
    typeof record.type !== "string"
    || typeof record.title !== "string"
    || !Number.isInteger(record.status)
    || (record.status as number) < 400
    || (record.status as number) > 599
    || typeof record.code !== "string"
    || typeof record.requestId !== "string"
    || typeof record.retryable !== "boolean"
    || ("detail" in record && record.detail !== null && typeof record.detail !== "string")
    || ("instance" in record && record.instance !== null && typeof record.instance !== "string")
  ) {
    return invalidResponse("Problem details");
  }
  return {
    title: record.title,
    status: record.status as number,
    code: record.code,
    requestId: record.requestId,
    retryable: record.retryable,
  };
}

async function jsonPayload(response: Response, context: string): Promise<unknown> {
  const contentType = response.headers.get("content-type")?.toLowerCase() ?? "";
  if (!contentType.includes("application/json") && !contentType.includes("application/problem+json")) {
    return invalidResponse(context);
  }
  try {
    return await response.json() as unknown;
  } catch {
    return invalidResponse(context);
  }
}

function checkedIdempotencyKey(value: string): string {
  if (typeof value !== "string" || !IDEMPOTENCY_KEY_PATTERN.test(value)) {
    return invalidRequest("Idempotency-Key must contain 8 to 128 visible ASCII characters.");
  }
  return value;
}

function checkedFileId(value: string): string {
  if (typeof value !== "string" || !FILE_ID_PATTERN.test(value)) {
    return invalidRequest("File ID is invalid.");
  }
  return encodeURIComponent(value);
}

async function throwHttpError(response: Response): Promise<never> {
  const problem = parseProblemDetails(await jsonPayload(response, "File error response"));
  if (problem.status !== response.status) invalidResponse("Problem details.status");
  throw new FileApiError("http", problem.title, {
    status: problem.status,
    code: problem.code,
    requestId: problem.requestId,
    retryable: problem.retryable,
  });
}

class DefaultFilesApi implements FilesApi {
  constructor(private readonly transport: DesktopTransport) {}

  async uploadFile(
    file: File,
    idempotencyKey: string,
    options: DesktopRequestOptions = {},
  ): Promise<FileRef> {
    if (!(file instanceof File) || !file.name || /[\u0000-\u001f\u007f]/u.test(file.name)) {
      return invalidRequest("A named file is required.");
    }
    const form = new FormData();
    form.append("file", file, file.name);
    const response = await this.transport.request(
      "/api/v1/files",
      {
        method: "POST",
        headers: {
          Accept: "application/json",
          "Idempotency-Key": checkedIdempotencyKey(idempotencyKey),
        },
        body: form,
      },
      options,
    );
    if (response.status !== 201) return throwHttpError(response);
    return parseFileRef(await jsonPayload(response, "Uploaded FileRef"));
  }

  async deleteFile(
    fileId: string,
    options: DesktopRequestOptions = {},
  ): Promise<void> {
    const response = await this.transport.request(
      `/api/v1/files/${checkedFileId(fileId)}`,
      { method: "DELETE", headers: { Accept: "application/json" } },
      options,
    );
    if (response.status === 204) return;
    if (response.ok) return invalidResponse("Deleted file response");
    return throwHttpError(response);
  }
}

export function createFilesApi(transport: DesktopTransport = desktopTransport): FilesApi {
  return new DefaultFilesApi(transport);
}

export const filesApi = createFilesApi();
