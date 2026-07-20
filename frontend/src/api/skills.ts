import type { components } from "./generated/openapi";
import {
  desktopTransport,
  type DesktopRequestOptions,
  type DesktopTransport,
} from "./desktopConnection";

type GeneratedSkill = components["schemas"]["Skill"];

export type Skill = Omit<GeneratedSkill, "source" | "uninstallable"> & {
  source: "bundled" | "local" | "registry" | "url" | "file";
  uninstallable: boolean;
};
export type SkillPage = Omit<components["schemas"]["SkillPage"], "items"> & {
  items: Skill[];
};
export type Operation = components["schemas"]["Operation"];
export type OperationProblem = components["schemas"]["Problem"];

export type InstallSkillInput =
  | { registryId: string; url?: never; fileId?: never }
  | { registryId?: never; url: string; fileId?: never }
  | { registryId?: never; url?: never; fileId: string };

export interface SkillListRequest {
  query?: string;
  cursor?: string;
  limit?: number;
}

export interface VersionedSkillPage {
  value: SkillPage;
  etag: string;
}

export interface VersionedSkill {
  value: Skill;
  etag: string;
}

export type SkillApiErrorKind = "http" | "invalid_request" | "invalid_response";

export class SkillApiError extends Error {
  readonly kind: SkillApiErrorKind;
  readonly status?: number;
  readonly code?: string;
  readonly requestId?: string;
  readonly retryable: boolean;
  readonly etag?: string;

  constructor(
    kind: SkillApiErrorKind,
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
    this.name = "SkillApiError";
    this.kind = kind;
    this.status = options.status;
    this.code = options.code;
    this.requestId = options.requestId;
    this.retryable = options.retryable ?? false;
    this.etag = options.etag;
  }
}

export interface SkillsApi {
  listSkills(
    profileId: string,
    request?: SkillListRequest,
    options?: DesktopRequestOptions,
  ): Promise<VersionedSkillPage>;
  updateSkill(
    profileId: string,
    skillId: string,
    enabled: boolean,
    etag: string,
    options?: DesktopRequestOptions,
  ): Promise<VersionedSkill>;
  installSkill(
    profileId: string,
    input: InstallSkillInput,
    idempotencyKey: string,
    options?: DesktopRequestOptions,
  ): Promise<Operation>;
  getOperation(
    operationId: string,
    options?: DesktopRequestOptions,
  ): Promise<Operation>;
  uninstallSkill(
    profileId: string,
    skillId: string,
    idempotencyKey: string,
    options?: DesktopRequestOptions,
  ): Promise<Operation>;
}

interface ProblemDetails {
  title: string;
  status: number;
  code: string;
  requestId: string;
  retryable: boolean;
}

const PROFILE_ID_PATTERN = /^(?:default|[a-z0-9_][a-z0-9_-]{0,63})$/u;
const STRONG_ETAG_PATTERN = /^"[\x21\x23-\x7e]{1,126}"$/u;
const SKILL_ID_PATTERN = /^skill_[0-9a-f]{32}$/u;
const OPERATION_ID_PATTERN = /^op_[0-9a-f]{32}$/u;
const FILE_ID_PATTERN = /^file_[0-9a-f]{32}$/u;
const SKILL_SOURCES = new Set(["bundled", "local", "registry", "url", "file"]);
const OPERATION_KINDS = new Set(["skillInstall", "skillUninstall"]);
const OPERATION_STATUSES = new Set(["queued", "running", "completed", "failed", "cancelled"]);
const IDEMPOTENCY_KEY_PATTERN = /^[\x21-\x7e]{8,128}$/u;
const RFC3339_PATTERN = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/u;

function invalidResponse(context: string): never {
  throw new SkillApiError(
    "invalid_response",
    `${context} did not match the API v1 contract.`,
  );
}

