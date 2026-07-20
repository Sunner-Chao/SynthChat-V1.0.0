import { invoke, isTauri } from "@tauri-apps/api/core";
import {
  BackendApiError,
  createBackendApiClient,
  normalizeBackendBaseUrl,
  type BackendApiClient,
  type BackendFetch,
  type BackendRequestOptions,
} from "./backend";

declare const __SYNTHCHAT_E2E_PROXY__: boolean;

const BACKEND_CONNECTION_COMMAND = "get_backend_connection";
const DESKTOP_TOKEN_PATTERN = /^[0-9a-f]{64}$/u;

export type DesktopConnectionErrorKind =
  | "desktop_unavailable"
  | "invalid_connection"
  | "network";

export class DesktopConnectionError extends Error {
  readonly kind: DesktopConnectionErrorKind;

  constructor(kind: DesktopConnectionErrorKind, message: string) {
    super(message);
    this.name = "DesktopConnectionError";
    this.kind = kind;
  }
}

export interface DesktopRequestOptions {
  signal?: AbortSignal;
}

export interface DesktopTransport {
  request(
    path: string,
    init?: RequestInit,
    options?: DesktopRequestOptions,
  ): Promise<Response>;
}

interface DesktopConnection {
  baseUrl: string;
  token: string;
}

interface DesktopTransportDependencies {
  connect?: () => Promise<unknown>;
  fetch?: BackendFetch;
}

interface DesktopBackendApiDependencies extends DesktopTransportDependencies {
  timeoutMs?: number;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function parseDesktopConnection(value: unknown): DesktopConnection {
  if (!isRecord(value)) {
    throw new DesktopConnectionError(
      "invalid_connection",
      "Desktop backend connection did not match the expected contract.",
    );
  }

  const keys = Object.keys(value);
  if (
    keys.length !== 2
    || !keys.includes("baseUrl")
    || !keys.includes("token")
    || typeof value.baseUrl !== "string"
    || typeof value.token !== "string"
    || !DESKTOP_TOKEN_PATTERN.test(value.token)
  ) {
    throw new DesktopConnectionError(
      "invalid_connection",
      "Desktop backend connection did not match the expected contract.",
    );
  }

  let baseUrl: string;
  try {
    baseUrl = normalizeBackendBaseUrl(value.baseUrl);
  } catch {
    throw new DesktopConnectionError(
      "invalid_connection",
      "Desktop backend connection used an invalid local URL.",
    );
  }

  return { baseUrl, token: value.token };
}

async function connectThroughDesktop(): Promise<unknown> {
  if (!isTauri()) {
    throw new DesktopConnectionError(
      "desktop_unavailable",
      "Protected backend features require the SynthChat Desktop application.",
    );
  }

  try {
    return await invoke(BACKEND_CONNECTION_COMMAND);
  } catch {
    throw new DesktopConnectionError(
      "desktop_unavailable",
      "The managed desktop backend is unavailable.",
    );
  }
}

function checkedApiPath(path: string): string {
  if (!path.startsWith("/api/v1/") || path.includes("://")) {
    throw new DesktopConnectionError(
      "invalid_connection",
      "Protected backend requests must use an API v1 relative path.",
    );
  }
  return path;
}

function canRetryAfterNetworkFailure(init: RequestInit): boolean {
  const method = (init.method ?? "GET").toUpperCase();
  if (["GET", "HEAD", "OPTIONS", "PUT", "DELETE"].includes(method)) {
    return true;
  }
  return new Headers(init.headers).has("Idempotency-Key");
}

export function createDesktopTransport(
  dependencies: DesktopTransportDependencies = {},
): DesktopTransport {
  const connect = dependencies.connect ?? connectThroughDesktop;
  const fetchImpl = dependencies.fetch ?? globalThis.fetch.bind(globalThis);
  let currentConnection: DesktopConnection | null = null;
  let pendingConnection: Promise<DesktopConnection> | null = null;

  const getConnection = async (): Promise<DesktopConnection> => {
    if (currentConnection) return currentConnection;
    if (!pendingConnection) {
      pendingConnection = connect()
        .then(parseDesktopConnection)
        .then((connection) => {
          currentConnection = connection;
          return connection;
        })
        .finally(() => {
          pendingConnection = null;
        });
    }
    return pendingConnection;
  };

  return {
    async request(path, init = {}, options = {}) {
      const apiPath = checkedApiPath(path);

      for (let attempt = 0; attempt < 2; attempt += 1) {
        const connection = await getConnection();
        const headers = new Headers(init.headers);
        headers.set("Authorization", `Bearer ${connection.token}`);

        let response: Response;
        try {
          response = await fetchImpl(`${connection.baseUrl}${apiPath}`, {
            ...init,
            headers,
            cache: "no-store",
            credentials: "omit",
            redirect: "error",
            signal: options.signal ?? init.signal,
          });
        } catch {
          if (options.signal?.aborted || init.signal?.aborted) {
            throw new DOMException("The request was aborted.", "AbortError");
          }
          if (currentConnection === connection) currentConnection = null;
          if (attempt === 0 && canRetryAfterNetworkFailure(init)) {
            continue;
          }
          throw new DesktopConnectionError(
            "network",
            "The local desktop backend could not be reached.",
          );
        }

        if (response.status !== 401 || attempt === 1) return response;

        if (currentConnection === connection) currentConnection = null;
        try {
          await response.body?.cancel();
        } catch {
          // The response is being discarded before a single authenticated retry.
        }
      }

      throw new DesktopConnectionError(
        "network",
        "The local desktop backend request did not complete.",
      );
    },
  };
}

export function createDesktopBackendApiClient(
  dependencies: DesktopBackendApiDependencies = {},
): BackendApiClient {
  const connect = dependencies.connect ?? connectThroughDesktop;
  const fetchImpl = dependencies.fetch ?? globalThis.fetch.bind(globalThis);

  return {
    baseUrl: "tauri://managed-backend",
    async getHealth(options: BackendRequestOptions = {}) {
      if (options.signal?.aborted) {
        throw new BackendApiError(
          "aborted",
          "Backend request was cancelled.",
        );
      }

      const connection = parseDesktopConnection(await connect());
      return createBackendApiClient({
        baseUrl: connection.baseUrl,
        fetch: fetchImpl,
        timeoutMs: dependencies.timeoutMs,
      }).getHealth(options);
    },
  };
}

function createE2eSameOriginTransport(): DesktopTransport {
  const fetchImpl = globalThis.fetch.bind(globalThis);

  return {
    async request(path, init = {}, options = {}) {
      const apiPath = checkedApiPath(path);
      const headers = new Headers(init.headers);
      headers.delete("Authorization");

      try {
        return await fetchImpl(apiPath, {
          ...init,
          headers,
          cache: "no-store",
          credentials: "omit",
          redirect: "error",
          signal: options.signal ?? init.signal,
        });
      } catch {
        if (options.signal?.aborted || init.signal?.aborted) {
          throw new DOMException("The request was aborted.", "AbortError");
        }
        throw new DesktopConnectionError(
          "network",
          "The local E2E backend proxy could not be reached.",
        );
      }
    },
  };
}

function e2eProxyEnabled(): boolean {
  return typeof __SYNTHCHAT_E2E_PROXY__ !== "undefined" && __SYNTHCHAT_E2E_PROXY__ === true;
}

export const desktopTransport = e2eProxyEnabled()
  ? createE2eSameOriginTransport()
  : createDesktopTransport();

export const desktopBackendApi = createDesktopBackendApiClient();
