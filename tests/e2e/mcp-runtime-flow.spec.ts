import {
  expect,
  test,
  type Page,
  type Request,
  type Response,
} from "@playwright/test";

function requiredEnvironment(name: string): string {
  const value = process.env[name]?.trim();
  if (!value) throw new Error(`${name} is required.`);
  return value;
}

function protectedApiRequest(request: Request): boolean {
  return new URL(request.url()).pathname.startsWith("/api/v1/");
}

function responseMatches(
  response: Response,
  method: string,
  pathname: string | RegExp,
): boolean {
  const responsePath = new URL(response.url()).pathname;
  return response.request().method() === method
    && (typeof pathname === "string" ? responsePath === pathname : pathname.test(responsePath));
}

async function createAndConfigureProfile(
  page: Page,
  profileId: string,
  profileName: string,
  providerBaseURL: string,
  model: string,
): Promise<void> {
  await page.getByRole("button", { name: "设置", exact: true }).click();
  await expect(page.getByRole("heading", { name: "模型配置" })).toBeVisible();
  await page.getByRole("button", { name: "创建 Profile" }).click();
  const createForm = page.locator("form").filter({
    has: page.getByRole("button", { name: "创建", exact: true }),
  });
  await createForm.getByLabel("标识").fill(profileId);
  await createForm.getByLabel("显示名称").fill(profileName);
  const createResponse = page.waitForResponse((response) => responseMatches(
    response,
    "POST",
    "/api/v1/profiles",
  ));
  await createForm.getByRole("button", { name: "创建", exact: true }).click();
  expect((await createResponse).status()).toBe(201);
  const profileDetail = page.getByLabel("Profile 详情");
  await expect(profileDetail.getByLabel("标识")).toHaveValue(profileId);
  await expect(page.getByText("正在加载配置")).toHaveCount(0);

  const modelSection = page.locator("section").filter({
    has: page.getByRole("heading", { name: "模型配置" }),
  });
  const provider = modelSection.locator("label").filter({
    has: page.getByText("Provider", { exact: true }),
  }).getByRole("combobox");
  await provider.selectOption("lmstudio");
  await modelSection.locator("label").filter({
    has: page.getByText("模型", { exact: true }),
  }).getByRole("textbox").fill(model);
  await modelSection.locator("label").filter({
    has: page.getByText("Base URL", { exact: true }),
  }).getByRole("textbox").fill(providerBaseURL);
  const configResponse = page.waitForResponse((response) => responseMatches(
    response,
    "PATCH",
    `/api/v1/profiles/${profileId}/config`,
  ));
  await modelSection.getByRole("button", { name: "保存配置" }).click();
  expect((await configResponse).status()).toBe(200);

  const activationResponse = page.waitForResponse((response) => responseMatches(
    response,
    "PUT",
    `/api/v1/profiles/${profileId}/active`,
  ));
  await page.getByRole("button", { name: "设为活动" }).click();
  expect((await activationResponse).status()).toBe(200);
}

async function createMcpServer(
  page: Page,
  profileId: string,
  serverName: string,
  command: string,
): Promise<{ body: string; id: string }> {
  await page.getByRole("button", { name: "工具 / MCP", exact: true }).click();
  await expect(page.getByRole("heading", { name: "MCP Servers" })).toBeVisible();
  const profile = page.getByRole("combobox", { name: "工具 Profile" });
  await profile.selectOption(profileId);
  await expect(profile).toHaveValue(profileId);
  await expect(page.getByText("正在加载 MCP servers")).toHaveCount(0);

  await page.getByRole("button", { name: "创建 MCP server" }).click();
  const form = page.getByRole("form", { name: "创建 MCP server" });
  await form.getByLabel("名称", { exact: true }).fill(serverName);
  await form.getByLabel("Executable").fill(command);
  await form.getByLabel("超时（秒）").fill("5");
  const createResponse = page.waitForResponse((response) => responseMatches(
    response,
    "POST",
    `/api/v1/profiles/${profileId}/mcp/servers`,
  ));
  await form.getByRole("button", { name: "创建", exact: true }).click();
  const response = await createResponse;
  expect(response.status()).toBe(201);
  const body = await response.text();
  const value = JSON.parse(body) as { id: string };
  expect(value.id).toMatch(/^mcp_[0-9a-f]{32}$/u);

  const row = page.locator("article.mcp-server-row").filter({ hasText: serverName });
  await expect(row).toContainText("Standard I/O");
  await expect(row).toContainText("运行时可用");
  await expect(row).toContainText("已启用");
  await expect(row).toContainText("密钥引用就绪");
  return { body, id: value.id };
}

