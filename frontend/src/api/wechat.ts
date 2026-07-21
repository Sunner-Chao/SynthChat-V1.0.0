import type { components } from "./generated/openapi";
import {
  desktopTransport,
  type DesktopRequestOptions,
  type DesktopTransport,
} from "./desktopConnection";

export type WechatAccount = components["schemas"]["WechatAccount"] & {
  linkedPersonaId: string | null;
};
export interface WechatConfig extends Omit<components["schemas"]["WechatConfig"], "accounts"> {
  accounts: WechatAccount[];
}
export type WechatConfigPatch = components["schemas"]["WechatConfigPatch"];
export type WechatQrStartRequest = components["schemas"]["WechatQrStartRequest"];
export type WechatQrStartResult = components["schemas"]["WechatQrStartResult"];
export type WechatQrStatusRequest = components["schemas"]["WechatQrStatusRequest"];
export interface WechatQrStatusResult extends Omit<components["schemas"]["WechatQrStatusResult"], "account"> {
  account: WechatAccount | null;
}

export interface WechatAccountLinkPatch {
  linkedPersonaId: string | null;
}

export interface WechatPollRequest {
  cursor?: string | null;
}

export interface WechatInboundMessage {
  id: string;
  peer: string;
  text: string;
}

export interface WechatPollResult {
  messages: WechatInboundMessage[];
  nextCursor: string | null;
  receivedCount: number;
  skippedCount: number;
}

export interface WechatSendRequest {
  peer: string;
  text: string;
}

export interface WechatSendResult {
  accepted: boolean;
  messageId: string | null;
}

export interface VersionedWechatConfig {
  value: WechatConfig;
  etag: string;
}

export class WechatApiError extends Error {
  readonly status?: number;
  readonly code?: string;
  readonly retryable: boolean;

  constructor(message: string, options: { status?: number; code?: string; retryable?: boolean } = {}) {
    super(message);
    this.name = "WechatApiError";
    this.status = options.status;
    this.code = options.code;
    this.retryable = options.retryable ?? false;
  }
}

export interface WechatApi {
  getConfig(profileId: string, options?: DesktopRequestOptions): Promise<VersionedWechatConfig>;
  updateConfig(profileId: string, patch: WechatConfigPatch, etag: string, options?: DesktopRequestOptions): Promise<VersionedWechatConfig>;
  startQr(profileId: string, request: WechatQrStartRequest, options?: DesktopRequestOptions): Promise<WechatQrStartResult>;
  checkQr(profileId: string, request: WechatQrStatusRequest, options?: DesktopRequestOptions): Promise<WechatQrStatusResult>;
  updateAccountLink(profileId: string, accountId: string, patch: WechatAccountLinkPatch, etag: string, options?: DesktopRequestOptions): Promise<VersionedWechatConfig>;
  pollMessages(profileId: string, accountId: string, request: WechatPollRequest, options?: DesktopRequestOptions): Promise<WechatPollResult>;
  sendMessage(profileId: string, accountId: string, request: WechatSendRequest, options?: DesktopRequestOptions): Promise<WechatSendResult>;
}

const PROFILE_ID = /^(?:default|[a-z0-9_][a-z0-9_-]{0,63})$/u;
const ETAG = /^"[\x21\x23-\x7e]{1,126}"$/u;
const CONTROL_CHARACTER = /[\u0000-\u001f\u007f]/u;
const INVALID_MESSAGE_CHARACTER = /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/u;
const MAX_IDENTIFIER_CHARS = 256;
const MAX_CURSOR_BYTES = 16 * 1024;
const MAX_MESSAGE_CHARS = 16_000;
const MAX_POLL_MESSAGES = 100;

function checkedProfileId(value: string): string {
  if (!PROFILE_ID.test(value)) throw new WechatApiError("Profile ID is invalid.");
  return encodeURIComponent(value);
}

