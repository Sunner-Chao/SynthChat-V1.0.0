import type { components } from "./generated/openapi";
import {
  desktopTransport,
  type DesktopRequestOptions,
  type DesktopTransport,
} from "./desktopConnection";

export type McpServer = components["schemas"]["McpServer"];
export type CreateMcpServerInput = components["schemas"]["CreateMcpServer"];
export type McpServerPatch = components["schemas"]["McpServerPatch"];
export type McpTransport = McpServer["transport"];

export interface VersionedMcpServers {
  value: McpServer[];
  etag: string;
}

export interface VersionedMcpServer {
  value: McpServer;
  etag: string;
}

export interface DeletedMcpServer {
  etag: string;
}

export type McpApiErrorKind = "http" | "invalid_request" | "invalid_response";

export class McpApiError extends Error {
  readonly kind: McpApiErrorKind;
  readonly status?: number;
  readonly code?: string;
  readonly requestId?: string;
  readonly retryable: boolean;
  readonly etag?: string;

  constructor(
    kind: McpApiErrorKind,
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
    this.name = "McpApiError";
    this.kind = kind;
    this.status = options.status;
    this.code = options.code;
    this.requestId = options.requestId;
    this.retryable = options.retryable ?? false;
    this.etag = options.etag;
  }
}

