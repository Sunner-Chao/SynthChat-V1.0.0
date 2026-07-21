import type { components } from "./generated/openapi";
import {
  desktopTransport,
  type DesktopRequestOptions,
  type DesktopTransport,
} from "./desktopConnection";
import {
  ALLOWED_FILE_MIME_TYPES,
  isFileMimeType,
  MAX_FILE_BYTES,
  type FileMimeType,
} from "./fileContract";

type GeneratedCapabilities = components["schemas"]["Capabilities"];
export type Capabilities = Omit<GeneratedCapabilities, "files" | "extensions"> & {
  files: {
    maxBytes: number;
    allowedMimeTypes: FileMimeType[];
  };
  extensions: GeneratedCapabilities["extensions"] & {
    wechatMessaging: boolean;
    plugins: boolean;
    personas: boolean;
    moments: boolean;
    worldbooks: boolean;
  };
};
export type Provider = components["schemas"]["Provider"];
export type ProfileSummary = components["schemas"]["Profile"];
export type ProfileMetadata = components["schemas"]["ProfileMetadata"];
export type CreateProfileInput = components["schemas"]["CreateProfile"];
export type ProfilePatch = components["schemas"]["ProfilePatch"];
export type ProfileConfig = components["schemas"]["ProfileConfig"];
export type ProfileConfigPatch = components["schemas"]["ProfileConfigPatch"];
export type CodeExecutionConfig = components["schemas"]["CodeExecutionConfig"];
export type CodeExecutionConfigPatch = components["schemas"]["CodeExecutionConfigPatch"];
export type SecretStatus = components["schemas"]["SecretStatus"];
export type ProblemDetails = components["schemas"]["Problem"];

export interface Versioned<T> {
  value: T;
  etag: string;
}

export type ProfileApiErrorKind = "http" | "invalid_request" | "invalid_response";

export class ProfileApiError extends Error {
  readonly kind: ProfileApiErrorKind;
  readonly status?: number;
  readonly code?: string;
  readonly requestId?: string;
  readonly retryable: boolean;
  readonly etag?: string;

  constructor(
    kind: ProfileApiErrorKind,
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
    this.name = "ProfileApiError";
    this.kind = kind;
    this.status = options.status;
    this.code = options.code;
    this.requestId = options.requestId;
    this.retryable = options.retryable ?? false;
    this.etag = options.etag;
  }
}

export interface ProfilesApi {
  getCapabilities(options?: DesktopRequestOptions): Promise<Capabilities>;
  listProviders(options?: DesktopRequestOptions): Promise<Provider[]>;
  listProfiles(options?: DesktopRequestOptions): Promise<ProfileSummary[]>;
  createProfile(
    input: CreateProfileInput,
    idempotencyKey: string,
    options?: DesktopRequestOptions,
  ): Promise<Versioned<ProfileMetadata>>;
  getProfile(
    profileId: string,
    options?: DesktopRequestOptions,
  ): Promise<Versioned<ProfileMetadata>>;
  updateProfile(
    profileId: string,
    patch: ProfilePatch,
    metadataEtag: string,
    options?: DesktopRequestOptions,
  ): Promise<Versioned<ProfileMetadata>>;
  deleteProfile(profileId: string, options?: DesktopRequestOptions): Promise<void>;
  activateProfile(
    profileId: string,
    options?: DesktopRequestOptions,
  ): Promise<ProfileSummary>;
  getProfileConfig(
    profileId: string,
    options?: DesktopRequestOptions,
  ): Promise<Versioned<ProfileConfig>>;
  updateProfileConfig(
    profileId: string,
    patch: ProfileConfigPatch,
    configEtag: string,
    options?: DesktopRequestOptions,
  ): Promise<Versioned<ProfileConfig>>;
  listSecretStatuses(
    profileId: string,
    options?: DesktopRequestOptions,
  ): Promise<SecretStatus[]>;
  putSecret(
    profileId: string,
    secretName: string,
    value: string,
    options?: DesktopRequestOptions,
  ): Promise<SecretStatus>;
  deleteSecret(
    profileId: string,
    secretName: string,
    options?: DesktopRequestOptions,
  ): Promise<void>;
}

