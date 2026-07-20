import type { components } from "./generated/openapi";
import {
  desktopTransport,
  type DesktopRequestOptions,
  type DesktopTransport,
} from "./desktopConnection";

export type Memory = components["schemas"]["Memory"];
export type MemoryCapabilities = components["schemas"]["MemoryCapabilities"];
export type MemoryPage = components["schemas"]["MemoryPage"];
export type CreateMemoryInput = components["schemas"]["CreateMemory"];
export type MemoryPatch = components["schemas"]["MemoryPatch"];
export type MemoryTarget = Memory["target"];

export interface MemoryListRequest {
  target: MemoryTarget;
  query?: string;
  cursor?: string;
  limit?: number;
}

export interface VersionedMemoryPage {
  value: MemoryPage;
  etag: string;
}

export interface VersionedMemory {
  value: Memory;
  etag: string;
}

export interface DeletedMemory {
  etag: string;
}

export type MemoryApiErrorKind = "http" | "invalid_request" | "invalid_response";

export class MemoryApiError extends Error {
  readonly kind: MemoryApiErrorKind;
  readonly status?: number;
  readonly code?: string;
  readonly requestId?: string;
  readonly retryable: boolean;
  readonly etag?: string;

  constructor(
    kind: MemoryApiErrorKind,
    message: string,
    options: {
      status?: number;
      code?: string;
      requestId?: string;
      retryable?: boolean;
      etag?: string;
    } = {},
  ) {
    super(message);
    this.name = "MemoryApiError";
    this.kind = kind;
    this.status = options.status;
    this.code = options.code;
    this.requestId = options.requestId;
    this.retryable = options.retryable ?? false;
    this.etag = options.etag;
  }
}

export interface MemoriesApi {
  listMemories(
    profileId: string,
    request: MemoryListRequest,
    options?: DesktopRequestOptions,
  ): Promise<VersionedMemoryPage>;
  createMemory(
    profileId: string,
    input: CreateMemoryInput,
    etag: string,
    idempotencyKey: string,
    options?: DesktopRequestOptions,
  ): Promise<VersionedMemory>;
  updateMemory(
    profileId: string,
    memoryId: string,
    patch: MemoryPatch,
    etag: string,
    options?: DesktopRequestOptions,
  ): Promise<VersionedMemory>;
  deleteMemory(
    profileId: string,
    memoryId: string,
    etag: string,
    options?: DesktopRequestOptions,
  ): Promise<DeletedMemory>;
}

interface ProblemDetails {
  type: string;
  title: string;
  status: number;
  code: string;
  requestId: string;
  retryable: boolean;
  detail?: string | null;
  instance?: string | null;
}

const PROFILE_ID_PATTERN = /^(?:default|[a-z0-9_][a-z0-9_-]{0,63})$/u;
const STRONG_ETAG_PATTERN = /^"[\x21\x23-\x7e]{1,126}"$/u;
const REVISION_PATTERN = /^[\x21\x23-\x7e]{1,126}$/u;
const MEMORY_TARGETS = new Set<MemoryTarget>(["memory", "user"]);
const CONTENT_LIMIT = 2_200;

function invalidResponse(context: string): never {
  throw new MemoryApiError(
    "invalid_response",
    `${context} did not match the API v1 contract.`,
  );
}

