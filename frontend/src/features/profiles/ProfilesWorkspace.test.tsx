// @vitest-environment jsdom

import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { DesktopConnectionError } from "../../api/desktopConnection";
import { ProfileApiError } from "../../api/profiles";
import type {
  Capabilities,
  ProfileConfig,
  ProfileMetadata,
  ProfileSummary,
  ProfilesApi,
} from "../../api/profiles";
import { ProfilesWorkspace } from "./ProfilesWorkspace";

const NOW = "2026-07-16T08:00:00Z";
const CAPABILITIES: Capabilities = {
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
    webSearch: true,
    webExtract: true,
    browserAutomation: false,
    browserCdp: false,
    browserDownloads: false,
    mcpStdio: false,
    mcpStreamableHttp: false,
    mcpSse: false,
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
    configRevision: "rev_default_1",
    createdAt: null,
    updatedAt: NOW,
  },
  {
    id: "work",
    displayName: "Work",
    isDefault: false,
    isActive: false,
    color: "#087f9d",
    avatarFileId: null,
    engineState: "stopped",
    configRevision: "rev_work_1",
    createdAt: NOW,
    updatedAt: NOW,
  },
];
const CONFIG: ProfileConfig = {
  revision: "rev_default_1",
  model: { provider: "openai", model: "gpt-5", baseUrl: null, reasoningEffort: null },
  codeExecution: { mode: "project", timeoutSeconds: 300, maxToolCalls: 50 },
  toolsets: {},
  skills: {},
  memoryProvider: "builtin",
  platforms: {},
  extensions: {},
};

function metadata(profileId: string): ProfileMetadata {
  const summary = PROFILES.find((profile) => profile.id === profileId) ?? PROFILES[0]!;
  return {
    id: summary.id,
    displayName: summary.displayName,
    isDefault: summary.isDefault,
    color: summary.color,
    avatarFileId: summary.avatarFileId,
    createdAt: summary.createdAt,
    updatedAt: summary.updatedAt,
  };
}

function makeClient(overrides: Partial<ProfilesApi> = {}): ProfilesApi {
  const client: ProfilesApi = {
    getCapabilities: vi.fn(async () => CAPABILITIES),
    listProviders: vi.fn(async () => [{
      id: "openai",
      displayName: "OpenAI",
      defaultBaseUrl: null,
      requiresSecret: true,
      secretNames: ["OPENAI_API_KEY"],
      supportsModelDiscovery: false,
    }]),
    listProfiles: vi.fn(async () => PROFILES),
    createProfile: vi.fn(async (input) => ({
      value: { ...metadata("work"), id: input.id, displayName: input.displayName },
      etag: '"meta_created"',
    })),
    getProfile: vi.fn(async (profileId) => ({
      value: metadata(profileId),
      etag: `"meta_${profileId}"`,
    })),
    updateProfile: vi.fn(async (profileId, patch) => ({
      value: { ...metadata(profileId), ...patch, updatedAt: NOW },
      etag: `"meta_${profileId}_2"`,
    })),
    deleteProfile: vi.fn(async () => undefined),
    activateProfile: vi.fn(async (profileId) => ({
      ...PROFILES.find((profile) => profile.id === profileId)!,
      isActive: true,
    })),
    getProfileConfig: vi.fn(async (profileId) => ({
      value: { ...CONFIG, revision: `rev_${profileId}_1` },
      etag: `"rev_${profileId}_1"`,
    })),
    updateProfileConfig: vi.fn(async (profileId, patch) => ({
      value: {
        ...CONFIG,
        revision: `rev_${profileId}_2`,
        model: { ...CONFIG.model, ...patch.model },
        codeExecution: { ...CONFIG.codeExecution, ...patch.codeExecution },
      },
      etag: `"rev_${profileId}_2"`,
    })),
    listSecretStatuses: vi.fn(async () => [{
      name: "OPENAI_API_KEY",
      configured: false,
      storage: "osKeychain" as const,
    }]),
    putSecret: vi.fn(async (_profileId: string, secretName: string) => ({
      name: secretName,
      configured: true,
      storage: "osKeychain" as const,
      updatedAt: NOW,
    })),
    deleteSecret: vi.fn(async () => undefined),
  };
  return Object.assign(client, overrides);
}

