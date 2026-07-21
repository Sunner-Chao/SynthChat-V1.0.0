import { describe, expect, it, vi } from "vitest";
import type { DesktopTransport } from "./desktopConnection";
import {
  createPluginsApi,
  parsePlugin,
  parsePluginPage,
  PluginApiError,
  type Plugin,
} from "./plugins";

const NOW = "2026-07-20T08:00:00Z";
const PLUGIN: Plugin = {
  id: "local-tools",
  name: "Local tools",
  version: "1.2.0",
  description: "Manifest-only local tools.",
  author: "SynthChat",
  providedTools: ["local.search"],
  requiresEnv: ["LOCAL_PLUGIN_TOKEN"],
  enabled: false,
  execution: "manifestOnly",
  installedAt: NOW,
  updatedAt: NOW,
};

const PROBLEM = {
  type: "about:blank",
  title: "Plugin catalog revision conflict",
  status: 409,
  detail: "The plugin catalog changed since it was read; refresh before updating.",
  instance: "/api/v1/plugins/local-tools",
  code: "revision_conflict",
  requestId: "req-plugin-1",
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
    expect.objectContaining<Partial<PluginApiError>>({ kind: "invalid_response" }),
  );
}

describe("Plugin API runtime contract", () => {
  it("strictly parses manifest-only plugins", () => {
    expect(parsePlugin(PLUGIN)).toEqual(PLUGIN);
    expect(parsePluginPage({ items: [PLUGIN] })).toEqual({ items: [PLUGIN] });

    invalidResponse(() => parsePlugin({ ...PLUGIN, entryPoint: "plugin.py" }));
    invalidResponse(() => parsePlugin({ ...PLUGIN, execution: "python" }));
    invalidResponse(() => parsePlugin({ ...PLUGIN, requiresEnv: ["lowercase"] }));
    invalidResponse(() => parsePlugin({ ...PLUGIN, providedTools: ["same", "same"] }));
    invalidResponse(() => parsePluginPage({ items: [PLUGIN, PLUGIN] }));
  });

  it("accepts the empty catalog revision and exercises catalog mutations", async () => {
    const requests: Array<{ path: string; init: RequestInit }> = [];
    const client = createPluginsApi({
      request: vi.fn(async (path, init = {}) => {
        requests.push({ path, init });
        if (path === "/api/v1/plugins" && init.method === "GET") {
          return jsonResponse({ items: [] }, 200, { ETag: '"plugin-catalog-0"' });
        }
        if (path.endsWith("/install")) {
          return jsonResponse(PLUGIN, 201, { ETag: '"plugin-catalog-1"' });
        }
        if (init.method === "PATCH") {
          return jsonResponse({ ...PLUGIN, enabled: true }, 200, { ETag: '"plugin-catalog-2"' });
        }
        return new Response(null, { status: 204, headers: { ETag: '"plugin-catalog-3"' } });
      }),
    });

    await expect(client.listPlugins()).resolves.toEqual({
      value: { items: [] },
      etag: '"plugin-catalog-0"',
    });
    await expect(client.installPlugin({ sourcePath: "local-tools" })).resolves.toEqual({
      value: PLUGIN,
      etag: '"plugin-catalog-1"',
    });
    await expect(client.updatePlugin("local-tools", { enabled: true }, '"plugin-catalog-1"'))
      .resolves.toMatchObject({ value: { enabled: true }, etag: '"plugin-catalog-2"' });
    await expect(client.uninstallPlugin("local-tools", '"plugin-catalog-2"'))
      .resolves.toEqual({ etag: '"plugin-catalog-3"' });

    expect(requests.map(({ path }) => path)).toEqual([
      "/api/v1/plugins",
      "/api/v1/plugins/install",
      "/api/v1/plugins/local-tools",
      "/api/v1/plugins/local-tools",
    ]);
    expect(requests.map(({ init }) => init.method)).toEqual(["GET", "POST", "PATCH", "DELETE"]);
    expect(new Headers(requests[2]!.init.headers).get("If-Match")).toBe('"plugin-catalog-1"');
    expect(new Headers(requests[3]!.init.headers).get("If-Match")).toBe('"plugin-catalog-2"');
    expect(JSON.parse(String(requests[1]!.init.body))).toEqual({ sourcePath: "local-tools" });
  });

  it("rejects malformed inputs and responses before they cross the contract", async () => {
    const transport: DesktopTransport = { request: vi.fn() };
    const client = createPluginsApi(transport);

    await expect(client.installPlugin(null as never)).rejects.toMatchObject({ kind: "invalid_request" });
    await expect(client.installPlugin({ sourcePath: " ../escape" })).rejects.toMatchObject({ kind: "invalid_request" });
    await expect(client.updatePlugin("../escape", { enabled: true }, '"plugin-catalog-1"'))
      .rejects.toMatchObject({ kind: "invalid_request" });
    await expect(client.updatePlugin("local-tools", { enabled: "yes" } as never, '"plugin-catalog-1"'))
      .rejects.toMatchObject({ kind: "invalid_request" });
    await expect(client.uninstallPlugin("local-tools", 'W/"plugin-catalog-1"'))
      .rejects.toMatchObject({ kind: "invalid_request" });
    expect(transport.request).not.toHaveBeenCalled();

    for (const response of [
      jsonResponse({ items: [] }, 200),
      jsonResponse({ items: [] }, 200, { ETag: '"other-0"' }),
      jsonResponse({ items: [], leaked: true }, 200, { ETag: '"plugin-catalog-0"' }),
    ]) {
      await expect(createPluginsApi({ request: async () => response }).listPlugins())
        .rejects.toMatchObject({ kind: "invalid_response" });
    }
  });

  it("returns sanitized HTTP errors and validates Problem envelopes", async () => {
    await expect(createPluginsApi({ request: async () => jsonResponse(PROBLEM, 409) })
      .uninstallPlugin("local-tools", '"plugin-catalog-1"'))
      .rejects.toMatchObject({
        kind: "http",
        status: 409,
        code: "revision_conflict",
        requestId: "req-plugin-1",
        retryable: false,
        message: "Plugin catalog revision conflict",
      });

    for (const response of [
      jsonResponse({ ...PROBLEM, status: 412 }, 409),
      jsonResponse({ ...PROBLEM, leaked: true }, 409),
      new Response("plain", { status: 503, headers: { "Content-Type": "text/plain" } }),
    ]) {
      await expect(createPluginsApi({ request: async () => response })
        .uninstallPlugin("local-tools", '"plugin-catalog-1"'))
        .rejects.toMatchObject({ kind: "invalid_response" });
    }
  });
});
