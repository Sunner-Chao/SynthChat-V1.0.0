import { access, readFile, rm } from "node:fs/promises";
import { isAbsolute, relative, resolve, sep } from "node:path";
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

interface AcceptedE2eRun {
  rawBody: string;
  run: { id: string; sessionId: string };
}

interface ReplayEvent {
  data: Record<string, unknown>;
  event: string;
  id: string;
  sequence: number;
}

interface PublicSurface {
  eventsAFirst: string;
  eventsASecond: string;
  eventsB: string;
  messages: string;
  runA: string;
  runB: string;
}

function objectRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function checkedLoopbackOrigin(name: string): string {
  const url = new URL(requiredEnvironment(name));
  if (
    url.protocol !== "http:"
    || !["127.0.0.1", "::1", "localhost"].includes(url.hostname)
    || url.username
    || url.password
    || url.pathname !== "/"
    || url.search
    || url.hash
  ) {
    throw new Error(`${name} must be a loopback HTTP origin.`);
  }
  return url.origin;
}

async function providerRequestCount(): Promise<number> {
  const response = await fetch(
    `${checkedLoopbackOrigin("SYNTHCHAT_E2E_PROVIDER_CONTROL_URL")}/status`,
    {
      cache: "no-store",
      headers: {
        Accept: "application/json",
        "X-SynthChat-E2E-Provider-Control": requiredEnvironment(
          "SYNTHCHAT_E2E_PROVIDER_CONTROL_CAPABILITY",
        ),
      },
      redirect: "error",
    },
  );
  expect(response.status).toBe(200);
  const value: unknown = await response.json();
  if (
    !objectRecord(value)
    || Object.keys(value).sort().join(",") !== "requestCount,state"
    || !Number.isSafeInteger(value.requestCount)
    || (value.requestCount as number) < 0
    || !["armed", "holding", "idle"].includes(String(value.state))
  ) {
    throw new Error("Provider control response did not match the E2E contract.");
  }
  return value.requestCount as number;
}

async function sendChatRun(page: Page, prompt: string): Promise<AcceptedE2eRun> {
  const responsePromise = page.waitForResponse((response) => responseMatches(
    response,
    "POST",
    /^\/api\/v1\/sessions\/[^/]+\/runs$/u,
  ));
  const composer = page.getByRole("textbox", { name: "消息", exact: true });
  await expect(composer).toBeEnabled();
  await composer.fill(prompt);
  await page.getByRole("button", { name: "发送消息" }).click();
  const response = await responsePromise;
  expect(response.status()).toBe(202);
  const rawBody = await response.text();
  const value: unknown = JSON.parse(rawBody);
  if (
    !objectRecord(value)
    || !objectRecord(value.run)
    || typeof value.run.id !== "string"
    || typeof value.run.sessionId !== "string"
  ) {
    throw new Error("Run accepted response did not match the E2E contract.");
  }
  return {
    rawBody,
    run: { id: value.run.id, sessionId: value.run.sessionId },
  };
}

function approvalResponseMatchesRun(response: Response, runId: string): boolean {
  const pathname = new URL(response.url()).pathname;
  const prefix = `/api/v1/runs/${encodeURIComponent(runId)}/approvals/`;
  return response.request().method() === "POST"
    && pathname.startsWith(prefix)
    && /^approval_[0-9a-f]{32}$/u.test(pathname.slice(prefix.length));
}

async function approvePendingTool(
  page: Page,
  runId: string,
  toolName: string,
  summary: string | RegExp,
  sensitiveValues: string[] = [],
): Promise<string> {
  const panel = page.getByRole("article").filter({
    has: page.getByRole("heading", { name: "需要确认工具调用" }),
  });
  await expect(panel).toBeVisible({ timeout: 30_000 });
  await expect(panel).toContainText(toolName);
  await expect(panel).toContainText(summary);
  await expect(page.locator(".chat-run-status")).toHaveText(/等待审批/u);
  const approvalDom = await page.content();
  const approvalStorage = await page.evaluate(() => JSON.stringify({
    local: { ...globalThis.localStorage },
    session: { ...globalThis.sessionStorage },
  }));
  for (const value of sensitiveValues) {
    expect(approvalDom).not.toContain(value);
    expect(approvalStorage).not.toContain(value);
  }
  const responsePromise = page.waitForResponse((response) => (
    approvalResponseMatchesRun(response, runId)
  ));
  await panel.getByRole("button", { name: "允许一次" }).click();
  const response = await responsePromise;
  expect(response.status()).toBe(200);
  const rawBody = await response.text();
  await expect(panel).toHaveCount(0);
  return rawBody;
}

