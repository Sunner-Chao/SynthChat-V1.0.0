import { describe, expect, it, vi } from "vitest";
import type { DesktopTransport } from "./desktopConnection";
import {
  createWebApi,
  parseEffectiveWebProvider,
  parseWebConfig,
  parseWebProvider,
  parseWebProviderList,
  WebApiError,
  type WebConfig,
  type WebProvider,
} from "./web";

const PROVIDER: WebProvider = {
  id: "tavily",
  displayName: "Tavily",
  supportsSearch: true,
  supportsExtract: true,
  secretNames: ["TAVILY_API_KEY"],
  defaultBaseUrl: "https://api.tavily.com",
  customEndpointSupported: false,
};

const RUNTIME_PROVIDER: WebProvider = {
  ...PROVIDER,
  defaultBaseUrl: "https://web-gateway.example.test/providers/tavily",
};

const CONFIG: WebConfig = {
  revision: "config-1",
  sharedProvider: "tavily",
  searchProvider: null,
  extractProvider: null,
  extractCharLimit: 15_000,
  effectiveSearch: {
    providerId: "tavily",
    status: "ready",
    missingSecretNames: [],
  },
  effectiveExtract: {
    providerId: "tavily",
    status: "missingSecret",
    missingSecretNames: ["TAVILY_API_KEY"],
  },
};

