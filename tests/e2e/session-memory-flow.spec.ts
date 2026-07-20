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

async function createConfiguredProfile(
  page: Page,
  profileId: string,
  profileName: string,
  providerBaseUrl: string,
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

  const profileButton = page.getByRole("button", { name: `${profileName} (${profileId})` });
  await expect(profileButton).toHaveAttribute("aria-current", "true");
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
  await baseUrlField.getByRole("textbox").fill(providerBaseUrl);
  const saveResponse = page.waitForResponse((response) => responseMatches(
    response,
    "PATCH",
    `/api/v1/profiles/${profileId}/config`,
  ));
  await modelSection.getByRole("button", { name: "保存配置" }).click();
  expect((await saveResponse).status()).toBe(200);

  const activateResponse = page.waitForResponse((response) => responseMatches(
    response,
    "PUT",
    `/api/v1/profiles/${profileId}/active`,
  ));
  await page.getByRole("button", { name: "设为活动" }).click();
  expect((await activateResponse).status()).toBe(200);
  await expect(page.getByRole("button", { name: "当前活动" })).toBeDisabled();
}

async function sendMessage(
  page: Page,
  prompt: string,
  reply: string,
  expectedReplyCount: number,
): Promise<void> {
  const runResponse = page.waitForResponse((response) => responseMatches(
    response,
    "POST",
    /^\/api\/v1\/sessions\/[^/]+\/runs$/u,
  ));
  const streamResponse = page.waitForResponse((response) => responseMatches(
    response,
    "GET",
    /^\/api\/v1\/runs\/[^/]+\/events$/u,
  ));
  await page.getByRole("textbox", { name: "消息", exact: true }).fill(prompt);
  await page.getByRole("button", { name: "发送消息" }).click();
  expect((await runResponse).status()).toBe(202);
  expect((await streamResponse).status()).toBe(200);
  await expect(page.getByText(prompt, { exact: true })).toBeVisible();
  await expect(page.getByText(reply, { exact: true })).toHaveCount(expectedReplyCount);
  await expect(page.getByRole("textbox", { name: "消息", exact: true })).toBeEnabled();
}