const PROFILE_ID_PATTERN = /^(?:default|[a-z0-9_][a-z0-9_-]{0,63})$/u;
const NAMED_PROFILE_ID_PATTERN = /^(?!default$)[a-z0-9_][a-z0-9_-]{0,63}$/u;
const SECRET_NAME_PATTERN = /^[A-Z][A-Z0-9_]{0,127}$/u;
const STRONG_ETAG_PATTERN = /^"[\x21\x23-\x7e]{1,126}"$/u;
const RFC3339_PATTERN = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/u;
const ENGINE_STATES = new Set(["stopped", "starting", "running", "degraded", "failed"]);
const REASONING_EFFORTS = new Set(["minimal", "low", "medium", "high", "xhigh"]);
const CODE_EXECUTION_MODES = new Set(["project", "strict"]);

function invalidResponse(context: string): never {
  throw new ProfileApiError(
    "invalid_response",
    `${context} did not match the API v1 contract.`,
  );
}

function invalidRequest(message: string): never {
  throw new ProfileApiError("invalid_request", message);
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

function nullableString(value: unknown, context: string): string | null {
  if (value === null) return null;
  return stringValue(value, context);
}

function dateTime(value: unknown, context: string): string {
  const result = stringValue(value, context);
  if (!RFC3339_PATTERN.test(result) || Number.isNaN(Date.parse(result))) {
    return invalidResponse(context);
  }
  return result;
}

function profileDisplayName(value: unknown, context: string): string {
  const result = stringValue(value, context);
  const scalarLength = Array.from(result).length;
  if (scalarLength < 1 || scalarLength > 80) return invalidResponse(context);
  return result;
}

function profileColor(value: unknown, context: string): string | null {
  const result = nullableString(value, context);
  if (result !== null && !/^#[0-9a-f]{6}$/iu.test(result)) return invalidResponse(context);
  return result;
}

function nullableUri(value: unknown, context: string): string | null {
  const result = nullableString(value, context);
  if (result === null) return null;
  try {
    const url = new URL(result);
    if (
      (url.protocol !== "http:" && url.protocol !== "https:")
      || url.username.length > 0
      || url.password.length > 0
      || url.search.length > 0
      || url.hash.length > 0
    ) {
      return invalidResponse(context);
    }
  } catch {
    return invalidResponse(context);
  }
  return result;
}

function optionalDateTime(value: unknown, context: string): string | null {
  if (value === null) return null;
  return dateTime(value, context);
}

function parseBooleanMap(value: unknown, context: string): Record<string, boolean> {
  const record = asRecord(value, context);
  const result: Record<string, boolean> = {};
  for (const [key, entry] of Object.entries(record)) {
    result[key] = booleanValue(entry, `${context}.${key}`);
  }
  return result;
}

function isNonNullJson(value: unknown): boolean {
  if (value === null || value === undefined) return false;
  if (["string", "number", "boolean"].includes(typeof value)) {
    return typeof value !== "number" || Number.isFinite(value);
  }
  if (Array.isArray(value)) return value.every(isNonNullJson);
  if (typeof value === "object") return Object.values(value).every(isNonNullJson);
  return false;
}

function parseNonNullJsonMap(
  value: unknown,
  context: string,
): ProfileConfig["extensions"] {
  const record = asRecord(value, context);
  if (!Object.values(record).every(isNonNullJson)) invalidResponse(context);
  return record as ProfileConfig["extensions"];
}

function parseFeatures(value: unknown): Capabilities["engine"]["features"] {
  const record = asRecord(value, "Engine features");
  const keys = [
    "runStreaming",
    "reasoningStreaming",
    "toolProgress",
    "approvals",
    "clarifications",
    "asyncToolDelivery",
    "profileManagement",
    "skillManagement",
    "memoryWrite",
    "mcpManagement",
    "oauthAccounts",
  ] as const;
  exactKeys(record, keys, [], "Engine features");
  return {
    runStreaming: booleanValue(record.runStreaming, "Engine features.runStreaming"),
    reasoningStreaming: booleanValue(record.reasoningStreaming, "Engine features.reasoningStreaming"),
    toolProgress: booleanValue(record.toolProgress, "Engine features.toolProgress"),
    approvals: booleanValue(record.approvals, "Engine features.approvals"),
    clarifications: booleanValue(record.clarifications, "Engine features.clarifications"),
    asyncToolDelivery: booleanValue(record.asyncToolDelivery, "Engine features.asyncToolDelivery"),
    profileManagement: booleanValue(record.profileManagement, "Engine features.profileManagement"),
    skillManagement: booleanValue(record.skillManagement, "Engine features.skillManagement"),
    memoryWrite: booleanValue(record.memoryWrite, "Engine features.memoryWrite"),
    mcpManagement: booleanValue(record.mcpManagement, "Engine features.mcpManagement"),
    oauthAccounts: booleanValue(record.oauthAccounts, "Engine features.oauthAccounts"),
  };
}

function parseCapabilityExtensions(value: unknown): Capabilities["extensions"] {
  const record = asRecord(value, "Capabilities extensions");
  const required = [
    "activeRunDiscovery",
    "runQueue",
    "toolsetManagement",
    "toolExecution",
    "codeExecution",
    "workspaceManagement",
    "skillDiscovery",
    "skillEnablement",
    "webSearch",
    "webExtract",
    "browserAutomation",
    "browserCdp",
    "browserDownloads",
    "mcpStdio",
    "mcpStreamableHttp",
    "mcpSse",
    "wechatAccounts",
    "wechatMessaging",
    "plugins",
    "personas",
    "moments",
    "worldbooks",
  ] as const;
  if (required.some((key) => !(key in record))) {
    return invalidResponse("Capabilities extensions");
  }
  return {
    ...record,
    activeRunDiscovery: booleanValue(
      record.activeRunDiscovery,
      "Capabilities extensions.activeRunDiscovery",
    ),
    runQueue: booleanValue(record.runQueue, "Capabilities extensions.runQueue"),
    toolsetManagement: booleanValue(
      record.toolsetManagement,
      "Capabilities extensions.toolsetManagement",
    ),
    toolExecution: booleanValue(
      record.toolExecution,
      "Capabilities extensions.toolExecution",
    ),
    codeExecution: booleanValue(
      record.codeExecution,
      "Capabilities extensions.codeExecution",
    ),
    workspaceManagement: booleanValue(
      record.workspaceManagement,
      "Capabilities extensions.workspaceManagement",
    ),
    skillDiscovery: booleanValue(
      record.skillDiscovery,
      "Capabilities extensions.skillDiscovery",
    ),
    skillEnablement: booleanValue(
      record.skillEnablement,
      "Capabilities extensions.skillEnablement",
    ),
    webSearch: booleanValue(record.webSearch, "Capabilities extensions.webSearch"),
    webExtract: booleanValue(record.webExtract, "Capabilities extensions.webExtract"),
    browserAutomation: booleanValue(
      record.browserAutomation,
      "Capabilities extensions.browserAutomation",
    ),
    browserCdp: booleanValue(record.browserCdp, "Capabilities extensions.browserCdp"),
    browserDownloads: booleanValue(
      record.browserDownloads,
      "Capabilities extensions.browserDownloads",
    ),
    mcpStdio: booleanValue(record.mcpStdio, "Capabilities extensions.mcpStdio"),
    mcpStreamableHttp: booleanValue(
      record.mcpStreamableHttp,
      "Capabilities extensions.mcpStreamableHttp",
    ),
    mcpSse: booleanValue(record.mcpSse, "Capabilities extensions.mcpSse"),
    wechatAccounts: booleanValue(
      record.wechatAccounts,
      "Capabilities extensions.wechatAccounts",
    ),
    wechatMessaging: booleanValue(
      record.wechatMessaging,
      "Capabilities extensions.wechatMessaging",
    ),
    plugins: booleanValue(record.plugins, "Capabilities extensions.plugins"),
    personas: booleanValue(record.personas, "Capabilities extensions.personas"),
    moments: booleanValue(record.moments, "Capabilities extensions.moments"),
    worldbooks: booleanValue(record.worldbooks, "Capabilities extensions.worldbooks"),
  } as Capabilities["extensions"];
}

export function parseCapabilities(value: unknown): Capabilities {
  const record = asRecord(value, "Capabilities response");
  exactKeys(
    record,
    ["contractVersion", "backendVersion", "engine", "sessionStorage", "sessionSearch", "files", "extensions"],
    [],
    "Capabilities response",
  );
  if (record.contractVersion !== "v1") invalidResponse("Capabilities response");

  const engine = asRecord(record.engine, "Capabilities engine");
  exactKeys(
    engine,
    ["kind", "available", "version", "pinnedCommit", "features"],
    [],
    "Capabilities engine",
  );
  const kind = stringValue(engine.kind, "Capabilities engine.kind");
  if (kind !== "hermes-rust" && kind !== "unavailable") {
    invalidResponse("Capabilities engine.kind");
  }
  const sessionSearch = asRecord(record.sessionSearch, "Session search capabilities");
  exactKeys(sessionSearch, ["mode"], [], "Session search capabilities");
  const mode = stringValue(sessionSearch.mode, "Session search capabilities.mode");
  if (!["fts5", "trigram", "like", "unavailable"].includes(mode)) {
    invalidResponse("Session search capabilities.mode");
  }
  const sessionStorage = asRecord(record.sessionStorage, "Session storage capabilities");
  exactKeys(
    sessionStorage,
    ["available", "schemaVersion", "hermesImportAvailable"],
    [],
    "Session storage capabilities",
  );
  const schemaVersion = sessionStorage.schemaVersion === null
    ? null
    : sessionStorage.schemaVersion;
  if (schemaVersion !== null && (!Number.isInteger(schemaVersion) || (schemaVersion as number) < 1)) {
    invalidResponse("Session storage capabilities.schemaVersion");
  }
  const files = asRecord(record.files, "File capabilities");
  exactKeys(files, ["maxBytes", "allowedMimeTypes"], [], "File capabilities");
  if (
    !Number.isSafeInteger(files.maxBytes)
    || files.maxBytes !== MAX_FILE_BYTES
  ) {
    invalidResponse("File capabilities.maxBytes");
  }
  const allowedMimeTypes = files.allowedMimeTypes;
  if (
    !Array.isArray(allowedMimeTypes)
    || !allowedMimeTypes.every(isFileMimeType)
    || allowedMimeTypes.length !== ALLOWED_FILE_MIME_TYPES.length
    || new Set(allowedMimeTypes).size !== allowedMimeTypes.length
  ) {
    invalidResponse("File capabilities.allowedMimeTypes");
  }

  const result: Capabilities = {
    contractVersion: "v1",
    backendVersion: stringValue(record.backendVersion, "Capabilities backendVersion"),
    engine: {
      kind,
      available: booleanValue(engine.available, "Capabilities engine.available"),
      version: nullableString(engine.version, "Capabilities engine.version"),
      pinnedCommit: nullableString(engine.pinnedCommit, "Capabilities engine.pinnedCommit"),
      features: parseFeatures(engine.features),
    },
    sessionStorage: {
      available: booleanValue(sessionStorage.available, "Session storage capabilities.available"),
      schemaVersion: schemaVersion as number | null,
      hermesImportAvailable: booleanValue(
        sessionStorage.hermesImportAvailable,
        "Session storage capabilities.hermesImportAvailable",
      ),
    },
    sessionSearch: { mode: mode as Capabilities["sessionSearch"]["mode"] },
    files: {
      maxBytes: files.maxBytes as number,
      allowedMimeTypes: [...allowedMimeTypes],
    },
    extensions: parseCapabilityExtensions(record.extensions),
  };
  return result;
}

export function parseProvider(value: unknown): Provider {
  const record = asRecord(value, "Provider");
  exactKeys(
    record,
    ["id", "displayName", "defaultBaseUrl", "requiresSecret", "secretNames", "supportsModelDiscovery"],
    [],
    "Provider",
  );
  if (!Array.isArray(record.secretNames) || !record.secretNames.every((item) => typeof item === "string" && SECRET_NAME_PATTERN.test(item))) {
    invalidResponse("Provider.secretNames");
  }
  return {
    id: stringValue(record.id, "Provider.id"),
    displayName: stringValue(record.displayName, "Provider.displayName"),
    defaultBaseUrl: nullableUri(record.defaultBaseUrl, "Provider.defaultBaseUrl"),
    requiresSecret: booleanValue(record.requiresSecret, "Provider.requiresSecret"),
    secretNames: [...record.secretNames] as string[],
    supportsModelDiscovery: booleanValue(record.supportsModelDiscovery, "Provider.supportsModelDiscovery"),
  };
}

export function parseProfileSummary(value: unknown): ProfileSummary {
  const record = asRecord(value, "Profile summary");
  exactKeys(
    record,
    ["id", "displayName", "isDefault", "isActive", "engineState", "configRevision", "updatedAt"],
    ["color", "avatarFileId", "createdAt"],
    "Profile summary",
  );
  const id = stringValue(record.id, "Profile summary.id");
  const engineState = stringValue(record.engineState, "Profile summary.engineState");
  if (!PROFILE_ID_PATTERN.test(id) || !ENGINE_STATES.has(engineState)) {
    invalidResponse("Profile summary");
  }
  const result: ProfileSummary = {
    id,
    displayName: profileDisplayName(record.displayName, "Profile summary.displayName"),
    isDefault: booleanValue(record.isDefault, "Profile summary.isDefault"),
    isActive: booleanValue(record.isActive, "Profile summary.isActive"),
    engineState: engineState as ProfileSummary["engineState"],
    configRevision: stringValue(record.configRevision, "Profile summary.configRevision"),
    updatedAt: dateTime(record.updatedAt, "Profile summary.updatedAt"),
  };
  if ("color" in record) result.color = profileColor(record.color, "Profile summary.color");
  if ("avatarFileId" in record) result.avatarFileId = nullableString(record.avatarFileId, "Profile summary.avatarFileId");
  if ("createdAt" in record) result.createdAt = optionalDateTime(record.createdAt, "Profile summary.createdAt");
  return result;
}

export function parseProfileMetadata(value: unknown): ProfileMetadata {
  const record = asRecord(value, "Profile metadata");
  exactKeys(
    record,
    ["id", "displayName", "isDefault", "updatedAt"],
    ["color", "avatarFileId", "createdAt"],
    "Profile metadata",
  );
  const id = stringValue(record.id, "Profile metadata.id");
  if (!PROFILE_ID_PATTERN.test(id)) invalidResponse("Profile metadata.id");
  const result: ProfileMetadata = {
    id,
    displayName: profileDisplayName(record.displayName, "Profile metadata.displayName"),
    isDefault: booleanValue(record.isDefault, "Profile metadata.isDefault"),
    updatedAt: dateTime(record.updatedAt, "Profile metadata.updatedAt"),
  };
  if ("color" in record) result.color = profileColor(record.color, "Profile metadata.color");
  if ("avatarFileId" in record) result.avatarFileId = nullableString(record.avatarFileId, "Profile metadata.avatarFileId");
  if ("createdAt" in record) result.createdAt = optionalDateTime(record.createdAt, "Profile metadata.createdAt");
  return result;
}

function parseReasoningEffort(value: unknown): ProfileConfig["model"]["reasoningEffort"] {
  if (value === null) return null;
  const effort = stringValue(value, "Profile config.model.reasoningEffort");
  if (!REASONING_EFFORTS.has(effort)) invalidResponse("Profile config.model.reasoningEffort");
  return effort as NonNullable<ProfileConfig["model"]["reasoningEffort"]>;
}

function parseCodeExecutionConfig(value: unknown): ProfileConfig["codeExecution"] {
  const record = asRecord(value, "Profile config.codeExecution");
  exactKeys(
    record,
    ["mode", "timeoutSeconds", "maxToolCalls"],
    [],
    "Profile config.codeExecution",
  );
  const mode = stringValue(record.mode, "Profile config.codeExecution.mode");
  if (!CODE_EXECUTION_MODES.has(mode)) {
    invalidResponse("Profile config.codeExecution.mode");
  }
  if (
    !Number.isInteger(record.timeoutSeconds)
    || (record.timeoutSeconds as number) < 1
    || (record.timeoutSeconds as number) > 600
  ) {
    invalidResponse("Profile config.codeExecution.timeoutSeconds");
  }
  if (
    !Number.isInteger(record.maxToolCalls)
    || (record.maxToolCalls as number) < 1
    || (record.maxToolCalls as number) > 100
  ) {
    invalidResponse("Profile config.codeExecution.maxToolCalls");
  }
  return {
    mode: mode as ProfileConfig["codeExecution"]["mode"],
    timeoutSeconds: record.timeoutSeconds as number,
    maxToolCalls: record.maxToolCalls as number,
  };
}

export function parseProfileConfig(value: unknown): ProfileConfig {
  const record = asRecord(value, "Profile config");
  exactKeys(
    record,
    [
      "revision",
      "model",
      "codeExecution",
      "toolsets",
      "skills",
      "memoryProvider",
      "platforms",
      "extensions",
    ],
    [],
    "Profile config",
  );
  const model = asRecord(record.model, "Profile config.model");
  exactKeys(model, ["provider", "model", "baseUrl"], ["reasoningEffort"], "Profile config.model");
  const parsedModel: ProfileConfig["model"] = {
    provider: stringValue(model.provider, "Profile config.model.provider"),
    model: stringValue(model.model, "Profile config.model.model"),
    baseUrl: nullableUri(model.baseUrl, "Profile config.model.baseUrl"),
  };
  if ("reasoningEffort" in model) parsedModel.reasoningEffort = parseReasoningEffort(model.reasoningEffort);
  return {
    revision: stringValue(record.revision, "Profile config.revision"),
    model: parsedModel,
    codeExecution: parseCodeExecutionConfig(record.codeExecution),
    toolsets: parseBooleanMap(record.toolsets, "Profile config.toolsets"),
    skills: parseBooleanMap(record.skills, "Profile config.skills"),
    memoryProvider: stringValue(record.memoryProvider, "Profile config.memoryProvider"),
    platforms: parseBooleanMap(record.platforms, "Profile config.platforms"),
    extensions: parseNonNullJsonMap(record.extensions, "Profile config.extensions"),
  };
}

export function parseSecretStatus(value: unknown): SecretStatus {
  const record = asRecord(value, "Secret status");
  exactKeys(record, ["name", "configured", "storage"], ["updatedAt"], "Secret status");
  const name = stringValue(record.name, "Secret status.name");
  if (!SECRET_NAME_PATTERN.test(name) || record.storage !== "osKeychain") {
    invalidResponse("Secret status");
  }
  const result: SecretStatus = {
    name,
    configured: booleanValue(record.configured, "Secret status.configured"),
    storage: "osKeychain",
  };
  if ("updatedAt" in record) result.updatedAt = optionalDateTime(record.updatedAt, "Secret status.updatedAt");
  return result;
}

export function parseProblemDetails(value: unknown): ProblemDetails {
  const record = asRecord(value, "Problem details");
  exactKeys(
    record,
    ["type", "title", "status", "code", "requestId", "retryable"],
    ["detail", "instance"],
    "Problem details",
  );
  if (!Number.isInteger(record.status) || (record.status as number) < 400 || (record.status as number) > 599) {
    invalidResponse("Problem details.status");
  }
  const result: ProblemDetails = {
    type: stringValue(record.type, "Problem details.type"),
    title: stringValue(record.title, "Problem details.title"),
    status: record.status as number,
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

function requiredStrongEtag(response: Response, context: string): string {
  const etag = optionalStrongEtag(response);
  if (!etag) return invalidResponse(`${context} ETag`);
  return etag;
}

function checkedStrongEtag(etag: string): string {
  if (!STRONG_ETAG_PATTERN.test(etag)) invalidRequest("A single strong ETag is required.");
  return etag;
}

async function throwHttpError(response: Response): Promise<never> {
  const payload = await jsonPayload(response, "Error response");
  const problem = parseProblemDetails(payload);
  if (problem.status !== response.status) invalidResponse("Problem details.status");
  throw new ProfileApiError("http", problem.title, {
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

async function versionedResponse<T>(
  response: Response,
  expectedStatus: number,
  context: string,
  parser: (value: unknown) => T,
): Promise<Versioned<T>> {
  if (response.status !== expectedStatus) return throwHttpError(response);
  const value = parser(await jsonPayload(response, context));
  return { value, etag: requiredStrongEtag(response, context) };
}

function checkedProfileId(profileId: string): string {
  if (!PROFILE_ID_PATTERN.test(profileId)) invalidRequest("Profile ID is invalid.");
  return encodeURIComponent(profileId);
}

function checkedSecretName(secretName: string): string {
  if (!SECRET_NAME_PATTERN.test(secretName)) invalidRequest("Secret name is invalid.");
  return encodeURIComponent(secretName);
}

function checkedCreateInput(input: CreateProfileInput): CreateProfileInput {
  if (
    !NAMED_PROFILE_ID_PATTERN.test(input.id)
    || input.displayName.trim().length === 0
    || Array.from(input.displayName).length > 80
    || (input.cloneFromProfileId !== undefined
      && input.cloneFromProfileId !== null
      && !PROFILE_ID_PATTERN.test(input.cloneFromProfileId))
  ) {
    return invalidRequest("Profile creation fields are invalid.");
  }
  return input;
}

function checkedIdempotencyKey(value: string): string {
  if (!/^[\x21-\x7e]{8,128}$/u.test(value)) {
    return invalidRequest("Idempotency-Key must contain 8 to 128 characters.");
  }
  return value;
}

async function expectNoContent(response: Response): Promise<void> {
  if (response.status !== 204) return throwHttpError(response);
}

class DefaultProfilesApi implements ProfilesApi {
  constructor(private readonly transport: DesktopTransport) {}

  async getCapabilities(options: DesktopRequestOptions = {}): Promise<Capabilities> {
    const response = await this.transport.request("/api/v1/capabilities", {
      method: "GET",
      headers: { Accept: "application/json" },
    }, options);
    return parsedResponse(response, 200, "Capabilities response", parseCapabilities);
  }

  async listProviders(options: DesktopRequestOptions = {}): Promise<Provider[]> {
    const response = await this.transport.request("/api/v1/providers", {
      method: "GET",
      headers: { Accept: "application/json" },
    }, options);
    return parsedResponse(response, 200, "Provider list", (value) => {
      if (!Array.isArray(value)) return invalidResponse("Provider list");
      return value.map(parseProvider);
    });
  }

  async listProfiles(options: DesktopRequestOptions = {}): Promise<ProfileSummary[]> {
    const response = await this.transport.request("/api/v1/profiles", {
      method: "GET",
      headers: { Accept: "application/json" },
    }, options);
    return parsedResponse(response, 200, "Profile list", (value) => {
      if (!Array.isArray(value)) return invalidResponse("Profile list");
      return value.map(parseProfileSummary);
    });
  }

  async createProfile(
    input: CreateProfileInput,
    idempotencyKey: string,
    options: DesktopRequestOptions = {},
  ): Promise<Versioned<ProfileMetadata>> {
    const response = await this.transport.request("/api/v1/profiles", {
      method: "POST",
      headers: {
        Accept: "application/json",
        "Content-Type": "application/json",
        "Idempotency-Key": checkedIdempotencyKey(idempotencyKey),
      },
      body: JSON.stringify(checkedCreateInput(input)),
    }, options);
    return versionedResponse(response, 201, "Created profile", parseProfileMetadata);
  }

  async getProfile(
    profileId: string,
    options: DesktopRequestOptions = {},
  ): Promise<Versioned<ProfileMetadata>> {
    const response = await this.transport.request(`/api/v1/profiles/${checkedProfileId(profileId)}`, {
      method: "GET",
      headers: { Accept: "application/json" },
    }, options);
    return versionedResponse(response, 200, "Profile metadata", parseProfileMetadata);
  }

  async updateProfile(
    profileId: string,
    patch: ProfilePatch,
    metadataEtag: string,
    options: DesktopRequestOptions = {},
  ): Promise<Versioned<ProfileMetadata>> {
    const response = await this.transport.request(`/api/v1/profiles/${checkedProfileId(profileId)}`, {
      method: "PATCH",
      headers: {
        Accept: "application/json",
        "Content-Type": "application/merge-patch+json",
        "If-Match": checkedStrongEtag(metadataEtag),
      },
      body: JSON.stringify(patch),
    }, options);
    return versionedResponse(response, 200, "Updated profile metadata", parseProfileMetadata);
  }

  async deleteProfile(profileId: string, options: DesktopRequestOptions = {}): Promise<void> {
    const response = await this.transport.request(`/api/v1/profiles/${checkedProfileId(profileId)}`, {
      method: "DELETE",
      headers: { Accept: "application/json" },
    }, options);
    return expectNoContent(response);
  }

  async activateProfile(
    profileId: string,
    options: DesktopRequestOptions = {},
  ): Promise<ProfileSummary> {
    const response = await this.transport.request(
      `/api/v1/profiles/${checkedProfileId(profileId)}/active`,
      { method: "PUT", headers: { Accept: "application/json" } },
      options,
    );
    return parsedResponse(response, 200, "Activated profile", parseProfileSummary);
  }

  async getProfileConfig(
    profileId: string,
    options: DesktopRequestOptions = {},
  ): Promise<Versioned<ProfileConfig>> {
    const response = await this.transport.request(
      `/api/v1/profiles/${checkedProfileId(profileId)}/config`,
      { method: "GET", headers: { Accept: "application/json" } },
      options,
    );
    const result = await versionedResponse(response, 200, "Profile config", parseProfileConfig);
    if (result.etag !== `"${result.value.revision}"`) invalidResponse("Profile config ETag");
    return result;
  }

  async updateProfileConfig(
    profileId: string,
    patch: ProfileConfigPatch,
    configEtag: string,
    options: DesktopRequestOptions = {},
  ): Promise<Versioned<ProfileConfig>> {
    const response = await this.transport.request(
      `/api/v1/profiles/${checkedProfileId(profileId)}/config`,
      {
        method: "PATCH",
        headers: {
          Accept: "application/json",
          "Content-Type": "application/merge-patch+json",
          "If-Match": checkedStrongEtag(configEtag),
        },
        body: JSON.stringify(patch),
      },
      options,
    );
    const result = await versionedResponse(response, 200, "Updated profile config", parseProfileConfig);
    if (result.etag !== `"${result.value.revision}"`) invalidResponse("Profile config ETag");
    return result;
  }

  async listSecretStatuses(
    profileId: string,
    options: DesktopRequestOptions = {},
  ): Promise<SecretStatus[]> {
    const response = await this.transport.request(
      `/api/v1/profiles/${checkedProfileId(profileId)}/secrets`,
      { method: "GET", headers: { Accept: "application/json" } },
      options,
    );
    return parsedResponse(response, 200, "Secret status list", (value) => {
      if (!Array.isArray(value)) return invalidResponse("Secret status list");
      return value.map(parseSecretStatus);
    });
  }

  async putSecret(
    profileId: string,
    secretName: string,
    value: string,
    options: DesktopRequestOptions = {},
  ): Promise<SecretStatus> {
    const byteLength = new TextEncoder().encode(value).byteLength;
    if (value.length === 0 || byteLength > 2_560) {
      return invalidRequest("Secret value must contain 1 to 2560 UTF-8 bytes.");
    }
    const response = await this.transport.request(
      `/api/v1/profiles/${checkedProfileId(profileId)}/secrets/${checkedSecretName(secretName)}`,
      {
        method: "PUT",
        headers: { Accept: "application/json", "Content-Type": "application/json" },
        body: JSON.stringify({ value }),
      },
      options,
    );
    return parsedResponse(response, 200, "Stored secret status", parseSecretStatus);
  }

  async deleteSecret(
    profileId: string,
    secretName: string,
    options: DesktopRequestOptions = {},
  ): Promise<void> {
    const response = await this.transport.request(
      `/api/v1/profiles/${checkedProfileId(profileId)}/secrets/${checkedSecretName(secretName)}`,
      { method: "DELETE", headers: { Accept: "application/json" } },
      options,
    );
    return expectNoContent(response);
  }
}

export function createProfilesApi(transport: DesktopTransport = desktopTransport): ProfilesApi {
  return new DefaultProfilesApi(transport);
}

export const profilesApi = createProfilesApi();
