// @vitest-environment jsdom

import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { DesktopConnectionError } from "../../api/desktopConnection";
import {
  ProfileApiError,
  type Capabilities,
  type ProfileSummary,
  type ProfilesApi,
} from "../../api/profiles";
import {
  SessionImportApiError,
  type HermesImportPreview,
  type HermesImportResult,
  type SessionImportsApi,
} from "../../api/sessionImports";
import {
  SessionApiError,
  type Message,
  type MessagePage,
  type Session,
  type SessionPage,
  type SessionsApi,
} from "../../api/sessions";
import { SessionMessageTimeline, SessionsWorkspace } from "./SessionsWorkspace";

const NOW = "2026-07-16T08:00:00Z";
const IMPORT_FINGERPRINT_A = "a".repeat(64);
const IMPORT_FINGERPRINT_B = "b".repeat(64);
const REFERENCE_COMMIT = "3f2a389c7e1f1729cad91ae63c26fb08c7753c74";
const CAPABILITIES: Capabilities = {
  contractVersion: "v1",
  backendVersion: "0.3.0",
  engine: {
    kind: "hermes-rust",
    available: true,
    version: "0.3.0",
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

const IMPORT_CAPABILITIES: Capabilities = {
  ...CAPABILITIES,
  sessionStorage: {
    ...CAPABILITIES.sessionStorage,
    hermesImportAvailable: true,
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
    configRevision: "config-2",
    createdAt: NOW,
    updatedAt: NOW,
  },
];

const SESSION: Session = {
  id: "session-1",
  profileId: "default",
  personaId: "persona_0123456789abcdef0123456789abcdef",
  title: "Migration review",
  preview: "Review the Rust session store",
  source: "desktop",
  model: "gpt-5",
  messageCount: 1,
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
    { type: "text", text: "The session store is ready." },
    { type: "file", fileId: "file-1", name: "report.md", mimeType: "text/markdown" },
  ],
  reasoning: "Validated the schema.",
  toolCalls: [{
    callId: "call-1",
    name: "read_file",
    status: "completed",
    resultSummary: "Read schema",
  }],
  usage: { promptTokens: 10, completionTokens: 5, totalTokens: 15, cost: null },
  createdAt: NOW,
};

const READY_IMPORT_PREVIEW: HermesImportPreview = {
  state: "ready",
  adapterId: "hermes-agent-state-v21",
  referenceCommit: REFERENCE_COMMIT,
  schemaVersion: 21,
  snapshotFingerprint: IMPORT_FINGERPRINT_A,
  sessionCount: 2,
  messageCount: 12,
  modelUsageRowCount: 3,
  attachmentCount: 1,
  rewoundMessageCount: 4,
  warnings: [{ code: "active_null_treated_as_active", count: 2 }],
  warningsDropped: 0,
};

const ABSENT_IMPORT_PREVIEW: HermesImportPreview = {
  state: "absent",
  adapterId: "hermes-agent-state-v21",
  referenceCommit: REFERENCE_COMMIT,
  schemaVersion: null,
  snapshotFingerprint: null,
  sessionCount: null,
  messageCount: null,
  modelUsageRowCount: null,
  attachmentCount: null,
  rewoundMessageCount: null,
  warnings: [],
  warningsDropped: 0,
};

const IMPORT_RESULT: HermesImportResult = {
  importId: "import_hv21_1",
  profileId: "default",
  disposition: "imported",
  adapterId: "hermes-agent-state-v21",
  referenceCommit: REFERENCE_COMMIT,
  sourceSchemaVersion: 21,
  snapshotFingerprint: IMPORT_FINGERPRINT_A,
  importedSessionCount: 2,
  importedMessageCount: 12,
  importedModelUsageRowCount: 3,
  omittedAttachmentCount: 1,
  warnings: [{ code: "attachment_omitted", count: 1 }],
  warningsDropped: 0,
};

function page(items: Session[] = [SESSION], nextCursor: string | null = null) {
  return { items, nextCursor };
}

function messagePage(
  items: Message[] = [MESSAGE],
  nextCursor: string | null = null,
  snapshotLastSequence = items.at(-1)?.sequence ?? 0,
): MessagePage {
  return {
    items,
    nextCursor,
    snapshotLastSequence,
    firstSequence: items[0]?.sequence ?? null,
    lastSequence: items.at(-1)?.sequence ?? null,
  };
}

function makeProfileClient(overrides: Partial<ProfilesApi> = {}) {
  return {
    getCapabilities: vi.fn(async () => CAPABILITIES),
    listProfiles: vi.fn(async () => PROFILES),
    ...overrides,
  } as Pick<ProfilesApi, "getCapabilities" | "listProfiles">;
}

function makeSessionClient(overrides: Partial<SessionsApi> = {}): SessionsApi {
  const client: SessionsApi = {
    listSessions: vi.fn(async () => page()),
    searchSessions: vi.fn(async () => page([{ ...SESSION, match: {
      field: "message",
      messageId: "message-1",
      snippet: "Rust session store",
      ranges: [{ start: 0, end: 4 }],
    } }])),
    createSession: vi.fn(async (input) => ({
      value: {
        ...SESSION,
        profileId: input.profileId,
        personaId: input.personaId ?? null,
        title: input.title ?? "新会话",
      },
      etag: '"session_rev_1"',
    })),
    getSession: vi.fn(async (sessionId) => ({
      value: { ...SESSION, id: sessionId },
      etag: '"session_rev_1"',
    })),
    updateSession: vi.fn(async (sessionId, patch) => ({
      value: { ...SESSION, id: sessionId, ...patch, revision: "session_rev_2" },
      etag: '"session_rev_2"',
    })),
    deleteSession: vi.fn(async () => undefined),
    listMessages: vi.fn(async () => messagePage()),
  };
  return Object.assign(client, overrides);
}

function makeImportClient(
  overrides: Partial<SessionImportsApi> = {},
): SessionImportsApi {
  const client: SessionImportsApi = {
    previewHermesV21Import: vi.fn(async () => READY_IMPORT_PREVIEW),
    importHermesV21: vi.fn(async () => IMPORT_RESULT),
  };
  return Object.assign(client, overrides);
}

afterEach(() => cleanup());

describe("SessionsWorkspace", () => {
  it("shows a desktop-only state without exposing or requesting a token", async () => {
    const profileClient = makeProfileClient({
      getCapabilities: vi.fn(async () => {
        throw new DesktopConnectionError("desktop_unavailable", "desktop required");
      }),
    });
    const client = makeSessionClient();

    render(<SessionsWorkspace client={client} profileClient={profileClient} />);

    expect(await screen.findByText("请在 SynthChat Desktop 中打开")).toBeTruthy();
    expect(screen.getByText(/浏览器模式不会接收桌面令牌/)).toBeTruthy();
    expect(client.listSessions).not.toHaveBeenCalled();
  });

  it("renders the dense list and complete message history for the active Profile", async () => {
    const onContinue = vi.fn();
    const client = makeSessionClient();
    render(
      <SessionsWorkspace
        client={client}
        onContinue={onContinue}
        profileClient={makeProfileClient()}
      />,
    );

    expect(await screen.findByText("Migration review")).toBeTruthy();
    expect(await screen.findByText("The session store is ready.")).toBeTruthy();
    expect(screen.getByText("report.md")).toBeTruthy();
    expect(screen.getByText("read_file")).toBeTruthy();
    expect(screen.getByText("15 tokens")).toBeTruthy();
    expect(client.listSessions).toHaveBeenCalledWith(
      { profileId: "default", archived: false, limit: 30 },
      expect.objectContaining({ signal: expect.any(AbortSignal) }),
    );

    await userEvent.click(screen.getByRole("button", { name: "继续对话" }));
    expect(onContinue).toHaveBeenCalledWith(expect.objectContaining({ id: "session-1" }));
  });

  it("filters by Profile, literal search, clear action, and archive status", async () => {
    const user = userEvent.setup();
    const client = makeSessionClient({
      listSessions: vi.fn(async (query) => page([{ ...SESSION, profileId: query.profileId, archived: query.archived ?? false }])),
      searchSessions: vi.fn(async (query) => page([{ ...SESSION, profileId: query.profileId, archived: query.archived ?? false, match: {
        field: "title",
        messageId: null,
        snippet: "Migration review",
        ranges: [],
      } }])),
    });
    render(<SessionsWorkspace client={client} profileClient={makeProfileClient()} />);
    await screen.findByText("Migration review");

    await user.selectOptions(screen.getByLabelText("按 Profile 筛选"), "work");
    await waitFor(() => expect(client.listSessions).toHaveBeenCalledWith(
      expect.objectContaining({ profileId: "work" }),
      expect.anything(),
    ));

    await user.type(screen.getByLabelText("搜索会话"), "Rust store");
    await user.keyboard("{Enter}");
    await waitFor(() => expect(client.searchSessions).toHaveBeenCalledWith(
      { profileId: "work", query: "Rust store", archived: false, limit: 30 },
      expect.anything(),
    ));
    expect(screen.getAllByText("Migration review").length).toBeGreaterThan(0);

    await user.click(screen.getByRole("button", { name: "清除搜索" }));
    await waitFor(() => expect(client.listSessions).toHaveBeenCalledWith(
      expect.objectContaining({ profileId: "work", archived: false }),
      expect.anything(),
    ));

    await user.click(screen.getByRole("button", { name: "已归档" }));
    await waitFor(() => expect(client.listSessions).toHaveBeenCalledWith(
      expect.objectContaining({ profileId: "work", archived: true }),
      expect.anything(),
    ));
  });

  it("disables search when unavailable and handles an installation with no Profiles", async () => {
    const client = makeSessionClient();
    render(
      <SessionsWorkspace
        client={client}
        profileClient={makeProfileClient({
          getCapabilities: vi.fn(async () => ({
            ...CAPABILITIES,
            sessionSearch: { mode: "unavailable" as const },
          })),
          listProfiles: vi.fn(async () => []),
        })}
      />,
    );

    const search = await screen.findByLabelText("搜索会话") as HTMLInputElement;
    expect(search.disabled).toBe(true);
    expect(screen.getByText("还没有会话")).toBeTruthy();
    expect((screen.getByRole("button", { name: "创建会话" }) as HTMLButtonElement).disabled).toBe(true);
    expect(client.listSessions).not.toHaveBeenCalled();
  });

  it("stops before Session routes when the Rust-owned database is unavailable", async () => {
    const user = userEvent.setup();
    const client = makeSessionClient();
    const getCapabilities = vi.fn(async () => ({
      ...CAPABILITIES,
      sessionStorage: { ...CAPABILITIES.sessionStorage, available: false, schemaVersion: null },
    }));
    render(
      <SessionsWorkspace
        client={client}
        profileClient={makeProfileClient({ getCapabilities })}
      />,
    );

    expect(await screen.findByText("会话存储不可用")).toBeTruthy();
    expect(client.listSessions).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "重试" }));
    await waitFor(() => expect(getCapabilities).toHaveBeenCalledTimes(2));
  });

  it("does not expose or request Hermes imports while the capability is false", async () => {
    const importClient = makeImportClient();
    render(
      <SessionsWorkspace
        client={makeSessionClient()}
        importClient={importClient}
        profileClient={makeProfileClient()}
      />,
    );

    await screen.findByText("Migration review");
    expect(screen.queryByRole("button", { name: "导入 Hermes 历史" })).toBeNull();
    expect(importClient.previewHermesV21Import).not.toHaveBeenCalled();
    expect(importClient.importHermesV21).not.toHaveBeenCalled();
  });

  it("previews, requires attachment consent, prevents double submit, and refreshes Sessions", async () => {
    const user = userEvent.setup();
    let resolveImport!: (result: HermesImportResult) => void;
    const pendingImport = new Promise<HermesImportResult>((resolve) => {
      resolveImport = resolve;
    });
    const importClient = makeImportClient({
      importHermesV21: vi.fn(async () => pendingImport),
    });
    const client = makeSessionClient();
    render(
      <SessionsWorkspace
        client={client}
        importClient={importClient}
        profileClient={makeProfileClient({
          getCapabilities: vi.fn(async () => IMPORT_CAPABILITIES),
        })}
      />,
    );
    await screen.findByText("Migration review");

    await user.click(screen.getByRole("button", { name: "导入 Hermes 历史" }));
    const confirm = await screen.findByRole("button", { name: "确认导入" });
    expect(importClient.previewHermesV21Import).toHaveBeenCalledWith(
      "default",
      expect.objectContaining({ signal: expect.any(AbortSignal) }),
    );
    expect((confirm as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByText("4").closest("div")?.textContent).toContain("历史消息");

    await user.click(screen.getByRole("checkbox", { name: /忽略 1 个附件引用并继续/ }));
    expect((confirm as HTMLButtonElement).disabled).toBe(false);
    fireEvent.click(confirm);
    fireEvent.click(confirm);

    expect(await screen.findByText("正在导入，完成前请保持应用运行")).toBeTruthy();
    expect((screen.getByLabelText("按 Profile 筛选") as HTMLSelectElement).disabled).toBe(true);
    expect(importClient.importHermesV21).toHaveBeenCalledTimes(1);
    expect(importClient.importHermesV21).toHaveBeenCalledWith(
      "default",
      {
        expectedSnapshotFingerprint: IMPORT_FINGERPRINT_A,
        allowAttachmentOmission: true,
      },
      expect.stringMatching(/^.{8,128}$/u),
      expect.objectContaining({ signal: expect.any(AbortSignal) }),
    );

    await act(async () => resolveImport(IMPORT_RESULT));
    expect(await screen.findByText("导入完成")).toBeTruthy();
    expect(screen.getByText("attachment_omitted")).toBeTruthy();
    await waitFor(() => expect(client.listSessions).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(client.getSession).toHaveBeenCalledTimes(2));
  });

  it("isolates delayed previews across rechecks and Profile switches", async () => {
    const user = userEvent.setup();
    let resolveFirst!: (preview: HermesImportPreview) => void;
    let firstSignal: AbortSignal | undefined;
    let workSignal: AbortSignal | undefined;
    const firstPreview = new Promise<HermesImportPreview>((resolve) => {
      resolveFirst = resolve;
    });
    const previewHermesV21Import = vi.fn(async (profileId: string, options = {}) => {
      const signal = (options as { signal?: AbortSignal }).signal;
      if (previewHermesV21Import.mock.calls.length === 1) {
        firstSignal = signal;
        return firstPreview;
      }
      if (profileId === "work") workSignal = signal;
      return ABSENT_IMPORT_PREVIEW;
    });
    const importClient = makeImportClient({ previewHermesV21Import });
    render(
      <SessionsWorkspace
        client={makeSessionClient()}
        importClient={importClient}
        profileClient={makeProfileClient({
          getCapabilities: vi.fn(async () => IMPORT_CAPABILITIES),
        })}
      />,
    );
    await screen.findByText("Migration review");

    const importButton = screen.getByRole("button", { name: "导入 Hermes 历史" });
    await user.click(importButton);
    expect(await screen.findByText("正在检查可导入历史")).toBeTruthy();
    await user.click(importButton);
    expect(await screen.findByText("未发现可导入的 Hermes 历史。")).toBeTruthy();
    expect(firstSignal?.aborted).toBe(true);

    await act(async () => resolveFirst(READY_IMPORT_PREVIEW));
    expect(screen.queryByRole("button", { name: "确认导入" })).toBeNull();
    expect(screen.getByText("未发现可导入的 Hermes 历史。")).toBeTruthy();

    await user.selectOptions(screen.getByLabelText("按 Profile 筛选"), "work");
    expect(screen.queryByLabelText("Hermes 历史导入")).toBeNull();
    await user.click(screen.getByRole("button", { name: "导入 Hermes 历史" }));
    expect(await screen.findByText("未发现可导入的 Hermes 历史。")).toBeTruthy();
    expect(workSignal?.aborted).toBe(false);
  });

  it("reuses the idempotency key for a network retry", async () => {
    const user = userEvent.setup();
    const preview = { ...READY_IMPORT_PREVIEW, attachmentCount: 0 };
    const importHermesV21 = vi.fn()
      .mockRejectedValueOnce(new DesktopConnectionError("network", "offline"))
      .mockResolvedValue({ ...IMPORT_RESULT, omittedAttachmentCount: 0 });
    const importClient = makeImportClient({
      previewHermesV21Import: vi.fn(async () => preview),
      importHermesV21,
    });
    render(
      <SessionsWorkspace
        client={makeSessionClient()}
        importClient={importClient}
        profileClient={makeProfileClient({
          getCapabilities: vi.fn(async () => IMPORT_CAPABILITIES),
        })}
      />,
    );
    await screen.findByText("Migration review");

    await user.click(screen.getByRole("button", { name: "导入 Hermes 历史" }));
    await user.click(await screen.findByRole("button", { name: "确认导入" }));
    expect(await screen.findByText("本地 Rust 后端无法连接。")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "重试导入" }));

    expect(await screen.findByText("导入完成")).toBeTruthy();
    expect(importHermesV21).toHaveBeenCalledTimes(2);
    expect(importHermesV21.mock.calls[0]![2]).toBe(importHermesV21.mock.calls[1]![2]);
  });

  it("requires a new preview and idempotency key after the source changes", async () => {
    const user = userEvent.setup();
    const firstPreview = { ...READY_IMPORT_PREVIEW, attachmentCount: 0 };
    const secondPreview = {
      ...firstPreview,
      snapshotFingerprint: IMPORT_FINGERPRINT_B,
      sessionCount: 3,
    };
    const previewHermesV21Import = vi.fn()
      .mockResolvedValueOnce(firstPreview)
      .mockResolvedValueOnce(secondPreview);
    const importHermesV21 = vi.fn()
      .mockRejectedValueOnce(new SessionImportApiError("http", "Source changed", {
        status: 409,
        code: "hermes_import_source_changed",
        requestId: "req-source",
      }))
      .mockResolvedValueOnce({
        ...IMPORT_RESULT,
        snapshotFingerprint: IMPORT_FINGERPRINT_B,
        importedSessionCount: 3,
      });
    render(
      <SessionsWorkspace
        client={makeSessionClient()}
        importClient={makeImportClient({ previewHermesV21Import, importHermesV21 })}
        profileClient={makeProfileClient({
          getCapabilities: vi.fn(async () => IMPORT_CAPABILITIES),
        })}
      />,
    );
    await screen.findByText("Migration review");

    await user.click(screen.getByRole("button", { name: "导入 Hermes 历史" }));
    await user.click(await screen.findByRole("button", { name: "确认导入" }));
    expect(await screen.findByText(/Hermes 历史在确认后发生了变化/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "重试导入" })).toBeNull();
    await user.click(screen.getByRole("button", { name: "重新检查" }));
    const importPanel = await screen.findByLabelText("Hermes 历史导入");
    expect(within(importPanel).getByText("会话").closest("div")?.textContent).toContain("3");
    await user.click(screen.getByRole("button", { name: "确认导入" }));

    expect(await screen.findByText("导入完成")).toBeTruthy();
    expect(importHermesV21).toHaveBeenCalledTimes(2);
    expect(importHermesV21.mock.calls[0]![2]).not.toBe(importHermesV21.mock.calls[1]![2]);
    expect(importHermesV21.mock.calls[1]![1]).toEqual({
      expectedSnapshotFingerprint: IMPORT_FINGERPRINT_B,
      allowAttachmentOmission: false,
    });
  });

  it("reports bounded conflicts and does not refresh after an atomic rollback", async () => {
    const user = userEvent.setup();
    const client = makeSessionClient();
    const conflict = new SessionImportApiError("http", "Hermes import conflict", {
      status: 409,
      code: "hermes_import_conflict",
      requestId: "req-conflict",
      conflicts: [{
        code: "targetModified",
        sourceKeyDigest: "c".repeat(64),
        targetSessionId: "session_hv21_1",
      }],
      conflictCount: 2,
      conflictsDropped: 1,
    });
    const importClient = makeImportClient({
      previewHermesV21Import: vi.fn(async () => ({
        ...READY_IMPORT_PREVIEW,
        attachmentCount: 0,
      })),
      importHermesV21: vi.fn(async () => {
        throw conflict;
      }),
    });
    render(
      <SessionsWorkspace
        client={client}
        importClient={importClient}
        profileClient={makeProfileClient({
          getCapabilities: vi.fn(async () => IMPORT_CAPABILITIES),
        })}
      />,
    );
    await screen.findByText("Migration review");

    await user.click(screen.getByRole("button", { name: "导入 Hermes 历史" }));
    await user.click(await screen.findByRole("button", { name: "确认导入" }));

    expect(await screen.findByText("存在 2 个冲突，整批未写入")).toBeTruthy();
    expect(screen.getByText("本地会话已修改")).toBeTruthy();
    expect(screen.getByText("session_hv21_1")).toBeTruthy();
    expect(screen.getByText("另有 1 个冲突未返回。")).toBeTruthy();
    expect(client.listSessions).toHaveBeenCalledTimes(1);
    expect(client.getSession).toHaveBeenCalledTimes(1);
  });

  it("creates with a stable idempotency key after a retry", async () => {
    const user = userEvent.setup();
    const client = makeSessionClient();
    vi.mocked(client.createSession)
      .mockRejectedValueOnce(new SessionApiError("http", "Storage busy", {
        status: 503,
        code: "session_storage_busy",
        retryable: true,
      }));
    render(<SessionsWorkspace client={client} profileClient={makeProfileClient()} />);
    await screen.findByText("Migration review");

    await user.click(screen.getByRole("button", { name: "创建会话" }));
    const form = screen.getByRole("button", { name: "创建" }).closest("form");
    expect(form).not.toBeNull();
    await user.type(within(form!).getByLabelText("标题（可选）"), "FTS planning");
    await user.click(within(form!).getByRole("button", { name: "创建" }));
    expect(await screen.findByText("会话库正忙，请稍后重试。")).toBeTruthy();
    await user.click(within(form!).getByRole("button", { name: "创建" }));

    await waitFor(() => expect(client.createSession).toHaveBeenCalledTimes(2));
    expect(client.createSession).toHaveBeenLastCalledWith(
      { profileId: "default", title: "FTS planning" },
      expect.stringMatching(/^.{8,128}$/u),
    );
    expect(vi.mocked(client.createSession).mock.calls[0]![1]).toBe(
      vi.mocked(client.createSession).mock.calls[1]![1],
    );
  });

  it("lets a form hold 500 astral Unicode scalar values without native truncation", async () => {
    const user = userEvent.setup();
    render(<SessionsWorkspace client={makeSessionClient()} profileClient={makeProfileClient()} />);
    await screen.findByText("Migration review");
    await user.click(screen.getByRole("button", { name: "创建会话" }));
    const title = screen.getByLabelText("标题（可选）") as HTMLInputElement;
    const boundaryTitle = "\u{1f642}".repeat(500);

    fireEvent.change(title, { target: { value: boundaryTitle } });

    expect(title.value).toBe(boundaryTitle);
    expect(Array.from(title.value)).toHaveLength(500);
    expect(title.hasAttribute("maxlength")).toBe(false);
  });

  it("updates title and archives with the latest cached ETag", async () => {
    const user = userEvent.setup();
    const client = makeSessionClient();
    render(<SessionsWorkspace client={client} profileClient={makeProfileClient()} />);
    await screen.findByText("The session store is ready.");

    const title = screen.getByLabelText("会话标题");
    await user.clear(title);
    await user.type(title, "Renamed session");
    await user.click(screen.getByRole("button", { name: "保存会话标题" }));
    await waitFor(() => expect(client.updateSession).toHaveBeenCalledWith(
      "session-1",
      { title: "Renamed session" },
      '"session_rev_1"',
    ));

    await user.click(screen.getByRole("button", { name: "归档" }));
    await waitFor(() => expect(client.updateSession).toHaveBeenLastCalledWith(
      "session-1",
      { archived: true },
      '"session_rev_2"',
    ));
  });

  it("restores an archived Session and disables continuation until restored", async () => {
    const user = userEvent.setup();
    const archivedSession = { ...SESSION, archived: true };
    const client = makeSessionClient({
      listSessions: vi.fn(async () => page([archivedSession])),
      getSession: vi.fn(async () => ({ value: archivedSession, etag: '"session_rev_1"' })),
    });
    render(<SessionsWorkspace client={client} profileClient={makeProfileClient()} />);
    await user.click(await screen.findByRole("button", { name: "已归档" }));
    const restore = await screen.findByRole("button", { name: "恢复" });

    expect((screen.getByRole("button", { name: "继续对话" }) as HTMLButtonElement).disabled).toBe(true);
    await user.click(restore);
    await waitFor(() => expect(client.updateSession).toHaveBeenCalledWith(
      "session-1",
      { archived: false },
      '"session_rev_1"',
    ));
  });

  it("requires explicit confirmation and sends a conditional DELETE", async () => {
    const user = userEvent.setup();
    const client = makeSessionClient();
    render(<SessionsWorkspace client={client} profileClient={makeProfileClient()} />);
    await screen.findByText("The session store is ready.");

    await user.click(screen.getByRole("button", { name: "删除会话" }));
    expect(client.deleteSession).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "确认删除" }));

    await waitFor(() => expect(client.deleteSession).toHaveBeenCalledWith(
      "session-1",
      '"session_rev_1"',
    ));
  });

  it("appends cursor pages for Sessions and Messages without duplicates", async () => {
    const user = userEvent.setup();
    const secondSession = { ...SESSION, id: "session-2", title: "Second session" };
    const messages = Array.from({ length: 100 }, (_, index): Message => ({
      ...MESSAGE,
      id: `message-${index + 1}`,
      sequence: index + 1,
      parts: [{ type: "text", text: `Message ${index + 1}` }],
      reasoning: null,
      toolCalls: [],
      usage: null,
    }));
    const client = makeSessionClient({
      listSessions: vi.fn(async (query) => query.cursor
        ? page([SESSION, secondSession])
        : page([SESSION], "next-session")),
      listMessages: vi.fn(async (_sessionId, query) => query?.cursor
        ? messagePage(messages.slice(0, 50), null, 100)
        : messagePage(messages.slice(50), "next-message", 100)),
    });
    render(<SessionsWorkspace client={client} profileClient={makeProfileClient()} />);
    await screen.findByText("Message 51");

    await user.click(screen.getByRole("button", { name: "加载更多会话" }));
    expect(await screen.findByText("Second session")).toBeTruthy();
    expect(screen.getAllByText("Migration review")).toHaveLength(1);

    await user.click(screen.getByRole("button", { name: "加载更多消息" }));
    expect(await screen.findByText("Message 1")).toBeTruthy();
    const renderedMessages = screen.getAllByText(/^Message \d+$/u);
    expect(renderedMessages).toHaveLength(100);
    expect(renderedMessages[0]?.textContent).toBe("Message 1");
    expect(renderedMessages.at(-1)?.textContent).toBe("Message 100");
  });

  it("discards a delayed cursor page after the Profile filter changes", async () => {
    const user = userEvent.setup();
    let resolveOldPage!: (value: SessionPage) => void;
    const oldPage = new Promise<SessionPage>((resolve) => {
      resolveOldPage = resolve;
    });
    const workSession = { ...SESSION, id: "work-session", profileId: "work", title: "Work session" };
    const staleSession = { ...SESSION, id: "stale-session", title: "Stale default page" };
    const client = makeSessionClient({
      listSessions: vi.fn(async (query) => {
        if (query.cursor) return oldPage;
        if (query.profileId === "work") return page([workSession]);
        return page([SESSION], "old-cursor");
      }),
    });
    render(<SessionsWorkspace client={client} profileClient={makeProfileClient()} />);
    await screen.findByText("Migration review");

    await user.click(screen.getByRole("button", { name: "加载更多会话" }));
    await waitFor(() => expect(client.listSessions).toHaveBeenCalledWith(
      expect.objectContaining({ profileId: "default", cursor: "old-cursor" }),
    ));
    await user.selectOptions(screen.getByLabelText("按 Profile 筛选"), "work");
    expect(await screen.findByText("Work session")).toBeTruthy();

    await act(async () => resolveOldPage(page([staleSession])));

    expect(screen.queryByText("Stale default page")).toBeNull();
    expect(screen.getByText("Work session")).toBeTruthy();
  });

  it("rejects a changed message snapshot and offers a detail reload", async () => {
    const user = userEvent.setup();
    const client = makeSessionClient({
      listMessages: vi.fn(async (_sessionId, query) => query?.cursor
        ? messagePage([{ ...MESSAGE, id: "message-1", sequence: 1 }], null, 101)
        : messagePage([{ ...MESSAGE, id: "message-51", sequence: 51 }], "next-message", 100)),
    });
    render(<SessionsWorkspace client={client} profileClient={makeProfileClient()} />);
    await screen.findByText("The session store is ready.");

    await user.click(screen.getByRole("button", { name: "加载更多消息" }));
    expect(await screen.findByText("消息分页快照已变化，请重新加载会话。")).toBeTruthy();
  });

  it("surfaces list, search, detail, paging, and conflict failures with retry paths", async () => {
    const user = userEvent.setup();
    const listSessions = vi.fn()
      .mockRejectedValueOnce(new SessionApiError("http", "Database unavailable", {
        status: 503,
        code: "session_storage_unavailable",
        requestId: "req-list",
      }))
      .mockResolvedValue(page([SESSION], "next-session"));
    const getSession = vi.fn()
      .mockRejectedValueOnce(new Error("read failed"))
      .mockResolvedValue({ value: SESSION, etag: '"session_rev_1"' });
    const client = makeSessionClient({ listSessions, getSession });
    render(<SessionsWorkspace client={client} profileClient={makeProfileClient()} />);

    expect(await screen.findByText(/Database unavailable.*req-list/)).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "重试" }));
    expect(await screen.findByText("会话加载失败")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "重新加载" }));
    expect(await screen.findByText("The session store is ready.")).toBeTruthy();

    vi.mocked(client.listSessions).mockRejectedValueOnce(new Error("next failed"));
    await user.click(screen.getByRole("button", { name: "加载更多会话" }));
    expect(await screen.findByText("无法加载更多会话。")).toBeTruthy();

    vi.mocked(client.updateSession).mockRejectedValueOnce(new SessionApiError(
      "http",
      "Revision conflict",
      { status: 409, code: "revision_conflict" },
    ));
    await user.click(screen.getByRole("button", { name: "归档" }));
    expect(await screen.findByText("Revision conflict")).toBeTruthy();
    await waitFor(() => expect(getSession).toHaveBeenCalledTimes(3));
  });

  it("retries bootstrap failures and maps a network disconnect", async () => {
    const user = userEvent.setup();
    const getCapabilities = vi.fn()
      .mockRejectedValueOnce(new DesktopConnectionError("network", "offline"))
      .mockResolvedValue(CAPABILITIES);
    render(
      <SessionsWorkspace
        client={makeSessionClient()}
        profileClient={makeProfileClient({ getCapabilities })}
      />,
    );

    expect(await screen.findByText("本地 Rust 后端无法连接。")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "重试" }));
    expect(await screen.findByText("Migration review")).toBeTruthy();
  });
});

