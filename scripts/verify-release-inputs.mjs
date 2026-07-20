import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  existsSync,
  lstatSync,
  readFileSync,
  readdirSync,
  realpathSync,
  statSync,
} from "node:fs";
import { dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const workspace = resolve(scriptDirectory, "..");
const workspaceRealPath = realpathSync.native(workspace);
const failures = [];
const passes = [];
const notes = [];
const petAssetDigests = [];
const arguments_ = process.argv.slice(2);
const requireClean = arguments_.includes("--require-clean");
const printPetAssetDigests = arguments_.includes("--print-pet-asset-digests");
const releaseCandidate = arguments_.includes("--release-candidate")
  || process.env.SYNTHCHAT_RELEASE_CANDIDATE === "1"
  || process.env.SYNTHCHAT_SIGNING === "1";
const supportedArguments = new Set([
  "--require-clean",
  "--release-candidate",
  "--print-pet-asset-digests",
]);
const unsupportedArguments = arguments_.filter((argument) => !supportedArguments.has(argument));

const REQUIRED_NODE_RANGE = ">=22.14.0 <23";
const REQUIRED_NPM_RANGE = ">=10.9.2 <11";
const REQUIRED_PACKAGE_MANAGER = "npm@10.9.2";
const REQUIRED_RUST_VERSION = "1.88";
const REQUIRED_RUST_TOOLCHAIN = "1.88.0";
const PET_PROVENANCE_MANIFEST = "docs/pet-asset-provenance.json";
const PET_LIBRARY_DIRECTORY = "frontend/public/pet/lib";
const PET_MODEL_DIRECTORY = "frontend/public/pet/model";
const PET_MODEL_METADATA_FILES = new Set(["MODEL_SOURCES.md"]);
const MIXED_RUNTIME_EVIDENCE_REPORT = "docs/release-evidence/mixed-runtime-8h.json";
const MIXED_RUNTIME_EVIDENCE_MANIFEST = "docs/release-evidence/mixed-runtime-8h.manifest.json";
const MIXED_RUNTIME_EVIDENCE_VERIFIER = "scripts/verify-mixed-runtime-evidence.mjs";
const REQUIRED_DELIVERY_INPUTS = [
  ".github/workflows/ci.yml",
  ".gitattributes",
  "package.json",
  "package-lock.json",
  "frontend/package.json",
  "frontend/package-lock.json",
  "frontend/src/api/generated/openapi.d.ts",
  "rust-toolchain.toml",
  "backend/Cargo.toml",
  "backend/Cargo.lock",
  "desktop/Cargo.toml",
  "desktop/Cargo.lock",
  "desktop/tauri.conf.json",
  "README.md",
  "CONTRIBUTING.md",
  "SECURITY.md",
  "docs/api-contract.md",
  "docs/architecture.md",
  "docs/code-impact.md",
  "docs/development.md",
  "docs/linux-native-build.md",
  "docs/macos-native-build.md",
  "docs/migration-status.md",
  "docs/openapi.yaml",
  "docs/performance-report.md",
  PET_PROVENANCE_MANIFEST,
  "docs/pet-asset-provenance.md",
  "docs/release.md",
  "docs/release-evidence/README.md",
  "docs/security-report.md",
  "docs/test-report.md",
  "docs/upstream-lock.json",
  "docs/windows-native-build.md",
  "playwright.config.ts",
  "build-one-click.ps1",
  "scripts/build-windows-native.ps1",
  "scripts/build-macos-native.sh",
  "scripts/build-linux-native.sh",
  "scripts/check-openapi-types.mjs",
  "scripts/dev-desktop.ps1",
  "scripts/e2e/run-local.mjs",
  "scripts/prepare-backend-sidecar.mjs",
  "scripts/run-tauri.mjs",
  "scripts/verify-backend-runtime.ps1",
  "scripts/verify-mixed-runtime.mjs",
  MIXED_RUNTIME_EVIDENCE_VERIFIER,
  "scripts/verify-nsis-artifact.ps1",
  "scripts/verify-release-inputs.mjs",
];

const LOCKED_BUILD_MARKERS = [
  ["scripts/prepare-backend-sidecar.mjs", '"build",\n  "--locked",'],
  ["build-one-click.ps1", '$Arguments += @("--", "--locked")'],
  ["scripts/build-macos-native.sh", 'build --bundles app,dmg -- --locked'],
  ["scripts/build-linux-native.sh", 'tauri_args+=(--config "$tauri_config_override" -- --locked)'],
];

function workspacePath(relativePath) {
  const absolutePath = resolve(workspace, relativePath);
  const pathFromWorkspace = relative(workspace, absolutePath);
  if (
    pathFromWorkspace === ".."
    || pathFromWorkspace.startsWith(`..${sep}`)
    || isAbsolute(pathFromWorkspace)
  ) {
    throw new Error(`Path escaped the workspace: ${relativePath}`);
  }
  return absolutePath;
}

function requireFile(relativePath) {
  const absolutePath = workspacePath(relativePath);
  if (!existsSync(absolutePath)) {
    failures.push(`Missing required file: ${relativePath}`);
    return null;
  }
  try {
    const metadata = lstatSync(absolutePath);
    if (metadata.isSymbolicLink() || !metadata.isFile()) {
      failures.push(`Required path must be a regular non-symlink file: ${relativePath}`);
      return null;
    }
    const realPath = realpathSync.native(absolutePath);
    const pathFromWorkspace = relative(workspaceRealPath, realPath);
    if (
      pathFromWorkspace === ".."
      || pathFromWorkspace.startsWith(`..${sep}`)
      || isAbsolute(pathFromWorkspace)
    ) {
      failures.push(`Required file escaped the real workspace boundary: ${relativePath}`);
      return null;
    }
    return absolutePath;
  } catch {
    failures.push(`Unable to inspect required file safely: ${relativePath}`);
    return null;
  }
}

function readJson(relativePath) {
  const absolutePath = requireFile(relativePath);
  if (!absolutePath) return null;
  try {
    return JSON.parse(readFileSync(absolutePath, "utf8"));
  } catch (error) {
    failures.push(`Invalid JSON in ${relativePath}: ${error.message}`);
    return null;
  }
}

function sameStringMap(left, right) {
  const normalize = (value) => Object.entries(value ?? {})
    .sort(([leftKey], [rightKey]) => leftKey.localeCompare(rightKey));
  return JSON.stringify(normalize(left)) === JSON.stringify(normalize(right));
}

function isRecord(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function isNonEmptyString(value) {
  return typeof value === "string" && value.trim().length > 0;
}

function isSha256(value) {
  return typeof value === "string" && /^sha256:[0-9a-f]{64}$/u.test(value);
}

function isHttpsUrl(value) {
  if (!isNonEmptyString(value)) return false;
  try {
    return new URL(value).protocol === "https:";
  } catch {
    return false;
  }
}

function isIsoTimestamp(value) {
  if (!isNonEmptyString(value)) return false;
  return !Number.isNaN(Date.parse(value));
}

function manifestWorkspacePath(relativePath, fieldName) {
  if (!isNonEmptyString(relativePath)) {
    failures.push(`${fieldName} must be a non-empty workspace-relative path.`);
    return null;
  }
  if (isAbsolute(relativePath)) {
    failures.push(`${fieldName} must be workspace-relative, not absolute: ${relativePath}`);
    return null;
  }
  try {
    const absolutePath = workspacePath(relativePath);
    const canonicalRelativePath = relative(workspace, absolutePath).replaceAll("\\", "/");
    if (canonicalRelativePath !== relativePath.replaceAll("\\", "/")) {
      failures.push(`${fieldName} must use a canonical workspace-relative path: ${relativePath}`);
      return null;
    }
    return { absolutePath, relativePath: canonicalRelativePath };
  } catch (error) {
    failures.push(`${fieldName} is outside the workspace: ${error.message}`);
    return null;
  }
}

function assertRegularFile(relativePath, fieldName, requireTracked = false) {
  const resolved = manifestWorkspacePath(relativePath, fieldName);
  if (!resolved) return false;
  try {
    const entry = lstatSync(resolved.absolutePath);
    if (!entry.isFile() || entry.isSymbolicLink()) {
      failures.push(`${fieldName} must reference a regular non-symlink file: ${relativePath}`);
      return false;
    }
  } catch {
    failures.push(`${fieldName} does not exist: ${relativePath}`);
    return false;
  }
  if (requireTracked) {
    if (!isGitCheckout()) {
      failures.push(`${fieldName} must be Git-tracked for a release candidate.`);
      return false;
    }
    const tracked = gitPaths(
      ["ls-files", "-z", "--", resolved.relativePath],
      `inspect Git tracking for ${fieldName}`,
    );
    if (!tracked || !tracked.includes(resolved.relativePath)) {
      failures.push(`${fieldName} must be Git-tracked for a release candidate: ${relativePath}`);
      return false;
    }
  }
  return true;
}

function listPetDirectory(relativePath, label) {
  const absolutePath = workspacePath(relativePath);
  try {
    const entry = lstatSync(absolutePath);
    if (!entry.isDirectory() || entry.isSymbolicLink()) {
      failures.push(`${label} must be a real directory, not a symlink: ${relativePath}`);
      return null;
    }
  } catch {
    failures.push(`Missing ${label}: ${relativePath}`);
    return null;
  }

  try {
    return readdirSync(absolutePath, { withFileTypes: true })
      .sort((left, right) => (left.name < right.name ? -1 : left.name > right.name ? 1 : 0));
  } catch (error) {
    failures.push(`Could not enumerate ${label} ${relativePath}: ${error.message}`);
    return null;
  }
}

function discoverPetAssetGroups() {
  const expected = new Map();
  const libraries = listPetDirectory(PET_LIBRARY_DIRECTORY, "Pet library directory");
  if (libraries) {
    for (const entry of libraries) {
      const entryPath = `${PET_LIBRARY_DIRECTORY}/${entry.name}`;
      if (entry.isSymbolicLink()) {
        failures.push(`Vendored Pet library entry must not be a symlink: ${entryPath}`);
        continue;
      }
      if (!entry.isFile() && !entry.isDirectory()) {
        failures.push(`Vendored Pet library entry must be a regular file or directory: ${entryPath}`);
        continue;
      }
      expected.set(entryPath, "library");
    }
  }

  const models = listPetDirectory(PET_MODEL_DIRECTORY, "Pet model directory");
  if (models) {
    for (const entry of models) {
      const entryPath = `${PET_MODEL_DIRECTORY}/${entry.name}`;
      if (entry.isSymbolicLink()) {
        failures.push(`Vendored Pet model entry must not be a symlink: ${entryPath}`);
        continue;
      }
      if (entry.isDirectory()) {
        expected.set(entryPath, "model");
      } else if (!entry.isFile() || !PET_MODEL_METADATA_FILES.has(entry.name)) {
        failures.push(
          `Unclassified file at the Pet model root: ${entryPath}. Add it to the reviewed metadata allowlist or make it part of a model group.`,
        );
      }
    }
  }
  return expected;
}

function sha256File(absolutePath) {
  return createHash("sha256").update(readFileSync(absolutePath)).digest("hex");
}

function petTreeSha256(rootPath) {
  const records = [];
  const walk = (absolutePath, relativePath) => {
    const entry = lstatSync(absolutePath);
    if (entry.isSymbolicLink()) {
      throw new Error(`symbolic links are not allowed: ${relativePath || "."}`);
    }
    if (entry.isFile()) {
      records.push([relativePath || ".", sha256File(absolutePath)]);
      return;
    }
    if (!entry.isDirectory()) {
      throw new Error(`unsupported file type: ${relativePath || "."}`);
    }
    const children = readdirSync(absolutePath, { withFileTypes: true })
      .sort((left, right) => (left.name < right.name ? -1 : left.name > right.name ? 1 : 0));
    for (const child of children) {
      const childPath = relativePath ? `${relativePath}/${child.name}` : child.name;
      walk(join(absolutePath, child.name), childPath);
    }
  };

  walk(rootPath, "");
  const digest = createHash("sha256");
  for (const [relativePath, fileDigest] of records) {
    digest.update(relativePath, "utf8");
    digest.update("\0", "utf8");
    digest.update(fileDigest, "utf8");
    digest.update("\n", "utf8");
  }
  return `sha256:${digest.digest("hex")}`;
}

function requireCandidateString(value, fieldName, groupId) {
  if (!isNonEmptyString(value)) {
    failures.push(`Pet asset group ${groupId} is missing ${fieldName} required for a release candidate.`);
    return false;
  }
  return true;
}

function requireCandidateEvidence(value, fieldName, groupId) {
  if (!isNonEmptyString(value)) {
    failures.push(`Pet asset group ${groupId} is missing ${fieldName} required for a release candidate.`);
    return false;
  }
  if (isHttpsUrl(value)) return true;
  return assertRegularFile(value, `Pet asset group ${groupId} ${fieldName}`, true);
}

function checkPetAssetProvenance() {
  const manifest = readJson(PET_PROVENANCE_MANIFEST);
  if (!manifest) return;
  if (!isRecord(manifest) || manifest.schemaVersion !== 1 || !Array.isArray(manifest.groups)) {
    failures.push(`${PET_PROVENANCE_MANIFEST} must contain schemaVersion 1 and a groups array.`);
    return;
  }
  if (manifest.releaseStatus !== "unverified" && manifest.releaseStatus !== "verified") {
    failures.push(`${PET_PROVENANCE_MANIFEST} releaseStatus must be \"unverified\" or \"verified\".`);
  }

  const expectedGroups = discoverPetAssetGroups();
  const declaredRoots = new Map();
  let unverifiedCount = 0;
  for (const group of manifest.groups) {
    if (!isRecord(group)) {
      failures.push(`${PET_PROVENANCE_MANIFEST} groups must contain objects.`);
      continue;
    }
    const groupId = isNonEmptyString(group.id) ? group.id : "<unnamed>";
    if (!/^[a-z0-9][a-z0-9-]*$/u.test(groupId)) {
      failures.push(`Pet asset group id must use lowercase letters, digits, and hyphens: ${group.id ?? "missing"}.`);
    }
    const root = manifestWorkspacePath(group.root, `Pet asset group ${groupId} root`);
    if (!root) continue;
    if (declaredRoots.has(root.relativePath)) {
      failures.push(`Pet asset group root is declared more than once: ${root.relativePath}`);
      continue;
    }
    declaredRoots.set(root.relativePath, group);
    const expectedKind = expectedGroups.get(root.relativePath);
    if (!expectedKind) {
      failures.push(`Pet asset group ${groupId} does not map to a current vendored library or model group: ${root.relativePath}`);
      continue;
    }
    if (group.kind !== expectedKind) {
      failures.push(`Pet asset group ${groupId} must have kind ${expectedKind}: ${root.relativePath}`);
    }
    const provenance = group.provenance;
    const license = group.license;
    const integrity = group.integrity;
    const review = group.review;
    if (!isRecord(provenance) || !isRecord(license) || !isRecord(integrity) || !isRecord(review)) {
      failures.push(`Pet asset group ${groupId} must contain provenance, license, integrity, and review objects.`);
      continue;
    }
    if (integrity.algorithm !== "sha256-tree-v1") {
      failures.push(`Pet asset group ${groupId} must use integrity.algorithm sha256-tree-v1.`);
    }
    if (integrity.sha256 !== null && !isSha256(integrity.sha256)) {
      failures.push(`Pet asset group ${groupId} integrity.sha256 must be null or a lowercase sha256 digest.`);
    }
    if (review.status !== "unverified" && review.status !== "verified") {
      failures.push(`Pet asset group ${groupId} review.status must be \"unverified\" or \"verified\".`);
    }
    if (!Array.isArray(review.evidence) || !review.evidence.every((value) => typeof value === "string")) {
      failures.push(`Pet asset group ${groupId} review.evidence must be an array of strings.`);
    }

    let actualDigest;
    try {
      actualDigest = petTreeSha256(root.absolutePath);
    } catch (error) {
      failures.push(`Could not calculate integrity for Pet asset group ${groupId}: ${error.message}`);
      continue;
    }
    petAssetDigests.push({ id: groupId, root: root.relativePath, sha256: actualDigest });
    if (isSha256(integrity.sha256) && integrity.sha256 !== actualDigest) {
      failures.push(
        `Pet asset group ${groupId} integrity mismatch: manifest ${integrity.sha256}, current ${actualDigest}.`,
      );
    }

    if (review.status !== "verified") {
      unverifiedCount += 1;
    }
    if (!releaseCandidate) continue;

    if (review.status !== "verified") {
      failures.push(`Pet asset group ${groupId} is ${review.status}; release candidates require verified provenance.`);
    }
    if (!isHttpsUrl(provenance.sourceUrl)) {
      failures.push(`Pet asset group ${groupId} provenance.sourceUrl must be an HTTPS URL for a release candidate.`);
    }
    requireCandidateString(provenance.immutableRef, "provenance.immutableRef", groupId);
    if (!isSha256(provenance.artifactSha256)) {
      failures.push(`Pet asset group ${groupId} provenance.artifactSha256 must be a lowercase sha256 digest for a release candidate.`);
    }
    if (!isIsoTimestamp(provenance.retrievedAt)) {
      failures.push(`Pet asset group ${groupId} provenance.retrievedAt must be an ISO timestamp for a release candidate.`);
    }
    requireCandidateString(license.identifier, "license.identifier", groupId);
    if (!isHttpsUrl(license.textUrl)) {
      failures.push(`Pet asset group ${groupId} license.textUrl must be an HTTPS URL for a release candidate.`);
    }
    if (!isNonEmptyString(license.noticePath)) {
      failures.push(`Pet asset group ${groupId} is missing license.noticePath required for a release candidate.`);
    } else {
      assertRegularFile(license.noticePath, `Pet asset group ${groupId} license.noticePath`, true);
    }
    requireCandidateEvidence(license.redistributionEvidence, "license.redistributionEvidence", groupId);
    if (!isSha256(integrity.sha256)) {
      failures.push(`Pet asset group ${groupId} integrity.sha256 must be a lowercase sha256 digest for a release candidate.`);
    }
    if (!isIsoTimestamp(review.reviewedAt)) {
      failures.push(`Pet asset group ${groupId} review.reviewedAt must be an ISO timestamp for a release candidate.`);
    }
    requireCandidateString(review.reviewedBy, "review.reviewedBy", groupId);
    if (!Array.isArray(review.evidence) || review.evidence.length === 0) {
      failures.push(`Pet asset group ${groupId} needs at least one review.evidence item for a release candidate.`);
    } else {
      review.evidence.forEach((evidence, index) => {
        requireCandidateEvidence(evidence, `review.evidence[${index}]`, groupId);
      });
    }
  }

  for (const [expectedRoot, expectedKind] of expectedGroups) {
    if (!declaredRoots.has(expectedRoot)) {
      failures.push(`Vendored Pet ${expectedKind} group is missing from ${PET_PROVENANCE_MANIFEST}: ${expectedRoot}`);
    }
  }
  if (manifest.groups.length !== expectedGroups.size) {
    failures.push(
      `${PET_PROVENANCE_MANIFEST} must declare exactly ${expectedGroups.size} current Pet library/model groups; found ${manifest.groups.length}.`,
    );
  }
  if (releaseCandidate && manifest.releaseStatus !== "verified") {
    failures.push(`${PET_PROVENANCE_MANIFEST} is ${manifest.releaseStatus}; release candidates require releaseStatus \"verified\".`);
  }
  if (unverifiedCount > 0) {
    notes.push(
      `Pet asset provenance is intentionally incomplete: ${unverifiedCount}/${expectedGroups.size} vendored library/model groups are unverified. This is non-blocking outside release-candidate mode.`,
    );
  } else if (failures.length === 0) {
    passes.push(`All ${expectedGroups.size} vendored Pet library/model groups have verified provenance metadata.`);
  }
}

function runGit(argumentsForGit) {
  return execFileSync(
    "git",
    ["-C", workspace, ...argumentsForGit],
    { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] },
  );
}

let gitCheckout;
function isGitCheckout() {
  if (gitCheckout !== undefined) return gitCheckout;
  try {
    gitCheckout = runGit(["rev-parse", "--is-inside-work-tree"]).trim() === "true";
  } catch {
    gitCheckout = false;
  }
  return gitCheckout;
}

function gitPaths(argumentsForGit, operation) {
  try {
    return runGit(argumentsForGit)
      .split("\0")
      .filter(Boolean)
      .map((path) => path.replaceAll("\\", "/"));
  } catch (error) {
    failures.push(`Could not ${operation}: ${error.message}`);
    return null;
  }
}

function requireTrackedFiles(relativePaths) {
  if (!isGitCheckout()) {
    failures.push("A Git checkout is required to verify that release inputs are tracked.");
    return;
  }

  const trackedPaths = gitPaths(["ls-files", "-z", "--", ...relativePaths], "inspect Git-tracked release inputs");
  if (!trackedPaths) return;
  const tracked = new Set(trackedPaths);
  const untracked = relativePaths.filter((relativePath) => !tracked.has(relativePath));
  if (untracked.length > 0) {
    failures.push(
      `Required release inputs are not Git-tracked:\n${untracked.map((path) => `  ${path}`).join("\n")}`,
    );
  } else {
    passes.push("Required delivery inputs and generated OpenAPI types are Git-tracked.");
  }
}

function packageVersionFromToml(relativePath) {
  const absolutePath = requireFile(relativePath);
  if (!absolutePath) return null;
  const lines = readFileSync(absolutePath, "utf8").split(/\r?\n/u);
  let inPackageSection = false;
  let version = null;
  for (const line of lines) {
    const trimmed = line.trim();
    if (trimmed === "[package]") {
      inPackageSection = true;
      continue;
    }
    if (inPackageSection && /^\[[^\]]+\]$/u.test(trimmed)) break;
    if (!inPackageSection) continue;
    const match = trimmed.match(/^version\s*=\s*"([^"\r\n]+)"\s*$/u);
    if (match) {
      [, version] = match;
      break;
    }
  }
  if (!version) {
    failures.push(`Unable to read [package].version from ${relativePath}`);
    return null;
  }
  return version;
}

