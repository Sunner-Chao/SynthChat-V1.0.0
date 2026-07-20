import { expect, test, type Request } from "@playwright/test";

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

function protectedApiRequest(request: Request): boolean {
  return new URL(request.url()).pathname.startsWith("/api/v1/");
}

test("creates a Profile and Session, then renders Rust Run SSE text and usage", async ({ page, baseURL }) => {
  const profileId = requiredEnvironment("SYNTHCHAT_E2E_PROFILE_ID");
  const profileName = requiredEnvironment("SYNTHCHAT_E2E_PROFILE_NAME");
  const providerBaseURL = requiredEnvironment("SYNTHCHAT_E2E_PROVIDER_BASE_URL");
  const model = requiredEnvironment("SYNTHCHAT_E2E_MODEL");
  const prompt = requiredEnvironment("SYNTHCHAT_E2E_PROMPT");
  const reply = requiredEnvironment("SYNTHCHAT_E2E_REPLY");
  const totalTokens = expectedTokenCount();
  const browserOrigin = new URL(requiredEnvironment("SYNTHCHAT_E2E_BASE_URL")).origin;
  const observedProtectedRequests: Array<{ authorization?: string; origin: string }> = [];

  page.on("request", (request) => {
    if (!protectedApiRequest(request)) return;
    observedProtectedRequests.push({
      authorization: request.headers().authorization,
      origin: new URL(request.url()).origin,
    });
  });

  await page.addInitScript(() => {
    const runtime = globalThis as typeof globalThis & {
      __SYNTHCHAT_BACKEND_URL__?: string;
    };
    runtime.__SYNTHCHAT_BACKEND_URL__ = globalThis.location.origin;
  });
  await page.goto(baseURL || "/");
  await expect(page.getByRole("button", { name: /后端在线/ })).toBeVisible();

  await page.getByRole("button", { name: "设置", exact: true }).click();
  await expect(page.getByRole("heading", { name: "模型配置" })).toBeVisible();

  await page.getByRole("button", { name: "创建 Profile" }).click();
  const createForm = page.locator("form").filter({
    has: page.getByRole("button", { name: "创建", exact: true }),
  });
  await createForm.getByLabel("标识").fill(profileId);
  await createForm.getByLabel("显示名称").fill(profileName);
  const createProfileResponse = page.waitForResponse((response) => (
    new URL(response.url()).pathname === "/api/v1/profiles"
    && response.request().method() === "POST"
  ));
  await createForm.getByRole("button", { name: "创建", exact: true }).click();
  expect((await createProfileResponse).status()).toBe(201);

  const profileButton = page.getByRole("button", { name: `${profileName} (${profileId})` });
  await expect(profileButton).toHaveAttribute("aria-current", "true");
  await expect(page.getByRole("heading", { name: "模型配置" })).toBeVisible();

  const modelSection = page.locator("section").filter({
    has: page.getByRole("heading", { name: "模型配置" }),
  });
  const providerField = modelSection.locator("label").filter({
    has: page.getByText("Provider", { exact: true }),
  });
  const modelField = modelSection.locator("label").filter({
    has: page.getByText("模型", { exact: true }),
  });
  const baseUrlField = modelSection.locator("label").filter({
    has: page.getByText("Base URL", { exact: true }),
  });
  await providerField.getByRole("combobox").selectOption("lmstudio");
  await modelField.getByRole("textbox").fill(model);
  await baseUrlField.getByRole("textbox").fill(providerBaseURL);
  const saveConfigurationResponse = page.waitForResponse((response) => (
    new URL(response.url()).pathname === `/api/v1/profiles/${profileId}/config`
    && response.request().method() === "PATCH"
  ));
  await modelSection.getByRole("button", { name: "保存配置" }).click();
  expect((await saveConfigurationResponse).status()).toBe(200);
  await expect(modelSection.getByRole("button", { name: "保存配置" })).toBeDisabled();

  const activationResponse = page.waitForResponse((response) => (
    new URL(response.url()).pathname === `/api/v1/profiles/${profileId}/active`
    && response.request().method() === "PUT"
  ));
  await page.getByRole("button", { name: "设为活动" }).click();
  expect((await activationResponse).status()).toBe(200);
  await expect(page.getByRole("button", { name: "当前活动" })).toBeDisabled();

  await page.getByRole("button", { name: "聊天", exact: true }).click();
  await expect(page.getByLabel("聊天 Profile")).toHaveValue(profileId);

  const createSessionResponse = page.waitForResponse((response) => (
    new URL(response.url()).pathname === "/api/v1/sessions"
    && response.request().method() === "POST"
  ));
  await page.getByRole("button", { name: "新建会话" }).click();
  expect((await createSessionResponse).status()).toBe(201);
  await expect(page.getByLabel("当前会话")).not.toHaveValue("");

  const runResponse = page.waitForResponse((response) => (
    /\/api\/v1\/sessions\/[^/]+\/runs$/u.test(new URL(response.url()).pathname)
    && response.request().method() === "POST"
  ));
  const streamResponse = page.waitForResponse((response) => (
    /\/api\/v1\/runs\/[^/]+\/events$/u.test(new URL(response.url()).pathname)
    && response.request().method() === "GET"
  ));
  await page.getByRole("textbox", { name: "消息", exact: true }).fill(prompt);
  await page.getByRole("button", { name: "发送消息" }).click();
  expect((await runResponse).status()).toBe(202);
  expect((await streamResponse).status()).toBe(200);

  await expect(page.getByText(reply, { exact: true })).toBeVisible();
  const composerFooter = page.locator("footer").filter({
    has: page.getByRole("button", { name: "发送消息" }),
  });
  await expect(composerFooter.getByText(
    `${totalTokens.toLocaleString("zh-CN")} tokens`,
    { exact: true },
  )).toBeVisible();
  await expect(page.getByText(prompt, { exact: true })).toBeVisible();

  expect(observedProtectedRequests.length).toBeGreaterThan(0);
  expect(observedProtectedRequests.every((request) => request.origin === browserOrigin)).toBe(true);
  expect(observedProtectedRequests.every((request) => request.authorization === undefined)).toBe(true);
});