describe("SessionMessageTimeline", () => {
  it("renders an empty state and all non-assistant role labels", () => {
    const { rerender } = render(<SessionMessageTimeline messages={[]} />);
    expect(screen.getByText("尚无消息")).toBeTruthy();

    rerender(<SessionMessageTimeline messages={[
      { ...MESSAGE, id: "user", role: "user", reasoning: null, toolCalls: [], usage: null },
      { ...MESSAGE, id: "system", role: "system", sequence: 2, parts: [], reasoning: null, toolCalls: [], usage: null },
      { ...MESSAGE, id: "tool", role: "tool", sequence: 3, parts: [], reasoning: null, toolCalls: [{ callId: "failed", name: "shell", status: "failed" }], usage: null },
      { ...MESSAGE, id: "running", role: "tool", sequence: 4, parts: [], reasoning: null, toolCalls: [{ callId: "running", name: "browser", status: "running" }], usage: null },
      { ...MESSAGE, id: "cancelled", role: "tool", sequence: 5, parts: [], reasoning: null, toolCalls: [{ callId: "cancelled", name: "file", status: "cancelled" }], usage: null },
    ]} />);

    expect(screen.getByText("你")).toBeTruthy();
    expect(screen.getByText("系统")).toBeTruthy();
    expect(screen.getAllByText("工具")).toHaveLength(3);
    expect(screen.getByText("失败")).toBeTruthy();
    expect(screen.getByText("运行中")).toBeTruthy();
    expect(screen.getByText("已取消")).toBeTruthy();
  });
});