function packageRustVersionFromToml(relativePath) {
  const absolutePath = requireFile(relativePath);
  if (!absolutePath) return null;
  const lines = readFileSync(absolutePath, "utf8").split(/\r?\n/u);
  let inPackageSection = false;
  for (const line of lines) {
    const trimmed = line.trim();
    if (trimmed === "[package]") {
      inPackageSection = true;
      continue;
    }
    if (inPackageSection && /^\[[^\]]+\]$/u.test(trimmed)) break;
    if (!inPackageSection) continue;
    const match = trimmed.match(/^rust-version\s*=\s*"([^"\r\n]+)"\s*$/u);
    if (match) return match[1];
  }
  failures.push(`Unable to read [package].rust-version from ${relativePath}`);
  return null;
}

function rustToolchainPolicy() {
  const relativePath = "rust-toolchain.toml";
  const absolutePath = requireFile(relativePath);
  if (!absolutePath) return null;
  const content = readFileSync(absolutePath, "utf8");
  const channel = content.match(/^channel\s*=\s*"([^"\r\n]+)"\s*$/mu)?.[1] ?? null;
  const profile = content.match(/^profile\s*=\s*"([^"\r\n]+)"\s*$/mu)?.[1] ?? null;
  const componentsSource = content.match(/^components\s*=\s*(\[[^\r\n]+\])\s*$/mu)?.[1];
  let components = null;
  if (componentsSource) {
    try {
      const parsed = JSON.parse(componentsSource);
      if (Array.isArray(parsed) && parsed.every((value) => typeof value === "string")) {
        components = parsed;
      }
    } catch {
      components = null;
    }
  }
  return { channel, components, profile };
}

