// @vitest-environment jsdom

import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  McpApiError,
  type McpApi,
  type McpServer,
  type VersionedMcpServer,
  type VersionedMcpServers,
} from "../../api/mcp";
import { McpServersPanel } from "./McpServersPanel";

const SERVER_ID = `mcp_${"a".repeat(32)}`;
const REMOTE_ID = `mcp_${"b".repeat(32)}`;

const STDIO_SERVER: McpServer = {
  id: SERVER_ID,
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

const REMOTE_SERVER: McpServer = {
  id: REMOTE_ID,
  name: "remote_tools",
  transport: "streamableHttp",
  command: null,
  args: [],
  url: "https://mcp.example.com/rpc",
  enabled: false,
  timeoutSeconds: 45,
  envSecretNames: [],
  bearerTokenSecretName: "REMOTE_TOKEN",
  missingSecretNames: [],
};

function versioned(
  value: McpServer[] = [STDIO_SERVER, REMOTE_SERVER],
  etag = '"config-1"',
): VersionedMcpServers {
  return { value, etag };
}

function client(overrides: Partial<McpApi> = {}): McpApi {
  return {
    listServers: vi.fn(async () => versioned()),
    createServer: vi.fn(async (_profileId, input): Promise<VersionedMcpServer> => ({
      value: input.transport === "stdio"
        ? {
          ...STDIO_SERVER,
          name: input.name,
          command: input.command,
          args: input.args,
          enabled: input.enabled,
          timeoutSeconds: input.timeoutSeconds,
          envSecretNames: input.envSecretNames,
          missingSecretNames: input.envSecretNames,
        }
        : {
          ...REMOTE_SERVER,
          name: input.name,
          transport: input.transport,
          url: input.url,
          enabled: input.enabled,
          timeoutSeconds: input.timeoutSeconds,
          bearerTokenSecretName: input.bearerTokenSecretName ?? null,
        },
      etag: '"config-2"',
    })),
    updateServer: vi.fn(async (_profileId, _serverId, patch): Promise<VersionedMcpServer> => ({
      value: { ...STDIO_SERVER, ...patch },
      etag: '"config-2"',
    })),
    deleteServer: vi.fn(async () => ({ etag: '"config-3"' })),
    ...overrides,
  };
}

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("McpServersPanel", () => {
  it("fails closed behind mcpManagement and never calls the client", () => {
    const api = client();
    render(<McpServersPanel available={false} client={api} profileId="default" />);

    expect(screen.getByText("当前后端未启用 MCP 管理能力。")).toBeTruthy();
    expect(screen.getByText("当前后端未启用 MCP 管理能力。").closest("div")?.dataset.capabilityState).toBe(
      "disabled",
    );
    expect(api.listServers).not.toHaveBeenCalled();
  });

  it("lists transport metadata and missing keychain reference names without secret fields", async () => {
    const { container } = render(
      <McpServersPanel
        available
        client={client()}
        profileId="default"
        transportRuntime={{ stdio: true, streamableHttp: false, sse: false }}
      />,
    );

    expect(await screen.findByText("local_tools")).toBeTruthy();
    expect(screen.getByText("remote_tools")).toBeTruthy();
    expect(screen.getByText("Standard I/O")).toBeTruthy();
    expect(screen.getByText("Streamable HTTP")).toBeTruthy();
    expect(screen.getByText("运行时可用")).toBeTruthy();
    expect(screen.getByText("配置已保存，运行时未启用")).toBeTruthy();
    expect(screen.getAllByText("MCP_TOKEN").length).toBeGreaterThanOrEqual(1);
    expect(screen.getByText("密钥链缺少")).toBeTruthy();
    expect(screen.getByText("密钥引用就绪")).toBeTruthy();
    expect(container.querySelector('input[type="password"]')).toBeNull();
    expect(container.textContent).not.toContain("plaintext-secret");
  });

  it("creates stdio configuration with a reusable idempotency key and uses its new ETag", async () => {
    const user = userEvent.setup();
    const createServer = vi.fn(async (_profileId, input): Promise<VersionedMcpServer> => ({
      value: {
        ...STDIO_SERVER,
        name: input.name,
        command: input.transport === "stdio" ? input.command : "npx",
        args: input.transport === "stdio" ? input.args : [],
        envSecretNames: input.transport === "stdio" ? input.envSecretNames : [],
        missingSecretNames: input.transport === "stdio" ? input.envSecretNames : [],
      },
      etag: '"config-2"',
    }));
    const updateServer = vi.fn(async (): Promise<VersionedMcpServer> => ({
      value: { ...STDIO_SERVER, name: "new_server", enabled: false },
      etag: '"config-3"',
    }));
    const api = client({ createServer, updateServer });
    render(<McpServersPanel available client={api} profileId="default" />);

    await user.click(await screen.findByRole("button", { name: "创建 MCP server" }));
    const form = screen.getByRole("form", { name: "创建 MCP server" });
    await user.type(within(form).getByLabelText("名称"), "new_server");
    await user.type(within(form).getByLabelText("Executable"), "npx");
    await user.type(within(form).getByLabelText(/Arguments/u), "-y\n@example/mcp");
    await user.type(within(form).getByLabelText(/环境变量密钥链名称引用/u), "MCP_TOKEN");
    await user.click(within(form).getByRole("button", { name: "创建" }));

    await waitFor(() => expect(createServer).toHaveBeenCalledTimes(1));
    expect(createServer).toHaveBeenCalledWith(
      "default",
      {
        name: "new_server",
        transport: "stdio",
        command: "npx",
        args: ["-y", "@example/mcp"],
        enabled: true,
        timeoutSeconds: 30,
        envSecretNames: ["MCP_TOKEN"],
      },
      expect.stringMatching(/^mcp-create-/u),
    );

    await user.click(await screen.findByRole("switch", { name: "停用 MCP server new_server" }));
    await waitFor(() => expect(updateServer).toHaveBeenCalledWith(
      "default",
      SERVER_ID,
      { enabled: false },
      '"config-2"',
    ));
  });

  it("creates remote configuration using only a bearer keychain name reference", async () => {
    const user = userEvent.setup();
    const createServer = vi.fn(async (): Promise<VersionedMcpServer> => ({
      value: REMOTE_SERVER,
      etag: '"config-2"',
    }));
    render(
      <McpServersPanel available client={client({ createServer })} profileId="default" />,
    );

    await user.click(await screen.findByRole("button", { name: "创建 MCP server" }));
    const form = screen.getByRole("form", { name: "创建 MCP server" });
    await user.type(within(form).getByLabelText("名称"), "remote_tools");
    await user.selectOptions(within(form).getByLabelText("Transport"), "streamableHttp");
    await user.type(within(form).getByLabelText("URL"), "https://mcp.example.com/rpc");
    await user.type(within(form).getByLabelText(/Bearer 密钥链名称引用/u), "REMOTE_TOKEN");
    await user.click(within(form).getByRole("button", { name: "创建" }));

    await waitFor(() => expect(createServer).toHaveBeenCalledWith(
      "default",
      {
        name: "remote_tools",
        transport: "streamableHttp",
        url: "https://mcp.example.com/rpc",
        enabled: true,
        timeoutSeconds: 30,
        bearerTokenSecretName: "REMOTE_TOKEN",
      },
      expect.stringMatching(/^mcp-create-/u),
    ));
  });

  it("reuses the same idempotency key when an identical create is retried", async () => {
    const user = userEvent.setup();
    const createServer = vi.fn()
      .mockRejectedValueOnce(new McpApiError("http", "Service unavailable", {
        status: 503,
        code: "service_unavailable",
        requestId: "req-mcp-retry",
        retryable: true,
      }))
      .mockResolvedValueOnce({ value: STDIO_SERVER, etag: '"config-2"' });
    render(
      <McpServersPanel
        available
        client={client({
          listServers: vi.fn(async () => versioned([])),
          createServer,
        })}
        profileId="default"
      />,
    );

    await user.click(await screen.findByRole("button", { name: "创建 MCP server" }));
    const form = screen.getByRole("form", { name: "创建 MCP server" });
    await user.type(within(form).getByLabelText("名称"), "local_tools");
    await user.type(within(form).getByLabelText("Executable"), "npx");
    await user.click(within(form).getByRole("button", { name: "创建" }));
    expect(await screen.findByRole("alert")).toHaveProperty("textContent", "Service unavailable 请求 ID：req-mcp-retry");

    await user.click(within(form).getByRole("button", { name: "创建" }));
    await waitFor(() => expect(createServer).toHaveBeenCalledTimes(2));
    expect(createServer.mock.calls[0]![2]).toBe(createServer.mock.calls[1]![2]);
    expect(await screen.findByText("local_tools")).toBeTruthy();
  });

  it("updates transport-specific fields then deletes with the returned ETag", async () => {
    const user = userEvent.setup();
    const updateServer = vi.fn(async (_profileId, _serverId, patch): Promise<VersionedMcpServer> => ({
      value: { ...REMOTE_SERVER, ...patch },
      etag: '"config-2"',
    }));
    const deleteServer = vi.fn(async () => ({ etag: '"config-3"' }));
    const api = client({ updateServer, deleteServer });
    render(<McpServersPanel available client={api} profileId="default" />);

    await user.click(await screen.findByRole("button", { name: "编辑 MCP server remote_tools" }));
    const editForm = screen.getByRole("form", { name: "编辑 MCP server remote_tools" });
    const name = within(editForm).getByLabelText("名称");
    await user.clear(name);
    await user.type(name, "remote_updated");
    const bearer = within(editForm).getByLabelText(/Bearer 密钥链名称引用/u);
    await user.clear(bearer);
    await user.click(within(editForm).getByRole("button", { name: "保存" }));

    await waitFor(() => expect(updateServer).toHaveBeenCalledWith(
      "default",
      REMOTE_ID,
      {
        name: "remote_updated",
        transport: "streamableHttp",
        enabled: false,
        timeoutSeconds: 45,
        url: "https://mcp.example.com/rpc",
        bearerTokenSecretName: null,
      },
      '"config-1"',
    ));

    await user.click(await screen.findByRole("button", { name: "删除 MCP server remote_updated" }));
    await user.click(screen.getByRole("button", { name: "确认删除 MCP server remote_updated" }));
    await waitFor(() => expect(deleteServer).toHaveBeenCalledWith(
      "default",
      REMOTE_ID,
      '"config-2"',
    ));
    expect(screen.queryByText("remote_updated")).toBeNull();
  });

  it("reloads the full shared revision after a conflict", async () => {
    const user = userEvent.setup();
    const listServers = vi.fn()
      .mockResolvedValueOnce(versioned([STDIO_SERVER], '"config-stale"'))
      .mockResolvedValueOnce(versioned([{ ...STDIO_SERVER, enabled: false }], '"config-current"'));
    const updateServer = vi.fn(async () => {
      throw new McpApiError("http", "Configuration changed", {
        status: 409,
        code: "revision_conflict",
        requestId: "req-mcp-conflict",
        etag: '"config-current"',
      });
    });
    render(
      <McpServersPanel
        available
        client={client({ listServers, updateServer })}
        profileId="default"
      />,
    );

    await user.click(await screen.findByRole("switch", { name: "停用 MCP server local_tools" }));
    await waitFor(() => expect(listServers).toHaveBeenCalledTimes(2));
    expect(await screen.findByRole("switch", { name: "启用 MCP server local_tools" })).toBeTruthy();
    expect(screen.getByRole("alert").textContent).toContain("已重新加载");
    expect(updateServer).toHaveBeenCalledWith(
      "default",
      SERVER_ID,
      { enabled: false },
      '"config-stale"',
    );
  });
});
