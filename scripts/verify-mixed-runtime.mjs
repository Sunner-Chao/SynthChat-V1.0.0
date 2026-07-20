import { execFile, spawn } from "node:child_process";
import { createHash, randomBytes } from "node:crypto";
import { once } from "node:events";
import {
  access,
  mkdir,
  mkdtemp,
  readFile,
  rm,
  stat,
  writeFile,
} from "node:fs/promises";
import { createServer } from "node:http";
import { isIP } from "node:net";
import { tmpdir } from "node:os";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";
import { performance } from "node:perf_hooks";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);
const scriptPath = fileURLToPath(import.meta.url);
const scriptDirectory = dirname(scriptPath);
const repositoryRoot = resolve(scriptDirectory, "..");
const READY_PREFIX = "SYNTHCHAT_BACKEND_READY ";
const TERMINAL_EVENTS = new Set(["run.completed", "run.failed", "run.cancelled"]);
const TOOL_PROBE_PATTERN = /\[session_search:([a-z0-9]{8,64}):(session_[a-f0-9]{32}):([A-Za-z0-9_-]{1,160}):([A-Za-z0-9_-]{0,344})\]/u;
const SESSION_SEARCH_DESCRIPTION = "Search this Profile's local conversation history.";
const SESSION_SEARCH_SCHEMA = {
  additionalProperties: false,
  properties: {
    limit: { maximum: 20, minimum: 1, type: "integer" },
    query: { maxLength: 500, minLength: 1, type: "string" },
  },
  required: ["query"],
  type: "object",
};
const BOOLEAN_OPTIONS = new Set([
  "resource-samples",
  "self-test",
  "skip-build",
  "smoke",
]);
const KNOWN_OPTIONS = new Set([
  "backend-bin",
  "backend-log",
  "backend-manifest",
  "build-timeout-ms",
  "cargo",
  "cargo-profile",
  "concurrency",
  "cycle-delay-ms",
  "duration-seconds",
  "failure-limit",
  "help",
  "host",
  "latency-sample-limit",
  "max-failures",
  "max-iterations",
  "max-response-bytes",
  "max-sse-bytes",
  "max-sse-events",
  "output",
  "profile-prefix",
  "prompt-prefix",
  "provider-completion-tokens",
  "provider-delay-ms",
  "provider-max-request-bytes",
  "provider-model",
  "provider-path",
  "provider-prompt-tokens",
  "provider-reply",
  "provider-total-tokens",
  "request-timeout-ms",
  "resource-interval-ms",
  "resource-sample-limit",
  "resource-samples",
  "rust-toolchain",
  "self-test",
  "shutdown-timeout-ms",
  "skip-build",
  "smoke",
  "sse-timeout-ms",
  "startup-timeout-ms",
  "temp-base",
  "tool-every-iterations",
]);

class VerifierError extends Error {
  constructor(code, status = undefined) {
    super(code);
    this.name = "VerifierError";
    this.code = code;
    this.status = status;
  }
}

class LatencyBook {
  constructor(sampleLimit) {
    this.sampleLimit = sampleLimit;
    this.entries = new Map();
  }

  observe(name, durationMs, success) {
    let entry = this.entries.get(name);
    if (!entry) {
      entry = {
        count: 0,
        droppedSamples: 0,
        failures: 0,
        max: 0,
        min: Number.POSITIVE_INFINITY,
        samples: [],
        sum: 0,
      };
      this.entries.set(name, entry);
    }
    const value = Math.max(0, durationMs);
    entry.count += 1;
    entry.sum += value;
    entry.min = Math.min(entry.min, value);
    entry.max = Math.max(entry.max, value);
    if (!success) entry.failures += 1;
    if (entry.samples.length < this.sampleLimit) entry.samples.push(value);
    else entry.droppedSamples += 1;
  }

  summary() {
    const result = {};
    for (const name of [...this.entries.keys()].sort()) {
      const entry = this.entries.get(name);
      const samples = [...entry.samples].sort((left, right) => left - right);
      result[name] = {
        count: entry.count,
        failures: entry.failures,
        min: roundMilliseconds(entry.min),
        mean: roundMilliseconds(entry.sum / entry.count),
        p50: percentile(samples, 0.5),
        p95: percentile(samples, 0.95),
        p99: percentile(samples, 0.99),
        max: roundMilliseconds(entry.max),
        droppedSamples: entry.droppedSamples,
      };
    }
    return result;
  }
}

function parseCli(argumentsList) {
  const values = new Map();
  for (let index = 0; index < argumentsList.length; index += 1) {
    const argument = argumentsList[index];
    if (!argument.startsWith("--")) throw new VerifierError("invalid_cli_argument");
    const equalsIndex = argument.indexOf("=");
    let key = equalsIndex === -1 ? argument.slice(2) : argument.slice(2, equalsIndex);
    let value = equalsIndex === -1 ? undefined : argument.slice(equalsIndex + 1);
    if (key.startsWith("no-") && BOOLEAN_OPTIONS.has(key.slice(3))) {
      key = key.slice(3);
      value = "false";
    }
    if (!KNOWN_OPTIONS.has(key)) throw new VerifierError("unknown_cli_option");
    if (values.has(key)) throw new VerifierError("duplicate_cli_option");
    if (value === undefined) {
      if (BOOLEAN_OPTIONS.has(key) || key === "help") value = "true";
      else {
        value = argumentsList[index + 1];
        if (value === undefined || value.startsWith("--")) {
          throw new VerifierError("missing_cli_value");
        }
        index += 1;
      }
    }
    values.set(key, value);
  }
  return values;
}

function environmentName(key) {
  return `SYNTHCHAT_MIXED_VERIFY_${key.replaceAll("-", "_").toUpperCase()}`;
}

function rawOption(cli, key) {
  if (cli.has(key)) return cli.get(key);
  const value = process.env[environmentName(key)]?.trim();
  return value || undefined;
}

function integerOption(cli, key, fallback, { min = 0, max = Number.MAX_SAFE_INTEGER } = {}) {
  const raw = rawOption(cli, key);
  if (raw === undefined) return fallback;
  const value = Number(raw);
  if (!Number.isSafeInteger(value) || value < min || value > max) {
    throw new VerifierError("invalid_numeric_option");
  }
  return value;
}

function booleanOption(cli, key, fallback) {
  const raw = rawOption(cli, key);
  if (raw === undefined) return fallback;
  const normalized = raw.toLowerCase();
  if (["1", "true"].includes(normalized)) return true;
  if (["0", "false"].includes(normalized)) return false;
  throw new VerifierError("invalid_boolean_option");
}

function stringOption(cli, key, fallback, { maxLength = 4096, optional = false } = {}) {
  const raw = rawOption(cli, key);
  if (raw === undefined) return fallback;
  if (!raw && !optional) throw new VerifierError("empty_string_option");
  if (raw.length > maxLength || [...raw].some((character) => character === "\0")) {
    throw new VerifierError("invalid_string_option");
  }
  return raw;
}

function configuredPath(cli, key, fallback, optional = false) {
  const value = stringOption(cli, key, fallback, { maxLength: 4096, optional });
  if (!value) return undefined;
  return isAbsolute(value) ? value : resolve(repositoryRoot, value);
}

function defaultRustToolchain() {
  if (process.platform === "win32" && process.arch === "x64") {
    return "1.88.0-x86_64-pc-windows-msvc";
  }
  return "1.88.0";
}

