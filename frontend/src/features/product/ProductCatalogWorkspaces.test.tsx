// @vitest-environment jsdom

import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type {
  Moment,
  Persona,
  ProductCatalogApi,
  Worldbook,
} from "../../api/productCatalog";
import type { Capabilities, ProfileSummary, ProfilesApi } from "../../api/profiles";
import type { Session, SessionsApi } from "../../api/sessions";
import {
  ContactsWorkspace,
  MomentsWorkspace,
  PersonasWorkspace,
  WorldbooksWorkspace,
} from "./ProductCatalogWorkspaces";

const PROFILE: ProfileSummary = {
  id: "default",
  displayName: "Default",
  isDefault: true,
  isActive: true,
  color: null,
  avatarFileId: null,
  engineState: "running",
  configRevision: "profile-1",
  createdAt: null,
  updatedAt: "2026-07-20T08:00:00Z",
};

const CONTACT_SESSION: Session = {
  id: "session_1",
  profileId: "default",
  personaId: "persona_11111111111111111111111111111111",
  title: "与 小可 对话",
  preview: "",
  source: "synthchat",
  model: "",
  messageCount: 0,
  archived: false,
  revision: "session_rev_1",
  createdAt: "2026-07-20T08:00:00Z",
  updatedAt: "2026-07-20T08:00:00Z",
  match: null,
};

const PERSONA: Persona = {
  id: "persona_11111111111111111111111111111111",
  name: "小可",
  avatar: null,
  systemPrompt: "保持角色一致",
  characterPrompt: "温柔可靠",
  outputExamples: "你好",
  systemInstructions: "使用中文",
  provider: "openai-api",
  model: "gpt-test",
  temperature: 0.8,
  maxTokens: 2048,
  toolsEnabled: true,
  memoryEnabled: true,
  proactiveEnabled: false,
  legacyAgentId: null,
  createdAt: "2026-07-20T08:00:00Z",
  updatedAt: "2026-07-20T08:00:00Z",
  revision: 1,
};

const MOMENT: Moment = {
  id: "moment_11111111111111111111111111111111",
  authorId: "user",
  body: "今天很好",
  coverFileId: null,
  likedBy: [],
  comments: [],
  createdAt: "2026-07-20T08:00:00Z",
  updatedAt: "2026-07-20T08:00:00Z",
  revision: 1,
};

const WORLDBOOK: Worldbook = {
  id: "worldbook_11111111111111111111111111111111",
  name: "城市设定",
  description: "海边城市",
  boundPersonaIds: [PERSONA.id],
  sections: [{ id: "section_1", key: "地点", content: "旧港", enabled: true }],
  createdAt: "2026-07-20T08:00:00Z",
  updatedAt: "2026-07-20T08:00:00Z",
  revision: 1,
};

type ProfileClient = Pick<ProfilesApi, "getCapabilities" | "listProfiles">;

function profileClient(): ProfileClient {
  return {
    getCapabilities: vi.fn(async () => ({
      extensions: { personas: true, moments: true, worldbooks: true },
    } as unknown as Capabilities)),
    listProfiles: vi.fn(async () => [PROFILE]),
  };
}

function productClient(overrides: Partial<ProductCatalogApi> = {}): ProductCatalogApi {
  return {
    listPersonas: vi.fn(async () => [PERSONA]),
    createPersona: vi.fn(async () => ({ value: { ...PERSONA, id: "persona_22222222222222222222222222222222" }, etag: '"product-persona-1"' })),
    getPersona: vi.fn(async () => ({ value: PERSONA, etag: '"product-persona-1"' })),
    updatePersona: vi.fn(async () => ({ value: { ...PERSONA, revision: 2 }, etag: '"product-persona-2"' })),
    deletePersona: vi.fn(async () => undefined),
    listWorldbooks: vi.fn(async () => []),
    createWorldbook: vi.fn(async () => ({ value: WORLDBOOK, etag: '"product-worldbook-1"' })),
    getWorldbook: vi.fn(async () => ({ value: WORLDBOOK, etag: '"product-worldbook-1"' })),
    updateWorldbook: vi.fn(async () => ({ value: { ...WORLDBOOK, revision: 2 }, etag: '"product-worldbook-2"' })),
    deleteWorldbook: vi.fn(async () => undefined),
    listMoments: vi.fn(async () => [MOMENT]),
    createMoment: vi.fn(async (_profileId, input) => ({ value: { ...MOMENT, id: "moment_22222222222222222222222222222222", body: input.body }, etag: '"product-moment-1"' })),
    getMoment: vi.fn(async () => ({ value: MOMENT, etag: '"product-moment-1"' })),
    updateMoment: vi.fn(async () => ({ value: { ...MOMENT, revision: 2 }, etag: '"product-moment-2"' })),
    deleteMoment: vi.fn(async () => undefined),
    addMomentComment: vi.fn(async (_profileId, _momentId, input) => ({ value: { ...MOMENT, comments: [{ id: "comment_1", authorId: input.authorId ?? "user", text: input.text, replyTo: null, createdAt: MOMENT.createdAt, updatedAt: MOMENT.updatedAt }], revision: 2 }, etag: '"product-moment-2"' })),
    deleteMomentComment: vi.fn(async () => ({ value: { ...MOMENT, revision: 2 }, etag: '"product-moment-2"' })),
    setMomentLike: vi.fn(async () => ({ value: { ...MOMENT, likedBy: ["user"], revision: 2 }, etag: '"product-moment-2"' })),
    ...overrides,
  };
}

