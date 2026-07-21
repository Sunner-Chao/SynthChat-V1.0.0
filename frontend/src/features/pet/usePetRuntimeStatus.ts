import { useCallback, useEffect, useRef, useState } from "react";
import {
  profilesApi as defaultProfilesApi,
  type Capabilities,
  type ProfileSummary,
  type ProfilesApi,
} from "../../api/profiles";
import {
  runsApi as defaultRunsApi,
  type Run,
  type RunsApi,
} from "../../api/runs";
import {
  sessionsApi as defaultSessionsApi,
  type Session,
  type SessionsApi,
} from "../../api/sessions";
import {
  runEventsApi as defaultRunEventsApi,
  type RunEventsApi,
} from "../../api/sse";
import { DEFAULT_PET_RUNTIME_CONFIG } from "../../config/runtimeConfig/pet";

export type PetRuntimePhase =
  | "loading"
  | "ready"
  | "queued"
  | "thinking"
  | "tool"
  | "approval"
  | "clarification"
  | "cancelling"
  | "offline";

export interface PetRuntimeStatus {
  phase: PetRuntimePhase;
  title: string;
  detail: string;
  profiles: ProfileSummary[];
  profile: ProfileSummary | null;
  latestSession: Session | null;
  activeRun: Run | null;
  latestDelta: string;
  engineAvailable: boolean;
  runTrackingAvailable: boolean;
  stale: boolean;
}

export interface PetRuntimeApis {
  profilesApi: Pick<ProfilesApi, "getCapabilities" | "listProfiles">;
  sessionsApi: Pick<SessionsApi, "listSessions">;
  runsApi: Pick<RunsApi, "listActiveRuns">;
  runEventsApi: Pick<RunEventsApi, "streamRunEvents">;
}

export interface PetRuntimeStatusOptions {
  apis?: PetRuntimeApis;
  pollIntervalMs?: number;
  selectedProfileId?: string | null;
}

const EMPTY_STATUS: PetRuntimeStatus = {
  phase: "loading",
  title: "正在连接 Hermes Rust",
  detail: "读取桌面后端状态",
  profiles: [],
  profile: null,
  latestSession: null,
  activeRun: null,
  latestDelta: "",
  engineAvailable: false,
  runTrackingAvailable: false,
  stale: false,
};

const DEFAULT_APIS: PetRuntimeApis = {
  profilesApi: defaultProfilesApi,
  sessionsApi: defaultSessionsApi,
  runsApi: defaultRunsApi,
  runEventsApi: defaultRunEventsApi,
};

function errorText(error: unknown): string {
  return error instanceof Error && error.message.trim()
    ? error.message.trim()
    : "本地 Rust 后端暂时不可用。";
}

function compactText(value: string, maximum = 150): string {
  const normalized = value.replace(/\s+/gu, " ").trim();
  if (normalized.length <= maximum) return normalized;
  return `${normalized.slice(0, Math.max(0, maximum - 1))}...`;
}

export function selectPetProfile(profiles: ProfileSummary[]): ProfileSummary | null {
  return profiles.find((profile) => profile.isActive)
    ?? profiles.find((profile) => profile.isDefault)
    ?? profiles[0]
    ?? null;
}

export function statusForRun(run: Run | null): Pick<PetRuntimeStatus, "phase" | "title" | "detail"> {
  if (!run) {
    return {
      phase: "ready",
      title: "准备就绪",
      detail: "没有正在进行的对话。",
    };
  }

  switch (run.status) {
    case "queued":
      return { phase: "queued", title: "对话排队中", detail: "等待上一轮对话完成。" };
    case "running":
      return { phase: "thinking", title: "正在思考", detail: "Hermes 正在生成回复。" };
    case "waitingApproval":
      return { phase: "approval", title: "等待工具授权", detail: "请在主聊天窗口处理授权请求。" };
    case "waitingClarification":
      return { phase: "clarification", title: "等待补充信息", detail: "请在主聊天窗口回答澄清问题。" };
    case "cancelling":
      return { phase: "cancelling", title: "正在停止", detail: "Hermes 正在结束当前对话。" };
    case "completed":
    case "cancelled":
    case "failed":
      return { phase: "ready", title: "准备就绪", detail: "没有正在进行的对话。" };
  }
}

