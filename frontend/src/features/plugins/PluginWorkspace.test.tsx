// @vitest-environment jsdom

import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Plugin, PluginsApi } from "../../api/plugins";
import { PluginApiError } from "../../api/plugins";
import { PluginWorkspace } from "./PluginWorkspace";

const NOW = "2026-07-20T08:00:00Z";
const PLUGIN: Plugin = {
  id: "local-tools",
  name: "Local tools",
  version: "1.2.0",
  description: "Manifest-only local tools.",
  author: "SynthChat",
  providedTools: ["local.search"],
  requiresEnv: ["LOCAL_PLUGIN_TOKEN"],
  enabled: false,
  execution: "manifestOnly",
  installedAt: NOW,
  updatedAt: NOW,
};

function client(overrides: Partial<PluginsApi> = {}): PluginsApi {
  return {
    listPlugins: vi.fn(async () => ({ value: { items: [PLUGIN] }, etag: '"plugin-catalog-1"' })),
    installPlugin: vi.fn(async () => ({ value: PLUGIN, etag: '"plugin-catalog-1"' })),
    updatePlugin: vi.fn(async (_id, patch) => ({
      value: { ...PLUGIN, enabled: patch.enabled },
      etag: '"plugin-catalog-2"',
    })),
    uninstallPlugin: vi.fn(async () => ({ etag: '"plugin-catalog-2"' })),
    ...overrides,
  };
}

afterEach(() => cleanup());

describe("PluginWorkspace", () => {
  it("loads, filters, and toggles a manifest-only plugin with the catalog ETag", async () => {
    const api = client();
    const user = userEvent.setup();
    render(<PluginWorkspace client={api} />);

    expect(await screen.findByText("Local tools")).toBeTruthy();
    expect(screen.getByText("1 个插件")).toBeTruthy();
    await user.click(screen.getByRole("switch", { name: "启用插件 Local tools" }));

    await waitFor(() => expect(api.updatePlugin).toHaveBeenCalledWith(
      "local-tools",
      { enabled: true },
      '"plugin-catalog-1"',
    ));
    expect(await screen.findByText("Local tools 已启用。")).toBeTruthy();
    expect((screen.getByRole("switch", { name: "停用插件 Local tools" }) as HTMLInputElement).checked)
      .toBe(true);

    await user.type(screen.getByRole("searchbox", { name: "搜索插件" }), "missing");
    expect(screen.getByText("没有匹配的插件")).toBeTruthy();
  });

  it("registers a local manifest directory and keeps it disabled", async () => {
    const installPlugin = vi.fn(async () => ({ value: PLUGIN, etag: '"plugin-catalog-1"' }));
    const api = client({
      listPlugins: vi.fn(async () => ({ value: { items: [] }, etag: '"plugin-catalog-0"' })),
      installPlugin,
    });
    const user = userEvent.setup();
    render(<PluginWorkspace client={api} />);

    expect(await screen.findByText("暂无已登记插件")).toBeTruthy();
    await user.type(screen.getByRole("textbox", { name: "本地插件目录" }), "local-tools");
    await user.click(screen.getByRole("button", { name: "登记" }));

    await waitFor(() => expect(installPlugin).toHaveBeenCalledWith({ sourcePath: "local-tools" }));
    expect(await screen.findByText("已登记 Local tools，当前保持停用。")).toBeTruthy();
    expect(screen.getByText("Local tools")).toBeTruthy();
  });

  it("requires inline confirmation before removing only the registration", async () => {
    const uninstallPlugin = vi.fn(async () => ({ etag: '"plugin-catalog-2"' }));
    const user = userEvent.setup();
    render(<PluginWorkspace client={client({ uninstallPlugin })} />);

    await user.click(await screen.findByRole("button", { name: "移除插件 Local tools" }));
    expect(screen.getByText("移除登记？")).toBeTruthy();
    expect(uninstallPlugin).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "确认移除插件 Local tools" }));

    await waitFor(() => expect(uninstallPlugin).toHaveBeenCalledWith(
      "local-tools",
      '"plugin-catalog-1"',
    ));
    expect(await screen.findByText("已移除 Local tools 的登记。")).toBeTruthy();
    expect(screen.getByText("暂无已登记插件")).toBeTruthy();
  });

  it("refreshes after a catalog revision conflict and retries load failures", async () => {
    const listPlugins = vi.fn()
      .mockRejectedValueOnce(new Error("offline"))
      .mockResolvedValue({ value: { items: [PLUGIN] }, etag: '"plugin-catalog-1"' });
    const api = client({ listPlugins });
    const user = userEvent.setup();
    render(<PluginWorkspace client={api} />);

    expect(await screen.findByText("插件目录加载失败")).toBeTruthy();
    expect(screen.getByRole("alert").textContent).toContain("插件目录连接失败。");
    await user.click(screen.getByRole("button", { name: "重试" }));
    expect(await screen.findByText("Local tools")).toBeTruthy();
    expect(listPlugins).toHaveBeenCalledTimes(2);

    const conflictList = vi.fn()
      .mockResolvedValueOnce({ value: { items: [PLUGIN] }, etag: '"plugin-catalog-1"' })
      .mockResolvedValue({ value: { items: [{ ...PLUGIN, enabled: true }] }, etag: '"plugin-catalog-2"' });
    const conflictApi = client({
      listPlugins: conflictList,
      updatePlugin: vi.fn(async () => {
        throw new PluginApiError("http", "Conflict", { code: "revision_conflict", status: 409 });
      }),
    });
    cleanup();
    render(<PluginWorkspace client={conflictApi} />);
    await user.click(await screen.findByRole("switch", { name: "启用插件 Local tools" }));
    await waitFor(() => expect(conflictList).toHaveBeenCalledTimes(2));
    expect(((await screen.findByRole("switch", { name: "停用插件 Local tools" })) as HTMLInputElement).checked)
      .toBe(true);
  });
});