function checkedEtag(value: string): string {
  if (!ETAG.test(value)) throw new WechatApiError("WeChat configuration ETag is invalid.");
  return value;
}

function checkedIdentifier(value: string, context: string): string {
  const result = value.trim();
  if (!result || Array.from(result).length > MAX_IDENTIFIER_CHARS || CONTROL_CHARACTER.test(result)) {
    throw new WechatApiError(`${context} is invalid.`);
  }
  return result;
}

function checkedPathIdentifier(value: string, context: string): string {
  return encodeURIComponent(checkedIdentifier(value, context));
}

function checkedCursor(value: string | null | undefined): string | null {
  if (value === undefined || value === null || value === "") return null;
  if (new TextEncoder().encode(value).byteLength > MAX_CURSOR_BYTES || CONTROL_CHARACTER.test(value)) {
    throw new WechatApiError("WeChat message cursor is invalid.");
  }
  return value;
}

function checkedMessageText(value: string): string {
  const result = value.replace(/\r\n?/gu, "\n").trim();
  if (!result || Array.from(result).length > MAX_MESSAGE_CHARS || INVALID_MESSAGE_CHARACTER.test(result)) {
    throw new WechatApiError("WeChat message text is invalid.");
  }
  return result;
}

function record(value: unknown, context: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new WechatApiError(`${context} did not match the API contract.`);
  }
  return value as Record<string, unknown>;
}

async function json(response: Response, context: string): Promise<unknown> {
  try {
    return await response.json();
  } catch {
    throw new WechatApiError(`${context} was not valid JSON.`, { status: response.status });
  }
}

function text(value: unknown, context: string): string {
  if (typeof value !== "string") throw new WechatApiError(`${context} is invalid.`);
  return value;
}

function nullableText(value: unknown, context: string): string | null {
  return value === null ? null : text(value, context);
}

function boundedText(value: unknown, context: string, maximum: number): string {
  const result = text(value, context);
  if (!result || Array.from(result).length > maximum || CONTROL_CHARACTER.test(result)) {
    throw new WechatApiError(`${context} is invalid.`);
  }
  return result;
}

function boundedMessageText(value: unknown, context: string): string {
  const result = text(value, context);
  if (!result || Array.from(result).length > MAX_MESSAGE_CHARS || INVALID_MESSAGE_CHARACTER.test(result)) {
    throw new WechatApiError(`${context} is invalid.`);
  }
  return result;
}

function nonNegativeInteger(value: unknown, context: string): number {
  if (!Number.isSafeInteger(value) || (value as number) < 0) {
    throw new WechatApiError(`${context} is invalid.`);
  }
  return value as number;
}

function account(value: unknown): WechatAccount {
  const item = record(value, "WeChat account");
  if (
    typeof item.online !== "boolean"
    || typeof item.credentialConfigured !== "boolean"
  ) throw new WechatApiError("WeChat account did not match the API contract.");
  return {
    id: text(item.id, "WeChat account.id"),
    note: text(item.note, "WeChat account.note"),
    online: item.online,
    createdAt: text(item.createdAt, "WeChat account.createdAt"),
    lastLoginAt: text(item.lastLoginAt, "WeChat account.lastLoginAt"),
    ilinkUserId: text(item.ilinkUserId, "WeChat account.ilinkUserId"),
    loginBaseUrl: text(item.loginBaseUrl, "WeChat account.loginBaseUrl"),
    credentialConfigured: item.credentialConfigured,
    linkedPersonaId: nullableText(item.linkedPersonaId, "WeChat account.linkedPersonaId"),
  };
}

function config(value: unknown): WechatConfig {
  const item = record(value, "WeChat configuration");
  if (!Number.isInteger(item.timeoutSeconds) || !Array.isArray(item.accounts)) {
    throw new WechatApiError("WeChat configuration did not match the API contract.");
  }
  return {
    revision: text(item.revision, "WeChat configuration.revision"),
    baseUrl: text(item.baseUrl, "WeChat configuration.baseUrl"),
    timeoutSeconds: item.timeoutSeconds as number,
    accounts: item.accounts.map(account),
  };
}