function parseSseReplay(raw: string, runId: string, sessionId: string): ReplayEvent[] {
  const events: ReplayEvent[] = [];
  const normalized = raw.replace(/\r\n?/gu, "\n");
  for (const block of normalized.split("\n\n")) {
    if (!block.trim()) continue;
    let id: string | null = null;
    let event: string | null = null;
    const dataLines: string[] = [];
    let frameTouched = false;
    for (const line of block.split("\n")) {
      if (!line || line.startsWith(":")) continue;
      frameTouched = true;
      const separator = line.indexOf(":");
      const field = separator === -1 ? line : line.slice(0, separator);
      let value = separator === -1 ? "" : line.slice(separator + 1);
      if (value.startsWith(" ")) value = value.slice(1);
      if (field === "id") id = value;
      else if (field === "event") event = value;
      else if (field === "data") dataLines.push(value);
      else throw new Error(`Unexpected SSE field: ${field}`);
    }
    if (!frameTouched) continue;
    if (!id || !event || dataLines.length === 0) throw new Error("Incomplete Run SSE frame.");
    const payload: unknown = JSON.parse(dataLines.join("\n"));
    if (
      !objectRecord(payload)
      || payload.runId !== runId
      || payload.sessionId !== sessionId
      || !Number.isSafeInteger(payload.sequence)
      || !objectRecord(payload.data)
      || id !== `${runId}:${String(payload.sequence)}`
    ) {
      throw new Error("Run SSE frame did not match its Run/Session envelope.");
    }
    events.push({
      data: payload.data,
      event,
      id,
      sequence: payload.sequence as number,
    });
  }
  events.forEach((event, index) => {
    if (event.sequence !== index + 1) throw new Error("Run SSE sequence was not continuous.");
  });
  return events;
}

async function terminalDeliveryObserved(
  page: Page,
  run: AcceptedE2eRun,
  callId: string,
  timeoutMs: number,
): Promise<boolean> {
  const raw = await page.evaluate(async ({ id, timeout }) => {
    const controller = new AbortController();
    const timer = globalThis.setTimeout(() => controller.abort(), timeout);
    try {
      const response = await fetch(`/api/v1/runs/${encodeURIComponent(id)}/events`, {
        cache: "no-store",
        headers: { Accept: "text/event-stream" },
        signal: controller.signal,
      });
      if (!response.ok) throw new Error(`Run delivery replay returned HTTP ${response.status}.`);
      return await response.text();
    } catch (error) {
      if (error instanceof DOMException && error.name === "AbortError") return null;
      throw error;
    } finally {
      globalThis.clearTimeout(timer);
    }
  }, { id: run.run.id, timeout: timeoutMs });
  if (raw === null) return false;
  return parseSseReplay(raw, run.run.id, run.run.sessionId).some((event) => (
    event.event === "tool.delivery"
    && event.data.callId === callId
    && ["exited", "killed", "lost", "failed_start"].includes(String(event.data.status))
  ));
}

