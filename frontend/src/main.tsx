import React from "react";
import ReactDOM from "react-dom/client";
import { App } from "./App";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { loadDesktopFrontendRuntimeConfig } from "./config/runtimeConfig/desktopBridge";
import { PetWindow } from "./features/pet/PetWindow";
import { isPetWindowRoute } from "./features/pet/route";
import "./styles.css";

// Surface unhandled rejections to the console so they are never silently lost.
window.addEventListener("unhandledrejection", (event) => {
  console.error("Unhandled promise rejection:", event.reason);
});

const RootView = isPetWindowRoute() ? PetWindow : App;
const rootElement = document.getElementById("root")!;

async function startFrontend(): Promise<void> {
  try {
    await loadDesktopFrontendRuntimeConfig();
  } catch {
    console.error("Desktop startup configuration could not be loaded.");
    ReactDOM.createRoot(rootElement).render(
      <div role="alert" style={{ padding: "24px", fontFamily: "sans-serif" }}>
        <strong>启动配置加载失败</strong>
        <p>请检查桌面应用的运行时配置并重新启动 SynthChat。</p>
      </div>,
    );
    return;
  }

  ReactDOM.createRoot(rootElement).render(
    <React.StrictMode>
      <ErrorBoundary>
        <RootView />
      </ErrorBoundary>
    </React.StrictMode>,
  );
}

void startFrontend();