function behaviorForEvent(
  previous: PetRuntimeStatus,
  event: string,
  data: unknown,
): PetRuntimeStatus {
  const record = data !== null && typeof data === "object" && !Array.isArray(data)
    ? data as Record<string, unknown>
    : {};
  const delta = typeof record.delta === "string" ? record.delta : "";
  const toolName = typeof record.name === "string" ? record.name : "";
  const detail = typeof record.message === "string" ? record.message : "";
  const terminal = event === "run.completed" || event === "run.cancelled" || event === "run.failed";

  if (terminal) {
    return {
      ...previous,
      activeRun: previous.activeRun ? { ...previous.activeRun, status: event === "run.failed" ? "failed" : event === "run.cancelled" ? "cancelled" : "completed" } : null,
      ...statusForRun(null),
    };
  }
  if (event === "message.delta") {
    return {
      ...previous,
      phase: "thinking",
      title: "正在回复",
      detail: "Hermes 正在输出消息。",
      latestDelta: compactText(`${previous.latestDelta}${delta}`, 240),
      stale: false,
    };
  }
  if (event === "reasoning.delta") {
    return { ...previous, phase: "thinking", title: "正在思考", detail: "Hermes 正在推理。", stale: false };
  }
  if (event === "tool.started") {
    return {
      ...previous,
      phase: "tool",
      title: "正在调用工具",
      detail: toolName ? `正在运行 ${compactText(toolName, 72)}。` : "正在运行 Rust 工具。",
      stale: false,
    };
  }
  if (event === "tool.progress") {
    return {
      ...previous,
      phase: "tool",
      title: "工具执行中",
      detail: detail ? compactText(detail) : "正在等待工具返回结果。",
      stale: false,
    };
  }
  if (event === "approval.required") {
    return { ...previous, phase: "approval", title: "等待工具授权", detail: "请在主聊天窗口处理授权请求。", stale: false };
  }
  if (event === "clarification.required") {
    return { ...previous, phase: "clarification", title: "等待补充信息", detail: "请在主聊天窗口回答澄清问题。", stale: false };
  }
  if (event === "run.cancelled") {
    return { ...previous, ...statusForRun(null), stale: false };
  }
  return { ...previous, stale: false };
}

function unavailableStatus(message: string): PetRuntimeStatus {
  return {
    ...EMPTY_STATUS,
    phase: "offline",
    title: "连接不可用",
    detail: message,
  };
}

function readyStatus(
  capabilities: Capabilities,
  profiles: ProfileSummary[],
  profile: ProfileSummary,
  latestSession: Session | null,
  activeRun: Run | null,
): PetRuntimeStatus {
  const runStatus = statusForRun(activeRun);
  const engineAvailable = capabilities.engine.available;
  if (!engineAvailable) {
    return {
      ...EMPTY_STATUS,
      phase: "offline",
      title: "Hermes 引擎不可用",
      detail: "请在主窗口检查 Profile 和后端状态。",
      profiles,
      profile,
      latestSession,
      activeRun,
      engineAvailable,
      runTrackingAvailable: capabilities.extensions.activeRunDiscovery,
    };
  }
  return {
    ...EMPTY_STATUS,
    ...runStatus,
    detail: activeRun ? runStatus.detail : latestSession
      ? `最近会话：${compactText(latestSession.title, 90)}`
      : "创建会话后，这里会显示运行状态。",
    profiles,
    profile,
    latestSession,
    activeRun,
    engineAvailable,
    runTrackingAvailable: capabilities.extensions.activeRunDiscovery,
  };
}

