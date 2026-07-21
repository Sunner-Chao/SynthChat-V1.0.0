// @vitest-environment jsdom

import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { DesktopConnectionError } from "../../api/desktopConnection";
import { FileApiError, type FileRef, type FilesApi } from "../../api/files";
import type { McpApi, McpServer } from "../../api/mcp";
import type {
  Capabilities,
  ProfileSummary,
  ProfilesApi,
  SecretStatus,
} from "../../api/profiles";
import {
  SkillApiError,
  type Skill,
  type Operation,
  type SkillsApi,
  type VersionedSkill,
  type VersionedSkillPage,
} from "../../api/skills";
import {
  ToolsetApiError,
  type Toolset,
  type ToolsetsApi,
  type VersionedToolset,
  type VersionedToolsets,
} from "../../api/toolsets";
import type {
  VersionedWebConfig,
  WebApi,
  WebConfig,
  WebProvider,
} from "../../api/web";
import { ToolsWorkspace } from "./ToolsWorkspace";

const NOW = "2026-07-16T08:00:00Z";
const CAPABILITIES: Capabilities = {
  contractVersion: "v1",
  backendVersion: "0.3.0",
  engine: {
    kind: "hermes-rust",
    available: true,
    version: "0.3.0",
    pinnedCommit: null,
    features: {
      runStreaming: true,
      reasoningStreaming: true,
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
  sessionStorage: { available: true, schemaVersion: 6, hermesImportAvailable: true },
  sessionSearch: { mode: "fts5" },
  files: { maxBytes: 0, allowedMimeTypes: [] },
  extensions: {
    activeRunDiscovery: false,
    runQueue: false,
    toolsetManagement: true,
    toolExecution: true,
    codeExecution: true,
    workspaceManagement: true,
    skillDiscovery: true,
    skillEnablement: true,
    webSearch: false,
    webExtract: false,
    browserAutomation: false,
    browserCdp: false,
    browserDownloads: false,
    mcpStdio: false,
    mcpStreamableHttp: false,
    mcpSse: false,
    wechatAccounts: false,
    wechatMessaging: false,
    plugins: false,
    personas: false,
    moments: false,
    worldbooks: false,
  },
};
const PROFILES: ProfileSummary[] = [
  {
    id: "default",
    displayName: "Default",
    isDefault: true,
    isActive: true,
    color: null,
    avatarFileId: null,
    engineState: "running",
    configRevision: "config-1",
    createdAt: null,
    updatedAt: NOW,
  },
  {
    id: "work",
    displayName: "Work",
    isDefault: false,
    isActive: false,
    color: null,
    avatarFileId: null,
    engineState: "stopped",
    configRevision: "work-config-1",
    createdAt: NOW,
    updatedAt: NOW,
  },
];
const WEB: Toolset = {
  id: "web",
  displayName: "Web",
  description: "Search and retrieve web content.",
  enabled: true,
  configured: true,
  tools: ["web_search", "web_fetch"],
};
const TERMINAL: Toolset = {
  id: "terminal",
  displayName: "Terminal",
  description: "Run local commands.",
  enabled: false,
  configured: false,
  tools: ["terminal"],
};
const RESEARCH: Skill = {
  id: "skill_11111111111111111111111111111111",
  name: "Research",
  description: "Find and synthesize reliable sources.",
  source: "bundled",
  version: "1.2.0",
  enabled: true,
  configurable: true,
  uninstallable: true,
};
const LOCAL_SKILL: Skill = {
  id: "skill_22222222222222222222222222222222",
  name: "Local notes",
  description: "Work with approved local notes.",
  source: "local",
  version: null,
  enabled: false,
  configurable: false,
  uninstallable: false,
};

const QUEUED_OPERATION: Operation = {
  id: "op_33333333333333333333333333333333",
  kind: "skillInstall",
  status: "queued",
  createdAt: NOW,
  updatedAt: NOW,
};

const COMPLETED_OPERATION: Operation = {
  ...QUEUED_OPERATION,
  status: "completed",
  updatedAt: "2026-07-16T08:00:01Z",
};

const SKILL_FILE: FileRef = {
  id: "file_44444444444444444444444444444444",
  name: "research.zip",
  mimeType: "application/zip",
  sizeBytes: 32,
  createdAt: NOW,
};

const WEB_PROVIDER: WebProvider = {
  id: "tavily",
  displayName: "Tavily",
  supportsSearch: true,
  supportsExtract: true,
  secretNames: ["TAVILY_API_KEY"],
  defaultBaseUrl: "https://api.tavily.com",
  customEndpointSupported: false,
};
const WEB_CONFIG: WebConfig = {
  revision: "config-1",
  sharedProvider: null,
  searchProvider: null,
  extractProvider: null,
  extractCharLimit: 15_000,
  effectiveSearch: { providerId: "tavily", status: "ready", missingSecretNames: [] },
  effectiveExtract: { providerId: "tavily", status: "ready", missingSecretNames: [] },
};
const TAVILY_SECRET: SecretStatus = {
  name: "TAVILY_API_KEY",
  configured: true,
  storage: "osKeychain",
  updatedAt: NOW,
};
const MCP_SERVER: McpServer = {
  id: `mcp_${"a".repeat(32)}`,
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

type ProfileClient = Pick<
  ProfilesApi,
  | "getCapabilities"
  | "listProfiles"
  | "listSecretStatuses"
  | "putSecret"
  | "deleteSecret"
>;

function profileClient(overrides: Partial<ProfileClient> = {}): ProfileClient {
  return {
    getCapabilities: vi.fn(async () => CAPABILITIES),
    listProfiles: vi.fn(async () => PROFILES),
    listSecretStatuses: vi.fn(async () => [TAVILY_SECRET]),
    putSecret: vi.fn(async () => TAVILY_SECRET),
    deleteSecret: vi.fn(async () => undefined),
    ...overrides,
  };
}

function versioned(value: Toolset[], etag = '"config-1"'): VersionedToolsets {
  return { value, etag };
}

function toolsetClient(overrides: Partial<ToolsetsApi> = {}): ToolsetsApi {
  return {
    listToolsets: vi.fn(async () => versioned([WEB, TERMINAL])),
    updateToolset: vi.fn(async (_profileId, _toolsetId, patch) => ({
      value: { ...WEB, enabled: patch.enabled },
      etag: '"config-2"',
    })),
    ...overrides,
  };
}

function mcpClient(overrides: Partial<McpApi> = {}): McpApi {
  return {
    listServers: vi.fn(async () => ({ value: [MCP_SERVER], etag: '"config-1"' })),
    createServer: vi.fn(async () => ({ value: MCP_SERVER, etag: '"config-2"' })),
    updateServer: vi.fn(async (_profileId, _serverId, patch) => ({
      value: { ...MCP_SERVER, ...patch },
      etag: '"config-2"',
    })),
    deleteServer: vi.fn(async () => ({ etag: '"config-2"' })),
    ...overrides,
  };
}

function versionedSkills(
  items: Skill[],
  etag = '"skills-1"',
  nextCursor: string | null = null,
): VersionedSkillPage {
  return { value: { items, nextCursor }, etag };
}

function skillClient(overrides: Partial<SkillsApi> = {}): SkillsApi {
  return {
    listSkills: vi.fn(async () => versionedSkills([RESEARCH, LOCAL_SKILL])),
    updateSkill: vi.fn(async (_profileId, skillId, enabled) => ({
      value: {
        ...(skillId === RESEARCH.id ? RESEARCH : LOCAL_SKILL),
        enabled,
      },
      etag: '"skills-2"',
    })),
    installSkill: vi.fn(async () => QUEUED_OPERATION),
    getOperation: vi.fn(async () => COMPLETED_OPERATION),
    uninstallSkill: vi.fn(async (): Promise<Operation> => ({
      ...QUEUED_OPERATION,
      kind: "skillUninstall",
    })),
    ...overrides,
  };
}

function filesClient(overrides: Partial<FilesApi> = {}): FilesApi {
  return {
    uploadFile: vi.fn(async () => SKILL_FILE),
    deleteFile: vi.fn(async () => undefined),
    ...overrides,
  };
}

function versionedWeb(
  value: WebConfig = WEB_CONFIG,
  etag = `"${value.revision}"`,
): VersionedWebConfig {
  return { value, etag };
}

function webClient(overrides: Partial<WebApi> = {}): WebApi {
  return {
    listProviders: vi.fn(async () => [WEB_PROVIDER]),
    getWebConfig: vi.fn(async () => versionedWeb()),
    updateWebConfig: vi.fn(async (_profileId, patch) => versionedWeb({
      ...WEB_CONFIG,
      ...patch,
      revision: "config-2",
    })),
    ...overrides,
  };
}

beforeEach(() => {
  sessionStorage.clear();
});

afterEach(() => {
  cleanup();
  sessionStorage.clear();
  vi.restoreAllMocks();
});

describe("ToolsWorkspace", () => {
  it("fails closed outside Desktop before loading Profiles or Toolsets", async () => {
    const profiles = profileClient({
      getCapabilities: vi.fn(async () => {
        throw new DesktopConnectionError("desktop_unavailable", "desktop required");
      }),
    });
    const toolsets = toolsetClient();
    const skills = skillClient();

    render(
      <ToolsWorkspace client={toolsets} profileClient={profiles} skillsClient={skills} />,
    );

    expect(await screen.findByText("请在 SynthChat Desktop 中打开")).toBeTruthy();
    expect(profiles.listProfiles).not.toHaveBeenCalled();
    expect(toolsets.listToolsets).not.toHaveBeenCalled();
    expect(skills.listSkills).not.toHaveBeenCalled();
  });

  it("keeps the workspace unavailable unless a catalog capability is explicitly true", async () => {
    const profiles = profileClient({
      getCapabilities: vi.fn(async () => ({
        ...CAPABILITIES,
        extensions: {
          ...CAPABILITIES.extensions,
          toolsetManagement: false,
          codeExecution: false,
          skillDiscovery: false,
          skillEnablement: false,
        },
      })),
    });

    render(
      <ToolsWorkspace
        client={toolsetClient()}
        profileClient={profiles}
        skillsClient={skillClient()}
      />,
    );

    expect(await screen.findByText("工具与 Skills 暂不可用")).toBeTruthy();
    expect(profiles.listProfiles).not.toHaveBeenCalled();
  });

  it("renders the dynamic catalog and saves one exact enablement update", async () => {
    const user = userEvent.setup();
    const updateToolset = vi.fn(async (
      _profileId: string,
      _toolsetId: string,
      patch: { enabled: boolean },
    ): Promise<VersionedToolset> => ({
      value: { ...WEB, enabled: patch.enabled },
      etag: '"config-2"',
    }));
    const client = toolsetClient({ updateToolset });
    render(
      <ToolsWorkspace
        client={client}
        profileClient={profileClient()}
        skillsClient={skillClient()}
      />,
    );

    const toggle = await screen.findByRole("switch", { name: "停用 Web (web)" });
    expect((toggle as HTMLInputElement).checked).toBe(true);
    expect(screen.getByText("web_search")).toBeTruthy();
    expect(await screen.findByText("Research")).toBeTruthy();
    expect(screen.getByRole("heading", { name: "MCP Servers" })).toBeTruthy();
    expect(screen.getByText("当前后端未启用 MCP 管理能力。")).toBeTruthy();
    expect(screen.getByText("Browser 自动化不可用")).toBeTruthy();
    expect(screen.getByText("代码执行").closest("div")?.textContent).toContain("可用");

    await user.click(toggle);
    await waitFor(() => expect(updateToolset).toHaveBeenCalledWith(
      "default",
      "web",
      { enabled: false },
      '"config-1"',
    ));
    expect(await screen.findByRole("switch", { name: "启用 Web (web)" })).toBeTruthy();
  });

  it("enters the workspace for MCP-only capability and follows the selected Profile", async () => {
    const user = userEvent.setup();
    const listServers = vi.fn(async (profileId: string) => ({
      value: [{ ...MCP_SERVER, name: `${profileId}_tools` }],
      etag: `"${profileId}-config-1"`,
    }));
    const mcp = mcpClient({ listServers });
    const toolsets = toolsetClient();
    const skills = skillClient();
    const profiles = profileClient({
      getCapabilities: vi.fn(async () => ({
        ...CAPABILITIES,
        engine: {
          ...CAPABILITIES.engine,
          features: { ...CAPABILITIES.engine.features, mcpManagement: true },
        },
        extensions: {
          ...CAPABILITIES.extensions,
          toolsetManagement: false,
          codeExecution: false,
          skillDiscovery: false,
          skillEnablement: false,
          mcpStdio: true,
          mcpStreamableHttp: false,
          mcpSse: false,
        },
      })),
    });

    render(
      <ToolsWorkspace
        client={toolsets}
        mcpClient={mcp}
        profileClient={profiles}
        skillsClient={skills}
      />,
    );

    expect(await screen.findByText("default_tools")).toBeTruthy();
    expect(screen.getByText("运行时可用")).toBeTruthy();
    expect(listServers).toHaveBeenCalledWith("default", expect.anything());
    expect(toolsets.listToolsets).not.toHaveBeenCalled();
    expect(skills.listSkills).not.toHaveBeenCalled();

    await user.selectOptions(screen.getByRole("combobox", { name: "工具 Profile" }), "work");
    expect(await screen.findByText("work_tools")).toBeTruthy();
    expect(listServers).toHaveBeenCalledWith("work", expect.anything());
  });

  it("keeps controls stable and disabled while a shared-revision update is pending", async () => {
    let resolveUpdate!: (value: VersionedToolset) => void;
    const updateToolset = vi.fn(() => new Promise<VersionedToolset>((resolve) => {
      resolveUpdate = resolve;
    }));
    const user = userEvent.setup();
    render(
      <ToolsWorkspace
        client={toolsetClient({ updateToolset })}
        profileClient={profileClient()}
        skillsClient={skillClient()}
      />,
    );

    const toggle = await screen.findByRole("switch", { name: "停用 Web (web)" });
    await user.click(toggle);
    await waitFor(() => expect(updateToolset).toHaveBeenCalledTimes(1));
    expect((toggle as HTMLInputElement).disabled).toBe(true);
    expect((screen.getByRole("combobox", { name: "工具 Profile" }) as HTMLSelectElement).disabled).toBe(true);
    expect(screen.getByText("保存中")).toBeTruthy();

    resolveUpdate({ value: { ...WEB, enabled: false }, etag: '"config-2"' });
    expect(await screen.findByRole("switch", { name: "启用 Web (web)" })).toBeTruthy();
  });

  it("reloads the whole table after a 409 and keeps a conflict notice visible", async () => {
    const listToolsets = vi.fn()
      .mockResolvedValueOnce(versioned([WEB], '"config-stale"'))
      .mockResolvedValueOnce(versioned([{ ...WEB, enabled: false }], '"config-current"'));
    const updateToolset = vi.fn(async () => {
      throw new ToolsetApiError("http", "Configuration changed", {
        status: 409,
        code: "revision_conflict",
        requestId: "req-conflict",
        etag: '"config-current"',
      });
    });
    const user = userEvent.setup();
    render(
      <ToolsWorkspace
        client={toolsetClient({ listToolsets, updateToolset })}
        profileClient={profileClient()}
        skillsClient={skillClient()}
      />,
    );

    await user.click(await screen.findByRole("switch", { name: "停用 Web (web)" }));
    await waitFor(() => expect(listToolsets).toHaveBeenCalledTimes(2));
    expect(await screen.findByRole("switch", { name: "启用 Web (web)" })).toBeTruthy();
    expect(screen.getByRole("alert").textContent).toContain("已重新加载最新状态");
    expect(updateToolset).toHaveBeenCalledWith(
      "default",
      "web",
      { enabled: false },
      '"config-stale"',
    );
  });

  it("aborts a stale Profile request and ignores its late result", async () => {
    let resolveDefault!: (value: VersionedToolsets) => void;
    let defaultSignal: AbortSignal | undefined;
    const listToolsets = vi.fn((profileId: string, options?: { signal?: AbortSignal }) => {
      if (profileId === "default") {
        defaultSignal = options?.signal;
        return new Promise<VersionedToolsets>((resolve) => {
          resolveDefault = resolve;
        });
      }
      return Promise.resolve(versioned([{ ...TERMINAL, displayName: "Work terminal" }], '"work-config-1"'));
    });
    const user = userEvent.setup();
    render(
      <ToolsWorkspace
        client={toolsetClient({ listToolsets })}
        profileClient={profileClient()}
        skillsClient={skillClient()}
      />,
    );

    const select = await screen.findByRole("combobox", { name: "工具 Profile" });
    await waitFor(() => expect(listToolsets).toHaveBeenCalledWith("default", expect.anything()));
    await user.selectOptions(select, "work");
    expect(await screen.findByText("Work terminal")).toBeTruthy();
    expect(defaultSignal?.aborted).toBe(true);

    resolveDefault(versioned([{ ...WEB, displayName: "Stale default" }]));
    await waitFor(() => expect(screen.queryByText("Stale default")).toBeNull());
  });

  it("offers a retry after a Toolset list error", async () => {
    const listToolsets = vi.fn()
      .mockRejectedValueOnce(new ToolsetApiError("http", "Tool catalog unavailable", {
        status: 503,
        code: "toolset_unavailable",
      }))
      .mockResolvedValueOnce(versioned([WEB]));
    const user = userEvent.setup();
    render(
      <ToolsWorkspace
        client={toolsetClient({ listToolsets })}
        profileClient={profileClient()}
        skillsClient={skillClient()}
      />,
    );

    expect(await screen.findByText("Tool catalog unavailable")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "重新加载" }));
    expect(await screen.findByRole("switch", { name: "停用 Web (web)" })).toBeTruthy();
    expect(listToolsets).toHaveBeenCalledTimes(2);
  });

  it("loads Skills in discovery-only mode and keeps enablement fail-closed", async () => {
    const profiles = profileClient({
      getCapabilities: vi.fn(async () => ({
        ...CAPABILITIES,
        extensions: {
          ...CAPABILITIES.extensions,
          toolsetManagement: false,
          skillDiscovery: true,
          skillEnablement: false,
        },
      })),
    });
    const toolsets = toolsetClient();
    const skills = skillClient();

    render(
      <ToolsWorkspace client={toolsets} profileClient={profiles} skillsClient={skills} />,
    );

    expect(await screen.findByText("Research")).toBeTruthy();
    expect(screen.getByText("当前后端未启用 Toolset 管理能力。")).toBeTruthy();
    expect(toolsets.listToolsets).not.toHaveBeenCalled();
    expect(skills.listSkills).toHaveBeenCalledTimes(1);
    expect((screen.getByRole("switch", {
      name: `停用 Skill ${RESEARCH.name} (${RESEARCH.id})`,
    }) as HTMLInputElement).disabled).toBe(true);
    expect(screen.getAllByText("只读").length).toBeGreaterThan(0);
  });

  it("searches and appends paginated Skills without mixing revisions", async () => {
    const listSkills = vi.fn(async (
      _profileId: string,
      request: { query?: string; cursor?: string; limit?: number } = {},
    ) => {
      if (request.query === "notes") {
        return versionedSkills([LOCAL_SKILL], '"skills-search"');
      }
      if (request.cursor === "skills-next") {
        return versionedSkills([LOCAL_SKILL], '"skills-1"');
      }
      return versionedSkills([RESEARCH], '"skills-1"', "skills-next");
    });
    const user = userEvent.setup();
    render(
      <ToolsWorkspace
        client={toolsetClient()}
        profileClient={profileClient()}
        skillsClient={skillClient({ listSkills })}
      />,
    );

    expect(await screen.findByText("Research")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "加载更多" }));
    expect(await screen.findByText("Local notes")).toBeTruthy();
    expect(listSkills).toHaveBeenCalledWith(
      "default",
      { cursor: "skills-next", limit: 30 },
      { signal: expect.any(AbortSignal) },
    );

    const search = screen.getByRole("searchbox", { name: "搜索 Skills" });
    await user.type(search, " notes ");
    await user.click(screen.getByRole("button", { name: "搜索" }));
    await waitFor(() => expect(listSkills).toHaveBeenCalledWith(
      "default",
      { query: "notes", limit: 30 },
      { signal: expect.any(AbortSignal) },
    ));
    await waitFor(() => expect(screen.queryByText("Research")).toBeNull());
    expect(screen.getByText("Local notes")).toBeTruthy();
    expect(screen.getByText("已加载全部")).toBeTruthy();
  });

  it("locks Skill enablement while a page with the shared ETag is loading", async () => {
    let resolvePage!: (value: VersionedSkillPage) => void;
    const listSkills = vi.fn()
      .mockResolvedValueOnce(versionedSkills([RESEARCH], '"skills-1"', "skills-next"))
      .mockImplementationOnce(() => new Promise<VersionedSkillPage>((resolve) => {
        resolvePage = resolve;
      }));
    const user = userEvent.setup();
    render(
      <ToolsWorkspace
        client={toolsetClient()}
        profileClient={profileClient()}
        skillsClient={skillClient({ listSkills })}
      />,
    );

    const toggle = await screen.findByRole("switch", {
      name: `停用 Skill ${RESEARCH.name} (${RESEARCH.id})`,
    });
    await user.click(screen.getByRole("button", { name: "加载更多" }));
    await waitFor(() => expect(listSkills).toHaveBeenCalledTimes(2));
    expect((toggle as HTMLInputElement).disabled).toBe(true);

    resolvePage(versionedSkills([LOCAL_SKILL], '"skills-1"'));
    expect(await screen.findByText("Local notes")).toBeTruthy();
    await waitFor(() => expect((toggle as HTMLInputElement).disabled).toBe(false));
  });

  it("uses the latest shared Skill ETag for serialized enablement updates", async () => {
    const updateSkill = vi.fn(async (
      _profileId: string,
      skillId: string,
      enabled: boolean,
    ): Promise<VersionedSkill> => ({
      value: {
        ...(skillId === RESEARCH.id ? RESEARCH : LOCAL_SKILL),
        enabled,
      },
      etag: skillId === RESEARCH.id ? '"skills-2"' : '"skills-3"',
    }));
    const user = userEvent.setup();
    render(
      <ToolsWorkspace
        client={toolsetClient()}
        profileClient={profileClient()}
        skillsClient={skillClient({ updateSkill })}
      />,
    );

    await user.click(await screen.findByRole("switch", {
      name: `停用 Skill ${RESEARCH.name} (${RESEARCH.id})`,
    }));
    await waitFor(() => expect(updateSkill).toHaveBeenNthCalledWith(
      1,
      "default",
      RESEARCH.id,
      false,
      '"skills-1"',
    ));
    await user.click(await screen.findByRole("switch", {
      name: `启用 Skill ${LOCAL_SKILL.name} (${LOCAL_SKILL.id})`,
    }));
    await waitFor(() => expect(updateSkill).toHaveBeenNthCalledWith(
      2,
      "default",
      LOCAL_SKILL.id,
      true,
      '"skills-2"',
    ));
  });

  it("refreshes the filtered Skill table after a 409 conflict", async () => {
    const listSkills = vi.fn()
      .mockResolvedValueOnce(versionedSkills([RESEARCH], '"skills-stale"'))
      .mockResolvedValueOnce(versionedSkills(
        [{ ...RESEARCH, enabled: false }],
        '"skills-current"',
      ));
    const updateSkill = vi.fn(async () => {
      throw new SkillApiError("http", "Skill catalog changed", {
        status: 409,
        code: "revision_conflict",
        requestId: "req-skill-conflict",
        etag: '"skills-current"',
      });
    });
    const user = userEvent.setup();
    render(
      <ToolsWorkspace
        client={toolsetClient()}
        profileClient={profileClient()}
        skillsClient={skillClient({ listSkills, updateSkill })}
      />,
    );

    await user.click(await screen.findByRole("switch", {
      name: `停用 Skill ${RESEARCH.name} (${RESEARCH.id})`,
    }));
    await waitFor(() => expect(listSkills).toHaveBeenCalledTimes(2));
    expect(await screen.findByRole("switch", {
      name: `启用 Skill ${RESEARCH.name} (${RESEARCH.id})`,
    })).toBeTruthy();
    expect(screen.getByRole("alert").textContent).toContain("已重新加载最新目录");
    expect(updateSkill).toHaveBeenCalledWith(
      "default",
      RESEARCH.id,
      false,
      '"skills-stale"',
    );
  });

  it("renders loading, retryable error, and empty Skill states", async () => {
    let rejectLoad!: (error: unknown) => void;
    const listSkills = vi.fn()
      .mockImplementationOnce(() => new Promise<VersionedSkillPage>((_resolve, reject) => {
        rejectLoad = reject;
      }))
      .mockResolvedValueOnce(versionedSkills([]));
    const user = userEvent.setup();
    render(
      <ToolsWorkspace
        client={toolsetClient()}
        profileClient={profileClient()}
        skillsClient={skillClient({ listSkills })}
      />,
    );

    expect(await screen.findByText("正在加载 Skills")).toBeTruthy();
    rejectLoad(new SkillApiError("http", "Skills unavailable", {
      status: 503,
      code: "skills_unavailable",
      requestId: "req-skills",
    }));
    expect(await screen.findByText(/Skills unavailable/)).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "重新加载 Skills" }));
    expect(await screen.findByText("当前 Profile 没有可用的 Skills。")).toBeTruthy();
    expect(listSkills).toHaveBeenCalledTimes(2);
  });

  it("loads Web management only from explicit capabilities and keeps Browser independent", async () => {
    const profiles = profileClient({
      getCapabilities: vi.fn(async () => ({
        ...CAPABILITIES,
        extensions: {
          ...CAPABILITIES.extensions,
          webSearch: true,
          webExtract: true,
          browserAutomation: false,
        },
      })),
    });
    const web = webClient();
    render(
      <ToolsWorkspace
        client={toolsetClient()}
        profileClient={profiles}
        skillsClient={skillClient()}
        webClient={web}
      />,
    );

    expect(await screen.findByRole("combobox", { name: "共享 Web Provider" })).toBeTruthy();
    expect(web.listProviders).toHaveBeenCalledWith({ signal: expect.any(AbortSignal) });
    expect(web.getWebConfig).toHaveBeenCalledWith(
      "default",
      { signal: expect.any(AbortSignal) },
    );
    expect(profiles.listSecretStatuses).toHaveBeenCalledWith(
      "default",
      { signal: expect.any(AbortSignal) },
    );
    expect(screen.getByText("Browser 自动化不可用")).toBeTruthy();
  });

  it("locks the shared Profile selector while a Web mutation is pending", async () => {
    let resolveUpdate!: (value: VersionedWebConfig) => void;
    const updateWebConfig = vi.fn(() => new Promise<VersionedWebConfig>((resolve) => {
      resolveUpdate = resolve;
    }));
    const profiles = profileClient({
      getCapabilities: vi.fn(async () => ({
        ...CAPABILITIES,
        extensions: {
          ...CAPABILITIES.extensions,
          webSearch: true,
          webExtract: true,
        },
      })),
    });
    const user = userEvent.setup();
    render(
      <ToolsWorkspace
        client={toolsetClient()}
        profileClient={profiles}
        skillsClient={skillClient()}
        webClient={webClient({ updateWebConfig })}
      />,
    );

    await user.selectOptions(
      await screen.findByRole("combobox", { name: "共享 Web Provider" }),
      "tavily",
    );
    await waitFor(() => expect(updateWebConfig).toHaveBeenCalledTimes(1));
    const profileSelect = screen.getByRole("combobox", { name: "工具 Profile" });
    expect((profileSelect as HTMLSelectElement).disabled).toBe(true);

    resolveUpdate(versionedWeb({
      ...WEB_CONFIG,
      revision: "config-2",
      sharedProvider: "tavily",
    }));
    await waitFor(() => expect((profileSelect as HTMLSelectElement).disabled).toBe(false));
  });

  it("installs a Registry Skill, polls to completion, and refreshes the first page", async () => {
    const profiles = profileClient({
      getCapabilities: vi.fn(async () => ({
        ...CAPABILITIES,
        engine: {
          ...CAPABILITIES.engine,
          features: { ...CAPABILITIES.engine.features, skillManagement: true },
        },
      })),
    });
    const listSkills = vi.fn(async () => versionedSkills([RESEARCH, LOCAL_SKILL]));
    const installSkill = vi.fn(async () => QUEUED_OPERATION);
    const getOperation = vi.fn(async () => COMPLETED_OPERATION);
    const skills = skillClient({ listSkills, installSkill, getOperation });
    const user = userEvent.setup();
    render(
      <ToolsWorkspace
        client={toolsetClient()}
        filesClient={filesClient()}
        profileClient={profiles}
        skillsClient={skills}
      />,
    );

    await user.type(
      await screen.findByRole("textbox", { name: "Registry Skill ID" }),
      "paper-search",
    );
    const install = screen.getByRole("button", { name: "安装" }) as HTMLButtonElement;
    await waitFor(() => expect(install.disabled).toBe(false));
    await user.click(install);

    await waitFor(() => expect(getOperation).toHaveBeenCalledWith(
      QUEUED_OPERATION.id,
      { signal: expect.any(AbortSignal) },
    ));
    expect(installSkill).toHaveBeenCalledWith(
      "default",
      { registryId: "paper-search" },
      expect.stringMatching(/^skill-install-/u),
      { signal: expect.any(AbortSignal) },
    );
    await waitFor(() => expect(listSkills).toHaveBeenCalledTimes(2));
    expect(listSkills).toHaveBeenLastCalledWith(
      "default",
      { limit: 30 },
      { signal: expect.any(AbortSignal) },
    );
    expect((screen.getByRole("textbox", { name: "Registry Skill ID" }) as HTMLInputElement).value)
      .toBe("");
  });

  it("renders only redacted public metadata for a failed installation", async () => {
    const profiles = profileClient({
      getCapabilities: vi.fn(async () => ({
        ...CAPABILITIES,
        engine: {
          ...CAPABILITIES.engine,
          features: { ...CAPABILITIES.engine.features, skillManagement: true },
        },
      })),
    });
    const installSkill = vi.fn(async (): Promise<Operation> => ({
      ...QUEUED_OPERATION,
      status: "failed",
      updatedAt: "2026-07-16T08:00:01Z",
      error: {
        type: "about:blank",
        title: "Provider included sk-live-secret in its title",
        status: 422,
        detail: "private /home/alice/.hermes path and sk-live-secret",
        code: "skill_install_failed",
        requestId: "req-install",
        retryable: false,
      },
    }));
    const skills = skillClient({ installSkill });
    const user = userEvent.setup();
    render(
      <ToolsWorkspace
        client={toolsetClient()}
        filesClient={filesClient()}
        profileClient={profiles}
        skillsClient={skills}
      />,
    );

    await user.type(
      await screen.findByRole("textbox", { name: "Registry Skill ID" }),
      "paper-search",
    );
    await user.click(screen.getByRole("button", { name: "安装" }));

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("skill_install_failed");
    expect(alert.textContent).toContain("req-install");
    expect(alert.textContent).not.toContain("sk-live-secret");
    expect(alert.textContent).not.toContain("/home/alice");
    expect(skills.getOperation).not.toHaveBeenCalled();
  });

  it("aborts an in-flight Skill operation poll when the workspace unmounts", async () => {
    const profiles = profileClient({
      getCapabilities: vi.fn(async () => ({
        ...CAPABILITIES,
        engine: {
          ...CAPABILITIES.engine,
          features: { ...CAPABILITIES.engine.features, skillManagement: true },
        },
      })),
    });
    let pollSignal: AbortSignal | undefined;
    const getOperation = vi.fn((
      _operationId: string,
      options?: { signal?: AbortSignal },
    ) => {
      pollSignal = options?.signal;
      return new Promise<Operation>((_resolve, reject) => {
        options?.signal?.addEventListener(
          "abort",
          () => reject(new DOMException("aborted", "AbortError")),
          { once: true },
        );
      });
    });
    const user = userEvent.setup();
    const view = render(
      <ToolsWorkspace
        client={toolsetClient()}
        filesClient={filesClient()}
        profileClient={profiles}
        skillsClient={skillClient({ getOperation })}
      />,
    );

    await user.type(
      await screen.findByRole("textbox", { name: "Registry Skill ID" }),
      "paper-search",
    );
    await user.click(screen.getByRole("button", { name: "安装" }));
    await waitFor(() => expect(getOperation).toHaveBeenCalledTimes(1));
    expect(pollSignal?.aborted).toBe(false);
    view.unmount();
    expect(pollSignal?.aborted).toBe(true);
  });

  it("does not expose or call management endpoints when the capability is false", async () => {
    const installSkill = vi.fn(async () => QUEUED_OPERATION);
    const getOperation = vi.fn(async () => COMPLETED_OPERATION);
    const uninstallSkill = vi.fn(async () => QUEUED_OPERATION);
    const files = filesClient();
    render(
      <ToolsWorkspace
        client={toolsetClient()}
        filesClient={files}
        profileClient={profileClient()}
        skillsClient={skillClient({ installSkill, getOperation, uninstallSkill })}
      />,
    );

    expect(await screen.findByText("Research")).toBeTruthy();
    expect(screen.queryByRole("form", { name: "Skill 安装" })).toBeNull();
    expect(screen.queryByRole("button", { name: /卸载 Skill/u })).toBeNull();
    expect(installSkill).not.toHaveBeenCalled();
    expect(getOperation).not.toHaveBeenCalled();
    expect(uninstallSkill).not.toHaveBeenCalled();
    expect(files.uploadFile).not.toHaveBeenCalled();
  });

  it("uploads a file before installing it by opaque file ID", async () => {
    const profiles = profileClient({
      getCapabilities: vi.fn(async () => ({
        ...CAPABILITIES,
        engine: {
          ...CAPABILITIES.engine,
          features: { ...CAPABILITIES.engine.features, skillManagement: true },
        },
      })),
    });
    const uploadFile = vi.fn(async () => SKILL_FILE);
    const deleteFile = vi.fn(async () => undefined);
    const installSkill = vi.fn(async () => QUEUED_OPERATION);
    const user = userEvent.setup();
    render(
      <ToolsWorkspace
        client={toolsetClient()}
        filesClient={filesClient({ deleteFile, uploadFile })}
        profileClient={profiles}
        skillsClient={skillClient({ installSkill })}
      />,
    );

    await user.click(await screen.findByRole("radio", { name: "文件" }));
    const file = new File(["archive"], "research.zip", { type: "application/zip" });
    const fileInput = screen.getByLabelText("Skill 文件") as HTMLInputElement;
    await user.upload(fileInput, file);
    expect(fileInput.files).toHaveLength(1);
    expect(fileInput.checkValidity()).toBe(true);
    expect(fileInput.form?.checkValidity()).toBe(true);
    const install = screen.getByRole("button", { name: "安装" }) as HTMLButtonElement;
    expect(install.disabled).toBe(false);
    await user.click(install);

    await waitFor(() => expect(uploadFile).toHaveBeenCalledWith(
      file,
      expect.stringMatching(/^skill-file-/u),
      { signal: expect.any(AbortSignal) },
    ));
    await waitFor(() => expect(installSkill).toHaveBeenCalledWith(
      "default",
      { fileId: SKILL_FILE.id },
      expect.stringMatching(/^skill-install-/u),
      { signal: expect.any(AbortSignal) },
    ));
    await waitFor(() => expect(deleteFile).toHaveBeenCalledWith(SKILL_FILE.id));
    expect(sessionStorage.length).toBe(0);
  });

  it("cleans up a file after a failed install without replacing either public error", async () => {
    const profiles = profileClient({
      getCapabilities: vi.fn(async () => ({
        ...CAPABILITIES,
        engine: {
          ...CAPABILITIES.engine,
          features: { ...CAPABILITIES.engine.features, skillManagement: true },
        },
      })),
    });
    const installSkill = vi.fn(async (): Promise<Operation> => ({
      ...QUEUED_OPERATION,
      status: "failed",
      updatedAt: "2026-07-16T08:00:01Z",
      error: {
        type: "about:blank",
        title: "private sk-install-secret",
        status: 422,
        detail: "private /home/alice/source",
        code: "skill_install_failed",
        requestId: "req-install-file",
        retryable: false,
      },
    }));
    const deleteFile = vi.fn(async () => {
      throw new FileApiError("http", "private sk-cleanup-secret", {
        status: 503,
        code: "file_store_unavailable",
        requestId: "req-file-cleanup",
        retryable: true,
      });
    });
    const user = userEvent.setup();
    render(
      <ToolsWorkspace
        client={toolsetClient()}
        filesClient={filesClient({ deleteFile })}
        profileClient={profiles}
        skillsClient={skillClient({ installSkill })}
      />,
    );

    await user.click(await screen.findByRole("radio", { name: "文件" }));
    await user.upload(
      screen.getByLabelText("Skill 文件"),
      new File(["archive"], "research.zip", { type: "application/zip" }),
    );
    await user.click(screen.getByRole("button", { name: "安装" }));

    await waitFor(() => expect(deleteFile).toHaveBeenCalledWith(SKILL_FILE.id));
    const publicAlerts = screen.getAllByRole("alert").map((alert) => alert.textContent).join(" ");
    expect(publicAlerts).toContain("skill_install_failed");
    expect(publicAlerts).toContain("req-install-file");
    expect(publicAlerts).toContain("file_store_unavailable");
    expect(publicAlerts).toContain("req-file-cleanup");
    expect(publicAlerts).not.toContain("sk-install-secret");
    expect(publicAlerts).not.toContain("sk-cleanup-secret");
    expect(publicAlerts).not.toContain("/home/alice");
    expect(sessionStorage.length).toBe(0);
  });

  it("cleans up an uploaded file even when polling is aborted on unmount", async () => {
    const profiles = profileClient({
      getCapabilities: vi.fn(async () => ({
        ...CAPABILITIES,
        engine: {
          ...CAPABILITIES.engine,
          features: { ...CAPABILITIES.engine.features, skillManagement: true },
        },
      })),
    });
    let pollSignal: AbortSignal | undefined;
    const getOperation = vi.fn((
      _operationId: string,
      options?: { signal?: AbortSignal },
    ) => {
      pollSignal = options?.signal;
      return new Promise<Operation>((_resolve, reject) => {
        options?.signal?.addEventListener(
          "abort",
          () => reject(new DOMException("aborted", "AbortError")),
          { once: true },
        );
      });
    });
    const deleteFile = vi.fn(async () => undefined);
    const user = userEvent.setup();
    const view = render(
      <ToolsWorkspace
        client={toolsetClient()}
        filesClient={filesClient({ deleteFile })}
        profileClient={profiles}
        skillsClient={skillClient({ getOperation })}
      />,
    );

    await user.click(await screen.findByRole("radio", { name: "文件" }));
    await user.upload(
      screen.getByLabelText("Skill 文件"),
      new File(["archive"], "research.zip", { type: "application/zip" }),
    );
    await user.click(screen.getByRole("button", { name: "安装" }));
    await waitFor(() => expect(getOperation).toHaveBeenCalledTimes(1));
    expect(sessionStorage.length).toBe(1);
    view.unmount();

    expect(pollSignal?.aborted).toBe(true);
    await waitFor(() => expect(deleteFile).toHaveBeenCalledWith(SKILL_FILE.id));
    expect(sessionStorage.length).toBe(1);
  });

  it("resumes an accepted operation after remount and clears it at terminal state", async () => {
    const profiles = profileClient({
      getCapabilities: vi.fn(async () => ({
        ...CAPABILITIES,
        engine: {
          ...CAPABILITIES.engine,
          features: { ...CAPABILITIES.engine.features, skillManagement: true },
        },
      })),
    });
    let firstPollSignal: AbortSignal | undefined;
    const getOperation = vi.fn((
      _operationId: string,
      options?: { signal?: AbortSignal },
    ): Promise<Operation> => {
      if (getOperation.mock.calls.length === 1) {
        firstPollSignal = options?.signal;
        return new Promise<Operation>((_resolve, reject) => {
          options?.signal?.addEventListener(
            "abort",
            () => reject(new DOMException("aborted", "AbortError")),
            { once: true },
          );
        });
      }
      return Promise.resolve(COMPLETED_OPERATION);
    });
    const skills = skillClient({ getOperation });
    const user = userEvent.setup();
    const first = render(
      <ToolsWorkspace
        client={toolsetClient()}
        filesClient={filesClient()}
        profileClient={profiles}
        skillsClient={skills}
      />,
    );

    await user.type(
      await screen.findByRole("textbox", { name: "Registry Skill ID" }),
      "paper-search",
    );
    await user.click(screen.getByRole("button", { name: "安装" }));
    await waitFor(() => expect(getOperation).toHaveBeenCalledTimes(1));
    expect(sessionStorage.length).toBe(1);
    first.unmount();
    expect(firstPollSignal?.aborted).toBe(true);

    render(
      <ToolsWorkspace
        client={toolsetClient()}
        filesClient={filesClient()}
        profileClient={profiles}
        skillsClient={skills}
      />,
    );
    await waitFor(() => expect(getOperation).toHaveBeenCalledTimes(2));
    expect(getOperation).toHaveBeenLastCalledWith(
      QUEUED_OPERATION.id,
      { signal: expect.any(AbortSignal) },
    );
    await waitFor(() => expect(sessionStorage.length).toBe(0));
    expect(await screen.findByText("Research")).toBeTruthy();
  });

  it("isolates recovered operations by backend build and Profile", async () => {
    const managementCapabilities = (backendVersion: string): Capabilities => ({
      ...CAPABILITIES,
      backendVersion,
      engine: {
        ...CAPABILITIES.engine,
        features: { ...CAPABILITIES.engine.features, skillManagement: true },
      },
    });
    const defaultBackend = profileClient({
      getCapabilities: vi.fn(async () => managementCapabilities("0.3.0")),
    });
    const otherBackend = profileClient({
      getCapabilities: vi.fn(async () => managementCapabilities("0.3.1")),
    });
    const workProfiles = PROFILES.map((profile) => ({
      ...profile,
      isActive: profile.id === "work",
    }));
    const workProfileClient = profileClient({
      getCapabilities: vi.fn(async () => managementCapabilities("0.3.0")),
      listProfiles: vi.fn(async () => workProfiles),
    });
    const getOperation = vi.fn((
      _operationId: string,
      options?: { signal?: AbortSignal },
    ): Promise<Operation> => {
      if (getOperation.mock.calls.length === 1) {
        return new Promise<Operation>((_resolve, reject) => {
          options?.signal?.addEventListener(
            "abort",
            () => reject(new DOMException("aborted", "AbortError")),
            { once: true },
          );
        });
      }
      return Promise.resolve(COMPLETED_OPERATION);
    });
    const skills = skillClient({ getOperation });
    const user = userEvent.setup();
    const first = render(
      <ToolsWorkspace
        client={toolsetClient()}
        filesClient={filesClient()}
        profileClient={defaultBackend}
        skillsClient={skills}
      />,
    );
    expect(await screen.findByText("Research")).toBeTruthy();
    await user.type(
      await screen.findByRole("textbox", { name: "Registry Skill ID" }),
      "paper-search",
    );
    const install = screen.getByRole("button", { name: "安装" }) as HTMLButtonElement;
    await waitFor(() => expect(install.disabled).toBe(false));
    await user.click(install);
    await waitFor(() => expect(getOperation).toHaveBeenCalledTimes(1));
    first.unmount();
    expect(sessionStorage.length).toBe(1);

    const second = render(
      <ToolsWorkspace
        client={toolsetClient()}
        filesClient={filesClient()}
        profileClient={otherBackend}
        skillsClient={skills}
      />,
    );
    expect(await screen.findByText("Research")).toBeTruthy();
    expect(getOperation).toHaveBeenCalledTimes(1);
    second.unmount();

    render(
      <ToolsWorkspace
        client={toolsetClient()}
        filesClient={filesClient()}
        profileClient={workProfileClient}
        skillsClient={skills}
      />,
    );
    const profileSelect = await screen.findByRole("combobox", { name: "工具 Profile" });
    await waitFor(() => expect((profileSelect as HTMLSelectElement).value).toBe("work"));
    expect(await screen.findByText("Research")).toBeTruthy();
    expect(getOperation).toHaveBeenCalledTimes(1);

    await user.selectOptions(profileSelect, "default");
    await waitFor(() => expect(getOperation).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(sessionStorage.length).toBe(0));
  });

  it("requires confirmation and keeps the row busy until uninstall completes", async () => {
    const profiles = profileClient({
      getCapabilities: vi.fn(async () => ({
        ...CAPABILITIES,
        engine: {
          ...CAPABILITIES.engine,
          features: { ...CAPABILITIES.engine.features, skillManagement: true },
        },
      })),
    });
    let completeUninstall!: (operation: Operation) => void;
    const uninstallSkill = vi.fn(async (): Promise<Operation> => ({
      ...QUEUED_OPERATION,
      kind: "skillUninstall",
    }));
    const getOperation = vi.fn(() => new Promise<Operation>((resolve) => {
      completeUninstall = resolve;
    }));
    const confirm = vi.spyOn(window, "confirm").mockReturnValueOnce(false).mockReturnValueOnce(true);
    const listSkills = vi.fn(async () => versionedSkills([RESEARCH, LOCAL_SKILL]));
    const user = userEvent.setup();
    render(
      <ToolsWorkspace
        client={toolsetClient()}
        filesClient={filesClient()}
        profileClient={profiles}
        skillsClient={skillClient({ getOperation, listSkills, uninstallSkill })}
      />,
    );

    const uninstall = await screen.findByRole("button", {
      name: `卸载 Skill ${RESEARCH.name} (${RESEARCH.id})`,
    });
    await user.click(uninstall);
    expect(uninstallSkill).not.toHaveBeenCalled();
    await user.click(uninstall);
    expect(confirm).toHaveBeenCalledTimes(2);
    await waitFor(() => expect(getOperation).toHaveBeenCalledTimes(1));
    expect(uninstallSkill).toHaveBeenCalledWith(
      "default",
      RESEARCH.id,
      expect.stringMatching(/^skill-uninstall-/u),
      { signal: expect.any(AbortSignal) },
    );
    expect((uninstall as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByRole("combobox", { name: "工具 Profile" }) as HTMLSelectElement).disabled)
      .toBe(true);

    completeUninstall({
      ...COMPLETED_OPERATION,
      kind: "skillUninstall",
    });
    await waitFor(() => expect(listSkills).toHaveBeenCalledTimes(2));
    await waitFor(() => expect((uninstall as HTMLButtonElement).disabled).toBe(false));
  });
});
