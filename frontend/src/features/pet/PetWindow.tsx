import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type FormEvent,
} from "react";
import {
  AlertTriangle,
  Check,
  Grip,
  LoaderCircle,
  Menu,
  RefreshCw,
  Save,
  SendHorizontal,
  ServerCog,
  Sparkles,
  Square,
  Wrench,
  X,
} from "lucide-react";
import { isTauri } from "@tauri-apps/api/core";
import {
  profilesApi as defaultProfilesApi,
  type ProfileConfig,
  type ProfilesApi,
  type Versioned,
} from "../../api/profiles";
import {
  RunApiError,
  runsApi as defaultRunsApi,
  type ProblemDetails,
  type Run,
  type RunsApi,
} from "../../api/runs";
import {
  sessionsApi as defaultSessionsApi,
  type Message,
  type SessionsApi,
} from "../../api/sessions";
import {
  runEventsApi as defaultRunEventsApi,
  type RunEventsApi,
} from "../../api/sse";
import { readPetRuntimeConfig } from "../../config/runtimeConfig/pet";
import {
  desktopPetWindow,
  type PetWindowBridge,
} from "./desktopPet";
import {
  usePetRuntimeStatus,
  type PetRuntimeApis,
} from "./usePetRuntimeStatus";
import "./pet.css";

const PET_FRAME_SOURCE = "synthchat-pet-frame";
const PET_HOST_SOURCE = "synthchat-pet-host";
const PET_MODEL_STORAGE_KEY = "synthchat.pet.defaultModelId";

const BUILTIN_PET_MODELS = [
  { id: "tororo", name: "Tororo", url: "/pet/model/Tororo/tororo.model3.json" },
  { id: "hijiki", name: "Hijiki", url: "/pet/model/Hijiki/hijiki.model3.json" },
  { id: "mao", name: "Mao", url: "/pet/model/Mao/Mao.model3.json" },
  { id: "wanko", name: "Wanko", url: "/pet/model/Wanko/Wanko.model3.json" },
  { id: "hiyori", name: "Hiyori", url: "/pet/model/Hiyori/Hiyori.model3.json" },
  { id: "natori", name: "Natori", url: "/pet/model/Natori/Natori.model3.json" },
  { id: "mark", name: "Mark", url: "/pet/model/Mark/Mark.model3.json" },
] as const;

type PetModel = { id: string; name: string; url: string };
type PetFrameMessage = {
  source?: string;
  type?: string;
  message?: string;
};
type PetChatPhase = "idle" | "creating" | "streaming" | "cancelling" | "stale";

interface PetChatState {
  phase: PetChatPhase;
  run: Run | null;
  sessionId: string | null;
  response: string;
  activity: string | null;
  error: string | null;
}

export interface PetInteractionApis {
  profilesApi: Pick<ProfilesApi, "getProfileConfig" | "updateProfileConfig">;
  sessionsApi: Pick<SessionsApi, "createSession" | "listMessages">;
  runsApi: Pick<RunsApi, "createRun" | "getRun" | "cancelRun">;
  runEventsApi: Pick<RunEventsApi, "streamRunEvents">;
}

export interface PetWindowProps {
  bridge?: PetWindowBridge;
  runtimeApis?: PetRuntimeApis;
  interactionApis?: PetInteractionApis;
  pollIntervalMs?: number;
  frameUrl?: string;
  modelUrl?: string;
}

const DEFAULT_INTERACTION_APIS: PetInteractionApis = {
  profilesApi: defaultProfilesApi,
  sessionsApi: defaultSessionsApi,
  runsApi: defaultRunsApi,
  runEventsApi: defaultRunEventsApi,
};

const EMPTY_CHAT: PetChatState = {
  phase: "idle",
  run: null,
  sessionId: null,
  response: "",
  activity: null,
  error: null,
};

function PetDesktopUnavailable() {
  return (
    <main className="pet-window pet-window--unavailable" aria-label="SynthChat 桌宠">
      <section className="pet-window__unavailable" role="alert">
        <ServerCog aria-hidden="true" size={28} />
        <strong>桌宠仅在 Desktop 应用中可用</strong>
        <span>请从 SynthChat Desktop 打开桌宠窗口。</span>
      </section>
    </main>
  );
}

