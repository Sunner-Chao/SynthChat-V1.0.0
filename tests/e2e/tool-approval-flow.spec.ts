import { access, readFile, readdir } from "node:fs/promises";
import { dirname, isAbsolute, relative, resolve, sep } from "node:path";
import {
  expect,
  test,
  type Page,
  type Request,
  type Response,
  type Route,
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

function workspaceTarget(workspaceRoot: string, relativePath: string): string {
  if (!isAbsolute(workspaceRoot)) {
    throw new Error("SYNTHCHAT_E2E_APPROVAL_WORKSPACE must be absolute.");
  }
  const root = resolve(workspaceRoot);
  const target = resolve(root, ...relativePath.split("/"));
  const contained = relative(root, target);
  if (
    !contained
    || isAbsolute(contained)
    || contained === ".."
    || contained.startsWith(`..${sep}`)
  ) {
    throw new Error("SYNTHCHAT_E2E_APPROVAL_RELATIVE_PATH must stay inside the Workspace.");
  }
  return target;
}

async function fileExists(path: string): Promise<boolean> {
  try {
    await access(path);
    return true;
  } catch {
    return false;
  }
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
      await new Promise((resolveDelay) => globalThis.setTimeout(resolveDelay, 100));
    }
    throw new Error("Timed out waiting for E2E Run cancellation.");
  }, { id: runId, timeoutMs: 15_000 });
}

async function readTerminalPublicState(
  page: Page,
  runId: string,
  sessionId: string,
): Promise<string> {
  return page.evaluate(async ({ id, session }) => {
    const paths = [
      `/api/v1/runs/${encodeURIComponent(id)}`,
      `/api/v1/runs/${encodeURIComponent(id)}/events`,
      `/api/v1/sessions/${encodeURIComponent(session)}/messages`,
    ];
    const responses = await Promise.all(paths.map(async (path) => {
      const response = await fetch(path, {
        cache: "no-store",
        headers: { Accept: path.endsWith("/events") ? "text/event-stream" : "application/json" },
      });
      return `${response.status}:${await response.text()}`;
    }));
    return responses.join("\n");
  }, { id: runId, session: sessionId });
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
  await expect(provider).toHaveValue("lmstudio");
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
  const savedConfigResponse = await configResponse;
  expect(savedConfigResponse.status()).toBe(200);
  const savedConfig = await savedConfigResponse.json() as Record<string, unknown>;
  expect(savedConfig.model).toEqual(expect.objectContaining({
    baseUrl: providerBaseURL,
    model,
    provider: "lmstudio",
  }));
  await expect(modelSection.getByRole("button", { name: "保存配置" })).toBeDisabled();

  const activationResponse = page.waitForResponse((response) => responseMatches(
    response,
    "PUT",
    `/api/v1/profiles/${profileId}/active`,
  ));
  await page.getByRole("button", { name: "设为活动" }).click();
  expect((await activationResponse).status()).toBe(200);
}

async function registerWorkspace(
  page: Page,
  profileId: string,
  workspaceRoot: string,
): Promise<string> {
  const response = await page.evaluate(async ({ id, path }) => {
    const registration = await fetch(`/api/v1/profiles/${encodeURIComponent(id)}/workspaces`, {
      body: JSON.stringify({ path }),
      cache: "no-store",
      headers: {
        Accept: "application/json",
        "Content-Type": "application/json",
        "Idempotency-Key": `e2e-workspace-${id}`,
      },
      method: "POST",
    });
    return { status: registration.status, text: await registration.text() };
  }, { id: profileId, path: workspaceRoot });
  expect(response.status, response.text).toBe(201);
  expect(response.text).not.toContain(workspaceRoot);
  const value = JSON.parse(response.text) as Record<string, unknown>;
  expect(value).toMatchObject({
    available: true,
    profileId,
  });
  expect(value.id).toEqual(expect.stringMatching(/^workspace_[A-Za-z0-9_]+$/u));
  return value.id as string;
}

async function enableFileToolset(page: Page, profileId: string): Promise<void> {
  await page.getByRole("button", { name: "工具 / MCP", exact: true }).click();
  await expect(page.getByRole("heading", { name: "工具集", exact: true })).toBeVisible();
  const profile = page.getByRole("combobox", { name: "工具 Profile" });
  await profile.selectOption(profileId);
  await expect(profile).toHaveValue(profileId);
  await expect(page.getByText("正在加载工具列表")).toHaveCount(0);

  const fileToolset = page.getByRole("switch", { name: "启用 File Operations (file)" });
  await expect(fileToolset).toBeVisible();
  const updateResponse = page.waitForResponse((response) => responseMatches(
    response,
    "PATCH",
    `/api/v1/profiles/${profileId}/toolsets/file`,
  ));
  await fileToolset.click();
  expect((await updateResponse).status()).toBe(200);
  await expect(page.getByRole("switch", { name: "停用 File Operations (file)" })).toBeChecked();
}