function parseConfiguration(argumentsList) {
  const cli = parseCli(argumentsList);
  const help = rawOption(cli, "help") === "true";
  const smoke = booleanOption(cli, "smoke", false);
  const selfTest = booleanOption(cli, "self-test", false);
  const host = stringOption(cli, "host", "127.0.0.1", { maxLength: 64 });
  if (isIP(host) === 0 || !["127.0.0.1", "::1"].includes(host)) {
    throw new VerifierError("host_must_be_loopback");
  }
  const providerPath = stringOption(
    cli,
    "provider-path",
    "/v1/chat/completions",
    { maxLength: 256 },
  );
  if (
    !providerPath.startsWith("/")
    || !providerPath.endsWith("/chat/completions")
    || providerPath.includes("?")
    || providerPath.includes("#")
  ) {
    throw new VerifierError("invalid_provider_path");
  }
  const profilePrefix = stringOption(cli, "profile-prefix", "mixed_runtime", {
    maxLength: 32,
  });
  if (!/^[a-z0-9_][a-z0-9_-]*$/u.test(profilePrefix)) {
    throw new VerifierError("invalid_profile_prefix");
  }
  const backendLog = stringOption(cli, "backend-log", "warn", { maxLength: 256 });
  if (!/^[A-Za-z0-9_:,=\-]+$/u.test(backendLog)) {
    throw new VerifierError("invalid_backend_log_filter");
  }
  const cargoProfile = stringOption(cli, "cargo-profile", "debug", { maxLength: 16 });
  if (!["debug", "release"].includes(cargoProfile)) {
    throw new VerifierError("invalid_cargo_profile");
  }

  const promptTokens = integerOption(cli, "provider-prompt-tokens", 7, {
    min: 1,
    max: 1_000_000,
  });
  const completionTokens = integerOption(cli, "provider-completion-tokens", 5, {
    min: 1,
    max: 1_000_000,
  });
  const totalTokens = integerOption(
    cli,
    "provider-total-tokens",
    promptTokens + completionTokens,
    { min: 1, max: 2_000_000 },
  );
  if (totalTokens < promptTokens + completionTokens) {
    throw new VerifierError("invalid_provider_token_totals");
  }
  const durationMs = integerOption(cli, "duration-seconds", smoke ? 15 : 1_800, {
    min: 1,
    max: 28_800,
  }) * 1_000;
  const resourceIntervalMs = integerOption(
    cli,
    "resource-interval-ms",
    smoke ? 250 : 5_000,
    { min: 100, max: 300_000 },
  );
  const defaultResourceSampleLimit = Math.min(
    10_000,
    Math.ceil(durationMs / resourceIntervalMs) + 2,
  );

  return {
    backendBin: configuredPath(cli, "backend-bin", undefined, true),
    backendLog,
    backendManifest: configuredPath(cli, "backend-manifest", "backend/Cargo.toml"),
    buildTimeoutMs: integerOption(cli, "build-timeout-ms", 300_000, {
      min: 1_000,
      max: 1_800_000,
    }),
    cargo: stringOption(cli, "cargo", process.platform === "win32" ? "cargo.exe" : "cargo", {
      maxLength: 1024,
    }),
    cargoProfile,
    concurrency: integerOption(cli, "concurrency", smoke ? 2 : 2, { min: 1, max: 32 }),
    cycleDelayMs: integerOption(cli, "cycle-delay-ms", smoke ? 0 : 3_000, {
      min: 0,
      max: 300_000,
    }),
    durationMs,
    failureLimit: integerOption(cli, "failure-limit", 50, { min: 1, max: 1_000 }),
    help,
    host,
    latencySampleLimit: integerOption(cli, "latency-sample-limit", 5_000, {
      min: 10,
      max: 50_000,
    }),
    maxFailures: integerOption(cli, "max-failures", smoke ? 1 : 25, {
      min: 1,
      max: 10_000,
    }),
    maxIterations: integerOption(cli, "max-iterations", smoke ? 4 : 0, {
      min: 0,
      max: 1_000_000,
    }),
    maxResponseBytes: integerOption(cli, "max-response-bytes", 2_097_152, {
      min: 1_024,
      max: 16_777_216,
    }),
    maxSseBytes: integerOption(cli, "max-sse-bytes", 4_194_304, {
      min: 1_024,
      max: 67_108_864,
    }),
    maxSseEvents: integerOption(cli, "max-sse-events", 10_000, {
      min: 10,
      max: 1_000_000,
    }),
    output: configuredPath(cli, "output", undefined, true),
    profilePrefix,
    promptPrefix: stringOption(cli, "prompt-prefix", "mixed runtime searchable", {
      maxLength: 256,
    }),
    providerCompletionTokens: completionTokens,
    providerDelayMs: integerOption(cli, "provider-delay-ms", smoke ? 1 : 10, {
      min: 0,
      max: 60_000,
    }),
    providerMaxRequestBytes: integerOption(
      cli,
      "provider-max-request-bytes",
      1_048_576,
      { min: 1_024, max: 16_777_216 },
    ),
    providerModel: stringOption(cli, "provider-model", "mixed-runtime-model", {
      maxLength: 256,
    }),
    providerPath,
    providerPromptTokens: promptTokens,
    providerReply: stringOption(cli, "provider-reply", "Mixed runtime reply", {
      maxLength: 4_096,
    }),
    providerTotalTokens: totalTokens,
    requestTimeoutMs: integerOption(cli, "request-timeout-ms", 15_000, {
      min: 100,
      max: 300_000,
    }),
    resourceIntervalMs,
    resourceSampleLimit: integerOption(cli, "resource-sample-limit", defaultResourceSampleLimit, {
      min: 1,
      max: 10_000,
    }),
    resourceSamples: booleanOption(cli, "resource-samples", true),
    rustToolchain: stringOption(cli, "rust-toolchain", defaultRustToolchain(), {
      maxLength: 128,
    }),
    selfTest,
    shutdownTimeoutMs: integerOption(cli, "shutdown-timeout-ms", 5_000, {
      min: 100,
      max: 120_000,
    }),
    skipBuild: booleanOption(cli, "skip-build", false),
    smoke,
    sseTimeoutMs: integerOption(cli, "sse-timeout-ms", 60_000, {
      min: 500,
      max: 600_000,
    }),
    startupTimeoutMs: integerOption(cli, "startup-timeout-ms", 15_000, {
      min: 500,
      max: 300_000,
    }),
    tempBase: configuredPath(cli, "temp-base", tmpdir()),
    toolEveryIterations: integerOption(cli, "tool-every-iterations", smoke ? 2 : 10, {
      min: 1,
      max: 1_000_000,
    }),
  };
}

function helpText() {
  return [
    "Usage: node scripts/verify-mixed-runtime.mjs [options]",
    "",
    "Modes:",
    "  --self-test                 Run built-in parser/provider tests only.",
    "  --smoke                     Run a four-iteration real-backend smoke test.",
    "",
    "Pilot defaults:",
    "  --duration-seconds 1800 --concurrency 2 --cycle-delay-ms 3000",
    "  --duration-seconds accepts up to 28800 (8 hours) for a mixed pilot.",
    "  --tool-every-iterations 10 runs a deterministic read-only session_search probe.",
    "  Resource retention defaults to the full duration at the selected interval.",
    "",
    "Every option is also available as SYNTHCHAT_MIXED_VERIFY_<OPTION_NAME>.",
    "Use --help with the source file for the complete validated option catalog.",
  ].join("\n");
}

function socketAddress(host, port) {
  return host.includes(":") ? `[${host}]:${port}` : `${host}:${port}`;
}

function origin(host, port) {
  return `http://${socketAddress(host, port)}`;
}

function roundMilliseconds(value) {
  return Number.isFinite(value) ? Math.round(value * 100) / 100 : null;
}

function sha256(value) {
  return `sha256:${createHash("sha256").update(value).digest("hex")}`;
}

function canonicalHash(value) {
  return sha256(JSON.stringify(value));
}

function workspaceRelativePath(absolutePath) {
  const value = relative(repositoryRoot, absolutePath).replaceAll("\\", "/");
  if (!value || value === ".." || value.startsWith("../") || isAbsolute(value)) {
    throw new VerifierError("provenance_path_outside_workspace");
  }
  return value;
}

function percentile(sortedSamples, fraction) {
  if (sortedSamples.length === 0) return null;
  const index = Math.min(
    sortedSamples.length - 1,
    Math.max(0, Math.ceil(sortedSamples.length * fraction) - 1),
  );
  return roundMilliseconds(sortedSamples[index]);
}

function failureRecord(stage, error) {
  const code = error instanceof VerifierError ? error.code : "unexpected_error";
  const status = error instanceof VerifierError && Number.isSafeInteger(error.status)
    ? error.status
    : null;
  return {
    stage: /^[a-z0-9_.-]{1,64}$/u.test(stage) ? stage : "unknown",
    code: /^[a-z0-9_]{1,64}$/u.test(code) ? code : "unexpected_error",
    status,
  };
}

function withTimeoutSignal(timeoutMs, outerSignal) {
  const timeoutSignal = AbortSignal.timeout(timeoutMs);
  return outerSignal ? AbortSignal.any([outerSignal, timeoutSignal]) : timeoutSignal;
}

async function delay(durationMs, signal = undefined) {
  if (durationMs <= 0) return;
  await new Promise((resolveDelay, reject) => {
    const complete = () => {
      signal?.removeEventListener("abort", abort);
      resolveDelay();
    };
    const abort = () => {
      clearTimeout(timeoutId);
      signal?.removeEventListener("abort", abort);
      reject(new VerifierError("operation_aborted"));
    };
    const timeoutId = setTimeout(complete, durationMs);
    if (signal?.aborted) abort();
    else signal?.addEventListener("abort", abort, { once: true });
  });
}

async function withTimeout(promise, timeoutMs, code) {
  let timeoutId;
  try {
    return await Promise.race([
      promise,
      new Promise((_, reject) => {
        timeoutId = setTimeout(() => reject(new VerifierError(code)), timeoutMs);
      }),
    ]);
  } finally {
    clearTimeout(timeoutId);
  }
}

async function timed(latencies, name, operation) {
  const started = performance.now();
  try {
    const result = await operation();
    latencies.observe(name, performance.now() - started, true);
    return result;
  } catch (error) {
    latencies.observe(name, performance.now() - started, false);
    throw error;
  }
}

async function readResponseText(response, maximumBytes) {
  if (!response.body) return "";
  const reader = response.body.getReader();
  const chunks = [];
  let size = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    size += value.byteLength;
    if (size > maximumBytes) {
      await reader.cancel();
      throw new VerifierError("response_too_large");
    }
    chunks.push(value);
  }
  return Buffer.concat(chunks.map((chunk) => Buffer.from(chunk))).toString("utf8");
}

async function apiJson(runtime, method, path, options = {}) {
  let response;
  try {
    response = await fetch(new URL(path, runtime.origin), {
      body: options.body === undefined ? undefined : JSON.stringify(options.body),
      cache: "no-store",
      headers: {
        Accept: "application/json",
        Authorization: `Bearer ${runtime.token}`,
        ...(options.body === undefined ? {} : { "Content-Type": "application/json" }),
        ...options.headers,
      },
      method,
      redirect: "error",
      signal: withTimeoutSignal(runtime.config.requestTimeoutMs, options.signal),
    });
  } catch {
    throw new VerifierError("http_request_failed");
  }
  const expected = options.expected ?? [200];
  const text = await readResponseText(response, runtime.config.maxResponseBytes);
  if (!expected.includes(response.status)) {
    let problemCode = "unexpected_http_status";
    try {
      const parsed = JSON.parse(text);
      if (/^[a-z0-9_]{1,64}$/u.test(parsed?.code)) problemCode = parsed.code;
    } catch {
      // Status and a bounded code are sufficient for secret-free diagnostics.
    }
    throw new VerifierError(problemCode, response.status);
  }
  let data = null;
  if (text) {
    try {
      data = JSON.parse(text);
    } catch {
      throw new VerifierError("invalid_json_response", response.status);
    }
  }
  return { data, headers: response.headers, status: response.status };
}

