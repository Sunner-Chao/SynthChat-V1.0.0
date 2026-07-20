import {
  DEFAULT_BACKEND_RUNTIME_CONFIG,
  readBackendRuntimeConfig,
} from "../config/runtimeConfig/backend";

export const DEFAULT_BACKEND_BASE_URL = "http://127.0.0.1:8642";
export const DEFAULT_BACKEND_TIMEOUT_MS = DEFAULT_BACKEND_RUNTIME_CONFIG.healthTimeoutMs;
export const BACKEND_SERVICE_NAME = "synthchat-hermes-backend" as const;

export interface BackendHealth {
  status: "ok";
  service: typeof BACKEND_SERVICE_NAME;
  version: string;
}

export type BackendApiErrorKind =
  | "configuration"
  | "http"
  | "timeout"
  | "aborted"
  | "network"
  | "invalid_response";

export class BackendApiError extends Error {
  readonly kind: BackendApiErrorKind;
  readonly status?: number;

  constructor(
    kind: BackendApiErrorKind,
    message: string,
    options: { cause?: unknown; status?: number } = {},
  ) {
    super(message, options.cause === undefined ? undefined : { cause: options.cause });
    this.name = "BackendApiError";
    this.kind = kind;
    this.status = options.status;
  }
}

export interface BackendRequestOptions {
  signal?: AbortSignal;
  timeoutMs?: number;
}

export type BackendFetch = (
  input: RequestInfo | URL,
  init?: RequestInit,
) => Promise<Response>;

export interface BackendApiClientOptions {
  baseUrl?: string;
  fetch?: BackendFetch;
  timeoutMs?: number;
}

export interface BackendApiClient {
  readonly baseUrl: string;
  getHealth(options?: BackendRequestOptions): Promise<BackendHealth>;
}

type RuntimeBackendConfig = typeof globalThis & {
  __SYNTHCHAT_BACKEND_URL__?: unknown;
};

type BackendImportMeta = ImportMeta & {
  env?: {
    VITE_SYNTHCHAT_BACKEND_URL?: unknown;
  };
};

function configuredBackendBaseUrl(): string {
  const runtimeValue = (globalThis as RuntimeBackendConfig).__SYNTHCHAT_BACKEND_URL__;
  if (typeof runtimeValue === "string" && runtimeValue.trim()) {
    return runtimeValue;
  }

  const buildValue = (import.meta as BackendImportMeta).env?.VITE_SYNTHCHAT_BACKEND_URL;
  if (typeof buildValue === "string" && buildValue.trim()) {
    return buildValue;
  }

  return DEFAULT_BACKEND_BASE_URL;
}

export function normalizeBackendBaseUrl(value: string): string {
  const candidate = value.trim();
  let url: URL;
  try {
    url = new URL(candidate);
  } catch (cause) {
    throw new BackendApiError("configuration", "Backend URL is invalid.", { cause });
  }

  if (url.protocol !== "http:" && url.protocol !== "https:") {
    throw new BackendApiError(
      "configuration",
      "Backend URL must use HTTP or HTTPS.",
    );
  }
  if (url.username || url.password || url.search || url.hash) {
    throw new BackendApiError(
      "configuration",
      "Backend URL must not include credentials, a query, or a fragment.",
    );
  }

  const hostname = url.hostname.toLowerCase();
  if (!["127.0.0.1", "localhost", "::1", "[::1]"].includes(hostname)) {
    throw new BackendApiError(
      "configuration",
      "Backend URL must use a loopback hostname.",
    );
  }

  const pathname = url.pathname.replace(/\/+$/u, "");
  return pathname ? `${url.origin}${pathname}` : url.origin;
}

