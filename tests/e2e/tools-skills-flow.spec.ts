import { access, readFile } from "node:fs/promises";
import {
  expect,
  test,
  type Page,
  type Request,
  type Response,
} from "@playwright/test";

const FIXTURE_SKILL_NAME = "synthchat-e2e-auditable";
const FIXTURE_SKILL_DESCRIPTION = "Auditable local fixture for the Skills UI lifecycle";
const SKILL_ID_PATTERN = "skill_[0-9a-f]{32}";
const OPERATION_ID_PATTERN = /^op_[0-9a-f]{32}$/u;

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

async function createProfile(page: Page, profileId: string, profileName: string): Promise<void> {
  await page.getByRole("button", { name: "设置", exact: true }).click();
  await expect(page.getByRole("heading", { name: "模型配置" })).toBeVisible();
  await page.getByRole("button", { name: "创建 Profile" }).click();
  const form = page.locator("form").filter({
    has: page.getByRole("button", { name: "创建", exact: true }),
  });
  await form.getByLabel("标识").fill(profileId);
  await form.getByLabel("显示名称").fill(profileName);
  const response = page.waitForResponse((candidate) => responseMatches(
    candidate,
    "POST",
    "/api/v1/profiles",
  ));
  await form.getByRole("button", { name: "创建", exact: true }).click();
  expect((await response).status()).toBe(201);
  await expect(page.getByRole("button", { name: `${profileName} (${profileId})` }))
    .toHaveAttribute("aria-current", "true");
}

async function openToolsProfile(page: Page, profileId: string): Promise<void> {
  await page.getByRole("button", { name: "工具 / MCP", exact: true }).click();
  await expect(page.getByRole("heading", { name: "工具集", exact: true })).toBeVisible();
  const profile = page.getByRole("combobox", { name: "工具 Profile" });
  await expect(profile).toBeEnabled();
  await profile.selectOption(profileId);
  await expect(profile).toHaveValue(profileId);
  await expect(page.getByText("正在加载工具列表")).toHaveCount(0);
  await expect(page.getByText("正在加载 Skills")).toHaveCount(0);
}

async function reloadToolsProfile(page: Page, profileId: string): Promise<void> {
  await page.reload();
  await expect(page.getByRole("button", { name: /后端在线/u })).toBeVisible();
  await openToolsProfile(page, profileId);
}

async function completedOperationResponse(
  page: Page,
  kind: "skillInstall" | "skillUninstall",
): Promise<Response> {
  return page.waitForResponse(async (response) => {
    if (!responseMatches(response, "GET", /^\/api\/v1\/operations\/op_[0-9a-f]{32}$/u)) {
      return false;
    }
    if (response.status() !== 200) return false;
    const operation = await response.json() as Record<string, unknown>;
    return operation.kind === kind && operation.status === "completed";
  });
}

async function operationBody(response: Response): Promise<{
  id: string;
  kind: string;
  status: string;
}> {
  const value = await response.json() as Record<string, unknown>;
  expect(value.id).toEqual(expect.stringMatching(OPERATION_ID_PATTERN));
  expect(typeof value.kind).toBe("string");
  expect(typeof value.status).toBe("string");
  return value as { id: string; kind: string; status: string };
}