function startResult(value: unknown): WechatQrStartResult {
  const item = record(value, "WeChat QR response");
  return {
    qrcode: text(item.qrcode, "WeChat QR response.qrcode"),
    qrImage: text(item.qrImage, "WeChat QR response.qrImage"),
    baseUrl: text(item.baseUrl, "WeChat QR response.baseUrl"),
  };
}

function statusResult(value: unknown): WechatQrStatusResult {
  const item = record(value, "WeChat QR status");
  return {
    status: text(item.status, "WeChat QR status.status"),
    message: nullableText(item.message, "WeChat QR status.message"),
    account: item.account === null ? null : account(item.account),
    host: nullableText(item.host, "WeChat QR status.host"),
  };
}

function pollResult(value: unknown): WechatPollResult {
  const item = record(value, "WeChat message poll response");
  if (!Array.isArray(item.messages) || item.messages.length > MAX_POLL_MESSAGES) {
    throw new WechatApiError("WeChat message poll response did not match the API contract.");
  }
  const messages = item.messages.map((value): WechatInboundMessage => {
    const message = record(value, "WeChat inbound message");
    return {
      id: boundedText(message.id, "WeChat inbound message.id", MAX_IDENTIFIER_CHARS),
      peer: boundedText(message.peer, "WeChat inbound message.peer", MAX_IDENTIFIER_CHARS),
      text: boundedMessageText(message.text, "WeChat inbound message.text"),
    };
  });
  const receivedCount = nonNegativeInteger(item.receivedCount, "WeChat message poll response.receivedCount");
  const skippedCount = nonNegativeInteger(item.skippedCount, "WeChat message poll response.skippedCount");
  if (
    receivedCount > MAX_POLL_MESSAGES
    || skippedCount > receivedCount
    || messages.length !== receivedCount - skippedCount
  ) {
    throw new WechatApiError("WeChat message poll response counts are invalid.");
  }
  const nextCursor = nullableText(item.nextCursor, "WeChat message poll response.nextCursor");
  checkedCursor(nextCursor);
  return { messages, nextCursor, receivedCount, skippedCount };
}

function sendResult(value: unknown): WechatSendResult {
  const item = record(value, "WeChat message send response");
  if (typeof item.accepted !== "boolean") {
    throw new WechatApiError("WeChat message send response did not match the API contract.");
  }
  const messageId = nullableText(item.messageId, "WeChat message send response.messageId");
  if (messageId !== null) checkedIdentifier(messageId, "WeChat message send response.messageId");
  return { accepted: item.accepted, messageId };
}

async function expectOk<T>(response: Response, parse: (value: unknown) => T): Promise<T> {
  if (response.status === 200) return parse(await json(response, "WeChat response"));
  const payload = record(await json(response, "WeChat error"), "WeChat error");
  throw new WechatApiError(
    typeof payload.detail === "string" ? payload.detail : "WeChat request failed.",
    {
      status: response.status,
      code: typeof payload.code === "string" ? payload.code : undefined,
      retryable: payload.retryable === true,
    },
  );
}

class DefaultWechatApi implements WechatApi {
  constructor(private readonly transport: DesktopTransport) {}

  async getConfig(profileId: string, options: DesktopRequestOptions = {}): Promise<VersionedWechatConfig> {
    const response = await this.transport.request(`/api/v1/profiles/${checkedProfileId(profileId)}/wechat`, {
      method: "GET",
      headers: { Accept: "application/json" },
    }, options);
    const value = await expectOk(response, config);
    const etag = response.headers.get("etag") ?? "";
    return { value, etag: checkedEtag(etag) };
  }