export function usePetRuntimeStatus(
  options: PetRuntimeStatusOptions = {},
): PetRuntimeStatus & { refresh(): void } {
  const apis = options.apis ?? DEFAULT_APIS;
  const pollIntervalMs = options.pollIntervalMs ?? DEFAULT_PET_RUNTIME_CONFIG.statusPollIntervalMs;
  const selectedProfileId = options.selectedProfileId ?? null;
  const [status, setStatus] = useState<PetRuntimeStatus>(EMPTY_STATUS);
  const [refreshVersion, setRefreshVersion] = useState(0);
  const activeRunRef = useRef<Run | null>(null);

  const refresh = useCallback(() => {
    setRefreshVersion((value) => value + 1);
  }, []);

  useEffect(() => {
    const controller = new AbortController();
    let disposed = false;
    setStatus((previous) => previous.phase === "offline" ? { ...EMPTY_STATUS } : previous);

    void (async () => {
      try {
        const [capabilities, profiles] = await Promise.all([
          apis.profilesApi.getCapabilities({ signal: controller.signal }),
          apis.profilesApi.listProfiles({ signal: controller.signal }),
        ]);
        const profile = profiles.find((item) => item.id === selectedProfileId)
          ?? selectPetProfile(profiles);
        if (!profile) {
          if (!disposed) {
            setStatus({
              ...EMPTY_STATUS,
              phase: "offline",
              title: "尚未配置 Profile",
              detail: "请在主窗口创建或导入 Hermes Profile。",
              profiles,
              engineAvailable: capabilities.engine.available,
              runTrackingAvailable: capabilities.extensions.activeRunDiscovery,
            });
          }
          return;
        }

        const sessions = await apis.sessionsApi.listSessions(
          { profileId: profile.id, limit: 1 },
          { signal: controller.signal },
        );
        const activeRuns = capabilities.extensions.activeRunDiscovery
          ? await apis.runsApi.listActiveRuns(profile.id, {}, { signal: controller.signal })
          : { items: [] };
        if (disposed) return;
        const activeRun = activeRuns.items.length > 0
          ? activeRuns.items[activeRuns.items.length - 1]!.run
          : null;
        activeRunRef.current = activeRun;
        setStatus(readyStatus(capabilities, profiles, profile, sessions.items[0] ?? null, activeRun));
      } catch (error) {
        if (!disposed && !controller.signal.aborted) setStatus(unavailableStatus(errorText(error)));
      }
    })();

    return () => {
      disposed = true;
      controller.abort();
    };
  }, [apis, refreshVersion, selectedProfileId]);

  useEffect(() => {
    if (!Number.isFinite(pollIntervalMs) || pollIntervalMs <= 0) return undefined;
    const interval = window.setInterval(refresh, Math.max(1_000, pollIntervalMs));
    return () => window.clearInterval(interval);
  }, [pollIntervalMs, refresh]);

  useEffect(() => {
    const run = activeRunRef.current ?? status.activeRun;
    if (!run || !status.runTrackingAvailable || status.phase === "offline") return undefined;

    const controller = new AbortController();
    let disposed = false;
    void (async () => {
      try {
        for await (const event of apis.runEventsApi.streamRunEvents(run.id, {
          lastSequence: run.lastSequence || undefined,
          sessionId: run.sessionId,
          signal: controller.signal,
        })) {
          if (disposed) return;
          setStatus((previous) => behaviorForEvent(previous, event.event, event.payload.data));
          if (event.event === "run.completed" || event.event === "run.cancelled" || event.event === "run.failed") {
            refresh();
            return;
          }
        }
      } catch (error) {
        if (!disposed && !controller.signal.aborted) {
          setStatus((previous) => ({ ...previous, stale: true, detail: "实时运行信号已断开，正在定期刷新。" }));
        }
      }
    })();

    return () => {
      disposed = true;
      controller.abort();
    };
  }, [
    apis,
    refresh,
    status.activeRun?.id,
    status.activeRun?.lastSequence,
    status.runTrackingAvailable,
  ]);

  return { ...status, refresh };
}
