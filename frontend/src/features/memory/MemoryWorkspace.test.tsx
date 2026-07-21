// @vitest-environment jsdom

import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { DesktopConnectionError } from "../../api/desktopConnection";
import {
  MemoryApiError,
  type MemoriesApi,
  type Memory,
  type MemoryPage,
} from "../../api/memories";
import type { Capabilities, ProfileSummary, ProfilesApi } from "../../api/profiles";
import { MemoryWorkspace } from "./MemoryWorkspace";

const CAPABILITIES: Capabilities = {
  contractVersion: "v1",
  backendVersion: "0.2.0",
  engine: {
    kind: "hermes-rust",
    available: true,
    version: "0.2.0",
    pinnedCommit: null,
    features: {
      runStreaming: true,
      reasoningStreaming: true,
      toolProgress: true,
      approvals: true,
      clarifications: true,
      asyncToolDelivery: false,
      profileManagement: true,
      skillManagement: false,
      memoryWrite: true,
      mcpManagement: false,
      oauthAccounts: false,
    },
  },
  sessionStorage: { available: true, schemaVersion: 10, hermesImportAvailable: true },
  sessionSearch: { mode: "fts5" },
  files: { maxBytes: 1_000_000, allowedMimeTypes: ["text/plain"] },
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

const PROFILES: ProfileSummary[] = [
  {
    id: "default",
    displayName: "Default",
    isDefault: true,
    isActive: true,
    color: null,
    avatarFileId: null,
    engineState: "stopped",
    configRevision: "rev-default",
    createdAt: null,
    updatedAt: "2026-07-17T00:00:00Z",
  },
  {
    id: "work",
    displayName: "Work",
    isDefault: false,
    isActive: false,
    color: null,
    avatarFileId: null,
    engineState: "stopped",
    configRevision: "rev-work",
    createdAt: null,
    updatedAt: "2026-07-17T00:00:00Z",
  },
];

const BASE_MEMORY: Memory = {
  id: "memory-1",
  target: "memory",
  content: "Prefer concise status updates.",
  provider: "builtin",
};

function page(
  items: Memory[] = [BASE_MEMORY],
  revision = "memory_default_1",
  overrides: Partial<MemoryPage> = {},
): MemoryPage {
  return {
    items,
    nextCursor: null,
    revision,
    provider: "builtin",
    charsUsed: items.reduce((total, item) => total + Array.from(item.content).length, 0),
    charLimit: 20_000,
    promptSafety: "clean",
    capabilities: { create: true, update: true, delete: true, search: true },
    ...overrides,
  };
}

function profileClient(
  capabilities: Capabilities = CAPABILITIES,
  profiles: ProfileSummary[] = PROFILES,
): Pick<ProfilesApi, "getCapabilities" | "listProfiles"> {
  return {
    getCapabilities: vi.fn(async () => capabilities),
    listProfiles: vi.fn(async () => profiles),
  };
}

function staticMemoryClient(memoryPage = page()): MemoriesApi {
  return {
    listMemories: vi.fn(async () => ({
      value: memoryPage,
      etag: `"${memoryPage.revision}"`,
    })),
    createMemory: vi.fn(),
    updateMemory: vi.fn(),
    deleteMemory: vi.fn(),
  };
}

afterEach(() => cleanup());

describe("Memory workspace", () => {
  it("does not probe Profile or Memory endpoints when memoryWrite is disabled", async () => {
    const profiles = profileClient({
      ...CAPABILITIES,
      engine: {
        ...CAPABILITIES.engine,
        features: { ...CAPABILITIES.engine.features, memoryWrite: false },
      },
    });
    const memories = staticMemoryClient();

    render(<MemoryWorkspace client={memories} profileClient={profiles} />);

    expect(await screen.findByText("记忆管理暂不可用")).toBeTruthy();
    expect(profiles.getCapabilities).toHaveBeenCalledTimes(1);
    expect(profiles.listProfiles).not.toHaveBeenCalled();
    expect(memories.listMemories).not.toHaveBeenCalled();
  });

  it("isolates lists by Profile and target", async () => {
    const listMemories = vi.fn(async (profileId: string, request: { target: "memory" | "user" }) => {
      const revision = `memory_${profileId}_${request.target}_1`;
      return {
        value: page([{
          id: `${profileId}-${request.target}`,
          target: request.target,
          content: `${profileId} ${request.target}`,
          provider: "builtin",
        }], revision),
        etag: `"${revision}"`,
      };
    });
    const client = { ...staticMemoryClient(), listMemories };
    const user = userEvent.setup();

    render(<MemoryWorkspace client={client} profileClient={profileClient()} />);

    expect(await screen.findByText("default memory")).toBeTruthy();
    await user.selectOptions(screen.getByLabelText("记忆 Profile"), "work");
    expect(await screen.findByText("work memory")).toBeTruthy();
    await user.click(screen.getByRole("tab", { name: "用户信息" }));
    expect(await screen.findByText("work user")).toBeTruthy();

    expect(listMemories).toHaveBeenCalledWith(
      "default",
      expect.objectContaining({ target: "memory" }),
      expect.any(Object),
    );
    expect(listMemories).toHaveBeenCalledWith(
      "work",
      expect.objectContaining({ target: "user" }),
      expect.any(Object),
    );
  });

  it("creates, edits, and deletes with the latest target revision", async () => {
    let revision = 1;
    let items = [BASE_MEMORY];
    const listMemories = vi.fn(async () => {
      const revisionId = `memory_default_${revision}`;
      return { value: page(items, revisionId), etag: `"${revisionId}"` };
    });
    const createMemory = vi.fn(async (
      _profileId: string,
      input: { target: "memory" | "user"; content: string },
    ) => {
      revision += 1;
      const value: Memory = { id: "memory-2", ...input, provider: "builtin" };
      items = [...items, value];
      return { value, etag: `"memory_default_${revision}"` };
    });
    const updateMemory = vi.fn(async (
      _profileId: string,
      memoryId: string,
      patch: { content: string },
    ) => {
      revision += 1;
      items = items.map((item) => item.id === memoryId ? { ...item, content: patch.content } : item);
      return {
        value: items.find((item) => item.id === memoryId)!,
        etag: `"memory_default_${revision}"`,
      };
    });
    const deleteMemory = vi.fn(async (_profileId: string, memoryId: string) => {
      revision += 1;
      items = items.filter((item) => item.id !== memoryId);
      return { etag: `"memory_default_${revision}"` };
    });
    const client: MemoriesApi = {
      listMemories,
      createMemory,
      updateMemory,
      deleteMemory,
    };
    const user = userEvent.setup();

    render(<MemoryWorkspace client={client} profileClient={profileClient()} />);
    expect(await screen.findByText(BASE_MEMORY.content)).toBeTruthy();

    await user.click(screen.getByRole("button", { name: "新增" }));
    await user.type(screen.getByLabelText("新增记忆内容"), "Use Rust for local services.");
    await user.click(screen.getByRole("button", { name: "添加" }));
    expect(await screen.findByText("Use Rust for local services.")).toBeTruthy();
    expect(createMemory).toHaveBeenCalledWith(
      "default",
      { target: "memory", content: "Use Rust for local services." },
      '"memory_default_1"',
      expect.stringMatching(/^memory-/u),
    );

    await user.click(screen.getByRole("button", { name: "编辑记忆 memory-2" }));
    const editField = screen.getByLabelText("编辑记忆内容 memory-2");
    await user.clear(editField);
    await user.type(editField, "Keep services local.");
    await user.click(screen.getByRole("button", { name: "保存" }));
    expect(await screen.findByText("Keep services local.")).toBeTruthy();
    expect(updateMemory).toHaveBeenCalledWith(
      "default",
      "memory-2",
      { content: "Keep services local." },
      '"memory_default_2"',
    );

    await user.click(screen.getByRole("button", { name: "删除记忆 memory-2" }));
    await user.click(screen.getByRole("button", { name: "确认删除" }));
    await waitFor(() => expect(screen.queryByText("Keep services local.")).toBeNull());
    expect(deleteMemory).toHaveBeenCalledWith("default", "memory-2", '"memory_default_3"');
  });

  it("searches and paginates without mixing revisions", async () => {
    const second: Memory = {
      id: "memory-2",
      target: "memory",
      content: "Second page",
      provider: "builtin",
    };
    const result: Memory = {
      id: "memory-search",
      target: "memory",
      content: "Search result",
      provider: "builtin",
    };
    const listMemories = vi.fn(async (_profileId: string, request: {
      target: "memory" | "user";
      query?: string;
      cursor?: string;
    }) => {
      if (request.query) {
        return {
          value: page([result], "memory_default_1"),
          etag: '"memory_default_1"',
        };
      }
      if (request.cursor) {
        return {
          value: page([second], "memory_default_1"),
          etag: '"memory_default_1"',
        };
      }
      return {
        value: page([BASE_MEMORY], "memory_default_1", { nextCursor: "cursor-2" }),
        etag: '"memory_default_1"',
      };
    });
    const user = userEvent.setup();

    render(<MemoryWorkspace
      client={{ ...staticMemoryClient(), listMemories }}
      profileClient={profileClient()}
    />);

    expect(await screen.findByText(BASE_MEMORY.content)).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "加载更多" }));
    expect(await screen.findByText("Second page")).toBeTruthy();
    await user.type(screen.getByPlaceholderText("搜索当前目标"), "result");
    await user.click(screen.getByRole("button", { name: "搜索" }));
    expect(await screen.findByText("Search result")).toBeTruthy();

    expect(listMemories).toHaveBeenCalledWith(
      "default",
      expect.objectContaining({ cursor: "cursor-2", target: "memory" }),
      expect.any(Object),
    );
    expect(listMemories).toHaveBeenCalledWith(
      "default",
      expect.objectContaining({ query: "result", target: "memory" }),
      expect.any(Object),
    );
  });

  it("reloads stale revisions and applies provider capability gating", async () => {
    const current = page([BASE_MEMORY], "memory_default_1");
    const latestMemory = { ...BASE_MEMORY, content: "Updated elsewhere" };
    const latest = page([latestMemory], "memory_default_2", {
      capabilities: { create: false, update: false, delete: false, search: false },
      promptSafety: "blocked",
    });
    const listMemories = vi.fn()
      .mockResolvedValueOnce({ value: current, etag: '"memory_default_1"' })
      .mockResolvedValue({ value: latest, etag: '"memory_default_2"' });
    const updateMemory = vi.fn(async () => {
      throw new MemoryApiError("http", "Revision conflict", {
        status: 409,
        code: "revision_conflict",
        requestId: "req-stale",
        etag: '"memory_default_2"',
      });
    });
    const client = { ...staticMemoryClient(), listMemories, updateMemory };
    const user = userEvent.setup();

    render(<MemoryWorkspace client={client} profileClient={profileClient()} />);
    expect(await screen.findByText(BASE_MEMORY.content)).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "编辑记忆 memory-1" }));
    const field = screen.getByLabelText("编辑记忆内容 memory-1");
    await user.clear(field);
    await user.type(field, "Local edit");
    await user.click(screen.getByRole("button", { name: "保存" }));

    expect(await screen.findByText("Updated elsewhere")).toBeTruthy();
    expect(screen.getByText(/已重新加载最新版本/u)).toBeTruthy();
    expect(screen.getByText(/未通过提示安全检查/u)).toBeTruthy();
    expect(screen.getByRole("button", { name: "新增" }).hasAttribute("disabled")).toBe(true);
    expect(screen.getByPlaceholderText("当前 provider 不支持搜索").hasAttribute("disabled")).toBe(true);
    expect(screen.getByRole("button", { name: "编辑记忆 memory-1" }).hasAttribute("disabled")).toBe(true);
    expect(screen.getByRole("button", { name: "删除记忆 memory-1" }).hasAttribute("disabled")).toBe(true);
  });

  it("shows the protected desktop requirement", async () => {
    const profiles = profileClient();
    vi.mocked(profiles.getCapabilities).mockRejectedValueOnce(
      new DesktopConnectionError("desktop_unavailable", "Desktop required"),
    );

    render(<MemoryWorkspace client={staticMemoryClient()} profileClient={profiles} />);

    expect(await screen.findByText("请在 SynthChat Desktop 中打开")).toBeTruthy();
    expect(screen.getByText(/受保护的桌面后端/u)).toBeTruthy();
  });
});