export function parseBackendHealth(value: unknown): BackendHealth {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new BackendApiError(
      "invalid_response",
      "Backend health response does not match the v1 contract.",
    );
  }

  const record = value as Record<string, unknown>;
  const allowedKeys = new Set(["status", "service", "version"]);
  const keys = Object.keys(record);
  const hasOnlyContractKeys =
    keys.length === allowedKeys.size && keys.every((key) => allowedKeys.has(key));

  if (
    !hasOnlyContractKeys
    || record.status !== "ok"
    || record.service !== BACKEND_SERVICE_NAME
    || typeof record.version !== "string"
  ) {
    throw new BackendApiError(
      "invalid_response",
      "Backend health response does not match the v1 contract.",
    );
  }

  return {
    status: "ok",
    service: BACKEND_SERVICE_NAME,
    version: record.version,
  };
}

function checkedTimeoutMs(value: number): number {
  if (!Number.isFinite(value) || value <= 0) {
    throw new BackendApiError(
      "configuration",
      "Backend request timeout must be a positive number.",
    );
  }
  return value;
}

function timeoutError(timeoutMs: number): BackendApiError {
  return new BackendApiError(
    "timeout",
    `Backend request timed out after ${timeoutMs} ms.`,
  );
}

class DefaultBackendApiClient implements BackendApiClient {
  readonly baseUrl: string;
  private readonly defaultTimeoutMs: number;
  private readonly fetchOverride?: BackendFetch;

  constructor(options: BackendApiClientOptions) {
    this.baseUrl = normalizeBackendBaseUrl(options.baseUrl ?? configuredBackendBaseUrl());
    this.defaultTimeoutMs = checkedTimeoutMs(
      options.timeoutMs ?? readBackendRuntimeConfig().healthTimeoutMs,
    );
    this.fetchOverride = options.fetch;
  }

  async getHealth(options: BackendRequestOptions = {}): Promise<BackendHealth> {
    if (options.signal?.aborted) {
      throw new BackendApiError("aborted", "Backend request was cancelled.");
    }

    const timeoutMs = checkedTimeoutMs(options.timeoutMs ?? this.defaultTimeoutMs);
    const controller = new AbortController();
    let timedOut = false;
    const abortFromCaller = () => controller.abort();
    options.signal?.addEventListener("abort", abortFromCaller, { once: true });
    const timeoutId = globalThis.setTimeout(() => {
      timedOut = true;
      controller.abort();
    }, timeoutMs);

    const ensureRequestActive = () => {
      if (timedOut) throw timeoutError(timeoutMs);
      if (options.signal?.aborted) {
        throw new BackendApiError("aborted", "Backend request was cancelled.");
      }
    };

    try {
      const fetchImpl = this.fetchOverride ?? globalThis.fetch.bind(globalThis);
      const response = await fetchImpl(`${this.baseUrl}/health`, {
        method: "GET",
        headers: { Accept: "application/json" },
        cache: "no-store",
        credentials: "omit",
        redirect: "error",
        signal: controller.signal,
      });
      ensureRequestActive();

      if (response.status !== 200) {
        throw new BackendApiError(
          "http",
          `Backend health request failed with HTTP ${response.status}.`,
          { status: response.status },
        );
      }

      let payload: unknown;
      try {
        payload = await response.json();
      } catch (cause) {
        throw new BackendApiError(
          "invalid_response",
          "Backend health response is not valid JSON.",
          { cause },
        );
      }
      ensureRequestActive();
      return parseBackendHealth(payload);
    } catch (cause) {
      if (cause instanceof BackendApiError) throw cause;
      if (timedOut) throw timeoutError(timeoutMs);
      if (options.signal?.aborted) {
        throw new BackendApiError("aborted", "Backend request was cancelled.", { cause });
      }
      throw new BackendApiError(
        "network",
        "Backend health request could not reach the local service.",
        { cause },
      );
    } finally {
      globalThis.clearTimeout(timeoutId);
      options.signal?.removeEventListener("abort", abortFromCaller);
    }
  }
}

export function createBackendApiClient(
  options: BackendApiClientOptions = {},
): BackendApiClient {
  return new DefaultBackendApiClient(options);
}

export const backendApi = createBackendApiClient();