test("runs real Rust write, read, search, and approved patch with private results", async ({
  page,
  baseURL,
}) => {
  const profileId = requiredEnvironment("SYNTHCHAT_E2E_APPROVAL_PROFILE_ID");
  const profileName = requiredEnvironment("SYNTHCHAT_E2E_APPROVAL_PROFILE_NAME");
  const providerBaseURL = requiredEnvironment("SYNTHCHAT_E2E_PROVIDER_BASE_URL");
  const model = requiredEnvironment("SYNTHCHAT_E2E_MODEL");
  const prompt = requiredEnvironment("SYNTHCHAT_E2E_APPROVAL_PROMPT");
  const finalReply = requiredEnvironment("SYNTHCHAT_E2E_APPROVAL_REPLY");
  const workspaceRoot = requiredEnvironment("SYNTHCHAT_E2E_APPROVAL_WORKSPACE");
  const relativePath = requiredEnvironment("SYNTHCHAT_E2E_APPROVAL_RELATIVE_PATH");
  const publicNeedle = requiredEnvironment("SYNTHCHAT_E2E_APPROVAL_PUBLIC_NEEDLE");
  const originalContent = requiredEnvironment("SYNTHCHAT_E2E_APPROVAL_PRIVATE_CONTENT");
  const patchedContent = requiredEnvironment("SYNTHCHAT_E2E_APPROVAL_PATCHED_CONTENT");
  const providerControlCapability = requiredEnvironment(
    "SYNTHCHAT_E2E_PROVIDER_CONTROL_CAPABILITY",
  );
  const backendControlCapability = requiredEnvironment("SYNTHCHAT_E2E_CONTROL_CAPABILITY");
  const browserOrigin = new URL(requiredEnvironment("SYNTHCHAT_E2E_BASE_URL")).origin;
  const targetPath = workspaceTarget(workspaceRoot, relativePath);
  const writeArguments = JSON.stringify({ path: relativePath, content: originalContent });
  const readArguments = JSON.stringify({ path: relativePath, offset: 1, limit: 2000 });
  const searchArguments = JSON.stringify({
    pattern: publicNeedle,
    target: "content",
    path: relativePath,
    limit: 10,
    offset: 0,
    output_mode: "content",
    context: 0,
  });
  const patchArguments = JSON.stringify({
    mode: "replace",
    path: relativePath,
    old_string: originalContent,
    new_string: patchedContent,
    replace_all: false,
  });
  const privateValues = [
    originalContent,
    patchedContent,
    writeArguments,
    readArguments,
    searchArguments,
    patchArguments,
    providerControlCapability,
    backendControlCapability,
  ];
  const publicSurfacePrivateValues = [
    workspaceRoot,
    targetPath,
    workspaceRoot.replaceAll("\\", "\\\\"),
    targetPath.replaceAll("\\", "\\\\"),
    ...privateValues,
  ];
  const observedProtectedRequests: Array<{
    authorization?: string;
    body: string | null;
    origin: string;
  }> = [];
  const externalRequests = new Set<string>();
  const consoleMessages: string[] = [];
  const approvalBodies: string[] = [];
  let acceptedRun: { id: string; sessionId: string } | null = null;

  expect(originalContent).toContain(publicNeedle);
  expect(patchedContent).toContain(publicNeedle);
  expect(patchedContent).not.toBe(originalContent);
  expect(originalContent).not.toMatch(/[\r\n]/u);
  expect(patchedContent).not.toMatch(/[\r\n]/u);
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
  const workspaceId = await registerWorkspace(page, profileId, workspaceRoot);
  await enableFileToolset(page, profileId);

  let injectedRunRequests = 0;
  const runRoutePattern = /\/api\/v1\/sessions\/[^/]+\/runs(?:\?.*)?$/u;
  let runRouteInstalled = true;
  const injectWorkspace = async (route: Route): Promise<void> => {
    if (route.request().method() !== "POST") {
      await route.continue();
      return;
    }
    const body = route.request().postDataJSON() as Record<string, unknown>;
    if ("workspaceId" in body) {
      throw new Error("The chat request unexpectedly supplied a Workspace already.");
    }
    injectedRunRequests += 1;
    await route.continue({
      postData: JSON.stringify({ ...body, workspaceId }),
    });
  };
  await page.route(runRoutePattern, injectWorkspace);

  try {
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
    const eventResponse = page.waitForResponse((response) => responseMatches(
      response,
      "GET",
      /^\/api\/v1\/runs\/[^/]+\/events$/u,
    ));
    await page.getByRole("textbox", { name: "消息", exact: true }).fill(prompt);
    await page.getByRole("button", { name: "发送消息" }).click();
    const acceptedRunResponse = await runResponse;
    const publicEventResponse = await eventResponse;
    expect(acceptedRunResponse.status()).toBe(202);
    expect(publicEventResponse.status()).toBe(200);
    const acceptedRunBody = await acceptedRunResponse.text();
    acceptedRun = (JSON.parse(acceptedRunBody) as {
      run: { id: string; sessionId: string };
    }).run;
    expect(injectedRunRequests).toBe(1);
    await page.unroute(runRoutePattern, injectWorkspace);
    runRouteInstalled = false;

    const approvalPanels = page.getByRole("article").filter({
      has: page.getByRole("heading", { name: "需要确认工具调用" }),
    });
    const writeApprovalPanel = approvalPanels.filter({ hasText: "write_file" });
    await expect(writeApprovalPanel).toBeVisible();
    await expect(page.locator(".chat-run-status")).toHaveText(/等待审批/u);
    await expect(writeApprovalPanel).toContainText(
      `Write ${relativePath} (${Buffer.byteLength(originalContent, "utf8")} bytes)`,
    );
    await expect(writeApprovalPanel.getByRole("button", { name: "允许一次" })).toBeVisible();
    await expect(writeApprovalPanel.getByRole("button", { name: "拒绝" })).toBeVisible();
    await expect(writeApprovalPanel.getByRole("button", { name: "本会话允许" })).toHaveCount(0);
    await expect(writeApprovalPanel.getByRole("button", { name: "始终允许" })).toHaveCount(0);
    const liveTools = page.locator(".chat-stream-message .chat-tool-list > div");
    await expect(liveTools.filter({ hasText: "write_file" })).toContainText("running");
    expect(await fileExists(targetPath)).toBe(false);
    const writePendingSurfaces = [
      await page.locator("body").innerText(),
      await page.content(),
      await writeApprovalPanel.innerText(),
    ].join("\n");
    for (const value of publicSurfacePrivateValues) {
      expect(writePendingSurfaces).not.toContain(value);
    }

    const writeApprovalResponsePromise = page.waitForResponse((response) => (
      responseMatches(
        response,
        "POST",
        /^\/api\/v1\/runs\/[^/]+\/approvals\/approval_[0-9a-f]{32}$/u,
      )
      && new URL(response.url()).pathname.startsWith(
        `/api/v1/runs/${encodeURIComponent(acceptedRun!.id)}/approvals/`,
      )
    ));
    await writeApprovalPanel.getByRole("button", { name: "允许一次" }).click();
    const writeApprovalResponse = await writeApprovalResponsePromise;
    expect(writeApprovalResponse.status()).toBe(200);
    approvalBodies.push(await writeApprovalResponse.text());
    const writeApprovalPath = new URL(writeApprovalResponse.url()).pathname;

    const patchApprovalPanel = approvalPanels.filter({ hasText: "patch" });
    await expect(patchApprovalPanel).toBeVisible({ timeout: 30_000 });
    await expect(page.locator(".chat-run-status")).toHaveText(/等待审批/u);
    await expect(patchApprovalPanel).toContainText(
      `Patch ${relativePath} (+1/-1 lines, one)`,
    );
    await expect(patchApprovalPanel.getByRole("button", { name: "允许一次" })).toBeVisible();
    await expect(patchApprovalPanel.getByRole("button", { name: "拒绝" })).toBeVisible();
    await expect(patchApprovalPanel.getByRole("button", { name: "本会话允许" })).toHaveCount(0);
    await expect(patchApprovalPanel.getByRole("button", { name: "始终允许" })).toHaveCount(0);
    await expect.poll(async () => (
      await fileExists(targetPath) ? readFile(targetPath, "utf8") : null
    )).toBe(originalContent);
    await expect(liveTools.filter({ hasText: "write_file" })).toContainText("completed");
    await expect(liveTools.filter({ hasText: "read_file" })).toContainText("completed");
    await expect(liveTools.filter({ hasText: "search_files" })).toContainText("completed");
    await expect(liveTools.filter({ hasText: "patch" })).toContainText("running");
    const patchPendingSurfaces = [
      await page.locator("body").innerText(),
      await page.content(),
      await patchApprovalPanel.innerText(),
    ].join("\n");
    for (const value of publicSurfacePrivateValues) {
      expect(patchPendingSurfaces).not.toContain(value);
    }

    const patchApprovalResponsePromise = page.waitForResponse((response) => (
      responseMatches(
        response,
        "POST",
        /^\/api\/v1\/runs\/[^/]+\/approvals\/approval_[0-9a-f]{32}$/u,
      )
      && new URL(response.url()).pathname.startsWith(
        `/api/v1/runs/${encodeURIComponent(acceptedRun!.id)}/approvals/`,
      )
    ));
    await patchApprovalPanel.getByRole("button", { name: "允许一次" }).click();
    const patchApprovalResponse = await patchApprovalResponsePromise;
    expect(patchApprovalResponse.status()).toBe(200);
    approvalBodies.push(await patchApprovalResponse.text());
    expect(new URL(patchApprovalResponse.url()).pathname).not.toBe(writeApprovalPath);

    await expect(page.getByText(finalReply, { exact: true })).toBeVisible({ timeout: 30_000 });
    await expect.poll(async () => readFile(targetPath, "utf8")).toBe(patchedContent);
    const atomicTemps = (await readdir(dirname(targetPath)))
      .filter((name) => name.startsWith(".synthchat-write-") && name.endsWith(".tmp"));
    expect(atomicTemps).toEqual([]);

    const completedTools = page.getByLabel("工具调用").locator(":scope > div");
    await expect(completedTools).toHaveCount(4);
    const completedWrite = completedTools.filter({ hasText: "write_file" });
    const completedRead = completedTools.filter({ hasText: "read_file" });
    const completedSearch = completedTools.filter({ hasText: "search_files" });
    const completedPatch = completedTools.filter({ hasText: "patch" });
    await expect(completedWrite).toContainText("已完成");
    await expect(completedWrite).toContainText(
      `Wrote ${Buffer.byteLength(originalContent, "utf8")} bytes to ${relativePath}`,
    );
    await expect(completedRead).toContainText("已完成");
    await expect(completedRead).toContainText(
      `1 lines from ${relativePath}`,
    );
    await expect(completedSearch).toContainText("已完成");
    await expect(completedSearch).toContainText("1 matches");
    await expect(completedPatch).toContainText("已完成");
    await expect(completedPatch).toContainText(
      "Applied 1 patch operation(s)",
    );
    await expect(page.locator(".chat-run-status")).toHaveText(/就绪/u);

    const publicState = await readTerminalPublicState(
      page,
      acceptedRun.id,
      acceptedRun.sessionId,
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
    const approvalBodyText = approvalBodies.join("\n");
    for (const value of privateValues) {
      expect(visibleText).not.toContain(value);
      expect(renderedMarkup).not.toContain(value);
      expect(browserStorage).not.toContain(value);
      expect(acceptedRunBody).not.toContain(value);
      expect(approvalBodyText).not.toContain(value);
      expect(publicState).not.toContain(value);
      expect(protectedRequestBodies).not.toContain(value);
      expect(consoleMessages.join("\n")).not.toContain(value);
    }
    const publicSurfaces = [
      visibleText,
      renderedMarkup,
      browserStorage,
      acceptedRunBody,
      approvalBodyText,
      publicState,
    ].join("\n");
    for (const value of publicSurfacePrivateValues) {
      expect(publicSurfaces).not.toContain(value);
    }
    expect(publicState).toContain("write_file");
    expect(publicState).toContain("read_file");
    expect(publicState).toContain("search_files");
    expect(publicState).toContain("patch");
    expect(publicState).not.toContain('"diff"');
    expect(visibleText).not.toContain("Authorization");
    expect(externalRequests).toEqual(new Set());
    expect(observedProtectedRequests.length).toBeGreaterThan(0);
    expect(observedProtectedRequests.every((request) => request.origin === browserOrigin)).toBe(true);
    expect(observedProtectedRequests.every((request) => request.authorization === undefined)).toBe(true);

    const messageComposer = page.getByRole("textbox", { name: "消息", exact: true });
    await expect(messageComposer).toBeEnabled();
    await messageComposer.fill(`Follow-up ${profileId}`);
    await expect(page.getByRole("button", { name: "发送消息" })).toBeEnabled();
  } finally {
    if (runRouteInstalled) await page.unroute(runRoutePattern, injectWorkspace);
    if (acceptedRun) await cancelRunIfActive(page, acceptedRun.id);
  }
});
