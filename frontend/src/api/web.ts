import type { components } from "./generated/openapi";
import {
  desktopTransport,
  type DesktopRequestOptions,
  type DesktopTransport,
} from "./desktopConnection";

export type WebProviderId = components["schemas"]["WebProviderId"];
export type WebProvider = components["schemas"]["WebProvider"];
export type EffectiveWebProvider = components["schemas"]["EffectiveWebProvider"];
export type EffectiveWebProviderStatus = EffectiveWebProvider["status"];
export type WebConfig = components["schemas"]["WebConfig"];
export type WebConfigPatch = components["schemas"]["WebConfigPatch"];

export interface VersionedWebConfig {
  value: WebConfig;
  etag: string;
}

export interface WebApi {
  listProviders(options?: DesktopRequestOptions): Promise<WebProvider[]>;
  getWebConfig(
    profileId: string,
    options?: DesktopRequestOptions,
  ): Promise<VersionedWebConfig>;
  updateWebConfig(
    profileId: string,
    patch: WebConfigPatch,
    etag: string,
    options?: DesktopRequestOptions,
  ): Promise<VersionedWebConfig>;
}

export type WebApiErrorKind = "http" | "invalid_request" | "invalid_response";

export class WebApiError extends Error {
  readonly kind: WebApiErrorKind;
  readonly status?: number;
  readonly code?: string;
  readonly requestId?: string;
  readonly retryable: boolean;
  readonly etag?: string;

