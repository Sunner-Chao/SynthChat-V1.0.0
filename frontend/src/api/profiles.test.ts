import { describe, expect, it, vi } from "vitest";
import type { DesktopTransport } from "./desktopConnection";
import { ALLOWED_FILE_MIME_TYPES, MAX_FILE_BYTES } from "./fileContract";
import {
  createProfilesApi,
  parseCapabilities,
  parseProfileConfig,
  parseProblemDetails,
  parseProvider,
  parseSecretStatus,
  parseProfileSummary,
  ProfileApiError,
  type Capabilities,
  type ProblemDetails,
  type ProfileConfig,
  type ProfileMetadata,
  type ProfileSummary,
  type Provider,
  type SecretStatus,
} from "./profiles";

const NOW = "2026-07-16T08:00:00Z";

export const CAPABILITIES: Capabilities = {
  contractVersion: "v1",
  backendVersion: "0.2.0",
  engine: {
    kind: "hermes-rust",
    available: true,
    version: "0.2.0",
    pinnedCommit: null,
    features: {
      runStreaming: false,
      reasoningStreaming: false,
      toolProgress: true,
      approvals: false,
      clarifications: false,
      asyncToolDelivery: false,
      profileManagement: true,
      skillManagement: false,
      memoryWrite: false,
      mcpManagement: false,
      oauthAccounts: false,
    },
  },
  sessionStorage: {
    available: true,
    schemaVersion: 1,
    hermesImportAvailable: false,
  },
  sessionSearch: { mode: "unavailable" },
  files: {
    maxBytes: MAX_FILE_BYTES,
    allowedMimeTypes: [...ALLOWED_FILE_MIME_TYPES],
  },
  extensions: {
    activeRunDiscovery: false,
    runQueue: false,
    toolsetManagement: true,
    toolExecution: true,
    codeExecution: true,
    workspaceManagement: true,
    skillDiscovery: true,
    skillEnablement: true,
    webSearch: true,
    webExtract: true,
    browserAutomation: false,
    browserCdp: false,
    browserDownloads: false,
    mcpStdio: false,
    mcpStreamableHttp: false,
    mcpSse: false,
    wechatAccounts: true,
    wechatMessaging: true,
    plugins: true,
    personas: true,
    moments: true,
    worldbooks: true,
  },
};

export const PROFILE: ProfileSummary = {
  id: "default",
  displayName: "Default",
  isDefault: true,
  isActive: true,
  color: null,
  avatarFileId: null,
  engineState: "stopped",
  configRevision: "rev_config_1",
  createdAt: null,
  updatedAt: NOW,
};

export const METADATA: ProfileMetadata = {
  id: "default",
  displayName: "Default",
  isDefault: true,
  color: null,
  avatarFileId: null,
  createdAt: null,
  updatedAt: NOW,
};

export const CONFIG: ProfileConfig = {
  revision: "rev_config_1",
  model: {
    provider: "openai",
    model: "gpt-5",
    baseUrl: null,
    reasoningEffort: null,
  },
  codeExecution: {
    mode: "project",
    timeoutSeconds: 300,
    maxToolCalls: 50,
  },
  toolsets: {},
  skills: {},
  memoryProvider: "builtin",
  platforms: {},
  extensions: {},
};

const PROVIDER: Provider = {
  id: "openai",
  displayName: "OpenAI",
  defaultBaseUrl: "https://api.openai.com/v1",
  requiresSecret: true,
  secretNames: ["OPENAI_API_KEY"],
  supportsModelDiscovery: false,
};

const SECRET_STATUS: SecretStatus = {
  name: "OPENAI_API_KEY",
  configured: true,
  storage: "osKeychain",
  updatedAt: NOW,
};

const PROBLEM: ProblemDetails = {
  type: "about:blank",
  title: "Request failed",
  status: 400,
  code: "invalid_request",
  requestId: "req-1",
  retryable: false,
  detail: "Invalid input",
  instance: "/api/v1/profiles",
};

function jsonResponse(value: unknown, status = 200, headers: HeadersInit = {}): Response {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "Content-Type": "application/json", ...Object.fromEntries(new Headers(headers)) },
  });
}

