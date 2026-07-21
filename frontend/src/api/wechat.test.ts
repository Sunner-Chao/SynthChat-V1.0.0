import { describe, expect, it, vi } from "vitest";
import type { DesktopTransport } from "./desktopConnection";
import {
  createWechatApi,
  WechatApiError,
  type WechatAccount,
  type WechatConfig,
} from "./wechat";

const ACCOUNT: WechatAccount = {
  id: "wx-1",
  note: "SynthChat",
  online: true,
  createdAt: "2026-07-20T08:00:00Z",
  lastLoginAt: "2026-07-20T08:05:00Z",
  ilinkUserId: "ilink-user-1",
  loginBaseUrl: "https://ilinkai.weixin.qq.com",
  credentialConfigured: true,
  linkedPersonaId: null,
};

const CONFIG: WechatConfig = {
  revision: "wechat-1",
  baseUrl: "https://ilinkai.weixin.qq.com",
  timeoutSeconds: 35,
  accounts: [ACCOUNT],
};

function jsonResponse(value: unknown, status = 200, headers: HeadersInit = {}): Response {
  return new Response(JSON.stringify(value), {
    status,
    headers: {
      "Content-Type": status >= 400 ? "application/problem+json" : "application/json",
      ...Object.fromEntries(new Headers(headers)),
    },
  });
}

