import { createRequire } from "node:module";
import { isIP } from "node:net";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = resolve(scriptDirectory, "../..");
const frontendRoot = configuredPath("SYNTHCHAT_E2E_FRONTEND_ROOT", "frontend");

function environment(name, fallback) {
  const value = process.env[name]?.trim();
  return value || fallback;
}

function configuredPath(name, fallback) {
  const value = environment(name, fallback);
  return isAbsolute(value) ? value : resolve(repositoryRoot, value);
}

function loopbackHost() {
  const host = environment("SYNTHCHAT_E2E_HOST", "127.0.0.1");
  if (isIP(host) === 0 || !["127.0.0.1", "::1"].includes(host)) {
    throw new Error("SYNTHCHAT_E2E_HOST must be a loopback IP address.");
  }
  return host;
}

function origin(host, port) {
  return `http://${host.includes(":") ? `[${host}]` : host}:${port}`;
}

const require = createRequire(import.meta.url);
const viteEntry = require.resolve("vite", { paths: [frontendRoot] });
const { createServer } = await import(pathToFileURL(viteEntry).href);
const host = loopbackHost();
const configFile = configuredPath(
  "SYNTHCHAT_E2E_VITE_CONFIG",
  join("frontend", "vite.config.ts"),
);
const server = await createServer({
  configFile,
  mode: "e2e",
  root: frontendRoot,
  server: {
    host,
    port: 0,
    strictPort: true,
  },
});

await server.listen();
const address = server.httpServer?.address();
if (!address || typeof address === "string") {
  await server.close();
  throw new Error("Vite did not expose its bound loopback address.");
}

process.stdout.write(`${JSON.stringify({
  baseUrl: origin(host, address.port),
  event: "ready",
})}\n`);

let closing = false;
async function shutdown(exitCode) {
  if (closing) return;
  closing = true;
  try {
    await server.close();
  } finally {
    process.exit(exitCode);
  }
}

process.once("SIGINT", () => void shutdown(0));
process.once("SIGTERM", () => void shutdown(0));
if (!process.stdin.isTTY) {
  process.stdin.resume();
  process.stdin.once("end", () => void shutdown(0));
}