const PROBLEM = {
  type: "about:blank",
  title: "Configuration changed",
  status: 409,
  code: "revision_conflict",
  requestId: "req-web-1",
  retryable: false,
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

function invalidResponse(block: () => unknown): void {
  expect(block).toThrowError(
    expect.objectContaining<Partial<WebApiError>>({ kind: "invalid_response" }),
  );
}

describe("Web API runtime contract", () => {
  it("strictly parses the Tavily catalog entry and rejects malformed providers", () => {
    expect(parseWebProvider(PROVIDER)).toEqual(PROVIDER);
    expect(parseWebProvider(RUNTIME_PROVIDER)).toEqual(RUNTIME_PROVIDER);
    expect(parseWebProviderList([RUNTIME_PROVIDER])).toEqual([RUNTIME_PROVIDER]);

    for (const payload of [
      { ...PROVIDER, id: "exa" },
      { ...PROVIDER, displayName: "tavily" },
      { ...PROVIDER, supportsExtract: false },
      { ...PROVIDER, secretNames: [] },
      { ...PROVIDER, secretNames: ["bad-key"] },
      { ...PROVIDER, defaultBaseUrl: "http://web-gateway.example.test" },
      { ...PROVIDER, defaultBaseUrl: "https://user@example.com" },
      { ...PROVIDER, defaultBaseUrl: "https://example.com/path?mode=search" },
      { ...PROVIDER, defaultBaseUrl: "https://example.com/path#search" },
      { ...PROVIDER, defaultBaseUrl: "javascript:alert(1)" },
      { ...PROVIDER, customEndpointSupported: true },
      { ...PROVIDER, extra: true },
      (({ displayName: _displayName, ...rest }) => rest)(PROVIDER),
    ]) invalidResponse(() => parseWebProvider(payload));

    invalidResponse(() => parseWebProviderList([PROVIDER, PROVIDER]));
    invalidResponse(() => parseWebProviderList({ items: [PROVIDER] }));
  });

  it("strictly parses independent readiness and all accepted statuses", () => {
    expect(parseEffectiveWebProvider(CONFIG.effectiveExtract)).toEqual(CONFIG.effectiveExtract);
    expect(parseEffectiveWebProvider({
      providerId: null,
      status: "capabilityUnsupported",
      missingSecretNames: [],
    })).toMatchObject({ status: "capabilityUnsupported" });

    for (const payload of [
      { ...CONFIG.effectiveSearch, status: "offline" },
      { ...CONFIG.effectiveSearch, providerId: "" },
      { ...CONFIG.effectiveSearch, providerId: "x".repeat(129) },
      { ...CONFIG.effectiveSearch, missingSecretNames: ["bad-key"] },
      { ...CONFIG.effectiveSearch, missingSecretNames: Array(9).fill("TAVILY_API_KEY") },
      { ...CONFIG.effectiveSearch, extra: true },
      (({ status: _status, ...rest }) => rest)(CONFIG.effectiveSearch),
    ]) invalidResponse(() => parseEffectiveWebProvider(payload));
  });

  it("strictly parses Web config bounds, revision, and exact keys", () => {
    expect(parseWebConfig(CONFIG)).toEqual(CONFIG);
    for (const payload of [
      { ...CONFIG, revision: "" },
      { ...CONFIG, sharedProvider: 1 },
      { ...CONFIG, searchProvider: "x".repeat(129) },
      { ...CONFIG, extractCharLimit: 1_999 },
      { ...CONFIG, extractCharLimit: 500_001 },
      { ...CONFIG, extractCharLimit: 2_000.5 },
      { ...CONFIG, effectiveSearch: { ...CONFIG.effectiveSearch, status: "unknown" } },
      { ...CONFIG, extra: true },
      (({ effectiveExtract: _effectiveExtract, ...rest }) => rest)(CONFIG),
    ]) invalidResponse(() => parseWebConfig(payload));
  });

  it("uses exact GET paths and forwards AbortSignal", async () => {
    const requests: Array<{ path: string; init: RequestInit; signal?: AbortSignal }> = [];
    const controller = new AbortController();
    const client = createWebApi({
      request: vi.fn(async (path, init = {}, options = {}) => {
        requests.push({ path, init, signal: options.signal });
        return path === "/api/v1/web/providers"
          ? jsonResponse([PROVIDER])
          : jsonResponse(CONFIG, 200, { ETag: '"config-1"' });
      }),
    });

    await expect(client.listProviders({ signal: controller.signal })).resolves.toEqual([PROVIDER]);
    await expect(client.getWebConfig("default", { signal: controller.signal })).resolves.toEqual({
      value: CONFIG,
      etag: '"config-1"',
    });

    expect(requests.map(({ path }) => path)).toEqual([
      "/api/v1/web/providers",
      "/api/v1/profiles/default/web",
    ]);
    expect(new Headers(requests[0]!.init.headers).get("Accept")).toBe("application/json");
    expect(requests[0]!.signal).toBe(controller.signal);
    expect(requests[1]!.signal).toBe(controller.signal);
  });

  it("sends an exact merge patch with the current strong ETag", async () => {
    const requests: Array<{ path: string; init: RequestInit; signal?: AbortSignal }> = [];
    const controller = new AbortController();
    const updated = { ...CONFIG, revision: "config-2", extractCharLimit: 20_000 };
    const client = createWebApi({
      request: vi.fn(async (path, init = {}, options = {}) => {
        requests.push({ path, init, signal: options.signal });
        return jsonResponse(updated, 200, { ETag: '"config-2"' });
      }),
    });

    await expect(client.updateWebConfig(
      "work_profile",
      { searchProvider: null, extractCharLimit: 20_000 },
      '"config-1"',
      { signal: controller.signal },
    )).resolves.toEqual({ value: updated, etag: '"config-2"' });

    expect(requests[0]!.path).toBe("/api/v1/profiles/work_profile/web");
    const headers = new Headers(requests[0]!.init.headers);
    expect(headers.get("Content-Type")).toBe("application/merge-patch+json");
    expect(headers.get("If-Match")).toBe('"config-1"');
    expect(requests[0]!.init.body).toBe('{"searchProvider":null,"extractCharLimit":20000}');
    expect(requests[0]!.signal).toBe(controller.signal);
  });

  it("requires response ETag to be strong and equal to the body revision", async () => {
    for (const etag of [undefined, 'W/"config-1"', '"different"']) {
      const client = createWebApi({
        request: vi.fn(async () => jsonResponse(CONFIG, 200, etag ? { ETag: etag } : {})),
      });
      await expect(client.getWebConfig("default")).rejects.toMatchObject({
        kind: "invalid_response",
      });
    }
  });

  it("preserves the current ETag on 409 and requires problem+json", async () => {
    const client = createWebApi({
      request: vi.fn(async () => jsonResponse(PROBLEM, 409, { ETag: '"config-current"' })),
    });
    await expect(client.updateWebConfig(
      "default",
      { sharedProvider: "tavily" },
      '"config-stale"',
    )).rejects.toMatchObject({
      kind: "http",
      status: 409,
      code: "revision_conflict",
      requestId: "req-web-1",
      retryable: false,
      etag: '"config-current"',
    });

    for (const response of [
      jsonResponse(PROBLEM, 409),
      jsonResponse(PROBLEM, 409, { ETag: 'W/"config-current"' }),
      jsonResponse(PROBLEM, 409, {
        ETag: '"config-current"',
        "Content-Type": "application/json",
      }),
      jsonResponse({ ...PROBLEM, status: 400 }, 409, { ETag: '"config-current"' }),
      jsonResponse({ ...PROBLEM, extra: true }, 409, { ETag: '"config-current"' }),
    ]) {
      const malformed = createWebApi({ request: vi.fn(async () => response) });
      await expect(malformed.updateWebConfig(
        "default",
        { sharedProvider: null },
        '"config-stale"',
      )).rejects.toMatchObject({ kind: "invalid_response" });
    }
  });

  it("rejects invalid requests before invoking the transport", async () => {
    const transport: DesktopTransport = {
      request: vi.fn(async () => jsonResponse(CONFIG, 200, { ETag: '"config-1"' })),
    };
    const client = createWebApi(transport);
    const invalidCalls: Array<Promise<unknown>> = [
      client.getWebConfig("../escape"),
      client.updateWebConfig("default", { sharedProvider: "exa" } as never, '"config-1"'),
      client.updateWebConfig("default", { extractProvider: undefined } as never, '"config-1"'),
      client.updateWebConfig("default", { extractCharLimit: 1_999 }, '"config-1"'),
      client.updateWebConfig("default", { extractCharLimit: 2_000.5 }, '"config-1"'),
      client.updateWebConfig("default", { extra: true } as never, '"config-1"'),
      client.updateWebConfig("default", { sharedProvider: null }, 'W/"config-1"'),
    ];
    for (const call of invalidCalls) {
      await expect(call).rejects.toMatchObject({ kind: "invalid_request" });
    }
    expect(transport.request).not.toHaveBeenCalled();
  });
});
