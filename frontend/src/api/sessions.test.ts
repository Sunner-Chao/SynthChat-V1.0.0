import { describe, expect, it, vi } from "vitest";
import type { DesktopTransport } from "./desktopConnection";
import {
  createSessionsApi,
  parseMessage,
  parseMessagePage,
  parseProblemDetails,
  parseSession,
  parseSessionPage,
  SessionApiError,
  type Message,
  type MessagePage,
  type ProblemDetails,
  type Session,
  type SessionPage,
} from "./sessions";

const NOW = "2026-07-16T08:00:00Z";

const MATCH = {
  field: "message" as const,
  messageId: "message-1",
  snippet: "matched text",
  ranges: [{ start: 0, end: 7 }],
};

const SESSION: Session = {
  id: "session-1",
  profileId: "default",
  personaId: null,
  title: "Architecture review",
  preview: "Review the migration plan",
  source: "desktop",
  model: "gpt-5",
  messageCount: 2,
  archived: false,
  revision: "session_rev_1",
  createdAt: NOW,
  updatedAt: NOW,
  match: null,
};

const MESSAGE: Message = {
  id: "message-1",
  sessionId: SESSION.id,
  sequence: 1,
  role: "assistant",
  parts: [
    { type: "text", text: "Result" },
    { type: "file", fileId: "file-1", name: "result.txt", mimeType: "text/plain" },
  ],
  reasoning: "Checked the source.",
  toolCalls: [{
    callId: "call-1",
    name: "read_file",
    status: "completed",
    inputSummary: "README.md",
    resultSummary: "Read 20 lines",
    artifacts: [{
      id: "artifact-1",
      name: "result.txt",
      mimeType: "text/plain",
      sizeBytes: 12,
      createdAt: NOW,
    }],
  }],
  usage: { promptTokens: 10, completionTokens: 5, totalTokens: 15, cost: 0.01 },
  createdAt: NOW,
};

const MESSAGE_PAGE: MessagePage = {
  items: [MESSAGE],
  nextCursor: "next-message-page",
  snapshotLastSequence: 3,
  firstSequence: 1,
  lastSequence: 1,
};

const PROBLEM: ProblemDetails = {
  type: "about:blank",
  title: "Revision conflict",
  status: 409,
  detail: "Reload before retrying.",
  instance: "/api/v1/sessions/session-1",
  code: "revision_conflict",
  requestId: "request-1",
  retryable: false,
};

function jsonResponse(value: unknown, status = 200, headers: HeadersInit = {}): Response {
  return new Response(JSON.stringify(value), {
    status,
    headers: {
      "Content-Type": "application/json; charset=utf-8",
      ...Object.fromEntries(new Headers(headers)),
    },
  });
}

function versionedResponse(value: Session, status = 200): Response {
  return jsonResponse(value, status, { ETag: `"${value.revision}"` });
}

function expectInvalid(parser: (value: unknown) => unknown, value: unknown): void {
  expect(() => parser(value)).toThrowError(
    expect.objectContaining<Partial<SessionApiError>>({ kind: "invalid_response" }),
  );
}