async function consumeBackendEvents(runtime, expectation, signal, eventCounts) {
  const { runId } = expectation;
  let response;
  try {
    response = await fetch(new URL(`/api/v1/runs/${encodeURIComponent(runId)}/events`, runtime.origin), {
      cache: "no-store",
      headers: {
        Accept: "text/event-stream",
        Authorization: `Bearer ${runtime.token}`,
      },
      redirect: "error",
      signal: withTimeoutSignal(runtime.config.sseTimeoutMs, signal),
    });
  } catch {
    throw new VerifierError("sse_request_failed");
  }
  if (response.status !== 200 || !response.body) {
    throw new VerifierError("sse_unexpected_status", response.status);
  }
  const contentType = response.headers.get("content-type") ?? "";
  if (!contentType.toLowerCase().startsWith("text/event-stream")) {
    throw new VerifierError("sse_invalid_content_type");
  }
  return consumeSseBody(response.body, runtime.config, eventCounts, expectation);
}

async function consumeSseBody(body, config, eventCounts = {}, expectation = undefined) {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  let observedBytes = 0;
  let observedEvents = 0;
  const validation = expectation ? {
    deltaCount: 0,
    messageCompleted: false,
    messageId: null,
    names: [],
    sequence: 0,
    toolState: "none",
    usage: null,
    usageUpdates: 0,
  } : null;
  for (;;) {
    let chunk;
    try {
      chunk = await reader.read();
    } catch {
      throw new VerifierError("sse_stream_failed");
    }
    if (chunk.done) break;
    observedBytes += chunk.value.byteLength;
    if (observedBytes > config.maxSseBytes) {
      await reader.cancel();
      throw new VerifierError("sse_too_large");
    }
    buffer += decoder.decode(chunk.value, { stream: true });
    buffer = buffer.replaceAll("\r\n", "\n");
    for (;;) {
      const separator = buffer.indexOf("\n\n");
      if (separator === -1) break;
      const block = buffer.slice(0, separator);
      buffer = buffer.slice(separator + 2);
      const event = parseSseBlock(block);
      if (!event) continue;
      observedEvents += 1;
      if (observedEvents > config.maxSseEvents) {
        await reader.cancel();
        throw new VerifierError("sse_too_many_events");
      }
      recordEventCount(eventCounts, event.name);
      if (validation) validateRunEvent(event, expectation, validation);
      if (TERMINAL_EVENTS.has(event.name)) {
        if (validation) validateRunLifecycle(expectation, validation, config);
        await reader.cancel();
        return {
          bytes: observedBytes,
          events: observedEvents,
          terminal: event.name,
          toolCompleted: validation?.toolState === "completed",
        };
      }
    }
  }
  throw new VerifierError("sse_terminal_event_missing");
}

function recordEventCount(eventCounts, name) {
  if (Object.hasOwn(eventCounts, name) || Object.keys(eventCounts).length < 64) {
    eventCounts[name] = (eventCounts[name] ?? 0) + 1;
  } else {
    eventCounts._other = (eventCounts._other ?? 0) + 1;
  }
}

function parseSseBlock(block) {
  let id;
  let name;
  const data = [];
  for (const line of block.split("\n")) {
    if (line.startsWith(":")) continue;
    if (line.startsWith("id:")) {
      if (id !== undefined) throw new VerifierError("sse_duplicate_id");
      id = line.slice(3).trimStart();
    } else if (line.startsWith("event:")) {
      if (name !== undefined) throw new VerifierError("sse_duplicate_event");
      name = line.slice(6).trimStart();
    } else if (line.startsWith("data:")) {
      data.push(line.slice(5).trimStart());
    }
  }
  if (name === undefined && data.length === 0) return null;
  if (!name || !/^[a-z][a-z0-9_.-]{0,63}$/u.test(name)) {
    throw new VerifierError("sse_invalid_event_name");
  }
  if (data.length === 0) throw new VerifierError("sse_data_missing");
  let parsed;
  try {
    parsed = JSON.parse(data.join("\n"));
  } catch {
    throw new VerifierError("sse_invalid_json");
  }
  return { data: parsed, id, name };
}

function validUsage(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const keys = Object.keys(value);
  if (
    !["completionTokens", "promptTokens", "totalTokens"].every((key) => keys.includes(key))
    || keys.some((key) => !["completionTokens", "cost", "promptTokens", "totalTokens"].includes(key))
  ) return false;
  const { completionTokens, promptTokens, totalTokens } = value;
  return Number.isSafeInteger(completionTokens)
    && completionTokens >= 0
    && Number.isSafeInteger(promptTokens)
    && promptTokens >= 0
    && Number.isSafeInteger(totalTokens)
    && totalTokens === promptTokens + completionTokens
    && (value.cost === undefined || value.cost === null || (
      typeof value.cost === "number" && Number.isFinite(value.cost) && value.cost >= 0
    ));
}

function validateRunEvent(event, expectation, state) {
  const sequence = state.sequence + 1;
  const envelope = event.data;
  if (
    event.id !== `${expectation.runId}:${sequence}`
    || !exactJson(
      Object.keys(envelope ?? {}).sort(),
      ["data", "occurredAt", "runId", "schemaVersion", "sequence", "sessionId"],
    )
    || envelope.schemaVersion !== 1
    || envelope.sequence !== sequence
    || envelope.runId !== expectation.runId
    || envelope.sessionId !== expectation.sessionId
    || typeof envelope.occurredAt !== "string"
    || !Number.isFinite(Date.parse(envelope.occurredAt))
    || !envelope.data
    || typeof envelope.data !== "object"
    || Array.isArray(envelope.data)
  ) {
    throw new VerifierError("sse_envelope_mismatch");
  }
  const data = envelope.data;
  state.sequence = sequence;
  state.names.push(event.name);
  switch (event.name) {
    case "run.started":
      if (sequence !== 1 || data.profileId !== expectation.profileId) {
        throw new VerifierError("sse_run_started_mismatch");
      }
      break;
    case "message.started":
      if (
        state.messageId !== null
        || typeof data.messageId !== "string"
        || data.role !== "assistant"
      ) throw new VerifierError("sse_message_started_mismatch");
      state.messageId = data.messageId;
      break;
    case "message.delta":
      if (
        data.messageId !== state.messageId
        || typeof data.delta !== "string"
        || data.delta.length === 0
        || state.messageCompleted
      ) throw new VerifierError("sse_message_delta_mismatch");
      state.deltaCount += 1;
      break;
    case "usage.updated":
      if (
        !validUsage(data)
        || state.usage && (
          data.promptTokens < state.usage.promptTokens
          || data.completionTokens < state.usage.completionTokens
          || data.totalTokens < state.usage.totalTokens
        )
      ) throw new VerifierError("sse_usage_mismatch");
      state.usage = data;
      state.usageUpdates += 1;
      break;
    case "tool.started":
      if (
        !expectation.toolProbe
        || state.toolState !== "none"
        || !exactJson(Object.keys(data).sort(), ["callId", "inputSummary", "name"])
        || data.callId !== toolCallId(expectation.query)
        || data.name !== "session_search"
        || data.inputSummary !== "session_search"
      ) throw new VerifierError("sse_tool_started_mismatch");
      state.toolState = "started";
      break;
    case "tool.completed":
      if (
        !expectation.toolProbe
        || state.toolState !== "started"
        || !exactJson(Object.keys(data).sort(), ["artifacts", "callId", "resultSummary"])
        || data.callId !== toolCallId(expectation.query)
        || data.resultSummary !== "1 matching sessions"
        || !Array.isArray(data.artifacts)
        || data.artifacts.length !== 0
      ) throw new VerifierError("sse_tool_completed_mismatch");
      state.toolState = "completed";
      break;
    case "message.completed":
      if (
        state.messageId === null
        || state.deltaCount === 0
        || state.messageCompleted
        || data.message?.id !== state.messageId
        || data.message?.role !== "assistant"
      ) throw new VerifierError("sse_message_completed_mismatch");
      state.messageCompleted = true;
      break;
    case "run.completed":
      if (
        !state.messageCompleted
        || data.messageId !== state.messageId
        || !validUsage(data.usage)
        || !exactJson(data.usage, state.usage)
      ) throw new VerifierError("sse_run_completed_mismatch");
      break;
    default:
      throw new VerifierError("sse_unexpected_event");
  }
}

function validateRunLifecycle(expectation, state, config) {
  const deltas = Array.from(
    { length: Math.min(2, [...config.providerReply].length) },
    () => "message.delta",
  );
  const expectedNames = expectation.toolProbe ? [
    "run.started",
    "message.started",
    "usage.updated",
    "tool.started",
    "tool.completed",
    ...deltas,
    "usage.updated",
    "message.completed",
    "run.completed",
  ] : [
    "run.started",
    "message.started",
    ...deltas,
    "usage.updated",
    "message.completed",
    "run.completed",
  ];
  if (
    !exactJson(state.names, expectedNames)
    || state.usageUpdates !== (expectation.toolProbe ? 2 : 1)
    || state.toolState !== (expectation.toolProbe ? "completed" : "none")
  ) throw new VerifierError("sse_lifecycle_mismatch");
}