function checkTrackedHygiene() {
  if (!isGitCheckout()) {
    failures.push("A Git checkout is required to inspect tracked-file hygiene.");
    return;
  }

  const forbidden = [
    "src-tauri/**",
    "synthchat-data/**",
    "desktop/synthchat-data/**",
    "skills/**",
    "data/mcp_servers/**",
    "data/tts/**",
    ".multi-agent/runs/**",
    "*.db",
    "*.sqlite",
    "*.sqlite3",
    ".env",
    ".env.*",
  ];

  try {
    const output = runGit(["ls-files", "--", ...forbidden]).trim();
    if (output) {
      failures.push(`Forbidden runtime or generated files are tracked:\n${output}`);
    } else {
      passes.push("No forbidden runtime data or legacy Agent paths are tracked.");
    }
  } catch (error) {
    failures.push(`Could not inspect Git tracked files: ${error.message}`);
  }
}

function checkRuntimePolicy() {
  const rootPackage = readJson("package.json");
  const rootLock = readJson("package-lock.json");
  if (!rootPackage || !rootLock) return;

  if (rootPackage.engines?.node !== REQUIRED_NODE_RANGE) {
    failures.push(
      `package.json engines.node must be ${REQUIRED_NODE_RANGE}; found ${rootPackage.engines?.node ?? "missing"}.`,
    );
  }
  if (rootPackage.engines?.npm !== REQUIRED_NPM_RANGE) {
    failures.push(
      `package.json engines.npm must be ${REQUIRED_NPM_RANGE}; found ${rootPackage.engines?.npm ?? "missing"}.`,
    );
  }
  if (rootPackage.packageManager !== REQUIRED_PACKAGE_MANAGER) {
    failures.push(
      `package.json packageManager must be ${REQUIRED_PACKAGE_MANAGER}; found ${rootPackage.packageManager ?? "missing"}.`,
    );
  }
  if (
    rootPackage.engines?.node === REQUIRED_NODE_RANGE
    && rootPackage.engines?.npm === REQUIRED_NPM_RANGE
    && rootPackage.packageManager === REQUIRED_PACKAGE_MANAGER
  ) {
    passes.push(`Node and npm release policy is pinned to Node 22.14.0 and npm 10.9.2.`);
  }

  const lockRoot = rootLock.packages?.[""];
  const lockMatchesPackage = rootLock.lockfileVersion === 3
    && rootLock.name === rootPackage.name
    && rootLock.version === rootPackage.version
    && lockRoot?.name === rootPackage.name
    && lockRoot?.version === rootPackage.version
    && sameStringMap(lockRoot?.dependencies, rootPackage.dependencies)
    && sameStringMap(lockRoot?.devDependencies, rootPackage.devDependencies)
    && sameStringMap(lockRoot?.engines, rootPackage.engines);
  if (!lockMatchesPackage) {
    failures.push("package-lock.json must be npm lockfileVersion 3 and exactly match the root package metadata, dependencies, devDependencies, and engines.");
  } else {
    passes.push("Root Playwright/E2E dependencies are pinned by package-lock.json and match package.json.");
  }
}

