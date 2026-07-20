import { LoaderCircle, Server, ServerOff } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { isTauri } from "@tauri-apps/api/core";
import {
  BackendApiError,
  backendApi,
  type BackendApiClient,
  type BackendHealth,
} from "../api/backend";
import { desktopBackendApi } from "../api/desktopConnection";
import { readBackendRuntimeConfig } from "../config/runtimeConfig/backend";

export type BackendStatusSnapshot =
  | { phase: "checking"; health: null; error: null }
  | { phase: "online"; health: BackendHealth; error: null }
  | { phase: "offline"; health: null; error: BackendApiError };

const INITIAL_BACKEND_STATUS: BackendStatusSnapshot = {
  phase: "checking",
  health: null,
  error: null,
};

export interface BackendStatusPresentation {
  label: string;
  title: string;
  tone: "checking" | "online" | "offline";
}

export function backendStatusPresentation(
  snapshot: BackendStatusSnapshot,
): BackendStatusPresentation {
  if (snapshot.phase === "online") {
    return {
      label: "后端在线",
      title: `Rust 后端在线，版本 ${snapshot.health.version || "未知"}。点击重新检查。`,
      tone: "online",
    };
  }
  if (snapshot.phase === "offline") {
    return {
      label: "后端未连接",
      title: `无法连接 Rust 后端：${snapshot.error.message} 点击重新检查。`,
      tone: "offline",
    };
  }
  return {
    label: "后端检测中",
    title: "正在检查 Rust 后端。",
    tone: "checking",
  };
}

export function BackendStatusView({
  onRefresh,
  snapshot,
}: {
  onRefresh: () => void;
  snapshot: BackendStatusSnapshot;
}) {
  const presentation = backendStatusPresentation(snapshot);
  const Icon = snapshot.phase === "checking"
    ? LoaderCircle
    : snapshot.phase === "online"
      ? Server
      : ServerOff;
  const accessibleLabel = snapshot.phase === "checking"
    ? presentation.label
    : `${presentation.label}，点击重新检查`;

  return (
    <button
      aria-busy={snapshot.phase === "checking"}
      aria-label={accessibleLabel}
      className={`backend-status backend-status--${presentation.tone}`}
      disabled={snapshot.phase === "checking"}
      onClick={onRefresh}
      title={presentation.title}
      type="button"
    >
      <Icon
        aria-hidden="true"
        className={snapshot.phase === "checking" ? "spin" : undefined}
        size={15}
      />
      <span aria-live="polite">{presentation.label}</span>
    </button>
  );
}

export function BackendStatusIndicator({
  client = isTauri() ? desktopBackendApi : backendApi,
  healthTimeoutMs,
  pollIntervalMs,
}: {
  client?: BackendApiClient;
  healthTimeoutMs?: number;
  pollIntervalMs?: number;
}) {
  const runtimeConfig = readBackendRuntimeConfig();
  const effectiveHealthTimeoutMs = healthTimeoutMs ?? runtimeConfig.healthTimeoutMs;
  const effectivePollIntervalMs = pollIntervalMs ?? runtimeConfig.statusPollIntervalMs;
  const [snapshot, setSnapshot] = useState<BackendStatusSnapshot>(
    INITIAL_BACKEND_STATUS,
  );
  const activeControllerRef = useRef<AbortController | null>(null);

  const refresh = useCallback(async () => {
    activeControllerRef.current?.abort();
    const controller = new AbortController();
    activeControllerRef.current = controller;
    setSnapshot((current) => current.phase === "online" ? current : INITIAL_BACKEND_STATUS);

    try {
      const health = await client.getHealth({
        signal: controller.signal,
        timeoutMs: effectiveHealthTimeoutMs,
      });
      if (activeControllerRef.current !== controller) return;
      setSnapshot({ phase: "online", health, error: null });
    } catch (error) {
      if (controller.signal.aborted || activeControllerRef.current !== controller) return;
      const backendError = error instanceof BackendApiError
        ? error
        : new BackendApiError(
          "network",
          "Backend health request failed unexpectedly.",
          { cause: error },
        );
      setSnapshot({ phase: "offline", health: null, error: backendError });
    } finally {
      if (activeControllerRef.current === controller) {
        activeControllerRef.current = null;
      }
    }
  }, [client, effectiveHealthTimeoutMs]);

  useEffect(() => {
    void refresh();
    const intervalId = window.setInterval(
      () => void refresh(),
      Math.max(1_000, effectivePollIntervalMs),
    );
    const refreshWhenOnline = () => void refresh();
    window.addEventListener("online", refreshWhenOnline);

    return () => {
      window.clearInterval(intervalId);
      window.removeEventListener("online", refreshWhenOnline);
      activeControllerRef.current?.abort();
      activeControllerRef.current = null;
    };
  }, [effectivePollIntervalMs, refresh]);

  return <BackendStatusView onRefresh={() => void refresh()} snapshot={snapshot} />;
}
