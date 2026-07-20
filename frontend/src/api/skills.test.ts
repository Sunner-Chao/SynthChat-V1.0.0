import { describe, expect, it, vi } from "vitest";
import type { DesktopTransport } from "./desktopConnection";
import {
  SkillApiError,
  createSkillsApi,
  parseOperation,
  parseSkill,
  parseSkillPage,
  type InstallSkillInput,
} from "./skills";

const SKILL = {
  id: "skill_0123456789abcdef0123456789abcdef",
  name: "paper-search",
  description: "Find papers",
  source: "local",
  version: "1.2.3",
  enabled: true,
  configurable: false,
  uninstallable: true,
} as const;

const OPERATION = {
  id: "op_0123456789abcdef0123456789abcdef",
  kind: "skillInstall",
  status: "queued",
  createdAt: "2026-07-17T08:00:00Z",
  updatedAt: "2026-07-17T08:00:00Z",
} as const;

function jsonResponse(
  body: unknown,
  status = 200,
  headers: Record<string, string> = {},
): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: {
      "Content-Type": status >= 400 ? "application/problem+json" : "application/json",
      ...headers,
    },
  });
}

function transport(response: Response): DesktopTransport & { request: ReturnType<typeof vi.fn> } {
  return {
    request: vi.fn(async () => response),
  };
}

