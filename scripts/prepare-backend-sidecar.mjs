import { copyFileSync, mkdirSync, statSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const scriptsDirectory = dirname(fileURLToPath(import.meta.url));
const workspace = resolve(scriptsDirectory, "..");

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: workspace,
    encoding: options.capture ? "utf8" : undefined,
    stdio: options.capture ? ["ignore", "pipe", "inherit"] : "inherit",
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`${command} exited with status ${result.status ?? "unknown"}`);
  }
  return options.capture ? result.stdout : "";
}

function defaultRustToolchain() {
  if (process.platform === "win32" && process.arch === "x64") {
    return "1.88.0-x86_64-pc-windows-msvc";
  }
  return "1.88.0";
}

const rustToolchain = process.env.SYNTHCHAT_RUST_TOOLCHAIN || defaultRustToolchain();
if (!/^[A-Za-z0-9_.-]+$/u.test(rustToolchain)) {
  throw new Error(`unsafe Rust toolchain: ${rustToolchain}`);
}
const rustupSelection = [`+${rustToolchain}`];

function hostTriple() {
  const version = run("rustc", [...rustupSelection, "-vV"], { capture: true });
  const host = version.match(/^host:\s*(\S+)\s*$/mu)?.[1];
  if (!host) throw new Error("rustc did not report a host target triple");
  return host;
}

const target = process.env.TAURI_ENV_TARGET_TRIPLE
  || process.env.CARGO_BUILD_TARGET
  || hostTriple();
if (!/^[A-Za-z0-9_.-]+$/u.test(target)) {
  throw new Error(`unsafe Rust target triple: ${target}`);
}

const executableSuffix = target.includes("windows") ? ".exe" : "";
const manifest = join(workspace, "backend", "Cargo.toml");
const metadata = JSON.parse(run("cargo", [
  ...rustupSelection,
  "metadata",
  "--locked",
  "--no-deps",
  "--format-version",
  "1",
  "--manifest-path",
  manifest,
], { capture: true }));
if (typeof metadata.target_directory !== "string" || !metadata.target_directory) {
  throw new Error("cargo metadata did not report a target directory");
}
const targetDirectory = resolve(metadata.target_directory);
run("cargo", [
  ...rustupSelection,
  "build",
  "--locked",
  "--manifest-path",
  manifest,
  "--release",
  "--target",
  target,
]);

const source = join(
  targetDirectory,
  target,
  "release",
  `synthchat-hermes-backend${executableSuffix}`,
);
const destinationDirectory = join(workspace, "desktop", "binaries");
const destination = join(
  destinationDirectory,
  `synthchat-hermes-backend-${target}${executableSuffix}`,
);

if (!statSync(source).isFile()) {
  throw new Error(`backend build did not produce a file: ${source}`);
}
mkdirSync(destinationDirectory, { recursive: true });
copyFileSync(source, destination);
console.log(`Prepared backend sidecar: ${destination}`);