afterEach(() => cleanup());

describe("ProfilesWorkspace", () => {
  it("shows a desktop-only message in a regular browser", async () => {
    const client = makeClient({
      getCapabilities: vi.fn(async () => {
        throw new DesktopConnectionError("desktop_unavailable", "desktop required");
      }),
    });

    render(<ProfilesWorkspace client={client} />);

    expect(await screen.findByText("请在 SynthChat Desktop 中打开")).toBeTruthy();
    expect(client.listProfiles).not.toHaveBeenCalled();
  });

  it("keeps the Profile domain unavailable when capability is false", async () => {
    const client = makeClient({
      getCapabilities: vi.fn(async () => ({
        ...CAPABILITIES,
        engine: {
          ...CAPABILITIES.engine,
          features: { ...CAPABILITIES.engine.features, profileManagement: false },
        },
      })),
    });

    render(<ProfilesWorkspace client={client} />);

    expect(await screen.findByText("Profile 暂不可用")).toBeTruthy();
    expect(client.listProviders).not.toHaveBeenCalled();
  });

  it("keeps selection separate from activation", async () => {
    const user = userEvent.setup();
    const client = makeClient();
    render(<ProfilesWorkspace client={client} />);
    await screen.findByText("模型配置");

    await user.click(screen.getByRole("button", { name: "Work (work)" }));
    await waitFor(() => expect(client.getProfile).toHaveBeenCalledWith("work", expect.anything()));
    expect(client.activateProfile).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: "设为活动" }));
    await waitFor(() => expect(client.activateProfile).toHaveBeenCalledWith("work"));
  });

  it("filters unconfigured catalog secrets to the current Provider", async () => {
    const client = makeClient({
      listProviders: vi.fn(async () => [
        {
          id: "openai",
          displayName: "OpenAI",
          defaultBaseUrl: null,
          requiresSecret: true,
          secretNames: ["OPENAI_API_KEY"],
          supportsModelDiscovery: false,
        },
        {
          id: "anthropic",
          displayName: "Anthropic",
          defaultBaseUrl: null,
          requiresSecret: true,
          secretNames: ["ANTHROPIC_API_KEY"],
          supportsModelDiscovery: false,
        },
      ]),
      listSecretStatuses: vi.fn(async () => [
        { name: "OPENAI_API_KEY", configured: false, storage: "osKeychain" as const },
        { name: "ANTHROPIC_API_KEY", configured: false, storage: "osKeychain" as const },
      ]),
    });

    render(<ProfilesWorkspace client={client} />);

    expect(await screen.findByLabelText("OPENAI_API_KEY 密钥值")).toBeTruthy();
    expect(screen.queryByLabelText("ANTHROPIC_API_KEY 密钥值")).toBeNull();
  });

  it("creates a Profile with a stable idempotency key", async () => {
    const user = userEvent.setup();
    const client = makeClient();
    vi.mocked(client.createProfile).mockRejectedValueOnce(
      new ProfileApiError("http", "Temporary failure", { status: 503, retryable: true }),
    );
    render(<ProfilesWorkspace client={client} />);
    await screen.findByText("模型配置");

    await user.click(screen.getByRole("button", { name: "创建 Profile" }));
    const submit = screen.getByRole("button", { name: "创建" });
    const form = submit.closest("form");
    expect(form).not.toBeNull();
    await user.type(within(form!).getByPlaceholderText("work"), "personal");
    await user.type(within(form!).getByLabelText("显示名称"), "Personal");
    await user.click(submit);
    await screen.findByText("Temporary failure");
    await user.click(submit);

    await waitFor(() => expect(client.createProfile).toHaveBeenCalledTimes(2));
    expect(client.createProfile).toHaveBeenLastCalledWith(
      { id: "personal", displayName: "Personal", cloneFromProfileId: null },
      expect.stringMatching(/^.{8,128}$/u),
    );
    expect(vi.mocked(client.createProfile).mock.calls[0]?.[1]).toBe(
      vi.mocked(client.createProfile).mock.calls[1]?.[1],
    );
  });

  it("allows an 80 Unicode scalar display name in the creation form", async () => {
    const user = userEvent.setup();
    const client = makeClient();
    render(<ProfilesWorkspace client={client} />);
    await screen.findByText("模型配置");

    await user.click(screen.getByRole("button", { name: "创建 Profile" }));
    const form = screen.getByRole("button", { name: "创建" }).closest("form");
    expect(form).not.toBeNull();
    const displayName = within(form!).getByLabelText("显示名称") as HTMLInputElement;
    const boundaryValue = "\u{1f642}".repeat(80);

    await user.type(displayName, boundaryValue);

    expect(displayName.value).toBe(boundaryValue);
    expect(Array.from(displayName.value)).toHaveLength(80);
  });

  it("saves metadata and config with their distinct ETags", async () => {
    const user = userEvent.setup();
    const client = makeClient();
    render(<ProfilesWorkspace client={client} />);
    await screen.findByText("模型配置");

    const displayName = screen.getByLabelText("显示名称");
    await user.clear(displayName);
    await user.type(displayName, "Renamed Default");
    await user.click(screen.getByRole("button", { name: "保存信息" }));
    await waitFor(() => expect(client.updateProfile).toHaveBeenCalledWith(
      "default",
      { displayName: "Renamed Default" },
      '"meta_default"',
    ));

    const model = screen.getByLabelText("模型");
    await user.clear(model);
    await user.type(model, "gpt-5.1");
    await user.click(screen.getByRole("button", { name: "保存配置" }));
    await waitFor(() => expect(client.updateProfileConfig).toHaveBeenCalledWith(
      "default",
      { model: { model: "gpt-5.1" } },
      '"rev_default_1"',
    ));
  });

  it("saves only changed code execution settings in the shared config patch", async () => {
    const user = userEvent.setup();
    const client = makeClient();
    render(<ProfilesWorkspace client={client} />);
    await screen.findByText("模型配置");

    await user.selectOptions(screen.getByLabelText("执行模式"), "strict");
    const timeout = screen.getByLabelText("超时（秒）");
    await user.clear(timeout);
    await user.type(timeout, "120");
    const maxToolCalls = screen.getByLabelText("工具调用上限");
    await user.clear(maxToolCalls);
    await user.type(maxToolCalls, "12");
    await user.click(screen.getByRole("button", { name: "保存配置" }));

    await waitFor(() => expect(client.updateProfileConfig).toHaveBeenCalledWith(
      "default",
      {
        codeExecution: {
          mode: "strict",
          timeoutSeconds: 120,
          maxToolCalls: 12,
        },
      },
      '"rev_default_1"',
    ));
  });

  it("rejects an out-of-range code execution timeout before the request", async () => {
    const user = userEvent.setup();
    const client = makeClient();
    render(<ProfilesWorkspace client={client} />);
    await screen.findByText("模型配置");

    const timeout = screen.getByLabelText("超时（秒）");
    await user.clear(timeout);
    await user.type(timeout, "601");
    await user.click(screen.getByRole("button", { name: "保存配置" }));

    expect((await screen.findByRole("alert")).textContent).toContain(
      "代码执行超时必须是 1 到 600 秒之间的整数。",
    );
    expect(client.updateProfileConfig).not.toHaveBeenCalled();
  });

  it("clears a password after PUT succeeds and supports DELETE", async () => {
    const user = userEvent.setup();
    const client = makeClient();
    render(<ProfilesWorkspace client={client} />);
    const password = await screen.findByLabelText("OPENAI_API_KEY 密钥值") as HTMLInputElement;

    await user.type(password, "top-secret-value");
    await user.click(screen.getByRole("button", { name: "保存 OPENAI_API_KEY" }));

    await waitFor(() => expect(password.value).toBe(""));
    expect(client.putSecret).toHaveBeenCalledWith(
      "default",
      "OPENAI_API_KEY",
      "top-secret-value",
    );

    await user.click(screen.getByRole("button", { name: "删除 OPENAI_API_KEY" }));
    await waitFor(() => expect(client.deleteSecret).toHaveBeenCalledWith(
      "default",
      "OPENAI_API_KEY",
    ));
  });
});
