import { isTauri } from "@tauri-apps/api/core";
import { Sparkles } from "lucide-react";
import { useState } from "react";
import { desktopPetWindow } from "./desktopPet";

export function DesktopPetToggle() {
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);

  if (!isTauri()) return null;

  const togglePet = async () => {
    if (pending) return;
    setPending(true);
    setError(null);
    try {
      await desktopPetWindow.toggle();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "无法切换桌宠窗口。");
    } finally {
      setPending(false);
    }
  };

  return (
    <div className="workspace-pet-control">
      <button
        aria-label="切换桌宠"
        className="workspace-pet-toggle"
        disabled={pending}
        onClick={() => void togglePet()}
        title="切换桌宠"
        type="button"
      >
        <Sparkles aria-hidden="true" size={16} />
      </button>
      {error ? <span className="workspace-pet-error" role="alert">桌宠不可用</span> : null}
    </div>
  );
}
