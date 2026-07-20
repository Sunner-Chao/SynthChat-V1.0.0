import { expect, test, type Page, type Response } from "@playwright/test";

interface BackendControlSnapshot {
  generation: number;
  state: "offline" | "online" | "starting" | "stopping";
}

interface ProviderControlSnapshot {
  requestCount: number;
  state: "armed" | "holding" | "idle";
}

function requiredEnvironment(name: string): string {
  const value = process.env[name]?.trim();
  if (!value) throw new Error(`${name} is required.`);
  return value;
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

function responseMatches(
  response: Response,
  method: string,
  pathname: string | RegExp,
): boolean {
  const responsePath = new URL(response.url()).pathname;
  return response.request().method() === method
    && (typeof pathname === "string" ? responsePath === pathname : pathname.test(responsePath));
}

function parseBackendControl(value: unknown): BackendControlSnapshot {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("Backend control response must be an object.");
  }
  const record = value as Record<string, unknown>;
  if (
    Object.keys(record).sort().join(",") !== "generation,state"
    || !Number.isSafeInteger(record.generation)
    || (record.generation as number) < 1
    || !["offline", "online", "starting", "stopping"].includes(String(record.state))
  ) {
    throw new Error("Backend control response exposed an unexpected field or invalid state.");
  }
  return record as unknown as BackendControlSnapshot;
}

function parseProviderControl(value: unknown): ProviderControlSnapshot {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("Provider control response must be an object.");
  }
  const record = value as Record<string, unknown>;
  if (
    Object.keys(record).sort().join(",") !== "requestCount,state"
    || !Number.isSafeInteger(record.requestCount)
    || (record.requestCount as number) < 0
    || !["armed", "holding", "idle"].includes(String(record.state))
  ) {
    throw new Error("Provider control response exposed an unexpected field or invalid state.");
  }
  return record as unknown as ProviderControlSnapshot;
}

async function backendControl(
  action: "restart" | "status" | "stop",
): Promise<BackendControlSnapshot> {
  const response = await fetch(
    `${checkedLoopbackOrigin("SYNTHCHAT_E2E_CONTROL_URL")}/${action}`,
    {
      cache: "no-store",
      headers: {
        Accept: "application/json",
        "X-SynthChat-E2E-Control": requiredEnvironment(
          "SYNTHCHAT_E2E_CONTROL_CAPABILITY",
        ),
      },
      method: action === "status" ? "GET" : "POST",
      redirect: "error",
    },
  );
  expect(response.status).toBe(200);
  return parseBackendControl(await response.json());
}

async function providerControl(
  action: "arm" | "release" | "status",
): Promise<ProviderControlSnapshot> {
  const response = await fetch(
    `${checkedLoopbackOrigin("SYNTHCHAT_E2E_PROVIDER_CONTROL_URL")}/${action}`,
    {
      cache: "no-store",
      headers: {
        Accept: "application/json",
        "X-SynthChat-E2E-Provider-Control": requiredEnvironment(
          "SYNTHCHAT_E2E_PROVIDER_CONTROL_CAPABILITY",
        ),
      },
      method: action === "status" ? "GET" : "POST",
      redirect: "error",
    },
  );
  expect(response.status).toBe(200);
  return parseProviderControl(await response.json());
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
  await expect(page.getByRole("button", { name: `${profileName} (${profileId})` }))
    .toHaveAttribute("aria-current", "true");

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
  await expect(modelSection.getByRole("button", { name: "保存配置" })).toBeDisabled();

  const activationResponse = page.waitForResponse((response) => responseMatches(
    response,
    "PUT",
    `/api/v1/profiles/${profileId}/active`,
  ));
  await page.getByRole("button", { name: "设为活动" }).click();
  expect((await activationResponse).status()).toBe(200);
  await expect(page.getByRole("button", { name: "当前活动" })).toBeDisabled();
}

async function sendRun(page: Page, prompt: string): Promise<string> {
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
  const acceptedResponse = await runResponse;
  expect(acceptedResponse.status()).toBe(202);
  expect((await streamResponse).status()).toBe(200);
  const accepted = await acceptedResponse.json() as { run?: { id?: unknown } };
  if (typeof accepted.run?.id !== "string") {
    throw new Error("Run accepted response did not contain a Run ID.");
  }
  return accepted.run.id;
}