async function startMockProvider(config) {
  const state = {
    activeRequests: 0,
    failures: 0,
    maxActiveRequests: 0,
    pendingToolCalls: new Map(),
    requestCount: 0,
    normalCompletions: 0,
    toolCallsIssued: 0,
    toolResultsValidated: 0,
    rejections: {},
  };
  const sockets = new Set();
  const server = createServer((request, response) => {
    void handleProviderRequest(request, response, config, state).catch(() => {
      state.failures += 1;
      if (!response.headersSent) response.writeHead(500, { "Content-Type": "text/plain" });
      if (!response.writableEnded) response.end("provider failure");
    });
  });
  server.on("connection", (socket) => {
    sockets.add(socket);
    socket.once("close", () => sockets.delete(socket));
  });
  await new Promise((resolveListening, reject) => {
    server.once("error", reject);
    server.listen({ exclusive: true, host: config.host, port: 0 }, resolveListening);
  });
  const address = server.address();
  if (!address || typeof address === "string" || address.port <= 0) {
    throw new VerifierError("provider_address_unavailable");
  }
  const providerOrigin = origin(config.host, address.port);
  const suffix = "/chat/completions";
  const basePath = config.providerPath.slice(0, -suffix.length);
  return {
    baseUrl: `${providerOrigin}${basePath}`,
    state,
    async close() {
      for (const socket of sockets) socket.destroy();
      if (!server.listening) return;
      await new Promise((resolveClosed) => server.close(resolveClosed));
    },
  };
}

function exactJson(actual, expected) {
  if (Array.isArray(expected)) {
    return Array.isArray(actual)
      && actual.length === expected.length
      && expected.every((value, index) => exactJson(actual[index], value));
  }
  if (expected && typeof expected === "object") {
    if (!actual || typeof actual !== "object" || Array.isArray(actual)) return false;
    const actualKeys = Object.keys(actual).sort();
    const expectedKeys = Object.keys(expected).sort();
    return exactJson(actualKeys, expectedKeys)
      && expectedKeys.every((key) => exactJson(actual[key], expected[key]));
  }
  return actual === expected;
}

function encodedProbeValue(value) {
  return Buffer.from(value, "utf8").toString("base64url");
}

function toolProbe(messages) {
  const probes = [];
  for (const message of messages) {
    if (message?.role !== "user" || typeof message.content !== "string") continue;
    const match = TOOL_PROBE_PATTERN.exec(message.content);
    if (!match) continue;
    const title = Buffer.from(match[3], "base64url").toString("utf8");
    const model = Buffer.from(match[4], "base64url").toString("utf8");
    if (
      !title
      || encodedProbeValue(title) !== match[3]
      || encodedProbeValue(model) !== match[4]
    ) return null;
    probes.push({ model, query: match[1], sessionId: match[2], title });
  }
  return probes.length === 1 ? probes[0] : null;
}

function toolCallId(query) {
  return `call-${query}`;
}

function hasSessionSearchDefinition(body) {
  if (body?.tool_choice !== "auto" || body?.tools?.length !== 1) return false;
  return exactJson(body.tools[0], {
    function: {
      description: SESSION_SEARCH_DESCRIPTION,
      name: "session_search",
      parameters: SESSION_SEARCH_SCHEMA,
      strict: true,
    },
    type: "function",
  });
}

function sessionSearchContinuationError(messages, callId, probe, config) {
  const assistant = messages.at(-2);
  const tool = messages.at(-1);
  if (
    !exactJson(Object.keys(assistant ?? {}).sort(), ["content", "role", "tool_calls"])
    || assistant.role !== "assistant"
    || assistant.content !== null
    || assistant.tool_calls?.length !== 1
    || !exactJson(Object.keys(tool ?? {}).sort(), ["content", "role", "tool_call_id"])
    || tool.role !== "tool"
    || tool.tool_call_id !== callId
    || typeof tool.content !== "string"
  ) return "continuation_messages";
  const call = assistant.tool_calls[0];
  if (
    !exactJson(Object.keys(call ?? {}).sort(), ["function", "id", "type"])
    || call.id !== callId
    || call.type !== "function"
    || !exactJson(Object.keys(call.function ?? {}).sort(), ["arguments", "name"])
    || call.function.name !== "session_search"
    || typeof call.function.arguments !== "string"
  ) return "continuation_call";
  try {
    const argumentsValue = JSON.parse(call.function.arguments);
    const result = JSON.parse(tool.content);
    if (!exactJson(argumentsValue, { limit: 5, query: probe.query })) {
      return "continuation_arguments";
    }
    if (!exactJson(Object.keys(result ?? {}).sort(), ["items"])) {
      return "continuation_result_shape";
    }
    if (result.items?.length !== 1) return "continuation_result_count";
    const item = result.items[0];
    if (!exactJson(
      Object.keys(item ?? {}).sort(),
      ["id", "match", "model", "preview", "title", "updatedAt"],
    )) return "continuation_item_shape";
    if (item.id !== probe.sessionId) return "continuation_item_owner";
    if (item.title !== probe.title) return "continuation_item_title";
    if (item.model !== probe.model) return "continuation_item_model";
    if (typeof item.preview !== "string" || !item.preview.includes(probe.query)) {
      return "continuation_item_preview";
    }
    if (!exactJson(Object.keys(item.match ?? {}).sort(), ["field", "snippet"])) {
      return "continuation_match_shape";
    }
    if (
      item.match.field !== "message"
      || typeof item.match.snippet !== "string"
      || !item.match.snippet.includes(probe.query)
    ) return "continuation_match_value";
    if (
      typeof item.updatedAt !== "string"
      || !Number.isFinite(Date.parse(item.updatedAt))
    ) return "continuation_item_timestamp";
    return null;
  } catch {
    return "continuation_json";
  }
}

function rejectToolProbe(response, state, reason) {
  state.failures += 1;
  state.rejections[reason] = (state.rejections[reason] ?? 0) + 1;
  response.writeHead(422, { "Content-Type": "text/plain" });
  response.end("invalid tool probe");
}

function beginProviderResponse(response) {
  response.writeHead(200, {
    "Cache-Control": "no-cache",
    Connection: "keep-alive",
    "Content-Type": "text/event-stream; charset=utf-8",
  });
}

async function writeTextCompletion(response, config) {
  const characters = [...config.providerReply];
  const midpoint = Math.max(1, Math.ceil(characters.length / 2));
  const replies = [characters.slice(0, midpoint).join(""), characters.slice(midpoint).join("")]
    .filter(Boolean);
  for (let index = 0; index < replies.length; index += 1) {
    response.write(`data: ${JSON.stringify({
      choices: [{
        delta: { content: replies[index] },
        finish_reason: index === replies.length - 1 ? "stop" : null,
        index: 0,
      }],
    })}\n\n`);
    await delay(config.providerDelayMs);
  }
  response.write(`data: ${JSON.stringify({
    choices: [],
    usage: {
      completion_tokens: config.providerCompletionTokens,
      prompt_tokens: config.providerPromptTokens,
      total_tokens: config.providerTotalTokens,
    },
  })}\n\n`);
  response.end("data: [DONE]\n\n");
}

async function writeToolCall(response, config, callId, query) {
  response.write(`data: ${JSON.stringify({
    choices: [{
      delta: {
        tool_calls: [{
          function: {
            arguments: JSON.stringify({ limit: 5, query }),
            name: "session_search",
          },
          id: callId,
          index: 0,
          type: "function",
        }],
      },
      finish_reason: "tool_calls",
      index: 0,
    }],
  })}\n\n`);
  await delay(config.providerDelayMs);
  response.write(`data: ${JSON.stringify({
    choices: [],
    usage: {
      completion_tokens: config.providerCompletionTokens,
      prompt_tokens: config.providerPromptTokens,
      total_tokens: config.providerTotalTokens,
    },
  })}\n\n`);
  response.end("data: [DONE]\n\n");
}

async function handleProviderRequest(request, response, config, state) {
  const requestUrl = new URL(request.url ?? "/", "http://loopback.invalid");
  if (request.method !== "POST" || requestUrl.pathname !== config.providerPath) {
    response.writeHead(404, { "Content-Type": "text/plain" });
    response.end("not found");
    return;
  }
  const chunks = [];
  let size = 0;
  for await (const chunk of request) {
    size += chunk.length;
    if (size > config.providerMaxRequestBytes) {
      response.writeHead(413, { "Content-Type": "text/plain" });
      response.end("request too large");
      return;
    }
    chunks.push(chunk);
  }
  let body;
  try {
    body = JSON.parse(Buffer.concat(chunks).toString("utf8"));
  } catch {
    response.writeHead(400, { "Content-Type": "text/plain" });
    response.end("invalid json");
    return;
  }
  if (
    body?.stream !== true
    || body?.stream_options?.include_usage !== true
    || body?.model !== config.providerModel
    || !Array.isArray(body?.messages)
    || body.messages.length === 0
  ) {
    response.writeHead(422, { "Content-Type": "text/plain" });
    response.end("invalid request");
    return;
  }

  state.requestCount += 1;
  state.activeRequests += 1;
  state.maxActiveRequests = Math.max(state.maxActiveRequests, state.activeRequests);
  response.once("close", () => {
    state.activeRequests = Math.max(0, state.activeRequests - 1);
  });
  const probe = toolProbe(body.messages);
  if (!probe) {
    state.normalCompletions += 1;
    beginProviderResponse(response);
    await writeTextCompletion(response, config);
    return;
  }
  if (!hasSessionSearchDefinition(body)) {
    rejectToolProbe(response, state, "tool_definition");
    return;
  }
  const callId = toolCallId(probe.query);
  const hasToolResult = body.messages.at(-1)?.role === "tool";
  if (!hasToolResult) {
    if (state.pendingToolCalls.has(callId)) {
      rejectToolProbe(response, state, "duplicate_tool_plan");
      return;
    }
    state.pendingToolCalls.set(callId, probe);
    state.toolCallsIssued += 1;
    beginProviderResponse(response);
    await writeToolCall(response, config, callId, probe.query);
    return;
  }
  const continuationError = sessionSearchContinuationError(body.messages, callId, probe, config);
  if (!exactJson(state.pendingToolCalls.get(callId), probe) || continuationError) {
    rejectToolProbe(
      response,
      state,
      continuationError ?? "continuation_pending_binding",
    );
    return;
  }
  state.pendingToolCalls.delete(callId);
  state.toolResultsValidated += 1;
  beginProviderResponse(response);
  await writeTextCompletion(response, config);
}