  constructor(
    kind: WebApiErrorKind,
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
    this.name = "WebApiError";
    this.kind = kind;
    this.status = options.status;
    this.code = options.code;
    this.requestId = options.requestId;
    this.retryable = options.retryable ?? false;
    this.etag = options.etag;
  }
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
const SECRET_NAME_PATTERN = /^[A-Z][A-Z0-9_]{0,127}$/u;
const STRONG_ETAG_PATTERN = /^"[\x21\x23-\x7e]{1,126}"$/u;
const EFFECTIVE_STATUSES = new Set<EffectiveWebProviderStatus>([
  "ready",
  "unconfigured",
  "missingSecret",
  "unsupported",
  "capabilityUnsupported",
]);
const MIN_EXTRACT_CHAR_LIMIT = 2_000;
const MAX_EXTRACT_CHAR_LIMIT = 500_000;

function invalidResponse(context: string): never {
  throw new WebApiError(
    "invalid_response",
    `${context} did not match the API v1 contract.`,
  );
}

function invalidRequest(message: string): never {
  throw new WebApiError("invalid_request", message);
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

function providerName(value: unknown, context: string): string {
  const result = nonEmptyString(value, context);
  if (Array.from(result).length > 128) return invalidResponse(context);
  return result;
}

function nullableProviderName(value: unknown, context: string): string | null {
  return value === null ? null : providerName(value, context);
}

function secretName(value: unknown, context: string): string {
  const result = stringValue(value, context);
  if (!SECRET_NAME_PATTERN.test(result)) return invalidResponse(context);
  return result;
}

function httpsBaseUrl(value: unknown, context: string): string {
  const result = nonEmptyString(value, context);
  if (result.length > 2_048 || result !== result.trim()) return invalidResponse(context);
  try {
    const parsed = new URL(result);
    if (
      parsed.protocol !== "https:"
      || !parsed.hostname
      || parsed.username
      || parsed.password
      || parsed.search
      || parsed.hash
    ) {
      return invalidResponse(context);
    }
  } catch {
    return invalidResponse(context);
  }
  return result;
}

function extractCharLimit(value: unknown, context: string): number {
  if (
    !Number.isInteger(value)
    || (value as number) < MIN_EXTRACT_CHAR_LIMIT
    || (value as number) > MAX_EXTRACT_CHAR_LIMIT
  ) {
    return invalidResponse(context);
  }
  return value as number;
}

export function parseWebProvider(value: unknown): WebProvider {
  const record = asRecord(value, "Web provider");
  exactKeys(
    record,
    [
      "id",
      "displayName",
      "supportsSearch",
      "supportsExtract",
      "secretNames",
      "defaultBaseUrl",
      "customEndpointSupported",
    ],
    [],
    "Web provider",
  );
  if (!Array.isArray(record.secretNames)) return invalidResponse("Web provider.secretNames");
  const secretNames = record.secretNames.map((name) => secretName(name, "Web provider.secretNames[]"));
  const defaultBaseUrl = httpsBaseUrl(record.defaultBaseUrl, "Web provider.defaultBaseUrl");
  if (
    record.id !== "tavily"
    || record.displayName !== "Tavily"
    || record.supportsSearch !== true
    || record.supportsExtract !== true
    || secretNames.length !== 1
    || secretNames[0] !== "TAVILY_API_KEY"
    || record.customEndpointSupported !== false
  ) {
    return invalidResponse("Web provider");
  }
  return {
    id: "tavily",
    displayName: "Tavily",
    supportsSearch: true,
    supportsExtract: true,
    secretNames: ["TAVILY_API_KEY"],
    defaultBaseUrl,
    customEndpointSupported: false,
  };
}

export function parseWebProviderList(value: unknown): WebProvider[] {
  if (!Array.isArray(value)) return invalidResponse("Web provider list");
  const providers = value.map(parseWebProvider);
  if (new Set(providers.map((provider) => provider.id)).size !== providers.length) {
    return invalidResponse("Web provider list IDs");
  }
  return providers;
}

export function parseEffectiveWebProvider(value: unknown): EffectiveWebProvider {
  const record = asRecord(value, "Effective web provider");
  exactKeys(
    record,
    ["providerId", "status", "missingSecretNames"],
    [],
    "Effective web provider",
  );
  const status = stringValue(record.status, "Effective web provider.status");
  if (!EFFECTIVE_STATUSES.has(status as EffectiveWebProviderStatus)) {
    return invalidResponse("Effective web provider.status");
  }
  if (!Array.isArray(record.missingSecretNames) || record.missingSecretNames.length > 8) {
    return invalidResponse("Effective web provider.missingSecretNames");
  }
  return {
    providerId: nullableProviderName(record.providerId, "Effective web provider.providerId"),
    status: status as EffectiveWebProviderStatus,
    missingSecretNames: record.missingSecretNames.map((name) => (
      secretName(name, "Effective web provider.missingSecretNames[]")
    )),
  };
}

export function parseWebConfig(value: unknown): WebConfig {
  const record = asRecord(value, "Web config");
  exactKeys(
    record,
    [
      "revision",
      "sharedProvider",
      "searchProvider",
      "extractProvider",
      "extractCharLimit",
      "effectiveSearch",
      "effectiveExtract",
    ],
    [],
    "Web config",
  );
  return {
    revision: nonEmptyString(record.revision, "Web config.revision"),
    sharedProvider: nullableProviderName(record.sharedProvider, "Web config.sharedProvider"),
    searchProvider: nullableProviderName(record.searchProvider, "Web config.searchProvider"),
    extractProvider: nullableProviderName(record.extractProvider, "Web config.extractProvider"),
    extractCharLimit: extractCharLimit(record.extractCharLimit, "Web config.extractCharLimit"),
    effectiveSearch: parseEffectiveWebProvider(record.effectiveSearch),
    effectiveExtract: parseEffectiveWebProvider(record.effectiveExtract),
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
    type: nonEmptyString(record.type, "Problem details.type"),
    title: nonEmptyString(record.title, "Problem details.title"),
    status: record.status as number,
    code: nonEmptyString(record.code, "Problem details.code"),
    requestId: nonEmptyString(record.requestId, "Problem details.requestId"),
    retryable: booleanValue(record.retryable, "Problem details.retryable"),
  };
  if ("detail" in record) result.detail = record.detail as string | null;
  if ("instance" in record) result.instance = record.instance as string | null;
  return result;
}

function mediaType(response: Response): string {
  return response.headers.get("content-type")?.split(";", 1)[0]?.trim().toLowerCase() ?? "";
}

async function jsonPayload(
  response: Response,
  expectedMediaType: "application/json" | "application/problem+json",
  context: string,
): Promise<unknown> {
  if (mediaType(response) !== expectedMediaType) return invalidResponse(`${context} Content-Type`);
  try {
    return await response.json() as unknown;
  } catch {
    return invalidResponse(context);
  }
}

function responseStrongEtag(response: Response, context: string): string | undefined {
  const etag = response.headers.get("etag") ?? undefined;
  if (etag !== undefined && !STRONG_ETAG_PATTERN.test(etag)) {
    return invalidResponse(`${context} ETag`);
  }
  return etag;
}

function requiredStrongEtag(response: Response, context: string): string {
  const etag = responseStrongEtag(response, context);
  if (!etag) return invalidResponse(`${context} ETag`);
  return etag;
}

function checkedStrongEtag(etag: string): string {
  if (typeof etag !== "string" || !STRONG_ETAG_PATTERN.test(etag)) {
    return invalidRequest("A single strong Web config ETag is required.");
  }
  return etag;
}

function checkedProfileId(profileId: string): string {
  if (typeof profileId !== "string" || !PROFILE_ID_PATTERN.test(profileId)) {
    return invalidRequest("Profile ID is invalid.");
  }
  return encodeURIComponent(profileId);
}

function checkedPatch(patch: WebConfigPatch): WebConfigPatch {
  if (patch === null || typeof patch !== "object" || Array.isArray(patch)) {
    return invalidRequest("Web config patch is invalid.");
  }
  const record = patch as Record<string, unknown>;
  const allowed = new Set([
    "sharedProvider",
    "searchProvider",
    "extractProvider",
    "extractCharLimit",
  ]);
  if (Object.keys(record).some((key) => !allowed.has(key))) {
    return invalidRequest("Web config patch contains an unsupported field.");
  }
  const result: WebConfigPatch = {};
  for (const key of ["sharedProvider", "searchProvider", "extractProvider"] as const) {
    if (key in record) {
      if (record[key] !== null && record[key] !== "tavily") {
        return invalidRequest(`${key} must be tavily or null.`);
      }
      result[key] = record[key] as WebProviderId | null;
    }
  }
  if ("extractCharLimit" in record) {
    if (
      !Number.isInteger(record.extractCharLimit)
      || (record.extractCharLimit as number) < MIN_EXTRACT_CHAR_LIMIT
      || (record.extractCharLimit as number) > MAX_EXTRACT_CHAR_LIMIT
    ) {
      return invalidRequest("extractCharLimit must be an integer from 2000 to 500000.");
    }
    result.extractCharLimit = record.extractCharLimit as number;
  }
  return result;
}

async function throwHttpError(response: Response): Promise<never> {
  const problem = parseProblemDetails(await jsonPayload(
    response,
    "application/problem+json",
    "Web error response",
  ));
  if (problem.status !== response.status) return invalidResponse("Problem details.status");
  const etag = responseStrongEtag(response, "Web error response");
  if (response.status === 409 && !etag) return invalidResponse("Web conflict ETag");
  throw new WebApiError("http", problem.title, {
    status: problem.status,
    code: problem.code,
    requestId: problem.requestId,
    retryable: problem.retryable,
    etag,
  });
}

async function versionedWebConfig(response: Response, context: string): Promise<VersionedWebConfig> {
  if (response.status !== 200) return throwHttpError(response);
  const value = parseWebConfig(await jsonPayload(response, "application/json", context));
  const etag = requiredStrongEtag(response, context);
  if (etag !== `"${value.revision}"`) return invalidResponse(`${context} ETag`);
  return { value, etag };
}

class DefaultWebApi implements WebApi {
  constructor(private readonly transport: DesktopTransport) {}

  async listProviders(options: DesktopRequestOptions = {}): Promise<WebProvider[]> {
    const response = await this.transport.request(
      "/api/v1/web/providers",
      { method: "GET", headers: { Accept: "application/json" } },
      options,
    );
    if (response.status !== 200) return throwHttpError(response);
    return parseWebProviderList(await jsonPayload(
      response,
      "application/json",
      "Web provider list",
    ));
  }

  async getWebConfig(
    profileId: string,
    options: DesktopRequestOptions = {},
  ): Promise<VersionedWebConfig> {
    const response = await this.transport.request(
      `/api/v1/profiles/${checkedProfileId(profileId)}/web`,
      { method: "GET", headers: { Accept: "application/json" } },
      options,
    );
    return versionedWebConfig(response, "Web config");
  }

  async updateWebConfig(
    profileId: string,
    patch: WebConfigPatch,
    etag: string,
    options: DesktopRequestOptions = {},
  ): Promise<VersionedWebConfig> {
    const checkedProfile = checkedProfileId(profileId);
    const checkedEtag = checkedStrongEtag(etag);
    const checkedBody = checkedPatch(patch);
    const response = await this.transport.request(
      `/api/v1/profiles/${checkedProfile}/web`,
      {
        method: "PATCH",
        headers: {
          Accept: "application/json",
          "Content-Type": "application/merge-patch+json",
          "If-Match": checkedEtag,
        },
        body: JSON.stringify(checkedBody),
      },
      options,
    );
    return versionedWebConfig(response, "Updated Web config");
  }
}

export function createWebApi(transport: DesktopTransport = desktopTransport): WebApi {
  return new DefaultWebApi(transport);
}

export const webApi = createWebApi();
