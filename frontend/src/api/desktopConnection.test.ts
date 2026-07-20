import { describe, expect, it, vi } from "vitest";
import {
  createDesktopBackendApiClient,
  createDesktopTransport,
  desktopTransport,
  DesktopConnectionError,
} from "./desktopConnection";

const TOKEN_A = "a".repeat(64);
const TOKEN_B = "b".repeat(64);

describe("desktop connection transport", () => {
  it("checks health at the runtime-reported backend address without sending the token", async () => {
    let requestedUrl = "";
    let requestedInit: RequestInit | undefined;
    const connect = vi.fn(async () => ({
      baseUrl: "http://127.0.0.1:49152",
      token: TOKEN_A,
    }));
    const client = createDesktopBackendApiClient({
      connect,
      fetch: async (input, init) => {
        requestedUrl = String(input);
        requestedInit = init;
        return new Response(JSON.stringify({
          status: "ok",
          service: "synthchat-hermes-backend",
          version: "0.1.0",
        }), { status: 200, headers: { "Content-Type": "application/json" } });
      },
    });

    await expect(client.getHealth()).resolves.toMatchObject({
      status: "ok",
      version: "0.1.0",
    });
    expect(connect).toHaveBeenCalledOnce();
    expect(requestedUrl).toBe("http://127.0.0.1:49152/health");
    expect(new Headers(requestedInit?.headers).has("Authorization")).toBe(false);
  });

  it("keeps the token behind a request-only transport", async () => {
    let requestedUrl = "";
    let requestedInit: RequestInit | undefined;
    const transport = createDesktopTransport({
      connect: async () => ({ baseUrl: "http://127.0.0.1:8642", token: TOKEN_A }),
      fetch: async (input, init) => {
        requestedUrl = String(input);
        requestedInit = init;
        return new Response("{}", { status: 200 });
      },
    });

    await transport.request("/api/v1/capabilities", { method: "GET" });

    expect(Object.keys(transport)).toEqual(["request"]);
    expect(requestedUrl).toBe("http://127.0.0.1:8642/api/v1/capabilities");
    expect(new Headers(requestedInit?.headers).get("Authorization")).toBe(`Bearer ${TOKEN_A}`);
    expect(requestedInit).toMatchObject({
      cache: "no-store",
      credentials: "omit",
      redirect: "error",
    });
  });

  it("performs at most one fresh handshake after a 401", async () => {
    const connect = vi.fn(async () => ({
      baseUrl: "http://127.0.0.1:8642",
      token: connect.mock.calls.length === 1 ? TOKEN_A : TOKEN_B,
    }));
    const seenTokens: Array<string | null> = [];
    const transport = createDesktopTransport({
      connect,
      fetch: async (_input, init) => {
        seenTokens.push(new Headers(init?.headers).get("Authorization"));
        return new Response(JSON.stringify({ title: "unauthorized" }), {
          status: 401,
          headers: { "Content-Type": "application/problem+json" },
        });
      },
    });

    const response = await transport.request("/api/v1/capabilities");

    expect(response.status).toBe(401);
    expect(connect).toHaveBeenCalledTimes(2);
    expect(seenTokens).toEqual([`Bearer ${TOKEN_A}`, `Bearer ${TOKEN_B}`]);
  });

  it("refreshes the managed address once after a loopback network failure", async () => {
    const connect = vi.fn(async () => ({
      baseUrl: connect.mock.calls.length === 1
        ? "http://127.0.0.1:49152"
        : "http://127.0.0.1:49153",
      token: connect.mock.calls.length === 1 ? TOKEN_A : TOKEN_B,
    }));
    const requests: Array<{ authorization: string | null; url: string }> = [];
    const fetchImpl = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      requests.push({
        authorization: new Headers(init?.headers).get("Authorization"),
        url: String(input),
      });
      if (requests.length === 1) throw new TypeError("connection refused");
      return new Response(null, { status: 200 });
    });
    const transport = createDesktopTransport({ connect, fetch: fetchImpl });

    await expect(transport.request("/api/v1/capabilities")).resolves.toMatchObject({
      status: 200,
    });

    expect(connect).toHaveBeenCalledTimes(2);
    expect(requests).toEqual([
      {
        authorization: `Bearer ${TOKEN_A}`,
        url: "http://127.0.0.1:49152/api/v1/capabilities",
      },
      {
        authorization: `Bearer ${TOKEN_B}`,
        url: "http://127.0.0.1:49153/api/v1/capabilities",
      },
    ]);
  });

  it("does not replay a non-idempotent write after an ambiguous network failure", async () => {
    const connect = vi.fn(async () => ({
      baseUrl: "http://127.0.0.1:49152",
      token: TOKEN_A,
    }));
    const fetchImpl = vi.fn(async () => {
      throw new TypeError("connection reset after upload");
    });
    const transport = createDesktopTransport({ connect, fetch: fetchImpl });

    await expect(transport.request("/api/v1/profiles/default/config", {
      method: "PATCH",
      body: JSON.stringify({ model: "updated" }),
      headers: { "Content-Type": "application/json" },
    })).rejects.toMatchObject({ kind: "network" });

    expect(connect).toHaveBeenCalledOnce();
    expect(fetchImpl).toHaveBeenCalledOnce();
  });

  it("may replay a create request when its idempotency key makes the retry explicit", async () => {
    const connect = vi.fn(async () => ({
      baseUrl: connect.mock.calls.length === 1
        ? "http://127.0.0.1:49152"
        : "http://127.0.0.1:49153",
      token: connect.mock.calls.length === 1 ? TOKEN_A : TOKEN_B,
    }));
    const fetchImpl = vi.fn(async () => {
      if (fetchImpl.mock.calls.length === 1) throw new TypeError("connection reset");
      return new Response(null, { status: 202 });
    });
    const transport = createDesktopTransport({ connect, fetch: fetchImpl });

    await expect(transport.request("/api/v1/sessions/session_1/runs", {
      method: "POST",
      headers: { "Idempotency-Key": "request-12345678" },
      body: "{}",
    })).resolves.toMatchObject({ status: 202 });

    expect(connect).toHaveBeenCalledTimes(2);
    expect(fetchImpl).toHaveBeenCalledTimes(2);
  });

  it("rejects malformed bridge data without exposing its token", async () => {
    const transport = createDesktopTransport({
      connect: async () => ({
        baseUrl: "http://127.0.0.1:8642",
        token: TOKEN_A,
        extra: true,
      }),
      fetch: async () => new Response(null, { status: 200 }),
    });

    const request = transport.request("/api/v1/capabilities");
    await expect(request).rejects.toMatchObject({ kind: "invalid_connection" });
    await expect(request).rejects.not.toHaveProperty("message", expect.stringContaining(TOKEN_A));
  });

  it.each([
    null,
    [],
    { baseUrl: "http://127.0.0.1:8642", other: TOKEN_A },
    { token: TOKEN_A, other: "http://127.0.0.1:8642" },
    { baseUrl: 1, token: TOKEN_A },
    { baseUrl: "http://127.0.0.1:8642", token: 1 },
    { baseUrl: "http://127.0.0.1:8642", token: "short" },
    { baseUrl: "https://example.com", token: TOKEN_A },
  ])("rejects malformed connection payload %j", async (payload) => {
    const transport = createDesktopTransport({
      connect: async () => payload,
      fetch: async () => new Response(null, { status: 200 }),
    });

    await expect(transport.request("/api/v1/capabilities")).rejects.toMatchObject({
      kind: "invalid_connection",
    });
  });

  it.each([
    "https://example.com/api/v1/capabilities",
    "/api/v1/https://example.com",
  ])("rejects a non-relative API path: %s", async (path) => {
    const connect = vi.fn(async () => ({ baseUrl: "http://127.0.0.1:8642", token: TOKEN_A }));
    const transport = createDesktopTransport({ connect });

    await expect(transport.request(path)).rejects.toMatchObject({ kind: "invalid_connection" });
    expect(connect).not.toHaveBeenCalled();
  });

  it("caches a successful connection across requests", async () => {
    const connect = vi.fn(async () => ({ baseUrl: "http://127.0.0.1:8642", token: TOKEN_A }));
    const fetchImpl = vi.fn(async () => new Response(null, { status: 200 }));
    const transport = createDesktopTransport({ connect, fetch: fetchImpl });

    await transport.request("/api/v1/capabilities");
    await transport.request("/api/v1/providers");

    expect(connect).toHaveBeenCalledOnce();
    expect(fetchImpl).toHaveBeenCalledTimes(2);
  });

  it("shares an in-flight connection handshake", async () => {
    let resolveConnection: ((value: unknown) => void) | undefined;
    const connect = vi.fn(() => new Promise<unknown>((resolve) => {
      resolveConnection = resolve;
    }));
    const fetchImpl = vi.fn(async () => new Response(null, { status: 200 }));
    const transport = createDesktopTransport({ connect, fetch: fetchImpl });

    const first = transport.request("/api/v1/capabilities");
    const second = transport.request("/api/v1/providers");
    resolveConnection?.({ baseUrl: "http://127.0.0.1:8642", token: TOKEN_A });
    await Promise.all([first, second]);

    expect(connect).toHaveBeenCalledOnce();
  });

  it("maps fetch failures and preserves either caller abort signal", async () => {
    const connection = async () => ({ baseUrl: "http://127.0.0.1:8642", token: TOKEN_A });
    const offline = createDesktopTransport({
      connect: connection,
      fetch: async () => {
        throw new TypeError("connection refused");
      },
    });
    await expect(offline.request("/api/v1/capabilities")).rejects.toMatchObject({ kind: "network" });

    const optionsController = new AbortController();
    optionsController.abort();
    await expect(offline.request(
      "/api/v1/capabilities",
      {},
      { signal: optionsController.signal },
    )).rejects.toMatchObject({ name: "AbortError" });

    const initController = new AbortController();
    initController.abort();
    await expect(offline.request(
      "/api/v1/capabilities",
      { signal: initController.signal },
    )).rejects.toMatchObject({ name: "AbortError" });
  });

  it("continues the authenticated retry when discarding a 401 body fails", async () => {
    let requestCount = 0;
    const transport = createDesktopTransport({
      connect: async () => ({ baseUrl: "http://127.0.0.1:8642", token: TOKEN_A }),
      fetch: async () => {
        requestCount += 1;
        if (requestCount === 1) {
          return {
            status: 401,
            body: { cancel: async () => { throw new Error("cancel failed"); } },
          } as unknown as Response;
        }
        return new Response(null, { status: 200 });
      },
    });

    await expect(transport.request("/api/v1/capabilities")).resolves.toMatchObject({ status: 200 });
    expect(requestCount).toBe(2);
  });

  it("fails closed with a friendly browser-mode error", async () => {
    await expect(desktopTransport.request("/api/v1/capabilities")).rejects.toEqual(
      expect.objectContaining<Partial<DesktopConnectionError>>({
        kind: "desktop_unavailable",
      }),
    );
  });
});
