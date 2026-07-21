import { describe, expect, it, vi } from "vitest";
import type { DesktopTransport } from "./desktopConnection";
import {
  createProductCatalogApi,
  parseMoment,
  parsePersona,
  parseWorldbook,
  ProductCatalogApiError,
  type Moment,
  type Persona,
  type Worldbook,
} from "./productCatalog";

const NOW = "2026-07-20T08:00:00Z";
const LATER = "2026-07-20T08:05:00Z";

const PERSONA: Persona = {
  id: "persona_1",
  name: "小日向",
  avatar: null,
  systemPrompt: "保持角色一致。",
  characterPrompt: "温和、直接。",
  outputExamples: "你好。",
  systemInstructions: "结合世界书回答。",
  provider: "openai-api",
  model: "gpt-5.5",
  temperature: 0.8,
  maxTokens: 2_048,
  toolsEnabled: true,
  memoryEnabled: true,
  proactiveEnabled: false,
  legacyAgentId: null,
  createdAt: NOW,
  updatedAt: NOW,
  revision: 1,
};

const WORLDBOOK: Worldbook = {
  id: "worldbook_1",
  name: "SynthChat 世界",
  description: "本地世界设定。",
  boundPersonaIds: [PERSONA.id],
  sections: [{
    id: "section_1",
    key: "城市",
    content: "故事发生在海边城市。",
    enabled: true,
  }],
  createdAt: NOW,
  updatedAt: NOW,
  revision: 1,
};

const MOMENT: Moment = {
  id: "moment_1",
  authorId: PERSONA.id,
  body: "今天完成了 Rust 后端接入。",
  coverFileId: null,
  likedBy: [],
  comments: [],
  createdAt: NOW,
  updatedAt: NOW,
  revision: 1,
};

const PROBLEM = {
  type: "about:blank",
  title: "Product revision conflict",
  status: 409,
  detail: "The product item changed since it was read; refresh before updating.",
  instance: "/api/v1/profiles/default/personas/persona_1",
  code: "revision_conflict",
  requestId: "req-product-1",
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
    expect.objectContaining<Partial<ProductCatalogApiError>>({ kind: "invalid_response" }),
  );
}