function checkLockedBuildEntrypoints() {
  const missingMarkers = [];
  for (const [relativePath, marker] of LOCKED_BUILD_MARKERS) {
    const absolutePath = requireFile(relativePath);
    if (!absolutePath || !readFileSync(absolutePath, "utf8").includes(marker)) {
      missingMarkers.push(relativePath);
    }
  }
  if (missingMarkers.length > 0) {
    failures.push(`Candidate build entrypoints must pass --locked to Cargo: ${missingMarkers.join(", ")}.`);
  } else {
    passes.push("Windows, macOS, Linux, and backend-sidecar candidate builds pass --locked to Cargo.");
  }
}

function checkRustPolicy() {
  const manifests = [
    ["backend/Cargo.toml", packageRustVersionFromToml("backend/Cargo.toml")],
    ["desktop/Cargo.toml", packageRustVersionFromToml("desktop/Cargo.toml")],
  ];
  const mismatches = manifests.filter(([, rustVersion]) => rustVersion !== REQUIRED_RUST_VERSION);
  const toolchain = rustToolchainPolicy();
  const toolchainMatches = toolchain?.channel === REQUIRED_RUST_TOOLCHAIN
    && toolchain.profile === "minimal"
    && toolchain.components?.length === 2
    && toolchain.components.includes("clippy")
    && toolchain.components.includes("rustfmt");
  if (mismatches.length > 0 || !toolchainMatches) {
    failures.push(
      `Rust package policy requires rust-version ${REQUIRED_RUST_VERSION} and rust-toolchain.toml channel ${REQUIRED_RUST_TOOLCHAIN} with minimal rustfmt/clippy: ${[
        ...mismatches.map(([file, rustVersion]) => `${file} (${rustVersion ?? "missing"})`),
        ...(!toolchainMatches ? [`rust-toolchain.toml (${JSON.stringify(toolchain)})`] : []),
      ].join(", ")}.`,
    );
  } else {
    passes.push(`Rust package policy pins ${REQUIRED_RUST_TOOLCHAIN} with rustfmt/clippy and matches both Cargo manifests.`);
  }
}

