import { describe, expect, it, vi } from "vitest";
import type { DesktopTransport } from "./desktopConnection";
import {
  createMemoriesApi,
  MemoryApiError,
  parseMemory,
  parseMemoryPage,
  type Memory,
  type MemoryPage,
} from "./memories";

const MEMORY: Memory = {
  id: "memory-1",
  target: "memory",
  content: "Prefer concise status updates.",
  provider: "builtin",
};

const PAGE: MemoryPage = {
  items: [MEMORY],
  nextCursor: "cursor-2",
  revision: "memory_default_7",
  provider: "builtin",
  charsUsed: 30,
  charLimit: 20_000,
  promptSafety: "clean",
  capabilities: { create: true, update: true, delete: true, search: true },
};

const PROBLEM = {
  type: "about:blank",
  title: "Revision conflict",
  status: 409,
  code: "revision_conflict",
  requestId: "req-memory-1",
  retryable: false,
};

function jsonResponse(value: unknown, status = 200, headers: HeadersInit = {}): Response {
  return new Response(JSON.stringify(value), {
    status,
    headers: {
      "Content-Type": "application/json",
      ...Object.fromEntries(new Headers(headers)),
    },
  });
}

describe("Memory API runtime contract", () => {
  it("strictly parses builtin Memory pages", () => {
    expect(parseMemory(MEMORY)).toEqual(MEMORY);
    expect(parseMemoryPage(PAGE)).toEqual(PAGE);
    expect(parseMemoryPage({
      ...PAGE,
      items: [{ ...MEMORY, target: "user" }],
      nextCursor: null,
      promptSafety: "blocked",
      capabilities: { create: false, update: false, delete: false, search: false },
    })).toMatchObject({
      nextCursor: null,
      promptSafety: "blocked",
      capabilities: { create: false, update: false, delete: false, search: false },
    });
    expect(parseMemoryPage({ ...PAGE, charsUsed: 20_001 }).charsUsed).toBe(20_001);
    expect(parseMemory({ ...MEMORY, content: "x".repeat(2_200) }).content).toHaveLength(2_200);
  });

  it.each([
    ["extra Memory field", { ...MEMORY, createdAt: "2026-07-17T00:00:00Z" }, parseMemory],
    ["unsupported target", { ...MEMORY, target: "session" }, parseMemory],
    ["unsupported provider", { ...MEMORY, provider: "remote" }, parseMemory],
    ["empty content", { ...MEMORY, content: "" }, parseMemory],
    ["oversized content", { ...MEMORY, content: "x".repeat(2_201) }, parseMemory],
    ["extra page field", { ...PAGE, leaked: true }, parseMemoryPage],
    ["duplicate IDs", { ...PAGE, items: [MEMORY, MEMORY] }, parseMemoryPage],
    ["weak revision", { ...PAGE, revision: "bad revision" }, parseMemoryPage],
    ["fractional usage", { ...PAGE, charsUsed: 1.5 }, parseMemoryPage],
    ["zero character limit", { ...PAGE, charLimit: 0 }, parseMemoryPage],
    ["invalid safety", { ...PAGE, promptSafety: "unknown" }, parseMemoryPage],
    ["extra capability", {
      ...PAGE,
      capabilities: { ...PAGE.capabilities, import: true },
    }, parseMemoryPage],
  ] satisfies Array<[string, unknown, (value: unknown) => unknown]>) (
    "rejects %s",
    (_case, payload, parser) => {
      expect(() => parser(payload)).toThrowError(
        expect.objectContaining<Partial<MemoryApiError>>({ kind: "invalid_response" }),
      );
    },
  );

  it("requires target and sends an isolated GET with its ETag", async () => {
    const controller = new AbortController();
    let captured: { path: string; init: RequestInit; signal?: AbortSignal } | undefined;
    const transport: DesktopTransport = {
      request: async (path, init = {}, options = {}) => {
        captured = { path, init, signal: options.signal };
        return jsonResponse(PAGE, 200, { ETag: '"memory_default_7"' });
      },
    };

    const result = await createMemoriesApi(transport).listMemories(
      "default",
      { target: "memory", query: "concise status", cursor: "cursor-1", limit: 25 },
      { signal: controller.signal },
    );

    expect(captured?.path).toBe(
      "/api/v1/profiles/default/memories?target=memory&q=concise+status&cursor=cursor-1&limit=25",
    );
    expect(captured?.init.method).toBe("GET");
    expect(new Headers(captured?.init.headers).get("Accept")).toBe("application/json");
    expect(captured?.signal).toBe(controller.signal);
    expect(result).toEqual({ value: PAGE, etag: '"memory_default_7"' });
  });

  it("sends the latest strong If-Match and a new idempotency key for creation", async () => {
    let captured: { path: string; init: RequestInit } | undefined;
    const transport: DesktopTransport = {
      request: async (path, init = {}) => {
        captured = { path, init };
        return jsonResponse(MEMORY, 201, { ETag: '"memory_default_8"' });
      },
    };

    const result = await createMemoriesApi(transport).createMemory(
      "default",
      { target: "memory", content: MEMORY.content },
      '"memory_default_7"',
      "memory-create-0001",
    );

    expect(captured?.path).toBe("/api/v1/profiles/default/memories");
    expect(captured?.init.method).toBe("POST");
    const headers = new Headers(captured?.init.headers);
    expect(headers.get("If-Match")).toBe('"memory_default_7"');
    expect(headers.get("Idempotency-Key")).toBe("memory-create-0001");
    expect(headers.get("Content-Type")).toBe("application/json");
    expect(captured?.init.body).toBe(JSON.stringify({ target: "memory", content: MEMORY.content }));
    expect(result.etag).toBe('"memory_default_8"');
  });

  it("accepts rotated PATCH IDs and requires bounded conditional writes", async () => {
    const requests: Array<{ path: string; init: RequestInit }> = [];
    const transport: DesktopTransport = {
      request: async (path, init = {}) => {
        requests.push({ path, init });
        if (init.method === "PATCH") {
          return jsonResponse({ ...MEMORY, id: "memory/rotated", content: "Updated" }, 200, {
            ETag: '"memory_default_8"',
          });
        }
        return new Response(null, { status: 204, headers: { ETag: '"memory_default_9"' } });
      },
    };
    const client = createMemoriesApi(transport);

    await expect(client.updateMemory(
      "default",
      "memory/1",
      { content: "Updated" },
      '"memory_default_7"',
    )).resolves.toMatchObject({
      value: { id: "memory/rotated", content: "Updated" },
      etag: '"memory_default_8"',
    });
    await expect(client.deleteMemory(
      "default",
      "memory/rotated",
      '"memory_default_8"',
    )).resolves.toEqual({ etag: '"memory_default_9"' });

    expect(requests.map((request) => request.path)).toEqual([
      "/api/v1/profiles/default/memories/memory%2F1",
      "/api/v1/profiles/default/memories/memory%2Frotated",
    ]);
    expect(requests.map((request) => new Headers(request.init.headers).get("If-Match"))).toEqual([
      '"memory_default_7"',
      '"memory_default_8"',
    ]);
    expect(new Headers(requests[0]?.init.headers).get("Content-Type")).toBe(
      "application/merge-patch+json",
    );
    expect(requests[0]?.init.body).toBe(JSON.stringify({ content: "Updated" }));
    expect(requests[1]?.init.method).toBe("DELETE");
  });

  it("returns a sanitized stale-revision error with the current ETag", async () => {
    const transport: DesktopTransport = {
      request: async () => jsonResponse(PROBLEM, 409, {
        ETag: '"memory_default_10"',
        "Content-Type": "application/problem+json",
      }),
    };

    await expect(createMemoriesApi(transport).deleteMemory(
      "default",
      "memory-1",
      '"memory_default_7"',
    )).rejects.toMatchObject({
      kind: "http",
      status: 409,
      code: "revision_conflict",
      requestId: "req-memory-1",
      etag: '"memory_default_10"',
    });
  });

  it("rejects missing, weak, mismatched, and target-leaking response ETags", async () => {
    const responses = [
      jsonResponse(PAGE),
      jsonResponse(PAGE, 200, { ETag: "W/\"memory_default_7\"" }),
      jsonResponse(PAGE, 200, { ETag: '"memory_default_8"' }),
      jsonResponse(
        { ...PAGE, items: [{ ...MEMORY, target: "user" }] },
        200,
        { ETag: '"memory_default_7"' },
      ),
    ];
    for (const response of responses) {
      await expect(createMemoriesApi({ request: async () => response }).listMemories(
        "default",
        { target: "memory" },
      )).rejects.toMatchObject({ kind: "invalid_response" });
    }
  });

  it("rejects invalid requests before using the Desktop transport", async () => {
    const transport: DesktopTransport = {
      request: vi.fn(async () => new Response(null, { status: 204 })),
    };
    const client = createMemoriesApi(transport);

    await expect(client.listMemories("default", {} as never)).rejects.toMatchObject({
      kind: "invalid_request",
    });
    await expect(client.listMemories("../escape", { target: "memory" })).rejects.toMatchObject({
      kind: "invalid_request",
    });
    await expect(client.listMemories("default", {
      target: "memory",
      limit: 101,
    })).rejects.toMatchObject({ kind: "invalid_request" });
    await expect(client.createMemory(
      "default",
      { target: "memory", content: "" },
      '"memory_default_1"',
      "memory-create-1",
    )).rejects.toMatchObject({ kind: "invalid_request" });
    await expect(client.updateMemory(
      "default",
      "memory-1",
      { content: "Updated", leaked: true } as never,
      "weak",
    )).rejects.toMatchObject({ kind: "invalid_request" });
    await expect(client.deleteMemory(
      "default",
      "memory-1",
      "weak",
    )).rejects.toMatchObject({ kind: "invalid_request" });
    expect(transport.request).not.toHaveBeenCalled();
  });

  it("strictly validates Problem envelopes and conflict ETags", async () => {
    const cases = [
      jsonResponse({ ...PROBLEM, status: 412 }, 409, {
        ETag: '"memory_default_2"',
        "Content-Type": "application/problem+json",
      }),
      jsonResponse(PROBLEM, 409, { "Content-Type": "application/problem+json" }),
      new Response("plain", { status: 422, headers: { "Content-Type": "text/plain" } }),
    ];
    for (const response of cases) {
      await expect(createMemoriesApi({ request: async () => response }).deleteMemory(
        "default",
        "memory-1",
        '"memory_default_1"',
      )).rejects.toMatchObject({ kind: "invalid_response" });
    }
  });
});
