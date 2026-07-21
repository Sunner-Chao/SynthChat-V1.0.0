import {
  desktopTransport,
  type DesktopRequestOptions,
  type DesktopTransport,
} from "./desktopConnection";

export interface Plugin {
  id: string;
  name: string;
  version: string;
  description: string;
  author: string;
  providedTools: string[];
  requiresEnv: string[];
  enabled: boolean;
  execution: "manifestOnly";
  installedAt: string;
  updatedAt: string;
}

export interface PluginPage {
  items: Plugin[];
}

export interface InstallPluginInput {
  sourcePath: string;
}

export interface PluginPatch {
  enabled: boolean;
}

export interface VersionedPluginPage {
  value: PluginPage;
  etag: string;
}

export interface VersionedPlugin {
  value: Plugin;
  etag: string;
}

export interface DeletedPlugin {
  etag: string;
}

export type PluginApiErrorKind = "http" | "invalid_request" | "invalid_response";

export class PluginApiError extends Error {
  readonly kind: PluginApiErrorKind;
  readonly status?: number;
  readonly code?: string;
  readonly requestId?: string;
  readonly retryable: boolean;

  constructor(
    kind: PluginApiErrorKind,
    message: string,
    options: {
      status?: number;
      code?: string;
      requestId?: string;
      retryable?: boolean;
    } = {},
  ) {
    super(message);
    this.name = "PluginApiError";
    this.kind = kind;
    this.status = options.status;
    this.code = options.code;
    this.requestId = options.requestId;
    this.retryable = options.retryable ?? false;
  }
}

export interface PluginsApi {
  listPlugins(options?: DesktopRequestOptions): Promise<VersionedPluginPage>;
  installPlugin(input: InstallPluginInput, options?: DesktopRequestOptions): Promise<VersionedPlugin>;
  updatePlugin(pluginId: string, patch: PluginPatch, etag: string, options?: DesktopRequestOptions): Promise<VersionedPlugin>;
  uninstallPlugin(pluginId: string, etag: string, options?: DesktopRequestOptions): Promise<DeletedPlugin>;
}

interface ProblemDetails {
  type: string;
  title: string;
  status: number;
  detail: string;
  instance: string;
  code: string;
  requestId: string;
  retryable: boolean;
}

const PLUGIN_ID = /^[a-z0-9][a-z0-9_.-]{0,63}$/u;
const ENV_NAME = /^[A-Z_][A-Z0-9_]{0,127}$/u;
const VERSION = /^[A-Za-z0-9][A-Za-z0-9._+-]{0,63}$/u;
const ETAG = /^"plugin-catalog-(0|[1-9]\d*)"$/u;
const RFC3339 = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/u;

function invalidResponse(context: string): never {
  throw new PluginApiError("invalid_response", `${context} did not match the API v1 contract.`);
}

function invalidRequest(message: string): never {
  throw new PluginApiError("invalid_request", message);
}

function record(value: unknown, context: string): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return invalidResponse(context);
  return value as Record<string, unknown>;
}

function requestRecord(value: unknown, message: string): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    return invalidRequest(message);
  }
  return value as Record<string, unknown>;
}

function exactKeys(
  value: Record<string, unknown>,
  required: readonly string[],
  optional: readonly string[],
  context: string,
): void {
  const allowed = new Set([...required, ...optional]);
  if (required.some((key) => !(key in value)) || Object.keys(value).some((key) => !allowed.has(key))) {
    invalidResponse(context);
  }
}

function text(value: unknown, max: number, context: string, allowEmpty = false): string {
  if (
    typeof value !== "string"
    || value.trim() !== value
    || (!allowEmpty && !value)
    || Array.from(value).length > max
    || /[\u0000-\u001f\u007f]/u.test(value)
  ) return invalidResponse(context);
  return value;
}

function boolean(value: unknown, context: string): boolean {
  if (typeof value !== "boolean") return invalidResponse(context);
  return value;
}

function dateTime(value: unknown, context: string): string {
  const result = text(value, 64, context);
  if (!RFC3339.test(result) || Number.isNaN(Date.parse(result))) return invalidResponse(context);
  return result;
}