function checkVersionAlignment() {
  const rootPackage = readJson("package.json");
  const frontendPackage = readJson("frontend/package.json");
  const tauriConfig = readJson("desktop/tauri.conf.json");
  const desktopVersion = packageVersionFromToml("desktop/Cargo.toml");
  const backendVersion = packageVersionFromToml("backend/Cargo.toml");

  const productVersion = rootPackage?.version;
  if (!productVersion) {
    failures.push("package.json does not declare a product version.");
    return;
  }

  const productVersions = [
    ["frontend/package.json", frontendPackage?.version],
    ["desktop/Cargo.toml", desktopVersion],
    ["desktop/tauri.conf.json", tauriConfig?.version],
  ];
  const mismatches = productVersions.filter(([, version]) => version !== productVersion);
  if (mismatches.length > 0) {
    failures.push(
      `Product version ${productVersion} does not match ${mismatches
        .map(([file, version]) => `${file} (${version ?? "missing"})`)
        .join(", ")}.`,
    );
  } else {
    passes.push(`Desktop product version is aligned at ${productVersion}.`);
  }

  if (backendVersion) {
    notes.push(
      `Backend crate version is ${backendVersion}; it is intentionally checked independently from the desktop product version.`,
    );
  }
}

function checkDeliveryInputs() {
  const initialFailureCount = failures.length;
  for (const relativePath of REQUIRED_DELIVERY_INPUTS) {
    requireFile(relativePath);
  }
  requireTrackedFiles(REQUIRED_DELIVERY_INPUTS);

  if (failures.length === initialFailureCount) {
    passes.push(
      "Required lockfiles, release-evidence verifier/docs, build entrypoints, and generated contract types are present.",
    );
  }
}