describe("Skills API", () => {
  it("strictly parses Skill resources and pages", () => {
    expect(parseSkill(SKILL)).toEqual(SKILL);
    expect(parseSkillPage({ items: [SKILL], nextCursor: null })).toEqual({
      items: [SKILL],
      nextCursor: null,
    });
    for (const invalid of [
      { ...SKILL, source: "remote" },
      { ...SKILL, enabled: "yes" },
      { ...SKILL, uninstallable: "yes" },
      { ...SKILL, unknown: true },
      { items: [SKILL, SKILL], nextCursor: null },
      { items: [SKILL] },
    ]) {
      expect(() => "items" in invalid ? parseSkillPage(invalid) : parseSkill(invalid))
        .toThrow(SkillApiError);
    }
    expect(parseSkill({ ...SKILL, source: "file" })).toEqual({ ...SKILL, source: "file" });
  });

  it("strictly parses asynchronous operations and nested Problems", () => {
    expect(parseOperation(OPERATION)).toEqual(OPERATION);
    expect(parseOperation({
      ...OPERATION,
      status: "failed",
      error: {
        type: "about:blank",
        title: "Installation failed",
        status: 422,
        detail: "private diagnostic",
        code: "skill_install_failed",
        requestId: "req-operation",
        retryable: false,
      },
      updatedAt: "2026-07-17T08:00:01Z",
    })).toMatchObject({ status: "failed", error: { code: "skill_install_failed" } });

    for (const invalid of [
      { ...OPERATION, status: "unknown" },
      { ...OPERATION, id: "operation-1" },
      { ...OPERATION, kind: "skill.install" },
      { ...OPERATION, updatedAt: "not-a-date" },
      { ...OPERATION, updatedAt: "2026-07-17T07:59:59Z" },
      { ...OPERATION, extra: true },
      {
        ...OPERATION,
        error: {
          type: "about:blank",
          title: "failed",
          status: 422,
          code: "failed",
          requestId: "req",
          retryable: false,
          secret: "must not be accepted",
        },
      },
    ]) {
      expect(() => parseOperation(invalid)).toThrow(SkillApiError);
    }
  });

  it("lists one encoded page and requires the Profile ETag", async () => {
    const mock = transport(jsonResponse(
      { items: [SKILL], nextCursor: "next.page" },
      200,
      { ETag: '"config-1"' },
    ));
    const api = createSkillsApi(mock);
    await expect(api.listSkills(
      "work",
      { query: "paper & code", cursor: "cursor.page", limit: 20 },
    )).resolves.toEqual({
      value: { items: [SKILL], nextCursor: "next.page" },
      etag: '"config-1"',
    });
    expect(mock.request).toHaveBeenCalledWith(
      "/api/v1/profiles/work/skills?q=paper+%26+code&cursor=cursor.page&limit=20",
      { method: "GET", headers: { Accept: "application/json" } },
      {},
    );

    const missingEtag = createSkillsApi(transport(jsonResponse({
      items: [],
      nextCursor: null,
    })));
    await expect(missingEtag.listSkills("default")).rejects.toMatchObject({
      kind: "invalid_response",
    });
  });

  it("sends an exact conditional enablement patch", async () => {
    const mock = transport(jsonResponse(
      { ...SKILL, enabled: false },
      200,
      { ETag: '"config-2"' },
    ));
    const api = createSkillsApi(mock);
    await expect(api.updateSkill("default", SKILL.id, false, '"config-1"'))
      .resolves.toEqual({
        value: { ...SKILL, enabled: false },
        etag: '"config-2"',
      });
    expect(mock.request).toHaveBeenCalledWith(
      `/api/v1/profiles/default/skills/${SKILL.id}`,
      {
        method: "PATCH",
        headers: {
          Accept: "application/json",
          "Content-Type": "application/merge-patch+json",
          "If-Match": '"config-1"',
        },
        body: JSON.stringify({ enabled: false }),
      },
      {},
    );
  });

  it("rejects invalid local inputs before transport", async () => {
    const mock = transport(jsonResponse({ items: [], nextCursor: null }));
    const api = createSkillsApi(mock);
    await expect(api.listSkills("INVALID")).rejects.toMatchObject({ kind: "invalid_request" });
    await expect(api.listSkills("default", { limit: 0 })).rejects.toMatchObject({
      kind: "invalid_request",
    });
    await expect(api.updateSkill("default", "", true, '"config"')).rejects.toMatchObject({
      kind: "invalid_request",
    });
    await expect(api.updateSkill("default", SKILL.id, true, "weak")).rejects.toMatchObject({
      kind: "invalid_request",
    });
    expect(mock.request).not.toHaveBeenCalled();
  });

  it("preserves conflict metadata from problem details", async () => {
    const mock = transport(jsonResponse(
      {
        type: "about:blank",
        title: "Revision conflict",
        status: 409,
        code: "revision_conflict",
        requestId: "req-1",
        retryable: false,
      },
      409,
      { ETag: '"config-current"' },
    ));
    const api = createSkillsApi(mock);
    await expect(api.updateSkill("default", SKILL.id, false, '"config-old"'))
      .rejects.toMatchObject({
        kind: "http",
        status: 409,
        code: "revision_conflict",
        requestId: "req-1",
        etag: '"config-current"',
      });
  });

  it("accepts an installation conflict Problem without a Profile ETag", async () => {
    const mock = transport(jsonResponse(
      {
        type: "about:blank",
        title: "Idempotency conflict",
        status: 409,
        code: "idempotency_conflict",
        requestId: "req-install-conflict",
        retryable: false,
      },
      409,
    ));
    await expect(createSkillsApi(mock).installSkill(
      "default",
      { registryId: "paper-search" },
      "install-skill-0001",
    )).rejects.toMatchObject({
      kind: "http",
      status: 409,
      code: "idempotency_conflict",
      etag: undefined,
    });
  });

  it("installs, polls, and uninstalls through exact asynchronous endpoints", async () => {
    const request = vi.fn()
      .mockResolvedValueOnce(jsonResponse(OPERATION, 202))
      .mockResolvedValueOnce(jsonResponse({
        ...OPERATION,
        status: "completed",
        updatedAt: "2026-07-17T08:00:01Z",
      }))
      .mockResolvedValueOnce(jsonResponse({
        ...OPERATION,
        id: "op_fedcba9876543210fedcba9876543210",
        kind: "skillUninstall",
      }, 202));
    const api = createSkillsApi({ request });

    await expect(api.installSkill(
      "default",
      { registryId: "paper-search" },
      "install-skill-0001",
    )).resolves.toEqual(OPERATION);
    expect(request).toHaveBeenNthCalledWith(
      1,
      "/api/v1/profiles/default/skills/install",
      {
        method: "POST",
        headers: {
          Accept: "application/json",
          "Content-Type": "application/json",
          "Idempotency-Key": "install-skill-0001",
        },
        body: JSON.stringify({ registryId: "paper-search" }),
      },
      {},
    );

    await expect(api.getOperation(OPERATION.id)).resolves.toMatchObject({
      id: OPERATION.id,
      status: "completed",
    });
    expect(request).toHaveBeenNthCalledWith(
      2,
      `/api/v1/operations/${OPERATION.id}`,
      { method: "GET", headers: { Accept: "application/json" } },
      {},
    );

    await expect(api.uninstallSkill(
      "default",
      SKILL.id,
      "uninstall-skill-0001",
    )).resolves.toMatchObject({
      id: "op_fedcba9876543210fedcba9876543210",
      kind: "skillUninstall",
    });
    expect(request).toHaveBeenNthCalledWith(
      3,
      `/api/v1/profiles/default/skills/${SKILL.id}`,
      {
        method: "DELETE",
        headers: {
          Accept: "application/json",
          "Idempotency-Key": "uninstall-skill-0001",
        },
      },
      {},
    );
  });

  it("rejects non-exclusive installation sources before transport", async () => {
    const mock = transport(jsonResponse(OPERATION, 202));
    const api = createSkillsApi(mock);
    for (const input of [
      {},
      { registryId: "one", url: "https://example.com/skill.zip" },
      { fileId: "" },
      { url: "file:///tmp/skill.zip" },
      { url: "http://example.com/SKILL.md" },
      { url: "https://example.com/skill.zip" },
      { registryId: "one", unexpected: true },
    ]) {
      await expect(api.installSkill(
        "default",
        input as unknown as InstallSkillInput,
        "install-skill-0001",
      )).rejects.toMatchObject({ kind: "invalid_request" });
    }
    await expect(api.installSkill(
      "default",
      { url: "https://example.com/skill.zip" },
      "short",
    )).rejects.toMatchObject({ kind: "invalid_request" });
    await expect(api.uninstallSkill(
      "default",
      SKILL.id,
      "short",
    )).rejects.toMatchObject({ kind: "invalid_request" });
    expect(mock.request).not.toHaveBeenCalled();
  });

  it("forwards AbortSignal while polling an operation", async () => {
    const controller = new AbortController();
    const request = vi.fn(async (
      _path: string,
      _init?: RequestInit,
      options?: { signal?: AbortSignal },
    ) => {
      expect(options?.signal).toBe(controller.signal);
      controller.abort();
      throw new DOMException("aborted", "AbortError");
    });
    const api = createSkillsApi({ request });
    await expect(api.getOperation(OPERATION.id, { signal: controller.signal }))
      .rejects.toMatchObject({ name: "AbortError" });
  });
});
