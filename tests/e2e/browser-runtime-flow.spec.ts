import { Buffer } from "node:buffer";
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

function requiredPositiveIntegerEnvironment(name: string): number {
  const value = Number(requiredEnvironment(name));
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new Error(`${name} must be a positive integer.`);
  }
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

async function enableBrowserToolset(page: Page, profileId: string): Promise<void> {
  await page.getByRole("button", { name: "工具 / MCP", exact: true }).click();
  await expect(page.getByRole("heading", { name: "工具集", exact: true })).toBeVisible();
  const profile = page.getByRole("combobox", { name: "工具 Profile" });
  await profile.selectOption(profileId);
  await expect(profile).toHaveValue(profileId);
  await expect(page.getByText("正在加载工具列表")).toHaveCount(0);
  const browserToolset = page.getByRole("switch", {
    name: "启用 Browser Automation (browser)",
  });
  await expect(browserToolset).toBeEnabled();
  const updateResponse = page.waitForResponse((response) => responseMatches(
    response,
    "PATCH",
    `/api/v1/profiles/${profileId}/toolsets/browser`,
  ));
  await browserToolset.click();
  expect((await updateResponse).status()).toBe(200);
  await expect(page.getByRole("switch", {
    name: "停用 Browser Automation (browser)",
  })).toBeChecked();
}

async function cancelRunIfActive(page: Page, runId: string): Promise<void> {
  await page.evaluate(async ({ id, timeoutMs }) => {
    const terminal = new Set(["completed", "cancelled", "failed"]);
    const runPath = `/api/v1/runs/${encodeURIComponent(id)}`;
    const readRun = async (): Promise<{ status?: string }> => {
      const response = await fetch(runPath, { cache: "no-store" });
      if (!response.ok) throw new Error(`Unable to inspect E2E Run: HTTP ${response.status}`);
      return response.json() as Promise<{ status?: string }>;
    };
    if (terminal.has((await readRun()).status || "")) return;
    const cancellation = await fetch(`${runPath}/cancel`, { method: "POST" });
    if (!cancellation.ok) {
      throw new Error(`Unable to cancel E2E Run: HTTP ${cancellation.status}`);
    }
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      if (terminal.has((await readRun()).status || "")) return;
      await new Promise((resolve) => globalThis.setTimeout(resolve, 100));
    }
    throw new Error("Timed out waiting for E2E Run cancellation.");
  }, { id: runId, timeoutMs: 15_000 });
}

async function readPublicRunState(page: Page, runId: string, sessionId: string): Promise<string> {
  return page.evaluate(async ({ id, session }) => {
    const runPath = `/api/v1/runs/${encodeURIComponent(id)}`;
    const messagePath = `/api/v1/sessions/${encodeURIComponent(session)}/messages`;
    const read = async (path: string): Promise<string> => {
      const response = await fetch(path, {
        cache: "no-store",
        headers: { Accept: path.endsWith("/events") ? "text/event-stream" : "application/json" },
      });
      return `${response.status}:${await response.text()}`;
    };
    const [runState, messages] = await Promise.all([read(runPath), read(messagePath)]);
    let status = "";
    try {
      const parsed = JSON.parse(runState.slice(runState.indexOf(":") + 1)) as {
        run?: { status?: string };
        status?: string;
      };
      status = parsed.run?.status || parsed.status || "";
    } catch {
      // The raw response remains part of the leak audit below.
    }
    const responses = [runState, messages];
    if (["completed", "cancelled", "failed"].includes(status)) {
      responses.push(await read(`${runPath}/events`));
    }
    return responses.join("\n");
  }, { id: runId, session: sessionId });
}

