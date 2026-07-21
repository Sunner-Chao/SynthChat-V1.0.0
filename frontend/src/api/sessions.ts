import type { components } from "./generated/openapi";
import { isFileMimeType, MAX_FILE_BYTES } from "./fileContract";
import {
  desktopTransport,
  type DesktopRequestOptions,
  type DesktopTransport,
} from "./desktopConnection";

export type Session = components["schemas"]["Session"] & { personaId: string | null };
export type SearchMatch = components["schemas"]["SearchMatch"];
export type CreateSessionInput = components["schemas"]["CreateSession"] & {
  personaId?: string | null;
};
export type SessionPatch = components["schemas"]["SessionPatch"];
export type SessionPage = Omit<components["schemas"]["SessionPage"], "items"> & {
  items: Session[];
};
export type Message = components["schemas"]["Message"];
export type MessagePage = components["schemas"]["MessagePage"];
export type TextPart = components["schemas"]["TextPart"];
export type FilePart = components["schemas"]["FilePart"];
export type ToolCall = components["schemas"]["ToolCall"];
export type FileRef = components["schemas"]["FileRef"];
export type Usage = components["schemas"]["Usage"];
export type ProblemDetails = components["schemas"]["Problem"];

export interface VersionedSession {
  value: Session;
  etag: string;
}

export interface ListSessionsQuery {
  profileId: string;
  q?: string;
  archived?: boolean;
  cursor?: string;
  limit?: number;
}

export interface SearchSessionsQuery {
  profileId: string;
  query: string;
  archived?: boolean;
  cursor?: string;
  limit?: number;
}

export interface ListMessagesQuery {
  cursor?: string;
  limit?: number;
}

export type SessionApiErrorKind = "http" | "invalid_request" | "invalid_response";

export class SessionApiError extends Error {
  readonly kind: SessionApiErrorKind;
  readonly status?: number;
  readonly code?: string;
  readonly requestId?: string;
  readonly retryable: boolean;
  readonly etag?: string;

  constructor(
    kind: SessionApiErrorKind,
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
    this.name = "SessionApiError";
    this.kind = kind;
    this.status = options.status;
    this.code = options.code;
    this.requestId = options.requestId;
    this.retryable = options.retryable ?? false;
    this.etag = options.etag;
  }
}

export interface SessionsApi {
  listSessions(
    query: ListSessionsQuery,
    options?: DesktopRequestOptions,
  ): Promise<SessionPage>;
  searchSessions(
    query: SearchSessionsQuery,
    options?: DesktopRequestOptions,
  ): Promise<SessionPage>;
  createSession(
    input: CreateSessionInput,
    idempotencyKey: string,
    options?: DesktopRequestOptions,
  ): Promise<VersionedSession>;
  getSession(
    sessionId: string,
    options?: DesktopRequestOptions,
  ): Promise<VersionedSession>;
  updateSession(
    sessionId: string,
    patch: SessionPatch,
    etag: string,
    options?: DesktopRequestOptions,
  ): Promise<VersionedSession>;
  deleteSession(
    sessionId: string,
    etag?: string,
    options?: DesktopRequestOptions,
  ): Promise<void>;
  listMessages(
    sessionId: string,
    query?: ListMessagesQuery,
    options?: DesktopRequestOptions,
  ): Promise<MessagePage>;
}

const PROFILE_ID_PATTERN = /^(?:default|[a-z0-9_][a-z0-9_-]{0,63})$/u;
const PERSONA_ID_PATTERN = /^persona_[0-9a-f]{32}$/u;
const STRONG_ETAG_PATTERN = /^"[\x21\x23-\x7e]{1,126}"$/u;
const REVISION_PATTERN = /^[\x21\x23-\x7e]{1,126}$/u;
const IDEMPOTENCY_KEY_PATTERN = /^[\x21-\x7e]{8,128}$/u;
const RFC3339_PATTERN = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/u;
const MESSAGE_ROLES = new Set(["user", "assistant", "system", "tool"]);
const TOOL_STATUSES = new Set(["unknown", "running", "completed", "failed", "cancelled"]);
const SEARCH_FIELDS = new Set(["title", "id", "message"]);

function invalidResponse(context: string): never {
  throw new SessionApiError(
    "invalid_response",
    `${context} did not match the API v1 contract.`,
  );
}

function invalidRequest(message: string): never {
  throw new SessionApiError("invalid_request", message);
}