afterEach(() => cleanup());

describe("Rust product catalog workspaces", () => {
  it("opens a real Session from a selected Contact", async () => {
    const createSession = vi.fn(async () => ({
      value: CONTACT_SESSION,
      etag: '"session-1"',
    }));
    const onNavigate = vi.fn();
    const onOpenSession = vi.fn();
    const user = userEvent.setup();
    render(<ContactsWorkspace
      client={productClient()}
      onNavigate={onNavigate}
      onOpenSession={onOpenSession}
      profileClient={profileClient()}
      sessionClient={{ createSession } as Pick<SessionsApi, "createSession">}
    />);

    await user.click(await screen.findByRole("button", { name: "发消息" }));

    expect(createSession).toHaveBeenCalledWith({
      profileId: "default",
      personaId: PERSONA.id,
      title: "与 小可 对话",
    }, expect.stringMatching(/^persona-session-/u));
    expect(onOpenSession).toHaveBeenCalledWith({
      id: "session_1",
      title: "与 小可 对话",
      personaId: PERSONA.id,
    });
    expect(onNavigate).toHaveBeenCalledWith("chat");
  });

  it("creates a Persona through the Rust API", async () => {
    const createPersona = vi.fn(async (_profileId, input) => ({
      value: { ...PERSONA, id: "persona_22222222222222222222222222222222", name: input.name },
      etag: '"product-persona-1"',
    }));
    const user = userEvent.setup();
    render(<PersonasWorkspace client={productClient({ createPersona })} profileClient={profileClient()} />);

    await user.click(await screen.findByRole("button", { name: "新建角色" }));
    await user.type(screen.getByRole("textbox", { name: "角色名称" }), "新角色");
    await user.click(screen.getByRole("button", { name: "保存角色" }));

    await waitFor(() => expect(createPersona).toHaveBeenCalledWith("default", expect.objectContaining({ name: "新角色" })));
    expect(await screen.findByText("角色已创建。")).toBeTruthy();
  });

  it("publishes, likes, and comments on Moments", async () => {
    const client = productClient();
    const user = userEvent.setup();
    render(<MomentsWorkspace client={client} profileClient={profileClient()} />);

    const composer = await screen.findByRole("textbox", { name: "朋友圈正文" });
    await user.type(composer, "新的动态");
    await user.click(screen.getByRole("button", { name: "发布" }));
    await waitFor(() => expect(client.createMoment).toHaveBeenCalledWith("default", { body: "新的动态", authorId: "user" }));

    await user.click(screen.getByRole("button", { name: "点赞 今天很好" }));
    await waitFor(() => expect(client.setMomentLike).toHaveBeenCalled());

    const comment = screen.getByRole("textbox", { name: "评论 今天很好" });
    await user.type(comment, "真好");
    await user.click(screen.getByRole("button", { name: "发送评论 今天很好" }));
    await waitFor(() => expect(client.addMomentComment).toHaveBeenCalledWith(
      "default",
      MOMENT.id,
      { authorId: "user", text: "真好" },
      '"product-moment-2"',
    ));
  });

  it("creates a Worldbook with a Persona binding and section", async () => {
    const createWorldbook = vi.fn(async () => ({ value: WORLDBOOK, etag: '"product-worldbook-1"' }));
    const user = userEvent.setup();
    render(<WorldbooksWorkspace client={productClient({ createWorldbook })} profileClient={profileClient()} />);

    await user.type(await screen.findByRole("textbox", { name: "世界书名称" }), "城市设定");
    await user.type(screen.getByRole("textbox", { name: "世界书说明" }), "海边城市");
    await user.type(screen.getByRole("textbox", { name: "世界书关键词" }), "地点");
    await user.type(screen.getByRole("textbox", { name: "世界书条目内容" }), "旧港");
    await user.click(screen.getByRole("checkbox", { name: "小可" }));
    await user.click(screen.getByRole("button", { name: "新建世界书" }));

    await waitFor(() => expect(createWorldbook).toHaveBeenCalledWith("default", {
      name: "城市设定",
      description: "海边城市",
      boundPersonaIds: [PERSONA.id],
      sections: [{ key: "地点", content: "旧港", enabled: true }],
    }));
  });
});
