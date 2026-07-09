import React from "react";
import ReactDOM from "react-dom/client";
import { App } from "./App";
import { PetWindow } from "./PetWindow";
import { ErrorBoundary } from "./components/ErrorBoundary";
import "./styles.css";

// Surface unhandled rejections to the console so they are never silently lost.
window.addEventListener("unhandledrejection", (event) => {
  console.error("Unhandled promise rejection:", event.reason);
});

const params = new URLSearchParams(window.location.search);
const Root = params.get("window") === "pet" ? PetWindow : App;

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <ErrorBoundary>
      <Root />
    </ErrorBoundary>
  </React.StrictMode>
);