async function runCaptured(executable, argumentsList, options) {
  const child = spawn(executable, argumentsList, {
    cwd: options.cwd,
    env: options.env ?? process.env,
    shell: false,
    stdio: ["ignore", "pipe", "pipe"],
    windowsHide: true,
  });
  let stdout = Buffer.alloc(0);
  let stderrBytes = 0;
  child.stdout.on("data", (chunk) => {
    if (stdout.length + chunk.length <= options.maxStdoutBytes) {
      stdout = Buffer.concat([stdout, chunk]);
    } else {
      child.kill();
    }
  });
  child.stderr.on("data", (chunk) => {
    stderrBytes += chunk.length;
  });
  let timeoutId;
  try {
    const outcome = await Promise.race([
      once(child, "exit").then(([code, signal]) => ({ code, signal })),
      once(child, "error").then(() => {
        throw new VerifierError("process_spawn_failed");
      }),
      new Promise((_, reject) => {
        timeoutId = setTimeout(() => reject(new VerifierError("process_timeout")), options.timeoutMs);
      }),
    ]);
    if (outcome.code !== 0) throw new VerifierError("process_failed", outcome.code ?? undefined);
    return { stderrBytes, stdout: stdout.toString("utf8") };
  } catch (error) {
    await terminateChild(child, 2_000);
    throw error;
  } finally {
    clearTimeout(timeoutId);
  }
}

async function fileProvenance(absolutePath) {
  const [contents, metadata] = await Promise.all([readFile(absolutePath), stat(absolutePath)]);
  if (!metadata.isFile()) throw new VerifierError("provenance_file_invalid");
  return {
    bytes: metadata.size,
    path: workspaceRelativePath(absolutePath),
    sha256: sha256(contents),
  };
}

async function gitProvenance() {
  try {
    const options = {
      cwd: repositoryRoot,
      encoding: "utf8",
      maxBuffer: 16_777_216,
      windowsHide: true,
    };
    const [objectFormat, commit, tree, status] = await Promise.all([
      execFileAsync("git", ["rev-parse", "--show-object-format"], options),
      execFileAsync("git", ["rev-parse", "--verify", "HEAD"], options),
      execFileAsync("git", ["rev-parse", "--verify", "HEAD^{tree}"], options),
      execFileAsync(
        "git",
        ["status", "--porcelain=v1", "--untracked-files=all", "-z"],
        options,
      ),
    ]);
    return {
      commit: commit.stdout.trim(),
      objectFormat: objectFormat.stdout.trim(),
      tree: tree.stdout.trim(),
      worktreeClean: status.stdout.length === 0,
    };
  } catch {
    return {
      commit: null,
      objectFormat: null,
      tree: null,
      worktreeClean: false,
    };
  }
}

async function collectProvenance(config, backendExecutable) {
  const overrideNames = Object.keys(process.env)
    .filter((name) => (
      name.startsWith("SYNTHCHAT_MIXED_VERIFY_")
      && Boolean(process.env[name]?.trim())
    ))
    .sort();
  const overrideEntries = overrideNames.map((name) => [name, process.env[name].trim()]);
  const effectiveConfiguration = Object.fromEntries(
    Object.entries(config).sort(([left], [right]) => left.localeCompare(right)),
  );
  const [git, verifier, backend] = await Promise.all([
    gitProvenance(),
    fileProvenance(scriptPath),
    fileProvenance(backendExecutable),
  ]);
  return {
    schemaVersion: 1,
    platform: process.platform,
    arch: process.arch,
    nodeVersion: process.versions.node,
    rustVersion: config.rustToolchain,
    git,
    verifier,
    backend,
    effectiveConfigSha256: canonicalHash(effectiveConfiguration),
    argvSha256: canonicalHash(process.argv.slice(2)),
    overrideNames,
    overrideNamesSha256: canonicalHash(overrideEntries),
  };
}

async function resolveBackendBinary(config) {
  if (!config.skipBuild) {
    const argumentsList = [
      `+${config.rustToolchain}`,
      "build",
      "--locked",
      "--manifest-path",
      config.backendManifest,
      "--bin",
      "synthchat-hermes-backend",
    ];
    if (config.cargoProfile === "release") argumentsList.push("--release");
    await runCaptured(config.cargo, argumentsList, {
      cwd: repositoryRoot,
      maxStdoutBytes: 1_048_576,
      timeoutMs: config.buildTimeoutMs,
    });
  }
  if (config.backendBin) {
    await access(config.backendBin);
    return config.backendBin;
  }
  const metadata = await runCaptured(
    config.cargo,
    [
      `+${config.rustToolchain}`,
      "metadata",
      "--locked",
      "--format-version",
      "1",
      "--no-deps",
      "--manifest-path",
      config.backendManifest,
    ],
    {
      cwd: repositoryRoot,
      maxStdoutBytes: 16_777_216,
      timeoutMs: config.buildTimeoutMs,
    },
  );
  let parsed;
  try {
    parsed = JSON.parse(metadata.stdout);
  } catch {
    throw new VerifierError("cargo_metadata_invalid");
  }
  if (typeof parsed?.target_directory !== "string") {
    throw new VerifierError("cargo_target_directory_missing");
  }
  const profileDirectory = config.cargoProfile === "release" ? "release" : "debug";
  const executable = join(
    parsed.target_directory,
    profileDirectory,
    process.platform === "win32" ? "synthchat-hermes-backend.exe" : "synthchat-hermes-backend",
  );
  await access(executable);
  return executable;
}

async function startBackend(config, executable, hermesHome, token, processMetrics) {
  const environment = { ...process.env };
  for (const name of [
    "SYNTHCHAT_ALLOWED_ORIGINS",
    "SYNTHCHAT_DESKTOP_TOKEN",
    "SYNTHCHAT_SKILL_GITHUB_API_BASE_URL",
    "SYNTHCHAT_SKILL_GITHUB_RAW_BASE_URL",
    "SYNTHCHAT_SKILL_REGISTRY_INDEX_URL",
    "SYNTHCHAT_TAVILY_BASE_URL",
  ]) delete environment[name];
  Object.assign(environment, {
    HERMES_HOME: hermesHome,
    RUST_LOG: config.backendLog,
    SYNTHCHAT_BACKEND_ADDR: socketAddress(config.host, 0),
  });
  const child = spawn(executable, [], {
    cwd: repositoryRoot,
    env: environment,
    shell: false,
    stdio: ["pipe", "pipe", "pipe"],
    windowsHide: true,
  });
  child.stdin.on("error", () => undefined);
  child.stderr.on("data", (chunk) => {
    processMetrics.stderrBytes += chunk.length;
  });
  let buffer = "";
  let readyResolve;
  let readyReject;
  const readyPromise = new Promise((resolveReady, rejectReady) => {
    readyResolve = resolveReady;
    readyReject = rejectReady;
  });
  let ready = false;
  child.stdout.on("data", (chunk) => {
    processMetrics.stdoutBytes += chunk.length;
    if (ready) return;
    buffer += chunk.toString("utf8");
    if (Buffer.byteLength(buffer) > 4_096) {
      readyReject(new VerifierError("backend_handshake_too_large"));
      return;
    }
    const newline = buffer.indexOf("\n");
    if (newline === -1) return;
    const line = buffer.slice(0, newline).trim();
    if (!line.startsWith(READY_PREFIX)) {
      readyReject(new VerifierError("backend_handshake_invalid"));
      return;
    }
    let parsed;
    try {
      parsed = new URL(`http://${line.slice(READY_PREFIX.length)}`);
    } catch {
      readyReject(new VerifierError("backend_handshake_invalid"));
      return;
    }
    const hostname = parsed.hostname.replace(/^\[|\]$/gu, "");
    const port = Number(parsed.port);
    if (hostname !== config.host || !Number.isSafeInteger(port) || port <= 0 || port > 65_535) {
      readyReject(new VerifierError("backend_handshake_invalid"));
      return;
    }
    ready = true;
    readyResolve({ address: socketAddress(config.host, port), origin: origin(config.host, port) });
  });
  child.once("error", () => readyReject(new VerifierError("backend_spawn_failed")));
  child.once("exit", () => {
    if (!ready) readyReject(new VerifierError("backend_exited_before_ready"));
  });
  child.stdin.write(`${token}\n`);

  let runtime;
  try {
    runtime = await withTimeout(
      readyPromise,
      config.startupTimeoutMs,
      "backend_startup_timeout",
    );
  } catch (error) {
    await terminateChild(child, config.shutdownTimeoutMs);
    throw error;
  }
  const backend = { child, config, origin: runtime.origin, token };
  await waitForBackend(backend);
  return backend;
}

async function waitForBackend(runtime) {
  const deadline = Date.now() + runtime.config.startupTimeoutMs;
  for (;;) {
    try {
      const health = await fetch(new URL("/health", runtime.origin), {
        cache: "no-store",
        redirect: "error",
        signal: AbortSignal.timeout(Math.min(1_000, runtime.config.requestTimeoutMs)),
      });
      if (health.status === 200) {
        const capabilities = await apiJson(runtime, "GET", "/api/v1/capabilities", {
          expected: [200],
        });
        if (capabilities.data) return;
      }
    } catch {
      // A readiness retry never includes response content in diagnostics.
    }
    if (Date.now() >= deadline) throw new VerifierError("backend_readiness_timeout");
    await delay(50);
  }
}