describe("Profile API runtime contract", () => {
  it("strictly validates capabilities and profile summaries", () => {
    expect(parseCapabilities(CAPABILITIES)).toEqual(CAPABILITIES);
    expect(parseProfileSummary(PROFILE)).toEqual(PROFILE);
    expect(() => parseProfileSummary({ ...PROFILE, leaked: true })).toThrowError(
      expect.objectContaining<Partial<ProfileApiError>>({ kind: "invalid_response" }),
    );
    expect(() => parseCapabilities({
      ...CAPABILITIES,
      engine: { ...CAPABILITIES.engine, features: { ...CAPABILITIES.engine.features, profileManagement: "yes" } },
    })).toThrowError(expect.objectContaining<Partial<ProfileApiError>>({ kind: "invalid_response" }));
  });

  it("rejects undeclared Capabilities fields", () => {
    expect(() => parseCapabilities({ ...CAPABILITIES, leaked: true })).toThrowError(
      expect.objectContaining<Partial<ProfileApiError>>({ kind: "invalid_response" }),
    );
    expect(() => parseCapabilities({
      ...CAPABILITIES,
      engine: { ...CAPABILITIES.engine, leaked: true },
    })).toThrowError(
      expect.objectContaining<Partial<ProfileApiError>>({ kind: "invalid_response" }),
    );
  });

  it.each(["ftp://models.example.com", "file:///tmp/models", "mailto:models@example.com"])(
    "rejects a non-HTTP(S) model Base URL: %s",
    (baseUrl) => {
      expect(() => parseProfileConfig({
        ...CONFIG,
        model: { ...CONFIG.model, baseUrl },
      })).toThrowError(
        expect.objectContaining<Partial<ProfileApiError>>({ kind: "invalid_response" }),
      );
    },
  );

  it("enforces the 80 Unicode scalar Profile display-name boundary", async () => {
    const displayName = "\u{1f642}".repeat(80);
    let requestCount = 0;
    const transport: DesktopTransport = {
      request: async () => {
        requestCount += 1;
        return jsonResponse({
          ...METADATA,
          id: "emoji",
          displayName,
          isDefault: false,
        }, 201, { ETag: '"meta_emoji_1"' });
      },
    };
    const client = createProfilesApi(transport);

    await expect(client.createProfile(
      { id: "emoji", displayName, cloneFromProfileId: null },
      "create-emoji-80",
    )).resolves.toMatchObject({ value: { displayName } });
    await expect(client.createProfile(
      { id: "emoji", displayName: `${displayName}\u{1f642}`, cloneFromProfileId: null },
      "create-emoji-81",
    )).rejects.toMatchObject({ kind: "invalid_request" });
    expect(requestCount).toBe(1);
  });

  it("keeps metadata and config ETags as separate versioned resources", async () => {
    const transport: DesktopTransport = {
      request: async (path) => {
        if (path.endsWith("/config")) {
          return jsonResponse(CONFIG, 200, { ETag: '"rev_config_1"' });
        }
        return jsonResponse(METADATA, 200, { ETag: '"meta_7"' });
      },
    };
    const client = createProfilesApi(transport);

    const metadata = await client.getProfile("default");
    const config = await client.getProfileConfig("default");

    expect(metadata.etag).toBe('"meta_7"');
    expect(config.etag).toBe('"rev_config_1"');
    expect(metadata.value).not.toHaveProperty("configRevision");
  });

  it("sends each resource's own ETag with merge patch media types", async () => {
    const requests: Array<{ path: string; init: RequestInit }> = [];
    const updatedConfig = { ...CONFIG, revision: "rev_config_2" };
    const transport: DesktopTransport = {
      request: async (path, init = {}) => {
        requests.push({ path, init });
        if (path.endsWith("/config")) {
          return jsonResponse(updatedConfig, 200, { ETag: '"rev_config_2"' });
        }
        return jsonResponse({ ...METADATA, displayName: "Renamed" }, 200, { ETag: '"meta_8"' });
      },
    };
    const client = createProfilesApi(transport);

    await client.updateProfile("default", { displayName: "Renamed" }, '"meta_7"');
    await client.updateProfileConfig("default", {
      model: { model: "gpt-5.1" },
      codeExecution: { mode: "strict", timeoutSeconds: 120, maxToolCalls: 12 },
    }, '"rev_config_1"');

    expect(new Headers(requests[0]?.init.headers).get("If-Match")).toBe('"meta_7"');
    expect(new Headers(requests[1]?.init.headers).get("If-Match")).toBe('"rev_config_1"');
    expect(new Headers(requests[0]?.init.headers).get("Content-Type")).toBe("application/merge-patch+json");
    expect(new Headers(requests[1]?.init.headers).get("Content-Type")).toBe("application/merge-patch+json");
    expect(JSON.parse(String(requests[1]?.init.body))).toEqual({
      model: { model: "gpt-5.1" },
      codeExecution: { mode: "strict", timeoutSeconds: 120, maxToolCalls: 12 },
    });
  });

  it("forwards an idempotency key for Profile creation", async () => {
    let requestInit: RequestInit | undefined;
    const transport: DesktopTransport = {
      request: async (_path, init) => {
        requestInit = init;
        return jsonResponse({ ...METADATA, id: "work", displayName: "Work", isDefault: false }, 201, {
          ETag: '"meta_work_1"',
        });
      },
    };

    await createProfilesApi(transport).createProfile(
      { id: "work", displayName: "Work", cloneFromProfileId: "default" },
      "create-work-0001",
    );

    expect(new Headers(requestInit?.headers).get("Idempotency-Key")).toBe("create-work-0001");
  });

  it("returns a sanitized conflict with the current ETag", async () => {
    const transport: DesktopTransport = {
      request: async () => jsonResponse({
        type: "about:blank",
        title: "Revision conflict",
        status: 409,
        code: "revision_conflict",
        requestId: "req-7",
        retryable: false,
      }, 409, { ETag: '"rev_current"', "Content-Type": "application/problem+json" }),
    };

    await expect(createProfilesApi(transport).updateProfileConfig(
      "default",
      { model: { model: "new" } },
      '"rev_old"',
    )).rejects.toMatchObject({
      kind: "http",
      status: 409,
      code: "revision_conflict",
      requestId: "req-7",
      etag: '"rev_current"',
    });
  });

  it("parses all optional response fields and non-null extension JSON", () => {
    expect(parseCapabilities({
      ...CAPABILITIES,
      extensions: { ...CAPABILITIES.extensions, trace: true },
    })).toMatchObject({
      extensions: { ...CAPABILITIES.extensions, trace: true },
    });
    expect(parseProvider(PROVIDER)).toEqual(PROVIDER);
    expect(parseProfileSummary({
      ...PROFILE,
      color: "#A1b2C3",
      avatarFileId: "avatar-1",
      createdAt: NOW,
    })).toMatchObject({ color: "#A1b2C3", avatarFileId: "avatar-1", createdAt: NOW });
    expect(parseProfileConfig({
      ...CONFIG,
      model: { ...CONFIG.model, reasoningEffort: "high" },
      toolsets: { browser: true },
      skills: { planner: false },
      platforms: { cli: true },
      extensions: {
        text: "value",
        count: 2,
        enabled: true,
        nested: [1, { ok: true }],
      },
    })).toMatchObject({
      model: { reasoningEffort: "high" },
      toolsets: { browser: true },
      extensions: { nested: [1, { ok: true }] },
    });
    expect(parseSecretStatus(SECRET_STATUS)).toEqual(SECRET_STATUS);
    expect(parseProblemDetails(PROBLEM)).toEqual(PROBLEM);
  });

  it.each([
    ["contract version", { ...CAPABILITIES, contractVersion: "v2" }],
    ["engine kind", { ...CAPABILITIES, engine: { ...CAPABILITIES.engine, kind: "python" } }],
    ["session search mode", { ...CAPABILITIES, sessionSearch: { mode: "elastic" } }],
    ["session storage unavailable type", { ...CAPABILITIES, sessionStorage: { ...CAPABILITIES.sessionStorage, available: "yes" } }],
    ["session storage schema zero", { ...CAPABILITIES, sessionStorage: { ...CAPABILITIES.sessionStorage, schemaVersion: 0 } }],
    ["session storage schema fractional", { ...CAPABILITIES, sessionStorage: { ...CAPABILITIES.sessionStorage, schemaVersion: 1.5 } }],
    ["session storage importer type", { ...CAPABILITIES, sessionStorage: { ...CAPABILITIES.sessionStorage, hermesImportAvailable: 0 } }],
    ["session storage extra", { ...CAPABILITIES, sessionStorage: { ...CAPABILITIES.sessionStorage, extra: true } }],
    ["extensions container", { ...CAPABILITIES, extensions: null }],
    ["extensions missing required key", {
      ...CAPABILITIES,
      extensions: { activeRunDiscovery: false, runQueue: false },
    }],
    ["extensions boolean type", {
      ...CAPABILITIES,
      extensions: { ...CAPABILITIES.extensions, toolsetManagement: "yes" },
    }],
    ["Code execution capability missing", {
      ...CAPABILITIES,
      extensions: Object.fromEntries(
        Object.entries(CAPABILITIES.extensions).filter(([key]) => key !== "codeExecution"),
      ),
    }],
    ["Code execution capability boolean type", {
      ...CAPABILITIES,
      extensions: { ...CAPABILITIES.extensions, codeExecution: "yes" },
    }],
    ["Web capability missing", {
      ...CAPABILITIES,
      extensions: Object.fromEntries(
        Object.entries(CAPABILITIES.extensions).filter(([key]) => key !== "webSearch"),
      ),
    }],
    ["Web capability boolean type", {
      ...CAPABILITIES,
      extensions: { ...CAPABILITIES.extensions, browserAutomation: "no" },
    }],
    ["MCP runtime capability missing", {
      ...CAPABILITIES,
      extensions: Object.fromEntries(
        Object.entries(CAPABILITIES.extensions).filter(([key]) => key !== "mcpStdio"),
      ),
    }],
    ["MCP runtime capability boolean type", {
      ...CAPABILITIES,
      extensions: { ...CAPABILITIES.extensions, mcpSse: "yes" },
    }],
    ["Product catalog capability missing", {
      ...CAPABILITIES,
      extensions: Object.fromEntries(
        Object.entries(CAPABILITIES.extensions).filter(([key]) => key !== "personas"),
      ),
    }],
    ["Product catalog capability boolean type", {
      ...CAPABILITIES,
      extensions: { ...CAPABILITIES.extensions, moments: "yes" },
    }],
    ["WeChat messaging capability missing", {
      ...CAPABILITIES,
      extensions: Object.fromEntries(
        Object.entries(CAPABILITIES.extensions).filter(([key]) => key !== "wechatMessaging"),
      ),
    }],
    ["Plugin capability boolean type", {
      ...CAPABILITIES,
      extensions: { ...CAPABILITIES.extensions, plugins: "yes" },
    }],
    ["negative file limit", { ...CAPABILITIES, files: { ...CAPABILITIES.files, maxBytes: -1 } }],
    ["fractional file limit", { ...CAPABILITIES, files: { ...CAPABILITIES.files, maxBytes: 1.5 } }],
    ["undersized file limit", { ...CAPABILITIES, files: { ...CAPABILITIES.files, maxBytes: MAX_FILE_BYTES - 1 } }],
    ["oversized file limit", { ...CAPABILITIES, files: { ...CAPABILITIES.files, maxBytes: 8 * 1024 * 1024 + 1 } }],
    ["MIME type container", { ...CAPABILITIES, files: { ...CAPABILITIES.files, allowedMimeTypes: "text/plain" } }],
    ["MIME type entry", { ...CAPABILITIES, files: { ...CAPABILITIES.files, allowedMimeTypes: [1] } }],
    ["incomplete MIME list", { ...CAPABILITIES, files: { ...CAPABILITIES.files, allowedMimeTypes: ["text/plain"] } }],
    ["unknown MIME type", { ...CAPABILITIES, files: { ...CAPABILITIES.files, allowedMimeTypes: [
      ...ALLOWED_FILE_MIME_TYPES.slice(1),
      "application/x-untrusted",
    ] } }],
    ["duplicate MIME type", { ...CAPABILITIES, files: { ...CAPABILITIES.files, allowedMimeTypes: [
      ...ALLOWED_FILE_MIME_TYPES.slice(1),
      "text/plain",
    ] } }],
    ["missing field", { ...CAPABILITIES, files: { maxBytes: 0 } }],
    ["non-object engine", { ...CAPABILITIES, engine: null }],
  ])("rejects invalid Capabilities %s", (_case, payload) => {
    expect(() => parseCapabilities(payload)).toThrowError(
      expect.objectContaining<Partial<ProfileApiError>>({ kind: "invalid_response" }),
    );
  });

  it.each([
    "https://user@models.example.com",
    "https://user:pass@models.example.com",
    "https://models.example.com?v=1",
    "https://models.example.com#models",
    "not a URL",
  ])("rejects an unsafe or malformed model Base URL: %s", (baseUrl) => {
    expect(() => parseProfileConfig({
      ...CONFIG,
      model: { ...CONFIG.model, baseUrl },
    })).toThrowError(expect.objectContaining<Partial<ProfileApiError>>({ kind: "invalid_response" }));
  });

  it.each([
    ["invalid summary ID", { ...PROFILE, id: "../escape" }, parseProfileSummary],
    ["invalid engine state", { ...PROFILE, engineState: "online" }, parseProfileSummary],
    ["empty display name", { ...PROFILE, displayName: "" }, parseProfileSummary],
    ["invalid display color", { ...PROFILE, color: "red" }, parseProfileSummary],
    ["invalid timestamp", { ...PROFILE, updatedAt: "yesterday" }, parseProfileSummary],
    ["invalid reasoning effort", {
      ...CONFIG,
      model: { ...CONFIG.model, reasoningEffort: "extreme" },
    }, parseProfileConfig],
    ["invalid code execution mode", {
      ...CONFIG,
      codeExecution: { ...CONFIG.codeExecution, mode: "sandbox" },
    }, parseProfileConfig],
    ["invalid code execution timeout", {
      ...CONFIG,
      codeExecution: { ...CONFIG.codeExecution, timeoutSeconds: 601 },
    }, parseProfileConfig],
    ["fractional code execution timeout", {
      ...CONFIG,
      codeExecution: { ...CONFIG.codeExecution, timeoutSeconds: 1.5 },
    }, parseProfileConfig],
    ["invalid code execution tool-call limit", {
      ...CONFIG,
      codeExecution: { ...CONFIG.codeExecution, maxToolCalls: 0 },
    }, parseProfileConfig],
    ["extra code execution field", {
      ...CONFIG,
      codeExecution: { ...CONFIG.codeExecution, extra: true },
    }, parseProfileConfig],
    ["invalid boolean map", { ...CONFIG, toolsets: { browser: "yes" } }, parseProfileConfig],
    ["null extension", { ...CONFIG, extensions: { nested: null } }, parseProfileConfig],
    ["non-finite extension", { ...CONFIG, extensions: { count: Number.NaN } }, parseProfileConfig],
    ["undefined extension", { ...CONFIG, extensions: { missing: undefined } }, parseProfileConfig],
    ["function extension", { ...CONFIG, extensions: { callback: () => undefined } }, parseProfileConfig],
    ["invalid secret name", { ...SECRET_STATUS, name: "lowercase" }, parseSecretStatus],
    ["invalid secret storage", { ...SECRET_STATUS, storage: "file" }, parseSecretStatus],
    ["invalid Problem status type", { ...PROBLEM, status: "400" }, parseProblemDetails],
    ["invalid Problem status range", { ...PROBLEM, status: 600 }, parseProblemDetails],
  ] satisfies Array<[string, unknown, (value: unknown) => unknown]>)(
    "rejects %s",
    (_case, payload, parser) => {
      expect(() => parser(payload)).toThrowError(
        expect.objectContaining<Partial<ProfileApiError>>({ kind: "invalid_response" }),
      );
    },
  );

  it.each([
    ["secret list", { ...PROVIDER, secretNames: "OPENAI_API_KEY" }],
    ["secret entry type", { ...PROVIDER, secretNames: [1] }],
    ["secret entry syntax", { ...PROVIDER, secretNames: ["openai_api_key"] }],
  ])("rejects an invalid Provider %s", (_case, payload) => {
    expect(() => parseProvider(payload)).toThrowError(
      expect.objectContaining<Partial<ProfileApiError>>({ kind: "invalid_response" }),
    );
  });

  it("exercises every Profile endpoint with explicit request options", async () => {
    const controller = new AbortController();
    const requests: Array<{ path: string; init: RequestInit; signal?: AbortSignal }> = [];
    const transport: DesktopTransport = {
      request: async (path, init = {}, options = {}) => {
        requests.push({ path, init, signal: options.signal });
        const method = init.method ?? "GET";
        if (path === "/api/v1/capabilities") return jsonResponse(CAPABILITIES);
        if (path === "/api/v1/providers") return jsonResponse([PROVIDER]);
        if (path === "/api/v1/profiles" && method === "GET") return jsonResponse([PROFILE]);
        if (path === "/api/v1/profiles" && method === "POST") {
          return jsonResponse({ ...METADATA, id: "work", isDefault: false }, 201, { ETag: '"meta_work"' });
        }
        if (path.endsWith("/active")) return jsonResponse(PROFILE);
        if (path.endsWith("/config")) return jsonResponse(CONFIG, 200, { ETag: '"rev_config_1"' });
        if (path.endsWith("/secrets/OPENAI_API_KEY") && method === "PUT") {
          return jsonResponse(SECRET_STATUS);
        }
        if (path.endsWith("/secrets") && method === "GET") return jsonResponse([SECRET_STATUS]);
        if (method === "DELETE") return new Response(null, { status: 204 });
        return jsonResponse(METADATA, 200, { ETag: '"meta_default"' });
      },
    };
    const client = createProfilesApi(transport);
    const options = { signal: controller.signal };

    await client.getCapabilities(options);
    await client.listProviders(options);
    await client.listProfiles(options);
    await client.createProfile({ id: "work", displayName: "Work" }, "create-work-2", options);
    await client.getProfile("default", options);
    await client.updateProfile("default", { color: "#112233" }, '"meta_default"', options);
    await client.deleteProfile("work", options);
    await client.activateProfile("default", options);
    await client.getProfileConfig("default", options);
    await client.updateProfileConfig("default", { model: { model: "gpt-5" } }, '"rev_config_1"', options);
    await client.listSecretStatuses("default", options);
    await client.putSecret("default", "OPENAI_API_KEY", "x".repeat(2_560), options);
    await client.deleteSecret("default", "OPENAI_API_KEY", options);

    expect(requests).toHaveLength(13);
    expect(requests.every((request) => request.signal === controller.signal)).toBe(true);
  });

  it("rejects invalid request inputs before transport", async () => {
    const transport: DesktopTransport = { request: vi.fn(async () => new Response(null, { status: 204 })) };
    const client = createProfilesApi(transport);

    await expect(client.getProfile("../escape")).rejects.toMatchObject({ kind: "invalid_request" });
    await expect(client.updateProfile("default", {}, "weak-etag")).rejects.toMatchObject({ kind: "invalid_request" });
    await expect(client.createProfile(
      { id: "default", displayName: "Default" },
      "create-default",
    )).rejects.toMatchObject({ kind: "invalid_request" });
    await expect(client.createProfile(
      { id: "work", displayName: "   " },
      "create-blank-name",
    )).rejects.toMatchObject({ kind: "invalid_request" });
    await expect(client.createProfile(
      { id: "work", displayName: "x", cloneFromProfileId: "../escape" },
      "create-bad-clone",
    )).rejects.toMatchObject({ kind: "invalid_request" });
    await expect(client.createProfile(
      { id: "work", displayName: "Work" },
      "short",
    )).rejects.toMatchObject({ kind: "invalid_request" });
    await expect(client.putSecret("default", "lowercase", "value")).rejects.toMatchObject({
      kind: "invalid_request",
    });
    await expect(client.putSecret("default", "OPENAI_API_KEY", "")).rejects.toMatchObject({
      kind: "invalid_request",
    });
    await expect(client.putSecret("default", "OPENAI_API_KEY", "\u{1f642}".repeat(641))).rejects.toMatchObject({
      kind: "invalid_request",
    });
    expect(transport.request).not.toHaveBeenCalled();
  });

  it("rejects invalid HTTP envelopes, JSON, Problem status, and ETags", async () => {
    const responses = [
      new Response("plain text", { status: 200, headers: { "Content-Type": "text/plain" } }),
      new Response("{", { status: 200, headers: { "Content-Type": "application/json" } }),
      jsonResponse({ ...PROBLEM, status: 400 }, 500, { "Content-Type": "application/problem+json" }),
    ];

    for (const response of responses) {
      const client = createProfilesApi({ request: async () => response });
      await expect(client.getCapabilities()).rejects.toMatchObject({ kind: "invalid_response" });
    }

    const missingEtag = createProfilesApi({ request: async () => jsonResponse(METADATA) });
    await expect(missingEtag.getProfile("default")).rejects.toMatchObject({ kind: "invalid_response" });
    const mismatchedConfigEtag = createProfilesApi({
      request: async () => jsonResponse(CONFIG, 200, { ETag: '"different"' }),
    });
    await expect(mismatchedConfigEtag.getProfileConfig("default")).rejects.toMatchObject({
      kind: "invalid_response",
    });
  });
});