function invalidRequest(message: string): never {
  throw new SkillApiError("invalid_request", message);
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

function booleanValue(value: unknown, context: string): boolean {
  if (typeof value !== "boolean") return invalidResponse(context);
  return value;
}

function dateTime(value: unknown, context: string): string {
  const result = stringValue(value, context);
  if (!RFC3339_PATTERN.test(result) || Number.isNaN(Date.parse(result))) {
    return invalidResponse(context);
  }
  return result;
}

function nonEmptyString(value: unknown, context: string): string {
  const result = stringValue(value, context);
  if (!result) return invalidResponse(context);
  return result;
}

function isJsonValue(value: unknown): boolean {
  if (value === null || typeof value === "string" || typeof value === "boolean") return true;
  if (typeof value === "number") return Number.isFinite(value);
  if (Array.isArray(value)) return value.every(isJsonValue);
  if (typeof value === "object") return Object.values(value).every(isJsonValue);
  return false;
}

export function parseSkill(value: unknown): Skill {
  const record = asRecord(value, "Skill");
  exactKeys(
    record,
    ["id", "name", "description", "source", "enabled", "configurable", "uninstallable"],
    ["version", "configSchema"],
    "Skill",
  );
  const source = nonEmptyString(record.source, "Skill.source");
  if (!SKILL_SOURCES.has(source)) return invalidResponse("Skill.source");
  if (
    typeof record.enabled !== "boolean"
    || typeof record.configurable !== "boolean"
    || typeof record.uninstallable !== "boolean"
    || ("version" in record && record.version !== null && typeof record.version !== "string")
  ) {
    return invalidResponse("Skill");
  }
  const id = nonEmptyString(record.id, "Skill.id");
  if (!SKILL_ID_PATTERN.test(id)) return invalidResponse("Skill.id");
  const result: Skill = {
    id,
    name: nonEmptyString(record.name, "Skill.name"),
    description: stringValue(record.description, "Skill.description"),
    source: source as Skill["source"],
    version: "version" in record ? record.version as string | null : null,
    enabled: record.enabled,
    configurable: record.configurable,
    uninstallable: record.uninstallable,
  };
  if ("configSchema" in record) {
    const schema = asRecord(record.configSchema, "Skill.configSchema");
    if (!Object.values(schema).every(isJsonValue)) {
      return invalidResponse("Skill.configSchema");
    }
    result.configSchema = { ...schema };
  }
  return result;
}

export function parseOperationProblem(value: unknown): OperationProblem {
  const record = asRecord(value, "Operation problem");
  exactKeys(
    record,
    ["type", "title", "status", "code", "requestId", "retryable"],
    ["detail", "instance"],
    "Operation problem",
  );
  if (
    !Number.isInteger(record.status)
    || (record.status as number) < 400
    || (record.status as number) > 599
    || ("detail" in record && record.detail !== null && typeof record.detail !== "string")
    || ("instance" in record && record.instance !== null && typeof record.instance !== "string")
  ) {
    return invalidResponse("Operation problem");
  }
  const problem: OperationProblem = {
    type: stringValue(record.type, "Operation problem.type"),
    title: stringValue(record.title, "Operation problem.title"),
    status: record.status as number,
    code: stringValue(record.code, "Operation problem.code"),
    requestId: stringValue(record.requestId, "Operation problem.requestId"),
    retryable: booleanValue(record.retryable, "Operation problem.retryable"),
  };
  if ("detail" in record) problem.detail = record.detail as string | null;
  if ("instance" in record) problem.instance = record.instance as string | null;
  return problem;
}

export function parseOperation(value: unknown): Operation {
  const record = asRecord(value, "Operation");
  exactKeys(
    record,
    ["id", "kind", "status", "createdAt", "updatedAt"],
    ["error"],
    "Operation",
  );
  const status = nonEmptyString(record.status, "Operation.status");
  if (!OPERATION_STATUSES.has(status)) return invalidResponse("Operation.status");
  const id = nonEmptyString(record.id, "Operation.id");
  if (!OPERATION_ID_PATTERN.test(id)) return invalidResponse("Operation.id");
  const kind = nonEmptyString(record.kind, "Operation.kind");
  if (!OPERATION_KINDS.has(kind)) return invalidResponse("Operation.kind");
  const createdAt = dateTime(record.createdAt, "Operation.createdAt");
  const updatedAt = dateTime(record.updatedAt, "Operation.updatedAt");
  if (Date.parse(updatedAt) < Date.parse(createdAt)) {
    return invalidResponse("Operation.updatedAt");
  }
  const operation: Operation = {
    id,
    kind: kind as Operation["kind"],
    status: status as Operation["status"],
    createdAt,
    updatedAt,
  };
  if ("error" in record) {
    operation.error = record.error === null ? null : parseOperationProblem(record.error);
  }
  return operation;
}

export function parseSkillPage(value: unknown): SkillPage {
  const record = asRecord(value, "Skill page");
  exactKeys(record, ["items", "nextCursor"], [], "Skill page");
  if (
    !Array.isArray(record.items)
    || (record.nextCursor !== null && typeof record.nextCursor !== "string")
  ) {
    return invalidResponse("Skill page");
  }
  const items = record.items.map(parseSkill);
  if (new Set(items.map((skill) => skill.id)).size !== items.length) {
    return invalidResponse("Skill page IDs");
  }
  return { items, nextCursor: record.nextCursor as string | null };
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
    || !Number.isInteger(record.status)
    || (record.status as number) < 400
    || (record.status as number) > 599
    || typeof record.retryable !== "boolean"
    || ("detail" in record && record.detail !== null && typeof record.detail !== "string")
    || ("instance" in record && record.instance !== null && typeof record.instance !== "string")
  ) {
    return invalidResponse("Problem details");
  }
  return {
    title: stringValue(record.title, "Problem details.title"),
    status: record.status as number,
    code: stringValue(record.code, "Problem details.code"),
    requestId: stringValue(record.requestId, "Problem details.requestId"),
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
  if (!STRONG_ETAG_PATTERN.test(etag)) {
    return invalidRequest("A single strong Skill ETag is required.");
  }
  return etag;
}

function checkedProfileId(profileId: string): string {
  if (!PROFILE_ID_PATTERN.test(profileId)) {
    return invalidRequest("Profile ID is invalid.");
  }
  return encodeURIComponent(profileId);
}

function checkedSkillId(skillId: string): string {
  if (!SKILL_ID_PATTERN.test(skillId)) {
    return invalidRequest("Skill ID is invalid.");
  }
  return encodeURIComponent(skillId);
}

function checkedOperationId(operationId: string): string {
  if (typeof operationId !== "string" || !OPERATION_ID_PATTERN.test(operationId)) {
    return invalidRequest("Operation ID is invalid.");
  }
  return encodeURIComponent(operationId);
}

function checkedIdempotencyKey(value: string): string {
  if (typeof value !== "string" || !IDEMPOTENCY_KEY_PATTERN.test(value)) {
    return invalidRequest("Idempotency-Key must contain 8 to 128 visible ASCII characters.");
  }
  return value;
}

function checkedInstallInput(input: InstallSkillInput): InstallSkillInput {
  if (input === null || typeof input !== "object" || Array.isArray(input)) {
    return invalidRequest("Skill installation source is invalid.");
  }
  const record = input as Record<string, unknown>;
  const allowed = ["registryId", "url", "fileId"];
  const keys = Object.keys(record);
  if (keys.length !== 1 || !allowed.includes(keys[0]!)) {
    return invalidRequest("Exactly one Skill installation source is required.");
  }
  const key = keys[0] as keyof InstallSkillInput;
  const source = record[key];
  if (
    typeof source !== "string"
    || !source
    || source.length > 2048
    || /[\u0000-\u001f\u007f]/u.test(source)
  ) {
    return invalidRequest("Skill installation source is invalid.");
  }
  if (key === "url") {
    try {
      const parsed = new URL(source);
      if (
        parsed.protocol !== "https:"
        || parsed.username
        || parsed.password
        || parsed.search
        || parsed.hash
        || !parsed.pathname.endsWith("/SKILL.md")
        || source.includes("%")
      ) {
        return invalidRequest("Skill URL must be a direct HTTPS /SKILL.md URL.");
      }
    } catch {
      return invalidRequest("Skill URL is invalid.");
    }
  }
  if (key === "registryId") return { registryId: source };
  if (key === "url") return { url: source };
  if (!FILE_ID_PATTERN.test(source)) {
    return invalidRequest("Skill file ID is invalid.");
  }
  return { fileId: source };
}

function checkedListRequest(request: SkillListRequest): URLSearchParams {
  if (request === null || typeof request !== "object" || Array.isArray(request)) {
    return invalidRequest("Skill list request is invalid.");
  }
  const record = request as Record<string, unknown>;
  if (Object.keys(record).some((key) => !["query", "cursor", "limit"].includes(key))) {
    return invalidRequest("Skill list request is invalid.");
  }
  const query = new URLSearchParams();
  if (request.query !== undefined) {
    if (
      typeof request.query !== "string"
      || [...request.query].length > 500
      || /[\u0000-\u001f\u007f]/u.test(request.query)
    ) {
      return invalidRequest("Skill query is invalid.");
    }
    query.set("q", request.query);
  }
  if (request.cursor !== undefined) {
    if (typeof request.cursor !== "string" || !request.cursor) {
      return invalidRequest("Skill cursor is invalid.");
    }
    query.set("cursor", request.cursor);
  }
  if (request.limit !== undefined) {
    if (!Number.isInteger(request.limit) || request.limit < 1 || request.limit > 100) {
      return invalidRequest("Skill page limit is invalid.");
    }
    query.set("limit", String(request.limit));
  }
  return query;
}

async function throwHttpError(
  response: Response,
  conflictEtagRequired = false,
): Promise<never> {
  const problem = parseProblemDetails(await jsonPayload(response, "Skill error response"));
  if (problem.status !== response.status) invalidResponse("Problem details.status");
  const etag = optionalStrongEtag(response);
  if (response.status === 409 && conflictEtagRequired && !etag) {
    invalidResponse("Skill conflict ETag");
  }
  throw new SkillApiError("http", problem.title, {
    status: problem.status,
    code: problem.code,
    requestId: problem.requestId,
    retryable: problem.retryable,
    etag,
  });
}

class DefaultSkillsApi implements SkillsApi {
  constructor(private readonly transport: DesktopTransport) {}

  async listSkills(
    profileId: string,
    request: SkillListRequest = {},
    options: DesktopRequestOptions = {},
  ): Promise<VersionedSkillPage> {
    const query = checkedListRequest(request).toString();
    const path = `/api/v1/profiles/${checkedProfileId(profileId)}/skills${query ? `?${query}` : ""}`;
    const response = await this.transport.request(
      path,
      { method: "GET", headers: { Accept: "application/json" } },
      options,
    );
    if (response.status !== 200) return throwHttpError(response);
    return {
      value: parseSkillPage(await jsonPayload(response, "Skill page")),
      etag: requiredStrongEtag(response, "Skill page"),
    };
  }

  async updateSkill(
    profileId: string,
    skillId: string,
    enabled: boolean,
    etag: string,
    options: DesktopRequestOptions = {},
  ): Promise<VersionedSkill> {
    if (typeof enabled !== "boolean") return invalidRequest("Skill enabled state is invalid.");
    const response = await this.transport.request(
      `/api/v1/profiles/${checkedProfileId(profileId)}/skills/${checkedSkillId(skillId)}`,
      {
        method: "PATCH",
        headers: {
          Accept: "application/json",
          "Content-Type": "application/merge-patch+json",
          "If-Match": checkedStrongEtag(etag),
        },
        body: JSON.stringify({ enabled }),
      },
      options,
    );
    if (response.status !== 200) return throwHttpError(response, true);
    const value = parseSkill(await jsonPayload(response, "Updated Skill"));
    if (value.id !== skillId) return invalidResponse("Updated Skill.id");
    return { value, etag: requiredStrongEtag(response, "Updated Skill") };
  }

  async installSkill(
    profileId: string,
    input: InstallSkillInput,
    idempotencyKey: string,
    options: DesktopRequestOptions = {},
  ): Promise<Operation> {
    const checkedInput = checkedInstallInput(input);
    const response = await this.transport.request(
      `/api/v1/profiles/${checkedProfileId(profileId)}/skills/install`,
      {
        method: "POST",
        headers: {
          Accept: "application/json",
          "Content-Type": "application/json",
          "Idempotency-Key": checkedIdempotencyKey(idempotencyKey),
        },
        body: JSON.stringify(checkedInput),
      },
      options,
    );
    if (response.status !== 202) return throwHttpError(response);
    return parseOperation(await jsonPayload(response, "Skill installation operation"));
  }

  async getOperation(
    operationId: string,
    options: DesktopRequestOptions = {},
  ): Promise<Operation> {
    const response = await this.transport.request(
      `/api/v1/operations/${checkedOperationId(operationId)}`,
      { method: "GET", headers: { Accept: "application/json" } },
      options,
    );
    if (response.status !== 200) return throwHttpError(response);
    const operation = parseOperation(await jsonPayload(response, "Operation"));
    if (operation.id !== operationId) return invalidResponse("Operation.id");
    return operation;
  }

  async uninstallSkill(
    profileId: string,
    skillId: string,
    idempotencyKey: string,
    options: DesktopRequestOptions = {},
  ): Promise<Operation> {
    const response = await this.transport.request(
      `/api/v1/profiles/${checkedProfileId(profileId)}/skills/${checkedSkillId(skillId)}`,
      {
        method: "DELETE",
        headers: {
          Accept: "application/json",
          "Idempotency-Key": checkedIdempotencyKey(idempotencyKey),
        },
      },
      options,
    );
    if (response.status !== 202) return throwHttpError(response);
    return parseOperation(await jsonPayload(response, "Skill uninstall operation"));
  }
}

export function createSkillsApi(transport: DesktopTransport = desktopTransport): SkillsApi {
  return new DefaultSkillsApi(transport);
}

export const skillsApi = createSkillsApi();
