import { spawn } from "node:child_process";
import { createHash, randomBytes, timingSafeEqual } from "node:crypto";
import { once } from "node:events";
import { access, mkdir, mkdtemp, rm } from "node:fs/promises";
import { createServer as createHttpServer, request as httpRequest } from "node:http";
import { isIP } from "node:net";
import { tmpdir } from "node:os";
import { basename, dirname, isAbsolute, join, resolve } from "node:path";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = resolve(scriptDirectory, "../..");
const frontendRoot = join(repositoryRoot, "frontend");
const backendManifest = join(repositoryRoot, "backend", "Cargo.toml");
const require = createRequire(import.meta.url);
const managedProcesses = [];
const managedServers = [];
let runtimeRoot;
let cleanupPromise;
let activeBackendSupervisor;
let shutdownRequested = false;
const preserveRunsShutdownCommand = "SYNTHCHAT_PRESERVE_RUNS\n";

function environment(name, fallback) {
  const value = process.env[name]?.trim();
  return value || fallback;
}

function positiveInteger(name, fallback) {
  const raw = process.env[name]?.trim();
  if (!raw) return fallback;
  const value = Number(raw);
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new Error(`${name} must be a positive integer.`);
  }
  return value;
}

function booleanEnvironment(name, fallback) {
  const raw = process.env[name]?.trim().toLowerCase();
  if (!raw) return fallback;
  if (raw === "1" || raw === "true") return true;
  if (raw === "0" || raw === "false") return false;
  throw new Error(`${name} must be true, false, 1, or 0.`);
}

function loopbackHost() {
  const host = environment("SYNTHCHAT_E2E_HOST", "127.0.0.1");
  if (isIP(host) === 0 || !["127.0.0.1", "::1"].includes(host)) {
    throw new Error("SYNTHCHAT_E2E_HOST must be a loopback IP address.");
  }
  return host;
}

function socketAddress(host, port) {
  return host.includes(":") ? `[${host}]:${port}` : `${host}:${port}`;
}

function origin(host, port) {
  return `http://${socketAddress(host, port)}`;
}

function configuredPath(name, fallback) {
  const value = environment(name, fallback);
  return isAbsolute(value) ? value : resolve(repositoryRoot, value);
}

function portableRelativeFilePath(name, fallback) {
  const value = environment(name, fallback);
  const components = value.split("/");
  if (
    isAbsolute(value)
    || value.includes("\\")
    || components.some((component) => (
      !component
      || component === "."
      || component === ".."
      || component.endsWith(" ")
      || component.endsWith(".")
      || /[<>:"|?*]/u.test(component)
      || [...component].some((character) => character <= "\u001f")
    ))
  ) {
    throw new Error(`${name} must be a portable Workspace-relative file path.`);
  }
  return value;
}

function defaultRustToolchain() {
  if (process.platform === "win32" && process.arch === "x64") {
    return "1.88.0-x86_64-pc-windows-msvc";
  }
  return "1.88.0";
}

function spawnProcess(name, executable, args, options = {}) {
  if (shutdownRequested) throw new Error(`Cannot start ${name} while E2E shutdown is in progress.`);
  const child = spawn(executable, args, {
    cwd: options.cwd || repositoryRoot,
    env: options.env || process.env,
    shell: false,
    stdio: options.stdio || (options.captureStdout ? ["ignore", "pipe", "pipe"] : "inherit"),
    detached: process.platform !== "win32",
    windowsHide: true,
  });
  managedProcesses.push({ child, name });
  return child;
}

async function runCommand(name, executable, args, options = {}) {
  const child = spawnProcess(name, executable, args, options);
  const exitPromise = once(child, "exit");
  let result;
  try {
    result = options.timeoutMs
      ? await withTimeout(exitPromise, options.timeoutMs, `${name} timed out.`)
      : await exitPromise;
  } catch (error) {
    await terminateProcess(
      { child, name },
      positiveInteger("SYNTHCHAT_E2E_SHUTDOWN_TIMEOUT_MS", 5_000),
    );
    throw error;
  }
  const [exitCode, signal] = result;
  if (exitCode !== 0) {
    throw new Error(`${name} failed with ${signal ? `signal ${signal}` : `exit code ${exitCode}`}.`);
  }
}

async function withTimeout(promise, timeoutMs, message) {
  let timeoutId;
  try {
    return await Promise.race([
      promise,
      new Promise((_, reject) => {
        timeoutId = setTimeout(() => reject(new Error(message)), timeoutMs);
      }),
    ]);
  } finally {
    clearTimeout(timeoutId);
  }
}

function delay(durationMs) {
  return new Promise((resolveDelay) => setTimeout(resolveDelay, durationMs));
}

async function waitForHttp(url, timeoutMs, pollIntervalMs) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(url, { cache: "no-store", redirect: "error" });
      if (response.ok) return;
      lastError = new Error(`HTTP ${response.status}`);
    } catch (error) {
      lastError = error;
    }
    await delay(pollIntervalMs);
  }
  throw new Error(`Timed out waiting for ${url}.`, { cause: lastError });
}

async function waitForAuthenticatedBackend(backendOrigin, token, timeoutMs, pollIntervalMs) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(`${backendOrigin}/api/v1/capabilities`, {
        cache: "no-store",
        headers: {
          Accept: "application/json",
          Authorization: `Bearer ${token}`,
        },
        redirect: "error",
      });
      if (response.status === 200) return;
      lastError = new Error(`HTTP ${response.status}`);
    } catch (error) {
      lastError = error;
    }
    await delay(pollIntervalMs);
  }
  throw new Error("Timed out verifying the authenticated backend generation.", {
    cause: lastError,
  });
}

async function waitForJsonReady(child, name, timeoutMs) {
  let buffer = "";
  const maxBytes = positiveInteger("SYNTHCHAT_E2E_READY_MAX_BYTES", 1_048_576);
  child.stderr.on("data", (chunk) => process.stderr.write(`[${name}] ${chunk}`));
  const ready = new Promise((resolveReady, reject) => {
    child.once("error", reject);
    child.once("exit", (code, signal) => {
      reject(new Error(`${name} exited before readiness (${signal || code}).`));
    });
    child.stdout.on("data", (chunk) => {
      buffer += chunk.toString("utf8");
      if (Buffer.byteLength(buffer, "utf8") > maxBytes) {
        reject(new Error(`${name} readiness output exceeded its configured bound.`));
        return;
      }
      while (buffer.includes("\n")) {
        const index = buffer.indexOf("\n");
        const line = buffer.slice(0, index).trim();
        buffer = buffer.slice(index + 1);
        if (!line) continue;
        let message;
        try {
          message = JSON.parse(line);
        } catch {
          process.stdout.write(`[${name}] ${line}\n`);
          continue;
        }
        if (message?.event === "ready" && typeof message.baseUrl === "string") {
          resolveReady(message);
        }
      }
    });
  });
  return withTimeout(ready, timeoutMs, `Timed out waiting for ${name}.`);
}

function checkedReadyAddress(value, expectedHost) {
  let url;
  try {
    url = new URL(`http://${value}`);
  } catch {
    throw new Error("Backend readiness handshake contained an invalid address.");
  }
  const hostname = url.hostname.replace(/^\[|\]$/gu, "");
  const port = Number(url.port);
  if (
    hostname !== expectedHost
    || !Number.isSafeInteger(port)
    || port <= 0
    || port > 65_535
    || url.pathname !== "/"
    || url.search
    || url.hash
  ) {
    throw new Error("Backend readiness handshake did not match its loopback bind request.");
  }
  return { address: socketAddress(expectedHost, port), origin: origin(expectedHost, port) };
}

async function waitForBackendHandshake(child, expectedHost, timeoutMs) {
  let buffer = "";
  const maxBytes = positiveInteger("SYNTHCHAT_E2E_BACKEND_HANDSHAKE_MAX_BYTES", 4_096);
  const handshake = new Promise((resolveReady, reject) => {
    child.once("error", reject);
    child.once("exit", (code, signal) => {
      reject(new Error(`Backend exited before readiness (${signal || code}).`));
    });
    child.stdout.on("data", (chunk) => {
      buffer += chunk.toString("utf8");
      if (Buffer.byteLength(buffer, "utf8") > maxBytes) {
        reject(new Error("Backend readiness handshake exceeded its configured bound."));
        return;
      }
      while (buffer.includes("\n")) {
        const index = buffer.indexOf("\n");
        const line = buffer.slice(0, index).trim();
        buffer = buffer.slice(index + 1);
        if (!line) continue;
        const prefix = "SYNTHCHAT_BACKEND_READY ";
        if (!line.startsWith(prefix)) {
          reject(new Error("Backend emitted unexpected stdout before readiness."));
          return;
        }
        resolveReady(checkedReadyAddress(line.slice(prefix.length), expectedHost));
        return;
      }
    });
  });
  return withTimeout(handshake, timeoutMs, "Timed out waiting for the backend handshake.");
}