export interface McpApi {
  listServers(
    profileId: string,
    options?: DesktopRequestOptions,
  ): Promise<VersionedMcpServers>;
  createServer(
    profileId: string,
    input: CreateMcpServerInput,
    idempotencyKey: string,
    options?: DesktopRequestOptions,
  ): Promise<VersionedMcpServer>;
  updateServer(
    profileId: string,
    serverId: string,
    patch: McpServerPatch,
    etag: string,
    options?: DesktopRequestOptions,
  ): Promise<VersionedMcpServer>;
  deleteServer(
    profileId: string,
    serverId: string,
    etag: string,
    options?: DesktopRequestOptions,
  ): Promise<DeletedMcpServer>;
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
const SERVER_ID_PATTERN = /^mcp_[0-9a-f]{32}$/u;
const SERVER_NAME_PATTERN = /^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/u;
const SECRET_NAME_PATTERN = /^[A-Z][A-Z0-9_]{0,127}$/u;
const STRONG_ETAG_PATTERN = /^"[\x21\x23-\x7e]{1,126}"$/u;
const IDEMPOTENCY_KEY_PATTERN = /^[\x21-\x7e]{8,128}$/u;
const TRANSPORTS = new Set<McpTransport>(["stdio", "streamableHttp", "sse"]);
const SENSITIVE_ARGUMENT_MARKERS = [
  "token",
  "apikey",
  "secret",
  "password",
  "passwd",
  "credential",
  "authorization",
  "auth",
  "privatekey",
  "accesskey",
] as const;

function invalidResponse(context: string): never {
  throw new McpApiError("invalid_response", `${context} did not match the API v1 contract.`);
}

function invalidRequest(message: string): never {
  throw new McpApiError("invalid_request", message);
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

function isExactObject(
  value: unknown,
  required: readonly string[],
  optional: readonly string[],
): value is Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return false;
  const record = value as Record<string, unknown>;
  const allowed = new Set([...required, ...optional]);
  return required.every((key) => key in record)
    && Object.keys(record).every((key) => allowed.has(key));
}

function stringValue(value: unknown, context: string): string {
  if (typeof value !== "string") return invalidResponse(context);
  return value;
}

function booleanValue(value: unknown, context: string): boolean {
  if (typeof value !== "boolean") return invalidResponse(context);
  return value;
}

function integerValue(value: unknown, minimum: number, maximum: number, context: string): number {
  if (!Number.isInteger(value) || (value as number) < minimum || (value as number) > maximum) {
    return invalidResponse(context);
  }
  return value as number;
}

function nullableString(value: unknown, context: string): string | null {
  return value === null ? null : stringValue(value, context);
}

function transportValue(value: unknown, context: string): McpTransport {
  if (typeof value !== "string" || !TRANSPORTS.has(value as McpTransport)) {
    return invalidResponse(context);
  }
  return value as McpTransport;
}

function serverName(value: unknown, context: string): string {
  const name = stringValue(value, context);
  if (!SERVER_NAME_PATTERN.test(name)) return invalidResponse(context);
  return name;
}

function secretName(value: unknown, context: string): string {
  const name = stringValue(value, context);
  if (!SECRET_NAME_PATTERN.test(name)) return invalidResponse(context);
  return name;
}

function secretNames(value: unknown, context: string): string[] {
  if (!Array.isArray(value) || value.length > 32) return invalidResponse(context);
  const names = value.map((item) => secretName(item, `${context}[]`));
  if (new Set(names).size !== names.length) return invalidResponse(context);
  return names;
}

function utf8Bytes(value: string): number {
  return new TextEncoder().encode(value).byteLength;
}

function sensitiveArgument(value: string): boolean {
  const trimmed = value.trim();
  const option = trimmed.replace(/^-+/u, "");
  const assignmentIndex = option.indexOf("=");
  const name = assignmentIndex >= 0 ? option.slice(0, assignmentIndex) : option;
  const normalized = [...name]
    .filter((character) => /[A-Za-z0-9]/u.test(character))
    .join("")
    .toLowerCase();
  const sensitive = SENSITIVE_ARGUMENT_MARKERS.some((marker) => normalized.includes(marker));
  return sensitive && (trimmed.startsWith("-") || assignmentIndex >= 0);
}

function argumentList(value: unknown, context: string): string[] {
  if (!Array.isArray(value) || value.length > 64) return invalidResponse(context);
  let total = 0;
  const args = value.map((item) => {
    const arg = stringValue(item, `${context}[]`);
    const bytes = utf8Bytes(arg);
    if (bytes > 2_048 || /[\0\r\n]/u.test(arg) || sensitiveArgument(arg)) {
      return invalidResponse(`${context}[]`);
    }
    total += bytes;
    return arg;
  });
  if (total > 16_384) return invalidResponse(context);
  return args;
}

function validCommand(value: unknown, context: string): string {
  const command = stringValue(value, context);
  if (
    command.length === 0
    || utf8Bytes(command) > 1_024
    || command.trim() !== command
    || command.startsWith("-")
    || /[\x00-\x1f\x7f&|;<>`$"']/u.test(command)
    || (/\s/u.test(command) && !/[\\/]/u.test(command))
  ) return invalidResponse(context);
  const executable = command.split(/[\\/]/u).at(-1)?.toLowerCase().replace(/\.exe$/u, "") ?? "";
  if (["sh", "bash", "zsh", "fish", "cmd", "powershell", "pwsh", "wscript", "cscript"].includes(executable)) {
    return invalidResponse(context);
  }
  return command;
}

function parseIpv4(value: string): number[] | null {
  const parts = value.split(".");
  if (parts.length !== 4 || parts.some((part) => !/^\d{1,3}$/u.test(part))) return null;
  const octets = parts.map(Number);
  return octets.every((octet) => octet >= 0 && octet <= 255) ? octets : null;
}

function restrictedIpv4([a, b, c, d]: number[]): boolean {
  return a === 0
    || a === 10
    || a === 127
    || (a === 169 && b === 254)
    || (a === 172 && b >= 16 && b <= 31)
    || (a === 192 && b === 168)
    || (a >= 224 && a <= 239)
    || a >= 240
    || (a === 100 && b >= 64 && b <= 127)
    || (a === 192 && b === 0 && (c === 0 || c === 2))
    || (a === 198 && (b === 18 || b === 19))
    || (a === 198 && b === 51 && c === 100)
    || (a === 203 && b === 0 && c === 113)
    || (a === 255 && b === 255 && c === 255 && d === 255);
}

function mappedIpv4(value: string): number[] | null {
  if (!value.startsWith("::ffff:")) return null;
  const tail = value.slice("::ffff:".length);
  const dotted = parseIpv4(tail);
  if (dotted) return dotted;
  const words = tail.split(":");
  if (words.length !== 2 || words.some((word) => !/^[0-9a-f]{1,4}$/u.test(word))) return null;
  const [high, low] = words.map((word) => Number.parseInt(word, 16));
  if (high === undefined || low === undefined) return null;
  return [high >>> 8, high & 0xff, low >>> 8, low & 0xff];
}

function remoteUrl(value: unknown, context: string): string {
  const raw = stringValue(value, context);
  if (raw.length === 0 || utf8Bytes(raw) > 2_048) return invalidResponse(context);
  let parsed: URL;
  try {
    parsed = new URL(raw);
  } catch {
    return invalidResponse(context);
  }
  if (parsed.username || parsed.password || parsed.search || parsed.hash) return invalidResponse(context);
  const hostname = parsed.hostname.toLowerCase();
  const ipv4 = parseIpv4(hostname);
  const bracketless = hostname.startsWith("[") && hostname.endsWith("]")
    ? hostname.slice(1, -1)
    : hostname;
  const loopback = hostname === "localhost"
    || hostname.endsWith(".localhost")
    || ipv4?.[0] === 127
    || bracketless === "::1";
  if (parsed.protocol !== "https:" && !(parsed.protocol === "http:" && loopback)) {
    return invalidResponse(context);
  }
  if (ipv4 && !loopback && restrictedIpv4(ipv4)) return invalidResponse(context);
  const mapped = mappedIpv4(bracketless);
  if (mapped && restrictedIpv4(mapped)) return invalidResponse(context);
  if (
    bracketless.includes(":")
    && !loopback
    && (/^::$/u.test(bracketless)
      || /^ff/u.test(bracketless)
      || /^f[cd]/u.test(bracketless)
      || /^fe[89ab]/u.test(bracketless))
  ) return invalidResponse(context);
  return raw;
}

export function parseMcpServer(value: unknown): McpServer {
  const record = asRecord(value, "McpServer");
  exactKeys(
    record,
    [
      "id",
      "name",
      "transport",
      "command",
      "args",
      "url",
      "enabled",
      "timeoutSeconds",
      "envSecretNames",
      "bearerTokenSecretName",
      "missingSecretNames",
    ],
    [],
    "McpServer",
  );
  const id = stringValue(record.id, "McpServer.id");
  if (!SERVER_ID_PATTERN.test(id)) return invalidResponse("McpServer.id");
  const transport = transportValue(record.transport, "McpServer.transport");
  const command = nullableString(record.command, "McpServer.command");
  const args = argumentList(record.args, "McpServer.args");
  const url = nullableString(record.url, "McpServer.url");
  const envSecretNames = secretNames(record.envSecretNames, "McpServer.envSecretNames");
  const bearerTokenSecretName = record.bearerTokenSecretName === null
    ? null
    : secretName(record.bearerTokenSecretName, "McpServer.bearerTokenSecretName");
  const missingSecretNames = secretNames(record.missingSecretNames, "McpServer.missingSecretNames");
  const declared = new Set([
    ...envSecretNames,
    ...(bearerTokenSecretName ? [bearerTokenSecretName] : []),
  ]);
  if (missingSecretNames.some((name) => !declared.has(name))) {
    return invalidResponse("McpServer.missingSecretNames");
  }
  if (transport === "stdio") {
    if (command === null || url !== null || bearerTokenSecretName !== null) {
      return invalidResponse("McpServer transport fields");
    }
    validCommand(command, "McpServer.command");
  } else {
    if (command !== null || args.length !== 0 || envSecretNames.length !== 0 || url === null) {
      return invalidResponse("McpServer transport fields");
    }
    remoteUrl(url, "McpServer.url");
  }
  return {
    id,
    name: serverName(record.name, "McpServer.name"),
    transport,
    command,
    args,
    url,
    enabled: booleanValue(record.enabled, "McpServer.enabled"),
    timeoutSeconds: integerValue(record.timeoutSeconds, 1, 600, "McpServer.timeoutSeconds"),
    envSecretNames,
    bearerTokenSecretName,
    missingSecretNames,
  };
}

export function parseMcpServerList(value: unknown): McpServer[] {
  if (!Array.isArray(value) || value.length > 128) return invalidResponse("McpServer list");
  const servers = value.map(parseMcpServer);
  if (
    new Set(servers.map((server) => server.id)).size !== servers.length
    || new Set(servers.map((server) => server.name)).size !== servers.length
  ) return invalidResponse("McpServer list");
  return servers;
}

function checkedProfileId(profileId: string): string {
  if (typeof profileId !== "string" || !PROFILE_ID_PATTERN.test(profileId)) {
    return invalidRequest("Profile ID is invalid.");
  }
  return encodeURIComponent(profileId);
}

function checkedServerId(serverId: string): string {
  if (typeof serverId !== "string" || !SERVER_ID_PATTERN.test(serverId)) {
    return invalidRequest("MCP server ID is invalid.");
  }
  return encodeURIComponent(serverId);
}

function checkedStrongEtag(etag: string): string {
  if (typeof etag !== "string" || !STRONG_ETAG_PATTERN.test(etag)) {
    return invalidRequest("A single strong MCP configuration ETag is required.");
  }
  return etag;
}

function checkedIdempotencyKey(key: string): string {
  if (typeof key !== "string" || !IDEMPOTENCY_KEY_PATTERN.test(key)) {
    return invalidRequest("A valid MCP Idempotency-Key is required.");
  }
  return key;
}

function checkedTimeout(value: unknown): number {
  if (!Number.isInteger(value) || (value as number) < 1 || (value as number) > 600) {
    return invalidRequest("MCP timeoutSeconds must be an integer from 1 to 600.");
  }
  return value as number;
}

function checkedName(value: unknown): string {
  if (typeof value !== "string" || !SERVER_NAME_PATTERN.test(value)) {
    return invalidRequest("MCP server name is invalid.");
  }
  return value;
}

function checkedSecretName(value: unknown): string {
  if (typeof value !== "string" || !SECRET_NAME_PATTERN.test(value)) {
    return invalidRequest("MCP secret reference name is invalid.");
  }
  return value;
}

function checkedSecretNames(value: unknown): string[] {
  if (!Array.isArray(value) || value.length > 32) {
    return invalidRequest("MCP secret reference list is invalid.");
  }
  const names = value.map(checkedSecretName);
  if (new Set(names).size !== names.length) {
    return invalidRequest("MCP secret reference names must be unique.");
  }
  return names;
}

function checkedArgs(value: unknown): string[] {
  try {
    return argumentList(value, "MCP args");
  } catch (error) {
    if (error instanceof McpApiError) {
      return invalidRequest("MCP argv is invalid or contains a sensitive option; use keychain references.");
    }
    throw error;
  }
}

function checkedCommand(value: unknown): string {
  try {
    return validCommand(value, "MCP command");
  } catch (error) {
    if (error instanceof McpApiError) return invalidRequest("MCP command must be one direct executable.");
    throw error;
  }
}

function checkedRemoteUrl(value: unknown): string {
  try {
    return remoteUrl(value, "MCP URL");
  } catch (error) {
    if (error instanceof McpApiError) return invalidRequest("MCP URL is not an allowed HTTPS or loopback HTTP URL.");
    throw error;
  }
}

function checkedCreateInput(input: CreateMcpServerInput): CreateMcpServerInput {
  const common = ["name", "transport", "enabled", "timeoutSeconds"] as const;
  if (input === null || typeof input !== "object" || Array.isArray(input)) {
    return invalidRequest("MCP create payload is invalid.");
  }
  const record = input as unknown as Record<string, unknown>;
  if (record.transport === "stdio") {
    if (!isExactObject(record, [...common, "command", "args", "envSecretNames"], [])) {
      return invalidRequest("MCP stdio create payload is invalid.");
    }
    return {
      name: checkedName(record.name),
      transport: "stdio",
      command: checkedCommand(record.command),
      args: checkedArgs(record.args),
      enabled: typeof record.enabled === "boolean" ? record.enabled : invalidRequest("MCP enabled is invalid."),
      timeoutSeconds: checkedTimeout(record.timeoutSeconds),
      envSecretNames: checkedSecretNames(record.envSecretNames),
    };
  }
  if (record.transport === "streamableHttp" || record.transport === "sse") {
    if (!isExactObject(record, [...common, "url"], ["bearerTokenSecretName"])) {
      return invalidRequest("MCP remote create payload is invalid.");
    }
    const result = {
      name: checkedName(record.name),
      transport: record.transport,
      url: checkedRemoteUrl(record.url),
      enabled: typeof record.enabled === "boolean" ? record.enabled : invalidRequest("MCP enabled is invalid."),
      timeoutSeconds: checkedTimeout(record.timeoutSeconds),
      ...(record.bearerTokenSecretName === undefined
        ? {}
        : { bearerTokenSecretName: checkedSecretName(record.bearerTokenSecretName) }),
    };
    return result as CreateMcpServerInput;
  }
  return invalidRequest("MCP transport is invalid.");
}

function checkedPatch(patch: McpServerPatch): McpServerPatch {
  const allowed = [
    "name",
    "transport",
    "command",
    "args",
    "url",
    "enabled",
    "timeoutSeconds",
    "envSecretNames",
    "bearerTokenSecretName",
  ] as const;
  if (!isExactObject(patch, [], allowed)) return invalidRequest("MCP patch is invalid.");
  const record = patch as Record<string, unknown>;
  if (
    ("command" in record || "args" in record || "envSecretNames" in record)
    && ("url" in record || "bearerTokenSecretName" in record)
  ) return invalidRequest("MCP patch cannot mix stdio and remote fields.");
  const result: McpServerPatch = {};
  if ("name" in record) result.name = checkedName(record.name);
  if ("transport" in record) {
    if (typeof record.transport !== "string" || !TRANSPORTS.has(record.transport as McpTransport)) {
      return invalidRequest("MCP transport is invalid.");
    }
    result.transport = record.transport as McpTransport;
  }
  if ("command" in record) result.command = checkedCommand(record.command);
  if ("args" in record) result.args = checkedArgs(record.args);
  if ("url" in record) result.url = checkedRemoteUrl(record.url);
  if ("enabled" in record) {
    if (typeof record.enabled !== "boolean") return invalidRequest("MCP enabled is invalid.");
    result.enabled = record.enabled;
  }
  if ("timeoutSeconds" in record) result.timeoutSeconds = checkedTimeout(record.timeoutSeconds);
  if ("envSecretNames" in record) result.envSecretNames = checkedSecretNames(record.envSecretNames);
  if ("bearerTokenSecretName" in record) {
    result.bearerTokenSecretName = record.bearerTokenSecretName === null
      ? null
      : checkedSecretName(record.bearerTokenSecretName);
  }
  return result;
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

async function jsonPayload(response: Response, context: string): Promise<unknown> {
  const contentType = response.headers.get("content-type")?.toLowerCase() ?? "";
  const expected = response.status >= 400 ? "application/problem+json" : "application/json";
  if (!contentType.includes(expected)) return invalidResponse(`${context} Content-Type`);
  try {
    return await response.json() as unknown;
  } catch {
    return invalidResponse(context);
  }
}

function parseProblemDetails(value: unknown): ProblemDetails {
  const record = asRecord(value, "MCP problem details");
  exactKeys(
    record,
    ["type", "title", "status", "code", "requestId", "retryable"],
    ["detail", "instance"],
    "MCP problem details",
  );
  const optionalText = (field: "detail" | "instance") => (
    !(field in record) || record[field] === null
      ? record[field] as null | undefined
      : stringValue(record[field], `MCP problem details.${field}`)
  );
  return {
    type: stringValue(record.type, "MCP problem details.type"),
    title: stringValue(record.title, "MCP problem details.title"),
    status: integerValue(record.status, 400, 599, "MCP problem details.status"),
    code: stringValue(record.code, "MCP problem details.code"),
    requestId: stringValue(record.requestId, "MCP problem details.requestId"),
    retryable: booleanValue(record.retryable, "MCP problem details.retryable"),
    ...(record.detail !== undefined ? { detail: optionalText("detail") } : {}),
    ...(record.instance !== undefined ? { instance: optionalText("instance") } : {}),
  };
}

async function throwHttpError(response: Response): Promise<never> {
  const problem = parseProblemDetails(await jsonPayload(response, "MCP error response"));
  if (problem.status !== response.status) invalidResponse("MCP problem details.status");
  const etag = optionalStrongEtag(response);
  if (response.status === 409 && !etag) invalidResponse("MCP conflict ETag");
  throw new McpApiError("http", problem.title, {
    status: problem.status,
    code: problem.code,
    requestId: problem.requestId,
    retryable: problem.retryable,
    etag,
  });
}

class DefaultMcpApi implements McpApi {
  constructor(private readonly transport: DesktopTransport) {}

  async listServers(
    profileId: string,
    options: DesktopRequestOptions = {},
  ): Promise<VersionedMcpServers> {
    const response = await this.transport.request(
      `/api/v1/profiles/${checkedProfileId(profileId)}/mcp/servers`,
      { method: "GET", headers: { Accept: "application/json" } },
      options,
    );
    if (response.status !== 200) return throwHttpError(response);
    return {
      value: parseMcpServerList(await jsonPayload(response, "MCP server list")),
      etag: requiredStrongEtag(response, "MCP server list"),
    };
  }

  async createServer(
    profileId: string,
    input: CreateMcpServerInput,
    idempotencyKey: string,
    options: DesktopRequestOptions = {},
  ): Promise<VersionedMcpServer> {
    const response = await this.transport.request(
      `/api/v1/profiles/${checkedProfileId(profileId)}/mcp/servers`,
      {
        method: "POST",
        headers: {
          Accept: "application/json",
          "Content-Type": "application/json",
          "Idempotency-Key": checkedIdempotencyKey(idempotencyKey),
        },
        body: JSON.stringify(checkedCreateInput(input)),
      },
      options,
    );
    if (response.status !== 201) return throwHttpError(response);
    return {
      value: parseMcpServer(await jsonPayload(response, "Created MCP server")),
      etag: requiredStrongEtag(response, "Created MCP server"),
    };
  }

  async updateServer(
    profileId: string,
    serverId: string,
    patch: McpServerPatch,
    etag: string,
    options: DesktopRequestOptions = {},
  ): Promise<VersionedMcpServer> {
    const response = await this.transport.request(
      `/api/v1/profiles/${checkedProfileId(profileId)}/mcp/servers/${checkedServerId(serverId)}`,
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
    const value = parseMcpServer(await jsonPayload(response, "Updated MCP server"));
    if (value.id !== serverId) return invalidResponse("Updated MCP server.id");
    return { value, etag: requiredStrongEtag(response, "Updated MCP server") };
  }

  async deleteServer(
    profileId: string,
    serverId: string,
    etag: string,
    options: DesktopRequestOptions = {},
  ): Promise<DeletedMcpServer> {
    const response = await this.transport.request(
      `/api/v1/profiles/${checkedProfileId(profileId)}/mcp/servers/${checkedServerId(serverId)}`,
      {
        method: "DELETE",
        headers: { Accept: "application/json", "If-Match": checkedStrongEtag(etag) },
      },
      options,
    );
    if (response.status !== 204) return throwHttpError(response);
    return { etag: requiredStrongEtag(response, "Deleted MCP server") };
  }
}

export function createMcpApi(transport: DesktopTransport = desktopTransport): McpApi {
  return new DefaultMcpApi(transport);
}

export const mcpApi = createMcpApi();
