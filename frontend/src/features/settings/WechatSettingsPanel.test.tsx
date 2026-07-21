// @vitest-environment jsdom

import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Persona, ProductCatalogApi } from "../../api/productCatalog";
import type { Capabilities, ProfileSummary, ProfilesApi } from "../../api/profiles";
import type {
  VersionedWechatConfig,
  WechatAccount,
  WechatApi,
  WechatConfig,
  WechatQrStatusResult,
} from "../../api/wechat";
import { WechatSettingsPanel } from "./WechatSettingsPanel";

const PROFILE: ProfileSummary = {
  id: "default",
  displayName: "Default",
  isDefault: true,
  isActive: true,
  color: null,
  avatarFileId: null,
  engineState: "stopped",
  configRevision: "profile-1",
  createdAt: null,
  updatedAt: "2026-07-20T08:00:00Z",
};

const ACCOUNT: WechatAccount = {
  id: "wx-1",
  note: "SynthChat",
  online: true,
  createdAt: "2026-07-20T08:00:00Z",
  lastLoginAt: "2026-07-20T08:05:00Z",
  ilinkUserId: "ilink-user-1",
  loginBaseUrl: "https://ilinkai.weixin.qq.com",
  credentialConfigured: true,
  linkedPersonaId: null,
};

const CONFIG: WechatConfig = {
  revision: "wechat-1",
  baseUrl: "https://ilinkai.weixin.qq.com",
  timeoutSeconds: 35,
  accounts: [],
};

const PERSONA: Persona = {
  id: "persona-1",
  name: "助手",
  avatar: null,
  systemPrompt: "",
  characterPrompt: "",
  outputExamples: "",
  systemInstructions: "",
  provider: "",
  model: "",
  temperature: 0.7,
  maxTokens: 2048,
  toolsEnabled: true,
  memoryEnabled: true,
  proactiveEnabled: false,
  legacyAgentId: null,
  createdAt: "2026-07-20T08:00:00Z",
  updatedAt: "2026-07-20T08:00:00Z",
  revision: 1,
};

function capabilities(wechatAccounts = true, wechatMessaging = true): Capabilities {
  return {
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
        skillManagement: true,
        memoryWrite: true,
        mcpManagement: true,
        oauthAccounts: false,
      },
    },
    sessionStorage: { available: true, schemaVersion: 1, hermesImportAvailable: false },
    sessionSearch: { mode: "fts5" },
    files: { maxBytes: 1_000_000, allowedMimeTypes: ["text/plain"] },
    extensions: {
      activeRunDiscovery: true,
      runQueue: true,
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
      wechatAccounts,
      wechatMessaging,
      plugins: true,
      personas: true,
      moments: true,
      worldbooks: true,
    },
  };
}

type ProfileClient = Pick<ProfilesApi, "getCapabilities" | "listProfiles">;
type CatalogClient = Pick<ProductCatalogApi, "listPersonas">;

function profileClient(available = true, messagingAvailable = true): ProfileClient {
  return {
    getCapabilities: vi.fn(async () => capabilities(available, messagingAvailable)),
    listProfiles: vi.fn(async () => [PROFILE]),
  };
}

function catalogClient(personas: Persona[] = [PERSONA]): CatalogClient {
  return { listPersonas: vi.fn(async () => personas) };
}

function versioned(value: WechatConfig = CONFIG): VersionedWechatConfig {
  return { value, etag: `"${value.revision}"` };
}

function wechatClient(overrides: Partial<WechatApi> = {}): WechatApi {
  return {
    getConfig: vi.fn(async () => versioned()),
    updateConfig: vi.fn(async (_profileId, patch) => versioned({
      ...CONFIG,
      ...patch,
      revision: "wechat-2",
    })),
    startQr: vi.fn(async () => ({
      qrcode: "challenge-1",
      qrImage: "data:image/svg+xml;base64,PHN2Zy8+",
      baseUrl: CONFIG.baseUrl,
    })),
    checkQr: vi.fn(async () => ({ status: "waiting", message: "等待扫码", account: null, host: null })),
    updateAccountLink: vi.fn(async () => versioned()),
    pollMessages: vi.fn(async () => ({ messages: [], nextCursor: null, receivedCount: 0, skippedCount: 0 })),
    sendMessage: vi.fn(async () => ({ accepted: true, messageId: null })),
    ...overrides,
  };
}