async function readPublicSurface(
  page: Page,
  runA: AcceptedE2eRun,
  runB: AcceptedE2eRun,
): Promise<PublicSurface> {
  return page.evaluate(async ({ first, second }) => {
    const read = async (path: string, accept: string): Promise<string> => {
      const controller = new AbortController();
      const timer = globalThis.setTimeout(() => controller.abort(), 20_000);
      try {
        const response = await fetch(path, {
          cache: "no-store",
          headers: { Accept: accept },
          signal: controller.signal,
        });
        const text = await response.text();
        if (response.status !== 200) throw new Error(`${path} returned HTTP ${response.status}: ${text}`);
        return text;
      } finally {
        globalThis.clearTimeout(timer);
      }
    };
    const firstRunPath = `/api/v1/runs/${encodeURIComponent(first.id)}`;
    const secondRunPath = `/api/v1/runs/${encodeURIComponent(second.id)}`;
    const eventsAFirst = await read(`${firstRunPath}/events`, "text/event-stream");
    const eventsASecond = await read(`${firstRunPath}/events`, "text/event-stream");
    return {
      eventsAFirst,
      eventsASecond,
      eventsB: await read(`${secondRunPath}/events`, "text/event-stream"),
      messages: await read(
        `/api/v1/sessions/${encodeURIComponent(first.sessionId)}/messages`,
        "application/json",
      ),
      runA: await read(firstRunPath, "application/json"),
      runB: await read(secondRunPath, "application/json"),
    };
  }, { first: runA.run, second: runB.run });
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
    if (!cancellation.ok) throw new Error(`Unable to cancel E2E Run: HTTP ${cancellation.status}`);
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      if (terminal.has((await readRun()).status || "")) return;
      await new Promise((resolve) => globalThis.setTimeout(resolve, 100));
    }
    throw new Error("Timed out waiting for E2E Run cancellation.");
  }, { id: runId, timeoutMs: 15_000 });
}

async function finishBackgroundControlRun(page: Page, runId: string): Promise<boolean> {
  return page.evaluate(async ({ id, timeoutMs }) => {
    const deadline = Date.now() + timeoutMs;
    const runPath = `/api/v1/runs/${encodeURIComponent(id)}`;
    while (Date.now() < deadline) {
      const response = await fetch(runPath, { cache: "no-store" });
      if (!response.ok) return false;
      const run = await response.json() as {
        pendingAction?: {
          approvalId?: string;
          kind?: string;
          toolName?: string;
        } | null;
        status?: string;
      };
      if (run.status === "completed") return true;
      if (run.status === "cancelled" || run.status === "failed") return false;
      if (
        run.status === "waitingApproval"
        && run.pendingAction?.kind === "approval"
        && run.pendingAction.toolName === "process"
        && typeof run.pendingAction.approvalId === "string"
      ) {
        const approval = await fetch(
          `${runPath}/approvals/${encodeURIComponent(run.pendingAction.approvalId)}`,
          {
            body: JSON.stringify({ decision: "once" }),
            headers: { "Content-Type": "application/json" },
            method: "POST",
          },
        );
        if (!approval.ok && approval.status !== 409) return false;
      }
      await new Promise((resolve) => globalThis.setTimeout(resolve, 100));
    }
    return false;
  }, { id: runId, timeoutMs: 15_000 });
}

async function createBackgroundCleanupRun(
  page: Page,
  sessionId: string,
  workspaceId: string,
  prompt: string,
): Promise<string> {
  return page.evaluate(async ({ id, key, text, workspace }) => {
    const response = await fetch(`/api/v1/sessions/${encodeURIComponent(id)}/runs`, {
      body: JSON.stringify({
        clientRequestId: key,
        message: { fileIds: [], text },
        workspaceId: workspace,
      }),
      headers: {
        Accept: "application/json",
        "Content-Type": "application/json",
        "Idempotency-Key": key,
      },
      method: "POST",
    });
    const rawBody = await response.text();
    if (response.status !== 202) {
      throw new Error(`Background cleanup Run returned HTTP ${response.status}: ${rawBody}`);
    }
    const value = JSON.parse(rawBody) as { run?: { id?: string } };
    if (typeof value.run?.id !== "string") {
      throw new Error("Background cleanup Run response did not contain a Run ID.");
    }
    return value.run.id;
  }, {
    id: sessionId,
    key: `e2e-background-cleanup-${Date.now().toString(36)}`,
    text: prompt,
    workspace: workspaceId,
  });
}