function requestRecord(
  value: unknown,
  allowedKeys: readonly string[],
  context: string,
): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    return invalidRequest(`${context} is invalid.`);
  }
  const record = value as Record<string, unknown>;
  if (Object.keys(record).some((key) => !allowedKeys.includes(key))) {
    return invalidRequest(`${context} contains an unknown field.`);
  }
  return record;
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
  if (result.length === 0) return invalidResponse(context);
  return result;
}

function nullableString(value: unknown, context: string): string | null {
  return value === null ? null : stringValue(value, context);
}

function booleanValue(value: unknown, context: string): boolean {
  if (typeof value !== "boolean") return invalidResponse(context);
  return value;
}

function integerValue(
  value: unknown,
  minimum: number,
  context: string,
  maximum = Number.MAX_SAFE_INTEGER,
): number {
  if (
    !Number.isSafeInteger(value)
    || (value as number) < minimum
    || (value as number) > maximum
  ) {
    return invalidResponse(context);
  }
  return value as number;
}

function nullableInteger(value: unknown, minimum: number, context: string): number | null {
  return value === null ? null : integerValue(value, minimum, context);
}

function dateTime(value: unknown, context: string): string {
  const result = stringValue(value, context);
  if (!RFC3339_PATTERN.test(result) || Number.isNaN(Date.parse(result))) {
    return invalidResponse(context);
  }
  return result;
}

function cursorValue(value: unknown, context: string): string | null {
  if (value === null) return null;
  const result = stringValue(value, context);
  if (result.length < 1 || result.length > 4_096) return invalidResponse(context);
  return result;
}

function titleValue(value: unknown, context: string): string {
  const result = stringValue(value, context);
  const length = Array.from(result).length;
  if (length < 1 || length > 500) return invalidResponse(context);
  return result;
}

function profileIdValue(value: unknown, context: string): string {
  const result = stringValue(value, context);
  if (!PROFILE_ID_PATTERN.test(result)) return invalidResponse(context);
  return result;
}

function parseSearchMatch(value: unknown): SearchMatch {
  const record = asRecord(value, "Session search match");
  exactKeys(record, ["field", "messageId", "snippet", "ranges"], [], "Session search match");
  const field = stringValue(record.field, "Session search match.field");
  if (!SEARCH_FIELDS.has(field) || !Array.isArray(record.ranges)) {
    return invalidResponse("Session search match");
  }
  const messageId = nullableString(record.messageId, "Session search match.messageId");
  if ((field === "message") !== (messageId !== null)) {
    return invalidResponse("Session search match.messageId");
  }
  const snippet = stringValue(record.snippet, "Session search match.snippet");
  const ranges = record.ranges.map((entry) => {
    const range = asRecord(entry, "Session search range");
    exactKeys(range, ["start", "end"], [], "Session search range");
    return {
      start: integerValue(range.start, 0, "Session search range.start"),
      end: integerValue(range.end, 0, "Session search range.end"),
    };
  });
  if (ranges.some((range, index) => (
    range.start >= range.end
    || range.end > snippet.length
    || (index > 0 && range.start < ranges[index - 1]!.end)
  ))) {
    return invalidResponse("Session search match.ranges");
  }
  return {
    field: field as SearchMatch["field"],
    messageId,
    snippet,
    ranges,
  };
}

export function parseSession(value: unknown): Session {
  const record = asRecord(value, "Session");
  exactKeys(
    record,
    [
      "id",
      "profileId",
      "personaId",
      "title",
      "preview",
      "source",
      "model",
      "messageCount",
      "archived",
      "revision",
      "createdAt",
      "updatedAt",
      "match",
    ],
    [],
    "Session",
  );
  const revision = stringValue(record.revision, "Session.revision");
  if (!REVISION_PATTERN.test(revision)) invalidResponse("Session.revision");
  return {
    id: nonEmptyString(record.id, "Session.id"),
    profileId: profileIdValue(record.profileId, "Session.profileId"),
    personaId: record.personaId === null
      ? null
      : (() => {
          const personaId = stringValue(record.personaId, "Session.personaId");
          if (!PERSONA_ID_PATTERN.test(personaId)) invalidResponse("Session.personaId");
          return personaId;
        })(),
    title: titleValue(record.title, "Session.title"),
    preview: stringValue(record.preview, "Session.preview"),
    source: stringValue(record.source, "Session.source"),
    model: stringValue(record.model, "Session.model"),
    messageCount: integerValue(record.messageCount, 0, "Session.messageCount"),
    archived: booleanValue(record.archived, "Session.archived"),
    revision,
    createdAt: dateTime(record.createdAt, "Session.createdAt"),
    updatedAt: dateTime(record.updatedAt, "Session.updatedAt"),
    match: record.match === null ? null : parseSearchMatch(record.match),
  };
}