test("persists Toolset and local Skill lifecycle changes in one isolated Profile", async ({
  page,
  baseURL,
}) => {
  const profileId = requiredEnvironment("SYNTHCHAT_E2E_TOOLS_PROFILE_ID");
  const profileName = requiredEnvironment("SYNTHCHAT_E2E_TOOLS_PROFILE_NAME");
  const skillFixture = requiredEnvironment("SYNTHCHAT_E2E_SKILL_FIXTURE");
  const browserOrigin = new URL(requiredEnvironment("SYNTHCHAT_E2E_BASE_URL")).origin;
  const observedProtectedRequests: Array<{ authorization?: string; origin: string }> = [];
  const externalRequests = new Set<string>();

  await access(skillFixture);
  const fixtureSource = await readFile(skillFixture, "utf8");
  expect(fixtureSource).toContain(`name: ${FIXTURE_SKILL_NAME}`);
  expect(fixtureSource).toContain(`description: ${FIXTURE_SKILL_DESCRIPTION}`);

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
  await createProfile(page, profileId, profileName);
  await openToolsProfile(page, profileId);

  await expect(page.getByRole("switch", { name: "启用 Task Planning (todo)" })).toBeVisible();
  const enableToolsetResponse = page.waitForResponse((response) => responseMatches(
    response,
    "PATCH",
    `/api/v1/profiles/${profileId}/toolsets/todo`,
  ));
  await page.getByRole("switch", { name: "启用 Task Planning (todo)" }).click();
  expect((await enableToolsetResponse).status()).toBe(200);
  await expect(page.getByRole("switch", { name: "停用 Task Planning (todo)" })).toBeChecked();

  await reloadToolsProfile(page, profileId);
  await expect(page.getByRole("switch", { name: "停用 Task Planning (todo)" })).toBeChecked();
  const disableToolsetResponse = page.waitForResponse((response) => responseMatches(
    response,
    "PATCH",
    `/api/v1/profiles/${profileId}/toolsets/todo`,
  ));
  await page.getByRole("switch", { name: "停用 Task Planning (todo)" }).click();
  expect((await disableToolsetResponse).status()).toBe(200);
  await expect(page.getByRole("switch", { name: "启用 Task Planning (todo)" })).not.toBeChecked();

  await reloadToolsProfile(page, profileId);
  await expect(page.getByRole("switch", { name: "启用 Task Planning (todo)" })).not.toBeChecked();

  const installForm = page.getByRole("form", { name: "Skill 安装" });
  await installForm.getByRole("radio", { name: "文件", exact: true }).click();
  await installForm.getByLabel("Skill 文件").setInputFiles(skillFixture);
  const uploadResponse = page.waitForResponse((response) => responseMatches(
    response,
    "POST",
    "/api/v1/files",
  ));
  const installAcceptedResponse = page.waitForResponse((response) => responseMatches(
    response,
    "POST",
    `/api/v1/profiles/${profileId}/skills/install`,
  ));
  const installCompletedResponse = completedOperationResponse(page, "skillInstall");
  const uploadCleanupResponse = page.waitForResponse((response) => responseMatches(
    response,
    "DELETE",
    /^\/api\/v1\/files\/file_[0-9a-f]{32}$/u,
  ));
  await installForm.getByRole("button", { name: "安装", exact: true }).click();
  expect((await uploadResponse).status()).toBe(201);
  const installAccepted = await operationBody(await installAcceptedResponse);
  expect(installAccepted.kind).toBe("skillInstall");
  expect(["queued", "running", "completed"]).toContain(installAccepted.status);
  const installCompleted = await operationBody(await installCompletedResponse);
  expect(installCompleted).toMatchObject({
    id: installAccepted.id,
    kind: "skillInstall",
    status: "completed",
  });
  expect((await uploadCleanupResponse).status()).toBe(204);

  const enabledSkillName = new RegExp(
    `^停用 Skill ${FIXTURE_SKILL_NAME} \\(${SKILL_ID_PATTERN}\\)$`,
    "u",
  );
  const disabledSkillName = new RegExp(
    `^启用 Skill ${FIXTURE_SKILL_NAME} \\(${SKILL_ID_PATTERN}\\)$`,
    "u",
  );
  const enabledSkill = page.getByRole("switch", { name: enabledSkillName });
  await expect(enabledSkill).toBeChecked();
  const enabledLabel = await enabledSkill.getAttribute("aria-label");
  const skillId = enabledLabel?.match(new RegExp(`(${SKILL_ID_PATTERN})`, "u"))?.[1];
  expect(skillId).toEqual(expect.stringMatching(new RegExp(`^${SKILL_ID_PATTERN}$`, "u")));
  if (!skillId) throw new Error("Installed Skill ID was not rendered in the UI.");

  const disableSkillResponse = page.waitForResponse((response) => responseMatches(
    response,
    "PATCH",
    `/api/v1/profiles/${profileId}/skills/${skillId}`,
  ));
  await enabledSkill.click();
  expect((await disableSkillResponse).status()).toBe(200);
  await expect(page.getByRole("switch", { name: disabledSkillName })).not.toBeChecked();

  await reloadToolsProfile(page, profileId);
  await expect(page.getByRole("switch", { name: disabledSkillName })).not.toBeChecked();
  const enableSkillResponse = page.waitForResponse((response) => responseMatches(
    response,
    "PATCH",
    `/api/v1/profiles/${profileId}/skills/${skillId}`,
  ));
  await page.getByRole("switch", { name: disabledSkillName }).click();
  expect((await enableSkillResponse).status()).toBe(200);
  await expect(page.getByRole("switch", { name: enabledSkillName })).toBeChecked();

  await reloadToolsProfile(page, profileId);
  await expect(page.getByRole("switch", { name: enabledSkillName })).toBeChecked();
  page.once("dialog", async (dialog) => {
    expect(dialog.message()).toContain(FIXTURE_SKILL_NAME);
    await dialog.accept();
  });
  const uninstallAcceptedResponse = page.waitForResponse((response) => responseMatches(
    response,
    "DELETE",
    `/api/v1/profiles/${profileId}/skills/${skillId}`,
  ));
  const uninstallCompletedResponse = completedOperationResponse(page, "skillUninstall");
  await page.getByRole("button", {
    name: new RegExp(`^卸载 Skill ${FIXTURE_SKILL_NAME} \\(${skillId}\\)$`, "u"),
  }).click();
  const uninstallAccepted = await operationBody(await uninstallAcceptedResponse);
  expect(uninstallAccepted.kind).toBe("skillUninstall");
  expect(["queued", "running", "completed"]).toContain(uninstallAccepted.status);
  const uninstallCompleted = await operationBody(await uninstallCompletedResponse);
  expect(uninstallCompleted).toMatchObject({
    id: uninstallAccepted.id,
    kind: "skillUninstall",
    status: "completed",
  });
  await expect(page.getByRole("switch", { name: enabledSkillName })).toHaveCount(0);

  await reloadToolsProfile(page, profileId);
  await expect(page.getByRole("switch", { name: enabledSkillName })).toHaveCount(0);
  await expect(page.getByRole("switch", { name: disabledSkillName })).toHaveCount(0);
  const persistedOperations = await page.evaluate(async (operationIds) => Promise.all(
    operationIds.map(async (operationId) => {
      const response = await fetch(`/api/v1/operations/${operationId}`, {
        cache: "no-store",
        headers: { Accept: "application/json" },
      });
      return { status: response.status, value: await response.json() as unknown };
    }),
  ), [installAccepted.id, uninstallAccepted.id]);
  expect(persistedOperations).toEqual([
    {
      status: 200,
      value: expect.objectContaining({
        id: installAccepted.id,
        kind: "skillInstall",
        status: "completed",
      }),
    },
    {
      status: 200,
      value: expect.objectContaining({
        id: uninstallAccepted.id,
        kind: "skillUninstall",
        status: "completed",
      }),
    },
  ]);

  expect(externalRequests).toEqual(new Set());
  expect(observedProtectedRequests.length).toBeGreaterThan(0);
  expect(observedProtectedRequests.every((request) => request.origin === browserOrigin)).toBe(true);
  expect(observedProtectedRequests.every((request) => request.authorization === undefined)).toBe(true);
});