async function assertSecretIsServerSide(frontendOrigin, secret) {
  for (const path of ["/", "/src/main.tsx", "/src/api/backend.ts", "/src/api/desktopConnection.ts"]) {
    const response = await fetch(`${frontendOrigin}${path}`, { cache: "no-store" });
    if (!response.ok) throw new Error(`Unable to audit frontend resource ${path}: HTTP ${response.status}.`);
    if ((await response.text()).includes(secret)) {
      throw new Error(`A local E2E capability leaked into the frontend resource ${path}.`);
    }
  }
}

function secureEqual(actual, expected) {
  if (typeof actual !== "string") return false;
  const actualBytes = Buffer.from(actual, "utf8");
  const expectedBytes = Buffer.from(expected, "utf8");
  return actualBytes.length === expectedBytes.length
    && timingSafeEqual(actualBytes, expectedBytes);
}

function jsonResponse(response, status, value) {
  const body = JSON.stringify(value);
  response.writeHead(status, {
    "Cache-Control": "no-store",
    "Content-Length": Buffer.byteLength(body),
    "Content-Type": "application/json; charset=utf-8",
  });
  response.end(body);
}

function backendUnavailableResponse(response, status, code, title) {
  jsonResponse(response, status, {
    type: "about:blank",
    title,
    status,
    code,
    requestId: `e2e-relay-${Date.now().toString(36)}`,
    retryable: true,
  });
}

function requestPath(request) {
  const rawUrl = request.url || "/";
  if (!rawUrl.startsWith("/") || rawUrl.startsWith("//")) return null;
  try {
    return new URL(rawUrl, "http://loopback.invalid");
  } catch {
    return null;
  }
}

const HOP_BY_HOP_HEADERS = new Set([
  "connection",
  "keep-alive",
  "proxy-authenticate",
  "proxy-authorization",
  "proxy-connection",
  "te",
  "trailer",
  "transfer-encoding",
  "upgrade",
]);

function forwardedRequestHeaders(headers, backendAddress, backendToken, protectedRequest) {
  const result = {};
  for (const [name, value] of Object.entries(headers)) {
    if (value === undefined || HOP_BY_HOP_HEADERS.has(name.toLowerCase())) continue;
    if (["authorization", "host"].includes(name.toLowerCase())) continue;
    result[name] = value;
  }
  result.host = backendAddress;
  if (protectedRequest) result.authorization = `Bearer ${backendToken}`;
  return result;
}

function forwardedResponseHeaders(headers) {
  const result = {};
  for (const [name, value] of Object.entries(headers)) {
    if (value === undefined || HOP_BY_HOP_HEADERS.has(name.toLowerCase())) continue;
    result[name] = value;
  }
  return result;
}

function proxyBackendRequest(
  request,
  response,
  url,
  backend,
  protectedRequest,
  activeUpstreams,
) {
  const upstreamUrl = new URL(`${url.pathname}${url.search}`, backend.origin);
  const active = {
    downstreamResponse: response,
    upstreamRequest: undefined,
    upstreamResponse: undefined,
  };
  const removeActive = () => activeUpstreams.delete(active);
  const failDownstream = () => {
    if (!response.headersSent) {
      backendUnavailableResponse(
        response,
        502,
        "backend_connection_failed",
        "Backend connection failed",
      );
    } else if (!response.destroyed) {
      response.destroy();
    }
    removeActive();
  };
  const upstream = httpRequest(upstreamUrl, {
    headers: forwardedRequestHeaders(
      request.headers,
      backend.address,
      backend.token,
      protectedRequest,
    ),
    method: request.method,
  }, (upstreamResponse) => {
    active.upstreamResponse = upstreamResponse;
    activeUpstreams.add(active);
    response.writeHead(
      upstreamResponse.statusCode || 502,
      forwardedResponseHeaders(upstreamResponse.headers),
    );
    upstreamResponse.once("aborted", failDownstream);
    upstreamResponse.once("error", failDownstream);
    upstreamResponse.once("end", removeActive);
    upstreamResponse.once("close", () => {
      if (!upstreamResponse.complete) failDownstream();
      else removeActive();
    });
    upstreamResponse.pipe(response);
  });
  active.upstreamRequest = upstream;
  activeUpstreams.add(active);

  upstream.once("error", failDownstream);
  request.once("aborted", () => upstream.destroy());
  response.once("close", () => {
    if (!response.writableEnded) upstream.destroy();
    removeActive();
  });
  if (request.readableEnded) {
    upstream.end();
  } else {
    request.pipe(upstream);
  }
}

function createBackendRelay(ingressToken) {
  let backend = null;
  const activeUpstreams = new Set();
  const pendingEventStreams = new Set();
  const eventWaitTimeoutMs = positiveInteger(
    "SYNTHCHAT_E2E_RELAY_EVENT_WAIT_TIMEOUT_MS",
    120_000,
  );

  const flushEventStreams = (selectedBackend) => {
    for (const pending of [...pendingEventStreams]) {
      pendingEventStreams.delete(pending);
      clearTimeout(pending.timeoutId);
      pending.started = true;
      if (pending.request.destroyed || pending.response.destroyed) continue;
      proxyBackendRequest(
        pending.request,
        pending.response,
        pending.url,
        selectedBackend,
        true,
        activeUpstreams,
      );
    }
  };

  const server = createHttpServer((request, response) => {
    const url = requestPath(request);
    if (!url) {
      jsonResponse(response, 400, { error: "invalid_request_target" });
      return;
    }
    const healthRequest = url.pathname === "/health";
    const protectedRequest = url.pathname === "/api/v1" || url.pathname.startsWith("/api/v1/");
    if (!healthRequest && !protectedRequest) {
      jsonResponse(response, 404, { error: "not_found" });
      return;
    }
    if (
      protectedRequest
      && !secureEqual(request.headers.authorization, `Bearer ${ingressToken}`)
    ) {
      jsonResponse(response, 401, { error: "unauthorized" });
      return;
    }

    const selectedBackend = backend;
    if (!selectedBackend) {
      const resumableEventStream = request.method === "GET"
        && /^\/api\/v1\/runs\/[^/]+\/events$/u.test(url.pathname);
      if (resumableEventStream) {
        const pending = {
          request,
          response,
          started: false,
          timeoutId: undefined,
          url,
        };
        const removePending = () => {
          if (pending.started) return;
          pendingEventStreams.delete(pending);
          clearTimeout(pending.timeoutId);
        };
        pending.timeoutId = setTimeout(() => {
          if (pending.started) return;
          pendingEventStreams.delete(pending);
          response.destroy();
        }, eventWaitTimeoutMs);
        request.once("aborted", removePending);
        response.once("close", removePending);
        pendingEventStreams.add(pending);
        return;
      }
      if (protectedRequest) {
        backendUnavailableResponse(
          response,
          503,
          "backend_unavailable",
          "Backend unavailable",
        );
      } else {
        jsonResponse(response, 503, { error: "backend_unavailable" });
      }
      return;
    }
    proxyBackendRequest(
      request,
      response,
      url,
      selectedBackend,
      protectedRequest,
      activeUpstreams,
    );
  });

  return {
    server,
    setBackend(nextBackend) {
      backend = nextBackend;
      if (nextBackend) {
        flushEventStreams(nextBackend);
      } else {
        for (const active of [...activeUpstreams]) {
          active.upstreamRequest?.destroy();
          active.upstreamResponse?.destroy();
          active.downstreamResponse.destroy();
          activeUpstreams.delete(active);
        }
      }
    },
  };
}

async function listenServer(name, server, host) {
  if (shutdownRequested) throw new Error(`Cannot start ${name} while E2E shutdown is in progress.`);
  await new Promise((resolveListening, reject) => {
    server.once("error", reject);
    server.listen({ host, port: 0, exclusive: true }, resolveListening);
  });
  const address = server.address();
  if (!address || typeof address === "string") {
    throw new Error(`${name} did not expose a loopback TCP address.`);
  }
  managedServers.push({ name, server });
  return origin(host, address.port);
}

