import { describe, expect, it, vi } from "vitest";
import type { DesktopTransport } from "./desktopConnection";
import {
  createToolsetsApi,
  parseToolset,
  parseToolsetList,
  ToolsetApiError,
  type Toolset,
} from "./toolsets";

const TOOLSET: Toolset = {
  id: "web",
  displayName: "Web",
  description: "Search and retrieve web content.",
  enabled: true,
  configured: true,
  tools: ["web_search", "web_fetch"],
};

const PROBLEM = {
  type: "about:blank",
  title: "Configuration changed",
  status: 409,
  code: "revision_conflict",
  requestId: "req-toolset-1",
  retryable: false,
};

function jsonResponse(value: unknown, status = 200, headers: HeadersInit = {}): Response {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "Content-Type": "application/json", ...Object.fromEntries(new Headers(headers)) },
  });
}

describe("Toolset API runtime contract", () => {
  it("strictly parses Toolsets and rejects duplicate list IDs", () => {
    expect(parseToolset(TOOLSET)).toEqual(TOOLSET);
    expect(parseToolset({ ...TOOLSET, configSchema: { type: "object" } })).toMatchObject({
      configSchema: { type: "object" },
    });
    expect(parseToolsetList([TOOLSET])).toEqual([TOOLSET]);

    for (const payload of [
      { ...TOOLSET, id: "" },
      { ...TOOLSET, enabled: "true" },
      { ...TOOLSET, tools: ["web_search", 1] },
      { ...TOOLSET, tools: ["web_search", "web_search"] },
      { ...TOOLSET, configSchema: [] },
      { ...TOOLSET, configSchema: { invalid: undefined } },
      (({ description: _description, ...rest }) => rest)(TOOLSET),
    ]) {
      expect(() => parseToolset(payload)).toThrowError(
        expect.objectContaining<Partial<ToolsetApiError>>({ kind: "invalid_response" }),
      );
    }
    expect(() => parseToolsetList([TOOLSET, TOOLSET])).toThrowError(
      expect.objectContaining<Partial<ToolsetApiError>>({ kind: "invalid_response" }),
    );
  });

  it("lists and updates Toolsets with required ETags and exact request headers", async () => {
    const controller = new AbortController();
    const requests: Array<{ path: string; init: RequestInit; signal?: AbortSignal }> = [];
    const transport: DesktopTransport = {
      request: vi.fn(async (path, init = {}, options = {}) => {
        requests.push({ path, init, signal: options.signal });
        if ((init.method ?? "GET") === "GET") {
          return jsonResponse([TOOLSET], 200, { ETag: '"config-1"' });
        }
        return jsonResponse(
          { ...TOOLSET, id: "web/search", enabled: false },
          200,
          { ETag: '"config-2"' },
        );
      }),
    };
    const client = createToolsetsApi(transport);

    await expect(client.listToolsets("default", { signal: controller.signal })).resolves.toEqual({
      value: [TOOLSET],
      etag: '"config-1"',
    });
    await expect(client.updateToolset(
      "default",
      "web/search",
      { enabled: false },
      '"config-1"',
      { signal: controller.signal },
    )).resolves.toMatchObject({ etag: '"config-2"', value: { enabled: false } });

    expect(requests[0]).toMatchObject({
      path: "/api/v1/profiles/default/toolsets",
      signal: controller.signal,
    });
    expect(new Headers(requests[0]!.init.headers).get("Accept")).toBe("application/json");
    expect(requests[1]!.path).toBe("/api/v1/profiles/default/toolsets/web%2Fsearch");
    expect(new Headers(requests[1]!.init.headers).get("Content-Type")).toBe("application/merge-patch+json");
    expect(new Headers(requests[1]!.init.headers).get("If-Match")).toBe('"config-1"');
    expect(requests[1]!.init.body).toBe('{"enabled":false}');
    expect(requests[1]!.signal).toBe(controller.signal);
  });

  it("rejects missing response ETags and invalid request patches before transport", async () => {
    const missingEtag = createToolsetsApi({
      request: vi.fn(async () => jsonResponse([TOOLSET])),
    });
    await expect(missingEtag.listToolsets("default")).rejects.toMatchObject({
      kind: "invalid_response",
    });

    const transport: DesktopTransport = {
      request: vi.fn(async () => jsonResponse(TOOLSET, 200, { ETag: '"config-2"' })),
    };
    const client = createToolsetsApi(transport);
    await expect(client.updateToolset(
      "../escape",
      "web",
      { enabled: false },
      '"config-1"',
    )).rejects.toMatchObject({ kind: "invalid_request" });
    await expect(client.updateToolset(
      "default",
      "web",
      { enabled: false },
      "weak",
    )).rejects.toMatchObject({ kind: "invalid_request" });
    await expect(client.updateToolset(
      "default",
      "web",
      { enabled: false, config: {} } as never,
      '"config-1"',
    )).rejects.toMatchObject({ kind: "invalid_request" });
    expect(transport.request).not.toHaveBeenCalled();
  });

  it("preserves the current service ETag on a 409 response", async () => {
    const client = createToolsetsApi({
      request: vi.fn(async () => jsonResponse(PROBLEM, 409, {
        "Content-Type": "application/problem+json",
        ETag: '"config-current"',
      })),
    });

    await expect(client.updateToolset(
      "default",
      "web",
      { enabled: false },
      '"config-stale"',
    )).rejects.toMatchObject({
      kind: "http",
      status: 409,
      code: "revision_conflict",
      requestId: "req-toolset-1",
      etag: '"config-current"',
    });

    const missingConflictEtag = createToolsetsApi({
      request: vi.fn(async () => jsonResponse(PROBLEM, 409, {
        "Content-Type": "application/problem+json",
      })),
    });
    await expect(missingConflictEtag.updateToolset(
      "default",
      "web",
      { enabled: false },
      '"config-stale"',
    )).rejects.toMatchObject({ kind: "invalid_response" });
  });
});