  async updateConfig(profileId: string, patch: WechatConfigPatch, etag: string, options: DesktopRequestOptions = {}): Promise<VersionedWechatConfig> {
    const response = await this.transport.request(`/api/v1/profiles/${checkedProfileId(profileId)}/wechat`, {
      method: "PATCH",
      headers: {
        Accept: "application/json",
        "Content-Type": "application/merge-patch+json",
        "If-Match": checkedEtag(etag),
      },
      body: JSON.stringify(patch),
    }, options);
    const value = await expectOk(response, config);
    return { value, etag: checkedEtag(response.headers.get("etag") ?? "") };
  }

  async startQr(profileId: string, request: WechatQrStartRequest, options: DesktopRequestOptions = {}): Promise<WechatQrStartResult> {
    const response = await this.transport.request(`/api/v1/profiles/${checkedProfileId(profileId)}/wechat/qr`, {
      method: "POST",
      headers: { Accept: "application/json", "Content-Type": "application/json" },
      body: JSON.stringify(request),
    }, options);
    return expectOk(response, startResult);
  }

  async checkQr(profileId: string, request: WechatQrStatusRequest, options: DesktopRequestOptions = {}): Promise<WechatQrStatusResult> {
    const response = await this.transport.request(`/api/v1/profiles/${checkedProfileId(profileId)}/wechat/qr/status`, {
      method: "POST",
      headers: { Accept: "application/json", "Content-Type": "application/json" },
      body: JSON.stringify(request),
    }, options);
    return expectOk(response, statusResult);
  }

  async updateAccountLink(profileId: string, accountId: string, patch: WechatAccountLinkPatch, etag: string, options: DesktopRequestOptions = {}): Promise<VersionedWechatConfig> {
    const linkedPersonaId = patch.linkedPersonaId === null
      ? null
      : checkedIdentifier(patch.linkedPersonaId, "Persona ID");
    const response = await this.transport.request(`/api/v1/profiles/${checkedProfileId(profileId)}/wechat/accounts/${checkedPathIdentifier(accountId, "WeChat account ID")}`, {
      method: "PATCH",
      headers: {
        Accept: "application/json",
        "Content-Type": "application/json",
        "If-Match": checkedEtag(etag),
      },
      body: JSON.stringify({ linkedPersonaId }),
    }, options);
    const value = await expectOk(response, config);
    return { value, etag: checkedEtag(response.headers.get("etag") ?? "") };
  }

  async pollMessages(profileId: string, accountId: string, request: WechatPollRequest, options: DesktopRequestOptions = {}): Promise<WechatPollResult> {
    const cursor = checkedCursor(request.cursor);
    const response = await this.transport.request(`/api/v1/profiles/${checkedProfileId(profileId)}/wechat/accounts/${checkedPathIdentifier(accountId, "WeChat account ID")}/poll`, {
      method: "POST",
      headers: { Accept: "application/json", "Content-Type": "application/json" },
      body: JSON.stringify(cursor === null ? {} : { cursor }),
    }, options);
    return expectOk(response, pollResult);
  }

  async sendMessage(profileId: string, accountId: string, request: WechatSendRequest, options: DesktopRequestOptions = {}): Promise<WechatSendResult> {
    const body: WechatSendRequest = {
      peer: checkedIdentifier(request.peer, "WeChat peer ID"),
      text: checkedMessageText(request.text),
    };
    const response = await this.transport.request(`/api/v1/profiles/${checkedProfileId(profileId)}/wechat/accounts/${checkedPathIdentifier(accountId, "WeChat account ID")}/messages`, {
      method: "POST",
      headers: { Accept: "application/json", "Content-Type": "application/json" },
      body: JSON.stringify(body),
    }, options);
    return expectOk(response, sendResult);
  }
}

export function createWechatApi(transport: DesktopTransport = desktopTransport): WechatApi {
  return new DefaultWechatApi(transport);
}

export const wechatApi = createWechatApi();
