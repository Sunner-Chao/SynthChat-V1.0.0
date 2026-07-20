import type { components } from "./generated/openapi";
import {
  desktopTransport,
  type DesktopRequestOptions,
  type DesktopTransport,
} from "./desktopConnection";

export type Toolset = components["schemas"]["Toolset"];
export type ToolsetPatch = components["schemas"]["ToolsetPatch"];

export interface VersionedToolsets {
  value: Toolset[];
  etag: string;
}

export interface VersionedToolset {
  value: Toolset;
  etag: string;
}

export type ToolsetApiErrorKind = "http" | "invalid_request" | "invalid_response";

export class ToolsetApiError extends Error {
  readonly kind: ToolsetApiErrorKind;
  readonly status?: number;
  readonly code?: string;
  readonly requestId?: string;
  readonly retryable: boolean;
  readonly etag?: string;

  constructor(
    kind: ToolsetApiErrorKind,
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
    this.name = "ToolsetApiError";
    this.kind = kind;
    this.status = options.status;
    this.code = options.code;
    this.requestId = options.requestId;
    this.retryable = options.retryable ?? false;
    this.etag = options.etag;
  }
}

export interface ToolsetsApi {
  listToolsets(
    profileId: string,
    options?: DesktopRequestOptions,
  ): Promise<VersionedToolsets>;
  updateToolset(
    profileId: string,
    toolsetId: string,
    patch: ToolsetPatch,
    etag: string,
    options?: DesktopRequestOptions,
  ): Promise<VersionedToolset>;
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

function invalidResponse(context: string): never {
  throw new ToolsetApiError(
    "invalid_response",
    `${context} did not match the API v1 contract.`,
  );
}

function invalidRequest(message: string): never {
  throw new ToolsetApiError("invalid_request", message);
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

function booleanValue(value: unknown, context: string): boolean {
  if (typeof value !== "boolean") return invalidResponse(context);
  return value;
}

function isJsonValue(value: unknown): boolean {
  if (value === null || typeof value === "string" || typeof value === "boolean") return true;
  if (typeof value === "number") return Number.isFinite(value);
  if (Array.isArray(value)) return value.every(isJsonValue);
  if (typeof value === "object") return Object.values(value).every(isJsonValue);
  return false;
}

export function parseToolset(value: unknown): Toolset {
  const record = asRecord(value, "Toolset");
  exactKeys(
    record,
    ["id", "displayName", "description", "enabled", "configured", "tools"],
    ["configSchema"],
    "Toolset",
  );
  if (!Array.isArray(record.tools)) return invalidResponse("Toolset.tools");
  const tools = record.tools.map((tool) => nonEmptyString(tool, "Toolset.tools[]"));
  if (new Set(tools).size !== tools.length) return invalidResponse("Toolset.tools");
  const result: Toolset = {
    id: nonEmptyString(record.id, "Toolset.id"),
    displayName: nonEmptyString(record.displayName, "Toolset.displayName"),
    description: stringValue(record.description, "Toolset.description"),
    enabled: booleanValue(record.enabled, "Toolset.enabled"),
    configured: booleanValue(record.configured, "Toolset.configured"),
    tools,
  };
  if ("configSchema" in record) {
    const configSchema = asRecord(record.configSchema, "Toolset.configSchema");
    if (!Object.values(configSchema).every(isJsonValue)) {
      return invalidResponse("Toolset.configSchema");
    }
    result.configSchema = { ...configSchema };
  }
  return result;
}

export function parseToolsetList(value: unknown): Toolset[] {
  if (!Array.isArray(value)) return invalidResponse("Toolset list");
  const toolsets = value.map(parseToolset);
  if (new Set(toolsets.map((toolset) => toolset.id)).size !== toolsets.length) {
    return invalidResponse("Toolset list IDs");
  }
  return toolsets;
}

function parseProblemDetails(value: unknown): ProblemDetails {
  const record = asRecord(value, "Problem details");
  exactKeys(
    record,
    ["type", "title", "status", "code", "requestId", "retryable"],
    ["detail", "instance"],
    "Problem details",
  );
  if (!Number.isInteger(record.status) || (record.status as number) < 400 || (record.status as number) > 599) {
    return invalidResponse("Problem details.status");
  }
  if (
    ("detail" in record && record.detail !== null && typeof record.detail !== "string")
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
    return invalidRequest("A single strong Toolset ETag is required.");
  }
  return etag;
}

function checkedProfileId(profileId: string): string {
  if (typeof profileId !== "string" || !PROFILE_ID_PATTERN.test(profileId)) {
    return invalidRequest("Profile ID is invalid.");
  }
  return encodeURIComponent(profileId);
}

function checkedToolsetId(toolsetId: string): string {
  if (typeof toolsetId !== "string" || toolsetId.length === 0) {
    return invalidRequest("Toolset ID is required.");
  }
  return encodeURIComponent(toolsetId);
}

function checkedPatch(patch: ToolsetPatch): ToolsetPatch {
  if (patch === null || typeof patch !== "object" || Array.isArray(patch)) {
    return invalidRequest("Toolset patch is invalid.");
  }
  const record = patch as unknown as Record<string, unknown>;
  if (
    Object.keys(record).length !== 1
    || !("enabled" in record)
    || typeof record.enabled !== "boolean"
  ) {
    return invalidRequest("Toolset patch must contain only enabled.");
  }
  return { enabled: record.enabled };
}

async function throwHttpError(response: Response): Promise<never> {
  const problem = parseProblemDetails(await jsonPayload(response, "Toolset error response"));
  if (problem.status !== response.status) invalidResponse("Problem details.status");
  const etag = optionalStrongEtag(response);
  if (response.status === 409 && !etag) invalidResponse("Toolset conflict ETag");
  throw new ToolsetApiError("http", problem.title, {
    status: problem.status,
    code: problem.code,
    requestId: problem.requestId,
    retryable: problem.retryable,
    etag,
  });
}

class DefaultToolsetsApi implements ToolsetsApi {
  constructor(private readonly transport: DesktopTransport) {}

  async listToolsets(
    profileId: string,
    options: DesktopRequestOptions = {},
  ): Promise<VersionedToolsets> {
    const response = await this.transport.request(
      `/api/v1/profiles/${checkedProfileId(profileId)}/toolsets`,
      { method: "GET", headers: { Accept: "application/json" } },
      options,
    );
    if (response.status !== 200) return throwHttpError(response);
    const value = parseToolsetList(await jsonPayload(response, "Toolset list"));
    return { value, etag: requiredStrongEtag(response, "Toolset list") };
  }

  async updateToolset(
    profileId: string,
    toolsetId: string,
    patch: ToolsetPatch,
    etag: string,
    options: DesktopRequestOptions = {},
  ): Promise<VersionedToolset> {
    const response = await this.transport.request(
      `/api/v1/profiles/${checkedProfileId(profileId)}/toolsets/${checkedToolsetId(toolsetId)}`,
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
    if (response.status !== 200) return throwHttpError(response);
    const value = parseToolset(await jsonPayload(response, "Updated Toolset"));
    if (value.id !== toolsetId) return invalidResponse("Updated Toolset.id");
    return { value, etag: requiredStrongEtag(response, "Updated Toolset") };
  }
}

export function createToolsetsApi(transport: DesktopTransport = desktopTransport): ToolsetsApi {
  return new DefaultToolsetsApi(transport);
}

export const toolsetsApi = createToolsetsApi();