describe("Session API runtime contract", () => {
  it("parses complete Session, search, Message, tool, file, usage, and Problem shapes", () => {
    expect(parseSession({ ...SESSION, match: MATCH })).toEqual({ ...SESSION, match: MATCH });
    expect(parseSession({
      ...SESSION,
      personaId: "persona_0123456789abcdef0123456789abcdef",
    })).toMatchObject({ personaId: "persona_0123456789abcdef0123456789abcdef" });
    expect(parseSession({
      ...SESSION,
      match: { ...MATCH, field: "title", messageId: null },
    })).toMatchObject({ match: { field: "title", messageId: null } });
    expect(parseSessionPage({ items: [SESSION], nextCursor: "cursor-1" })).toEqual({
      items: [SESSION],
      nextCursor: "cursor-1",
    });
    expect(parseMessage(MESSAGE)).toEqual(MESSAGE);
    expect(parseMessage({
      ...MESSAGE,
      reasoning: null,
      usage: { promptTokens: 0, completionTokens: 0, totalTokens: 0, cost: null },
      toolCalls: [{ callId: "call-2", name: "shell", status: "running" }],
    })).toMatchObject({ reasoning: null, usage: { cost: null } });
    expect(parseMessagePage(MESSAGE_PAGE)).toEqual(MESSAGE_PAGE);
    expect(parseMessagePage({
      items: [],
      nextCursor: null,
      snapshotLastSequence: 0,
      firstSequence: null,
      lastSequence: null,
    })).toMatchObject({ items: [], snapshotLastSequence: 0 });
    expect(parseProblemDetails(PROBLEM)).toEqual(PROBLEM);
  });

  it.each([
    ["non-object", null],
    ["array", []],
    ["missing key", (({ match: _match, ...rest }) => rest)(SESSION)],
    ["unknown key", { ...SESSION, token: "leak" }],
    ["empty ID", { ...SESSION, id: "" }],
    ["invalid Profile", { ...SESSION, profileId: "../escape" }],
    ["invalid Persona", { ...SESSION, personaId: "persona-invalid" }],
    ["empty title", { ...SESSION, title: "" }],
    ["long title", { ...SESSION, title: "x".repeat(501) }],
    ["negative count", { ...SESSION, messageCount: -1 }],
    ["fractional count", { ...SESSION, messageCount: 1.5 }],
    ["non-boolean archived", { ...SESSION, archived: 0 }],
    ["invalid revision", { ...SESSION, revision: "bad\"revision" }],
    ["invalid created date", { ...SESSION, createdAt: "today" }],
    ["invalid updated date", { ...SESSION, updatedAt: "2026-15-99T00:00:00Z" }],
    ["invalid match field", { ...SESSION, match: { ...MATCH, field: "preview" } }],
    ["message match without ID", { ...SESSION, match: { ...MATCH, messageId: null } }],
    ["title match with ID", { ...SESSION, match: { ...MATCH, field: "title" } }],
    ["non-array ranges", { ...SESSION, match: { ...MATCH, ranges: {} } }],
    ["unknown range key", { ...SESSION, match: { ...MATCH, ranges: [{ start: 0, end: 1, extra: true }] } }],
    ["negative range", { ...SESSION, match: { ...MATCH, ranges: [{ start: -1, end: 1 }] } }],
    ["empty range", { ...SESSION, match: { ...MATCH, ranges: [{ start: 1, end: 1 }] } }],
    ["range past snippet", { ...SESSION, match: { ...MATCH, ranges: [{ start: 0, end: 99 }] } }],
    ["overlapping ranges", { ...SESSION, match: { ...MATCH, ranges: [{ start: 0, end: 3 }, { start: 2, end: 4 }] } }],
  ])("rejects invalid Session %s", (_case, value) => {
    expectInvalid(parseSession, value);
  });

  it.each([
    ["non-object", null],
    ["unknown field", { ...MESSAGE, trace: true }],
    ["missing reasoning", (({ reasoning: _reasoning, ...rest }) => rest)(MESSAGE)],
    ["missing usage", (({ usage: _usage, ...rest }) => rest)(MESSAGE)],
    ["empty ID", { ...MESSAGE, id: "" }],
    ["empty Session ID", { ...MESSAGE, sessionId: "" }],
    ["zero sequence", { ...MESSAGE, sequence: 0 }],
    ["fractional sequence", { ...MESSAGE, sequence: 1.5 }],
    ["invalid role", { ...MESSAGE, role: "developer" }],
    ["parts container", { ...MESSAGE, parts: {} }],
    ["unknown part", { ...MESSAGE, parts: [{ type: "video" }] }],
    ["text part extra", { ...MESSAGE, parts: [{ type: "text", text: "x", html: "x" }] }],
    ["text part value", { ...MESSAGE, parts: [{ type: "text", text: 2 }] }],
    ["file part empty ID", { ...MESSAGE, parts: [{ type: "file", fileId: "", name: "x", mimeType: "x" }] }],
    ["file part missing key", { ...MESSAGE, parts: [{ type: "file", fileId: "f", name: "x" }] }],
    ["tool container", { ...MESSAGE, toolCalls: {} }],
    ["tool extra", { ...MESSAGE, toolCalls: [{ callId: "c", name: "x", status: "completed", extra: true }] }],
    ["tool empty ID", { ...MESSAGE, toolCalls: [{ callId: "", name: "x", status: "completed" }] }],
    ["tool empty name", { ...MESSAGE, toolCalls: [{ callId: "c", name: "", status: "completed" }] }],
    ["tool status", { ...MESSAGE, toolCalls: [{ callId: "c", name: "x", status: "pending" }] }],
    ["tool summary", { ...MESSAGE, toolCalls: [{ callId: "c", name: "x", status: "failed", inputSummary: 1 }] }],
    ["artifact container", { ...MESSAGE, toolCalls: [{ callId: "c", name: "x", status: "completed", artifacts: {} }] }],
    ["artifact unknown", { ...MESSAGE, toolCalls: [{ callId: "c", name: "x", status: "completed", artifacts: [{ ...MESSAGE.toolCalls[0]!.artifacts![0]!, extra: true }] }] }],
    ["artifact size", { ...MESSAGE, toolCalls: [{ callId: "c", name: "x", status: "completed", artifacts: [{ ...MESSAGE.toolCalls[0]!.artifacts![0]!, sizeBytes: -1 }] }] }],
    ["artifact oversized", { ...MESSAGE, toolCalls: [{ callId: "c", name: "x", status: "completed", artifacts: [{ ...MESSAGE.toolCalls[0]!.artifacts![0]!, sizeBytes: 8 * 1024 * 1024 + 1 }] }] }],
    ["artifact MIME", { ...MESSAGE, toolCalls: [{ callId: "c", name: "x", status: "completed", artifacts: [{ ...MESSAGE.toolCalls[0]!.artifacts![0]!, mimeType: "application/x-untrusted" }] }] }],
    ["reasoning", { ...MESSAGE, reasoning: 3 }],
    ["usage missing", { ...MESSAGE, usage: { promptTokens: 1, completionTokens: 2 } }],
    ["usage negative", { ...MESSAGE, usage: { promptTokens: -1, completionTokens: 2, totalTokens: 1 } }],
    ["usage cost", { ...MESSAGE, usage: { promptTokens: 1, completionTokens: 2, totalTokens: 3, cost: Number.NaN } }],
    ["created date", { ...MESSAGE, createdAt: "soon" }],
  ])("rejects invalid Message %s", (_case, value) => {
    expectInvalid(parseMessage, value);
  });

  it.each([
    ["Session page extra", parseSessionPage, { items: [], nextCursor: null, extra: true }],
    ["Session items", parseSessionPage, { items: {}, nextCursor: null }],
    ["Session cursor empty", parseSessionPage, { items: [], nextCursor: "" }],
    ["Session cursor long", parseSessionPage, { items: [], nextCursor: "x".repeat(4_097) }],
    ["Message page extra", parseMessagePage, { ...MESSAGE_PAGE, extra: true }],
    ["Message items", parseMessagePage, { ...MESSAGE_PAGE, items: {} }],
    ["Message order", parseMessagePage, {
      ...MESSAGE_PAGE,
      items: [{ ...MESSAGE, sequence: 2 }, { ...MESSAGE, id: "message-2", sequence: 1 }],
      firstSequence: 2,
      lastSequence: 1,
    }],
    ["empty bounds", parseMessagePage, { ...MESSAGE_PAGE, items: [], firstSequence: 1, lastSequence: null }],
    ["first bound", parseMessagePage, { ...MESSAGE_PAGE, firstSequence: 2 }],
    ["last bound", parseMessagePage, { ...MESSAGE_PAGE, lastSequence: 2 }],
    ["snapshot bound", parseMessagePage, { ...MESSAGE_PAGE, snapshotLastSequence: 0 }],
    ["negative snapshot", parseMessagePage, { ...MESSAGE_PAGE, snapshotLastSequence: -1 }],
    ["invalid first type", parseMessagePage, { ...MESSAGE_PAGE, firstSequence: "1" }],
  ] satisfies Array<[string, (value: unknown) => unknown, unknown]>) (
    "rejects invalid page invariant: %s",
    (_case, parser, value) => expectInvalid(parser, value),
  );

  it.each([
    ["unknown", { ...PROBLEM, debug: true }],
    ["low status", { ...PROBLEM, status: 399 }],
    ["high status", { ...PROBLEM, status: 600 }],
    ["fractional status", { ...PROBLEM, status: 409.5 }],
    ["retryable", { ...PROBLEM, retryable: "no" }],
    ["detail", { ...PROBLEM, detail: 7 }],
  ])("rejects invalid Problem %s", (_case, value) => {
    expectInvalid(parseProblemDetails, value);
  });

  it("exercises every endpoint, cursor, filter, header, body, and request option", async () => {
    const requests: Array<{ path: string; init: RequestInit; signal?: AbortSignal }> = [];
    const controller = new AbortController();
    const transport: DesktopTransport = {
      request: async (path, init = {}, options = {}) => {
        requests.push({ path, init, signal: options.signal });
        const method = init.method ?? "GET";
        if (path.includes("/messages")) return jsonResponse(MESSAGE_PAGE);
        if (path.includes("?") && method === "GET") {
          return jsonResponse({ items: [SESSION], nextCursor: "session-cursor" });
        }
        if (method === "POST") return versionedResponse(SESSION, 201);
        if (method === "PATCH") {
          return versionedResponse({ ...SESSION, title: "Renamed", revision: "session_rev_2" });
        }
        if (method === "DELETE") return new Response(null, { status: 204 });
        return versionedResponse(SESSION);
      },
    };
    const client = createSessionsApi(transport);
    const options = { signal: controller.signal };

    await client.listSessions({
      profileId: "default",
      q: "review plan",
      archived: false,
      cursor: "session page",
      limit: 25,
    }, options);
    await client.searchSessions({ profileId: "default", query: "review", archived: false }, options);
    await client.createSession({ profileId: "default", title: "Architecture review" }, "create-session-001", options);
    await client.getSession("session-1", options);
    await client.updateSession("session-1", { title: "Renamed", archived: false }, '"session_rev_1"', options);
    await client.deleteSession("session-1", '"session_rev_2"', options);
    await client.listMessages("session-1", { cursor: "message page", limit: 10 }, options);
    await client.listMessages("session-1", {}, options);

    expect(requests).toHaveLength(8);
    expect(requests.every((request) => request.signal === controller.signal)).toBe(true);
    expect(requests[0]!.path).toBe(
      "/api/v1/sessions?profileId=default&q=review+plan&archived=false&cursor=session+page&limit=25",
    );
    expect(requests[1]!.path).toContain("q=review");
    expect(requests[6]!.path).toBe(
      "/api/v1/sessions/session-1/messages?cursor=message+page&limit=10",
    );
    expect(requests[7]!.path).toBe("/api/v1/sessions/session-1/messages");
    expect(new Headers(requests[2]!.init.headers).get("Idempotency-Key")).toBe("create-session-001");
    expect(JSON.parse(String(requests[2]!.init.body))).toEqual({
      profileId: "default",
      title: "Architecture review",
    });
    expect(new Headers(requests[4]!.init.headers).get("If-Match")).toBe('"session_rev_1"');
    expect(new Headers(requests[4]!.init.headers).get("Content-Type")).toBe(
      "application/merge-patch+json",
    );
    expect(new Headers(requests[5]!.init.headers).get("If-Match")).toBe('"session_rev_2"');
  });

  it("allows an absent-resource DELETE replay without If-Match", async () => {
    let headers: HeadersInit | undefined;
    const client = createSessionsApi({
      request: async (_path, init = {}) => {
        headers = init.headers;
        return new Response(null, { status: 204 });
      },
    });

    await client.deleteSession("already-deleted");

    expect(new Headers(headers).has("If-Match")).toBe(false);
  });

  it("supports default list filters, archived pages, generated titles, and encoded IDs", async () => {
    const paths: string[] = [];
    const archived = { ...SESSION, id: "id/with space", archived: true };
    const transport: DesktopTransport = {
      request: async (path, init = {}) => {
        paths.push(path);
        if (path.includes("?")) return jsonResponse({ items: [archived], nextCursor: null });
        if (init.method === "POST") return versionedResponse(SESSION, 201);
        return versionedResponse(archived);
      },
    };
    const client = createSessionsApi(transport);

    await client.listSessions({ profileId: "default", archived: true });
    await client.createSession({ profileId: "default", title: null }, "create-session-002");
    await client.getSession("id/with space");

    expect(paths[0]).toBe("/api/v1/sessions?profileId=default&archived=true");
    expect(paths[2]).toBe("/api/v1/sessions/id%2Fwith%20space");
  });

  it("creates and binds a Session to a strictly validated Persona ID", async () => {
    const personaId = "persona_0123456789abcdef0123456789abcdef";
    const request = vi.fn(async (_path: string, _init?: RequestInit) => (
      versionedResponse({ ...SESSION, personaId }, 201)
    ));
    const client = createSessionsApi({ request });

    await expect(client.createSession(
      { profileId: "default", personaId, title: "Persona chat" },
      "create-persona-session-001",
    )).resolves.toMatchObject({ value: { personaId } });
    expect(JSON.parse(String(request.mock.calls[0]![1]?.body))).toEqual({
      profileId: "default",
      personaId,
      title: "Persona chat",
    });
  });

  it("enforces Session title limits by Unicode scalar rather than UTF-16 units", async () => {
    const boundaryTitle = "\u{1f642}".repeat(500);
    let requestCount = 0;
    const client = createSessionsApi({
      request: async () => {
        requestCount += 1;
        return versionedResponse({ ...SESSION, title: boundaryTitle }, 201);
      },
    });

    await expect(client.createSession(
      { profileId: "default", title: boundaryTitle },
      "create-emoji-session-500",
    )).resolves.toMatchObject({ value: { title: boundaryTitle } });
    await expect(client.createSession(
      { profileId: "default", title: `${boundaryTitle}\u{1f642}` },
      "create-emoji-session-501",
    )).rejects.toMatchObject({ kind: "invalid_request" });
    expect(requestCount).toBe(1);
  });

  it("rejects response filter and resource binding violations", async () => {
    const cases: Array<() => Promise<unknown>> = [
      () => createSessionsApi({
        request: async () => jsonResponse({ items: [{ ...SESSION, profileId: "work" }], nextCursor: null }),
      }).listSessions({ profileId: "default" }),
      () => createSessionsApi({
        request: async () => jsonResponse({ items: [{ ...SESSION, archived: true }], nextCursor: null }),
      }).listSessions({ profileId: "default" }),
      () => createSessionsApi({
        request: async () => versionedResponse({ ...SESSION, profileId: "work" }, 201),
      }).createSession({ profileId: "default" }, "create-session-003"),
      () => createSessionsApi({
        request: async () => versionedResponse({ ...SESSION, archived: true }, 201),
      }).createSession({ profileId: "default" }, "create-session-004"),
      () => createSessionsApi({
        request: async () => versionedResponse({ ...SESSION, id: "other" }),
      }).getSession("session-1"),
      () => createSessionsApi({
        request: async () => versionedResponse({ ...SESSION, id: "other" }),
      }).updateSession("session-1", { archived: true }, '"session_rev_1"'),
      () => createSessionsApi({
        request: async () => jsonResponse({ ...MESSAGE_PAGE, items: [{ ...MESSAGE, sessionId: "other" }] }),
      }).listMessages("session-1"),
    ];

    for (const operation of cases) {
      await expect(operation()).rejects.toMatchObject({ kind: "invalid_response" });
    }
  });

  it("returns sanitized HTTP errors with request metadata and current ETag", async () => {
    const client = createSessionsApi({
      request: async () => jsonResponse(PROBLEM, 409, {
        "Content-Type": "application/problem+json",
        ETag: '"session_rev_current"',
      }),
    });

    await expect(client.updateSession(
      "session-1",
      { title: "New title" },
      '"session_rev_old"',
    )).rejects.toMatchObject({
      kind: "http",
      status: 409,
      code: "revision_conflict",
      requestId: "request-1",
      retryable: false,
      etag: '"session_rev_current"',
    });
  });

  it("rejects invalid success and error envelopes", async () => {
    const operations: Array<() => Promise<unknown>> = [
      () => createSessionsApi({ request: async () => new Response("text") })
        .listSessions({ profileId: "default" }),
      () => createSessionsApi({
        request: async () => new Response("{", { headers: { "Content-Type": "application/json" } }),
      }).listSessions({ profileId: "default" }),
      () => createSessionsApi({ request: async () => versionedResponse(SESSION, 200) })
        .createSession({ profileId: "default" }, "create-session-005"),
      () => createSessionsApi({ request: async () => jsonResponse(SESSION) })
        .getSession("session-1"),
      () => createSessionsApi({
        request: async () => jsonResponse(SESSION, 200, { ETag: '"different"' }),
      }).getSession("session-1"),
      () => createSessionsApi({
        request: async () => jsonResponse({ ...PROBLEM, status: 400 }, 500),
      }).getSession("session-1"),
      () => createSessionsApi({
        request: async () => new Response(null, { status: 200 }),
      }).deleteSession("session-1", '"session_rev_1"'),
    ];

    for (const operation of operations) {
      await expect(operation()).rejects.toMatchObject({ kind: "invalid_response" });
    }
  });

  it("rejects every invalid request before transport", async () => {
    const transport: DesktopTransport = { request: vi.fn(async () => new Response(null, { status: 204 })) };
    const client = createSessionsApi(transport);
    const operations: Array<() => Promise<unknown>> = [
      () => client.listSessions({ profileId: "../escape" }),
      () => client.listSessions({ profileId: "default", q: "x".repeat(501) }),
      () => client.listSessions({ profileId: "default", cursor: "" }),
      () => client.listSessions({ profileId: "default", cursor: "x".repeat(4_097) }),
      () => client.listSessions({ profileId: "default", limit: 0 }),
      () => client.listSessions({ profileId: "default", limit: 101 }),
      () => client.listSessions({ profileId: "default", limit: 1.5 }),
      () => client.listSessions({ profileId: "default", archived: "yes" } as never),
      () => client.listSessions({ profileId: "default", extra: true } as never),
      () => client.listSessions(null as never),
      () => client.searchSessions({ profileId: "default", query: "   " }),
      () => client.searchSessions({ profileId: "default", query: 7 } as never),
      () => client.searchSessions({ profileId: "default", query: "x", extra: true } as never),
      () => client.createSession({ profileId: "bad/profile" }, "create-session-006"),
      () => client.createSession({ profileId: "default", title: "" }, "create-session-007"),
      () => client.createSession({ profileId: "default", title: "x".repeat(501) }, "create-session-008"),
      () => client.createSession({ profileId: "default", personaId: "persona-invalid" }, "create-session-008b"),
      () => client.createSession({ profileId: "default", extra: true } as never, "create-session-009"),
      () => client.createSession({} as never, "create-session-010"),
      () => client.createSession(null as never, "create-session-011"),
      () => client.createSession({ profileId: "default", title: 7 } as never, "create-session-012"),
      () => client.createSession({ profileId: "default" }, "short"),
      () => client.getSession(""),
      () => client.updateSession("session-1", {}, '"session_rev_1"'),
      () => client.updateSession("session-1", { title: "" }, '"session_rev_1"'),
      () => client.updateSession("session-1", { archived: "yes" } as never, '"session_rev_1"'),
      () => client.updateSession("session-1", { extra: true } as never, '"session_rev_1"'),
      () => client.updateSession("session-1", { archived: true }, "weak"),
      () => client.deleteSession("session-1", "*"),
      () => client.listMessages("session-1", { limit: 0 }),
      () => client.listMessages("session-1", { extra: true } as never),
      () => client.listMessages(7 as never),
    ];

    for (const operation of operations) {
      await expect(operation()).rejects.toMatchObject({ kind: "invalid_request" });
    }
    expect(transport.request).not.toHaveBeenCalled();
  });
});