function unique(values: string[], context: string): string[] {
  if (new Set(values).size !== values.length) return invalidResponse(context);
  return values;
}

export function parsePlugin(value: unknown): Plugin {
  const item = record(value, "Plugin");
  exactKeys(item, [
    "id", "name", "version", "description", "author", "providedTools", "requiresEnv",
    "enabled", "execution", "installedAt", "updatedAt",
  ], [], "Plugin");
  const id = text(item.id, 64, "Plugin.id");
  if (!PLUGIN_ID.test(id)) return invalidResponse("Plugin.id");
  const version = text(item.version, 64, "Plugin.version");
  if (!VERSION.test(version)) return invalidResponse("Plugin.version");
  if (!Array.isArray(item.providedTools) || !Array.isArray(item.requiresEnv)) {
    return invalidResponse("Plugin lists");
  }
  if (item.providedTools.length > 128 || item.requiresEnv.length > 128 || item.execution !== "manifestOnly") {
    return invalidResponse("Plugin limits");
  }
  const providedTools = unique(item.providedTools.map((tool) => {
    const result = text(tool, 128, "Plugin.providedTools");
    if (!/^[A-Za-z0-9_.:-]+$/u.test(result)) return invalidResponse("Plugin.providedTools");
    return result;
  }), "Plugin.providedTools");
  const requiresEnv = unique(item.requiresEnv.map((name) => {
    const result = text(name, 128, "Plugin.requiresEnv");
    if (!ENV_NAME.test(result)) return invalidResponse("Plugin.requiresEnv");
    return result;
  }), "Plugin.requiresEnv");
  return {
    id,
    name: text(item.name, 120, "Plugin.name"),
    version,
    description: text(item.description, 4_096, "Plugin.description", true),
    author: text(item.author, 120, "Plugin.author"),
    providedTools,
    requiresEnv,
    enabled: boolean(item.enabled, "Plugin.enabled"),
    execution: "manifestOnly",
    installedAt: dateTime(item.installedAt, "Plugin.installedAt"),
    updatedAt: dateTime(item.updatedAt, "Plugin.updatedAt"),
  };
}

export function parsePluginPage(value: unknown): PluginPage {
  const item = record(value, "Plugin page");
  exactKeys(item, ["items"], [], "Plugin page");
  if (!Array.isArray(item.items) || item.items.length > 512) return invalidResponse("Plugin page.items");
  const items = item.items.map(parsePlugin);
  unique(items.map((plugin) => plugin.id), "Plugin page IDs");
  return { items };
}

function parseProblem(value: unknown): ProblemDetails {
  const item = record(value, "Problem details");
  exactKeys(item, ["type", "title", "status", "detail", "instance", "code", "requestId", "retryable"], [], "Problem details");
  if (!Number.isInteger(item.status) || (item.status as number) < 400 || (item.status as number) > 599) {
    return invalidResponse("Problem details.status");
  }
  return {
    type: text(item.type, 2_048, "Problem details.type"),
    title: text(item.title, 512, "Problem details.title"),
    status: item.status as number,
    detail: text(item.detail, 4_096, "Problem details.detail"),
    instance: text(item.instance, 4_096, "Problem details.instance"),
    code: text(item.code, 256, "Problem details.code"),
    requestId: text(item.requestId, 256, "Problem details.requestId"),
    retryable: boolean(item.retryable, "Problem details.retryable"),
  };
}

