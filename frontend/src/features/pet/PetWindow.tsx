import { useCallback, useEffect, useRef, useState } from "react";
import {
  AlertTriangle,
  Grip,
  LoaderCircle,
  RefreshCw,
  ServerCog,
  Sparkles,
  Wrench,
} from "lucide-react";
import { isTauri } from "@tauri-apps/api/core";
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
type PetFrameMessage = {
  source?: string;
  type?: string;
  message?: string;
};

export interface PetWindowProps {
  bridge?: PetWindowBridge;
  runtimeApis?: PetRuntimeApis;
  pollIntervalMs?: number;
  frameUrl?: string;
  modelUrl?: string;
}

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

function ConnectedPetWindow({
  bridge = desktopPetWindow,
  frameUrl,
  modelUrl,
  runtimeApis,
  pollIntervalMs,
}: PetWindowProps) {
  const petResources = readPetRuntimeConfig({ frameUrl, modelUrl });
  const frameRef = useRef<HTMLIFrameElement>(null);
  const modelReadyRef = useRef(false);
  const [modelError, setModelError] = useState<string | null>(null);
  const [dragError, setDragError] = useState<string | null>(null);
  const runtime = usePetRuntimeStatus({
    apis: runtimeApis,
    pollIntervalMs: pollIntervalMs ?? petResources.statusPollIntervalMs,
  });

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
      setDragError(error instanceof Error ? error.message : "无法拖动桌宠窗口。");
    });
  }, [bridge]);

  useEffect(() => {
    document.documentElement.classList.add("pet-window-route");
    void bridge.setIgnoreCursorEvents(false).catch(() => undefined);
    return () => {
      document.documentElement.classList.remove("pet-window-route");
      void bridge.setIgnoreCursorEvents(false).catch(() => undefined);
    };
  }, [bridge]);

  useEffect(() => {
    if (!modelReadyRef.current) return;
    postToModel({ type: "behavior", name: behaviorForPhase(runtime.phase) });
  }, [postToModel, runtime.phase]);

  useEffect(() => {
    const handleFrameMessage = (event: MessageEvent<PetFrameMessage>) => {
      if (event.origin !== window.location.origin || event.source !== frameRef.current?.contentWindow) return;
      const message = event.data;
      if (!message || message.source !== PET_FRAME_SOURCE) return;

      if (message.type === "ready") {
        postToModel({ type: "load", url: petResources.modelUrl });
      } else if (message.type === "loaded") {
        modelReadyRef.current = true;
        setModelError(null);
        postToModel({ type: "behavior", name: behaviorForPhase(runtime.phase) });
      } else if (message.type === "error") {
        modelReadyRef.current = false;
        setModelError(message.message?.trim() || "桌宠模型无法加载。");
      } else if (message.type === "model_drag_start") {
        startDragging();
      }
    };

    window.addEventListener("message", handleFrameMessage as EventListener);
    return () => window.removeEventListener("message", handleFrameMessage as EventListener);
  }, [petResources.modelUrl, postToModel, runtime.phase, startDragging]);

  const onFrameLoad = () => {
    postToModel({ type: "load", url: petResources.modelUrl });
  };
  const displayError = modelError ?? dragError;
  const preview = runtime.latestDelta.trim();

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

        <div className={`pet-window__cloud ${phaseClassName(runtime.phase)}`} role="status" aria-live="polite">
          <span className="pet-window__cloud-indicator" aria-hidden="true">
            {runtime.phase === "thinking" || runtime.phase === "tool" || runtime.phase === "loading" ? (
              <LoaderCircle size={14} />
            ) : runtime.phase === "offline" ? (
              <AlertTriangle size={14} />
            ) : runtime.phase === "approval" || runtime.phase === "clarification" ? (
              <Wrench size={14} />
            ) : (
              <Sparkles size={14} />
            )}
          </span>
          <div>
            <strong>{runtime.title}</strong>
            <span>{runtime.detail}</span>
          </div>
        </div>

        {preview ? (
          <p className="pet-window__preview" title={preview}>
            {preview}
          </p>
        ) : null}
      </section>

      <section className="pet-window__control-strip" aria-label="桌宠控制">
        <div className="pet-window__profile" title={runtime.profile?.displayName ?? "尚未连接 Profile"}>
          <ServerCog aria-hidden="true" size={14} />
          <span>{runtime.profile?.displayName ?? "未连接 Profile"}</span>
          {runtime.stale ? <small>信号重连中</small> : null}
        </div>

        <div className="pet-window__actions">
          <button
            aria-label="刷新桌宠状态"
            className="pet-window__icon-button"
            onClick={runtime.refresh}
            title="刷新状态"
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

      {displayError ? (
        <p className="pet-window__error" role="alert">{displayError}</p>
      ) : null}

      {!runtime.runTrackingAvailable && runtime.phase !== "loading" && runtime.phase !== "offline" ? (
        <p className="pet-window__notice">
          当前后端未提供 Active Run 发现，桌宠不会显示实时对话状态。
        </p>
      ) : null}
    </main>
  );
}

export function PetWindow(props: PetWindowProps) {
  return isTauri() ? <ConnectedPetWindow {...props} /> : <PetDesktopUnavailable />;
}
