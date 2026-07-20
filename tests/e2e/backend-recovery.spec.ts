import { expect, test, type Page, type Response } from "@playwright/test";

type ControlState = "offline" | "online" | "starting" | "stopping";

interface ControlSnapshot {
  generation: number;
  state: ControlState;
}

function requiredEnvironment(name: string): string {
  const value = process.env[name]?.trim();
  if (!value) throw new Error(`${name} is required.`);
  return value;
}

function expectedTokenCount(): number {
  const value = Number(requiredEnvironment("SYNTHCHAT_E2E_TOTAL_TOKENS"));
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new Error("SYNTHCHAT_E2E_TOTAL_TOKENS must be a positive integer.");
  }
  return value;
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

function checkedControlOrigin(): string {
  const url = new URL(requiredEnvironment("SYNTHCHAT_E2E_CONTROL_URL"));
  if (
    url.protocol !== "http:"
    || !["127.0.0.1", "::1", "localhost"].includes(url.hostname)
    || url.username
    || url.password
    || url.pathname !== "/"
    || url.search
    || url.hash
  ) {
    throw new Error("SYNTHCHAT_E2E_CONTROL_URL must be a loopback HTTP origin.");
  }
  return url.origin;
}

function parseControlSnapshot(value: unknown): ControlSnapshot {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("Control response must be an object.");
  }
  const record = value as Record<string, unknown>;
  if (
    Object.keys(record).sort().join(",") !== "generation,state"
    || !Number.isSafeInteger(record.generation)
    || (record.generation as number) < 1
    || !["offline", "online", "starting", "stopping"].includes(String(record.state))
  ) {
    throw new Error("Control response exposed an unexpected field or invalid state.");
  }
  return record as unknown as ControlSnapshot;
}

async function controlRequest(
  action: "restart" | "status" | "stop",
): Promise<ControlSnapshot> {
  const capability = requiredEnvironment("SYNTHCHAT_E2E_CONTROL_CAPABILITY");
  if (!/^[0-9a-f]{64}$/u.test(capability)) {
    throw new Error("SYNTHCHAT_E2E_CONTROL_CAPABILITY is invalid.");
  }
  const response = await fetch(`${checkedControlOrigin()}/${action}`, {
    cache: "no-store",
    headers: {
      Accept: "application/json",
      "X-SynthChat-E2E-Control": capability,
    },
    method: action === "status" ? "GET" : "POST",
    redirect: "error",
  });
  expect(response.status).toBe(200);
  return parseControlSnapshot(await response.json());
}

async function expectBrowserShapedControlRequestRejected(browserOrigin: string): Promise<void> {
  const response = await fetch(`${checkedControlOrigin()}/status`, {
    cache: "no-store",
    headers: {
      Accept: "application/json",
      Origin: browserOrigin,
      "X-SynthChat-E2E-Control": requiredEnvironment("SYNTHCHAT_E2E_CONTROL_CAPABILITY"),
    },
    method: "GET",
    redirect: "error",
  });
  expect(response.status).toBe(403);
  expect(await response.json()).toEqual({ error: "forbidden" });
}

async function createConfiguredProfile(
  page: Page,
  profileId: string,
  profileName: string,
  model: string,
  providerBaseURL: string,
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

  const profileButton = page.getByRole("button", { name: `${profileName} (${profileId})` });
  await expect(profileButton).toHaveAttribute("aria-current", "true");
  const modelSection = page.locator("section").filter({
    has: page.getByRole("heading", { name: "模型配置" }),
  });
  await modelSection.locator("label").filter({
    has: page.getByText("Provider", { exact: true }),
  }).getByRole("combobox").selectOption("lmstudio");
  await modelSection.locator("label").filter({
    has: page.getByText("模型", { exact: true }),
  }).getByRole("textbox").fill(model);
  await modelSection.locator("label").filter({
    has: page.getByText("Base URL", { exact: true }),
  }).getByRole("textbox").fill(providerBaseURL);
  const saveResponse = page.waitForResponse((response) => responseMatches(
    response,
    "PATCH",
    `/api/v1/profiles/${profileId}/config`,
  ));
  await modelSection.getByRole("button", { name: "保存配置" }).click();
  expect((await saveResponse).status()).toBe(200);

  const activationResponse = page.waitForResponse((response) => responseMatches(
    response,
    "PUT",
    `/api/v1/profiles/${profileId}/active`,
  ));
  await page.getByRole("button", { name: "设为活动" }).click();
  expect((await activationResponse).status()).toBe(200);
  await expect(page.getByRole("button", { name: "当前活动" })).toBeDisabled();
}

async function sendRun(page: Page, prompt: string): Promise<void> {
  const runResponse = page.waitForResponse((response) => responseMatches(
    response,
    "POST",
    /\/api\/v1\/sessions\/[^/]+\/runs$/u,
  ));
  const streamResponse = page.waitForResponse((response) => responseMatches(
    response,
    "GET",
    /\/api\/v1\/runs\/[^/]+\/events$/u,
  ));
  await page.getByRole("textbox", { name: "消息", exact: true }).fill(prompt);
  await page.getByRole("button", { name: "发送消息" }).click();
  expect((await runResponse).status()).toBe(202);
  expect((await streamResponse).status()).toBe(200);
}