async function terminateChild(child, timeoutMs) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return { forced: false };
  child.stdin?.end();
  const graceful = await waitForChildExit(child, timeoutMs);
  if (graceful) return { forced: false };
  child.kill();
  await waitForChildExit(child, timeoutMs);
  if (child.exitCode === null && child.signalCode === null) {
    child.kill("SIGKILL");
    await waitForChildExit(child, timeoutMs);
  }
  return { forced: true };
}

async function waitForChildExit(child, timeoutMs) {
  if (child.exitCode !== null || child.signalCode !== null) return true;
  return new Promise((resolveExit) => {
    let settled = false;
    let timeoutId;
    const finish = (exited) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeoutId);
      child.removeListener("exit", onExit);
      resolveExit(exited);
    };
    const onExit = () => finish(true);
    child.once("exit", onExit);
    timeoutId = setTimeout(() => finish(false), timeoutMs);
    if (child.exitCode !== null || child.signalCode !== null) finish(true);
  });
}

async function configureProfile(runtime, providerBaseUrl, config, runTag, signal) {
  const suffix = randomBytes(6).toString("hex");
  const profileId = `${config.profilePrefix}_${suffix}`.slice(0, 64);
  await apiJson(runtime, "POST", "/api/v1/profiles", {
    body: {
      cloneFromProfileId: "default",
      displayName: `Mixed Runtime ${runTag}`.slice(0, 80),
      id: profileId,
    },
    expected: [201],
    headers: { "Idempotency-Key": `profile-${runTag}` },
    signal,
  });
  const current = await apiJson(
    runtime,
    "GET",
    `/api/v1/profiles/${encodeURIComponent(profileId)}/config`,
    { signal },
  );
  const etag = current.headers.get("etag");
  if (!etag) throw new VerifierError("profile_config_etag_missing");
  await apiJson(
    runtime,
    "PATCH",
    `/api/v1/profiles/${encodeURIComponent(profileId)}/config`,
    {
      body: {
        model: {
          baseUrl: providerBaseUrl,
          model: config.providerModel,
          provider: "lmstudio",
          reasoningEffort: null,
        },
        toolsets: { session_search: true },
      },
      expected: [200],
      headers: {
        "Content-Type": "application/merge-patch+json",
        "If-Match": etag,
      },
      signal,
    },
  );
  await apiJson(
    runtime,
    "PUT",
    `/api/v1/profiles/${encodeURIComponent(profileId)}/active`,
    { expected: [200], signal },
  );
  return profileId;
}

async function runIteration(context, worker, iteration, signal) {
  const { config, eventCounts, latencies, profileId, runTag, runtime } = context;
  const searchTerm = `mix${runTag}${worker.toString(36)}${iteration.toString(36)}`;
  const sessionTitle = `Mixed runtime worker ${worker} iteration ${iteration}`;
  const session = await timed(latencies, "session.create", () => apiJson(
    runtime,
    "POST",
    "/api/v1/sessions",
    {
      body: {
        profileId,
        title: sessionTitle,
      },
      expected: [201],
      headers: { "Idempotency-Key": `session-${runTag}-${worker}-${iteration}` },
      signal,
    },
  ));
  const sessionId = session.data?.id;
  if (typeof sessionId !== "string") throw new VerifierError("session_id_missing");

  const prompt = `${config.promptPrefix} ${searchTerm}`;
  const toolProbe = iteration % config.toolEveryIterations === 0;
  const modelLabel = `lmstudio/${config.providerModel}`;
  const marker = `[session_search:${searchTerm}:${sessionId}:${encodedProbeValue(sessionTitle)}:${encodedProbeValue(modelLabel)}]`;
  const runPrompt = toolProbe ? `${prompt} ${marker}` : prompt;
  const accepted = await timed(latencies, "run.post", () => apiJson(
    runtime,
    "POST",
    `/api/v1/sessions/${encodeURIComponent(sessionId)}/runs`,
    {
      body: {
        clientRequestId: `request-${runTag}-${worker}-${iteration}`,
        message: { fileIds: [], text: runPrompt },
        modelOverride: null,
        reasoningEffort: null,
      },
      expected: [202],
      headers: { "Idempotency-Key": `run-${runTag}-${worker}-${iteration}` },
      signal,
    },
  ));
  const runId = accepted.data?.run?.id;
  if (typeof runId !== "string") throw new VerifierError("run_id_missing");

  const eventPromise = timed(latencies, "run.sse_terminal", () => (
    consumeBackendEvents(runtime, {
      profileId,
      query: searchTerm,
      runId,
      sessionId,
      toolProbe,
    }, signal, eventCounts)
  ));
  const concurrentReads = Promise.all([
    timed(latencies, "session.read_live", () => apiJson(
      runtime,
      "GET",
      `/api/v1/sessions/${encodeURIComponent(sessionId)}`,
      { signal },
    )),
    timed(latencies, "messages.read_live", () => apiJson(
      runtime,
      "GET",
      `/api/v1/sessions/${encodeURIComponent(sessionId)}/messages?limit=20`,
      { signal },
    )),
    timed(latencies, "history.list_live", () => apiJson(
      runtime,
      "GET",
      `/api/v1/sessions?profileId=${encodeURIComponent(profileId)}&limit=20`,
      { signal },
    )),
  ]);
  const [terminal] = await Promise.all([eventPromise, concurrentReads]);
  if (terminal.terminal !== "run.completed") {
    throw new VerifierError("run_terminal_failure");
  }

  const [run, sessionRead, messages, history, search] = await Promise.all([
    timed(latencies, "run.read_terminal", () => apiJson(
      runtime,
      "GET",
      `/api/v1/runs/${encodeURIComponent(runId)}`,
      { signal },
    )),
    timed(latencies, "session.read_terminal", () => apiJson(
      runtime,
      "GET",
      `/api/v1/sessions/${encodeURIComponent(sessionId)}`,
      { signal },
    )),
    timed(latencies, "messages.read_terminal", () => apiJson(
      runtime,
      "GET",
      `/api/v1/sessions/${encodeURIComponent(sessionId)}/messages?limit=20`,
      { signal },
    )),
    timed(latencies, "history.list_terminal", () => apiJson(
      runtime,
      "GET",
      `/api/v1/sessions?profileId=${encodeURIComponent(profileId)}&limit=20`,
      { signal },
    )),
    timed(latencies, "history.fts_search", () => apiJson(
      runtime,
      "GET",
      `/api/v1/sessions?profileId=${encodeURIComponent(profileId)}&q=${encodeURIComponent(searchTerm)}&limit=20`,
      { signal },
    )),
  ]);
  if (run.data?.status !== "completed") throw new VerifierError("run_state_not_completed");
  if (sessionRead.data?.id !== sessionId) throw new VerifierError("session_read_mismatch");
  if (!history.data?.items?.some((item) => item.id === sessionId)) {
    throw new VerifierError("history_session_missing");
  }
  const messageItems = messages.data?.items;
  if (!Array.isArray(messageItems) || messageItems.length < 2) {
    throw new VerifierError("committed_messages_missing");
  }
  const promptPersisted = messageItems.some((message) => (
    message?.role === "user"
    && message?.parts?.some((part) => part?.type === "text" && part?.text?.includes(searchTerm))
  ));
  if (!promptPersisted) throw new VerifierError("prompt_message_missing");
  const matched = search.data?.items?.find((item) => item.id === sessionId);
  if (!matched || matched.match?.field !== "message") {
    throw new VerifierError("fts_message_match_missing");
  }
}

async function runWorkload(context, signal) {
  const startedAt = Date.now();
  const deadline = startedAt + context.config.durationMs;
  const state = {
    failures: 0,
    iterationsCompleted: 0,
    iterationsStarted: 0,
    successes: 0,
  };
  const stop = new AbortController();
  const workloadSignal = signal ? AbortSignal.any([signal, stop.signal]) : stop.signal;

  function reserveIteration() {
    if (workloadSignal.aborted || Date.now() >= deadline) return null;
    if (
      context.config.maxIterations > 0
      && state.iterationsStarted >= context.config.maxIterations
    ) return null;
    const iteration = state.iterationsStarted;
    state.iterationsStarted += 1;
    return iteration;
  }

  async function worker(workerId) {
    for (;;) {
      const iteration = reserveIteration();
      if (iteration === null) return;
      try {
        await runIteration(context, workerId, iteration, workloadSignal);
        state.successes += 1;
      } catch (error) {
        if (workloadSignal.aborted) return;
        state.failures += 1;
        context.recordFailure("iteration", error);
        if (state.failures >= context.config.maxFailures) {
          stop.abort();
          return;
        }
      } finally {
        state.iterationsCompleted += 1;
      }
      try {
        await delay(context.config.cycleDelayMs, workloadSignal);
      } catch {
        return;
      }
    }
  }

  await Promise.all(
    Array.from({ length: context.config.concurrency }, (_, index) => worker(index)),
  );
  return { ...state, elapsedMs: Date.now() - startedAt };
}

function expectedToolCallCount(iterations, everyIterations) {
  return iterations === 0 ? 0 : Math.floor((iterations - 1) / everyIterations) + 1;
}

function expectedMessageDeltaCount(config) {
  return Math.min(2, [...config.providerReply].length);
}

