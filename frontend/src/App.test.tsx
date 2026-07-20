// @vitest-environment jsdom

import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("./features/sessions/SessionsWorkspace", () => ({
  SessionsWorkspace: ({ onContinue }: {
    onContinue?: (session: { id: string; title: string }) => void;
  }) => (
    <button
      onClick={() => onContinue?.({ id: "session-test", title: "Test session" })}
      type="button"
    >
      模拟继续对话
    </button>
  ),
}));

vi.mock("./features/profiles/ProfilesWorkspace", () => ({
  ProfilesWorkspace: () => <div>Profile workspace mock</div>,
}));

vi.mock("./features/memory/MemoryWorkspace", () => ({
  MemoryWorkspace: () => <div>Memory workspace mock</div>,
}));

vi.mock("./features/tools/ToolsWorkspace", () => ({
  ToolsWorkspace: () => <div>Tools workspace mock</div>,
}));

import { App, PHASE_TWO_SECTIONS, PhaseTwoWorkspace } from "./App";

afterEach(() => cleanup());

describe("phase two frontend shell", () => {
  it("restores every non-Agent product destination and backend health state", () => {
    const markup = renderToStaticMarkup(<App />);

    for (const section of PHASE_TWO_SECTIONS) {
      expect(markup).toContain(section.label);
    }
    expect(markup).toContain("aria-label=\"主导航\"");
    expect(markup).toContain("后端检测中");
    expect(markup).toContain("LOCAL RUST");
    expect(PHASE_TWO_SECTIONS.map((section) => section.id)).toEqual([
      "chat",
      "sessions",
      "contacts",
      "discover",
      "personas",
      "moments",
      "memory",
      "worldbooks",
      "plugins",
      "tools",
      "skills",
      "settings",
    ]);
    expect(markup).not.toContain("智能体");
  });

  it("keeps the generic capability fallback explicit", () => {
    const section = PHASE_TWO_SECTIONS.find((item) => item.id === "plugins")!;
    const markup = renderToStaticMarkup(<PhaseTwoWorkspace section={section} />);

    expect(markup).toContain(section.unavailableTitle);
    expect(markup).toContain("未启用");
    expect(markup).toContain("接口版本");
    expect(markup).toContain("v1");
  });

  it("routes existing Rust workspaces and a selected Session into chat", async () => {
    const user = userEvent.setup();
    render(<App />);

    await user.click(screen.getByRole("button", { name: "会话" }));
    await user.click(screen.getByRole("button", { name: "模拟继续对话" }));

    expect(screen.getByRole("button", { name: "聊天" }).getAttribute("aria-current")).toBe("page");
    expect(await screen.findByText(/Test session.*session-test/u)).toBeTruthy();

    await user.click(screen.getByRole("button", { name: "记忆" }));
    expect(await screen.findByText("Memory workspace mock")).toBeTruthy();

    await user.click(screen.getByRole("button", { name: "工具 / MCP" }));
    expect(await screen.findByText("Tools workspace mock")).toBeTruthy();

    await user.click(screen.getByRole("button", { name: "技能" }));
    expect(await screen.findByText("Tools workspace mock")).toBeTruthy();

    await user.click(screen.getByRole("button", { name: "设置" }));
    expect(await screen.findByText("Profile workspace mock")).toBeTruthy();
    expect(screen.getByRole("button", { name: /Profile 与密钥/u }).getAttribute("aria-current")).toBe("page");
  });

  it("renders restored product shells without mock-success actions", async () => {
    const user = userEvent.setup();
    render(<App />);

    await user.click(screen.getByRole("button", { name: "通讯录" }));
    const contacts = await screen.findByRole("region", { name: "通讯录产品面板" });
    expect(within(contacts).getByText("角色目录未启用")).toBeTruthy();
    expect((within(contacts).getByRole("button", { name: "导入角色" }) as HTMLButtonElement).disabled).toBe(true);

    await user.click(screen.getByRole("button", { name: "角色" }));
    const personas = await screen.findByRole("region", { name: "角色产品面板" });
    expect(within(personas).getByRole("tab", { name: "角色设定" })).toBeTruthy();
    expect((within(personas).getByRole("button", { name: "保存角色" }) as HTMLButtonElement).disabled).toBe(true);

    await user.click(screen.getByRole("button", { name: "朋友圈" }));
    expect(await screen.findByText("朋友圈数据服务未启用")).toBeTruthy();
    expect((screen.getByRole("button", { name: "发布" }) as HTMLButtonElement).disabled).toBe(true);

    await user.click(screen.getByRole("button", { name: "世界书" }));
    expect(await screen.findByText("世界书目录未启用")).toBeTruthy();
    expect((screen.getByRole("button", { name: "新建世界书" }) as HTMLButtonElement).disabled).toBe(true);

    await user.click(screen.getByRole("button", { name: "插件" }));
    expect(await screen.findByText("插件目录未启用")).toBeTruthy();
    expect((screen.getByRole("button", { name: "安装插件" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("keeps Discover navigation functional and settings categories capability-aware", async () => {
    const user = userEvent.setup();
    render(<App />);

    await user.click(screen.getByRole("button", { name: "发现" }));
    const discover = await screen.findByRole("region", { name: "发现产品面板" });
    await user.click(within(discover).getByRole("button", { name: /世界书/u }));
    expect(screen.getByRole("button", { name: "世界书" }).getAttribute("aria-current")).toBe("page");

    await user.click(screen.getByRole("button", { name: "设置" }));
    await user.click(screen.getByRole("button", { name: /模型服务/u }));
    expect(screen.getByRole("heading", { name: "模型服务" })).toBeTruthy();
    expect(screen.getByText("模型服务未启用")).toBeTruthy();
  });
});
