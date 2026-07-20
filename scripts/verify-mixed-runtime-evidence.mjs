import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { lstatSync, readFileSync, realpathSync } from "node:fs";
import { isAbsolute, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const workspace = resolve(fileURLToPath(new URL("..", import.meta.url)));
const EIGHT_HOURS_MS = 28_800_000;
const REQUIRED_NODE_VERSION = "22.14.0";
const REQUIRED_RUST_VERSION = "1.88.0";
const REPORT_MAX_BYTES = 16 * 1024 * 1024;
const MANIFEST_MAX_BYTES = 64 * 1024;
const BINARY_MAX_BYTES = 512 * 1024 * 1024;
const VERIFIER_MAX_BYTES = 4 * 1024 * 1024;
const HASH_PATTERN = /^sha256:[0-9a-f]{64}$/u;
const COMMIT_PATTERN = /^(?:[0-9a-f]{40}|[0-9a-f]{64})$/u;
const SAFE_PLATFORM = new Set(["darwin", "linux", "win32"]);
const SAFE_ARCH = new Set(["arm64", "x64"]);
const REPORT_PREFIX = "docs/release-evidence/";
const REPORT_TOP_LEVEL_KEYS = [
  "backendLogs",
  "cleanup",
  "configuration",
  "durationMs",
  "events",
  "failures",
  "failuresDropped",
  "finishedAt",
  "latenciesMs",
  "mode",
  "provider",
  "provenance",
  "resources",
  "schemaVersion",
  "startedAt",
  "status",
  "workload",
];
const CONFIGURATION_KEYS = [
  "concurrency",
  "cycleDelayMs",
  "durationMs",
  "maxFailures",
  "maxIterations",
  "providerDelayMs",
  "providerReplyBytes",
  "providerReplyCodePoints",
  "latencySampleLimit",
  "resourceIntervalMs",
  "resourceSampleLimit",
  "resourceSamples",
  "toolEveryIterations",
];
const WORKLOAD_KEYS = [
  "elapsedMs",
  "failures",
  "iterationsCompleted",
  "iterationsStarted",
  "successes",
  "toolProbesExpected",
];
const PROVIDER_KEYS = [
  "failures",
  "maxActiveRequests",
  "normalCompletions",
  "pendingToolCalls",
  "rejections",
  "requests",
  "toolCallsIssued",
  "toolResultsValidated",
];
const RESOURCE_KEYS = ["backendRssUnavailable", "dropped", "samples", "skipped"];
const RESOURCE_SAMPLE_KEYS = [
  "backendRssBytes",
  "elapsedMs",
  "runnerCpuSystemMicros",
  "runnerCpuUserMicros",
  "runnerHeapUsedBytes",
  "runnerRssBytes",
];
const CLEANUP_KEYS = ["backendForced", "backendStopped", "providerStopped", "tempRemoved"];
const LATENCY_KEYS = [
  "count",
  "droppedSamples",
  "failures",
  "max",
  "mean",
  "min",
  "p50",
  "p95",
  "p99",
];
const ITERATION_LATENCIES = [
  "history.fts_search",
  "history.list_live",
  "history.list_terminal",
  "messages.read_live",
  "messages.read_terminal",
  "run.post",
  "run.read_terminal",
  "run.sse_terminal",
  "session.create",
  "session.read_live",
  "session.read_terminal",
];
const SETUP_LATENCIES = [
  "setup.backend_build",
  "setup.backend_start",
  "setup.profile",
  "setup.provider",
];
const MANIFEST_KEYS = ["candidate", "command", "kind", "report", "rssReview", "schemaVersion"];
const CANDIDATE_KEYS = [
  "arch",
  "backendBinary",
  "effectiveConfigSha256",
  "gitCommit",
  "nodeVersion",
  "platform",
  "rustVersion",
];
const PROVENANCE_KEYS = [
  "arch",
  "argvSha256",
  "backend",
  "effectiveConfigSha256",
  "git",
  "nodeVersion",
  "overrideNames",
  "overrideNamesSha256",
  "platform",
  "rustVersion",
  "schemaVersion",
  "verifier",
];
const GIT_PROVENANCE_KEYS = ["commit", "objectFormat", "tree", "worktreeClean"];
const FILE_PROVENANCE_KEYS = ["bytes", "path", "sha256"];
const BINARY_KEYS = ["path", "sha256"];
const REPORT_BINDING_KEYS = ["bytes", "path", "sha256"];
const COMMAND_KEYS = ["arguments", "executable"];
const RSS_REVIEW_KEYS = [
  "availableSamples",
  "decision",
  "droppedSamples",
  "finalWindowHours",
  "finalWindowSlopeMiBPerHour",
  "firstMiB",
  "fullWindowSlopeMiBPerHour",
  "lastMiB",
  "peakMiB",
  "reviewedAt",
  "reviewer",
  "sampleCount",
  "skippedSamples",
  "summary",
  "unavailableSamples",
];

class EvidenceError extends Error {
  constructor(code) {
    super(code);
    this.name = "EvidenceError";
    this.code = code;
  }
}

function fail(code) {
  throw new EvidenceError(code);
}

function requireCondition(condition, code) {
  if (!condition) fail(code);
}

function isRecord(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function exactKeys(value, expected, code) {
  requireCondition(isRecord(value), code);
  const actual = Object.keys(value).sort();
  const wanted = [...expected].sort();
  requireCondition(
    actual.length === wanted.length && actual.every((key, index) => key === wanted[index]),
    code,
  );
}

function safeInteger(value, { min = 0, max = Number.MAX_SAFE_INTEGER } = {}) {
  return Number.isSafeInteger(value) && value >= min && value <= max;
}

function finiteNumber(value) {
  return typeof value === "number" && Number.isFinite(value);
}

function safeText(value, minLength, maxLength) {
  return typeof value === "string"
    && value.length >= minLength
    && value.length <= maxLength
    && !/[\u0000-\u001f\u007f]/u.test(value);
}

function canonicalTimestamp(value) {
  if (typeof value !== "string") return null;
  const milliseconds = Date.parse(value);
  if (!Number.isFinite(milliseconds)) return null;
  return new Date(milliseconds).toISOString() === value ? milliseconds : null;
}

function sha256(bytes) {
  return `sha256:${createHash("sha256").update(bytes).digest("hex")}`;
}

function requiredRustToolchain(platform, arch) {
  return platform === "win32" && arch === "x64"
    ? `${REQUIRED_RUST_VERSION}-x86_64-pc-windows-msvc`
    : REQUIRED_RUST_VERSION;
}

function requiredBackendBinaryPath(platform) {
  return platform === "win32"
    ? "backend/target/release/synthchat-hermes-backend.exe"
    : "backend/target/release/synthchat-hermes-backend";
}

function parseJson(bytes, code) {
  try {
    return JSON.parse(bytes.toString("utf8"));
  } catch {
    fail(code);
  }
}

function readBoundedRegularFile(path, maximumBytes, code) {
  let metadata;
  try {
    metadata = lstatSync(path);
  } catch {
    fail(`${code}_missing`);
  }
  requireCondition(metadata.isFile() && !metadata.isSymbolicLink(), `${code}_not_regular`);
  requireCondition(metadata.size > 0 && metadata.size <= maximumBytes, `${code}_size`);
  let bytes;
  try {
    bytes = readFileSync(path);
  } catch {
    fail(`${code}_read`);
  }
  requireCondition(bytes.length === metadata.size, `${code}_changed`);
  return bytes;
}

function workspaceRelativePath(value, code, prefix = undefined, allowMissing = false) {
  requireCondition(safeText(value, 1, 4096), code);
  requireCondition(!isAbsolute(value) && !value.includes("\\"), code);
  const absolute = resolve(workspace, value);
  const fromWorkspace = relative(workspace, absolute);
  requireCondition(
    fromWorkspace !== ".."
      && !fromWorkspace.startsWith(`..${sep}`)
      && !isAbsolute(fromWorkspace),
    code,
  );
  const normalized = fromWorkspace.split(sep).join("/");
  requireCondition(normalized === value && (!prefix || normalized.startsWith(prefix)), code);
  if (allowMissing) return absolute;
  let realWorkspace;
  let realAbsolute;
  try {
    realWorkspace = realpathSync.native(workspace);
    realAbsolute = realpathSync.native(absolute);
  } catch {
    fail(`${code}_unresolved`);
  }
  const realFromWorkspace = relative(realWorkspace, realAbsolute);
  requireCondition(
    realFromWorkspace !== ".."
      && !realFromWorkspace.startsWith(`..${sep}`)
      && !isAbsolute(realFromWorkspace),
    `${code}_realpath_escape`,
  );
  if (prefix) {
    const prefixAbsolute = resolve(workspace, prefix);
    let realPrefix;
    try {
      realPrefix = realpathSync.native(prefixAbsolute);
    } catch {
      fail(`${code}_prefix_unresolved`);
    }
    const realFromPrefix = relative(realPrefix, realAbsolute);
    requireCondition(
      realFromPrefix !== ".."
        && !realFromPrefix.startsWith(`..${sep}`)
        && !isAbsolute(realFromPrefix),
      `${code}_realpath_prefix_escape`,
    );
  }
  return absolute;
}

function expectedToolProbes(iterations, everyIterations) {
  return iterations === 0 ? 0 : Math.floor((iterations - 1) / everyIterations) + 1;
}

function validateConfiguration(configuration) {
  exactKeys(configuration, CONFIGURATION_KEYS, "report_configuration_shape");
  requireCondition(configuration.durationMs === EIGHT_HOURS_MS, "report_duration_configuration");
  requireCondition(safeInteger(configuration.concurrency, { min: 1, max: 32 }), "report_concurrency");
  requireCondition(safeInteger(configuration.cycleDelayMs, { max: 300_000 }), "report_cycle_delay");
  requireCondition(safeInteger(configuration.maxFailures, { min: 1, max: 10_000 }), "report_max_failures");
  requireCondition(configuration.maxIterations === 0, "report_max_iterations");
  requireCondition(safeInteger(configuration.providerDelayMs, { max: 60_000 }), "report_provider_delay");
  requireCondition(
    configuration.providerReplyBytes === 19 && configuration.providerReplyCodePoints === 19,
    "report_provider_reply",
  );
  requireCondition(
    safeInteger(configuration.latencySampleLimit, { min: 10, max: 50_000 }),
    "report_latency_capacity",
  );
  requireCondition(
    safeInteger(configuration.resourceIntervalMs, { min: 100, max: 300_000 }),
    "report_resource_interval",
  );
  requireCondition(configuration.resourceSamples === true, "report_resource_sampling_disabled");
  const requiredCapacity = Math.ceil(EIGHT_HOURS_MS / configuration.resourceIntervalMs) + 2;
  requireCondition(
    safeInteger(configuration.resourceSampleLimit, { min: requiredCapacity, max: 10_000 }),
    "report_resource_capacity",
  );
  requireCondition(
    safeInteger(configuration.toolEveryIterations, { min: 1, max: 1_000_000 }),
    "report_tool_interval",
  );
}

function validateWorkload(workload, configuration) {
  exactKeys(workload, WORKLOAD_KEYS, "report_workload_shape");
  const started = workload.iterationsStarted;
  requireCondition(safeInteger(started, { min: 1 }), "report_workload_empty");
  requireCondition(
    workload.failures === 0
      && workload.iterationsCompleted === started
      && workload.successes === started,
    "report_workload_conservation",
  );
  requireCondition(
    finiteNumber(workload.elapsedMs) && workload.elapsedMs >= EIGHT_HOURS_MS,
    "report_workload_duration",
  );
  const expected = expectedToolProbes(started, configuration.toolEveryIterations);
  requireCondition(workload.toolProbesExpected === expected, "report_workload_tool_probes");
  return { iterations: started, toolProbes: expected };
}

function validateProvider(provider, workload) {
  exactKeys(provider, PROVIDER_KEYS, "report_provider_shape");
  requireCondition(isRecord(provider.rejections) && Object.keys(provider.rejections).length === 0, "report_provider_rejections");
  requireCondition(
    provider.failures === 0
      && provider.pendingToolCalls === 0
      && provider.normalCompletions === workload.iterations - workload.toolProbes
      && provider.toolCallsIssued === workload.toolProbes
      && provider.toolResultsValidated === workload.toolProbes
      && provider.requests === workload.iterations + workload.toolProbes,
    "report_provider_conservation",
  );
  requireCondition(
    safeInteger(provider.maxActiveRequests, { min: 1, max: workload.concurrency }),
    "report_provider_concurrency",
  );
}

function validateEvents(events, workload, configuration) {
  const expected = {
    "message.completed": workload.iterations,
    "message.delta": workload.iterations * Math.min(2, configuration.providerReplyCodePoints),
    "message.started": workload.iterations,
    "run.completed": workload.iterations,
    "run.started": workload.iterations,
    "tool.completed": workload.toolProbes,
    "tool.started": workload.toolProbes,
    "usage.updated": workload.iterations + workload.toolProbes,
  };
  exactKeys(events, Object.keys(expected), "report_events_shape");
  requireCondition(
    Object.entries(expected).every(([name, count]) => events[name] === count),
    "report_event_conservation",
  );
}

function validateLatencyEntry(entry, expectedCount, expectedDropped, name) {
  exactKeys(entry, LATENCY_KEYS, `report_latency_shape_${name}`);
  requireCondition(
    entry.count === expectedCount
      && entry.failures === 0
      && entry.droppedSamples === expectedDropped,
    `report_latency_count_${name}`,
  );
  for (const key of ["min", "mean", "p50", "p95", "p99", "max"]) {
    requireCondition(finiteNumber(entry[key]) && entry[key] >= 0, `report_latency_value_${name}`);
  }
  requireCondition(
    entry.min <= entry.p50
      && entry.p50 <= entry.p95
      && entry.p95 <= entry.p99
      && entry.p99 <= entry.max
      && entry.mean >= entry.min
      && entry.mean <= entry.max,
    `report_latency_order_${name}`,
  );
}

function validateLatencies(latencies, iterations, latencySampleLimit) {
  const expectedNames = [...ITERATION_LATENCIES, ...SETUP_LATENCIES].sort();
  exactKeys(latencies, expectedNames, "report_latencies_shape");
  const iterationDropped = Math.max(0, iterations - latencySampleLimit);
  for (const name of ITERATION_LATENCIES) {
    validateLatencyEntry(latencies[name], iterations, iterationDropped, name);
  }
  for (const name of SETUP_LATENCIES) validateLatencyEntry(latencies[name], 1, 0, name);
}

function validateCleanup(cleanup) {
  exactKeys(cleanup, CLEANUP_KEYS, "report_cleanup_shape");
  requireCondition(
    cleanup.backendForced === false
      && cleanup.backendStopped === true
      && cleanup.providerStopped === true
      && cleanup.tempRemoved === true,
    "report_cleanup_failed",
  );
}

function validateResourceSample(sample, previous, index) {
  exactKeys(sample, RESOURCE_SAMPLE_KEYS, "report_resource_sample_shape");
  requireCondition(
    safeInteger(sample.elapsedMs)
      && (!previous || sample.elapsedMs > previous.elapsedMs),
    "report_resource_sample_time",
  );
  for (const key of [
    "runnerCpuSystemMicros",
    "runnerCpuUserMicros",
    "runnerHeapUsedBytes",
    "runnerRssBytes",
  ]) {
    requireCondition(safeInteger(sample[key]), "report_resource_sample_value");
  }
  requireCondition(
    sample.backendRssBytes === null
      || safeInteger(sample.backendRssBytes, { min: 1 }),
    "report_resource_backend_rss",
  );
  if (index > 0) {
    requireCondition(
      sample.runnerCpuSystemMicros >= previous.runnerCpuSystemMicros
        && sample.runnerCpuUserMicros >= previous.runnerCpuUserMicros,
      "report_resource_cpu_order",
    );
  }
}

function roundMetric(value) {
  return Math.round(value * 1_000_000) / 1_000_000;
}

function rssSlope(samples) {
  requireCondition(samples.length >= 2, "report_rss_samples_insufficient");
  const firstElapsed = samples[0].elapsedMs;
  let sumX = 0;
  let sumY = 0;
  for (const sample of samples) {
    sumX += (sample.elapsedMs - firstElapsed) / 3_600_000;
    sumY += sample.backendRssBytes / (1024 * 1024);
  }
  const meanX = sumX / samples.length;
  const meanY = sumY / samples.length;
  let numerator = 0;
  let denominator = 0;
  for (const sample of samples) {
    const x = (sample.elapsedMs - firstElapsed) / 3_600_000;
    const y = sample.backendRssBytes / (1024 * 1024);
    numerator += (x - meanX) * (y - meanY);
    denominator += (x - meanX) ** 2;
  }
  requireCondition(denominator > 0, "report_rss_time_span");
  return roundMetric(numerator / denominator);
}

function calculateRssMetrics(samples, intervalMs, finalWindowHours) {
  const available = samples.filter((sample) => sample.backendRssBytes !== null);
  requireCondition(available.length >= 2, "report_rss_samples_insufficient");
  const timelineStart = samples[0].elapsedMs;
  const timelineEnd = samples.at(-1).elapsedMs;
  requireCondition(
    available[0].elapsedMs <= timelineStart + intervalMs * 2
      && available.at(-1).elapsedMs >= timelineEnd - intervalMs * 2,
    "report_rss_endpoint_coverage",
  );
  const finalStart = timelineEnd - finalWindowHours * 3_600_000;
  const finalWindow = available.filter((sample) => sample.elapsedMs >= finalStart);
  requireCondition(finalWindow.length >= 2, "report_rss_final_window_insufficient");
  requireCondition(
    finalWindow[0].elapsedMs <= finalStart + intervalMs * 2
      && finalWindow.at(-1).elapsedMs >= timelineEnd - intervalMs * 2,
    "report_rss_final_window_coverage",
  );
  const values = available.map((sample) => sample.backendRssBytes / (1024 * 1024));
  return {
    availableSamples: available.length,
    finalWindowSlopeMiBPerHour: rssSlope(finalWindow),
    firstMiB: roundMetric(values[0]),
    fullWindowSlopeMiBPerHour: rssSlope(available),
    lastMiB: roundMetric(values.at(-1)),
    peakMiB: roundMetric(Math.max(...values)),
  };
}

function validateResources(resources, configuration, reportDurationMs, workloadElapsedMs) {
  exactKeys(resources, RESOURCE_KEYS, "report_resources_shape");
  requireCondition(resources.dropped === 0, "report_resources_dropped");
  requireCondition(safeInteger(resources.skipped), "report_resources_skipped");
  requireCondition(Array.isArray(resources.samples), "report_resources_samples");
  requireCondition(
    resources.samples.length >= 2
      && resources.samples.length <= configuration.resourceSampleLimit,
    "report_resources_sample_count",
  );
  let previous;
  resources.samples.forEach((sample, index) => {
    validateResourceSample(sample, previous, index);
    previous = sample;
  });
  const unavailable = resources.samples.filter((sample) => sample.backendRssBytes === null).length;
  requireCondition(
    resources.backendRssUnavailable === unavailable,
    "report_resources_unavailable_count",
  );
  const first = resources.samples[0].elapsedMs;
  const last = resources.samples.at(-1).elapsedMs;
  const totalOverheadMs = reportDurationMs - workloadElapsedMs;
  requireCondition(
    totalOverheadMs >= 0
      && first <= totalOverheadMs
      && last >= workloadElapsedMs
      && last <= reportDurationMs,
    "report_resources_timeline",
  );
  const span = last - first;
  requireCondition(
    span >= EIGHT_HOURS_MS - configuration.resourceIntervalMs * 2,
    "report_resources_full_window",
  );
  const scheduledLowerBound = Math.floor(span / configuration.resourceIntervalMs);
  requireCondition(
    resources.samples.length + resources.skipped + 1 >= scheduledLowerBound,
    "report_resources_schedule_gap",
  );
  const expectedSamples = Math.floor(EIGHT_HOURS_MS / configuration.resourceIntervalMs) + 1;
  const maximumSkipped = Math.ceil(expectedSamples * 0.01);
  const maximumUnavailable = Math.ceil(resources.samples.length * 0.01);
  requireCondition(resources.skipped <= maximumSkipped, "report_resources_skipped_quality");
  requireCondition(unavailable <= maximumUnavailable, "report_resources_unavailable_quality");
  requireCondition(
    resources.samples.length >= expectedSamples - resources.skipped - 2,
    "report_resources_sample_density",
  );
  return { sampleCount: resources.samples.length, unavailable };
}

function validateRawReport(report) {
  exactKeys(report, REPORT_TOP_LEVEL_KEYS, "report_shape");
  requireCondition(report.schemaVersion === 2, "report_schema_version");
  requireCondition(report.mode === "pilot", "report_mode");
  requireCondition(report.status === "passed", "report_status");
  const startedAt = canonicalTimestamp(report.startedAt);
  const finishedAt = canonicalTimestamp(report.finishedAt);
  requireCondition(startedAt !== null && finishedAt !== null && finishedAt > startedAt, "report_timestamps");
  requireCondition(
    safeInteger(report.durationMs, { min: EIGHT_HOURS_MS })
      && Math.abs(finishedAt - startedAt - report.durationMs) <= 2_000,
    "report_wall_duration",
  );
  validateConfiguration(report.configuration);
  const workload = validateWorkload(report.workload, report.configuration);
  requireCondition(report.durationMs >= report.workload.elapsedMs, "report_duration_order");
  validateProvider(report.provider, { ...workload, concurrency: report.configuration.concurrency });
  validateEvents(report.events, workload, report.configuration);
  validateLatencies(
    report.latenciesMs,
    workload.iterations,
    report.configuration.latencySampleLimit,
  );
  validateCleanup(report.cleanup);
  requireCondition(Array.isArray(report.failures) && report.failures.length === 0, "report_failures");
  requireCondition(report.failuresDropped === 0, "report_failures_dropped");
  exactKeys(report.backendLogs, ["stderrBytes", "stdoutBytes"], "report_backend_logs_shape");
  requireCondition(
    safeInteger(report.backendLogs.stderrBytes) && safeInteger(report.backendLogs.stdoutBytes),
    "report_backend_logs",
  );
  const resources = validateResources(
    report.resources,
    report.configuration,
    report.durationMs,
    report.workload.elapsedMs,
  );
  return { ...workload, ...resources, finishedAt };
}

function validateManifestShape(manifest, context) {
  exactKeys(manifest, MANIFEST_KEYS, "manifest_shape");
  requireCondition(manifest.schemaVersion === 1, "manifest_schema_version");
  requireCondition(manifest.kind === "synthchat-mixed-runtime-candidate-evidence", "manifest_kind");

  exactKeys(manifest.report, REPORT_BINDING_KEYS, "manifest_report_shape");
  const reportAbsolute = workspaceRelativePath(
    manifest.report.path,
    "manifest_report_path",
    REPORT_PREFIX,
    context.selfTest,
  );
  requireCondition(reportAbsolute === context.reportPath, "manifest_report_path_mismatch");
  requireCondition(safeInteger(manifest.report.bytes, { min: 1, max: REPORT_MAX_BYTES }), "manifest_report_bytes");
  requireCondition(HASH_PATTERN.test(manifest.report.sha256), "manifest_report_hash_format");

  exactKeys(manifest.candidate, CANDIDATE_KEYS, "manifest_candidate_shape");
  requireCondition(COMMIT_PATTERN.test(manifest.candidate.gitCommit), "manifest_commit_format");
  requireCondition(
    context.selfTest || !/^([0-9a-f])\1+$/u.test(manifest.candidate.gitCommit),
    "manifest_commit_placeholder",
  );
  requireCondition(SAFE_PLATFORM.has(manifest.candidate.platform), "manifest_platform");
  requireCondition(SAFE_ARCH.has(manifest.candidate.arch), "manifest_arch");
  requireCondition(manifest.candidate.nodeVersion === REQUIRED_NODE_VERSION, "manifest_node_version");
  requireCondition(
    manifest.candidate.rustVersion
      === requiredRustToolchain(manifest.candidate.platform, manifest.candidate.arch),
    "manifest_rust_version",
  );
  requireCondition(
    HASH_PATTERN.test(manifest.candidate.effectiveConfigSha256),
    "manifest_effective_config_hash_format",
  );
  requireCondition(manifest.candidate.platform === context.platform, "manifest_platform_mismatch");
  requireCondition(manifest.candidate.arch === context.arch, "manifest_arch_mismatch");
  requireCondition(manifest.candidate.nodeVersion === context.nodeVersion, "manifest_node_runtime_mismatch");
  requireCondition(manifest.candidate.rustVersion === context.rustVersion, "manifest_rust_runtime_mismatch");
  requireCondition(manifest.candidate.gitCommit === context.currentGit.commit, "manifest_commit_mismatch");
  requireCondition(context.cleanCheckout === true, "manifest_checkout_dirty");
  exactKeys(manifest.candidate.backendBinary, BINARY_KEYS, "manifest_backend_shape");
  requireCondition(
    manifest.candidate.backendBinary.path
      === requiredBackendBinaryPath(manifest.candidate.platform),
    "manifest_backend_release_path",
  );
  const backendAbsolute = workspaceRelativePath(
    manifest.candidate.backendBinary.path,
    "manifest_backend_path",
    undefined,
    context.selfTest,
  );
  requireCondition(HASH_PATTERN.test(manifest.candidate.backendBinary.sha256), "manifest_backend_hash_format");

  exactKeys(manifest.command, COMMAND_KEYS, "manifest_command_shape");
  requireCondition(manifest.command.executable === "node", "manifest_command_executable");
  requireCondition(
    Array.isArray(manifest.command.arguments)
      && manifest.command.arguments.every((argument) => safeText(argument, 1, 4096)),
    "manifest_command_arguments",
  );

  exactKeys(manifest.rssReview, RSS_REVIEW_KEYS, "manifest_rss_review_shape");
  return { backendAbsolute };
}

function gitObjectIdMatches(objectFormat, value) {
  const length = objectFormat === "sha1" ? 40 : objectFormat === "sha256" ? 64 : 0;
  return length > 0
    && typeof value === "string"
    && value.length === length
    && COMMIT_PATTERN.test(value);
}

function validateProvenance(provenance, manifest, context, backendBytes) {
  exactKeys(provenance, PROVENANCE_KEYS, "report_provenance_shape");
  requireCondition(provenance.schemaVersion === 1, "report_provenance_schema_version");
  requireCondition(SAFE_PLATFORM.has(provenance.platform), "report_provenance_platform");
  requireCondition(SAFE_ARCH.has(provenance.arch), "report_provenance_arch");
  requireCondition(provenance.nodeVersion === REQUIRED_NODE_VERSION, "report_provenance_node_version");
  requireCondition(
    provenance.rustVersion === requiredRustToolchain(provenance.platform, provenance.arch),
    "report_provenance_rust_version",
  );
  requireCondition(
    provenance.platform === manifest.candidate.platform
      && provenance.platform === context.platform
      && provenance.arch === manifest.candidate.arch
      && provenance.arch === context.arch
      && provenance.nodeVersion === manifest.candidate.nodeVersion
      && provenance.nodeVersion === context.nodeVersion
      && provenance.rustVersion === manifest.candidate.rustVersion
      && provenance.rustVersion === context.rustVersion,
    "report_provenance_runtime_mismatch",
  );

  exactKeys(provenance.git, GIT_PROVENANCE_KEYS, "report_provenance_git_shape");
  requireCondition(
    provenance.git.objectFormat === "sha1" || provenance.git.objectFormat === "sha256",
    "report_provenance_git_object_format",
  );
  requireCondition(
    gitObjectIdMatches(provenance.git.objectFormat, provenance.git.commit),
    "report_provenance_git_commit_format",
  );
  requireCondition(
    gitObjectIdMatches(provenance.git.objectFormat, provenance.git.tree),
    "report_provenance_git_tree_format",
  );
  requireCondition(provenance.git.worktreeClean === true, "report_provenance_git_dirty");
  requireCondition(
    provenance.git.objectFormat === context.currentGit.objectFormat
      && provenance.git.commit === context.currentGit.commit
      && provenance.git.commit === manifest.candidate.gitCommit
      && provenance.git.tree === context.currentGit.tree,
    "report_provenance_git_mismatch",
  );

  exactKeys(provenance.verifier, FILE_PROVENANCE_KEYS, "report_provenance_verifier_shape");
  requireCondition(
    provenance.verifier.path === "scripts/verify-mixed-runtime.mjs",
    "report_provenance_verifier_path",
  );
  const verifierAbsolute = workspaceRelativePath(
    provenance.verifier.path,
    "report_provenance_verifier_path",
    undefined,
    context.selfTest,
  );
  requireCondition(
    safeInteger(provenance.verifier.bytes, { min: 1, max: VERIFIER_MAX_BYTES }),
    "report_provenance_verifier_bytes",
  );
  requireCondition(
    HASH_PATTERN.test(provenance.verifier.sha256),
    "report_provenance_verifier_hash_format",
  );
  const verifierBytes = context.verifierBytes
    ?? readBoundedRegularFile(verifierAbsolute, VERIFIER_MAX_BYTES, "producer_verifier");
  requireCondition(
    provenance.verifier.bytes === verifierBytes.length
      && provenance.verifier.sha256 === sha256(verifierBytes),
    "report_provenance_verifier_mismatch",
  );

  exactKeys(provenance.backend, FILE_PROVENANCE_KEYS, "report_provenance_backend_shape");
  requireCondition(
    provenance.backend.path === manifest.candidate.backendBinary.path,
    "report_provenance_backend_path_mismatch",
  );
  requireCondition(
    safeInteger(provenance.backend.bytes, { min: 1, max: BINARY_MAX_BYTES }),
    "report_provenance_backend_bytes",
  );
  requireCondition(
    HASH_PATTERN.test(provenance.backend.sha256),
    "report_provenance_backend_hash_format",
  );
  requireCondition(
    provenance.backend.bytes === backendBytes.length
      && provenance.backend.sha256 === sha256(backendBytes)
      && provenance.backend.sha256 === manifest.candidate.backendBinary.sha256,
    "report_provenance_backend_mismatch",
  );

  requireCondition(
    HASH_PATTERN.test(provenance.effectiveConfigSha256),
    "report_provenance_effective_config_hash_format",
  );
  requireCondition(
    provenance.effectiveConfigSha256 === manifest.candidate.effectiveConfigSha256,
    "report_provenance_effective_config_mismatch",
  );
  requireCondition(HASH_PATTERN.test(provenance.argvSha256), "report_provenance_argv_hash_format");
  requireCondition(
    provenance.argvSha256 === sha256(JSON.stringify(manifest.command.arguments.slice(1))),
    "report_provenance_argv_mismatch",
  );
  requireCondition(
    Array.isArray(provenance.overrideNames)
      && provenance.overrideNames.every((name) => safeText(name, 1, 256)),
    "report_provenance_override_names",
  );
  requireCondition(provenance.overrideNames.length === 0, "report_provenance_overrides_present");
  requireCondition(
    HASH_PATTERN.test(provenance.overrideNamesSha256),
    "report_provenance_override_hash_format",
  );
  requireCondition(
    provenance.overrideNamesSha256 === sha256(JSON.stringify([])),
    "report_provenance_override_hash_mismatch",
  );
}

function expectedCommand(report, manifest) {
  const configuration = report.configuration;
  return [
    "scripts/verify-mixed-runtime.mjs",
    "--duration-seconds",
    "28800",
    "--concurrency",
    String(configuration.concurrency),
    "--cycle-delay-ms",
    String(configuration.cycleDelayMs),
    "--max-failures",
    String(configuration.maxFailures),
    "--latency-sample-limit",
    String(configuration.latencySampleLimit),
    "--provider-delay-ms",
    String(configuration.providerDelayMs),
    "--resource-interval-ms",
    String(configuration.resourceIntervalMs),
    "--resource-sample-limit",
    String(configuration.resourceSampleLimit),
    "--resource-samples",
    "--tool-every-iterations",
    String(configuration.toolEveryIterations),
    "--backend-bin",
    manifest.candidate.backendBinary.path,
    "--skip-build",
    "--output",
    manifest.report.path,
  ];
}

function validateReview(review, report, rawSummary, context) {
  requireCondition(review.decision === "accepted", "manifest_rss_decision");
  requireCondition(
    safeText(review.reviewer, 3, 200)
      && (context.selfTest || !/^(?:n\/?a|self[- ]?test|todo|unknown)$/iu.test(review.reviewer)),
    "manifest_rss_reviewer",
  );
  requireCondition(safeText(review.summary, 20, 2_000), "manifest_rss_summary");
  const reviewedAt = canonicalTimestamp(review.reviewedAt);
  requireCondition(
    reviewedAt !== null
      && reviewedAt >= rawSummary.finishedAt
      && reviewedAt <= Date.now() + 300_000,
    "manifest_rss_reviewed_at",
  );
  requireCondition(
    finiteNumber(review.finalWindowHours)
      && review.finalWindowHours >= 1
      && review.finalWindowHours <= 4,
    "manifest_rss_final_window",
  );
  const calculated = calculateRssMetrics(
    report.resources.samples,
    report.configuration.resourceIntervalMs,
    review.finalWindowHours,
  );
  const expected = {
    availableSamples: calculated.availableSamples,
    droppedSamples: report.resources.dropped,
    finalWindowSlopeMiBPerHour: calculated.finalWindowSlopeMiBPerHour,
    firstMiB: calculated.firstMiB,
    fullWindowSlopeMiBPerHour: calculated.fullWindowSlopeMiBPerHour,
    lastMiB: calculated.lastMiB,
    peakMiB: calculated.peakMiB,
    sampleCount: rawSummary.sampleCount,
    skippedSamples: report.resources.skipped,
    unavailableSamples: rawSummary.unavailable,
  };
  requireCondition(
    Object.entries(expected).every(([key, value]) => review[key] === value),
    "manifest_rss_metrics",
  );
}

function validateEvidence(report, manifest, context) {
  const canonicalReport = Buffer.from(`${JSON.stringify(report)}\n`, "utf8");
  requireCondition(context.reportBytes.equals(canonicalReport), "report_json_not_canonical");
  const rawSummary = validateRawReport(report);
  const { backendAbsolute } = validateManifestShape(manifest, context);
  requireCondition(context.reportBytes.length === manifest.report.bytes, "manifest_report_size_mismatch");
  requireCondition(sha256(context.reportBytes) === manifest.report.sha256, "manifest_report_hash_mismatch");
  const backendBytes = context.backendBytes
    ?? readBoundedRegularFile(backendAbsolute, BINARY_MAX_BYTES, "backend_binary");
  requireCondition(
    sha256(backendBytes) === manifest.candidate.backendBinary.sha256,
    "manifest_backend_hash_mismatch",
  );
  requireCondition(
    manifest.command.arguments.length === expectedCommand(report, manifest).length
      && manifest.command.arguments.every(
        (argument, index) => argument === expectedCommand(report, manifest)[index],
      ),
    "manifest_command_mismatch",
  );
  validateProvenance(report.provenance, manifest, context, backendBytes);
  validateReview(manifest.rssReview, report, rawSummary, context);
  return {
    candidateCommit: manifest.candidate.gitCommit,
    reportSha256: manifest.report.sha256,
    rss: {
      availableSamples: manifest.rssReview.availableSamples,
      finalWindowSlopeMiBPerHour: manifest.rssReview.finalWindowSlopeMiBPerHour,
      fullWindowSlopeMiBPerHour: manifest.rssReview.fullWindowSlopeMiBPerHour,
      sampleCount: manifest.rssReview.sampleCount,
    },
    workload: {
      iterations: rawSummary.iterations,
      toolProbes: rawSummary.toolProbes,
    },
  };
}

function currentGitContext() {
  try {
    const options = {
      cwd: workspace,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
      windowsHide: true,
    };
    return {
      commit: execFileSync("git", ["rev-parse", "--verify", "HEAD"], options).trim(),
      objectFormat: execFileSync("git", ["rev-parse", "--show-object-format"], options).trim(),
      tree: execFileSync("git", ["rev-parse", "--verify", "HEAD^{tree}"], options).trim(),
    };
  } catch {
    fail("git_context_unavailable");
  }
}

function candidateCheckoutIsClean(reportPath, manifestPath) {
  let output;
  try {
    output = execFileSync(
      "git",
      ["status", "--porcelain=v1", "-z", "--untracked-files=all"],
      {
        cwd: workspace,
        encoding: "utf8",
        stdio: ["ignore", "pipe", "ignore"],
        windowsHide: true,
      },
    );
  } catch {
    fail("git_status_unavailable");
  }
  const allowed = new Set();
  for (const path of [reportPath, manifestPath]) {
    const fromWorkspace = relative(workspace, path);
    if (
      fromWorkspace !== ".."
      && !fromWorkspace.startsWith(`..${sep}`)
      && !isAbsolute(fromWorkspace)
    ) {
      allowed.add(fromWorkspace.split(sep).join("/"));
    }
  }
  return output.split("\0").filter(Boolean).every((entry) => (
    entry.startsWith("?? ") && allowed.has(entry.slice(3).replaceAll("\\", "/"))
  ));
}

function latencyEntry(count, sampleLimit) {
  return {
    count,
    droppedSamples: Math.max(0, count - sampleLimit),
    failures: 0,
    max: 4,
    mean: 2,
    min: 1,
    p50: 2,
    p95: 3,
    p99: 4,
  };
}

function syntheticReport({ backendBytes, backendPath, effectiveConfigSha256, git, verifierBytes }) {
  const iterations = 10_000;
  const toolProbes = 1_000;
  const configuration = {
    concurrency: 2,
    cycleDelayMs: 3_000,
    durationMs: EIGHT_HOURS_MS,
    maxFailures: 25,
    maxIterations: 0,
    providerDelayMs: 10,
    providerReplyBytes: 19,
    providerReplyCodePoints: 19,
    latencySampleLimit: 5_000,
    resourceIntervalMs: 5_000,
    resourceSampleLimit: 5_762,
    resourceSamples: true,
    toolEveryIterations: 10,
  };
  const samples = Array.from({ length: 5_761 }, (_, index) => ({
    backendRssBytes: 40 * 1024 * 1024 + index * 128,
    elapsedMs: 30_000 + index * 5_000,
    runnerCpuSystemMicros: index * 100,
    runnerCpuUserMicros: index * 200,
    runnerHeapUsedBytes: 16 * 1024 * 1024 + index,
    runnerRssBytes: 30 * 1024 * 1024 + index,
  }));
  const latenciesMs = {};
  for (const name of ITERATION_LATENCIES) {
    latenciesMs[name] = latencyEntry(iterations, configuration.latencySampleLimit);
  }
  for (const name of SETUP_LATENCIES) {
    latenciesMs[name] = latencyEntry(1, configuration.latencySampleLimit);
  }
  return {
    backendLogs: { stderrBytes: 0, stdoutBytes: 64 },
    cleanup: {
      backendForced: false,
      backendStopped: true,
      providerStopped: true,
      tempRemoved: true,
    },
    configuration,
    durationMs: 28_860_000,
    events: {
      "message.completed": iterations,
      "message.delta": iterations * 2,
      "message.started": iterations,
      "run.completed": iterations,
      "run.started": iterations,
      "tool.completed": toolProbes,
      "tool.started": toolProbes,
      "usage.updated": iterations + toolProbes,
    },
    failures: [],
    failuresDropped: 0,
    finishedAt: "2026-01-01T08:01:00.000Z",
    latenciesMs,
    mode: "pilot",
    provider: {
      failures: 0,
      maxActiveRequests: 2,
      normalCompletions: iterations - toolProbes,
      pendingToolCalls: 0,
      rejections: {},
      requests: iterations + toolProbes,
      toolCallsIssued: toolProbes,
      toolResultsValidated: toolProbes,
    },
    provenance: {
      arch: process.arch,
      argvSha256: sha256(JSON.stringify([])),
      backend: {
        bytes: backendBytes.length,
        path: backendPath,
        sha256: sha256(backendBytes),
      },
      effectiveConfigSha256,
      git: { ...git, worktreeClean: true },
      nodeVersion: REQUIRED_NODE_VERSION,
      overrideNames: [],
      overrideNamesSha256: sha256(JSON.stringify([])),
      platform: process.platform,
      rustVersion: requiredRustToolchain(process.platform, process.arch),
      schemaVersion: 1,
      verifier: {
        bytes: verifierBytes.length,
        path: "scripts/verify-mixed-runtime.mjs",
        sha256: sha256(verifierBytes),
      },
    },
    resources: {
      backendRssUnavailable: 0,
      dropped: 0,
      samples,
      skipped: 0,
    },
    schemaVersion: 2,
    startedAt: "2026-01-01T00:00:00.000Z",
    status: "passed",
    workload: {
      elapsedMs: 28_800_500,
      failures: 0,
      iterationsCompleted: iterations,
      iterationsStarted: iterations,
      successes: iterations,
      toolProbesExpected: toolProbes,
    },
  };
}

function bindSyntheticReportBytes(fixture) {
  fixture.reportBytes = Buffer.from(`${JSON.stringify(fixture.report)}\n`, "utf8");
  fixture.manifest.report.bytes = fixture.reportBytes.length;
  fixture.manifest.report.sha256 = sha256(fixture.reportBytes);
  fixture.context.reportBytes = fixture.reportBytes;
}

function bindSyntheticFixture(fixture) {
  fixture.report.provenance.argvSha256 = sha256(
    JSON.stringify(fixture.manifest.command.arguments.slice(1)),
  );
  Object.assign(fixture.report.provenance.backend, {
    bytes: fixture.context.backendBytes.length,
    path: fixture.manifest.candidate.backendBinary.path,
    sha256: sha256(fixture.context.backendBytes),
  });
  Object.assign(fixture.report.provenance.verifier, {
    bytes: fixture.context.verifierBytes.length,
    path: "scripts/verify-mixed-runtime.mjs",
    sha256: sha256(fixture.context.verifierBytes),
  });
  const metrics = calculateRssMetrics(
    fixture.report.resources.samples,
    fixture.report.configuration.resourceIntervalMs,
    fixture.manifest.rssReview.finalWindowHours,
  );
  Object.assign(fixture.manifest.rssReview, {
    availableSamples: metrics.availableSamples,
    droppedSamples: fixture.report.resources.dropped,
    finalWindowSlopeMiBPerHour: metrics.finalWindowSlopeMiBPerHour,
    firstMiB: metrics.firstMiB,
    fullWindowSlopeMiBPerHour: metrics.fullWindowSlopeMiBPerHour,
    lastMiB: metrics.lastMiB,
    peakMiB: metrics.peakMiB,
    sampleCount: fixture.report.resources.samples.length,
    skippedSamples: fixture.report.resources.skipped,
    unavailableSamples: fixture.report.resources.samples
      .filter((sample) => sample.backendRssBytes === null).length,
  });
  bindSyntheticReportBytes(fixture);
}

function syntheticFixture() {
  const reportPath = `${REPORT_PREFIX}synthetic-self-test-report.json`;
  const backendPath = requiredBackendBinaryPath(process.platform);
  const backendBytes = Buffer.from("synthetic backend bytes used only in memory", "utf8");
  const verifierBytes = Buffer.from("synthetic verifier bytes used only in memory", "utf8");
  const currentGit = {
    commit: "a".repeat(40),
    objectFormat: "sha1",
    tree: "c".repeat(40),
  };
  const effectiveConfigSha256 = sha256(JSON.stringify({ fixture: "mixed-runtime-v2" }));
  const report = syntheticReport({
    backendBytes,
    backendPath,
    effectiveConfigSha256,
    git: currentGit,
    verifierBytes,
  });
  const manifest = {
    candidate: {
      arch: process.arch,
      backendBinary: { path: backendPath, sha256: sha256(backendBytes) },
      effectiveConfigSha256,
      gitCommit: currentGit.commit,
      nodeVersion: REQUIRED_NODE_VERSION,
      platform: process.platform,
      rustVersion: requiredRustToolchain(process.platform, process.arch),
    },
    command: { arguments: [], executable: "node" },
    kind: "synthchat-mixed-runtime-candidate-evidence",
    report: { bytes: 1, path: reportPath, sha256: `sha256:${"0".repeat(64)}` },
    rssReview: {
      availableSamples: 0,
      decision: "accepted",
      droppedSamples: 0,
      finalWindowHours: 4,
      finalWindowSlopeMiBPerHour: 0,
      firstMiB: 0,
      fullWindowSlopeMiBPerHour: 0,
      lastMiB: 0,
      peakMiB: 0,
      reviewedAt: "2026-01-01T09:00:00.000Z",
      reviewer: "synthetic-self-test",
      sampleCount: 0,
      skippedSamples: 0,
      summary: "Synthetic in-memory review used only by the verifier self-test.",
      unavailableSamples: 0,
    },
    schemaVersion: 1,
  };
  const context = {
    arch: process.arch,
    backendBytes,
    cleanCheckout: true,
    currentGit,
    nodeVersion: REQUIRED_NODE_VERSION,
    platform: process.platform,
    reportBytes: Buffer.alloc(0),
    reportPath: resolve(workspace, reportPath),
    rustVersion: requiredRustToolchain(process.platform, process.arch),
    selfTest: true,
    verifierBytes,
  };
  const fixture = { context, manifest, report, reportBytes: Buffer.alloc(0) };
  manifest.command.arguments = expectedCommand(report, manifest);
  bindSyntheticFixture(fixture);
  return fixture;
}

function cloneFixture() {
  const fixture = syntheticFixture();
  fixture.report = structuredClone(fixture.report);
  fixture.manifest = structuredClone(fixture.manifest);
  fixture.context = {
    ...fixture.context,
    backendBytes: Buffer.from(fixture.context.backendBytes),
    currentGit: { ...fixture.context.currentGit },
    verifierBytes: Buffer.from(fixture.context.verifierBytes),
  };
  return fixture;
}

function expectFailure(checks, name, expectedCode, mutate, rebind = true) {
  const fixture = cloneFixture();
  mutate(fixture);
  if (rebind) {
    fixture.manifest.command.arguments = expectedCommand(fixture.report, fixture.manifest);
    bindSyntheticFixture(fixture);
  }
  try {
    validateEvidence(fixture.report, fixture.manifest, fixture.context);
  } catch (error) {
    if (error instanceof EvidenceError && error.code === expectedCode) {
      checks.push(name);
      return;
    }
    throw error;
  }
  fail(`self_test_expected_failure_${name}`);
}

function runSelfTest() {
  const checks = [];
  requireCondition(
    requiredBackendBinaryPath("win32")
      === "backend/target/release/synthchat-hermes-backend.exe"
      && requiredBackendBinaryPath("linux")
        === "backend/target/release/synthchat-hermes-backend"
      && requiredBackendBinaryPath("darwin")
        === "backend/target/release/synthchat-hermes-backend",
    "self_test_release_backend_paths_failed",
  );
  checks.push("release_backend_paths");
  requireCondition(
    expectedToolProbes(0, 10) === 0
      && expectedToolProbes(1, 10) === 1
      && expectedToolProbes(10, 10) === 1
      && expectedToolProbes(11, 10) === 2,
    "self_test_tool_probe_boundaries_failed",
  );
  checks.push("tool_probe_boundaries");
  const slopeSamples = (first, second) => [
    { backendRssBytes: first * 1024 * 1024, elapsedMs: 0 },
    { backendRssBytes: second * 1024 * 1024, elapsedMs: 3_600_000 },
  ];
  requireCondition(
    rssSlope(slopeSamples(10, 11)) === 1
      && rssSlope(slopeSamples(10, 10)) === 0
      && rssSlope(slopeSamples(11, 10)) === -1,
    "self_test_rss_slope_failed",
  );
  checks.push("rss_slope_signs");
  const positive = syntheticFixture();
  validateEvidence(positive.report, positive.manifest, positive.context);
  checks.push("synthetic_positive");

  const alternateLatencyCapacity = cloneFixture();
  alternateLatencyCapacity.report.configuration.latencySampleLimit = 4_000;
  for (const name of ITERATION_LATENCIES) {
    alternateLatencyCapacity.report.latenciesMs[name].droppedSamples = 6_000;
  }
  alternateLatencyCapacity.manifest.candidate.effectiveConfigSha256 = sha256(
    JSON.stringify({ fixture: "mixed-runtime-v2", latencySampleLimit: 4_000 }),
  );
  alternateLatencyCapacity.report.provenance.effectiveConfigSha256 =
    alternateLatencyCapacity.manifest.candidate.effectiveConfigSha256;
  alternateLatencyCapacity.manifest.command.arguments = expectedCommand(
    alternateLatencyCapacity.report,
    alternateLatencyCapacity.manifest,
  );
  bindSyntheticFixture(alternateLatencyCapacity);
  validateEvidence(
    alternateLatencyCapacity.report,
    alternateLatencyCapacity.manifest,
    alternateLatencyCapacity.context,
  );
  checks.push("synthetic_dynamic_latency_capacity");

  const accountedGap = cloneFixture();
  accountedGap.report.resources.samples[100].backendRssBytes = null;
  accountedGap.report.resources.backendRssUnavailable = 1;
  accountedGap.manifest.command.arguments = expectedCommand(accountedGap.report, accountedGap.manifest);
  bindSyntheticFixture(accountedGap);
  validateEvidence(accountedGap.report, accountedGap.manifest, accountedGap.context);
  checks.push("synthetic_accounted_rss_gap");

  expectFailure(checks, "reject_failed_status", "report_status", (fixture) => {
    fixture.report.status = "failed";
  });
  expectFailure(checks, "reject_raw_v1", "report_schema_version", (fixture) => {
    fixture.report.schemaVersion = 1;
  });
  expectFailure(checks, "reject_short_duration", "report_duration_configuration", (fixture) => {
    fixture.report.configuration.durationMs = 3_600_000;
  });
  expectFailure(checks, "reject_workload_drift", "report_workload_conservation", (fixture) => {
    fixture.report.workload.successes -= 1;
  });
  expectFailure(checks, "reject_provider_drift", "report_provider_conservation", (fixture) => {
    fixture.report.provider.requests -= 1;
  });
  expectFailure(checks, "reject_event_drift", "report_event_conservation", (fixture) => {
    fixture.report.events["tool.completed"] -= 1;
  });
  expectFailure(checks, "reject_forced_cleanup", "report_cleanup_failed", (fixture) => {
    fixture.report.cleanup.backendForced = true;
  });
  expectFailure(checks, "reject_dropped_resources", "report_resources_dropped", (fixture) => {
    fixture.report.resources.dropped = 1;
  });
  expectFailure(checks, "reject_unaccounted_rss_gap", "report_resources_unavailable_count", (fixture) => {
    fixture.report.resources.samples[100].backendRssBytes = null;
  });
  expectFailure(checks, "reject_sparse_backend_rss", "report_resources_unavailable_quality", (fixture) => {
    for (let index = 100; index < 200; index += 1) {
      fixture.report.resources.samples[index].backendRssBytes = null;
    }
    fixture.report.resources.backendRssUnavailable = 100;
  });
  expectFailure(checks, "reject_truncated_resource_window", "report_resources_timeline", (fixture) => {
    fixture.report.resources.samples = fixture.report.resources.samples.slice(0, 100);
    bindSyntheticReportBytes(fixture);
  }, false);
  expectFailure(checks, "reject_report_hash", "manifest_report_hash_mismatch", (fixture) => {
    fixture.manifest.report.sha256 = `sha256:${"f".repeat(64)}`;
  }, false);
  expectFailure(checks, "reject_noncanonical_report", "report_json_not_canonical", (fixture) => {
    fixture.context.reportBytes = Buffer.from(JSON.stringify(fixture.report, null, 2), "utf8");
  }, false);
  expectFailure(checks, "reject_backend_hash", "manifest_backend_hash_mismatch", (fixture) => {
    fixture.manifest.candidate.backendBinary.sha256 = `sha256:${"e".repeat(64)}`;
  }, false);
  expectFailure(
    checks,
    "reject_nonrelease_backend_path",
    "manifest_backend_release_path",
    (fixture) => {
      const suffix = fixture.manifest.candidate.platform === "win32" ? ".exe" : "";
      fixture.manifest.candidate.backendBinary.path =
        `backend/target/debug/synthchat-hermes-backend${suffix}`;
    },
  );
  expectFailure(checks, "reject_commit_mismatch", "manifest_commit_mismatch", (fixture) => {
    fixture.manifest.candidate.gitCommit = "b".repeat(40);
  }, false);
  expectFailure(checks, "reject_dirty_checkout", "manifest_checkout_dirty", (fixture) => {
    fixture.context.cleanCheckout = false;
  }, false);
  expectFailure(checks, "reject_command_drift", "manifest_command_mismatch", (fixture) => {
    fixture.manifest.command.arguments[2] = "3600";
  }, false);
  expectFailure(checks, "reject_provenance_git_tree", "report_provenance_git_mismatch", (fixture) => {
    fixture.report.provenance.git.tree = "d".repeat(40);
  });
  expectFailure(checks, "reject_provenance_dirty", "report_provenance_git_dirty", (fixture) => {
    fixture.report.provenance.git.worktreeClean = false;
  });
  expectFailure(
    checks,
    "reject_provenance_verifier_hash",
    "report_provenance_verifier_mismatch",
    (fixture) => {
      fixture.report.provenance.verifier.sha256 = `sha256:${"d".repeat(64)}`;
      bindSyntheticReportBytes(fixture);
    },
    false,
  );
  expectFailure(
    checks,
    "reject_provenance_backend_hash",
    "report_provenance_backend_mismatch",
    (fixture) => {
      fixture.report.provenance.backend.sha256 = `sha256:${"d".repeat(64)}`;
      bindSyntheticReportBytes(fixture);
    },
    false,
  );
  expectFailure(
    checks,
    "reject_provenance_argv",
    "report_provenance_argv_mismatch",
    (fixture) => {
      fixture.report.provenance.argvSha256 = `sha256:${"d".repeat(64)}`;
      bindSyntheticReportBytes(fixture);
    },
    false,
  );
  expectFailure(
    checks,
    "reject_provenance_override",
    "report_provenance_overrides_present",
    (fixture) => {
      fixture.report.provenance.overrideNames = ["SYNTHCHAT_MIXED_VERIFY_DURATION_SECONDS"];
      fixture.report.provenance.overrideNamesSha256 = sha256(JSON.stringify([
        ["SYNTHCHAT_MIXED_VERIFY_DURATION_SECONDS", "28800"],
      ]));
      bindSyntheticReportBytes(fixture);
    },
    false,
  );
  expectFailure(
    checks,
    "reject_effective_config_mismatch",
    "report_provenance_effective_config_mismatch",
    (fixture) => {
      fixture.manifest.candidate.effectiveConfigSha256 = `sha256:${"d".repeat(64)}`;
    },
    false,
  );
  expectFailure(checks, "reject_rss_decision", "manifest_rss_decision", (fixture) => {
    fixture.manifest.rssReview.decision = "rejected";
  }, false);
  expectFailure(checks, "reject_rss_metric_drift", "manifest_rss_metrics", (fixture) => {
    fixture.manifest.rssReview.fullWindowSlopeMiBPerHour += 1;
  }, false);

  return {
    checks,
    mode: "self-test",
    schemaVersion: 1,
    status: "passed",
  };
}

function parseCli(argumentsList) {
  const values = new Map();
  for (let index = 0; index < argumentsList.length; index += 1) {
    const argument = argumentsList[index];
    requireCondition(argument.startsWith("--"), "cli_invalid_argument");
    const equals = argument.indexOf("=");
    const key = equals === -1 ? argument.slice(2) : argument.slice(2, equals);
    requireCondition(["help", "manifest", "report", "self-test"].includes(key), "cli_unknown_option");
    requireCondition(!values.has(key), "cli_duplicate_option");
    let value = equals === -1 ? undefined : argument.slice(equals + 1);
    if (["help", "self-test"].includes(key)) {
      requireCondition(value === undefined || value === "true", "cli_boolean_value");
      value = "true";
    } else if (value === undefined) {
      value = argumentsList[index + 1];
      requireCondition(value !== undefined && !value.startsWith("--"), "cli_missing_value");
      index += 1;
    }
    values.set(key, value);
  }
  return values;
}

function helpText() {
  return [
    "Usage:",
    "  node scripts/verify-mixed-runtime-evidence.mjs --report <raw.json> --manifest <candidate.json>",
    "  node scripts/verify-mixed-runtime-evidence.mjs --self-test",
    "",
    "The verifier reads files only. It does not create or approve release evidence.",
  ].join("\n");
}

function runCli() {
  const cli = parseCli(process.argv.slice(2));
  if (cli.get("help") === "true") {
    process.stdout.write(`${helpText()}\n`);
    return;
  }
  requireCondition(process.versions.node === REQUIRED_NODE_VERSION, "node_runtime_version");
  if (cli.get("self-test") === "true") {
    requireCondition(cli.size === 1, "cli_self_test_conflict");
    process.stdout.write(`${JSON.stringify(runSelfTest())}\n`);
    return;
  }
  requireCondition(cli.size === 2 && cli.has("report") && cli.has("manifest"), "cli_inputs_required");
  const reportPath = isAbsolute(cli.get("report"))
    ? resolve(cli.get("report"))
    : resolve(workspace, cli.get("report"));
  const manifestPath = isAbsolute(cli.get("manifest"))
    ? resolve(cli.get("manifest"))
    : resolve(workspace, cli.get("manifest"));
  const reportBytes = readBoundedRegularFile(reportPath, REPORT_MAX_BYTES, "report_file");
  const manifestBytes = readBoundedRegularFile(manifestPath, MANIFEST_MAX_BYTES, "manifest_file");
  const report = parseJson(reportBytes, "report_json_invalid");
  const manifest = parseJson(manifestBytes, "manifest_json_invalid");
  requireCondition(
    manifestBytes.equals(Buffer.from(`${JSON.stringify(manifest)}\n`, "utf8")),
    "manifest_json_not_canonical",
  );
  const result = validateEvidence(report, manifest, {
    arch: process.arch,
    cleanCheckout: candidateCheckoutIsClean(reportPath, manifestPath),
    currentGit: currentGitContext(),
    nodeVersion: process.versions.node,
    platform: process.platform,
    reportBytes,
    reportPath,
    rustVersion: requiredRustToolchain(process.platform, process.arch),
    selfTest: false,
  });
  process.stdout.write(`${JSON.stringify({
    ...result,
    schemaVersion: 1,
    status: "passed",
  })}\n`);
}

try {
  runCli();
} catch (error) {
  const code = error instanceof EvidenceError ? error.code : "unexpected_failure";
  process.stderr.write(`FAIL: ${code}\n`);
  process.exitCode = 1;
}