test("searches Session message history, continues it, and manages builtin Memory", async ({
  page,
  baseURL,
}) => {
  const profileId = requiredEnvironment("SYNTHCHAT_E2E_HISTORY_MEMORY_PROFILE_ID");
  const profileName = requiredEnvironment("SYNTHCHAT_E2E_HISTORY_MEMORY_PROFILE_NAME");
  const providerBaseUrl = requiredEnvironment("SYNTHCHAT_E2E_PROVIDER_BASE_URL");
  const model = requiredEnvironment("SYNTHCHAT_E2E_MODEL");
  const historySearchTerm = requiredEnvironment("SYNTHCHAT_E2E_HISTORY_SEARCH_TERM");
  const historyPrompt = requiredEnvironment("SYNTHCHAT_E2E_HISTORY_PROMPT");
  const continuationPrompt = requiredEnvironment("SYNTHCHAT_E2E_HISTORY_CONTINUATION_PROMPT");
  const reply = requiredEnvironment("SYNTHCHAT_E2E_REPLY");
  const memorySearchTerm = requiredEnvironment("SYNTHCHAT_E2E_MEMORY_SEARCH_TERM");
  const memoryContent = requiredEnvironment("SYNTHCHAT_E2E_MEMORY_CONTENT");
  const memoryUpdatedContent = requiredEnvironment("SYNTHCHAT_E2E_MEMORY_UPDATED_CONTENT");
  expect(historyPrompt).toContain(historySearchTerm);
  expect(memoryContent).toContain(memorySearchTerm);
  expect(memoryUpdatedContent).toContain(memorySearchTerm);

  const browserOrigin = new URL(requiredEnvironment("SYNTHCHAT_E2E_BASE_URL")).origin;
  const externalRequests = new Set<string>();
  const observedProtectedRequests: Array<{ authorization?: string; origin: string }> = [];
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
  await createConfiguredProfile(page, profileId, profileName, providerBaseUrl, model);

  await page.getByRole("button", { name: "聊天", exact: true }).click();
  await expect(page.getByLabel("聊天 Profile")).toHaveValue(profileId);
  const createSessionResponse = page.waitForResponse((response) => responseMatches(
    response,
    "POST",
    "/api/v1/sessions",
  ));
  await page.getByRole("button", { name: "新建会话" }).click();
  expect((await createSessionResponse).status()).toBe(201);
  const currentSession = page.getByLabel("当前会话");
  await expect(currentSession).not.toHaveValue("");
  const sessionId = await currentSession.inputValue();
  expect(sessionId).not.toBe("");
  await sendMessage(page, historyPrompt, reply, 1);

  await page.getByRole("button", { name: "会话", exact: true }).click();
  await expect(page.getByRole("heading", { name: "会话", exact: true })).toBeVisible();
  await expect(page.getByLabel("按 Profile 筛选")).toHaveValue(profileId);
  const searchResponse = page.waitForResponse((response) => {
    const url = new URL(response.url());
    return response.request().method() === "GET"
      && url.pathname === "/api/v1/sessions"
      && url.searchParams.get("profileId") === profileId
      && url.searchParams.get("q") === historySearchTerm;
  });
  await page.getByRole("search").getByLabel("搜索会话").fill(historySearchTerm);
  await page.getByRole("button", { name: "执行搜索" }).click();
  expect((await searchResponse).status()).toBe(200);

  const sessionList = page.getByLabel("会话列表");
  const searchResult = sessionList.getByRole("button").filter({ hasText: historySearchTerm });
  await expect(searchResult).toHaveCount(1);
  await searchResult.click();
  const sessionDetail = page.getByLabel("会话详情");
  await expect(sessionDetail.getByText(historyPrompt, { exact: true })).toBeVisible();
  const detailTitle = sessionDetail.getByRole("textbox", { name: "会话标题", exact: true });
  await expect(detailTitle).toBeVisible();
  expect((await detailTitle.inputValue()).toLocaleLowerCase())
    .not.toContain(historySearchTerm.toLocaleLowerCase());

  await sessionDetail.getByRole("button", { name: "继续对话" }).click();
  await expect(page.getByRole("heading", { name: "聊天", exact: true })).toBeVisible();
  await expect(page.getByLabel("当前会话")).toHaveValue(sessionId);
  await expect(page.getByText(historyPrompt, { exact: true })).toBeVisible();
  await sendMessage(page, continuationPrompt, reply, 2);

  await page.getByRole("button", { name: "记忆", exact: true }).click();
  await expect(page.getByRole("heading", { name: "记忆", exact: true })).toBeVisible();
  await expect(page.getByLabel("记忆 Profile")).toHaveValue(profileId);
  await expect(page.getByText("正在加载记忆", { exact: true })).toHaveCount(0);

  await page.getByRole("button", { name: "新增", exact: true }).click();
  await page.getByLabel("新增记忆内容").fill(memoryContent);
  const createMemoryResponse = page.waitForResponse((response) => responseMatches(
    response,
    "POST",
    `/api/v1/profiles/${profileId}/memories`,
  ));
  await page.getByRole("button", { name: "添加", exact: true }).click();
  const createdMemoryResponse = await createMemoryResponse;
  expect(createdMemoryResponse.status()).toBe(201);
  const createdMemory = await createdMemoryResponse.json() as Record<string, unknown>;
  expect(typeof createdMemory.id).toBe("string");
  const memoryId = createdMemory.id as string;
  await expect(page.getByText(memoryContent, { exact: true })).toBeVisible();

  const memorySearch = page.getByRole("search", { name: "记忆搜索" });
  await memorySearch.getByRole("searchbox", { name: "搜索记忆" }).fill(memorySearchTerm);
  const searchMemoryResponse = page.waitForResponse((response) => {
    const url = new URL(response.url());
    return response.request().method() === "GET"
      && url.pathname === `/api/v1/profiles/${profileId}/memories`
      && url.searchParams.get("target") === "memory"
      && url.searchParams.get("q") === memorySearchTerm;
  });
  await memorySearch.getByRole("button", { name: "搜索", exact: true }).click();
  expect((await searchMemoryResponse).status()).toBe(200);
  await expect(page.getByText(memoryContent, { exact: true })).toBeVisible();

  await page.getByRole("button", { name: `编辑记忆 ${memoryId}` }).click();
  await page.getByLabel(`编辑记忆内容 ${memoryId}`).fill(memoryUpdatedContent);
  const updateMemoryResponse = page.waitForResponse((response) => responseMatches(
    response,
    "PATCH",
    `/api/v1/profiles/${profileId}/memories/${encodeURIComponent(memoryId)}`,
  ));
  await page.getByRole("button", { name: "保存", exact: true }).click();
  const updatedMemoryResponse = await updateMemoryResponse;
  expect(updatedMemoryResponse.status()).toBe(200);
  const updatedMemory = await updatedMemoryResponse.json() as Record<string, unknown>;
  expect(typeof updatedMemory.id).toBe("string");
  const updatedMemoryId = updatedMemory.id as string;
  expect(updatedMemoryId).not.toBe(memoryId);
  await expect(page.getByText(memoryUpdatedContent, { exact: true })).toBeVisible();
  await expect(page.getByText(memoryContent, { exact: true })).toHaveCount(0);

  await page.getByRole("button", { name: `删除记忆 ${updatedMemoryId}` }).click();
  const deleteConfirmation = page.getByRole("alert").filter({
    hasText: "确认删除这条记忆？",
  });
  await expect(deleteConfirmation).toBeVisible();
  const deleteMemoryResponse = page.waitForResponse((response) => responseMatches(
    response,
    "DELETE",
    `/api/v1/profiles/${profileId}/memories/${encodeURIComponent(updatedMemoryId)}`,
  ));
  await deleteConfirmation.getByRole("button", { name: "确认删除" }).click();
  expect((await deleteMemoryResponse).status()).toBe(204);
  await expect(page.getByText(memoryUpdatedContent, { exact: true })).toHaveCount(0);
  await expect(page.getByText("没有匹配的记忆。", { exact: true })).toBeVisible();

  expect(externalRequests).toEqual(new Set());
  expect(observedProtectedRequests.length).toBeGreaterThan(0);
  expect(observedProtectedRequests.every((request) => request.origin === browserOrigin)).toBe(true);
  expect(observedProtectedRequests.every((request) => request.authorization === undefined)).toBe(true);
});