test("downloads metadata-only content after owner-bound CDP and download approvals", async ({
  page,
  baseURL,
}) => {
  const profileId = requiredEnvironment("SYNTHCHAT_E2E_BROWSER_PROFILE_ID");
  const profileName = requiredEnvironment("SYNTHCHAT_E2E_BROWSER_PROFILE_NAME");
  const providerBaseURL = requiredEnvironment("SYNTHCHAT_E2E_PROVIDER_BASE_URL");
  const model = requiredEnvironment("SYNTHCHAT_E2E_MODEL");
  const prompt = requiredEnvironment("SYNTHCHAT_E2E_BROWSER_PROMPT");
  const finalReply = requiredEnvironment("SYNTHCHAT_E2E_BROWSER_REPLY");
  const targetUrl = requiredEnvironment("SYNTHCHAT_E2E_BROWSER_URL");
  const privateTitle = requiredEnvironment("SYNTHCHAT_E2E_BROWSER_EXPECTED_TITLE");
  const downloadFilename = requiredEnvironment("SYNTHCHAT_E2E_BROWSER_DOWNLOAD_FILENAME");
  const downloadPrivateContent = requiredEnvironment(
    "SYNTHCHAT_E2E_BROWSER_DOWNLOAD_PRIVATE_CONTENT",
  );
  const downloadSelector = requiredEnvironment("SYNTHCHAT_E2E_BROWSER_DOWNLOAD_SELECTOR");
  const downloadSha256 = requiredEnvironment("SYNTHCHAT_E2E_BROWSER_DOWNLOAD_SHA256");
  const downloadSizeBytes = requiredPositiveIntegerEnvironment(
    "SYNTHCHAT_E2E_BROWSER_DOWNLOAD_SIZE_BYTES",
  );
  const providerControlCapability = requiredEnvironment(
    "SYNTHCHAT_E2E_PROVIDER_CONTROL_CAPABILITY",
  );
  const backendControlCapability = requiredEnvironment("SYNTHCHAT_E2E_CONTROL_CAPABILITY");
  const browserOrigin = new URL(requiredEnvironment("SYNTHCHAT_E2E_BASE_URL")).origin;
  const privateDownloadPayload = Buffer.from(downloadPrivateContent, "utf8").toString("base64");
  const privateDownloadUrl = `data:text/plain;base64,${privateDownloadPayload}`;
  const privateValues = [
    privateTitle,
    downloadPrivateContent,
    privateDownloadPayload,
    privateDownloadUrl,
    providerControlCapability,
    backendControlCapability,
  ];
  const observedProtectedRequests: Array<{ authorization?: string; body: string | null; origin: string }> = [];
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
  await enableBrowserToolset(page, profileId);

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

  try {
    const approvalPanels = page.getByRole("article").filter({
      has: page.getByRole("heading", { name: "需要确认工具调用" }),
    });
    const cdpApprovalPanel = approvalPanels.filter({ hasText: "browser_cdp" });
    await expect(cdpApprovalPanel).toBeVisible({ timeout: 30_000 });
    await expect(cdpApprovalPanel).toContainText("Run approved browser Runtime.evaluate");
    await expect(cdpApprovalPanel.getByRole("button", { name: "允许一次" })).toBeVisible();
    await expect(cdpApprovalPanel.getByRole("button", { name: "拒绝" })).toBeVisible();
    await expect(cdpApprovalPanel.getByRole("button", { name: "本会话允许" })).toHaveCount(0);
    await expect(cdpApprovalPanel.getByRole("button", { name: "始终允许" })).toHaveCount(0);
    await expect(page.locator(".chat-run-status")).toHaveText(/等待审批/u);
    const streamedTools = page.locator(".chat-stream-message .chat-tool-list");
    await expect(streamedTools).toContainText("browser_navigate");
    await expect(streamedTools).toContainText("browser_snapshot");
    await expect(streamedTools).toContainText("browser_cdp");
    await expect(streamedTools).toContainText("completed");

    const cdpApprovalSurface = [
      await cdpApprovalPanel.innerText(),
      await readPublicRunState(page, acceptedRun.run.id, acceptedRun.run.sessionId),
      await page.evaluate(() => JSON.stringify({
        local: { ...globalThis.localStorage },
        session: { ...globalThis.sessionStorage },
      })),
    ].join("\n");
    for (const value of privateValues) expect(cdpApprovalSurface).not.toContain(value);

    const cdpApprovalResponsePromise = page.waitForResponse((response) => (
      responseMatches(
        response,
        "POST",
        /^\/api\/v1\/runs\/[^/]+\/approvals\/approval_[0-9a-f]{32}$/u,
      )
      && new URL(response.url()).pathname.startsWith(
        `/api/v1/runs/${encodeURIComponent(acceptedRun.run.id)}/approvals/`,
      )
    ));
    await cdpApprovalPanel.getByRole("button", { name: "允许一次" }).click();
    const cdpApprovalResponse = await cdpApprovalResponsePromise;
    expect(cdpApprovalResponse.status()).toBe(200);
    const cdpApprovalBody = await cdpApprovalResponse.text();
    const cdpApprovalPath = new URL(cdpApprovalResponse.url()).pathname;

    const downloadApprovalPanel = approvalPanels.filter({ hasText: "browser_download" });
    await expect(downloadApprovalPanel).toBeVisible({ timeout: 30_000 });
    await expect(downloadApprovalPanel).toContainText(
      `Download browser resource from ${downloadSelector}`,
    );
    await expect(downloadApprovalPanel.getByRole("button", { name: "允许一次" })).toBeVisible();
    await expect(downloadApprovalPanel.getByRole("button", { name: "拒绝" })).toBeVisible();
    await expect(downloadApprovalPanel.getByRole("button", { name: "本会话允许" })).toHaveCount(0);
    await expect(downloadApprovalPanel.getByRole("button", { name: "始终允许" })).toHaveCount(0);
    await expect(page.locator(".chat-run-status")).toHaveText(/等待审批/u);
    await expect(streamedTools).toContainText("browser_download");
    const preDownloadPublicState = await readPublicRunState(
      page,
      acceptedRun.run.id,
      acceptedRun.run.sessionId,
    );
    expect(preDownloadPublicState).not.toContain(
      `Browser download accepted after isolated safety scan (${downloadSizeBytes} bytes)`,
    );
    const downloadApprovalSurface = [
      await downloadApprovalPanel.innerText(),
      preDownloadPublicState,
      await page.evaluate(() => JSON.stringify({
        local: { ...globalThis.localStorage },
        session: { ...globalThis.sessionStorage },
      })),
    ].join("\n");
    for (const value of privateValues) expect(downloadApprovalSurface).not.toContain(value);

    const downloadApprovalResponsePromise = page.waitForResponse((response) => (
      responseMatches(
        response,
        "POST",
        /^\/api\/v1\/runs\/[^/]+\/approvals\/approval_[0-9a-f]{32}$/u,
      )
      && new URL(response.url()).pathname.startsWith(
        `/api/v1/runs/${encodeURIComponent(acceptedRun.run.id)}/approvals/`,
      )
    ));
    await downloadApprovalPanel.getByRole("button", { name: "允许一次" }).click();
    const downloadApprovalResponse = await downloadApprovalResponsePromise;
    expect(downloadApprovalResponse.status()).toBe(200);
    const downloadApprovalBody = await downloadApprovalResponse.text();
    const downloadApprovalPath = new URL(downloadApprovalResponse.url()).pathname;
    expect(downloadApprovalPath).not.toBe(cdpApprovalPath);

    await expect(page.getByText(finalReply, { exact: true })).toBeVisible({ timeout: 30_000 });
    const completedTools = page.getByLabel("工具调用").locator(":scope > div");
    const navigateTool = completedTools.filter({ hasText: "browser_navigate" });
    const snapshotTools = completedTools.filter({ hasText: "browser_snapshot" });
    const cdpTool = completedTools.filter({ hasText: "browser_cdp" });
    const downloadTool = completedTools.filter({ hasText: "browser_download" });
    await expect(navigateTool).toContainText("Browser navigation completed");
    await expect(snapshotTools).toHaveCount(2);
    await expect(snapshotTools.nth(0)).toContainText(/Browser accessibility snapshot/u);
    await expect(snapshotTools.nth(1)).toContainText(/Browser accessibility snapshot/u);
    await expect(cdpTool).toContainText("已完成");
    await expect(cdpTool).toContainText("Approved CDP Runtime.evaluate completed");
    await expect(downloadTool).toContainText("已完成");
    await expect(downloadTool).toContainText(
      `Browser download accepted after isolated safety scan (${downloadSizeBytes} bytes)`,
    );
    await expect(page.locator(".chat-run-status")).toHaveText(/就绪/u);

    const publicState = await readPublicRunState(
      page,
      acceptedRun.run.id,
      acceptedRun.run.sessionId,
    );
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
      expect(cdpApprovalBody).not.toContain(value);
      expect(downloadApprovalBody).not.toContain(value);
      expect(cdpApprovalSurface).not.toContain(value);
      expect(downloadApprovalSurface).not.toContain(value);
      expect(publicState).not.toContain(value);
      expect(protectedRequestBodies).not.toContain(value);
      expect(consoleMessages.join("\n")).not.toContain(value);
    }
    expect(publicState).not.toContain(targetUrl);
    expect(publicState).not.toMatch(/"(?:filePath|downloadPath|path)"\s*:/iu);
    expect(visibleText).not.toMatch(/(?:\.synthchat[\\/]browser|[\\/]downloads[\\/])/iu);
    expect(renderedMarkup).not.toMatch(/(?:\.synthchat[\\/]browser|[\\/]downloads[\\/])/iu);
    expect(downloadSha256).toMatch(/^[0-9a-f]{64}$/u);
    expect(downloadFilename).toMatch(/\.txt$/u);
    expect(externalRequests).toEqual(new Set());
    expect(observedProtectedRequests.length).toBeGreaterThan(0);
    expect(observedProtectedRequests.every((request) => request.origin === browserOrigin)).toBe(true);
    expect(observedProtectedRequests.every((request) => request.authorization === undefined)).toBe(true);
  } finally {
    await cancelRunIfActive(page, acceptedRun.run.id).catch((error) => {
      console.error(`Browser E2E Run cleanup failed: ${String(error)}`);
    });
  }
});