async function sampleBackendRss(pid) {
  try {
    if (process.platform === "linux") {
      const status = await readFile(`/proc/${pid}/status`, "utf8");
      const match = /^VmRSS:\s+(\d+)\s+kB$/mu.exec(status);
      return match ? Number(match[1]) * 1_024 : null;
    }
    if (process.platform === "win32") {
      if (!Number.isSafeInteger(pid) || pid <= 0) return null;
      const command = `$p=Get-Process -Id ${pid} -ErrorAction Stop; [Console]::Out.Write($p.WorkingSet64)`;
      const result = await execFileAsync(
        "powershell.exe",
        ["-NoProfile", "-NonInteractive", "-Command", command],
        { timeout: 2_000, windowsHide: true },
      );
      const value = Number(result.stdout.trim());
      return Number.isSafeInteger(value) && value >= 0 ? value : null;
    }
    const result = await execFileAsync("ps", ["-o", "rss=", "-p", String(pid)], {
      timeout: 2_000,
    });
    const value = Number(result.stdout.trim());
    return Number.isFinite(value) && value >= 0 ? value * 1_024 : null;
  } catch {
    return null;
  }
}

function startResourceSampler(config, child, resourceState, startedAt) {
  if (!config.resourceSamples) return { async stop() {} };
  const initialCpu = process.cpuUsage();
  let pending = Promise.resolve();
  let sampling = false;
  const sample = async () => {
    const memory = process.memoryUsage();
    const cpu = process.cpuUsage(initialCpu);
    const backendRssBytes = await sampleBackendRss(child.pid);
    if (backendRssBytes === null) resourceState.backendRssUnavailable += 1;
    const entry = {
      elapsedMs: Date.now() - startedAt,
      runnerCpuSystemMicros: cpu.system,
      runnerCpuUserMicros: cpu.user,
      runnerHeapUsedBytes: memory.heapUsed,
      runnerRssBytes: memory.rss,
      backendRssBytes,
    };
    if (resourceState.samples.length >= config.resourceSampleLimit) {
      resourceState.samples.shift();
      resourceState.dropped += 1;
    }
    resourceState.samples.push(entry);
  };
  const schedule = () => {
    if (sampling) {
      resourceState.skipped += 1;
      return;
    }
    sampling = true;
    pending = sample().finally(() => {
      sampling = false;
    });
  };
  schedule();
  const interval = setInterval(() => {
    schedule();
  }, config.resourceIntervalMs);
  interval.unref();
  return {
    async stop() {
      clearInterval(interval);
      await pending;
    },
  };
}

async function runSelfTest(config) {
  const started = performance.now();
  const checks = [];
  const provider = await startMockProvider(config);
  try {
    const response = await fetch(`${provider.baseUrl}/chat/completions`, {
      body: JSON.stringify({
        messages: [{ content: "self test", role: "user" }],
        model: config.providerModel,
        stream: true,
        stream_options: { include_usage: true },
      }),
      headers: { "Content-Type": "application/json" },
      method: "POST",
      signal: AbortSignal.timeout(config.requestTimeoutMs),
    });
    const text = await readResponseText(response, config.maxResponseBytes);
    if (response.status !== 200 || !text.includes("data: [DONE]")) {
      throw new VerifierError("self_test_provider_failed");
    }
    checks.push("provider_dynamic_port");

    const query = "mixselftest00";
    const sessionId = `session_${"a".repeat(32)}`;
    const title = "Self test session";
    const modelLabel = `lmstudio/${config.providerModel}`;
    const callId = toolCallId(query);
    const tool = {
      function: {
        description: SESSION_SEARCH_DESCRIPTION,
        name: "session_search",
        parameters: SESSION_SEARCH_SCHEMA,
        strict: true,
      },
      type: "function",
    };
    const userMessage = {
      content: `self test [session_search:${query}:${sessionId}:${encodedProbeValue(title)}:${encodedProbeValue(modelLabel)}]`,
      role: "user",
    };
    const providerBody = (messages) => ({
      messages,
      model: config.providerModel,
      stream: true,
      stream_options: { include_usage: true },
      tool_choice: "auto",
      tools: [tool],
    });
    const assistantToolCall = (id = callId) => ({
      content: null,
      role: "assistant",
      tool_calls: [{
        function: {
          arguments: JSON.stringify({ limit: 5, query }),
          name: "session_search",
        },
        id,
        type: "function",
      }],
    });
    const resultItem = {
      id: sessionId,
      match: { field: "message", snippet: `found ${query}` },
      model: modelLabel,
      preview: `self test ${query}`,
      title,
      updatedAt: "2026-07-20T00:00:00Z",
    };
    const toolResult = (items, id = callId) => ({
      content: JSON.stringify({ items }),
      role: "tool",
      tool_call_id: id,
    });
    const toolCallResponse = await fetch(`${provider.baseUrl}/chat/completions`, {
      body: JSON.stringify(providerBody([userMessage])),
      headers: { "Content-Type": "application/json" },
      method: "POST",
      signal: AbortSignal.timeout(config.requestTimeoutMs),
    });
    const toolCallText = await readResponseText(toolCallResponse, config.maxResponseBytes);
    if (
      toolCallResponse.status !== 200
      || !toolCallText.includes(callId)
      || !toolCallText.includes('"finish_reason":"tool_calls"')
    ) {
      throw new VerifierError("self_test_tool_call_failed");
    }
    const continuationResponse = await fetch(`${provider.baseUrl}/chat/completions`, {
      body: JSON.stringify(providerBody([
        userMessage,
        assistantToolCall(),
        toolResult([resultItem]),
      ])),
      headers: { "Content-Type": "application/json" },
      method: "POST",
      signal: AbortSignal.timeout(config.requestTimeoutMs),
    });
    const continuationText = await readResponseText(
      continuationResponse,
      config.maxResponseBytes,
    );
    if (
      continuationResponse.status !== 200
      || !continuationText.includes('"finish_reason":"stop"')
      || !continuationText.includes("data: [DONE]")
      || provider.state.toolCallsIssued !== 1
      || provider.state.toolResultsValidated !== 1
      || provider.state.pendingToolCalls.size !== 0
    ) {
      throw new VerifierError("self_test_tool_result_failed");
    }
    checks.push("provider_session_search_round_trip");

    const negativeProvider = await startMockProvider(config);
    try {
      const initial = await fetch(`${negativeProvider.baseUrl}/chat/completions`, {
        body: JSON.stringify(providerBody([userMessage])),
        headers: { "Content-Type": "application/json" },
        method: "POST",
        signal: AbortSignal.timeout(config.requestTimeoutMs),
      });
      await readResponseText(initial, config.maxResponseBytes);
      if (initial.status !== 200) throw new VerifierError("self_test_tool_negative_setup_failed");

      const wrongCallId = `${callId}-wrong`;
      const wrongCall = await fetch(`${negativeProvider.baseUrl}/chat/completions`, {
        body: JSON.stringify(providerBody([
          userMessage,
          assistantToolCall(wrongCallId),
          toolResult([resultItem], wrongCallId),
        ])),
        headers: { "Content-Type": "application/json" },
        method: "POST",
        signal: AbortSignal.timeout(config.requestTimeoutMs),
      });
      await readResponseText(wrongCall, config.maxResponseBytes);
      const extraItem = { ...resultItem, id: `session_${"b".repeat(32)}` };
      const foreignResult = await fetch(`${negativeProvider.baseUrl}/chat/completions`, {
        body: JSON.stringify(providerBody([
          userMessage,
          assistantToolCall(),
          toolResult([resultItem, extraItem]),
        ])),
        headers: { "Content-Type": "application/json" },
        method: "POST",
        signal: AbortSignal.timeout(config.requestTimeoutMs),
      });
      await readResponseText(foreignResult, config.maxResponseBytes);
      if (
        wrongCall.status !== 422
        || foreignResult.status !== 422
        || negativeProvider.state.failures !== 2
        || negativeProvider.state.pendingToolCalls.size !== 1
      ) throw new VerifierError("self_test_tool_negative_failed");
      checks.push("provider_tool_probe_fail_closed");
    } finally {
      await negativeProvider.close();
    }

    const body = new ReadableStream({
      start(controller) {
        controller.enqueue(new TextEncoder().encode(
          "event: run.started\ndata: {\"schemaVersion\":1}\n\n"
          + "event: run.completed\ndata: {\"schemaVersion\":1}\n\n",
        ));
        controller.close();
      },
    });
    const parsed = await consumeSseBody(body, config);
    if (parsed.terminal !== "run.completed" || parsed.events !== 2) {
      throw new VerifierError("self_test_sse_failed");
    }
    checks.push("sse_terminal_parser");

    const latencies = new LatencyBook(10);
    latencies.observe("self", 1, true);
    latencies.observe("self", 2, false);
    if (latencies.summary().self.failures !== 1) {
      throw new VerifierError("self_test_latency_failed");
    }
    checks.push("bounded_latency_summary");

    const eightHourConfig = parseConfiguration(["--duration-seconds", "28800"]);
    if (
      eightHourConfig.durationMs !== 28_800_000
      || eightHourConfig.resourceSampleLimit !== 5_762
    ) {
      throw new VerifierError("self_test_duration_upper_bound_failed");
    }
    checks.push("eight_hour_duration_upper_bound");

    const unavailableResourceState = {
      backendRssUnavailable: 0,
      dropped: 0,
      samples: [],
      skipped: 0,
    };
    const unavailableSampler = startResourceSampler(
      {
        ...config,
        resourceIntervalMs: 60_000,
        resourceSampleLimit: 1,
        resourceSamples: true,
      },
      { pid: null },
      unavailableResourceState,
      Date.now(),
    );
    await unavailableSampler.stop();
    if (
      unavailableResourceState.backendRssUnavailable <= 0
      || unavailableResourceState.samples.length !== 1
      || unavailableResourceState.samples[0].backendRssBytes !== null
      || unavailableResourceState.dropped !== 0
      || unavailableResourceState.skipped !== 0
    ) {
      throw new VerifierError("self_test_backend_rss_unavailable_failed");
    }
    checks.push("backend_rss_unavailable_accounting");

    const secret = randomBytes(32).toString("hex");
    const safe = JSON.stringify(failureRecord("self", new Error(secret)));
    if (safe.includes(secret)) throw new VerifierError("self_test_redaction_failed");
    checks.push("secret_free_failure_record");
  } finally {
    await provider.close();
  }
  return {
    schemaVersion: 2,
    mode: "self-test",
    status: "passed",
    checks,
    durationMs: roundMilliseconds(performance.now() - started),
  };
}