function invalidRequest(message: string): never {
  throw new MemoryApiError("invalid_request", message);
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

function booleanValue(value: unknown, context: string): boolean {
  if (typeof value !== "boolean") return invalidResponse(context);
  return value;
}

function integerValue(value: unknown, context: string): number {
  if (!Number.isSafeInteger(value) || (value as number) < 0) {
    return invalidResponse(context);
  }
  return value as number;
}

function memoryTarget(value: unknown, context: string): MemoryTarget {
  if (typeof value !== "string" || !MEMORY_TARGETS.has(value as MemoryTarget)) {
    return invalidResponse(context);
  }
  return value as MemoryTarget;
}

export function parseMemory(value: unknown): Memory {
  const record = asRecord(value, "Memory");
  exactKeys(record, ["id", "target", "content", "provider"], [], "Memory");
  if (record.provider !== "builtin") return invalidResponse("Memory.provider");
  const content = stringValue(record.content, "Memory.content");
  if (!content || Array.from(content).length > CONTENT_LIMIT) {
    return invalidResponse("Memory.content");
  }
  const id = nonEmptyString(record.id, "Memory.id");
  if (Array.from(id).length > 512) return invalidResponse("Memory.id");
  return {
    id,
    target: memoryTarget(record.target, "Memory.target"),
    content,
    provider: "builtin",
  };
}

function parseMemoryCapabilities(value: unknown): MemoryCapabilities {
  const record = asRecord(value, "Memory capabilities");
  exactKeys(
    record,
    ["create", "update", "delete", "search"],
    [],
    "Memory capabilities",
  );
  return {
    create: booleanValue(record.create, "Memory capabilities.create"),
    update: booleanValue(record.update, "Memory capabilities.update"),
    delete: booleanValue(record.delete, "Memory capabilities.delete"),
    search: booleanValue(record.search, "Memory capabilities.search"),
  };
}

export function parseMemoryPage(value: unknown): MemoryPage {
  const record = asRecord(value, "Memory page");
  exactKeys(
    record,
    [
      "items",
      "nextCursor",
      "revision",
      "provider",
      "charsUsed",
      "charLimit",
      "promptSafety",
      "capabilities",
    ],
    [],
    "Memory page",
  );
  if (!Array.isArray(record.items)) return invalidResponse("Memory page.items");
  const items = record.items.map(parseMemory);
  if (new Set(items.map((item) => item.id)).size !== items.length) {
    return invalidResponse("Memory page IDs");
  }
  if (
    record.provider !== "builtin"
    || (record.nextCursor !== null && (
      typeof record.nextCursor !== "string" || !record.nextCursor
    ))
    || (record.promptSafety !== "clean" && record.promptSafety !== "blocked")
  ) {
    return invalidResponse("Memory page");
  }
  const revision = stringValue(record.revision, "Memory page.revision");
  if (!REVISION_PATTERN.test(revision)) return invalidResponse("Memory page.revision");
  const charsUsed = integerValue(record.charsUsed, "Memory page.charsUsed");
  const charLimit = integerValue(record.charLimit, "Memory page.charLimit");
  if (charLimit === 0) return invalidResponse("Memory page.charLimit");
  return {
    items,
    nextCursor: record.nextCursor as string | null,
    revision,
    provider: "builtin",
    charsUsed,
    charLimit,
    promptSafety: record.promptSafety,
    capabilities: parseMemoryCapabilities(record.capabilities),
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
    !Number.isInteger(record.status)
    || (record.status as number) < 400
    || (record.status as number) > 599
    || ("detail" in record && record.detail !== null && typeof record.detail !== "string")
    || ("instance" in record && record.instance !== null && typeof record.instance !== "string")
  ) {
    return invalidResponse("Problem details");
  }
  const result: ProblemDetails = {
    type: stringValue(record.type, "Problem details.type"),
    title: stringValue(record.title, "Problem details.title"),
    status: record.status as number,
    code: stringValue(record.code, "Problem details.code"),
    requestId: stringValue(record.requestId, "Problem details.requestId"),
    retryable: booleanValue(record.retryable, "Problem details.retryable"),
  };
  if ("detail" in record) result.detail = record.detail as string | null;
  if ("instance" in record) result.instance = record.instance as string | null;
  return result;
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

function optionalStrongEtag(response: Response): string | undefined {
  const etag = response.headers.get("etag") ?? undefined;
  return etag && STRONG_ETAG_PATTERN.test(etag) ? etag : undefined;
}

function requiredStrongEtag(response: Response, context: string): string {
  const etag = optionalStrongEtag(response);
  if (!etag) return invalidResponse(`${context} ETag`);
  return etag;
}

function checkedStrongEtag(etag: string): string {
  if (typeof etag !== "string" || !STRONG_ETAG_PATTERN.test(etag)) {
    return invalidRequest("A single strong Memory ETag is required.");
  }
  return etag;
}

function checkedProfileId(profileId: string): string {
  if (typeof profileId !== "string" || !PROFILE_ID_PATTERN.test(profileId)) {
    return invalidRequest("Profile ID is invalid.");
  }
  return encodeURIComponent(profileId);
}

function checkedMemoryId(memoryId: string): string {
  if (
    typeof memoryId !== "string"
    || !memoryId
    || Array.from(memoryId).length > 512
    || /[\u0000-\u001f\u007f]/u.test(memoryId)
  ) {
    return invalidRequest("Memory ID is invalid.");
  }
  return encodeURIComponent(memoryId);
}

function checkedTarget(target: unknown): MemoryTarget {
  if (typeof target !== "string" || !MEMORY_TARGETS.has(target as MemoryTarget)) {
    return invalidRequest("Memory target is required.");
  }
  return target as MemoryTarget;
}

function checkedContent(content: unknown): string {
  if (
    typeof content !== "string"
    || !content
    || Array.from(content).length > CONTENT_LIMIT
  ) {
    return invalidRequest(`Memory content must contain 1 to ${CONTENT_LIMIT} characters.`);
  }
  return content;
}

function checkedIdempotencyKey(value: string): string {
  if (typeof value !== "string" || !/^[\x21-\x7e]{8,128}$/u.test(value)) {
    return invalidRequest("Idempotency-Key must contain 8 to 128 characters.");
  }
  return value;
}

function checkedListRequest(request: MemoryListRequest): URLSearchParams {
  if (request === null || typeof request !== "object" || Array.isArray(request)) {
    return invalidRequest("Memory list request is invalid.");
  }
  const record = request as unknown as Record<string, unknown>;
  if (Object.keys(record).some((key) => !["target", "query", "cursor", "limit"].includes(key))) {
    return invalidRequest("Memory list request is invalid.");
  }
  const query = new URLSearchParams({ target: checkedTarget(request.target) });
  if (request.query !== undefined) {
    if (
      typeof request.query !== "string"
      || Array.from(request.query).length > 500
      || /[\u0000-\u001f\u007f]/u.test(request.query)
    ) {
      return invalidRequest("Memory query is invalid.");
    }
    query.set("q", request.query);
  }
  if (request.cursor !== undefined) {
    if (
      typeof request.cursor !== "string"
      || !request.cursor
      || request.cursor.length > 4_096
      || /[\u0000-\u001f\u007f]/u.test(request.cursor)
    ) {
      return invalidRequest("Memory cursor is invalid.");
    }
    query.set("cursor", request.cursor);
  }
  if (request.limit !== undefined) {
    if (!Number.isInteger(request.limit) || request.limit < 1 || request.limit > 100) {
      return invalidRequest("Memory page limit is invalid.");
    }
    query.set("limit", String(request.limit));
  }
  return query;
}

async function throwHttpError(response: Response): Promise<never> {
  const problem = parseProblemDetails(await jsonPayload(response, "Memory error response"));
  if (problem.status !== response.status) invalidResponse("Problem details.status");
  const etag = optionalStrongEtag(response);
  if ((response.status === 409 || response.status === 412) && !etag) {
    invalidResponse("Memory conflict ETag");
  }
  throw new MemoryApiError("http", problem.title, {
    status: problem.status,
    code: problem.code,
    requestId: problem.requestId,
    retryable: problem.retryable,
    etag,
  });
}

class DefaultMemoriesApi implements MemoriesApi {
  constructor(private readonly transport: DesktopTransport) {}

  async listMemories(
    profileId: string,
    request: MemoryListRequest,
    options: DesktopRequestOptions = {},
  ): Promise<VersionedMemoryPage> {
    const query = checkedListRequest(request);
    const response = await this.transport.request(
      `/api/v1/profiles/${checkedProfileId(profileId)}/memories?${query}`,
      { method: "GET", headers: { Accept: "application/json" } },
      options,
    );
    if (response.status !== 200) return throwHttpError(response);
    const value = parseMemoryPage(await jsonPayload(response, "Memory page"));
    if (value.items.some((item) => item.target !== request.target)) {
      return invalidResponse("Memory page target");
    }
    const etag = requiredStrongEtag(response, "Memory page");
    if (etag !== `"${value.revision}"`) return invalidResponse("Memory page ETag");
    return { value, etag };
  }

  async createMemory(
    profileId: string,
    input: CreateMemoryInput,
    etag: string,
    idempotencyKey: string,
    options: DesktopRequestOptions = {},
  ): Promise<VersionedMemory> {
    if (input === null || typeof input !== "object" || Array.isArray(input)) {
      return invalidRequest("Memory creation input is invalid.");
    }
    const record = input as unknown as Record<string, unknown>;
    if (Object.keys(record).some((key) => !["target", "content"].includes(key))) {
      return invalidRequest("Memory creation input is invalid.");
    }
    const body: CreateMemoryInput = {
      target: checkedTarget(input.target),
      content: checkedContent(input.content),
    };
    const response = await this.transport.request(
      `/api/v1/profiles/${checkedProfileId(profileId)}/memories`,
      {
        method: "POST",
        headers: {
          Accept: "application/json",
          "Content-Type": "application/json",
          "Idempotency-Key": checkedIdempotencyKey(idempotencyKey),
          "If-Match": checkedStrongEtag(etag),
        },
        body: JSON.stringify(body),
      },
      options,
    );
    if (response.status !== 201) return throwHttpError(response);
    const value = parseMemory(await jsonPayload(response, "Created Memory"));
    if (value.target !== body.target) return invalidResponse("Created Memory.target");
    return { value, etag: requiredStrongEtag(response, "Created Memory") };
  }

  async updateMemory(
    profileId: string,
    memoryId: string,
    patch: MemoryPatch,
    etag: string,
    options: DesktopRequestOptions = {},
  ): Promise<VersionedMemory> {
    if (patch === null || typeof patch !== "object" || Array.isArray(patch)) {
      return invalidRequest("Memory patch is invalid.");
    }
    const record = patch as unknown as Record<string, unknown>;
    if (Object.keys(record).length !== 1 || !("content" in record)) {
      return invalidRequest("Memory patch must contain only content.");
    }
    const body: MemoryPatch = { content: checkedContent(patch.content) };
    const response = await this.transport.request(
      `/api/v1/profiles/${checkedProfileId(profileId)}/memories/${checkedMemoryId(memoryId)}`,
      {
        method: "PATCH",
        headers: {
          Accept: "application/json",
          "Content-Type": "application/merge-patch+json",
          "If-Match": checkedStrongEtag(etag),
        },
        body: JSON.stringify(body),
      },
      options,
    );
    if (response.status !== 200) return throwHttpError(response);
    const value = parseMemory(await jsonPayload(response, "Updated Memory"));
    // Memory IDs are revision-scoped, so PATCH returns the replacement ID.
    return { value, etag: requiredStrongEtag(response, "Updated Memory") };
  }

  async deleteMemory(
    profileId: string,
    memoryId: string,
    etag: string,
    options: DesktopRequestOptions = {},
  ): Promise<DeletedMemory> {
    const response = await this.transport.request(
      `/api/v1/profiles/${checkedProfileId(profileId)}/memories/${checkedMemoryId(memoryId)}`,
      {
        method: "DELETE",
        headers: {
          Accept: "application/json",
          "If-Match": checkedStrongEtag(etag),
        },
      },
      options,
    );
    if (response.status !== 204) return throwHttpError(response);
    return { etag: requiredStrongEtag(response, "Deleted Memory") };
  }
}

export function createMemoriesApi(transport: DesktopTransport = desktopTransport): MemoriesApi {
  return new DefaultMemoriesApi(transport);
}

export const memoriesApi = createMemoriesApi();