afterEach(() => cleanup());

describe("WechatSettingsPanel", () => {
  it("gates the UI with the explicit account-management capability", async () => {
    const client = wechatClient();
    render(<WechatSettingsPanel catalogClient={catalogClient()} client={client} profileClient={profileClient(false)} />);

    expect(await screen.findByText("当前 Rust 后端未启用微信账号管理能力。")).toBeTruthy();
    expect(client.getConfig).not.toHaveBeenCalled();
  });

  it("keeps login and Persona binding available when explicit messaging is disabled", async () => {
    const accountConfig = { ...CONFIG, accounts: [ACCOUNT] };
    render(<WechatSettingsPanel
      catalogClient={catalogClient()}
      client={wechatClient({ getConfig: vi.fn(async () => versioned(accountConfig)) })}
      profileClient={profileClient(true, false)}
    />);

    expect((await screen.findByRole("button", { name: "扫码登录" }) as HTMLButtonElement).disabled).toBe(false);
    expect((screen.getByRole("combobox", { name: "绑定角色 SynthChat" }) as HTMLSelectElement).disabled).toBe(false);
    expect((screen.getByRole("button", { name: "拉取消息" }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByRole("textbox", { name: "接收方 ID SynthChat" }) as HTMLInputElement).disabled).toBe(true);
    expect((screen.getByRole("textbox", { name: "消息 SynthChat" }) as HTMLTextAreaElement).disabled).toBe(true);
    expect((screen.getByRole("button", { name: "发送" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("loads and saves the selected Profile configuration with its current ETag", async () => {
    const updateConfig = vi.fn(async (_profileId, patch: { baseUrl?: string; timeoutSeconds?: number }) => versioned({
      ...CONFIG,
      ...patch,
      revision: "wechat-2",
    }));
    const client = wechatClient({ updateConfig });
    const user = userEvent.setup();
    render(<WechatSettingsPanel catalogClient={catalogClient()} client={client} profileClient={profileClient()} />);

    const baseUrl = await screen.findByRole("textbox", { name: "iLink Base URL" });
    const timeout = screen.getByRole("spinbutton", { name: "状态超时（秒）" });
    await user.clear(baseUrl);
    await user.type(baseUrl, "https://ilinkai.weixin.qq.com");
    await user.clear(timeout);
    await user.type(timeout, "45");
    await user.click(screen.getByRole("button", { name: "保存接口" }));

    await waitFor(() => expect(updateConfig).toHaveBeenCalledWith("default", {
      baseUrl: "https://ilinkai.weixin.qq.com",
      timeoutSeconds: 45,
    }, '"wechat-1"'));
    expect(await screen.findByText("微信连接配置已保存。")).toBeTruthy();
  });

  it("starts QR login, polls immediately, and refreshes accounts after confirmation", async () => {
    const confirmedConfig = { ...CONFIG, revision: "wechat-2", accounts: [ACCOUNT] };
    const getConfig = vi.fn()
      .mockResolvedValueOnce(versioned())
      .mockResolvedValueOnce(versioned(confirmedConfig));
    let confirmQr!: (result: WechatQrStatusResult) => void;
    const checkQr = vi.fn(() => new Promise<WechatQrStatusResult>((resolve) => {
      confirmQr = resolve;
    }));
    const client = wechatClient({ getConfig, checkQr });
    const user = userEvent.setup();
    render(<WechatSettingsPanel catalogClient={catalogClient()} client={client} profileClient={profileClient()} />);

    await user.click(await screen.findByRole("button", { name: "扫码登录" }));
    expect((await screen.findByRole("img", {
      name: "微信登录二维码",
    }) as HTMLImageElement).getAttribute("src")).toBe("data:image/svg+xml;base64,PHN2Zy8+");
    await waitFor(() => expect(checkQr).toHaveBeenCalledWith("default", {
      qrcode: "challenge-1",
      baseUrl: CONFIG.baseUrl,
    }, { signal: expect.any(AbortSignal) }));
    confirmQr({
      status: "confirmed",
      message: null,
      account: ACCOUNT,
      host: "ilinkai.weixin.qq.com",
    });
    expect(await screen.findByText("SynthChat")).toBeTruthy();
    expect(screen.getByText("密钥链已保存")).toBeTruthy();
    expect(screen.getByText("微信账号 SynthChat 已登录，凭据已写入系统密钥链。")).toBeTruthy();
    expect(getConfig).toHaveBeenCalledTimes(2);
  });

  it("reloads the current Profile when the refresh button is pressed", async () => {
    const refreshed = { ...CONFIG, revision: "wechat-2", accounts: [ACCOUNT] };
    const getConfig = vi.fn()
      .mockResolvedValueOnce(versioned())
      .mockResolvedValueOnce(versioned(refreshed));
    const user = userEvent.setup();
    render(<WechatSettingsPanel catalogClient={catalogClient()} client={wechatClient({ getConfig })} profileClient={profileClient()} />);

    await user.click(await screen.findByRole("button", { name: "刷新微信账号" }));

    await waitFor(() => expect(getConfig).toHaveBeenCalledTimes(2));
    expect(await screen.findByText("SynthChat")).toBeTruthy();
  });

  it("binds a Persona and keeps message polling and sending explicit", async () => {
    const accountConfig = { ...CONFIG, accounts: [ACCOUNT] };
    const updateAccountLink = vi.fn(async () => versioned({
      ...accountConfig,
      revision: "wechat-2",
      accounts: [{ ...ACCOUNT, linkedPersonaId: PERSONA.id }],
    }));
    const pollMessages = vi.fn(async () => ({
      messages: [{ id: "message-1", peer: "peer-1", text: "你好" }],
      nextCursor: "cursor-2",
      receivedCount: 2,
      skippedCount: 1,
    }));
    const sendMessage = vi.fn(async () => ({ accepted: true, messageId: "sent-1" }));
    const client = wechatClient({
      getConfig: vi.fn(async () => versioned(accountConfig)),
      updateAccountLink,
      pollMessages,
      sendMessage,
    });
    const user = userEvent.setup();
    render(<WechatSettingsPanel catalogClient={catalogClient()} client={client} profileClient={profileClient()} />);

    await user.selectOptions(await screen.findByRole("combobox", { name: "绑定角色 SynthChat" }), PERSONA.id);
    await waitFor(() => expect(updateAccountLink).toHaveBeenCalledWith(
      "default",
      ACCOUNT.id,
      { linkedPersonaId: PERSONA.id },
      '"wechat-1"',
    ));
    expect(await screen.findByText("已绑定角色 助手。")).toBeTruthy();

    await user.click(screen.getByRole("button", { name: "拉取消息" }));
    await waitFor(() => expect(pollMessages).toHaveBeenNthCalledWith(1, "default", ACCOUNT.id, { cursor: null }));
    expect(await screen.findByText("你好")).toBeTruthy();
    expect(screen.getByText("收到 2 · 跳过 1")).toBeTruthy();

    await user.click(screen.getByRole("button", { name: "拉取消息" }));
    await waitFor(() => expect(pollMessages).toHaveBeenNthCalledWith(2, "default", ACCOUNT.id, { cursor: "cursor-2" }));

    const peer = screen.getByRole("textbox", { name: "接收方 ID SynthChat" }) as HTMLInputElement;
    expect(peer.value).toBe("peer-1");
    await user.type(screen.getByRole("textbox", { name: "消息 SynthChat" }), "收到");
    await user.click(screen.getByRole("button", { name: "发送" }));
    await waitFor(() => expect(sendMessage).toHaveBeenCalledWith("default", ACCOUNT.id, {
      peer: "peer-1",
      text: "收到",
    }));
    expect(await screen.findByText("微信消息已发送（sent-1）。")).toBeTruthy();
  });

  it("disables a Persona already linked to another account", async () => {
    const secondAccount: WechatAccount = {
      ...ACCOUNT,
      id: "wx-2",
      note: "Second",
      ilinkUserId: "ilink-user-2",
    };
    const linked = {
      ...CONFIG,
      accounts: [{ ...ACCOUNT, linkedPersonaId: PERSONA.id }, secondAccount],
    };
    render(<WechatSettingsPanel
      catalogClient={catalogClient()}
      client={wechatClient({ getConfig: vi.fn(async () => versioned(linked)) })}
      profileClient={profileClient()}
    />);

    const options = await screen.findAllByRole("option", { name: PERSONA.name });
    expect((options[0] as HTMLOptionElement).disabled).toBe(false);
    expect((options[1] as HTMLOptionElement).disabled).toBe(true);
  });
});