export function parseSessionPage(value: unknown): SessionPage {
  const record = asRecord(value, "Session page");
  exactKeys(record, ["items", "nextCursor"], [], "Session page");
  if (!Array.isArray(record.items)) return invalidResponse("Session page.items");
  return {
    items: record.items.map(parseSession),
    nextCursor: cursorValue(record.nextCursor, "Session page.nextCursor"),
  };
}

function parseUsage(value: unknown): Usage {
  const record = asRecord(value, "Message usage");
  exactKeys(
    record,
    ["promptTokens", "completionTokens", "totalTokens"],
    ["cost"],
    "Message usage",
  );
  const result: Usage = {
    promptTokens: integerValue(record.promptTokens, 0, "Message usage.promptTokens"),
    completionTokens: integerValue(record.completionTokens, 0, "Message usage.completionTokens"),
    totalTokens: integerValue(record.totalTokens, 0, "Message usage.totalTokens"),
  };
  if ("cost" in record) {
    if (record.cost === null) {
      result.cost = null;
    } else if (typeof record.cost === "number" && Number.isFinite(record.cost) && record.cost >= 0) {
      result.cost = record.cost;
    } else {
      invalidResponse("Message usage.cost");
    }
  }
  return result;
}

function parseFileRef(value: unknown): FileRef {
  const record = asRecord(value, "File reference");
  exactKeys(record, ["id", "name", "mimeType", "sizeBytes", "createdAt"], [], "File reference");
  if (!isFileMimeType(record.mimeType)) {
    return invalidResponse("File reference.mimeType");
  }
  return {
    id: nonEmptyString(record.id, "File reference.id"),
    name: stringValue(record.name, "File reference.name"),
    mimeType: record.mimeType,
    sizeBytes: integerValue(record.sizeBytes, 0, "File reference.sizeBytes", MAX_FILE_BYTES),
    createdAt: dateTime(record.createdAt, "File reference.createdAt"),
  };
}

function parseToolCall(value: unknown): ToolCall {
  const record = asRecord(value, "Tool call");
  exactKeys(
    record,
    ["callId", "name", "status"],
    ["inputSummary", "resultSummary", "artifacts"],
    "Tool call",
  );
  const status = stringValue(record.status, "Tool call.status");
  if (!TOOL_STATUSES.has(status)) invalidResponse("Tool call.status");
  const result: ToolCall = {
    callId: nonEmptyString(record.callId, "Tool call.callId"),
    name: nonEmptyString(record.name, "Tool call.name"),
    status: status as ToolCall["status"],
  };
  if ("inputSummary" in record) {
    result.inputSummary = nullableString(record.inputSummary, "Tool call.inputSummary");
  }
  if ("resultSummary" in record) {
    result.resultSummary = nullableString(record.resultSummary, "Tool call.resultSummary");
  }
  if ("artifacts" in record) {
    if (!Array.isArray(record.artifacts)) invalidResponse("Tool call.artifacts");
    result.artifacts = record.artifacts.map(parseFileRef);
  }
  return result;
}

function parseMessagePart(value: unknown): TextPart | FilePart {
  const record = asRecord(value, "Message part");
  if (record.type === "text") {
    exactKeys(record, ["type", "text"], [], "Text message part");
    return { type: "text", text: stringValue(record.text, "Text message part.text") };
  }
  if (record.type === "file") {
    exactKeys(record, ["type", "fileId", "name", "mimeType"], [], "File message part");
    return {
      type: "file",
      fileId: nonEmptyString(record.fileId, "File message part.fileId"),
      name: stringValue(record.name, "File message part.name"),
      mimeType: stringValue(record.mimeType, "File message part.mimeType"),
    };
  }
  return invalidResponse("Message part.type");
}