test("creates and calls one stdio MCP tool through Rust, then deletes its config", async ({
  page,
  baseURL,
}) => {
  const profileId = requiredEnvironment("SYNTHCHAT_E2E_MCP_PROFILE_ID");
  const profileName = requiredEnvironment("SYNTHCHAT_E2E_MCP_PROFILE_NAME");
  const providerBaseURL = requiredEnvironment("SYNTHCHAT_E2E_PROVIDER_BASE_URL");
  const model = requiredEnvironment("SYNTHCHAT_E2E_MODEL");
  const serverName = requiredEnvironment("SYNTHCHAT_E2E_MCP_SERVER_NAME");
  const toolName = requiredEnvironment("SYNTHCHAT_E2E_MCP_TOOL_NAME");
  const command = requiredEnvironment("SYNTHCHAT_E2E_MCP_COMMAND");
  const prompt = requiredEnvironment("SYNTHCHAT_E2E_MCP_PROMPT");
  const finalReply = requiredEnvironment("SYNTHCHAT_E2E_MCP_REPLY");
  const privateResult = requiredEnvironment("SYNTHCHAT_E2E_MCP_PRIVATE_RESULT");
  const providerControlCapability = requiredEnvironment(
    "SYNTHCHAT_E2E_PROVIDER_CONTROL_CAPABILITY",
  );
  const backendControlCapability = requiredEnvironment("SYNTHCHAT_E2E_CONTROL_CAPABILITY");
  const browserOrigin = new URL(requiredEnvironment("SYNTHCHAT_E2E_BASE_URL")).origin;
  const privateValues = [privateResult, providerControlCapability, backendControlCapability];
  const observedProtectedRequests: Array<{ authorization?: string; origin: string }> = [];
  const externalRequests = new Set<string>();
  const consoleMessages: string[] = [];

  page.on("console", (message) => consoleMessages.push(message.text()));
  page.on("request", (request) => {
    const url = new URL(request.url());
    if (["http:", "https:"].includes(url.protocol) && url.origin !== browserOrigin) {
      externalRequests.add(url.origin);
    }
    if (!protectedApiRequest(request)) return;
    observedProtectedRequests.push({
      authorization: request.headers().authorization,
      origin: url.origin,
    });
  });

  await page.addInitScript(() => {
    const runtime = globalThis as typeof globalThis & {
      __SYNTHCHAT_BACKEND_URL__?: string;
    };
    runtime.__SYNTHCHAT_BACKEND_URL__ = globalThis.location.origin;
  });
  await page.goto(baseURL || "/");
  await expect(page.getByRole("button", { name: /后端在线/u })).toBeVisible();
  await createAndConfigureProfile(page, profileId, profileName, providerBaseURL, model);
  const created = await createMcpServer(page, profileId, serverName, command);
  for (const value of privateValues) expect(created.body).not.toContain(value);

  await page.getByRole("button", { name: "聊天", exact: true }).click();
  await expect(page.getByLabel("聊天 Profile")).toHaveValue(profileId);
  const sessionResponse = page.waitForResponse((response) => responseMatches(
    response,
    "POST",
    "/api/v1/sessions",
  ));
  await page.getByRole("button", { name: "新建会话" }).click();
  expect((await sessionResponse).status()).toBe(201);

  const runResponse = page.waitForResponse((response) => responseMatches(
    response,
    "POST",
    /^\/api\/v1\/sessions\/[^/]+\/runs$/u,
  ));
  await page.getByRole("textbox", { name: "消息", exact: true }).fill(prompt);
  await page.getByRole("button", { name: "发送消息" }).click();
  const acceptedRunResponse = await runResponse;
  expect(acceptedRunResponse.status()).toBe(202);
  const acceptedRunBody = await acceptedRunResponse.text();
  const acceptedRun = JSON.parse(acceptedRunBody) as {
    run: { id: string; sessionId: string };
  };

  const approvalPanel = page.getByRole("article").filter({
    has: page.getByRole("heading", { name: "需要确认工具调用" }),
  });
  await expect(approvalPanel).toBeVisible();
  await expect(approvalPanel).toContainText(toolName);
  await expect(approvalPanel).toContainText(`MCP tool ${toolName}`);
  await expect(page.locator(".chat-run-status")).toHaveText(/等待审批/u);
  const runningTool = page.locator(".chat-stream-message").filter({ hasText: toolName });
  await expect(runningTool).toContainText("running");

  const approvalResponse = page.waitForResponse((response) => responseMatches(
    response,
    "POST",
    /^\/api\/v1\/runs\/[^/]+\/approvals\/approval_[0-9a-f]{32}$/u,
  ));
  await approvalPanel.getByRole("button", { name: "允许一次" }).click();
  const acceptedApprovalResponse = await approvalResponse;
  expect(acceptedApprovalResponse.status()).toBe(200);
  const acceptedApprovalBody = await acceptedApprovalResponse.text();

  await expect(approvalPanel).toHaveCount(0);
  await expect(page.getByText(finalReply, { exact: true })).toBeVisible();
  const completedTool = page.getByLabel("工具调用").filter({ hasText: toolName });
  await expect(completedTool).toContainText("已完成");
  await expect(completedTool).toContainText("MCP tool completed");
  await expect(page.locator(".chat-run-status")).toHaveText(/就绪/u);

  const publicState = await page.evaluate(async ({ profileId: id, runId, sessionId }) => {
    const paths = [
      `/api/v1/profiles/${encodeURIComponent(id)}/mcp/servers`,
      `/api/v1/runs/${encodeURIComponent(runId)}`,
      `/api/v1/runs/${encodeURIComponent(runId)}/events`,
      `/api/v1/sessions/${encodeURIComponent(sessionId)}/messages`,
    ];
    const responses = await Promise.all(paths.map(async (path) => {
      const response = await fetch(path, {
        cache: "no-store",
        headers: { Accept: path.endsWith("/events") ? "text/event-stream" : "application/json" },
      });
      return `${response.status}:${await response.text()}`;
    }));
    return responses.join("\n");
  }, {
    profileId,
    runId: acceptedRun.run.id,
    sessionId: acceptedRun.run.sessionId,
  });
  const visibleText = await page.locator("body").innerText();
  const renderedMarkup = await page.content();
  const browserStorage = await page.evaluate(() => JSON.stringify({
    local: { ...globalThis.localStorage },
    session: { ...globalThis.sessionStorage },
  }));
  for (const value of privateValues) {
    expect(visibleText).not.toContain(value);
    expect(renderedMarkup).not.toContain(value);
    expect(browserStorage).not.toContain(value);
    expect(acceptedRunBody).not.toContain(value);
    expect(acceptedApprovalBody).not.toContain(value);
    expect(publicState).not.toContain(value);
    expect(consoleMessages.join("\n")).not.toContain(value);
  }

  await page.getByRole("button", { name: "工具 / MCP", exact: true }).click();
  const toolsProfile = page.getByRole("combobox", { name: "工具 Profile" });
  await toolsProfile.selectOption(profileId);
  await expect(page.getByText("正在加载 MCP servers")).toHaveCount(0);
  const row = page.locator("article.mcp-server-row").filter({ hasText: serverName });
  await row.getByRole("button", { name: `删除 MCP server ${serverName}` }).click();
  const deleteResponse = page.waitForResponse((response) => responseMatches(
    response,
    "DELETE",
    `/api/v1/profiles/${profileId}/mcp/servers/${created.id}`,
  ));
  await row.getByRole("button", { name: `确认删除 MCP server ${serverName}` }).click();
  expect((await deleteResponse).status()).toBe(204);
  await expect(row).toHaveCount(0);

  expect(externalRequests).toEqual(new Set());
  expect(observedProtectedRequests.length).toBeGreaterThan(0);
  expect(observedProtectedRequests.every((request) => request.origin === browserOrigin)).toBe(true);
  expect(observedProtectedRequests.every((request) => request.authorization === undefined)).toBe(true);
});