function createBackendSupervisor({
  allowedOrigin,
  backendEnvironment = {},
  backendBinary,
  hermesHome,
  host,
  onStarted,
  pollIntervalMs,
  relay,
  startupTimeoutMs,
}) {
  let current = null;
  let generation = 0;
  let lastIdentity = null;
  let state = "offline";
  let operation = Promise.resolve();

  const serialize = (task) => {
    const result = operation.then(task, task);
    operation = result.catch(() => undefined);
    return result;
  };

  const terminateCandidate = async (candidate, name, finalInput) => {
    if (!candidate || candidate.exitCode !== null || candidate.signalCode !== null) return;
    const exited = once(candidate, "exit");
    candidate.stdin?.end(finalInput);
    try {
      await withTimeout(
        exited,
        positiveInteger("SYNTHCHAT_E2E_SHUTDOWN_TIMEOUT_MS", 5_000),
        `${name} did not stop after stdin closed.`,
      );
    } catch {
      await terminateProcess(
        { child: candidate, name },
        positiveInteger("SYNTHCHAT_E2E_SHUTDOWN_TIMEOUT_MS", 5_000),
      );
    }
  };

  const startInternal = async () => {
    if (current) return { generation, state: "online" };
    state = "starting";
    const attempts = positiveInteger("SYNTHCHAT_E2E_BACKEND_START_ATTEMPTS", 5);

    try {
      for (let attempt = 1; attempt <= attempts; attempt += 1) {
        let token = randomBytes(32).toString("hex");
        while (lastIdentity?.token === token) token = randomBytes(32).toString("hex");
        const nextGeneration = generation + 1;
        const childEnvironment = { ...process.env, ...backendEnvironment };
        delete childEnvironment.SYNTHCHAT_DESKTOP_TOKEN;
        Object.assign(childEnvironment, {
          HERMES_HOME: hermesHome,
          SYNTHCHAT_ALLOWED_ORIGINS: allowedOrigin,
          SYNTHCHAT_BACKEND_ADDR: socketAddress(host, 0),
        });
        const child = spawnProcess(
          `backend generation ${nextGeneration}.${attempt}`,
          backendBinary,
          [],
          {
            env: childEnvironment,
            stdio: ["pipe", "pipe", "pipe"],
          },
        );
        child.stderr.on("data", (chunk) => {
          process.stderr.write(`[backend:${nextGeneration}] ${chunk}`);
        });
        child.stdin.on("error", () => undefined);
        child.stdin.write(`${token}\n`);

        let ready;
        try {
          ready = await waitForBackendHandshake(child, host, startupTimeoutMs);
          await waitForAuthenticatedBackend(
            ready.origin,
            token,
            startupTimeoutMs,
            pollIntervalMs,
          );
        } catch (error) {
          await terminateCandidate(child, `backend generation ${nextGeneration}.${attempt}`);
          throw error;
        }

        if (lastIdentity?.address === ready.address) {
          await terminateCandidate(child, `backend generation ${nextGeneration}.${attempt}`);
          if (attempt === attempts) {
            throw new Error("The operating system repeatedly reused the previous backend port.");
          }
          continue;
        }

        const started = {
          address: ready.address,
          child,
          generation: nextGeneration,
          origin: ready.origin,
          token,
        };
        current = started;
        generation = nextGeneration;
        lastIdentity = { address: ready.address, token };
        relay.setBackend(started);
        child.once("exit", () => {
          if (current?.child !== child) return;
          current = null;
          relay.setBackend(null);
          state = "offline";
        });
        try {
          await onStarted(started);
          if (child.exitCode !== null || child.signalCode !== null || current?.child !== child) {
            throw new Error("Backend exited while its generation was being verified.");
          }
        } catch (error) {
          current = null;
          relay.setBackend(null);
          await terminateCandidate(child, `backend generation ${nextGeneration}`);
          throw error;
        }
        state = "online";
        return { generation, state };
      }
    } catch (error) {
      state = "offline";
      throw error;
    }

    state = "offline";
    throw new Error("Backend startup attempts were exhausted.");
  };

  const stopInternal = async () => {
    const stopped = current;
    current = null;
    relay.setBackend(null);
    if (!stopped) {
      state = "offline";
      return { generation, state };
    }
    state = "stopping";
    await terminateCandidate(stopped.child, `backend generation ${stopped.generation}`);
    state = "offline";
    return { generation, state };
  };

  const crashInternal = async () => {
    const stopped = current;
    current = null;
    relay.setBackend(null);
    if (!stopped) {
      state = "offline";
      return { generation, state };
    }
    state = "stopping";
    await terminateCandidate(
      stopped.child,
      `backend generation ${stopped.generation}`,
      preserveRunsShutdownCommand,
    );
    state = "offline";
    return { generation, state };
  };

  return {
    crash: () => serialize(crashInternal),
    restart: () => serialize(async () => {
      await stopInternal();
      return startInternal();
    }),
    snapshot: () => ({ generation, state }),
    start: () => serialize(startInternal),
    stop: () => serialize(stopInternal),
  };
}

function createControlServer(supervisor, capability) {
  const capabilityHeader = "x-synthchat-e2e-control";
  return createHttpServer((request, response) => {
    void (async () => {
      const url = requestPath(request);
      if (
        !url
        || request.headers.origin !== undefined
        || !secureEqual(request.headers[capabilityHeader], capability)
      ) {
        jsonResponse(response, 403, { error: "forbidden" });
        return;
      }
      if (request.headers["transfer-encoding"] !== undefined) {
        jsonResponse(response, 400, { error: "request_body_not_allowed" });
        return;
      }
      const contentLength = Number(request.headers["content-length"] || "0");
      if (!Number.isSafeInteger(contentLength) || contentLength !== 0) {
        jsonResponse(response, 400, { error: "request_body_not_allowed" });
        return;
      }

      let snapshot;
      if (request.method === "GET" && url.pathname === "/status") {
        snapshot = supervisor.snapshot();
      } else if (request.method === "POST" && url.pathname === "/stop") {
        snapshot = await supervisor.crash();
      } else if (request.method === "POST" && url.pathname === "/restart") {
        snapshot = await supervisor.restart();
      } else {
        jsonResponse(response, 404, { error: "not_found" });
        return;
      }
      jsonResponse(response, 200, snapshot);
    })().catch((error) => {
      process.stderr.write(`[control] ${error instanceof Error ? error.message : String(error)}\n`);
      if (!response.headersSent) {
        jsonResponse(response, 500, { error: "control_operation_failed" });
      } else {
        response.destroy();
      }
    });
  });
}

async function terminateProcess({ child, name }, timeoutMs) {
  if (child.exitCode !== null || child.signalCode !== null) return;
  const exited = once(child, "exit");
  if (child.stdin && !child.stdin.destroyed) {
    child.stdin.end();
    try {
      await withTimeout(exited, timeoutMs, `${name} did not stop after stdin closed.`);
      return;
    } catch {
      // Fall through to the bounded platform termination path.
    }
  }

  const signalProcess = (signal) => {
    try {
      if (process.platform === "win32") child.kill(signal);
      else process.kill(-child.pid, signal);
    } catch (error) {
      if (error?.code !== "ESRCH") throw error;
    }
  };
  signalProcess("SIGTERM");
  try {
    await withTimeout(exited, timeoutMs, `${name} did not stop after SIGTERM.`);
  } catch {
    signalProcess("SIGKILL");
    await withTimeout(exited, timeoutMs, `${name} did not stop after SIGKILL.`);
  }
}

async function closeServer({ name, server }, timeoutMs) {
  if (!server.listening) return;
  const closed = new Promise((resolveClosed, reject) => {
    server.close((error) => error ? reject(error) : resolveClosed());
    server.closeIdleConnections?.();
  });
  try {
    await withTimeout(closed, timeoutMs, `${name} did not stop accepting connections.`);
  } catch (error) {
    server.closeAllConnections?.();
    await withTimeout(
      closed.catch(() => undefined),
      timeoutMs,
      `${name} did not close after active connections were destroyed.`,
    );
    if (server.listening) throw error;
  }
}

async function cleanup() {
  if (cleanupPromise) return cleanupPromise;
  cleanupPromise = (async () => {
    const shutdownTimeoutMs = positiveInteger("SYNTHCHAT_E2E_SHUTDOWN_TIMEOUT_MS", 5_000);
    const errors = [];
    const attempt = async (label, operation) => {
      try {
        await operation();
      } catch (error) {
        errors.push(new Error(`Failed to clean up ${label}.`, { cause: error }));
      }
    };
    const processes = [...managedProcesses].reverse();
    for (const processInfo of processes.filter(({ name }) => name === "Playwright")) {
      await attempt(processInfo.name, () => terminateProcess(processInfo, shutdownTimeoutMs));
    }
    if (activeBackendSupervisor) {
      const supervisor = activeBackendSupervisor;
      activeBackendSupervisor = undefined;
      await attempt("backend supervisor", () => supervisor.stop());
    }
    for (const processInfo of processes.filter(({ name }) => name !== "Playwright")) {
      await attempt(processInfo.name, () => terminateProcess(processInfo, shutdownTimeoutMs));
    }
    for (const serverInfo of [...managedServers].reverse()) {
      await attempt(serverInfo.name, () => closeServer(serverInfo, shutdownTimeoutMs));
    }
    if (runtimeRoot && basename(runtimeRoot).startsWith("synthchat-hermes-e2e-")) {
      await attempt("E2E runtime directory", () => rm(runtimeRoot, {
        force: true,
        maxRetries: positiveInteger("SYNTHCHAT_E2E_RM_MAX_RETRIES", 20),
        recursive: true,
        retryDelay: positiveInteger("SYNTHCHAT_E2E_RM_RETRY_DELAY_MS", 250),
      }));
    }
    if (errors.length > 0) {
      throw new AggregateError(errors, "One or more local E2E resources could not be cleaned up.");
    }
  })();
  return cleanupPromise;
}

