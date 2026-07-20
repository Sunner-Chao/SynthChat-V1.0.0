import { afterEach, describe, expect, it, vi } from "vitest";
import {
  BACKEND_SERVICE_NAME,
  BackendApiError,
  DEFAULT_BACKEND_BASE_URL,
  createBackendApiClient,
  normalizeBackendBaseUrl,
  parseBackendHealth,
  type BackendFetch,
  type BackendHealth,
} from "./backend";

const HEALTH: BackendHealth = {
  status: "ok",
  service: BACKEND_SERVICE_NAME,
  version: "0.1.0",
};

function jsonResponse(value: unknown, status = 200): Response {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
  delete (globalThis as typeof globalThis & { __SYNTHCHAT_BACKEND_URL__?: unknown })
    .__SYNTHCHAT_BACKEND_URL__;
});

describe("backend health client", () => {
  it("uses the local default URL and validates the health payload", async () => {
    let requestedUrl = "";
    let requestedInit: RequestInit | undefined;
    const fetchImpl: BackendFetch = async (input, init) => {
      requestedUrl = String(input);
      requestedInit = init;
      return jsonResponse(HEALTH);
    };

    const client = createBackendApiClient({ fetch: fetchImpl });
    await expect(client.getHealth()).resolves.toEqual(HEALTH);
    expect(client.baseUrl).toBe(DEFAULT_BACKEND_BASE_URL);
    expect(requestedUrl).toBe(`${DEFAULT_BACKEND_BASE_URL}/health`);
    expect(requestedInit).toMatchObject({
      method: "GET",
      cache: "no-store",
      credentials: "omit",
      redirect: "error",
    });
    expect(requestedInit?.signal).toBeInstanceOf(AbortSignal);
  });

  it("supports an explicit loopback base path and removes trailing slashes", async () => {
    let requestedUrl = "";
    const fetchImpl: BackendFetch = async (input) => {
      requestedUrl = String(input);
      return jsonResponse(HEALTH);
    };
    const client = createBackendApiClient({
      baseUrl: "http://localhost:9123/local-api///",
      fetch: fetchImpl,
    });

    await client.getHealth();
    expect(client.baseUrl).toBe("http://localhost:9123/local-api");
    expect(requestedUrl).toBe("http://localhost:9123/local-api/health");
    expect(normalizeBackendBaseUrl("http://[::1]:8642/")).toBe(
      "http://[::1]:8642",
    );
  });

  it("rejects remote hosts before a request can be sent", () => {
    expect(() => createBackendApiClient({ baseUrl: "http://192.168.1.20:8642" }))
      .toThrowError(expect.objectContaining<Partial<BackendApiError>>({
        kind: "configuration",
      }));
    expect(() => createBackendApiClient({ baseUrl: "https://example.com" }))
      .toThrowError(expect.objectContaining<Partial<BackendApiError>>({
        kind: "configuration",
      }));
  });

  it.each([
    "not a URL",
    "ftp://localhost:8642",
    "http://user:pass@localhost:8642",
    "http://localhost:8642?debug=true",
    "http://localhost:8642#debug",
  ])("rejects an unsafe configured backend URL: %s", (baseUrl) => {
    expect(() => createBackendApiClient({ baseUrl })).toThrowError(
      expect.objectContaining<Partial<BackendApiError>>({ kind: "configuration" }),
    );
  });

  it("uses a non-empty runtime loopback URL", async () => {
    (globalThis as typeof globalThis & { __SYNTHCHAT_BACKEND_URL__?: unknown })
      .__SYNTHCHAT_BACKEND_URL__ = "http://localhost:9124/runtime";
    const client = createBackendApiClient({ fetch: async () => jsonResponse(HEALTH) });

    expect(client.baseUrl).toBe("http://localhost:9124/runtime");
    await expect(client.getHealth()).resolves.toEqual(HEALTH);
  });

  it.each([0, -1, Number.NaN, Number.POSITIVE_INFINITY])(
    "rejects an invalid default timeout: %s",
    (timeoutMs) => {
      expect(() => createBackendApiClient({ timeoutMs })).toThrowError(
        expect.objectContaining<Partial<BackendApiError>>({ kind: "configuration" }),
      );
    },
  );

  it.each([
    null,
    {},
    { status: "ok", service: BACKEND_SERVICE_NAME },
    { ...HEALTH, status: "degraded" },
    { ...HEALTH, service: "hermes-agent" },
    { ...HEALTH, version: 1 },
    { ...HEALTH, extra: true },
  ])("rejects a health payload outside the strict schema: %j", (payload) => {
    expect(() => parseBackendHealth(payload)).toThrowError(
      expect.objectContaining<Partial<BackendApiError>>({ kind: "invalid_response" }),
    );
  });

  it("reports non-200 responses as HTTP errors", async () => {
    const client = createBackendApiClient({
      fetch: async () => jsonResponse({ error: "starting" }, 503),
    });

    await expect(client.getHealth()).rejects.toMatchObject({
      kind: "http",
      status: 503,
    });
  });

  it("aborts a request when its timeout expires", async () => {
    vi.useFakeTimers();
    const fetchImpl: BackendFetch = (_input, init) => new Promise((_resolve, reject) => {
      init?.signal?.addEventListener(
        "abort",
        () => reject(new Error("request aborted")),
        { once: true },
      );
    });
    const client = createBackendApiClient({ fetch: fetchImpl });

    const request = client.getHealth({ timeoutMs: 25 });
    const assertion = expect(request).rejects.toMatchObject({ kind: "timeout" });
    await vi.advanceTimersByTimeAsync(25);
    await assertion;
  });

  it("honors a caller AbortSignal separately from timeout", async () => {
    const fetchImpl: BackendFetch = (_input, init) => new Promise((_resolve, reject) => {
      init?.signal?.addEventListener(
        "abort",
        () => reject(new Error("request aborted")),
        { once: true },
      );
    });
    const client = createBackendApiClient({ fetch: fetchImpl });
    const controller = new AbortController();

    const request = client.getHealth({ signal: controller.signal });
    controller.abort();
    await expect(request).rejects.toMatchObject({ kind: "aborted" });
  });

  it("rejects a request whose caller signal is already aborted", async () => {
    const fetchImpl = vi.fn<BackendFetch>(async () => jsonResponse(HEALTH));
    const controller = new AbortController();
    controller.abort();

    await expect(createBackendApiClient({ fetch: fetchImpl }).getHealth({
      signal: controller.signal,
    })).rejects.toMatchObject({ kind: "aborted" });
    expect(fetchImpl).not.toHaveBeenCalled();
  });

  it("checks caller cancellation after fetch resolves", async () => {
    const controller = new AbortController();
    const client = createBackendApiClient({
      fetch: async () => {
        controller.abort();
        return jsonResponse(HEALTH);
      },
    });

    await expect(client.getHealth({ signal: controller.signal })).rejects.toMatchObject({
      kind: "aborted",
    });
  });

  it("checks timeout state when a late fetch resolves", async () => {
    vi.useFakeTimers();
    let resolveFetch: ((response: Response) => void) | undefined;
    const client = createBackendApiClient({
      fetch: () => new Promise<Response>((resolve) => {
        resolveFetch = resolve;
      }),
    });

    const request = client.getHealth({ timeoutMs: 25 });
    await vi.advanceTimersByTimeAsync(25);
    resolveFetch?.(jsonResponse(HEALTH));

    await expect(request).rejects.toMatchObject({ kind: "timeout" });
  });

  it("maps malformed JSON and raw fetch failures", async () => {
    const malformed = createBackendApiClient({
      fetch: async () => new Response("{", {
        status: 200,
        headers: { "Content-Type": "application/json" },
      }),
    });
    await expect(malformed.getHealth()).rejects.toMatchObject({ kind: "invalid_response" });

    const offline = createBackendApiClient({
      fetch: async () => {
        throw new TypeError("connection refused");
      },
    });
    await expect(offline.getHealth()).rejects.toMatchObject({ kind: "network" });
  });

  it("uses global fetch when no override is provided", async () => {
    const fetchImpl = vi.fn(async () => jsonResponse(HEALTH));
    vi.stubGlobal("fetch", fetchImpl);

    await expect(createBackendApiClient().getHealth()).resolves.toEqual(HEALTH);
    expect(fetchImpl).toHaveBeenCalledOnce();
  });
});
