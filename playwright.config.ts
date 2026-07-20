import { resolve } from "node:path";
import { defineConfig, devices } from "@playwright/test";

function requiredEnvironment(name: string): string {
  const value = process.env[name]?.trim();
  if (!value) throw new Error(`${name} is required.`);
  return value;
}

function positiveInteger(name: string, fallback: number): number {
  const raw = process.env[name]?.trim();
  if (!raw) return fallback;
  const value = Number(raw);
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new Error(`${name} must be a positive integer.`);
  }
  return value;
}

function nonNegativeInteger(name: string, fallback: number): number {
  const raw = process.env[name]?.trim();
  if (!raw) return fallback;
  const value = Number(raw);
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new Error(`${name} must be a non-negative integer.`);
  }
  return value;
}

function booleanEnvironment(name: string, fallback: boolean): boolean {
  const raw = process.env[name]?.trim().toLowerCase();
  if (!raw) return fallback;
  if (raw === "1" || raw === "true") return true;
  if (raw === "0" || raw === "false") return false;
  throw new Error(`${name} must be true, false, 1, or 0.`);
}

function configuredPath(name: string, fallback: string): string {
  return resolve(process.env[name]?.trim() || fallback);
}

const baseURL = requiredEnvironment("SYNTHCHAT_E2E_BASE_URL");
const parsedBaseURL = new URL(baseURL);
if (!parsedBaseURL.hostname || !["127.0.0.1", "localhost", "::1"].includes(parsedBaseURL.hostname)) {
  throw new Error("SYNTHCHAT_E2E_BASE_URL must use a loopback hostname.");
}

export default defineConfig({
  testDir: "./tests/e2e",
  fullyParallel: false,
  forbidOnly: Boolean(process.env.CI),
  globalTimeout: positiveInteger("SYNTHCHAT_E2E_GLOBAL_TIMEOUT_MS", 1_200_000),
  retries: nonNegativeInteger("SYNTHCHAT_E2E_RETRIES", process.env.CI ? 1 : 0),
  workers: positiveInteger("SYNTHCHAT_E2E_WORKERS", 1),
  timeout: positiveInteger("SYNTHCHAT_E2E_TEST_TIMEOUT_MS", 90_000),
  expect: {
    timeout: positiveInteger("SYNTHCHAT_E2E_EXPECT_TIMEOUT_MS", 15_000),
  },
  outputDir: configuredPath("SYNTHCHAT_E2E_OUTPUT_DIR", "test-results/e2e"),
  reporter: [
    ["list"],
    ["html", {
      open: "never",
      outputFolder: configuredPath("SYNTHCHAT_E2E_REPORT_DIR", "playwright-report/e2e"),
    }],
  ],
  use: {
    ...devices["Desktop Chrome"],
    baseURL: parsedBaseURL.origin,
    actionTimeout: positiveInteger("SYNTHCHAT_E2E_ACTION_TIMEOUT_MS", 15_000),
    navigationTimeout: positiveInteger("SYNTHCHAT_E2E_NAVIGATION_TIMEOUT_MS", 30_000),
    headless: booleanEnvironment("SYNTHCHAT_E2E_HEADLESS", true),
    screenshot: "only-on-failure",
    trace: "retain-on-failure",
    video: "retain-on-failure",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
});