async function json(response: Response, context: string): Promise<unknown> {
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

function requiredEtag(response: Response, context: string): string {
  const etag = response.headers.get("etag") ?? "";
  const match = ETAG.exec(etag);
  if (!match || !Number.isSafeInteger(Number(match[1]))) return invalidResponse(`${context} ETag`);
  return etag;
}

function checkedEtag(value: string): string {
  if (typeof value !== "string" || !ETAG.test(value) || !Number.isSafeInteger(Number(ETAG.exec(value)?.[1]))) {
    return invalidRequest("A strong plugin catalog ETag is required.");
  }
  return value;
}

function checkedPluginId(value: string): string {
  if (typeof value !== "string" || !PLUGIN_ID.test(value)) return invalidRequest("Plugin ID is invalid.");
  return encodeURIComponent(value);
}

function checkedInstallInput(input: InstallPluginInput): InstallPluginInput {
  const item = requestRecord(input, "Plugin installation input is invalid.");
  const keys = Object.keys(item);
  if (keys.length !== 1 || keys[0] !== "sourcePath") return invalidRequest("Plugin installation input is invalid.");
  if (
    typeof item.sourcePath !== "string"
    || item.sourcePath.trim() !== item.sourcePath
    || !item.sourcePath
    || item.sourcePath.length > 4_096
    || /[\u0000-\u001f\u007f]/u.test(item.sourcePath)
  ) return invalidRequest("Plugin source path is invalid.");
  return { sourcePath: item.sourcePath };
}

function checkedPatch(patch: PluginPatch): PluginPatch {
  const item = requestRecord(patch, "Plugin patch must contain only enabled.");
  const keys = Object.keys(item);
  if (keys.length !== 1 || keys[0] !== "enabled" || typeof item.enabled !== "boolean") {
    return invalidRequest("Plugin patch must contain only enabled.");
  }
  return { enabled: item.enabled };
}

async function throwHttpError(response: Response): Promise<never> {
  const problem = parseProblem(await json(response, "Plugin error response"));
  if (problem.status !== response.status) return invalidResponse("Problem details.status");
  throw new PluginApiError("http", problem.title, {
    status: problem.status,
    code: problem.code,
    requestId: problem.requestId,
    retryable: problem.retryable,
  });
}

class DefaultPluginsApi implements PluginsApi {
  constructor(private readonly transport: DesktopTransport) {}

  async listPlugins(options: DesktopRequestOptions = {}): Promise<VersionedPluginPage> {
    const response = await this.transport.request("/api/v1/plugins", {
      method: "GET",
      headers: { Accept: "application/json" },
    }, options);
    if (response.status !== 200) return throwHttpError(response);
    return {
      value: parsePluginPage(await json(response, "Plugin page")),
      etag: requiredEtag(response, "Plugin page"),
    };
  }

  async installPlugin(input: InstallPluginInput, options: DesktopRequestOptions = {}): Promise<VersionedPlugin> {
    const body = checkedInstallInput(input);
    const response = await this.transport.request("/api/v1/plugins/install", {
      method: "POST",
      headers: { Accept: "application/json", "Content-Type": "application/json" },
      body: JSON.stringify(body),
    }, options);
    if (response.status !== 201) return throwHttpError(response);
    return {
      value: parsePlugin(await json(response, "Installed plugin")),
      etag: requiredEtag(response, "Installed plugin"),
    };
  }

  async updatePlugin(pluginId: string, patch: PluginPatch, etag: string, options: DesktopRequestOptions = {}): Promise<VersionedPlugin> {
    const response = await this.transport.request(`/api/v1/plugins/${checkedPluginId(pluginId)}`, {
      method: "PATCH",
      headers: {
        Accept: "application/json",
        "Content-Type": "application/merge-patch+json",
        "If-Match": checkedEtag(etag),
      },
      body: JSON.stringify(checkedPatch(patch)),
    }, options);
    if (response.status !== 200) return throwHttpError(response);
    return {
      value: parsePlugin(await json(response, "Updated plugin")),
      etag: requiredEtag(response, "Updated plugin"),
    };
  }

  async uninstallPlugin(pluginId: string, etag: string, options: DesktopRequestOptions = {}): Promise<DeletedPlugin> {
    const response = await this.transport.request(`/api/v1/plugins/${checkedPluginId(pluginId)}`, {
      method: "DELETE",
      headers: { Accept: "application/json", "If-Match": checkedEtag(etag) },
    }, options);
    if (response.status !== 204) return throwHttpError(response);
    return { etag: requiredEtag(response, "Deleted plugin") };
  }
}

export function createPluginsApi(transport: DesktopTransport = desktopTransport): PluginsApi {
  return new DefaultPluginsApi(transport);
}

export const pluginsApi = createPluginsApi();