function checkMixedRuntimeEvidence() {
  if (!releaseCandidate) {
    notes.push(
      "The eight-hour mixed-runtime evidence pair is enforced only in --release-candidate mode.",
    );
    return;
  }
  const evidenceInputs = [MIXED_RUNTIME_EVIDENCE_REPORT, MIXED_RUNTIME_EVIDENCE_MANIFEST];
  const initialFailureCount = failures.length;
  const verifierPath = requireFile(MIXED_RUNTIME_EVIDENCE_VERIFIER);
  for (const relativePath of evidenceInputs) requireFile(relativePath);
  if (!verifierPath || failures.length !== initialFailureCount) return;
  try {
    execFileSync(
      process.execPath,
      [
        verifierPath,
        "--report",
        MIXED_RUNTIME_EVIDENCE_REPORT,
        "--manifest",
        MIXED_RUNTIME_EVIDENCE_MANIFEST,
      ],
      {
        cwd: workspace,
        encoding: "utf8",
        maxBuffer: 1_048_576,
        stdio: ["ignore", "pipe", "pipe"],
        timeout: 60_000,
        windowsHide: true,
      },
    );
    passes.push("Canonical eight-hour mixed-runtime report and reviewed manifest passed the strict evidence verifier.");
  } catch (error) {
    const detail = [error?.stdout, error?.stderr]
      .filter((value) => typeof value === "string" && value.trim())
      .join(" ")
      .replaceAll(/\s+/gu, " ")
      .slice(0, 1_000);
    failures.push(
      `Eight-hour mixed-runtime evidence verification failed${detail ? `: ${detail}` : "."}`,
    );
  }
}