function workspaceTarget(workspaceRoot: string, relativePath: string): string {
  if (!isAbsolute(workspaceRoot)) {
    throw new Error("SYNTHCHAT_E2E_TERMINAL_WORKSPACE must be absolute.");
  }
  const root = resolve(workspaceRoot);
  const target = resolve(root, ...relativePath.split("/"));
  const contained = relative(root, target);
  if (!contained || isAbsolute(contained) || contained === ".." || contained.startsWith(`..${sep}`)) {
    throw new Error("SYNTHCHAT_E2E_TERMINAL_RELATIVE_PATH must stay inside the Workspace.");
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
        "Idempotency-Key": `e2e-terminal-workspace-${id}`,
      },
      method: "POST",
    });
    return { status: registration.status, text: await registration.text() };
  }, { id: profileId, path: workspaceRoot });
  expect(response.status, response.text).toBe(201);
  expect(response.text).not.toContain(workspaceRoot);
  const value = JSON.parse(response.text) as { id: string };
  expect(value.id).toMatch(/^workspace_[A-Za-z0-9_]+$/u);
  return value.id;
}

async function enableTerminalToolset(page: Page, profileId: string): Promise<void> {
  await page.getByRole("button", { name: "工具 / MCP", exact: true }).click();
  await expect(page.getByRole("heading", { name: "工具集", exact: true })).toBeVisible();
  const profile = page.getByRole("combobox", { name: "工具 Profile" });
  await profile.selectOption(profileId);
  await expect(profile).toHaveValue(profileId);
  await expect(page.getByText("正在加载工具列表")).toHaveCount(0);
  const terminalToolset = page.getByRole("switch", {
    name: "启用 Terminal & Processes (terminal)",
  });
  const updateResponse = page.waitForResponse((response) => responseMatches(
    response,
    "PATCH",
    `/api/v1/profiles/${profileId}/toolsets/terminal`,
  ));
  await terminalToolset.click();
  expect((await updateResponse).status()).toBe(200);
  await expect(page.getByRole("switch", {
    name: "停用 Terminal & Processes (terminal)",
  })).toBeChecked();
}