describe("WeChat API runtime contract", () => {
  it("uses the exact config routes, ETag, body, and AbortSignal", async () => {
    const requests: Array<{ path: string; init: RequestInit; signal?: AbortSignal }> = [];
    const controller = new AbortController();
    const transport: DesktopTransport = {
      request: vi.fn(async (path, init = {}, options = {}) => {
        requests.push({ path, init, signal: options.signal });
        const updated = { ...CONFIG, revision: "wechat-2", timeoutSeconds: 45 };
        return init.method === "PATCH"
          ? jsonResponse(updated, 200, { ETag: '"wechat-2"' })
          : jsonResponse(CONFIG, 200, { ETag: '"wechat-1"' });
      }),
    };
    const client = createWechatApi(transport);

    await expect(client.getConfig("default", { signal: controller.signal })).resolves.toEqual({
      value: CONFIG,
      etag: '"wechat-1"',
    });
    await expect(client.updateConfig(
      "work_profile",
      { baseUrl: "https://ilinkai.weixin.qq.com", timeoutSeconds: 45 },
      '"wechat-1"',
      { signal: controller.signal },
    )).resolves.toMatchObject({ value: { revision: "wechat-2", timeoutSeconds: 45 }, etag: '"wechat-2"' });

    expect(requests.map(({ path }) => path)).toEqual([
      "/api/v1/profiles/default/wechat",
      "/api/v1/profiles/work_profile/wechat",
    ]);
    expect(requests.every(({ signal }) => signal === controller.signal)).toBe(true);
    const patchHeaders = new Headers(requests[1]!.init.headers);
    expect(patchHeaders.get("If-Match")).toBe('"wechat-1"');
    expect(patchHeaders.get("Content-Type")).toBe("application/merge-patch+json");
    expect(JSON.parse(String(requests[1]!.init.body))).toEqual({
      baseUrl: "https://ilinkai.weixin.qq.com",
      timeoutSeconds: 45,
    });
  });

  it("uses the exact QR routes and returns only the declared response fields", async () => {
    const requests: Array<{ path: string; init: RequestInit }> = [];
    const client = createWechatApi({
      request: vi.fn(async (path, init = {}) => {
        requests.push({ path, init });
        if (path.endsWith("/status")) {
          return jsonResponse({
            status: "confirmed",
            message: null,
            account: ACCOUNT,
            host: "ilinkai.weixin.qq.com",
          });
        }
        return jsonResponse({
          qrcode: "challenge-1",
          qrImage: "data:image/svg+xml;base64,PHN2Zy8+",
          baseUrl: CONFIG.baseUrl,
        });
      }),
    });

    await expect(client.startQr("default", { baseUrl: null })).resolves.toMatchObject({ qrcode: "challenge-1" });
    await expect(client.checkQr("default", {
      qrcode: "challenge-1",
      baseUrl: CONFIG.baseUrl,
    })).resolves.toEqual({
      status: "confirmed",
      message: null,
      account: ACCOUNT,
      host: "ilinkai.weixin.qq.com",
    });

    expect(requests.map(({ path }) => path)).toEqual([
      "/api/v1/profiles/default/wechat/qr",
      "/api/v1/profiles/default/wechat/qr/status",
    ]);
    expect(JSON.parse(String(requests[0]!.init.body))).toEqual({ baseUrl: null });
    expect(JSON.parse(String(requests[1]!.init.body))).toEqual({
      qrcode: "challenge-1",
      baseUrl: CONFIG.baseUrl,
    });
  });

  it("binds a Persona and explicitly polls and sends through the exact account routes", async () => {
    const requests: Array<{ path: string; init: RequestInit; signal?: AbortSignal }> = [];
    const controller = new AbortController();
    const client = createWechatApi({
      request: vi.fn(async (path, init = {}, options = {}) => {
        requests.push({ path, init, signal: options.signal });
        if (path.endsWith("/poll")) {
          return jsonResponse({
            messages: [{
              id: "message-1",
              peer: "peer-1",
              text: "hello",
              upstreamCredential: "must-not-escape",
            }],
            nextCursor: "cursor-2",
            receivedCount: 2,
            skippedCount: 1,
            upstreamPayload: { private: true },
          });
        }
        if (path.endsWith("/messages")) {
          return jsonResponse({ accepted: true, messageId: "sent-1", upstreamPayload: "private" });
        }
        return jsonResponse({
          ...CONFIG,
          revision: "wechat-2",
          accounts: [{ ...ACCOUNT, linkedPersonaId: "persona-1" }],
        }, 200, { ETag: '"wechat-2"' });
      }),
    });

    await expect(client.updateAccountLink(
      "default",
      "wx account/1",
      { linkedPersonaId: "persona-1" },
      '"wechat-1"',
      { signal: controller.signal },
    )).resolves.toMatchObject({
      value: { accounts: [{ linkedPersonaId: "persona-1" }] },
      etag: '"wechat-2"',
    });
    await expect(client.pollMessages(
      "default",
      "wx account/1",
      { cursor: "cursor-1" },
      { signal: controller.signal },
    )).resolves.toEqual({
      messages: [{ id: "message-1", peer: "peer-1", text: "hello" }],
      nextCursor: "cursor-2",
      receivedCount: 2,
      skippedCount: 1,
    });
    await expect(client.sendMessage(
      "default",
      "wx account/1",
      { peer: " peer-1 ", text: " Hello\r\nworld " },
      { signal: controller.signal },
    )).resolves.toEqual({ accepted: true, messageId: "sent-1" });

    expect(requests.map(({ path }) => path)).toEqual([
      "/api/v1/profiles/default/wechat/accounts/wx%20account%2F1",
      "/api/v1/profiles/default/wechat/accounts/wx%20account%2F1/poll",
      "/api/v1/profiles/default/wechat/accounts/wx%20account%2F1/messages",
    ]);
    expect(requests.every(({ signal }) => signal === controller.signal)).toBe(true);
    expect(new Headers(requests[0]!.init.headers).get("If-Match")).toBe('"wechat-1"');
    expect(JSON.parse(String(requests[0]!.init.body))).toEqual({ linkedPersonaId: "persona-1" });
    expect(JSON.parse(String(requests[1]!.init.body))).toEqual({ cursor: "cursor-1" });
    expect(JSON.parse(String(requests[2]!.init.body))).toEqual({ peer: "peer-1", text: "Hello\nworld" });
  });

  it("rejects invalid identifiers and ETags before reaching the transport", async () => {
    const request = vi.fn<DesktopTransport["request"]>();
    const client = createWechatApi({ request });

    await expect(client.getConfig("../escape")).rejects.toBeInstanceOf(WechatApiError);
    await expect(client.updateConfig("default", {}, "weak-etag")).rejects.toBeInstanceOf(WechatApiError);
    await expect(client.pollMessages("default", "wx-1", { cursor: "bad\ncursor" })).rejects.toBeInstanceOf(WechatApiError);
    await expect(client.sendMessage("default", "wx-1", { peer: "", text: "hello" })).rejects.toBeInstanceOf(WechatApiError);
    await expect(client.updateAccountLink("default", "wx-1", { linkedPersonaId: "" }, '"wechat-1"')).rejects.toBeInstanceOf(WechatApiError);
    expect(request).not.toHaveBeenCalled();
  });

  it("rejects malformed success payloads and preserves sanitized problem details", async () => {
    const malformed = createWechatApi({
      request: vi.fn(async () => jsonResponse({ ...CONFIG, timeoutSeconds: 1.5 }, 200, { ETag: '"wechat-1"' })),
    });
    await expect(malformed.getConfig("default")).rejects.toBeInstanceOf(WechatApiError);

    const failed = createWechatApi({
      request: vi.fn(async () => jsonResponse({
        detail: "iLink is temporarily unavailable.",
        code: "wechat_upstream_unavailable",
        retryable: true,
      }, 502)),
    });
    await expect(failed.startQr("default", { baseUrl: null })).rejects.toMatchObject({
      message: "iLink is temporarily unavailable.",
      status: 502,
      code: "wechat_upstream_unavailable",
      retryable: true,
    });
  });
});
