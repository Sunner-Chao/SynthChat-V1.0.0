import { describe, expect, it, vi } from "vitest";
import type { DesktopTransport } from "./desktopConnection";
import {
  createMcpApi,
  McpApiError,
  parseMcpServer,
  parseMcpServerList,
  type CreateMcpServerInput,
  type McpServer,
} from "./mcp";

const SERVER_ID = `mcp_${"a".repeat(32)}`;
const REMOTE_ID = `mcp_${"b".repeat(32)}`;

const STDIO_SERVER: McpServer = {
  id: SERVER_ID,
  name: "local_tools",
  transport: "stdio",
  command: "npx",
  args: ["-y", "@example/mcp"],
  url: null,
  enabled: true,
  timeoutSeconds: 30,
  envSecretNames: ["MCP_TOKEN"],
  bearerTokenSecretName: null,
  missingSecretNames: ["MCP_TOKEN"],
};

const REMOTE_SERVER: McpServer = {
  id: REMOTE_ID,
  name: "remote_tools",
  transport: "streamableHttp",
  command: null,
  args: [],
  url: "https://mcp.example.com/rpc",
  enabled: false,
  timeoutSeconds: 45,
  envSecretNames: [],
  bearerTokenSecretName: "REMOTE_TOKEN",
  missingSecretNames: [],
};

const PROBLEM = {
  type: "about:blank",
  title: "Configuration changed",
  status: 409,
  code: "revision_conflict",
  requestId: "req-mcp-1",
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
    expect.objectContaining<Partial<McpApiError>>({ kind: "invalid_response" }),
  );
}