test("approves one foreground Rust terminal command and keeps output private", async ({
  page,
  baseURL,
}) => {
  const profileId = requiredEnvironment("SYNTHCHAT_E2E_TERMINAL_PROFILE_ID");
  const profileName = requiredEnvironment("SYNTHCHAT_E2E_TERMINAL_PROFILE_NAME");
  const providerBaseURL = requiredEnvironment("SYNTHCHAT_E2E_PROVIDER_BASE_URL");
  const model = requiredEnvironment("SYNTHCHAT_E2E_MODEL");
  const prompt = requiredEnvironment("SYNTHCHAT_E2E_TERMINAL_PROMPT");
  const command = requiredEnvironment("SYNTHCHAT_E2E_TERMINAL_COMMAND");
  const finalReply = requiredEnvironment("SYNTHCHAT_E2E_TERMINAL_REPLY");
  const workspaceRoot = requiredEnvironment("SYNTHCHAT_E2E_TERMINAL_WORKSPACE");
  const relativePath = requiredEnvironment("SYNTHCHAT_E2E_TERMINAL_RELATIVE_PATH");
  const privateOutput = requiredEnvironment("SYNTHCHAT_E2E_TERMINAL_PRIVATE_OUTPUT");
  const privateFile = requiredEnvironment("SYNTHCHAT_E2E_TERMINAL_PRIVATE_FILE");
  const providerControlCapability = requiredEnvironment(
    "SYNTHCHAT_E2E_PROVIDER_CONTROL_CAPABILITY",
  );
  const backendControlCapability = requiredEnvironment("SYNTHCHAT_E2E_CONTROL_CAPABILITY");
  const browserOrigin = new URL(requiredEnvironment("SYNTHCHAT_E2E_BASE_URL")).origin;
  const targetPath = workspaceTarget(workspaceRoot, relativePath);
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
  const workspaceId = await registerWorkspace(page, profileId, workspaceRoot);
  await enableTerminalToolset(page, profileId);

  let injectedRunRequests = 0;
  await page.route(/\/api\/v1\/sessions\/[^/]+\/runs(?:\?.*)?$/u, async (route) => {
    if (route.request().method() !== "POST") {
      await route.continue();
      return;
    }
    const body = route.request().postDataJSON() as Record<string, unknown>;
    injectedRunRequests += 1;
    await route.continue({ postData: JSON.stringify({ ...body, workspaceId }) });
  });

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
  expect(injectedRunRequests).toBe(1);

  const approvalPanel = page.getByRole("article").filter({
    has: page.getByRole("heading", { name: "需要确认工具调用" }),
  });
  await expect(approvalPanel).toBeVisible();
  await expect(approvalPanel).toContainText("terminal");
  await expect(approvalPanel).toContainText("Run terminal command (foreground)");
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
  const completedTool = page.getByLabel("工具调用").filter({ hasText: "terminal" });
  await expect(completedTool).toContainText("已完成");
  await expect(completedTool).toContainText("Terminal command exited with code 0");
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

test("keeps one background terminal delivery across two Runs and stops it once", async ({
  page,
  baseURL,
}) => {
  const profileId = requiredEnvironment("SYNTHCHAT_E2E_BACKGROUND_TERMINAL_PROFILE_ID");
  const profileName = requiredEnvironment("SYNTHCHAT_E2E_BACKGROUND_TERMINAL_PROFILE_NAME");
  const providerBaseURL = requiredEnvironment("SYNTHCHAT_E2E_PROVIDER_BASE_URL");
  const model = requiredEnvironment("SYNTHCHAT_E2E_MODEL");
  const launchPrompt = requiredEnvironment("SYNTHCHAT_E2E_BACKGROUND_TERMINAL_PROMPT");
  const launchReply = requiredEnvironment("SYNTHCHAT_E2E_BACKGROUND_TERMINAL_REPLY");
  const controlPrompt = requiredEnvironment("SYNTHCHAT_E2E_BACKGROUND_PROCESS_PROMPT");
  const controlReply = requiredEnvironment("SYNTHCHAT_E2E_BACKGROUND_PROCESS_REPLY");
  const terminalCallId = requiredEnvironment("SYNTHCHAT_E2E_BACKGROUND_TERMINAL_CALL_ID");
  const listCallId = requiredEnvironment("SYNTHCHAT_E2E_BACKGROUND_PROCESS_LIST_CALL_ID");
  const killCallId = requiredEnvironment("SYNTHCHAT_E2E_BACKGROUND_PROCESS_KILL_CALL_ID");
  const command = requiredEnvironment("SYNTHCHAT_E2E_BACKGROUND_TERMINAL_COMMAND");
  const privateOutput = requiredEnvironment("SYNTHCHAT_E2E_BACKGROUND_TERMINAL_PRIVATE_OUTPUT");
  const workspaceRoot = requiredEnvironment("SYNTHCHAT_E2E_BACKGROUND_TERMINAL_WORKSPACE");
  const relativePath = requiredEnvironment("SYNTHCHAT_E2E_BACKGROUND_TERMINAL_RELATIVE_PATH");
  const providerControlCapability = requiredEnvironment(
    "SYNTHCHAT_E2E_PROVIDER_CONTROL_CAPABILITY",
  );
  const backendControlCapability = requiredEnvironment("SYNTHCHAT_E2E_CONTROL_CAPABILITY");
  const browserOrigin = new URL(requiredEnvironment("SYNTHCHAT_E2E_BASE_URL")).origin;
  const targetPath = workspaceTarget(workspaceRoot, relativePath);
  const privateValues = [
    privateOutput,
    providerControlCapability,
    backendControlCapability,
  ];
  const observedProtectedRequests: Array<{
    authorization?: string;
    body: string | null;
    method: string;
    origin: string;
    pathname: string;
  }> = [];
  const externalRequests = new Set<string>();
  const consoleMessages: string[] = [];
  const acceptedBodies: string[] = [];
  const approvalBodies: string[] = [];
  let injectedRunRequests = 0;
  let launchRun: AcceptedE2eRun | null = null;
  let controlRun: AcceptedE2eRun | null = null;
  let cleanupRunId: string | null = null;
  let backgroundStopped = false;
  let workspaceId: string | null = null;
  let testFailed = false;

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
      method: request.method(),
      origin: url.origin,
      pathname: url.pathname,
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
  workspaceId = await registerWorkspace(page, profileId, workspaceRoot);
  await enableTerminalToolset(page, profileId);

  await page.route(/\/api\/v1\/sessions\/[^/]+\/runs(?:\?.*)?$/u, async (route) => {
    if (route.request().method() !== "POST") {
      await route.continue();
      return;
    }
    const body = route.request().postDataJSON() as Record<string, unknown>;
    injectedRunRequests += 1;
    await route.continue({ postData: JSON.stringify({ ...body, workspaceId }) });
  });

  await page.getByRole("button", { name: "聊天", exact: true }).click();
  await expect(page.getByLabel("聊天 Profile")).toHaveValue(profileId);
  const sessionResponse = page.waitForResponse((response) => responseMatches(
    response,
    "POST",
    "/api/v1/sessions",
  ));
  await page.getByRole("button", { name: "新建会话" }).click();
  expect((await sessionResponse).status()).toBe(201);
  const providerBaseline = await providerRequestCount();

  try {
    launchRun = await sendChatRun(page, launchPrompt);
    acceptedBodies.push(launchRun.rawBody);
    expect(injectedRunRequests).toBe(1);
    await expect.poll(providerRequestCount).toBe(providerBaseline + 1);
    await expect(page.getByRole("heading", { name: "需要确认工具调用" })).toBeVisible();
    expect(await fileExists(targetPath)).toBe(false);

    approvalBodies.push(await approvePendingTool(
      page,
      launchRun.run.id,
      "terminal",
      "Run terminal command (background)",
      [privateOutput, providerControlCapability, backendControlCapability],
    ));
    await expect(page.getByText(launchReply, { exact: true })).toBeVisible({ timeout: 30_000 });
    await expect.poll(providerRequestCount).toBe(providerBaseline + 2);
    await expect.poll(async () => fileExists(targetPath), { timeout: 30_000 }).toBe(true);
    expect(await readFile(targetPath, "utf8")).toBe("started");
    const backgroundTool = page.getByLabel("工具调用").filter({
      hasText: "Background process started",
    });
    await expect(backgroundTool).toHaveCount(1);
    await expect(backgroundTool).toContainText("terminal");
    await expect(backgroundTool).toContainText("已完成");
    const pendingDelivery = page.locator(".chat-async-delivery").filter({
      hasText: "等待完成通知",
    });
    await expect(pendingDelivery).toHaveCount(1);
    await expect(pendingDelivery).toContainText("后台终端任务");
    await expect(page.locator(".chat-run-status")).toHaveText(/就绪/u);
    await expect(page.getByRole("textbox", { name: "消息", exact: true })).toBeEnabled();

    controlRun = await sendChatRun(page, controlPrompt);
    acceptedBodies.push(controlRun.rawBody);
    expect(controlRun.run.id).not.toBe(launchRun.run.id);
    expect(controlRun.run.sessionId).toBe(launchRun.run.sessionId);
    expect(injectedRunRequests).toBe(2);
    await expect.poll(providerRequestCount).toBe(providerBaseline + 4);
    await expect(pendingDelivery).toHaveCount(1);

    approvalBodies.push(await approvePendingTool(
      page,
      controlRun.run.id,
      "process",
      "Process kill",
      [privateOutput, providerControlCapability, backendControlCapability],
    ));
    await expect(page.getByText(controlReply, { exact: true })).toBeVisible({ timeout: 30_000 });
    await expect.poll(providerRequestCount).toBe(providerBaseline + 5);
    await expect(page.locator(".chat-run-status")).toHaveText(/就绪/u);
    await expect(pendingDelivery).toHaveCount(0);
    const stoppedDelivery = page.locator(".chat-async-delivery").filter({
      hasText: "后台任务已停止",
    });
    await expect(stoppedDelivery).toHaveCount(1);
    await expect(stoppedDelivery).toContainText("后台终端任务");
    backgroundStopped = true;

    const publicSurface = await readPublicSurface(page, launchRun, controlRun);
    const eventsAFirst = parseSseReplay(
      publicSurface.eventsAFirst,
      launchRun.run.id,
      launchRun.run.sessionId,
    );
    const eventsASecond = parseSseReplay(
      publicSurface.eventsASecond,
      launchRun.run.id,
      launchRun.run.sessionId,
    );
    const eventsB = parseSseReplay(
      publicSurface.eventsB,
      controlRun.run.id,
      controlRun.run.sessionId,
    );
    expect(eventsASecond).toEqual(eventsAFirst);

    const terminalCompletions = eventsAFirst.filter((event) => (
      event.event === "tool.completed" && event.data.callId === terminalCallId
    ));
    expect(terminalCompletions).toHaveLength(1);
    expect(terminalCompletions[0]?.data).toMatchObject({
      artifacts: [],
      asyncDeliveryPending: true,
      callId: terminalCallId,
      resultSummary: "Background process started",
    });
    expect(eventsAFirst.find((event) => (
      event.event === "tool.started" && event.data.callId === terminalCallId
    ))?.data.inputSummary).toBe("Run terminal command (background)");
    const runACompletionIndex = eventsAFirst.findIndex((event) => event.event === "run.completed");
    expect(runACompletionIndex).toBeGreaterThanOrEqual(0);
    const deliveries = eventsAFirst.filter((event) => event.event === "tool.delivery");
    expect(deliveries).toHaveLength(1);
    const delivery = deliveries[0];
    expect(delivery).toBeDefined();
    expect(eventsAFirst.at(-1)).toEqual(delivery);
    expect(eventsAFirst.indexOf(delivery!)).toBe(runACompletionIndex + 1);
    expect(delivery!.sequence).toBe(eventsAFirst[runACompletionIndex]!.sequence + 1);
    expect(delivery!.data).toMatchObject({
      callId: terminalCallId,
      delivery: "completion",
      status: "killed",
    });
    expect(delivery!.data.processId).toMatch(/^process_[0-9a-f]{32}$/u);
    expect(Object.keys(delivery!.data).every((key) => (
      ["callId", "delivery", "exitCode", "processId", "status"].includes(key)
    ))).toBe(true);
    expect(eventsAFirst.filter((event) => event.event === "approval.required")).toHaveLength(1);
    expect(eventsAFirst.find((event) => event.event === "approval.required")?.data).toMatchObject({
      callId: terminalCallId,
      inputSummary: "Run terminal command (background)",
      toolName: "terminal",
    });

    expect(eventsB.filter((event) => event.event === "tool.delivery")).toHaveLength(0);
    expect(eventsB.filter((event) => (
      event.event === "tool.completed" && event.data.asyncDeliveryPending === true
    ))).toHaveLength(0);
    expect(eventsB.filter((event) => event.event === "approval.required")).toHaveLength(1);
    expect(eventsB.find((event) => event.event === "approval.required")?.data).toMatchObject({
      callId: killCallId,
      inputSummary: "Process kill",
      toolName: "process",
    });
    expect(eventsB.find((event) => (
      event.event === "tool.completed" && event.data.callId === listCallId
    ))?.data.resultSummary).toBe("Listed 1 background processes");
    expect(eventsB.find((event) => (
      event.event === "tool.started" && event.data.callId === listCallId
    ))?.data.inputSummary).toBe("Process list");
    expect(eventsB.find((event) => (
      event.event === "tool.completed" && event.data.callId === killCallId
    ))?.data.resultSummary).toBe("Process kill returned killed");
    expect(eventsB.find((event) => (
      event.event === "tool.started" && event.data.callId === killCallId
    ))?.data.inputSummary).toBe("Process kill");

    const publicRunA: unknown = JSON.parse(publicSurface.runA);
    const publicRunB: unknown = JSON.parse(publicSurface.runB);
    expect(publicRunA).toMatchObject({
      id: launchRun.run.id,
      lastSequence: delivery!.sequence,
      status: "completed",
    });
    expect(publicRunB).toMatchObject({
      id: controlRun.run.id,
      lastSequence: eventsB.at(-1)?.sequence,
      status: "completed",
    });

    const approvalRequests = observedProtectedRequests.filter((request) => (
      request.method === "POST"
      && /^\/api\/v1\/runs\/[^/]+\/approvals\/approval_[0-9a-f]{32}$/u.test(request.pathname)
    ));
    expect(approvalRequests).toHaveLength(2);
    expect(approvalRequests[0]?.pathname.startsWith(
      `/api/v1/runs/${encodeURIComponent(launchRun.run.id)}/approvals/`,
    )).toBe(true);
    expect(approvalRequests[1]?.pathname.startsWith(
      `/api/v1/runs/${encodeURIComponent(controlRun.run.id)}/approvals/`,
    )).toBe(true);
    for (const request of approvalRequests) {
      expect(JSON.parse(request.body ?? "null")).toEqual({ decision: "once", reason: null });
    }

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
    const publicState = Object.values(publicSurface).join("\n");
    const acceptedState = acceptedBodies.join("\n");
    const approvalState = approvalBodies.join("\n");
    for (const value of privateValues) {
      expect(visibleText).not.toContain(value);
      expect(renderedMarkup).not.toContain(value);
      expect(browserStorage).not.toContain(value);
      expect(acceptedState).not.toContain(value);
      expect(approvalState).not.toContain(value);
      expect(publicState).not.toContain(value);
      expect(protectedRequestBodies).not.toContain(value);
      expect(consoleMessages.join("\n")).not.toContain(value);
    }
    expect(visibleText).not.toContain(command);
    expect(renderedMarkup).not.toContain(command);
    expect(browserStorage).not.toContain(command);
    expect(acceptedState).not.toContain(command);
    expect(approvalState).not.toContain(command);
    expect(publicState).not.toContain(command);
    expect(protectedRequestBodies).not.toContain(command);
    expect(consoleMessages.join("\n")).not.toContain(command);
    expect(externalRequests).toEqual(new Set());
    expect(observedProtectedRequests.length).toBeGreaterThan(0);
    expect(observedProtectedRequests.every((request) => request.origin === browserOrigin)).toBe(true);
    expect(observedProtectedRequests.every((request) => request.authorization === undefined)).toBe(true);
  } catch (error) {
    testFailed = true;
    throw error;
  } finally {
    const cleanupErrors: unknown[] = [];
    let stopped = backgroundStopped;
    if (!stopped && launchRun) {
      try {
        stopped = await terminalDeliveryObserved(page, launchRun, terminalCallId, 500);
      } catch (error) {
        cleanupErrors.push(error);
      }
    }
    if (!stopped && controlRun) {
      try {
        await finishBackgroundControlRun(page, controlRun.run.id);
        stopped = await terminalDeliveryObserved(page, launchRun!, terminalCallId, 5_000);
      } catch (error) {
        cleanupErrors.push(error);
      }
    }
    if (!stopped && launchRun && workspaceId) {
      if (controlRun) {
        try {
          await cancelRunIfActive(page, controlRun.run.id);
        } catch (error) {
          cleanupErrors.push(error);
        }
      }
      try {
        await cancelRunIfActive(page, launchRun.run.id);
      } catch (error) {
        cleanupErrors.push(error);
      }
      try {
        cleanupRunId = await createBackgroundCleanupRun(
          page,
          launchRun.run.sessionId,
          workspaceId,
          controlPrompt,
        );
      } catch (error) {
        cleanupErrors.push(error);
      }
      if (cleanupRunId) {
        try {
          await finishBackgroundControlRun(page, cleanupRunId);
          stopped = await terminalDeliveryObserved(page, launchRun, terminalCallId, 5_000);
        } catch (error) {
          cleanupErrors.push(error);
        }
      }
      if (!stopped) {
        cleanupErrors.push(new Error("Background terminal cleanup could not confirm process termination."));
      }
    }
    for (const runId of [cleanupRunId, controlRun?.run.id, launchRun?.run.id]) {
      if (!runId) continue;
      try {
        await cancelRunIfActive(page, runId);
      } catch (error) {
        cleanupErrors.push(error);
      }
    }
    try {
      await rm(targetPath, { force: true });
    } catch (error) {
      cleanupErrors.push(error);
    }
    if (cleanupErrors.length > 0) {
      const cleanupError = new AggregateError(
        cleanupErrors,
        "Background terminal E2E cleanup did not complete cleanly.",
      );
      if (!testFailed) throw cleanupError;
      console.error(cleanupError);
    }
  }
});