test("terminalizes an in-flight Run after backend crash and unlocks the composer", async ({
  page,
  baseURL,
}) => {
  const profileId = requiredEnvironment("SYNTHCHAT_E2E_INFLIGHT_PROFILE_ID");
  const profileName = requiredEnvironment("SYNTHCHAT_E2E_INFLIGHT_PROFILE_NAME");
  const providerBaseURL = requiredEnvironment("SYNTHCHAT_E2E_PROVIDER_BASE_URL");
  const model = requiredEnvironment("SYNTHCHAT_E2E_MODEL");
  const interruptedPrompt = requiredEnvironment("SYNTHCHAT_E2E_INFLIGHT_PROMPT");
  const recoveryPrompt = requiredEnvironment("SYNTHCHAT_E2E_INFLIGHT_RECOVERY_PROMPT");
  const reply = requiredEnvironment("SYNTHCHAT_E2E_REPLY");
  const partialReply = reply.slice(0, Math.max(1, Math.floor(reply.length / 2)));
  const browserOrigins = new Set<string>();
  let runCreationRequests = 0;

  page.on("request", (request) => {
    const url = new URL(request.url());
    if (["http:", "https:"].includes(url.protocol)) browserOrigins.add(url.origin);
    if (
      request.method() === "POST"
      && /\/api\/v1\/sessions\/[^/]+\/runs$/u.test(url.pathname)
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
      chat: {
        reconnectInitialDelayMs: 50,
        reconnectMaxAttempts: 100,
        reconnectMaxDelayMs: 250,
        runStatusPollIntervalMs: 500,
      },
    };
  });
  await page.goto(baseURL || "/");
  const browserOrigin = new URL(page.url()).origin;
  await expect(page.getByRole("button", { name: /后端在线/u })).toBeVisible();
  const initialBackend = await backendControl("status");
  expect(initialBackend.state).toBe("online");

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

  const providerBefore = await providerControl("status");
  expect(providerBefore.state).toBe("idle");
  expect(await providerControl("arm")).toEqual({
    requestCount: providerBefore.requestCount,
    state: "armed",
  });
  const interruptedRunId = await sendRun(page, interruptedPrompt);
  await expect.poll(async () => providerControl("status")).toEqual({
    requestCount: providerBefore.requestCount + 1,
    state: "holding",
  });
  await expect(page.locator(".chat-run-status")).toContainText("正在生成");
  await expect(page.getByText(interruptedPrompt, { exact: true })).toBeVisible();
  await expect(page.getByText(partialReply, { exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "停止生成" })).toBeVisible();
  await expect(page.getByRole("button", { name: "发送消息" })).toHaveCount(0);
  expect(runCreationRequests).toBe(1);

  const pageUrlBeforeCrash = page.url();
  expect(await backendControl("stop")).toEqual({
    generation: initialBackend.generation,
    state: "offline",
  });
  await expect(page.getByRole("button", { name: /后端未连接/u })).toBeVisible();
  await expect(page.locator(".chat-run-status")).toContainText(/正在生成|正在重连/u);
  await expect(page.getByText(interruptedPrompt, { exact: true })).toBeVisible();
  expect(page.url()).toBe(pageUrlBeforeCrash);
  expect(runCreationRequests).toBe(1);
  await expect.poll(async () => providerControl("status")).toEqual({
    requestCount: providerBefore.requestCount + 1,
    state: "idle",
  });

  const restarted = await backendControl("restart");
  expect(restarted).toEqual({
    generation: initialBackend.generation + 1,
    state: "online",
  });
  await expect(page.getByRole("button", { name: /后端在线/u })).toBeVisible();
  expect(page.url()).toBe(pageUrlBeforeCrash);

  await expect.poll(async () => page.evaluate(async (runId) => {
    const response = await fetch(`/api/v1/runs/${runId}`, {
      cache: "no-store",
      headers: { Accept: "application/json" },
    });
    return { body: await response.json() as unknown, status: response.status };
  }, interruptedRunId)).toMatchObject({
    body: {
      error: { title: "Run interrupted" },
      status: "failed",
    },
    status: 200,
  });
  await expect(page.getByRole("textbox", { name: "消息", exact: true })).toBeEnabled();
  await expect.soft(
    page.getByRole("alert").filter({
      hasText: /Run interrupted|backend restarted|后端.*重启|对话.*中断/iu,
    }),
    "The terminal backend-restart failure must be visible without reloading the page.",
  ).toBeVisible({ timeout: 3_000 });
  await expect(page.getByRole("button", { name: "停止生成" })).toHaveCount(0);
  await expect(page.getByText(interruptedPrompt, { exact: true })).toBeVisible();

  await page.getByRole("textbox", { name: "消息", exact: true }).fill(recoveryPrompt);
  await expect(page.getByRole("button", { name: "发送消息" })).toBeEnabled();
  await sendRun(page, recoveryPrompt);
  await expect(page.getByText(recoveryPrompt, { exact: true })).toBeVisible();
  await expect(page.getByText(reply, { exact: true })).toBeVisible();
  await expect(page.locator(".chat-run-status")).toContainText("就绪");
  expect(runCreationRequests).toBe(2);
  expect(await providerControl("status")).toEqual({
    requestCount: providerBefore.requestCount + 2,
    state: "idle",
  });
  expect(browserOrigins).toEqual(new Set([browserOrigin]));
  expect(await backendControl("status")).toEqual(restarted);
});