async function runVerifier(config, externalSignal) {
  const startedAt = Date.now();
  const startedIso = new Date(startedAt).toISOString();
  const runTag = randomBytes(5).toString("hex");
  const token = randomBytes(32).toString("hex");
  const sensitiveValues = [token];
  const failures = [];
  let failuresDropped = 0;
  const recordFailure = (stage, error) => {
    if (failures.length < config.failureLimit) failures.push(failureRecord(stage, error));
    else failuresDropped += 1;
  };
  const latencies = new LatencyBook(config.latencySampleLimit);
  const eventCounts = {};
  const processMetrics = { stderrBytes: 0, stdoutBytes: 0 };
  const resourceState = {
    backendRssUnavailable: 0,
    dropped: 0,
    samples: [],
    skipped: 0,
  };
  const cleanup = {
    backendForced: false,
    backendStopped: false,
    providerStopped: false,
    tempRemoved: false,
  };

  await mkdir(config.tempBase, { recursive: true });
  const runtimeRoot = await mkdtemp(join(config.tempBase, "synthchat-mixed-runtime-"));
  const hermesHome = join(runtimeRoot, "hermes-home");
  let provider;
  let backend;
  let provenance;
  let sampler = { async stop() {} };
  let workload = {
    elapsedMs: 0,
    failures: 0,
    iterationsCompleted: 0,
    iterationsStarted: 0,
    successes: 0,
  };
  let fatalError;
  try {
    await mkdir(hermesHome, { recursive: true });
    provider = await timed(latencies, "setup.provider", () => startMockProvider(config));
    const executable = await timed(latencies, "setup.backend_build", () => (
      resolveBackendBinary(config)
    ));
    provenance = await collectProvenance(config, executable);
    backend = await timed(latencies, "setup.backend_start", () => (
      startBackend(config, executable, hermesHome, token, processMetrics)
    ));
    sampler = startResourceSampler(config, backend.child, resourceState, startedAt);
    const profileId = await timed(latencies, "setup.profile", () => (
      configureProfile(backend, provider.baseUrl, config, runTag, externalSignal)
    ));
    workload = await runWorkload({
      config,
      eventCounts,
      latencies,
      profileId,
      recordFailure,
      runTag,
      runtime: backend,
    }, externalSignal);
  } catch (error) {
    fatalError = error;
    recordFailure("fatal", error);
  } finally {
    await sampler.stop().catch(() => undefined);
    if (backend) {
      const termination = await terminateChild(backend.child, config.shutdownTimeoutMs);
      cleanup.backendForced = termination.forced;
      cleanup.backendStopped = backend.child.exitCode !== null || backend.child.signalCode !== null;
      if (cleanup.backendForced) {
        recordFailure("cleanup", new VerifierError("backend_shutdown_forced"));
      }
      if (!cleanup.backendStopped) {
        recordFailure("cleanup", new VerifierError("backend_shutdown_failed"));
      }
    }
    if (provider) {
      try {
        await provider.close();
        cleanup.providerStopped = true;
      } catch {
        cleanup.providerStopped = false;
        recordFailure("cleanup", new VerifierError("provider_shutdown_failed"));
      }
    }
    if (provider && provider.state.failures > 0) {
      recordFailure("provider", new VerifierError("provider_validation_failed"));
    }
    const expectedToolCalls = expectedToolCallCount(
      workload.iterationsStarted,
      config.toolEveryIterations,
    );
    if (
      provider
      && workload.failures === 0
      && !fatalError
      && (
        provider.state.pendingToolCalls.size !== 0
        || provider.state.normalCompletions !== workload.iterationsStarted - expectedToolCalls
        || provider.state.toolCallsIssued !== expectedToolCalls
        || provider.state.toolResultsValidated !== expectedToolCalls
        || provider.state.requestCount !== workload.iterationsStarted + expectedToolCalls
      )
    ) {
      recordFailure("provider", new VerifierError("provider_tool_probe_mismatch"));
    }
    const expectedEvents = {
      "message.completed": workload.iterationsStarted,
      "message.delta": workload.iterationsStarted * expectedMessageDeltaCount(config),
      "message.started": workload.iterationsStarted,
      "run.completed": workload.iterationsStarted,
      "run.started": workload.iterationsStarted,
      "tool.completed": expectedToolCalls,
      "tool.started": expectedToolCalls,
      "usage.updated": workload.iterationsStarted + expectedToolCalls,
    };
    if (
      workload.failures === 0
      && !fatalError
      && (
        !exactJson(eventCounts, expectedEvents)
        || workload.iterationsCompleted !== workload.iterationsStarted
        || workload.successes !== workload.iterationsStarted
      )
    ) {
      recordFailure("events", new VerifierError("event_conservation_mismatch"));
    }
    try {
      await rm(runtimeRoot, { force: true, maxRetries: 3, recursive: true });
      cleanup.tempRemoved = true;
    } catch {
      recordFailure("cleanup", new VerifierError("temp_cleanup_failed"));
    }
  }

  const interrupted = externalSignal?.aborted ?? false;
  const failed = Boolean(fatalError) || workload.failures > 0 || failures.length > 0;
  const result = {
    schemaVersion: 2,
    mode: config.smoke ? "smoke" : "pilot",
    status: interrupted ? "interrupted" : failed ? "failed" : "passed",
    startedAt: startedIso,
    finishedAt: new Date().toISOString(),
    durationMs: Date.now() - startedAt,
    configuration: {
      concurrency: config.concurrency,
      cycleDelayMs: config.cycleDelayMs,
      durationMs: config.durationMs,
      maxFailures: config.maxFailures,
      maxIterations: config.maxIterations,
      providerDelayMs: config.providerDelayMs,
      providerReplyBytes: Buffer.byteLength(config.providerReply),
      providerReplyCodePoints: [...config.providerReply].length,
      latencySampleLimit: config.latencySampleLimit,
      resourceIntervalMs: config.resourceIntervalMs,
      resourceSampleLimit: config.resourceSampleLimit,
      resourceSamples: config.resourceSamples,
      toolEveryIterations: config.toolEveryIterations,
    },
    workload: {
      ...workload,
      toolProbesExpected: expectedToolCallCount(
        workload.iterationsStarted,
        config.toolEveryIterations,
      ),
    },
    provider: {
      failures: provider?.state.failures ?? 0,
      maxActiveRequests: provider?.state.maxActiveRequests ?? 0,
      normalCompletions: provider?.state.normalCompletions ?? 0,
      pendingToolCalls: provider?.state.pendingToolCalls.size ?? 0,
      rejections: Object.fromEntries(Object.entries(provider?.state.rejections ?? {}).sort()),
      requests: provider?.state.requestCount ?? 0,
      toolCallsIssued: provider?.state.toolCallsIssued ?? 0,
      toolResultsValidated: provider?.state.toolResultsValidated ?? 0,
    },
    events: Object.fromEntries(Object.entries(eventCounts).sort(([left], [right]) => (
      left.localeCompare(right)
    ))),
    latenciesMs: latencies.summary(),
    resources: {
      backendRssUnavailable: resourceState.backendRssUnavailable,
      dropped: resourceState.dropped,
      skipped: resourceState.skipped,
      samples: resourceState.samples,
    },
    failures,
    failuresDropped,
    backendLogs: processMetrics,
    cleanup,
    provenance,
  };
  const serialized = JSON.stringify(result);
  if (sensitiveValues.some((value) => serialized.includes(value))) {
    throw new VerifierError("result_secret_scan_failed");
  }
  return result;
}

async function emitResult(result, outputPath) {
  const serialized = `${JSON.stringify(result)}\n`;
  if (outputPath) {
    await mkdir(dirname(outputPath), { recursive: true });
    await writeFile(outputPath, serialized, { encoding: "utf8", mode: 0o600 });
  }
  process.stdout.write(serialized);
}

function minimalFailure(error, mode = "startup") {
  return {
    schemaVersion: 2,
    mode,
    status: "failed",
    failures: [failureRecord("fatal", error)],
  };
}

async function main() {
  if (Number(process.versions.node.split(".")[0]) < 22) {
    await emitResult(minimalFailure(new VerifierError("node_22_required")));
    process.exitCode = 1;
    return;
  }
  let config;
  try {
    config = parseConfiguration(process.argv.slice(2));
  } catch (error) {
    await emitResult(minimalFailure(error));
    process.exitCode = 1;
    return;
  }
  if (config.help) {
    process.stdout.write(`${helpText()}\n`);
    return;
  }
  try {
    if (config.selfTest) {
      await emitResult(await runSelfTest(config), config.output);
      return;
    }
    const controller = new AbortController();
    const interrupt = () => controller.abort();
    process.once("SIGINT", interrupt);
    process.once("SIGTERM", interrupt);
    const result = await runVerifier(config, controller.signal);
    await emitResult(result, config.output);
    if (result.status !== "passed") process.exitCode = 1;
  } catch (error) {
    await emitResult(minimalFailure(error, config.smoke ? "smoke" : "pilot"), config.output);
    process.exitCode = 1;
  }
}

await main();
