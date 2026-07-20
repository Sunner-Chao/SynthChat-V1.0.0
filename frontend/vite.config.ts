import { defineConfig, loadEnv, type ProxyOptions } from "vite";
import react from "@vitejs/plugin-react";

const LOOPBACK_HOSTS = new Set(["127.0.0.1", "localhost", "::1"]);

function configuredHost(value: string | undefined): string {
  const host = value?.trim() || "127.0.0.1";
  if (!LOOPBACK_HOSTS.has(host)) {
    throw new Error("SYNTHCHAT_FRONTEND_HOST must use a loopback hostname.");
  }
  return host;
}

function configuredPort(value: string | undefined): number {
  const port = Number(value?.trim() || "1421");
  if (!Number.isSafeInteger(port) || port <= 0 || port > 65_535) {
    throw new Error("SYNTHCHAT_FRONTEND_PORT must be an integer between 1 and 65535.");
  }
  return port;
}

function configuredBoolean(value: string | undefined, fallback: boolean): boolean {
  const normalized = value?.trim().toLowerCase();
  if (!normalized) return fallback;
  if (normalized === "1" || normalized === "true") return true;
  if (normalized === "0" || normalized === "false") return false;
  throw new Error("SYNTHCHAT_FRONTEND_STRICT_PORT must be true, false, 1, or 0.");
}

function requiredEnvironment(environment: Record<string, string>, name: string): string {
  const value = environment[name]?.trim();
  if (!value) throw new Error(`${name} is required in E2E serve mode.`);
  return value;
}

function checkedBackendTarget(value: string): string {
  const url = new URL(value);
  if (
    !["http:", "https:"].includes(url.protocol)
    || !LOOPBACK_HOSTS.has(url.hostname)
    || url.username
    || url.password
    || url.search
    || url.hash
    || !["", "/"].includes(url.pathname)
  ) {
    throw new Error("SYNTHCHAT_E2E_BACKEND_URL must be a loopback HTTP(S) origin.");
  }
  return url.origin;
}

function checkedBackendToken(value: string): string {
  if (value.length < 32 || value.length > 128 || !/^[\x21-\x7e]+$/u.test(value)) {
    throw new Error("SYNTHCHAT_E2E_BACKEND_TOKEN must be 32 to 128 visible ASCII characters.");
  }
  return value;
}

function protectedProxy(target: string, token: string): ProxyOptions {
  return {
    target,
    changeOrigin: false,
    configure(proxy) {
      proxy.on("proxyReq", (request) => {
        request.removeHeader("authorization");
        request.setHeader("authorization", `Bearer ${token}`);
      });
    },
  };
}

export default defineConfig(({ command, mode }) => {
  const environment = loadEnv(mode, process.cwd(), "");
  const e2eProxyEnabled = command === "serve" && mode === "e2e";
  let proxy: Record<string, ProxyOptions> | undefined;

  if (e2eProxyEnabled) {
    const target = checkedBackendTarget(
      requiredEnvironment(environment, "SYNTHCHAT_E2E_BACKEND_URL"),
    );
    const token = checkedBackendToken(
      requiredEnvironment(environment, "SYNTHCHAT_E2E_BACKEND_TOKEN"),
    );
    proxy = {
      "/health": { target, changeOrigin: false },
      "/api/v1": protectedProxy(target, token),
    };
  }

  return {
    plugins: [react()],
    define: {
      __SYNTHCHAT_E2E_PROXY__: JSON.stringify(e2eProxyEnabled),
    },
    server: {
      host: configuredHost(environment.SYNTHCHAT_FRONTEND_HOST),
      port: configuredPort(environment.SYNTHCHAT_FRONTEND_PORT),
      strictPort: configuredBoolean(environment.SYNTHCHAT_FRONTEND_STRICT_PORT, true),
      proxy,
      watch: {
        ignored: [
          "**/target/**",
          "**/target-codex-check/**",
          "../backend/target/**",
          "../desktop/target/**",
        ],
      },
    },
    clearScreen: false,
  };
});