test("recovers on a rotated backend generation without losing the persisted Session", async ({
  page,
  baseURL,
}) => {
  const profileId = requiredEnvironment("SYNTHCHAT_E2E_RECOVERY_PROFILE_ID");
  const profileName = requiredEnvironment("SYNTHCHAT_E2E_RECOVERY_PROFILE_NAME");
  const providerBaseURL = requiredEnvironment("SYNTHCHAT_E2E_PROVIDER_BASE_URL");
  const model = requiredEnvironment("SYNTHCHAT_E2E_MODEL");
  const firstPrompt = requiredEnvironment("SYNTHCHAT_E2E_PROMPT");
  const secondPrompt = requiredEnvironment("SYNTHCHAT_E2E_RECOVERY_PROMPT");
  const reply = requiredEnvironment("SYNTHCHAT_E2E_REPLY");
  const totalTokens = expectedTokenCount();
  let runCreationRequests = 0;

  page.on("request", (request) => {
    if (
      request.method() === "POST"
      && /\/api\/v1\/sessions\/[^/]+\/runs$/u.test(new URL(request.url()).pathname)
    ) {
      runCreationRequests += 1;
    }
  });
  await page.addInitScript(() => {
    const runtime = globalThis as typeof globalThis & {
      __SYNTHCHAT_BACKEND_URL__?: string;
      __SYNTHCHAT_RUNTIME_CONFIG__?: Record<string, unknown>;
    };
    runtime.__SYNTHCHAT_BACKEND_URL__ = globalThis.location.origin;
    runtime.__SYNTHCHAT_RUNTIME_CONFIG__ = {
      backend: {
        healthTimeoutMs: 500,
        statusPollIntervalMs: 1_000,
      },
    };
  });
  await page.goto(baseURL || "/");
  await expect(page.getByRole("button", { name: /后端在线/u })).toBeVisible();
  await expectBrowserShapedControlRequestRejected(new URL(page.url()).origin);
  const initialControl = await controlRequest("status");
  expect(initialControl.state).toBe("online");

  await createConfiguredProfile(page, profileId, profileName, model, providerBaseURL);
  await page.getByRole("button", { name: "聊天", exact: true }).click();
  await expect(page.getByLabel("聊天 Profile")).toHaveValue(profileId);
  const createSessionResponse = page.waitForResponse((response) => responseMatches(
    response,
    "POST",
    "/api/v1/sessions",
  ));
  await page.getByRole("button", { name: "新建会话" }).click();
  expect((await createSessionResponse).status()).toBe(201);
  await expect(page.getByLabel("当前会话")).not.toHaveValue("");

  await sendRun(page, firstPrompt);
  await expect(page.getByText(firstPrompt, { exact: true })).toBeVisible();
  await expect(page.getByText(reply, { exact: true })).toBeVisible();
  const composerFooter = page.locator("footer").filter({
    has: page.getByRole("button", { name: "发送消息" }),
  });
  await expect(composerFooter.getByText(
    `${totalTokens.toLocaleString("zh-CN")} tokens`,
    { exact: true },
  )).toBeVisible();
  await expect(page.locator(".chat-run-status")).toContainText("就绪");

  const pageUrlBeforeStop = page.url();
  const stopped = await controlRequest("stop");
  expect(stopped).toEqual({ generation: initialControl.generation, state: "offline" });
  await expect(page.getByRole("button", { name: /后端未连接/u })).toBeVisible();
  expect(page.url()).toBe(pageUrlBeforeStop);
  await expect(page.getByText(firstPrompt, { exact: true })).toBeVisible();
  await expect(page.getByText(reply, { exact: true })).toBeVisible();
  expect(await controlRequest("status")).toEqual(stopped);

  const restarted = await controlRequest("restart");
  expect(restarted).toEqual({
    generation: initialControl.generation + 1,
    state: "online",
  });
  await expect(page.getByRole("button", { name: /后端在线/u })).toBeVisible();
  expect(page.url()).toBe(pageUrlBeforeStop);
  await expect(page.getByText(firstPrompt, { exact: true })).toBeVisible();
  await expect(page.getByText(reply, { exact: true })).toBeVisible();

  await page.getByRole("button", { name: "设置", exact: true }).click();
  await expect(page.getByRole("heading", { name: "模型配置" })).toBeVisible();
  await page.getByRole("button", { name: "聊天", exact: true }).click();
  await expect(page.getByLabel("聊天 Profile")).toHaveValue(profileId);
  await expect(page.getByLabel("当前会话")).not.toHaveValue("");
  await expect(page.getByText(firstPrompt, { exact: true })).toBeVisible();
  await expect(page.getByText(reply, { exact: true })).toBeVisible();

  await sendRun(page, secondPrompt);
  await expect(page.getByText(secondPrompt, { exact: true })).toBeVisible();
  await expect(page.getByText(reply, { exact: true })).toHaveCount(2);
  await expect(page.locator(".chat-run-status")).toContainText("就绪");
  expect(runCreationRequests).toBe(2);
  expect(await controlRequest("status")).toEqual(restarted);
});
