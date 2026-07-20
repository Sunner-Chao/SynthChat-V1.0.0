import { spawnSync } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const scriptsDirectory = dirname(fileURLToPath(import.meta.url));
const workspace = resolve(scriptsDirectory, "..");
const cli = join(
  workspace,
  "frontend",
  "node_modules",
  "@tauri-apps",
  "cli",
  "tauri.js",
);
const result = spawnSync(process.execPath, [cli, ...process.argv.slice(2)], {
  cwd: join(workspace, "desktop"),
  env: process.env,
  stdio: "inherit",
});

if (result.error) throw result.error;
process.exitCode = result.status ?? 1;