export function parseMessage(value: unknown): Message {
  const record = asRecord(value, "Message");
  exactKeys(
    record,
    ["id", "sessionId", "sequence", "role", "parts", "reasoning", "toolCalls", "usage", "createdAt"],
    [],
    "Message",
  );
  const role = stringValue(record.role, "Message.role");
  if (!MESSAGE_ROLES.has(role) || !Array.isArray(record.parts) || !Array.isArray(record.toolCalls)) {
    return invalidResponse("Message");
  }
  const result: Message = {
    id: nonEmptyString(record.id, "Message.id"),
    sessionId: nonEmptyString(record.sessionId, "Message.sessionId"),
    sequence: integerValue(record.sequence, 1, "Message.sequence"),
    role: role as Message["role"],
    parts: record.parts.map(parseMessagePart),
    reasoning: nullableString(record.reasoning, "Message.reasoning"),
    toolCalls: record.toolCalls.map(parseToolCall),
    usage: record.usage === null ? null : parseUsage(record.usage),
    createdAt: dateTime(record.createdAt, "Message.createdAt"),
  };
  return result;
}

export function parseMessagePage(value: unknown): MessagePage {
  const record = asRecord(value, "Message page");
  exactKeys(
    record,
    ["items", "nextCursor", "snapshotLastSequence", "firstSequence", "lastSequence"],
    [],
    "Message page",
  );
  if (!Array.isArray(record.items)) return invalidResponse("Message page.items");
  const items = record.items.map(parseMessage);
  const firstSequence = nullableInteger(record.firstSequence, 1, "Message page.firstSequence");
  const lastSequence = nullableInteger(record.lastSequence, 1, "Message page.lastSequence");
  const snapshotLastSequence = integerValue(
    record.snapshotLastSequence,
    0,
    "Message page.snapshotLastSequence",
  );
  const ascending = items.every((item, index) => index === 0 || item.sequence > items[index - 1]!.sequence);
  if (
    !ascending
    || (items.length === 0 && (firstSequence !== null || lastSequence !== null))
    || (items.length > 0 && (
      firstSequence !== items[0]!.sequence
      || lastSequence !== items[items.length - 1]!.sequence
      || snapshotLastSequence < lastSequence
    ))
  ) {
    return invalidResponse("Message page sequence bounds");
  }
  return {
    items,
    nextCursor: cursorValue(record.nextCursor, "Message page.nextCursor"),
    snapshotLastSequence,
    firstSequence,
    lastSequence,
  };
}

