import { access, readFile } from "node:fs/promises";
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

async function fileExists(path: string): Promise<boolean> {
  try {
    await access(path);
    return true;
  } catch {
    return false;
  }
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
  await expect(page.getByLabel("Profile 详情").getByLabel("标识")).toHaveValue(profileId);
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

async function enableCodeToolset(page: Page, profileId: string): Promise<void> {
  await page.getByRole("button", { name: "工具 / MCP", exact: true }).click();
  await expect(page.getByRole("heading", { name: "工具集", exact: true })).toBeVisible();
  const profile = page.getByRole("combobox", { name: "工具 Profile" });
  await profile.selectOption(profileId);
  await expect(profile).toHaveValue(profileId);
  await expect(page.getByText("正在加载工具列表")).toHaveCount(0);
  const codeToolset = page.getByRole("switch", {
    name: "启用 Code Execution (code_execution)",
  });
  await expect(codeToolset).toBeEnabled();
  const updateResponse = page.waitForResponse((response) => responseMatches(
    response,
    "PATCH",
    `/api/v1/profiles/${profileId}/toolsets/code_execution`,
  ));
  await codeToolset.click();
  expect((await updateResponse).status()).toBe(200);
  await expect(page.getByRole("switch", {
    name: "停用 Code Execution (code_execution)",
  })).toBeChecked();
}

test("approves one real Python execute_code run and keeps source output private", async ({
  page,
  baseURL,
}) => {
  const profileId = requiredEnvironment("SYNTHCHAT_E2E_CODE_PROFILE_ID");
  const profileName = requiredEnvironment("SYNTHCHAT_E2E_CODE_PROFILE_NAME");
  const providerBaseURL = requiredEnvironment("SYNTHCHAT_E2E_PROVIDER_BASE_URL");
  const model = requiredEnvironment("SYNTHCHAT_E2E_MODEL");
  const prompt = requiredEnvironment("SYNTHCHAT_E2E_CODE_PROMPT");
  const finalReply = requiredEnvironment("SYNTHCHAT_E2E_CODE_REPLY");
  const targetPath = requiredEnvironment("SYNTHCHAT_E2E_CODE_TARGET");
  const privateOutput = requiredEnvironment("SYNTHCHAT_E2E_CODE_PRIVATE_OUTPUT");
  const privateFile = requiredEnvironment("SYNTHCHAT_E2E_CODE_PRIVATE_FILE");
  const providerControlCapability = requiredEnvironment(
    "SYNTHCHAT_E2E_PROVIDER_CONTROL_CAPABILITY",
  );
  const backendControlCapability = requiredEnvironment("SYNTHCHAT_E2E_CONTROL_CAPABILITY");
  const browserOrigin = new URL(requiredEnvironment("SYNTHCHAT_E2E_BASE_URL")).origin;
  const privateValues = [
    privateOutput,
    privateFile,
    providerControlCapability,
    backendControlCapability,
  ];
  const observedProtectedRequests: Array<{ authorization?: string; body: string | null; origin: string }> = [];
  const externalRequests = new Set<string>();
  const consoleMessages: string[] = [];

  expect(await fileExists(targetPath)).toBe(false);
  page.on("console", (message) => consoleMessages.push(message.text()));
  page.on("request", (request) => {
    const url = new URL(request.url());
    if (["http:", "https:"].includes(url.protocol) && url.origin !== browserOrigin) {
      externalRequests.add(url.origin);
    }
    if (!protectedApiRequest(request)) return;
    observedProtectedRequests.push({
      authorization: request.headers().authorization,
      body: request.postData(),
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
  await enableCodeToolset(page, profileId);

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
  await expect(approvalPanel).toContainText("execute_code");
  await expect(approvalPanel).toContainText("Execute Python programmatic tool script");
  await expect(page.locator(".chat-run-status")).toHaveText(/等待审批/u);
  expect(await fileExists(targetPath)).toBe(false);

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
  await expect.poll(async () => fileExists(targetPath)).toBe(true);
  expect(await readFile(targetPath, "utf8")).toBe(privateFile);
  const completedTool = page.getByLabel("工具调用").filter({ hasText: "execute_code" });
  await expect(completedTool).toContainText("已完成");
  await expect(completedTool).toContainText("Python execution completed with 0 tool calls");
  await expect(page.locator(".chat-run-status")).toHaveText(/就绪/u);

  const publicState = await page.evaluate(async ({ runId, sessionId }) => {
    const paths = [
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
    runId: acceptedRun.run.id,
    sessionId: acceptedRun.run.sessionId,
  });
  const visibleText = await page.locator("body").innerText();
  const renderedMarkup = await page.content();
  const browserStorage = await page.evaluate(() => JSON.stringify({
    local: { ...globalThis.localStorage },
    session: { ...globalThis.sessionStorage },
  }));
  const protectedRequestBodies = observedProtectedRequests
    .map((request) => request.body)
    .filter((body): body is string => body !== null)
    .join("\n");
  for (const value of privateValues) {
    expect(visibleText).not.toContain(value);
    expect(renderedMarkup).not.toContain(value);
    expect(browserStorage).not.toContain(value);
    expect(acceptedRunBody).not.toContain(value);
    expect(acceptedApprovalBody).not.toContain(value);
    expect(publicState).not.toContain(value);
    expect(protectedRequestBodies).not.toContain(value);
    expect(consoleMessages.join("\n")).not.toContain(value);
  }
  expect(externalRequests).toEqual(new Set());
  expect(observedProtectedRequests.length).toBeGreaterThan(0);
  expect(observedProtectedRequests.every((request) => request.origin === browserOrigin)).toBe(true);
  expect(observedProtectedRequests.every((request) => request.authorization === undefined)).toBe(true);
});