describe("MCP API runtime contract", () => {
  it("strictly parses transport-specific servers and secret-reference readiness", () => {
    expect(parseMcpServer(STDIO_SERVER)).toEqual(STDIO_SERVER);
    expect(parseMcpServer(REMOTE_SERVER)).toEqual(REMOTE_SERVER);
    expect(parseMcpServerList([STDIO_SERVER, REMOTE_SERVER])).toEqual([
      STDIO_SERVER,
      REMOTE_SERVER,
    ]);

    for (const payload of [
      { ...STDIO_SERVER, secretValue: "must-not-exist" },
      { ...STDIO_SERVER, id: "mcp_bad" },
      { ...STDIO_SERVER, transport: "http" },
      { ...STDIO_SERVER, command: null },
      { ...STDIO_SERVER, url: "https://example.com" },
      { ...STDIO_SERVER, args: ["--api-key=plaintext"] },
      { ...STDIO_SERVER, timeoutSeconds: 0 },
      { ...STDIO_SERVER, envSecretNames: ["bad-secret"] },
      { ...STDIO_SERVER, missingSecretNames: ["OTHER_TOKEN"] },
      { ...REMOTE_SERVER, command: "npx" },
      { ...REMOTE_SERVER, args: ["--safe"] },
      { ...REMOTE_SERVER, envSecretNames: ["REMOTE_TOKEN"] },
      { ...REMOTE_SERVER, url: "http://example.com/mcp" },
      { ...REMOTE_SERVER, url: "https://10.0.0.1/mcp" },
      { ...REMOTE_SERVER, url: "https://user@example.com/mcp" },
      (({ missingSecretNames: _missing, ...rest }) => rest)(STDIO_SERVER),
    ]) invalidResponse(() => parseMcpServer(payload));

    invalidResponse(() => parseMcpServerList([STDIO_SERVER, STDIO_SERVER]));
    invalidResponse(() => parseMcpServerList({ items: [STDIO_SERVER] }));
  });

  it("accepts only public HTTPS or loopback HTTP remote URLs", () => {
    for (const url of [
      "https://example.com/mcp",
      "https://8.8.8.8/mcp",
      "http://127.0.0.1:9000/mcp",
      "http://localhost:9000/sse",
    ]) {
      expect(parseMcpServer({ ...REMOTE_SERVER, url })).toMatchObject({ url });
    }
    for (const url of [
      "http://example.com/mcp",
      "https://169.254.169.254/latest",
      "https://192.168.1.1/mcp",
      "https://224.0.0.1/mcp",
      "https://[::ffff:a00:1]/mcp",
      "https://example.com/mcp?token=x",
      "javascript:alert(1)",
    ]) invalidResponse(() => parseMcpServer({ ...REMOTE_SERVER, url }));
  });

  it("uses exact CRUD paths, media types, idempotency, and shared Profile ETags", async () => {
    const requests: Array<{ path: string; init: RequestInit; signal?: AbortSignal }> = [];
    const controller = new AbortController();
    const client = createMcpApi({
      request: vi.fn(async (path, init = {}, options = {}) => {
        requests.push({ path, init, signal: options.signal });
        if (init.method === "GET") return jsonResponse([STDIO_SERVER], 200, { ETag: '"config-1"' });
        if (init.method === "POST") return jsonResponse(STDIO_SERVER, 201, { ETag: '"config-2"' });
        if (init.method === "PATCH") {
          return jsonResponse({ ...STDIO_SERVER, enabled: false }, 200, { ETag: '"config-3"' });
        }
        return new Response(null, { status: 204, headers: { ETag: '"config-4"' } });
      }),
    });
    const input: CreateMcpServerInput = {
      name: "local_tools",
      transport: "stdio",
      command: "npx",
      args: ["-y", "@example/mcp"],
      enabled: true,
      timeoutSeconds: 30,
      envSecretNames: ["MCP_TOKEN"],
    };

    await expect(client.listServers("default", { signal: controller.signal })).resolves.toEqual({
      value: [STDIO_SERVER],
      etag: '"config-1"',
    });
    await expect(client.createServer(
      "default",
      input,
      "mcp-create-key-001",
      { signal: controller.signal },
    )).resolves.toEqual({ value: STDIO_SERVER, etag: '"config-2"' });
    await expect(client.updateServer(
      "default",
      SERVER_ID,
      { enabled: false },
      '"config-2"',
      { signal: controller.signal },
    )).resolves.toMatchObject({ etag: '"config-3"' });
    await expect(client.deleteServer(
      "default",
      SERVER_ID,
      '"config-3"',
      { signal: controller.signal },
    )).resolves.toEqual({ etag: '"config-4"' });

    expect(requests.map(({ path }) => path)).toEqual([
      "/api/v1/profiles/default/mcp/servers",
      "/api/v1/profiles/default/mcp/servers",
      `/api/v1/profiles/default/mcp/servers/${SERVER_ID}`,
      `/api/v1/profiles/default/mcp/servers/${SERVER_ID}`,
    ]);
    expect(requests.every(({ signal }) => signal === controller.signal)).toBe(true);
    const postHeaders = new Headers(requests[1]!.init.headers);
    expect(postHeaders.get("Content-Type")).toBe("application/json");
    expect(postHeaders.get("Idempotency-Key")).toBe("mcp-create-key-001");
    expect(requests[1]!.init.body).toBe(JSON.stringify(input));
    const patchHeaders = new Headers(requests[2]!.init.headers);
    expect(patchHeaders.get("Content-Type")).toBe("application/merge-patch+json");
    expect(patchHeaders.get("If-Match")).toBe('"config-2"');
    expect(requests[2]!.init.body).toBe('{"enabled":false}');
    expect(new Headers(requests[3]!.init.headers).get("If-Match")).toBe('"config-3"');
  });

  it("rejects plaintext-like or malformed requests before transport", async () => {
    const transport: DesktopTransport = {
      request: vi.fn(async () => jsonResponse([STDIO_SERVER], 200, { ETag: '"config-1"' })),
    };
    const client = createMcpApi(transport);
    const common: Extract<CreateMcpServerInput, { transport: "stdio" }> = {
      name: "local_tools",
      transport: "stdio",
      command: "npx",
      args: [],
      enabled: true,
      timeoutSeconds: 30,
      envSecretNames: [],
    };
    const invalidCalls: Array<Promise<unknown>> = [
      client.listServers("../escape"),
      client.createServer("default", { ...common, token: "plaintext" } as never, "valid-key-001"),
      client.createServer("default", { ...common, args: ["--token=plaintext"] }, "valid-key-002"),
      client.createServer("default", { ...common, command: "powershell" }, "valid-key-003"),
      client.createServer("default", { ...common, envSecretNames: ["bad-secret"] }, "valid-key-004"),
      client.createServer("default", {
        name: "remote",
        transport: "streamableHttp",
        url: "https://10.0.0.1/mcp",
        enabled: true,
        timeoutSeconds: 30,
      }, "valid-key-005"),
      client.createServer("default", common, "short"),
      client.updateServer("default", "bad-id", { enabled: false }, '"config-1"'),
      client.updateServer("default", SERVER_ID, { enabled: false }, "weak"),
      client.updateServer("default", SERVER_ID, {
        command: "npx",
        bearerTokenSecretName: "MCP_TOKEN",
      }, '"config-1"'),
    ];
    for (const call of invalidCalls) {
      await expect(call).rejects.toMatchObject({ kind: "invalid_request" });
    }
    expect(transport.request).not.toHaveBeenCalled();
  });

  it("requires strong response ETags and exact response media types", async () => {
    for (const response of [
      jsonResponse([STDIO_SERVER]),
      jsonResponse([STDIO_SERVER], 200, { ETag: 'W/"config-1"' }),
      jsonResponse({ items: [STDIO_SERVER] }, 200, { ETag: '"config-1"' }),
      jsonResponse([{ ...STDIO_SERVER, bearerToken: "plaintext" }], 200, { ETag: '"config-1"' }),
      new Response(JSON.stringify([STDIO_SERVER]), {
        status: 200,
        headers: { "Content-Type": "text/plain", ETag: '"config-1"' },
      }),
    ]) {
      const client = createMcpApi({ request: vi.fn(async () => response) });
      await expect(client.listServers("default")).rejects.toMatchObject({ kind: "invalid_response" });
    }
  });

  it("surfaces conflict metadata and rejects malformed problem details", async () => {
    const client = createMcpApi({
      request: vi.fn(async () => jsonResponse(PROBLEM, 409, { ETag: '"config-current"' })),
    });
    await expect(client.updateServer(
      "default",
      SERVER_ID,
      { enabled: false },
      '"config-stale"',
    )).rejects.toMatchObject({
      kind: "http",
      status: 409,
      code: "revision_conflict",
      requestId: "req-mcp-1",
      retryable: false,
      etag: '"config-current"',
    });

    for (const response of [
      jsonResponse(PROBLEM, 409),
      jsonResponse(PROBLEM, 409, { ETag: 'W/"config-current"' }),
      jsonResponse({ ...PROBLEM, status: 400 }, 409, { ETag: '"config-current"' }),
      jsonResponse({ ...PROBLEM, secret: "plaintext" }, 409, { ETag: '"config-current"' }),
    ]) {
      const malformed = createMcpApi({ request: vi.fn(async () => response) });
      await expect(malformed.updateServer(
        "default",
        SERVER_ID,
        { enabled: false },
        '"config-stale"',
      )).rejects.toMatchObject({ kind: "invalid_response" });
    }
  });
});