function installSignalHandlers() {
  const stop = (signal, exitCode) => {
    shutdownRequested = true;
    process.stderr.write(`Received ${signal}; stopping local E2E services.\n`);
    void cleanup()
      .catch((error) => process.stderr.write(`${error?.stack || error}\n`))
      .finally(() => process.exit(exitCode));
  };
  process.once("SIGINT", () => stop("SIGINT", 130));
  process.once("SIGTERM", () => stop("SIGTERM", 143));
}

async function main() {
  installSignalHandlers();
  const host = loopbackHost();
  const startupTimeoutMs = positiveInteger("SYNTHCHAT_E2E_STARTUP_TIMEOUT_MS", 120_000);
  const pollIntervalMs = positiveInteger("SYNTHCHAT_E2E_POLL_INTERVAL_MS", 100);
  const runId = randomBytes(8).toString("hex");
  const profileId = environment("SYNTHCHAT_E2E_PROFILE_ID", `e2e_${runId}`);
  const profileName = environment("SYNTHCHAT_E2E_PROFILE_NAME", `E2E ${runId}`);
  const recoveryProfileId = environment(
    "SYNTHCHAT_E2E_RECOVERY_PROFILE_ID",
    `e2e_recovery_${runId}`,
  );
  const recoveryProfileName = environment(
    "SYNTHCHAT_E2E_RECOVERY_PROFILE_NAME",
    `E2E Recovery ${runId}`,
  );
  const recoveryPrompt = environment(
    "SYNTHCHAT_E2E_RECOVERY_PROMPT",
    `E2E recovery prompt ${runId}`,
  );
  const inflightProfileId = environment(
    "SYNTHCHAT_E2E_INFLIGHT_PROFILE_ID",
    `e2e_inflight_${runId}`,
  );
  const inflightProfileName = environment(
    "SYNTHCHAT_E2E_INFLIGHT_PROFILE_NAME",
    `E2E In-flight ${runId}`,
  );
  const inflightPrompt = environment(
    "SYNTHCHAT_E2E_INFLIGHT_PROMPT",
    `E2E in-flight prompt ${runId}`,
  );
  const inflightRecoveryPrompt = environment(
    "SYNTHCHAT_E2E_INFLIGHT_RECOVERY_PROMPT",
    `E2E post-crash prompt ${runId}`,
  );
  const toolsProfileId = environment("SYNTHCHAT_E2E_TOOLS_PROFILE_ID", `e2e_tools_${runId}`);
  const toolsProfileName = environment(
    "SYNTHCHAT_E2E_TOOLS_PROFILE_NAME",
    `E2E Tools ${runId}`,
  );
  const approvalProfileId = environment(
    "SYNTHCHAT_E2E_APPROVAL_PROFILE_ID",
    `e2e_approval_${runId}`,
  );
  const approvalProfileName = environment(
    "SYNTHCHAT_E2E_APPROVAL_PROFILE_NAME",
    `E2E Approval ${runId}`,
  );
  const approvalPrompt = environment(
    "SYNTHCHAT_E2E_APPROVAL_PROMPT",
    `E2E dangerous tool approval ${runId}`,
  );
  const approvalReply = environment(
    "SYNTHCHAT_E2E_APPROVAL_REPLY",
    `Rust tool approval completed ${runId}`,
  );
  const approvalCallId = environment(
    "SYNTHCHAT_E2E_APPROVAL_CALL_ID",
    `call-e2e-approval-${runId}`,
  );
  const approvalReadCallId = environment(
    "SYNTHCHAT_E2E_APPROVAL_READ_CALL_ID",
    `call-e2e-approval-read-${runId}`,
  );
  const approvalSearchCallId = environment(
    "SYNTHCHAT_E2E_APPROVAL_SEARCH_CALL_ID",
    `call-e2e-approval-search-${runId}`,
  );
  const approvalPatchCallId = environment(
    "SYNTHCHAT_E2E_APPROVAL_PATCH_CALL_ID",
    `call-e2e-approval-patch-${runId}`,
  );
  const approvalRelativePath = portableRelativeFilePath(
    "SYNTHCHAT_E2E_APPROVAL_RELATIVE_PATH",
    `generated/approval-${runId}.txt`,
  );
  const approvalPublicNeedle = environment(
    "SYNTHCHAT_E2E_APPROVAL_PUBLIC_NEEDLE",
    `PUBLIC_NEEDLE_${runId}`,
  );
  const approvalPrivateContent = environment(
    "SYNTHCHAT_E2E_APPROVAL_PRIVATE_CONTENT",
    `${approvalPublicNeedle}::PRIVATE_${randomBytes(16).toString("hex")}`,
  );
  const approvalPatchedContent = environment(
    "SYNTHCHAT_E2E_APPROVAL_PATCHED_CONTENT",
    `${approvalPublicNeedle}::PATCHED_PRIVATE_${randomBytes(16).toString("hex")}`,
  );
  if (
    [approvalPublicNeedle, approvalPrivateContent, approvalPatchedContent]
      .some((value) => /[\r\n]/u.test(value))
    || !approvalPrivateContent.includes(approvalPublicNeedle)
    || !approvalPatchedContent.includes(approvalPublicNeedle)
    || approvalPrivateContent === approvalPatchedContent
  ) {
    throw new Error(
      "The Files E2E needle and original/patched content must be distinct single-line values.",
    );
  }
  const clarificationProfileId = environment(
    "SYNTHCHAT_E2E_CLARIFICATION_PROFILE_ID",
    `e2e_clarification_${runId}`,
  );
  const clarificationProfileName = environment(
    "SYNTHCHAT_E2E_CLARIFICATION_PROFILE_NAME",
    `E2E Clarification ${runId}`,
  );
  const clarificationPrompt = environment(
    "SYNTHCHAT_E2E_CLARIFICATION_PROMPT",
    `E2E clarification prompt ${runId}`,
  );
  const clarificationReply = environment(
    "SYNTHCHAT_E2E_CLARIFICATION_REPLY",
    `Clarification completed ${runId}`,
  );
  const clarificationCallId = environment(
    "SYNTHCHAT_E2E_CLARIFICATION_CALL_ID",
    `call-e2e-clarification-${runId}`,
  );
  const clarificationQuestion = environment(
    "SYNTHCHAT_E2E_CLARIFICATION_QUESTION",
    `Which private deployment target should be used for ${runId}?`,
  );
  const clarificationAnswer = environment(
    "SYNTHCHAT_E2E_CLARIFICATION_ANSWER",
    `PRIVATE_CLARIFICATION_ANSWER_${randomBytes(16).toString("hex")}`,
  );
  const mcpProfileId = environment(
    "SYNTHCHAT_E2E_MCP_PROFILE_ID",
    `e2e_mcp_${runId}`,
  );
  const mcpProfileName = environment(
    "SYNTHCHAT_E2E_MCP_PROFILE_NAME",
    `E2E MCP ${runId}`,
  );
  const mcpServerName = environment("SYNTHCHAT_E2E_MCP_SERVER_NAME", `fixture_${runId}`);
  const mcpToolName = environment(
    "SYNTHCHAT_E2E_MCP_TOOL_NAME",
    `mcp__${mcpServerName}__echo`,
  );
  const mcpPrompt = environment(
    "SYNTHCHAT_E2E_MCP_PROMPT",
    `E2E MCP prompt ${runId}`,
  );
  const mcpReply = environment(
    "SYNTHCHAT_E2E_MCP_REPLY",
    `MCP completed ${runId}`,
  );
  const mcpCallId = environment(
    "SYNTHCHAT_E2E_MCP_CALL_ID",
    `call-e2e-mcp-${runId}`,
  );
  const mcpPrivateResult = environment(
    "SYNTHCHAT_E2E_MCP_PRIVATE_RESULT",
    "MCP_E2E_PRIVATE_RESULT_DO_NOT_EXPOSE",
  );
  const terminalProfileId = environment(
    "SYNTHCHAT_E2E_TERMINAL_PROFILE_ID",
    `e2e_terminal_${runId}`,
  );
  const terminalProfileName = environment(
    "SYNTHCHAT_E2E_TERMINAL_PROFILE_NAME",
    `E2E Terminal ${runId}`,
  );
  const terminalPrompt = environment(
    "SYNTHCHAT_E2E_TERMINAL_PROMPT",
    `E2E terminal prompt ${runId}`,
  );
  const terminalReply = environment(
    "SYNTHCHAT_E2E_TERMINAL_REPLY",
    `Terminal completed ${runId}`,
  );
  const terminalCallId = environment(
    "SYNTHCHAT_E2E_TERMINAL_CALL_ID",
    `call-e2e-terminal-${runId}`,
  );
  const terminalRelativePath = environment(
    "SYNTHCHAT_E2E_TERMINAL_RELATIVE_PATH",
    "generated/terminal-e2e.txt",
  );
  const terminalPrivateOutput = environment(
    "SYNTHCHAT_E2E_TERMINAL_PRIVATE_OUTPUT",
    "TERMINAL_E2E_PRIVATE_STDOUT_DO_NOT_EXPOSE",
  );
  const terminalPrivateFile = environment(
    "SYNTHCHAT_E2E_TERMINAL_PRIVATE_FILE",
    "TERMINAL_E2E_PRIVATE_FILE_DO_NOT_EXPOSE",
  );
  const backgroundTerminalProfileId = environment(
    "SYNTHCHAT_E2E_BACKGROUND_TERMINAL_PROFILE_ID",
    `e2e_background_terminal_${runId}`,
  );
  const backgroundTerminalProfileName = environment(
    "SYNTHCHAT_E2E_BACKGROUND_TERMINAL_PROFILE_NAME",
    `E2E Background Terminal ${runId}`,
  );
  const backgroundTerminalPrompt = environment(
    "SYNTHCHAT_E2E_BACKGROUND_TERMINAL_PROMPT",
    `E2E background terminal launch ${runId}`,
  );
  const backgroundTerminalReply = environment(
    "SYNTHCHAT_E2E_BACKGROUND_TERMINAL_REPLY",
    `Background terminal started ${runId}`,
  );
  const backgroundProcessPrompt = environment(
    "SYNTHCHAT_E2E_BACKGROUND_PROCESS_PROMPT",
    `E2E background process stop ${runId}`,
  );
  const backgroundProcessReply = environment(
    "SYNTHCHAT_E2E_BACKGROUND_PROCESS_REPLY",
    `Background terminal stopped ${runId}`,
  );
  const backgroundTerminalCallId = environment(
    "SYNTHCHAT_E2E_BACKGROUND_TERMINAL_CALL_ID",
    `call-e2e-background-terminal-${runId}`,
  );
  const backgroundProcessListCallId = environment(
    "SYNTHCHAT_E2E_BACKGROUND_PROCESS_LIST_CALL_ID",
    `call-e2e-background-process-list-${runId}`,
  );
  const backgroundProcessKillCallId = environment(
    "SYNTHCHAT_E2E_BACKGROUND_PROCESS_KILL_CALL_ID",
    `call-e2e-background-process-kill-${runId}`,
  );
  const backgroundTerminalPrivateOutput = environment(
    "SYNTHCHAT_E2E_BACKGROUND_TERMINAL_PRIVATE_OUTPUT",
    `TERMINAL_E2E_BACKGROUND_PRIVATE_STDOUT_${randomBytes(16).toString("hex")}`,
  );
  const backgroundTerminalRelativePath = portableRelativeFilePath(
    "SYNTHCHAT_E2E_BACKGROUND_TERMINAL_RELATIVE_PATH",
    "generated/background-terminal-started.txt",
  );
  const codeProfileId = environment(
    "SYNTHCHAT_E2E_CODE_PROFILE_ID",
    `e2e_code_${runId}`,
  );
  const codeProfileName = environment(
    "SYNTHCHAT_E2E_CODE_PROFILE_NAME",
    `E2E Code ${runId}`,
  );
  const codePrompt = environment(
    "SYNTHCHAT_E2E_CODE_PROMPT",
    `E2E execute code prompt ${runId}`,
  );
  const codeReply = environment(
    "SYNTHCHAT_E2E_CODE_REPLY",
    `Code execution completed ${runId}`,
  );
  const codeCallId = environment(
    "SYNTHCHAT_E2E_CODE_CALL_ID",
    `call-e2e-code-${runId}`,
  );
  const codePrivateOutput = environment(
    "SYNTHCHAT_E2E_CODE_PRIVATE_OUTPUT",
    "CODE_E2E_PRIVATE_STDOUT_DO_NOT_EXPOSE",
  );
  const codePrivateFile = environment(
    "SYNTHCHAT_E2E_CODE_PRIVATE_FILE",
    "CODE_E2E_PRIVATE_FILE_DO_NOT_EXPOSE",
  );
  const browserProfileId = environment(
    "SYNTHCHAT_E2E_BROWSER_PROFILE_ID",
    `e2e_browser_${runId}`,
  );
  const browserProfileName = environment(
    "SYNTHCHAT_E2E_BROWSER_PROFILE_NAME",
    `E2E Browser ${runId}`,
  );
  const browserPrompt = environment(
    "SYNTHCHAT_E2E_BROWSER_PROMPT",
    `E2E Browser prompt ${runId}`,
  );
  const browserReply = environment(
    "SYNTHCHAT_E2E_BROWSER_REPLY",
    `Browser completed ${runId}`,
  );
  const browserNavigateCallId = environment(
    "SYNTHCHAT_E2E_BROWSER_NAVIGATE_CALL_ID",
    `call-e2e-browser-navigate-${runId}`,
  );
  const browserSnapshotCallId = environment(
    "SYNTHCHAT_E2E_BROWSER_SNAPSHOT_CALL_ID",
    `call-e2e-browser-snapshot-${runId}`,
  );
  const browserCdpCallId = environment(
    "SYNTHCHAT_E2E_BROWSER_CDP_CALL_ID",
    `call-e2e-browser-cdp-${runId}`,
  );
  const browserPostCdpSnapshotCallId = environment(
    "SYNTHCHAT_E2E_BROWSER_POST_CDP_SNAPSHOT_CALL_ID",
    `call-e2e-browser-post-cdp-snapshot-${runId}`,
  );
  const browserDownloadCallId = environment(
    "SYNTHCHAT_E2E_BROWSER_DOWNLOAD_CALL_ID",
    `call-e2e-browser-download-${runId}`,
  );
  const browserDownloadSelector = environment(
    "SYNTHCHAT_E2E_BROWSER_DOWNLOAD_SELECTOR",
    `#synthchat-e2e-download-${runId}`,
  );
  const browserDownloadFilename = environment(
    "SYNTHCHAT_E2E_BROWSER_DOWNLOAD_FILENAME",
    `synthchat-e2e-${runId}.txt`,
  );
  const browserDownloadPrivateContent = environment(
    "SYNTHCHAT_E2E_BROWSER_DOWNLOAD_PRIVATE_CONTENT",
    `BROWSER_E2E_PRIVATE_DOWNLOAD_${runId}_DO_NOT_EXPOSE`,
  );
  const browserDownloadSizeBytes = Buffer.byteLength(browserDownloadPrivateContent, "utf8");
  const browserDownloadSha256 = createHash("sha256")
    .update(browserDownloadPrivateContent, "utf8")
    .digest("hex");
  const browserUrl = environment("SYNTHCHAT_E2E_BROWSER_URL", "https://example.com/");
  const browserExpectedTitle = environment(
    "SYNTHCHAT_E2E_BROWSER_EXPECTED_TITLE",
    "Example Domain",
  );
  const historyMemoryProfileId = environment(
    "SYNTHCHAT_E2E_HISTORY_MEMORY_PROFILE_ID",
    `e2e_history_memory_${runId}`,
  );
  const historyMemoryProfileName = environment(
    "SYNTHCHAT_E2E_HISTORY_MEMORY_PROFILE_NAME",
    `E2E History Memory ${runId}`,
  );
  const historySearchTerm = environment(
    "SYNTHCHAT_E2E_HISTORY_SEARCH_TERM",
    `historyterm${runId}`,
  );
  const historyPrompt = environment(
    "SYNTHCHAT_E2E_HISTORY_PROMPT",
    `Persist this searchable message ${historySearchTerm}`,
  );
  const historyContinuationPrompt = environment(
    "SYNTHCHAT_E2E_HISTORY_CONTINUATION_PROMPT",
    `Continue the recovered Session ${runId}`,
  );
  const memorySearchTerm = environment(
    "SYNTHCHAT_E2E_MEMORY_SEARCH_TERM",
    `memoryterm${runId}`,
  );
  const memoryContent = environment(
    "SYNTHCHAT_E2E_MEMORY_CONTENT",
    `Builtin Memory original ${memorySearchTerm}`,
  );
  const memoryUpdatedContent = environment(
    "SYNTHCHAT_E2E_MEMORY_UPDATED_CONTENT",
    `Builtin Memory updated ${memorySearchTerm}`,
  );
  const model = environment("SYNTHCHAT_E2E_MODEL", `e2e-model-${runId}`);
  const prompt = environment("SYNTHCHAT_E2E_PROMPT", `E2E prompt ${runId}`);
  const reply = environment("SYNTHCHAT_E2E_REPLY", `Hermes E2E reply ${runId}`);
  const promptTokens = positiveInteger("SYNTHCHAT_E2E_PROMPT_TOKENS", 7);
  const completionTokens = positiveInteger("SYNTHCHAT_E2E_COMPLETION_TOKENS", 5);
  const totalTokens = positiveInteger(
    "SYNTHCHAT_E2E_TOTAL_TOKENS",
    promptTokens + completionTokens,
  );
  const skillFixture = configuredPath(
    "SYNTHCHAT_E2E_SKILL_FIXTURE",
    "tests/e2e/fixtures/auditable-skill.md",
  );
  await access(skillFixture);
  const mcpFixtureSource = configuredPath(
    "SYNTHCHAT_E2E_MCP_FIXTURE_SOURCE",
    "scripts/e2e/mcp-stdio-fixture.rs",
  );
  await access(mcpFixtureSource);
  const terminalFixtureSource = configuredPath(
    "SYNTHCHAT_E2E_TERMINAL_FIXTURE_SOURCE",
    "scripts/e2e/terminal-fixture.rs",
  );
  await access(terminalFixtureSource);
  const browserExecutable = configuredPath(
    "SYNTHCHAT_E2E_BROWSER_BINARY",
    require("playwright").chromium.executablePath(),
  );
  await access(browserExecutable);
  const tempBase = configuredPath("SYNTHCHAT_E2E_TEMP_BASE", tmpdir());
  await mkdir(tempBase, { recursive: true });
  runtimeRoot = await mkdtemp(join(tempBase, "synthchat-hermes-e2e-"));
  const hermesHome = join(runtimeRoot, "hermes-home");
  await mkdir(hermesHome, { recursive: true });
  const approvalWorkspace = join(runtimeRoot, "approval-workspace");
  await mkdir(approvalWorkspace, { recursive: true });
  const terminalWorkspace = join(runtimeRoot, "terminal-workspace");
  await mkdir(terminalWorkspace, { recursive: true });
  const backgroundTerminalWorkspace = join(runtimeRoot, "background-terminal-workspace");
  await mkdir(backgroundTerminalWorkspace, { recursive: true });
  const codeTarget = join(runtimeRoot, "code-side-effect.txt");
  const codeSource = [
    "from pathlib import Path",
    `Path(${JSON.stringify(codeTarget)}).write_text(${JSON.stringify(codePrivateFile)}, encoding=\"utf-8\")`,
    `print(${JSON.stringify(codePrivateOutput)})`,
    "",
  ].join("\n");
  const mcpFixture = join(
    runtimeRoot,
    process.platform === "win32" ? "mcp-stdio-fixture.exe" : "mcp-stdio-fixture",
  );
  const rustcExecutable = environment(
    "SYNTHCHAT_E2E_RUSTC",
    process.platform === "win32" ? "rustc.exe" : "rustc",
  );
  await runCommand(
    "MCP stdio fixture build",
    rustcExecutable,
    [mcpFixtureSource, "-o", mcpFixture],
    { timeoutMs: positiveInteger("SYNTHCHAT_E2E_MCP_BUILD_TIMEOUT_MS", 120_000) },
  );
  await access(mcpFixture);
  const terminalFixture = join(
    runtimeRoot,
    process.platform === "win32" ? "terminal-fixture.exe" : "terminal-fixture",
  );
  await runCommand(
    "terminal fixture build",
    rustcExecutable,
    [terminalFixtureSource, "-o", terminalFixture],
    {
      env: {
        ...process.env,
        SYNTHCHAT_E2E_BACKGROUND_TERMINAL_PRIVATE_OUTPUT: backgroundTerminalPrivateOutput,
      },
      timeoutMs: positiveInteger("SYNTHCHAT_E2E_TERMINAL_BUILD_TIMEOUT_MS", 120_000),
    },
  );
  await access(terminalFixture);
  const terminalCommand = `"${terminalFixture.replaceAll("\\", "/")}"`;
  const backgroundTerminalCommand = `${terminalCommand} background`;

  const providerScript = configuredPath(
    "SYNTHCHAT_E2E_PROVIDER_SCRIPT",
    "scripts/e2e/mock-openai-provider.mjs",
  );
  const providerControlCapability = randomBytes(32).toString("hex");
  const provider = spawnProcess(
    "mock provider",
    process.execPath,
    [providerScript],
    {
      captureStdout: true,
      stdio: ["pipe", "pipe", "pipe"],
      env: {
        ...process.env,
        SYNTHCHAT_E2E_APPROVAL_CALL_ID: approvalCallId,
        SYNTHCHAT_E2E_APPROVAL_PATCH_CALL_ID: approvalPatchCallId,
        SYNTHCHAT_E2E_APPROVAL_PATCHED_CONTENT: approvalPatchedContent,
        SYNTHCHAT_E2E_APPROVAL_PRIVATE_CONTENT: approvalPrivateContent,
        SYNTHCHAT_E2E_APPROVAL_PROMPT: approvalPrompt,
        SYNTHCHAT_E2E_APPROVAL_PUBLIC_NEEDLE: approvalPublicNeedle,
        SYNTHCHAT_E2E_APPROVAL_READ_CALL_ID: approvalReadCallId,
        SYNTHCHAT_E2E_APPROVAL_RELATIVE_PATH: approvalRelativePath,
        SYNTHCHAT_E2E_APPROVAL_REPLY: approvalReply,
        SYNTHCHAT_E2E_APPROVAL_SEARCH_CALL_ID: approvalSearchCallId,
        SYNTHCHAT_E2E_BACKGROUND_PROCESS_LIST_CALL_ID: backgroundProcessListCallId,
        SYNTHCHAT_E2E_BACKGROUND_PROCESS_KILL_CALL_ID: backgroundProcessKillCallId,
        SYNTHCHAT_E2E_BACKGROUND_PROCESS_PROMPT: backgroundProcessPrompt,
        SYNTHCHAT_E2E_BACKGROUND_PROCESS_REPLY: backgroundProcessReply,
        SYNTHCHAT_E2E_BACKGROUND_TERMINAL_CALL_ID: backgroundTerminalCallId,
        SYNTHCHAT_E2E_BACKGROUND_TERMINAL_COMMAND: backgroundTerminalCommand,
        SYNTHCHAT_E2E_BACKGROUND_TERMINAL_PRIVATE_OUTPUT: backgroundTerminalPrivateOutput,
        SYNTHCHAT_E2E_BACKGROUND_TERMINAL_PROMPT: backgroundTerminalPrompt,
        SYNTHCHAT_E2E_BACKGROUND_TERMINAL_REPLY: backgroundTerminalReply,
        SYNTHCHAT_E2E_BROWSER_CDP_CALL_ID: browserCdpCallId,
        SYNTHCHAT_E2E_BROWSER_DOWNLOAD_CALL_ID: browserDownloadCallId,
        SYNTHCHAT_E2E_BROWSER_DOWNLOAD_FILENAME: browserDownloadFilename,
        SYNTHCHAT_E2E_BROWSER_DOWNLOAD_PRIVATE_CONTENT: browserDownloadPrivateContent,
        SYNTHCHAT_E2E_BROWSER_DOWNLOAD_SELECTOR: browserDownloadSelector,
        SYNTHCHAT_E2E_BROWSER_EXPECTED_TITLE: browserExpectedTitle,
        SYNTHCHAT_E2E_BROWSER_NAVIGATE_CALL_ID: browserNavigateCallId,
        SYNTHCHAT_E2E_BROWSER_POST_CDP_SNAPSHOT_CALL_ID: browserPostCdpSnapshotCallId,
        SYNTHCHAT_E2E_BROWSER_PROMPT: browserPrompt,
        SYNTHCHAT_E2E_BROWSER_REPLY: browserReply,
        SYNTHCHAT_E2E_BROWSER_SNAPSHOT_CALL_ID: browserSnapshotCallId,
        SYNTHCHAT_E2E_BROWSER_URL: browserUrl,
        SYNTHCHAT_E2E_CLARIFICATION_ANSWER: clarificationAnswer,
        SYNTHCHAT_E2E_CLARIFICATION_CALL_ID: clarificationCallId,
        SYNTHCHAT_E2E_CLARIFICATION_PROMPT: clarificationPrompt,
        SYNTHCHAT_E2E_CLARIFICATION_QUESTION: clarificationQuestion,
        SYNTHCHAT_E2E_CLARIFICATION_REPLY: clarificationReply,
        SYNTHCHAT_E2E_CODE_CALL_ID: codeCallId,
        SYNTHCHAT_E2E_CODE_PRIVATE_OUTPUT: codePrivateOutput,
        SYNTHCHAT_E2E_CODE_PROMPT: codePrompt,
        SYNTHCHAT_E2E_CODE_REPLY: codeReply,
        SYNTHCHAT_E2E_CODE_SOURCE: codeSource,
        SYNTHCHAT_E2E_COMPLETION_TOKENS: String(completionTokens),
        SYNTHCHAT_E2E_MCP_CALL_ID: mcpCallId,
        SYNTHCHAT_E2E_MCP_PRIVATE_RESULT: mcpPrivateResult,
        SYNTHCHAT_E2E_MCP_PROMPT: mcpPrompt,
        SYNTHCHAT_E2E_MCP_REPLY: mcpReply,
        SYNTHCHAT_E2E_MCP_TOOL_NAME: mcpToolName,
        SYNTHCHAT_E2E_PROVIDER_CONTROL_CAPABILITY: providerControlCapability,
        SYNTHCHAT_E2E_PROVIDER_CONTROL_PORT: "0",
        SYNTHCHAT_E2E_PROVIDER_HOST: host,
        SYNTHCHAT_E2E_PROVIDER_PORT: "0",
        SYNTHCHAT_E2E_PROMPT_TOKENS: String(promptTokens),
        SYNTHCHAT_E2E_REPLY: reply,
        SYNTHCHAT_E2E_TERMINAL_CALL_ID: terminalCallId,
        SYNTHCHAT_E2E_TERMINAL_COMMAND: terminalCommand,
        SYNTHCHAT_E2E_TERMINAL_PRIVATE_OUTPUT: terminalPrivateOutput,
        SYNTHCHAT_E2E_TERMINAL_PROMPT: terminalPrompt,
        SYNTHCHAT_E2E_TERMINAL_REPLY: terminalReply,
        SYNTHCHAT_E2E_TOTAL_TOKENS: String(totalTokens),
      },
    },
  );
  const providerReady = await waitForJsonReady(provider, "provider", startupTimeoutMs);
  const providerBaseUrl = providerReady.baseUrl;
  if (typeof providerReady.controlUrl !== "string") {
    throw new Error("Mock provider readiness did not include its Node control URL.");
  }
  const providerControlUrl = providerReady.controlUrl;

  const cargoExecutable = environment(
    "SYNTHCHAT_E2E_CARGO",
    process.platform === "win32" ? "cargo.exe" : "cargo",
  );
  const rustToolchain = environment(
    "SYNTHCHAT_E2E_RUST_TOOLCHAIN",
    defaultRustToolchain(),
  );
  if (!booleanEnvironment("SYNTHCHAT_E2E_SKIP_BACKEND_BUILD", false)) {
    await runCommand(
      "backend build",
      cargoExecutable,
      [
        `+${rustToolchain}`,
        "build",
        "--locked",
        "--manifest-path",
        backendManifest,
        "--bin",
        "synthchat-hermes-backend",
      ],
      { timeoutMs: positiveInteger("SYNTHCHAT_E2E_BUILD_TIMEOUT_MS", 300_000) },
    );
  }
  const targetRoot = configuredPath(
    "SYNTHCHAT_E2E_CARGO_TARGET_DIR",
    process.env.CARGO_TARGET_DIR || "backend/target",
  );
  const backendBinary = configuredPath(
    "SYNTHCHAT_E2E_BACKEND_BIN",
    join(
      targetRoot,
      "debug",
      process.platform === "win32" ? "synthchat-hermes-backend.exe" : "synthchat-hermes-backend",
    ),
  );
  await access(backendBinary);

  const relayIngressToken = randomBytes(32).toString("hex");
  const relay = createBackendRelay(relayIngressToken);
  const relayOrigin = await listenServer("backend relay", relay.server, host);

  const viteServerScript = configuredPath(
    "SYNTHCHAT_E2E_VITE_SERVER_SCRIPT",
    "scripts/e2e/vite-server.mjs",
  );
  const frontend = spawnProcess(
    "frontend",
    process.execPath,
    [viteServerScript],
    {
      captureStdout: true,
      cwd: frontendRoot,
      stdio: ["pipe", "pipe", "pipe"],
      env: {
        ...process.env,
        SYNTHCHAT_E2E_BACKEND_TOKEN: relayIngressToken,
        SYNTHCHAT_E2E_BACKEND_URL: relayOrigin,
        SYNTHCHAT_E2E_HOST: host,
        SYNTHCHAT_FRONTEND_HOST: host,
      },
    },
  );
  const frontendReady = await waitForJsonReady(frontend, "frontend", startupTimeoutMs);
  const frontendOrigin = frontendReady.baseUrl;
  await waitForHttp(frontendOrigin, startupTimeoutMs, pollIntervalMs);
  await assertSecretIsServerSide(frontendOrigin, relayIngressToken);
  await assertSecretIsServerSide(frontendOrigin, providerControlCapability);

  const supervisor = createBackendSupervisor({
    allowedOrigin: frontendOrigin,
    backendEnvironment: { SYNTHCHAT_BROWSER_BINARY: browserExecutable },
    backendBinary,
    hermesHome,
    host,
    onStarted: async (backend) => assertSecretIsServerSide(frontendOrigin, backend.token),
    pollIntervalMs,
    relay,
    startupTimeoutMs,
  });
  activeBackendSupervisor = supervisor;
  await supervisor.start();
  await waitForHttp(`${frontendOrigin}/health`, startupTimeoutMs, pollIntervalMs);

  const controlCapability = randomBytes(32).toString("hex");
  const controlServer = createControlServer(supervisor, controlCapability);
  const controlOrigin = await listenServer("backend control", controlServer, host);
  await assertSecretIsServerSide(frontendOrigin, controlCapability);

  const playwrightPackage = dirname(require.resolve("@playwright/test/package.json"));
  const playwrightCli = join(playwrightPackage, "cli.js");
  const playwrightGlobalTimeoutMs = positiveInteger(
    "SYNTHCHAT_E2E_GLOBAL_TIMEOUT_MS",
    1_200_000,
  );
  await runCommand(
    "Playwright",
    process.execPath,
    [
      playwrightCli,
      "test",
      "--config",
      join(repositoryRoot, "playwright.config.ts"),
      "--project=chromium",
      ...process.argv.slice(2),
    ],
    {
      env: {
        ...process.env,
        SYNTHCHAT_E2E_APPROVAL_PRIVATE_CONTENT: approvalPrivateContent,
        SYNTHCHAT_E2E_APPROVAL_PATCHED_CONTENT: approvalPatchedContent,
        SYNTHCHAT_E2E_APPROVAL_PROFILE_ID: approvalProfileId,
        SYNTHCHAT_E2E_APPROVAL_PROFILE_NAME: approvalProfileName,
        SYNTHCHAT_E2E_APPROVAL_PROMPT: approvalPrompt,
        SYNTHCHAT_E2E_APPROVAL_PUBLIC_NEEDLE: approvalPublicNeedle,
        SYNTHCHAT_E2E_APPROVAL_RELATIVE_PATH: approvalRelativePath,
        SYNTHCHAT_E2E_APPROVAL_REPLY: approvalReply,
        SYNTHCHAT_E2E_APPROVAL_WORKSPACE: approvalWorkspace,
        SYNTHCHAT_E2E_BACKGROUND_PROCESS_LIST_CALL_ID: backgroundProcessListCallId,
        SYNTHCHAT_E2E_BACKGROUND_PROCESS_KILL_CALL_ID: backgroundProcessKillCallId,
        SYNTHCHAT_E2E_BACKGROUND_PROCESS_PROMPT: backgroundProcessPrompt,
        SYNTHCHAT_E2E_BACKGROUND_PROCESS_REPLY: backgroundProcessReply,
        SYNTHCHAT_E2E_BACKGROUND_TERMINAL_CALL_ID: backgroundTerminalCallId,
        SYNTHCHAT_E2E_BACKGROUND_TERMINAL_COMMAND: backgroundTerminalCommand,
        SYNTHCHAT_E2E_BACKGROUND_TERMINAL_PRIVATE_OUTPUT: backgroundTerminalPrivateOutput,
        SYNTHCHAT_E2E_BACKGROUND_TERMINAL_PROFILE_ID: backgroundTerminalProfileId,
        SYNTHCHAT_E2E_BACKGROUND_TERMINAL_PROFILE_NAME: backgroundTerminalProfileName,
        SYNTHCHAT_E2E_BACKGROUND_TERMINAL_PROMPT: backgroundTerminalPrompt,
        SYNTHCHAT_E2E_BACKGROUND_TERMINAL_RELATIVE_PATH: backgroundTerminalRelativePath,
        SYNTHCHAT_E2E_BACKGROUND_TERMINAL_REPLY: backgroundTerminalReply,
        SYNTHCHAT_E2E_BACKGROUND_TERMINAL_WORKSPACE: backgroundTerminalWorkspace,
        SYNTHCHAT_E2E_BASE_URL: frontendOrigin,
        SYNTHCHAT_E2E_BROWSER_DOWNLOAD_FILENAME: browserDownloadFilename,
        SYNTHCHAT_E2E_BROWSER_DOWNLOAD_PRIVATE_CONTENT: browserDownloadPrivateContent,
        SYNTHCHAT_E2E_BROWSER_DOWNLOAD_SELECTOR: browserDownloadSelector,
        SYNTHCHAT_E2E_BROWSER_DOWNLOAD_SHA256: browserDownloadSha256,
        SYNTHCHAT_E2E_BROWSER_DOWNLOAD_SIZE_BYTES: String(browserDownloadSizeBytes),
        SYNTHCHAT_E2E_BROWSER_EXPECTED_TITLE: browserExpectedTitle,
        SYNTHCHAT_E2E_BROWSER_PROFILE_ID: browserProfileId,
        SYNTHCHAT_E2E_BROWSER_PROFILE_NAME: browserProfileName,
        SYNTHCHAT_E2E_BROWSER_PROMPT: browserPrompt,
        SYNTHCHAT_E2E_BROWSER_REPLY: browserReply,
        SYNTHCHAT_E2E_BROWSER_URL: browserUrl,
        SYNTHCHAT_E2E_CLARIFICATION_ANSWER: clarificationAnswer,
        SYNTHCHAT_E2E_CLARIFICATION_PROFILE_ID: clarificationProfileId,
        SYNTHCHAT_E2E_CLARIFICATION_PROFILE_NAME: clarificationProfileName,
        SYNTHCHAT_E2E_CLARIFICATION_PROMPT: clarificationPrompt,
        SYNTHCHAT_E2E_CLARIFICATION_QUESTION: clarificationQuestion,
        SYNTHCHAT_E2E_CLARIFICATION_REPLY: clarificationReply,
        SYNTHCHAT_E2E_CODE_PRIVATE_FILE: codePrivateFile,
        SYNTHCHAT_E2E_CODE_PRIVATE_OUTPUT: codePrivateOutput,
        SYNTHCHAT_E2E_CODE_PROFILE_ID: codeProfileId,
        SYNTHCHAT_E2E_CODE_PROFILE_NAME: codeProfileName,
        SYNTHCHAT_E2E_CODE_PROMPT: codePrompt,
        SYNTHCHAT_E2E_CODE_REPLY: codeReply,
        SYNTHCHAT_E2E_CODE_TARGET: codeTarget,
        SYNTHCHAT_E2E_CONTROL_CAPABILITY: controlCapability,
        SYNTHCHAT_E2E_CONTROL_URL: controlOrigin,
        SYNTHCHAT_E2E_HISTORY_CONTINUATION_PROMPT: historyContinuationPrompt,
        SYNTHCHAT_E2E_HISTORY_MEMORY_PROFILE_ID: historyMemoryProfileId,
        SYNTHCHAT_E2E_HISTORY_MEMORY_PROFILE_NAME: historyMemoryProfileName,
        SYNTHCHAT_E2E_HISTORY_PROMPT: historyPrompt,
        SYNTHCHAT_E2E_HISTORY_SEARCH_TERM: historySearchTerm,
        SYNTHCHAT_E2E_INFLIGHT_PROFILE_ID: inflightProfileId,
        SYNTHCHAT_E2E_INFLIGHT_PROFILE_NAME: inflightProfileName,
        SYNTHCHAT_E2E_INFLIGHT_PROMPT: inflightPrompt,
        SYNTHCHAT_E2E_INFLIGHT_RECOVERY_PROMPT: inflightRecoveryPrompt,
        SYNTHCHAT_E2E_MODEL: model,
        SYNTHCHAT_E2E_MEMORY_CONTENT: memoryContent,
        SYNTHCHAT_E2E_MEMORY_SEARCH_TERM: memorySearchTerm,
        SYNTHCHAT_E2E_MEMORY_UPDATED_CONTENT: memoryUpdatedContent,
        SYNTHCHAT_E2E_MCP_COMMAND: mcpFixture,
        SYNTHCHAT_E2E_MCP_PRIVATE_RESULT: mcpPrivateResult,
        SYNTHCHAT_E2E_MCP_PROFILE_ID: mcpProfileId,
        SYNTHCHAT_E2E_MCP_PROFILE_NAME: mcpProfileName,
        SYNTHCHAT_E2E_MCP_PROMPT: mcpPrompt,
        SYNTHCHAT_E2E_MCP_REPLY: mcpReply,
        SYNTHCHAT_E2E_MCP_SERVER_NAME: mcpServerName,
        SYNTHCHAT_E2E_MCP_TOOL_NAME: mcpToolName,
        SYNTHCHAT_E2E_PROFILE_ID: profileId,
        SYNTHCHAT_E2E_PROFILE_NAME: profileName,
        SYNTHCHAT_E2E_PROMPT: prompt,
        SYNTHCHAT_E2E_PROVIDER_BASE_URL: providerBaseUrl,
        SYNTHCHAT_E2E_PROVIDER_CONTROL_CAPABILITY: providerControlCapability,
        SYNTHCHAT_E2E_PROVIDER_CONTROL_URL: providerControlUrl,
        SYNTHCHAT_E2E_RECOVERY_PROFILE_ID: recoveryProfileId,
        SYNTHCHAT_E2E_RECOVERY_PROFILE_NAME: recoveryProfileName,
        SYNTHCHAT_E2E_RECOVERY_PROMPT: recoveryPrompt,
        SYNTHCHAT_E2E_REPLY: reply,
        SYNTHCHAT_E2E_SKILL_FIXTURE: skillFixture,
        SYNTHCHAT_E2E_TOOLS_PROFILE_ID: toolsProfileId,
        SYNTHCHAT_E2E_TOOLS_PROFILE_NAME: toolsProfileName,
        SYNTHCHAT_E2E_TERMINAL_COMMAND: terminalCommand,
        SYNTHCHAT_E2E_TERMINAL_PRIVATE_FILE: terminalPrivateFile,
        SYNTHCHAT_E2E_TERMINAL_PRIVATE_OUTPUT: terminalPrivateOutput,
        SYNTHCHAT_E2E_TERMINAL_PROFILE_ID: terminalProfileId,
        SYNTHCHAT_E2E_TERMINAL_PROFILE_NAME: terminalProfileName,
        SYNTHCHAT_E2E_TERMINAL_PROMPT: terminalPrompt,
        SYNTHCHAT_E2E_TERMINAL_RELATIVE_PATH: terminalRelativePath,
        SYNTHCHAT_E2E_TERMINAL_REPLY: terminalReply,
        SYNTHCHAT_E2E_TERMINAL_WORKSPACE: terminalWorkspace,
        SYNTHCHAT_E2E_TOTAL_TOKENS: String(totalTokens),
        SYNTHCHAT_E2E_GLOBAL_TIMEOUT_MS: String(playwrightGlobalTimeoutMs),
        SYNTHCHAT_E2E_WORKERS: environment("SYNTHCHAT_E2E_WORKERS", "1"),
      },
      timeoutMs: positiveInteger(
        "SYNTHCHAT_E2E_PLAYWRIGHT_TIMEOUT_MS",
        playwrightGlobalTimeoutMs + 30_000,
      ),
    },
  );
}

let mainError;
try {
  await main();
} catch (error) {
  mainError = error;
}
let cleanupError;
try {
  await cleanup();
} catch (error) {
  cleanupError = error;
}
if (mainError && cleanupError) {
  throw new AggregateError([mainError, cleanupError], "Local E2E execution and cleanup failed.");
}
if (mainError) {
  throw mainError;
}
if (cleanupError) {
  throw cleanupError;
}