export function parseProblemDetails(value: unknown): ProblemDetails {
  const record = asRecord(value, "Problem details");
  exactKeys(
    record,
    ["type", "title", "status", "code", "requestId", "retryable"],
    ["detail", "instance"],
    "Problem details",
  );
  const status = integerValue(record.status, 400, "Problem details.status");
  if (status > 599) invalidResponse("Problem details.status");
  const result: ProblemDetails = {
    type: stringValue(record.type, "Problem details.type"),
    title: stringValue(record.title, "Problem details.title"),
    status,
    code: stringValue(record.code, "Problem details.code"),
    requestId: stringValue(record.requestId, "Problem details.requestId"),
    retryable: booleanValue(record.retryable, "Problem details.retryable"),
  };
  if ("detail" in record) result.detail = nullableString(record.detail, "Problem details.detail");
  if ("instance" in record) result.instance = nullableString(record.instance, "Problem details.instance");
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

function checkedStrongEtag(etag: string): string {
  if (!STRONG_ETAG_PATTERN.test(etag)) {
    return invalidRequest("A single strong Session ETag is required.");
  }
  return etag;
}

async function throwHttpError(response: Response): Promise<never> {
  const problem = parseProblemDetails(await jsonPayload(response, "Session error response"));
  if (problem.status !== response.status) invalidResponse("Problem details.status");
  throw new SessionApiError("http", problem.title, {
    status: problem.status,
    code: problem.code,
    requestId: problem.requestId,
    retryable: problem.retryable,
    etag: optionalStrongEtag(response),
  });
}

async function parsedResponse<T>(
  response: Response,
  expectedStatus: number,
  context: string,
  parser: (value: unknown) => T,
): Promise<T> {
  if (response.status !== expectedStatus) return throwHttpError(response);
  return parser(await jsonPayload(response, context));
}

async function versionedResponse(
  response: Response,
  expectedStatus: number,
  context: string,
): Promise<VersionedSession> {
  if (response.status !== expectedStatus) return throwHttpError(response);
  const value = parseSession(await jsonPayload(response, context));
  const etag = optionalStrongEtag(response);
  if (!etag || etag !== `"${value.revision}"`) {
    return invalidResponse(`${context} ETag`);
  }
  return { value, etag };
}

function checkedProfileId(value: string): string {
  if (typeof value !== "string" || !PROFILE_ID_PATTERN.test(value)) {
    return invalidRequest("Profile ID is invalid.");
  }
  return value;
}

function checkedSessionId(value: string): string {
  if (typeof value !== "string" || value.length === 0) {
    return invalidRequest("Session ID is required.");
  }
  return encodeURIComponent(value);
}

function checkedCursor(value: string | undefined): string | undefined {
  if (value === undefined) return undefined;
  if (typeof value !== "string" || value.length < 1 || value.length > 4_096) {
    return invalidRequest("Cursor must contain 1 to 4096 characters.");
  }
  return value;
}

function checkedLimit(value: number | undefined): number | undefined {
  if (value === undefined) return undefined;
  if (!Number.isInteger(value) || value < 1 || value > 100) {
    return invalidRequest("Page limit must be an integer from 1 to 100.");
  }
  return value;
}

function checkedQuery(value: string | undefined): string | undefined {
  if (value === undefined) return undefined;
  if (typeof value !== "string" || Array.from(value).length > 500) {
    return invalidRequest("Session search query must not exceed 500 characters.");
  }
  return value;
}

function checkedTitle(value: string, context: string): string {
  const length = Array.from(value).length;
  if (length < 1 || length > 500) return invalidRequest(`${context} must contain 1 to 500 characters.`);
  return value;
}

function checkedCreateInput(input: CreateSessionInput): CreateSessionInput {
  const record = requestRecord(input, ["profileId", "personaId", "title"], "Session creation input");
  if (
    !("profileId" in record)
    || typeof input.profileId !== "string"
    || ("personaId" in record
      && input.personaId !== undefined
      && input.personaId !== null
      && (typeof input.personaId !== "string" || !PERSONA_ID_PATTERN.test(input.personaId)))
    || ("title" in record
      && input.title !== undefined
      && input.title !== null
      && typeof input.title !== "string")
  ) {
    return invalidRequest("Session creation fields are invalid.");
  }
  checkedProfileId(input.profileId);
  if (input.title !== undefined && input.title !== null) {
    checkedTitle(input.title, "Session title");
  }
  return input;
}

function checkedPatch(patch: SessionPatch): SessionPatch {
  const record = requestRecord(patch, ["title", "archived"], "Session patch");
  const keys = Object.keys(record);
  if (
    keys.length === 0
    || ("title" in record && typeof patch.title !== "string")
    || ("archived" in record && typeof patch.archived !== "boolean")
  ) {
    return invalidRequest("Session patch fields are invalid.");
  }
  if (patch.title !== undefined) checkedTitle(patch.title, "Session title");
  return patch;
}

function checkedIdempotencyKey(value: string): string {
  if (!IDEMPOTENCY_KEY_PATTERN.test(value)) {
    return invalidRequest("Idempotency-Key must contain 8 to 128 visible ASCII characters.");
  }
  return value;
}

function sessionQueryPath(query: ListSessionsQuery): string {
  const record = requestRecord(
    query,
    ["profileId", "q", "archived", "cursor", "limit"],
    "Session list query",
  );
  if (
    !("profileId" in record)
    || ("archived" in record
      && query.archived !== undefined
      && typeof query.archived !== "boolean")
  ) {
    invalidRequest("Session list query fields are invalid.");
  }
  const params = new URLSearchParams();
  params.set("profileId", checkedProfileId(query.profileId));
  const q = checkedQuery(query.q);
  if (q !== undefined) params.set("q", q);
  if (query.archived !== undefined) params.set("archived", String(query.archived));
  const cursor = checkedCursor(query.cursor);
  if (cursor !== undefined) params.set("cursor", cursor);
  const limit = checkedLimit(query.limit);
  if (limit !== undefined) params.set("limit", String(limit));
  return `/api/v1/sessions?${params.toString()}`;
}

function messageQueryPath(sessionId: string, query: ListMessagesQuery): string {
  requestRecord(query, ["cursor", "limit"], "Message list query");
  const params = new URLSearchParams();
  const cursor = checkedCursor(query.cursor);
  if (cursor !== undefined) params.set("cursor", cursor);
  const limit = checkedLimit(query.limit);
  if (limit !== undefined) params.set("limit", String(limit));
  const suffix = params.size > 0 ? `?${params.toString()}` : "";
  return `/api/v1/sessions/${checkedSessionId(sessionId)}/messages${suffix}`;
}

async function expectNoContent(response: Response): Promise<void> {
  if (response.status !== 204) return throwHttpError(response);
}

class DefaultSessionsApi implements SessionsApi {
  constructor(private readonly transport: DesktopTransport) {}

  async listSessions(
    query: ListSessionsQuery,
    options: DesktopRequestOptions = {},
  ): Promise<SessionPage> {
    const response = await this.transport.request(sessionQueryPath(query), {
      method: "GET",
      headers: { Accept: "application/json" },
    }, options);
    const page = await parsedResponse(response, 200, "Session page", parseSessionPage);
    const archived = query.archived ?? false;
    if (page.items.some((item) => item.profileId !== query.profileId || item.archived !== archived)) {
      return invalidResponse("Session page filters");
    }
    return page;
  }

  async searchSessions(
    query: SearchSessionsQuery,
    options: DesktopRequestOptions = {},
  ): Promise<SessionPage> {
    const record = requestRecord(
      query,
      ["profileId", "query", "archived", "cursor", "limit"],
      "Session search query",
    );
    if (!("profileId" in record) || typeof query.query !== "string" || query.query.trim().length === 0) {
      return invalidRequest("A non-empty Session search query is required.");
    }
    return this.listSessions({
      profileId: query.profileId,
      q: query.query,
      archived: query.archived,
      cursor: query.cursor,
      limit: query.limit,
    }, options);
  }

  async createSession(
    input: CreateSessionInput,
    idempotencyKey: string,
    options: DesktopRequestOptions = {},
  ): Promise<VersionedSession> {
    const checked = checkedCreateInput(input);
    const response = await this.transport.request("/api/v1/sessions", {
      method: "POST",
      headers: {
        Accept: "application/json",
        "Content-Type": "application/json",
        "Idempotency-Key": checkedIdempotencyKey(idempotencyKey),
      },
      body: JSON.stringify(checked),
    }, options);
    const created = await versionedResponse(response, 201, "Created Session");
    if (
      created.value.profileId !== input.profileId
      || created.value.personaId !== (input.personaId ?? null)
      || created.value.archived
    ) {
      return invalidResponse("Created Session");
    }
    return created;
  }

  async getSession(
    sessionId: string,
    options: DesktopRequestOptions = {},
  ): Promise<VersionedSession> {
    const response = await this.transport.request(
      `/api/v1/sessions/${checkedSessionId(sessionId)}`,
      { method: "GET", headers: { Accept: "application/json" } },
      options,
    );
    const session = await versionedResponse(response, 200, "Session response");
    if (session.value.id !== sessionId) return invalidResponse("Session response.id");
    return session;
  }

  async updateSession(
    sessionId: string,
    patch: SessionPatch,
    etag: string,
    options: DesktopRequestOptions = {},
  ): Promise<VersionedSession> {
    const response = await this.transport.request(
      `/api/v1/sessions/${checkedSessionId(sessionId)}`,
      {
        method: "PATCH",
        headers: {
          Accept: "application/json",
          "Content-Type": "application/merge-patch+json",
          "If-Match": checkedStrongEtag(etag),
        },
        body: JSON.stringify(checkedPatch(patch)),
      },
      options,
    );
    const session = await versionedResponse(response, 200, "Updated Session");
    if (session.value.id !== sessionId) return invalidResponse("Updated Session.id");
    return session;
  }

  async deleteSession(
    sessionId: string,
    etag?: string,
    options: DesktopRequestOptions = {},
  ): Promise<void> {
    const headers = new Headers({ Accept: "application/json" });
    if (etag !== undefined) headers.set("If-Match", checkedStrongEtag(etag));
    const response = await this.transport.request(
      `/api/v1/sessions/${checkedSessionId(sessionId)}`,
      {
        method: "DELETE",
        headers,
      },
      options,
    );
    return expectNoContent(response);
  }

  async listMessages(
    sessionId: string,
    query: ListMessagesQuery = {},
    options: DesktopRequestOptions = {},
  ): Promise<MessagePage> {
    const response = await this.transport.request(messageQueryPath(sessionId, query), {
      method: "GET",
      headers: { Accept: "application/json" },
    }, options);
    const page = await parsedResponse(response, 200, "Message page", parseMessagePage);
    if (page.items.some((message) => message.sessionId !== sessionId)) {
      return invalidResponse("Message page Session binding");
    }
    return page;
  }
}

export function createSessionsApi(transport: DesktopTransport = desktopTransport): SessionsApi {
  return new DefaultSessionsApi(transport);
}

export const sessionsApi = createSessionsApi();