function summarizePaths(label, paths) {
  const displayLimit = 20;
  const shown = paths.slice(0, displayLimit).map((path) => `  ${path}`);
  const remainder = paths.length - shown.length;
  return [
    `${label} (${paths.length}):`,
    ...shown,
    ...(remainder > 0 ? [`  ... and ${remainder} more`] : []),
  ].join("\n");
}

function checkCleanWorktree() {
  if (!isGitCheckout()) {
    failures.push("--require-clean requires a Git checkout.");
    return;
  }

  const staged = gitPaths(["diff", "--cached", "--name-only", "-z"], "inspect staged changes");
  const unstaged = gitPaths(["diff", "--name-only", "-z"], "inspect unstaged changes");
  const untracked = gitPaths(["ls-files", "--others", "--exclude-standard", "-z"], "inspect untracked files");
  if (!staged || !unstaged || !untracked) return;
  const allowedEvidenceArtifacts = releaseCandidate
    ? new Set([MIXED_RUNTIME_EVIDENCE_REPORT, MIXED_RUNTIME_EVIDENCE_MANIFEST])
    : new Set();
  const unexpectedUntracked = untracked.filter((path) => !allowedEvidenceArtifacts.has(path));
  const dirtySections = [
    staged.length > 0 ? summarizePaths("Staged changes", staged) : null,
    unstaged.length > 0 ? summarizePaths("Unstaged changes", unstaged) : null,
    unexpectedUntracked.length > 0 ? summarizePaths("Untracked files", unexpectedUntracked) : null,
  ].filter(Boolean);

  if (dirtySections.length > 0) {
    failures.push(
      [
        "Clean release mode requires no staged, unstaged, or untracked Git changes.",
        ...dirtySections,
      ].join("\n"),
    );
  } else {
    passes.push(
      releaseCandidate
        ? "Git worktree is clean apart from the canonical untracked mixed-runtime evidence pair."
        : "Git worktree is clean (no staged, unstaged, or untracked drift).",
    );
  }
}