describe("Product catalog API runtime contract", () => {
  it("strictly parses Persona, Worldbook, and Moment responses", () => {
    expect(parsePersona(PERSONA)).toEqual(PERSONA);
    expect(parseWorldbook(WORLDBOOK)).toEqual(WORLDBOOK);
    expect(parseMoment({
      ...MOMENT,
      likedBy: ["user"],
      comments: [{
        id: "comment_1",
        authorId: "user",
        text: "做得好。",
        replyTo: null,
        createdAt: NOW,
        updatedAt: NOW,
      }, {
        id: "comment_2",
        authorId: PERSONA.id,
        text: "谢谢。",
        replyTo: "comment_1",
        createdAt: LATER,
        updatedAt: LATER,
      }],
    })).toMatchObject({ likedBy: ["user"], comments: [{ id: "comment_1" }, { replyTo: "comment_1" }] });
  });

  it.each([
    ["extra Persona field", { ...PERSONA, secret: "leak" }, parsePersona],
    ["invalid Persona revision", { ...PERSONA, revision: 0 }, parsePersona],
    ["invalid Persona temperature", { ...PERSONA, temperature: 2.1 }, parsePersona],
    ["duplicate Worldbook section ID", {
      ...WORLDBOOK,
      sections: [WORLDBOOK.sections[0], WORLDBOOK.sections[0]],
    }, parseWorldbook],
    ["invalid Worldbook binding ID", { ...WORLDBOOK, boundPersonaIds: ["../persona"] }, parseWorldbook],
    ["duplicate Moment like", { ...MOMENT, likedBy: ["user", "user"] }, parseMoment],
    ["dangling Moment reply", {
      ...MOMENT,
      comments: [{
        id: "comment_1",
        authorId: "user",
        text: "Reply",
        replyTo: "comment_missing",
        createdAt: NOW,
        updatedAt: NOW,
      }],
    }, parseMoment],
    ["invalid Moment timestamp", { ...MOMENT, updatedAt: "today" }, parseMoment],
  ] satisfies Array<[string, unknown, (value: unknown) => unknown]>) (
    "rejects %s",
    (_case, payload, parser) => invalidResponse(() => parser(payload)),
  );

  it("exercises every Persona route with full replacement and strong ETags", async () => {
    const requests: Array<{ path: string; init: RequestInit; signal?: AbortSignal }> = [];
    const controller = new AbortController();
    const updated = { ...PERSONA, name: "小日向（更新）", updatedAt: LATER, revision: 2 };
    const transport: DesktopTransport = {
      request: vi.fn(async (path, init = {}, options = {}) => {
        requests.push({ path, init, signal: options.signal });
        if (init.method === "DELETE") return new Response(null, { status: 204 });
        if (init.method === "POST") return jsonResponse(PERSONA, 201, { ETag: '"product-persona-1"' });
        if (init.method === "PATCH") return jsonResponse(updated, 200, { ETag: '"product-persona-2"' });
        return path.includes("?q=") ? jsonResponse([PERSONA]) : jsonResponse(PERSONA);
      }),
    };
    const client = createProductCatalogApi(transport);
    const input = {
      name: PERSONA.name,
      avatar: PERSONA.avatar,
      systemPrompt: PERSONA.systemPrompt,
      characterPrompt: PERSONA.characterPrompt,
      outputExamples: PERSONA.outputExamples,
      systemInstructions: PERSONA.systemInstructions,
      provider: PERSONA.provider,
      model: PERSONA.model,
      temperature: PERSONA.temperature,
      maxTokens: PERSONA.maxTokens,
      toolsEnabled: PERSONA.toolsEnabled,
      memoryEnabled: PERSONA.memoryEnabled,
      proactiveEnabled: PERSONA.proactiveEnabled,
      legacyAgentId: PERSONA.legacyAgentId,
    };

    await expect(client.listPersonas("default", "小 日", { signal: controller.signal })).resolves.toEqual([PERSONA]);
    await expect(client.getPersona("default", PERSONA.id, { signal: controller.signal })).resolves.toEqual({
      value: PERSONA,
      etag: '"product-persona-1"',
    });
    await expect(client.createPersona("default", input, { signal: controller.signal })).resolves.toMatchObject({ etag: '"product-persona-1"' });
    await expect(client.updatePersona("default", PERSONA.id, { ...input, name: updated.name }, '"product-persona-1"', { signal: controller.signal })).resolves.toEqual({
      value: updated,
      etag: '"product-persona-2"',
    });
    await expect(client.deletePersona("default", PERSONA.id, '"product-persona-2"', { signal: controller.signal })).resolves.toBeUndefined();

    expect(requests.map(({ path }) => path)).toEqual([
      "/api/v1/profiles/default/personas?q=%E5%B0%8F+%E6%97%A5",
      "/api/v1/profiles/default/personas/persona_1",
      "/api/v1/profiles/default/personas",
      "/api/v1/profiles/default/personas/persona_1",
      "/api/v1/profiles/default/personas/persona_1",
    ]);
    expect(requests.map(({ init }) => init.method)).toEqual(["GET", "GET", "POST", "PATCH", "DELETE"]);
    expect(requests.every(({ signal }) => signal === controller.signal)).toBe(true);
    expect(new Headers(requests[3]!.init.headers).get("If-Match")).toBe('"product-persona-1"');
    expect(new Headers(requests[3]!.init.headers).get("Content-Type")).toBe("application/json");
    expect(JSON.parse(String(requests[3]!.init.body))).toMatchObject({ name: updated.name, model: PERSONA.model });
    expect(new Headers(requests[4]!.init.headers).get("If-Match")).toBe('"product-persona-2"');
  });

  it("exercises every Worldbook route with section inputs", async () => {
    const requests: Array<{ path: string; init: RequestInit }> = [];
    const updated = { ...WORLDBOOK, description: "更新后的设定。", updatedAt: LATER, revision: 2 };
    const client = createProductCatalogApi({
      request: vi.fn(async (path, init = {}) => {
        requests.push({ path, init });
        if (init.method === "DELETE") return new Response(null, { status: 204 });
        if (init.method === "POST") return jsonResponse(WORLDBOOK, 201, { ETag: '"product-worldbook-1"' });
        if (init.method === "PATCH") return jsonResponse(updated, 200, { ETag: '"product-worldbook-2"' });
        return path.includes("?q=") ? jsonResponse([WORLDBOOK]) : jsonResponse(WORLDBOOK);
      }),
    });
    const input = {
      name: WORLDBOOK.name,
      description: WORLDBOOK.description,
      boundPersonaIds: WORLDBOOK.boundPersonaIds,
      sections: WORLDBOOK.sections.map(({ key, content, enabled }) => ({ key, content, enabled })),
    };

    await expect(client.listWorldbooks("work_profile", "城市")).resolves.toEqual([WORLDBOOK]);
    await expect(client.getWorldbook("work_profile", WORLDBOOK.id)).resolves.toMatchObject({ etag: '"product-worldbook-1"' });
    await expect(client.createWorldbook("work_profile", input)).resolves.toMatchObject({ etag: '"product-worldbook-1"' });
    await expect(client.updateWorldbook("work_profile", WORLDBOOK.id, {
      ...input,
      description: updated.description,
    }, '"product-worldbook-1"')).resolves.toEqual({ value: updated, etag: '"product-worldbook-2"' });
    await client.deleteWorldbook("work_profile", WORLDBOOK.id, '"product-worldbook-2"');

    expect(requests.map(({ path }) => path)).toEqual([
      "/api/v1/profiles/work_profile/worldbooks?q=%E5%9F%8E%E5%B8%82",
      "/api/v1/profiles/work_profile/worldbooks/worldbook_1",
      "/api/v1/profiles/work_profile/worldbooks",
      "/api/v1/profiles/work_profile/worldbooks/worldbook_1",
      "/api/v1/profiles/work_profile/worldbooks/worldbook_1",
    ]);
    expect(JSON.parse(String(requests[2]!.init.body))).toEqual(input);
    expect(new Headers(requests[3]!.init.headers).get("If-Match")).toBe('"product-worldbook-1"');
  });

  it("exercises Moment CRUD, comments, and like mutations", async () => {
    const requests: Array<{ path: string; init: RequestInit }> = [];
    const revisions = Array.from({ length: 5 }, (_, index) => ({
      ...MOMENT,
      body: index === 1 ? "更新后的动态。" : MOMENT.body,
      likedBy: index === 4 ? ["user"] : [],
      comments: index === 2 ? [{
        id: "comment_1",
        authorId: "user",
        text: "很好。",
        replyTo: null,
        createdAt: LATER,
        updatedAt: LATER,
      }] : [],
      updatedAt: index === 0 ? NOW : LATER,
      revision: index + 1,
    }));
    const client = createProductCatalogApi({
      request: vi.fn(async (path, init = {}) => {
        requests.push({ path, init });
        if (path.endsWith("/comments") && init.method === "POST") return jsonResponse(revisions[2], 200, { ETag: '"product-moment-3"' });
        if (path.includes("/comments/") && init.method === "DELETE") return jsonResponse(revisions[3], 200, { ETag: '"product-moment-4"' });
        if (path.endsWith("/like")) return jsonResponse(revisions[4], 200, { ETag: '"product-moment-5"' });
        if (init.method === "DELETE") return new Response(null, { status: 204 });
        if (init.method === "POST") return jsonResponse(revisions[0], 201, { ETag: '"product-moment-1"' });
        if (init.method === "PATCH") return jsonResponse(revisions[1], 200, { ETag: '"product-moment-2"' });
        return path.endsWith("/moments") ? jsonResponse([revisions[0]]) : jsonResponse(revisions[0]);
      }),
    });

    await expect(client.listMoments("default")).resolves.toEqual([revisions[0]]);
    await expect(client.getMoment("default", MOMENT.id)).resolves.toMatchObject({ etag: '"product-moment-1"' });
    await expect(client.createMoment("default", { body: MOMENT.body, authorId: MOMENT.authorId, coverFileId: null })).resolves.toMatchObject({ etag: '"product-moment-1"' });
    await expect(client.updateMoment("default", MOMENT.id, {
      body: "更新后的动态。",
      authorId: MOMENT.authorId,
      coverFileId: null,
    }, '"product-moment-1"')).resolves.toEqual({ value: revisions[1], etag: '"product-moment-2"' });
    await expect(client.addMomentComment("default", MOMENT.id, {
      authorId: "user",
      text: "很好。",
      replyTo: null,
    }, '"product-moment-2"')).resolves.toEqual({ value: revisions[2], etag: '"product-moment-3"' });
    await expect(client.deleteMomentComment("default", MOMENT.id, "comment_1", '"product-moment-3"')).resolves.toEqual({ value: revisions[3], etag: '"product-moment-4"' });
    await expect(client.setMomentLike("default", MOMENT.id, { actorId: "user", liked: true }, '"product-moment-4"')).resolves.toEqual({ value: revisions[4], etag: '"product-moment-5"' });
    await client.deleteMoment("default", MOMENT.id, '"product-moment-5"');

    expect(requests.map(({ path }) => path)).toEqual([
      "/api/v1/profiles/default/moments",
      "/api/v1/profiles/default/moments/moment_1",
      "/api/v1/profiles/default/moments",
      "/api/v1/profiles/default/moments/moment_1",
      "/api/v1/profiles/default/moments/moment_1/comments",
      "/api/v1/profiles/default/moments/moment_1/comments/comment_1",
      "/api/v1/profiles/default/moments/moment_1/like",
      "/api/v1/profiles/default/moments/moment_1",
    ]);
    expect(requests.map(({ init }) => init.method)).toEqual(["GET", "GET", "POST", "PATCH", "POST", "DELETE", "PUT", "DELETE"]);
    expect(requests.slice(3).map(({ init }) => new Headers(init.headers).get("If-Match"))).toEqual([
      '"product-moment-1"',
      '"product-moment-2"',
      '"product-moment-3"',
      '"product-moment-4"',
      '"product-moment-5"',
    ]);
  });

  it("rejects invalid inputs and kind-mismatched ETags before transport", async () => {
    const transport: DesktopTransport = { request: vi.fn() };
    const client = createProductCatalogApi(transport);

    await expect(client.listPersonas("../escape")).rejects.toMatchObject({ kind: "invalid_request" });
    await expect(client.listWorldbooks("default", "x".repeat(201))).rejects.toMatchObject({ kind: "invalid_request" });
    await expect(client.createPersona("default", { name: "" })).rejects.toMatchObject({ kind: "invalid_request" });
    await expect(client.createWorldbook("default", {
      name: "Book",
      sections: [{ key: "", content: "Content" }],
    })).rejects.toMatchObject({ kind: "invalid_request" });
    await expect(client.createMoment("default", { body: "", coverFileId: null })).rejects.toMatchObject({ kind: "invalid_request" });
    await expect(client.updatePersona("default", PERSONA.id, { name: "Valid" }, '"product-moment-1"')).rejects.toMatchObject({ kind: "invalid_request" });
    await expect(client.addMomentComment("default", MOMENT.id, { text: "Comment", leaked: true } as never, '"product-moment-1"')).rejects.toMatchObject({ kind: "invalid_request" });
    await expect(client.setMomentLike("default", MOMENT.id, { liked: "yes" } as never, '"product-moment-1"')).rejects.toMatchObject({ kind: "invalid_request" });
    expect(transport.request).not.toHaveBeenCalled();
  });

  it("requires mutation ETags to exactly match the returned revision", async () => {
    for (const response of [
      jsonResponse(PERSONA, 201),
      jsonResponse(PERSONA, 201, { ETag: "W/\"product-persona-1\"" }),
      jsonResponse(PERSONA, 201, { ETag: '"product-persona-2"' }),
      jsonResponse({ ...PERSONA, revision: 2 }, 201, { ETag: '"product-worldbook-2"' }),
    ]) {
      await expect(createProductCatalogApi({ request: async () => response }).createPersona(
        "default",
        { name: PERSONA.name },
      )).rejects.toMatchObject({ kind: "invalid_response" });
    }
  });

  it("returns sanitized HTTP errors and strictly validates Problem envelopes", async () => {
    await expect(createProductCatalogApi({
      request: async () => jsonResponse(PROBLEM, 409),
    }).deletePersona("default", PERSONA.id, '"product-persona-1"')).rejects.toMatchObject({
      kind: "http",
      status: 409,
      code: "revision_conflict",
      requestId: "req-product-1",
      retryable: false,
      message: "Product revision conflict",
    });

    for (const response of [
      jsonResponse({ ...PROBLEM, status: 412 }, 409),
      jsonResponse({ ...PROBLEM, leaked: true }, 409),
      new Response("plain", { status: 503, headers: { "Content-Type": "text/plain" } }),
    ]) {
      await expect(createProductCatalogApi({ request: async () => response }).deletePersona(
        "default",
        PERSONA.id,
        '"product-persona-1"',
      )).rejects.toMatchObject({ kind: "invalid_response" });
    }
  });
});