function behaviorForPhase(phase: string): string {
  switch (phase) {
    case "thinking":
      return "thinking";
    case "tool":
      return "curious";
    case "approval":
    case "clarification":
      return "listening";
    case "offline":
    case "stale":
      return "error";
    case "ready":
      return "idle";
    default:
      return "idle";
  }
}

function phaseClassName(phase: string): string {
  return `pet-window__cloud--${phase}`;
}

function newRequestId(): string {
  if (typeof globalThis.crypto?.randomUUID === "function") return globalThis.crypto.randomUUID();
  const bytes = new Uint8Array(16);
  globalThis.crypto.getRandomValues(bytes);
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

function messageText(message: Message): string {
  return message.parts
    .filter((part): part is Extract<Message["parts"][number], { type: "text" }> => part.type === "text")
    .map((part) => part.text)
    .join("\n")
    .trim();
}

function errorText(error: unknown, fallback: string): string {
  return error instanceof Error && error.message.trim() ? error.message.trim() : fallback;
}

function publicProblemMessage(problem: ProblemDetails): string {
  switch (problem.code) {
    case "provider_configuration_invalid":
      return "当前 Profile 的 Provider、模型或 Base URL 配置无效。";
    case "provider_authentication_failed":
      return "模型服务拒绝了当前 API Key，请在主窗口重新保存密钥。";
    case "provider_rate_limited":
      return "模型服务正在限流，请稍后重试。";
    case "provider_request_rejected":
      return "模型服务拒绝了当前模型或请求参数。";
    case "provider_stream_failed":
      return "模型服务的流式响应中断，请刷新后重试。";
    case "provider_response_invalid":
      return "模型服务返回了不完整或不兼容的响应。";
    default:
      return problem.detail?.trim() || problem.title;
  }
}

function trustedPetModels(configuredUrl: string): PetModel[] {
  const builtins = BUILTIN_PET_MODELS.map((model) => ({
    ...model,
    url: readPetRuntimeConfig({ modelUrl: model.url }).modelUrl,
  }));
  if (builtins.some((model) => model.url === configuredUrl)) return builtins;
  return [{ id: "configured", name: "Configured", url: configuredUrl }, ...builtins];
}

function initialPetModel(
  models: PetModel[],
  configuredUrl: string,
  forceConfigured: boolean,
): PetModel {
  if (forceConfigured) return models.find((model) => model.url === configuredUrl) ?? models[0]!;
  if (!forceConfigured) {
    try {
      const stored = window.localStorage.getItem(PET_MODEL_STORAGE_KEY);
      const selected = models.find((model) => model.id === stored);
      if (selected) return selected;
    } catch {
      // Preference storage is optional in restricted desktop environments.
    }
  }
  return models.find((model) => model.url === configuredUrl)
    ?? models.find((model) => model.url.includes("/Hiyori/"))
    ?? models[0]!;
}

function ConnectedPetWindow({
  bridge = desktopPetWindow,
  frameUrl,
  modelUrl,
  runtimeApis,
  interactionApis = DEFAULT_INTERACTION_APIS,
  pollIntervalMs,
}: PetWindowProps) {
  const petResources = readPetRuntimeConfig({ frameUrl, modelUrl });
  const models = useMemo(() => trustedPetModels(petResources.modelUrl), [petResources.modelUrl]);
  const [selectedModel, setSelectedModel] = useState<PetModel>(() => (
    initialPetModel(models, petResources.modelUrl, modelUrl !== undefined)
  ));
  const [selectedProfileId, setSelectedProfileId] = useState<string | null>(null);
  const frameRef = useRef<HTMLIFrameElement>(null);
  const modelReadyRef = useRef(false);
  const streamControllerRef = useRef<AbortController | null>(null);
  const lastSequenceRef = useRef(0);
  const sessionByProfileRef = useRef(new Map<string, string>());
  const sendAttemptRef = useRef<{
    profileId: string;
    text: string;
    clientRequestId: string;
    idempotencyKey: string;
  } | null>(null);
  const [modelError, setModelError] = useState<string | null>(null);
  const [dragError, setDragError] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const [menuOpen, setMenuOpen] = useState(false);
  const [profileConfig, setProfileConfig] = useState<Versioned<ProfileConfig> | null>(null);
  const [inferenceModel, setInferenceModel] = useState("");
  const [configLoading, setConfigLoading] = useState(false);
  const [configSaving, setConfigSaving] = useState(false);
  const [configError, setConfigError] = useState<string | null>(null);
  const [chat, setChat] = useState<PetChatState>(EMPTY_CHAT);
  const runtime = usePetRuntimeStatus({
    apis: runtimeApis,
    pollIntervalMs: pollIntervalMs ?? petResources.statusPollIntervalMs,
    selectedProfileId,
  });
  const selectedProfile = runtime.profiles.find((profile) => profile.id === selectedProfileId);
  const effectiveProfileId = selectedProfile?.id ?? runtime.profile?.id ?? null;
  const chatBusy = chat.phase === "creating"
    || chat.phase === "streaming"
    || chat.phase === "cancelling";
  const runtimeBusy = Boolean(runtime.activeRun
    && !["completed", "cancelled", "failed"].includes(runtime.activeRun.status));

  const selectProfile = (profileId: string) => {
    setSelectedProfileId(profileId);
    setProfileConfig(null);
    setInferenceModel("");
    setConfigError(null);
    setChat(EMPTY_CHAT);
  };

  const postToModel = useCallback((message: Record<string, unknown>): void => {
    const target = frameRef.current?.contentWindow;
    if (!target) return;
    target.postMessage(
      { source: PET_HOST_SOURCE, ...message },
      window.location.origin,
    );
  }, []);

  const startDragging = useCallback(() => {
    setDragError(null);
    void bridge.startDragging().catch((error: unknown) => {
      setDragError(errorText(error, "无法拖动桌宠窗口。"));
    });
  }, [bridge]);

  useEffect(() => {
    document.documentElement.classList.add("pet-window-route");
    void bridge.setIgnoreCursorEvents(false).catch(() => undefined);
    return () => {
      document.documentElement.classList.remove("pet-window-route");
      streamControllerRef.current?.abort();
      void bridge.setIgnoreCursorEvents(false).catch(() => undefined);
    };
  }, [bridge]);

  useEffect(() => {
    if (selectedProfileId === null && runtime.profile) setSelectedProfileId(runtime.profile.id);
  }, [runtime.profile, selectedProfileId]);

  useEffect(() => {
    if (!modelReadyRef.current) return;
    const phase = chat.phase === "streaming" ? "thinking" : runtime.phase;
    postToModel({ type: "behavior", name: behaviorForPhase(phase) });
  }, [chat.phase, postToModel, runtime.phase]);

  useEffect(() => {
    const handleFrameMessage = (event: MessageEvent<PetFrameMessage>) => {
      if (event.origin !== window.location.origin || event.source !== frameRef.current?.contentWindow) return;
      const message = event.data;
      if (!message || message.source !== PET_FRAME_SOURCE) return;

      if (message.type === "ready") {
        postToModel({ type: "load", url: selectedModel.url });
      } else if (message.type === "loaded") {
        modelReadyRef.current = true;
        setModelError(null);
        const phase = chat.phase === "streaming" ? "thinking" : runtime.phase;
        postToModel({ type: "behavior", name: behaviorForPhase(phase) });
      } else if (message.type === "error") {
        modelReadyRef.current = false;
        setModelError(message.message?.trim() || "桌宠模型无法加载。");
      } else if (message.type === "model_drag_start") {
        startDragging();
      }
    };

    window.addEventListener("message", handleFrameMessage as EventListener);
    return () => window.removeEventListener("message", handleFrameMessage as EventListener);
  }, [chat.phase, postToModel, runtime.phase, selectedModel.url, startDragging]);

  useEffect(() => {
    if (!menuOpen || !effectiveProfileId) return undefined;
    const controller = new AbortController();
    setConfigLoading(true);
    setConfigError(null);
    void interactionApis.profilesApi.getProfileConfig(effectiveProfileId, {
      signal: controller.signal,
    }).then((config) => {
      if (controller.signal.aborted) return;
      setProfileConfig(config);
      setInferenceModel(config.value.model.model);
    }).catch((error: unknown) => {
      if (!controller.signal.aborted) setConfigError(errorText(error, "无法读取模型配置。"));
    }).finally(() => {
      if (!controller.signal.aborted) setConfigLoading(false);
    });
    return () => controller.abort();
  }, [effectiveProfileId, interactionApis.profilesApi, menuOpen]);

  const recoverAssistantText = useCallback(async (sessionId: string): Promise<string> => {
    const page = await interactionApis.sessionsApi.listMessages(sessionId, { limit: 100 });
    const assistant = [...page.items].reverse().find((message) => message.role === "assistant");
    return assistant ? messageText(assistant) : "";
  }, [interactionApis.sessionsApi]);

  const streamRun = useCallback(async (run: Run, resumeFrom = 0): Promise<void> => {
    streamControllerRef.current?.abort();
    const controller = new AbortController();
    streamControllerRef.current = controller;
    lastSequenceRef.current = resumeFrom;
    setChat((current) => ({
      ...current,
      phase: "streaming",
      run,
      sessionId: run.sessionId,
      activity: resumeFrom > 0 ? "正在恢复实时回复" : "Hermes 正在思考",
      error: null,
    }));

    try {
      for await (const event of interactionApis.runEventsApi.streamRunEvents(run.id, {
        lastSequence: resumeFrom || undefined,
        sessionId: run.sessionId,
        signal: controller.signal,
      })) {
        if (controller.signal.aborted) return;
        lastSequenceRef.current = event.payload.sequence;
        const data = event.payload.data as Record<string, unknown>;
        if (event.event === "message.delta" && typeof data.delta === "string") {
          setChat((current) => ({
            ...current,
            response: `${current.response}${data.delta}`,
            activity: "Hermes 正在回复",
          }));
        } else if (event.event === "tool.started") {
          const name = typeof data.name === "string" ? data.name : "Rust 工具";
          setChat((current) => ({ ...current, activity: `正在调用 ${name}` }));
        } else if (event.event === "tool.progress") {
          const detail = typeof data.message === "string" ? data.message : "工具执行中";
          setChat((current) => ({ ...current, activity: detail }));
        } else if (event.event === "approval.required") {
          setChat((current) => ({ ...current, activity: "等待主窗口授权工具" }));
        } else if (event.event === "clarification.required") {
          setChat((current) => ({ ...current, activity: "等待主窗口补充信息" }));
        } else if (event.event === "message.completed") {
          const message = data.message as Message | undefined;
          const completedText = message ? messageText(message) : "";
          if (completedText) setChat((current) => ({ ...current, response: completedText }));
        } else if (event.event === "run.completed") {
          setChat((current) => ({ ...current, phase: "idle", activity: "回复完成", error: null }));
          sendAttemptRef.current = null;
          runtime.refresh();
          return;
        } else if (event.event === "run.cancelled") {
          setChat((current) => ({ ...current, phase: "idle", activity: "已停止回复", error: null }));
          sendAttemptRef.current = null;
          runtime.refresh();
          return;
        } else if (event.event === "run.failed") {
          const problem = data.error as ProblemDetails | undefined;
          setChat((current) => ({
            ...current,
            phase: "idle",
            activity: null,
            error: problem ? publicProblemMessage(problem) : "Hermes 无法完成本次回复。",
          }));
          sendAttemptRef.current = null;
          runtime.refresh();
          return;
        }
      }

      const recovered = await interactionApis.runsApi.getRun(run.id);
      if (recovered.status === "completed") {
        const response = await recoverAssistantText(run.sessionId);
        setChat((current) => ({
          ...current,
          phase: "idle",
          run: recovered,
          response: response || current.response,
          activity: "回复完成",
          error: null,
        }));
        sendAttemptRef.current = null;
      } else if (recovered.status === "failed") {
        setChat((current) => ({
          ...current,
          phase: "idle",
          run: recovered,
          activity: null,
          error: recovered.error ? publicProblemMessage(recovered.error) : "Hermes 无法完成本次回复。",
        }));
      } else {
        setChat((current) => ({
          ...current,
          phase: "stale",
          run: recovered,
          activity: "实时回复已断开",
          error: "点击刷新可继续接收当前回复。",
        }));
      }
      runtime.refresh();
    } catch (error) {
      if (controller.signal.aborted) return;
      setChat((current) => ({
        ...current,
        phase: "stale",
        activity: "实时回复已断开",
        error: errorText(error, "点击刷新可继续接收当前回复。"),
      }));
      runtime.refresh();
    }
  }, [interactionApis.runEventsApi, interactionApis.runsApi, recoverAssistantText, runtime]);

  const handleSubmit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const text = draft.trim();
    const profileId = effectiveProfileId;
    if (!text || !profileId || chatBusy || runtimeBusy || !runtime.engineAvailable) return;

    let attempt = sendAttemptRef.current;
    if (!attempt || attempt.profileId !== profileId || attempt.text !== text) {
      attempt = {
        profileId,
        text,
        clientRequestId: newRequestId(),
        idempotencyKey: `pet-${newRequestId()}`,
      };
      sendAttemptRef.current = attempt;
    }

    setChat({ ...EMPTY_CHAT, phase: "creating", activity: "正在发送消息" });
    try {
      let sessionId = sessionByProfileRef.current.get(profileId)
        ?? (runtime.latestSession?.profileId === profileId ? runtime.latestSession.id : null);
      if (!sessionId) {
        const created = await interactionApis.sessionsApi.createSession(
          { profileId, title: "Pet 对话" },
          `pet-session-${newRequestId()}`,
        );
        sessionId = created.value.id;
      }
      sessionByProfileRef.current.set(profileId, sessionId);
      const accepted = await interactionApis.runsApi.createRun(sessionId, {
        clientRequestId: attempt.clientRequestId,
        message: { text, fileIds: [] },
      }, attempt.idempotencyKey);
      setDraft("");
      runtime.refresh();
      void streamRun(accepted.run);
    } catch (error) {
      const retryable = error instanceof RunApiError && error.retryable;
      if (!retryable) sendAttemptRef.current = null;
      setChat((current) => ({
        ...current,
        phase: "idle",
        activity: null,
        error: errorText(error, "无法发起 Pet 对话。"),
      }));
    }
  };

  const handleCancel = async () => {
    const run = chat.run ?? runtime.activeRun;
    if (!run || chat.phase === "cancelling") return;
    setChat((current) => ({ ...current, phase: "cancelling", activity: "正在停止回复", error: null }));
    try {
      const cancelled = await interactionApis.runsApi.cancelRun(run.id);
      streamControllerRef.current?.abort();
      sendAttemptRef.current = null;
      setChat((current) => ({
        ...current,
        phase: "idle",
        run: cancelled,
        activity: "已停止回复",
        error: null,
      }));
      runtime.refresh();
    } catch (error) {
      setChat((current) => ({
        ...current,
        phase: "streaming",
        activity: "仍在接收回复",
        error: errorText(error, "无法取消当前回复。"),
      }));
    }
  };

  const handleRefresh = () => {
    runtime.refresh();
    const run = chat.run;
    if (!run || chat.phase !== "stale") return;
    void interactionApis.runsApi.getRun(run.id).then(async (latest) => {
      if (latest.status === "completed") {
        const response = await recoverAssistantText(latest.sessionId);
        setChat((current) => ({
          ...current,
          phase: "idle",
          run: latest,
          response: response || current.response,
          activity: "回复完成",
          error: null,
        }));
      } else if (latest.status === "failed") {
        setChat((current) => ({
          ...current,
          phase: "idle",
          run: latest,
          activity: null,
          error: latest.error ? publicProblemMessage(latest.error) : "Hermes 无法完成本次回复。",
        }));
      } else if (latest.status === "cancelled") {
        setChat((current) => ({ ...current, phase: "idle", run: latest, activity: "已停止回复", error: null }));
      } else {
        void streamRun(latest, lastSequenceRef.current);
      }
    }).catch((error: unknown) => {
      setChat((current) => ({ ...current, error: errorText(error, "无法刷新当前回复。") }));
    });
  };

  const switchPetModel = (model: PetModel) => {
    setSelectedModel(model);
    setModelError(null);
    try {
      window.localStorage.setItem(PET_MODEL_STORAGE_KEY, model.id);
    } catch {
      // Preference storage is optional in restricted desktop environments.
    }
    postToModel({ type: "load", url: model.url, force: true });
  };

  const saveInferenceModel = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const profileId = effectiveProfileId;
    const model = inferenceModel.trim();
    if (!profileId || !profileConfig || !model || configSaving) return;
    if (model === profileConfig.value.model.model) return;
    setConfigSaving(true);
    setConfigError(null);
    try {
      const updated = await interactionApis.profilesApi.updateProfileConfig(
        profileId,
        { model: { model } },
        profileConfig.etag,
      );
      setProfileConfig(updated);
      setInferenceModel(updated.value.model.model);
      runtime.refresh();
    } catch (error) {
      setConfigError(errorText(error, "无法保存推理模型。"));
    } finally {
      setConfigSaving(false);
    }
  };

  const onFrameLoad = () => {
    postToModel({ type: "load", url: selectedModel.url });
  };
  const canCancel = Boolean(
    (chat.run && (chat.phase === "streaming" || chat.phase === "cancelling"))
    || runtimeBusy,
  );
  const displayError = modelError ?? dragError ?? chat.error;
  const preview = (chat.response || runtime.latestDelta).trim();
  const displayPhase = chat.phase === "creating" || chat.phase === "streaming"
    ? "thinking"
    : chat.phase === "cancelling"
      ? "cancelling"
      : chat.phase === "stale"
        ? "stale"
        : runtime.phase;
  const displayTitle = chat.activity
    ?? (chat.error ? "对话需要处理" : runtime.title);
  const displayDetail = chat.response
    ? "回复会实时显示在桌宠旁。"
    : chat.error
      ? chat.error
      : runtime.detail;

  return (
    <main className="pet-window" aria-label="SynthChat 桌宠">
      <section className="pet-window__stage" aria-label="桌宠模型">
        <iframe
          className="pet-window__frame"
          ref={frameRef}
          src={petResources.frameUrl}
          title="SynthChat Live2D 桌宠"
          onLoad={onFrameLoad}
        />

        <div className={`pet-window__cloud ${phaseClassName(displayPhase)}`} role="status" aria-live="polite">
          <span className="pet-window__cloud-indicator" aria-hidden="true">
            {displayPhase === "thinking" || displayPhase === "tool" || displayPhase === "loading" ? (
              <LoaderCircle size={14} />
            ) : displayPhase === "offline" || displayPhase === "stale" ? (
              <AlertTriangle size={14} />
            ) : displayPhase === "approval" || displayPhase === "clarification" ? (
              <Wrench size={14} />
            ) : (
              <Sparkles size={14} />
            )}
          </span>
          <div>
            <strong>{displayTitle}</strong>
            <span>{displayDetail}</span>
          </div>
        </div>

        {preview && !menuOpen ? (
          <p className="pet-window__preview" title={preview}>{preview}</p>
        ) : null}

        {menuOpen ? (
          <section className="pet-window__menu" aria-label="桌宠设置">
            <header>
              <strong>桌宠设置</strong>
              <button aria-label="关闭桌宠设置" onClick={() => setMenuOpen(false)} title="关闭" type="button">
                <X aria-hidden="true" size={15} />
              </button>
            </header>

            <label className="pet-window__field">
              <span>Profile</span>
              <select
                aria-label="Pet Profile"
                disabled={chatBusy || runtimeBusy || runtime.profiles.length === 0}
                onChange={(event) => selectProfile(event.target.value)}
                value={effectiveProfileId ?? ""}
              >
                {runtime.profiles.map((profile) => (
                  <option key={profile.id} value={profile.id}>{profile.displayName}</option>
                ))}
              </select>
            </label>

            <form className="pet-window__model-config" onSubmit={saveInferenceModel}>
              <label className="pet-window__field">
                <span>推理模型</span>
                <input
                  aria-label="Pet 推理模型"
                  autoComplete="off"
                  disabled={configLoading || configSaving || !profileConfig}
                  onChange={(event) => setInferenceModel(event.target.value)}
                  placeholder={configLoading ? "正在读取..." : "模型名称"}
                  value={inferenceModel}
                />
              </label>
              <button
                aria-label="保存 Pet 推理模型"
                disabled={
                  configLoading
                  || configSaving
                  || !profileConfig
                  || !inferenceModel.trim()
                  || inferenceModel.trim() === profileConfig.value.model.model
                }
                title="保存推理模型"
                type="submit"
              >
                {configSaving ? <LoaderCircle aria-hidden="true" size={14} /> : <Save aria-hidden="true" size={14} />}
              </button>
            </form>
            {configError ? <p className="pet-window__menu-error" role="alert">{configError}</p> : null}

            <div className="pet-window__model-picker" role="group" aria-label="Live2D 模型">
              <span>Live2D 模型</span>
              <div>
                {models.map((model) => (
                  <button
                    className={model.id === selectedModel.id ? "is-selected" : ""}
                    key={model.id}
                    onClick={() => switchPetModel(model)}
                    title={`切换到 ${model.name}`}
                    type="button"
                  >
                    {model.id === selectedModel.id ? <Check aria-hidden="true" size={12} /> : null}
                    {model.name}
                  </button>
                ))}
              </div>
            </div>
          </section>
        ) : null}
      </section>

      <form className="pet-window__composer" aria-label="Pet 聊天输入" onSubmit={handleSubmit}>
        <button
          aria-expanded={menuOpen}
          aria-label="打开桌宠设置"
          className="pet-window__icon-button"
          onClick={() => setMenuOpen((open) => !open)}
          title="Profile 与模型"
          type="button"
        >
          <Menu aria-hidden="true" size={16} />
        </button>
        <input
          aria-label="给 Pet 发送消息"
          autoComplete="off"
          disabled={!effectiveProfileId || !runtime.engineAvailable}
          onChange={(event) => setDraft(event.target.value)}
          placeholder={effectiveProfileId ? "说点什么..." : "请先配置 Profile"}
          value={draft}
        />
        {canCancel ? (
          <button
            aria-label="停止 Pet 回复"
            className="pet-window__send-button is-stop"
            disabled={chat.phase === "cancelling"}
            onClick={() => void handleCancel()}
            title="停止"
            type="button"
          >
            {chat.phase === "cancelling" ? <LoaderCircle aria-hidden="true" size={15} /> : <Square aria-hidden="true" size={13} />}
          </button>
        ) : (
          <button
            aria-label="发送 Pet 消息"
            className="pet-window__send-button"
            disabled={chatBusy || runtimeBusy || !draft.trim() || !effectiveProfileId || !runtime.engineAvailable}
            title="发送"
            type="submit"
          >
            {chat.phase === "creating"
              ? <LoaderCircle aria-hidden="true" size={15} />
              : <SendHorizontal aria-hidden="true" size={16} />}
          </button>
        )}
      </form>

      <section className="pet-window__control-strip" aria-label="桌宠控制">
        <label className="pet-window__profile" title={runtime.profile?.displayName ?? "尚未连接 Profile"}>
          <ServerCog aria-hidden="true" size={14} />
          <select
            aria-label="桌宠 Profile"
            disabled={chatBusy || runtimeBusy || runtime.profiles.length === 0}
            onChange={(event) => selectProfile(event.target.value)}
            value={effectiveProfileId ?? ""}
          >
            {runtime.profiles.length === 0 ? <option value="">未连接 Profile</option> : null}
            {runtime.profiles.map((profile) => (
              <option key={profile.id} value={profile.id}>{profile.displayName}</option>
            ))}
          </select>
          {runtime.stale ? <small>重连中</small> : null}
        </label>

        <div className="pet-window__actions">
          <button
            aria-label="刷新桌宠状态"
            className="pet-window__icon-button"
            onClick={handleRefresh}
            title={chat.phase === "stale" ? "恢复实时回复" : "刷新状态"}
            type="button"
          >
            <RefreshCw aria-hidden="true" size={15} />
          </button>
          <button
            aria-label="拖动桌宠"
            className="pet-window__icon-button pet-window__drag-button"
            onPointerDown={startDragging}
            title="拖动桌宠"
            type="button"
          >
            <Grip aria-hidden="true" size={17} />
          </button>
        </div>
      </section>

      {displayError ? <p className="pet-window__error" role="alert">{displayError}</p> : null}

      {!runtime.runTrackingAvailable && runtime.phase !== "loading" && runtime.phase !== "offline" ? (
        <p className="pet-window__notice">当前后端未提供 Active Run 发现，外部对话不会同步到桌宠。</p>
      ) : null}
    </main>
  );
}

export function PetWindow(props: PetWindowProps) {
  return isTauri() ? <ConnectedPetWindow {...props} /> : <PetDesktopUnavailable />;
}