for (const argument of unsupportedArguments) {
  failures.push(
    `Unsupported argument: ${argument}. Supported arguments: --require-clean, --release-candidate, --print-pet-asset-digests`,
  );
}
if (releaseCandidate && !requireClean) {
  failures.push("--release-candidate requires --require-clean so release provenance is checked from a clean checkout.");
}

console.log("SynthChat release-input check");
console.log(`Workspace: ${workspace}`);
console.log(
  `Mode: ${releaseCandidate
    ? "signed/release-candidate validation"
    : requireClean
      ? "clean source-input validation"
      : "source-input validation"}`,
);
checkRuntimePolicy();
checkRustPolicy();
checkLockedBuildEntrypoints();
checkVersionAlignment();
checkDeliveryInputs();
checkMixedRuntimeEvidence();
checkPetAssetProvenance();
checkTrackedHygiene();
if (requireClean) checkCleanWorktree();

if (printPetAssetDigests) {
  console.log("Pet asset sha256-tree-v1 digests (recording a digest does not verify provenance):");
  for (const digest of petAssetDigests) {
    console.log(`PET_ASSET_DIGEST ${JSON.stringify(digest)}`);
  }
}

for (const pass of passes) console.log(`PASS: ${pass}`);
for (const note of notes) console.log(`NOTE: ${note}`);
for (const failure of failures) console.error(`FAIL: ${failure}`);

if (failures.length > 0) {
  process.exitCode = 1;
} else if (releaseCandidate) {
  console.log("Release source inputs, Pet asset provenance, and clean-worktree checks passed. Run the locked build and test gates before signing or shipping.");
} else if (requireClean) {
  console.log("Clean source inputs passed. This mode does not approve a signed release; use --release-candidate after Pet asset provenance is verified.");
} else {
  console.log("Source inputs are internally consistent. This mode does not assert release readiness; use --require-clean --release-candidate from a clean checkout before signing or shipping.");
}
