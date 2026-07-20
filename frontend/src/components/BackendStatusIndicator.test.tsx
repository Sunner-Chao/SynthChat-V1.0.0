// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  BACKEND_SERVICE_NAME,
  BackendApiError,
  type BackendApiClient,
} from "../api/backend";
import {
  BackendStatusIndicator,
  BackendStatusView,
  backendStatusPresentation,
  type BackendStatusSnapshot,
} from "./BackendStatusIndicator";

afterEach(() => cleanup());

describe("BackendStatusView", () => {
  it("renders a stable initial checking state", () => {
    const snapshot: BackendStatusSnapshot = {
      phase: "checking",
      health: null,
      error: null,
    };
    const markup = renderToStaticMarkup(
      <BackendStatusView onRefresh={() => undefined} snapshot={snapshot} />,
    );

    expect(markup).toContain("backend-status--checking");
    expect(markup).toContain("后端检测中");
    expect(markup).toContain("aria-busy=\"true\"");
    expect(markup).toContain("aria-label=\"后端检测中\"");
    expect(markup).toContain("disabled");
  });

  it("shows the validated backend version when online", () => {
    const snapshot: BackendStatusSnapshot = {
      phase: "online",
      health: {
        status: "ok",
        service: BACKEND_SERVICE_NAME,
        version: "0.1.0",
      },
      error: null,
    };
    const presentation = backendStatusPresentation(snapshot);
    const markup = renderToStaticMarkup(
      <BackendStatusView onRefresh={() => undefined} snapshot={snapshot} />,
    );

    expect(presentation.title).toContain("0.1.0");
    expect(markup).toContain("backend-status--online");
    expect(markup).toContain("后端在线");
    expect(markup).not.toContain("disabled");
  });

  it("keeps an offline retry control visible", () => {
    const snapshot: BackendStatusSnapshot = {
      phase: "offline",
      health: null,
      error: new BackendApiError("network", "Local service is unavailable."),
    };
    const markup = renderToStaticMarkup(
      <BackendStatusView onRefresh={() => undefined} snapshot={snapshot} />,
    );

    expect(markup).toContain("backend-status--offline");
    expect(markup).toContain("后端未连接");
    expect(markup).toContain("点击重新检查");
  });

  it("checks health on mount and refreshes an online snapshot on click", async () => {
    const getHealth = vi.fn(async () => ({
      status: "ok" as const,
      service: BACKEND_SERVICE_NAME,
      version: "0.2.0",
    }));
    const client: BackendApiClient = {
      baseUrl: "http://127.0.0.1:8642",
      getHealth,
    };
    render(<BackendStatusIndicator client={client} pollIntervalMs={60_000} />);

    const online = await screen.findByRole("button", { name: /后端在线/ });
    expect(online.getAttribute("title")).toContain("0.2.0");

    fireEvent.click(online);
    await waitFor(() => expect(getHealth).toHaveBeenCalledTimes(2));
    expect(screen.getByRole("button", { name: /后端在线/ })).toBeTruthy();
  });

  it.each([
    new BackendApiError("network", "connection refused"),
    new Error("unexpected failure"),
  ])("maps a rejected health check to an interactive offline state", async (error) => {
    const getHealth = vi.fn(async () => {
      throw error;
    });
    const client: BackendApiClient = {
      baseUrl: "http://127.0.0.1:8642",
      getHealth,
    };
    render(<BackendStatusIndicator client={client} pollIntervalMs={60_000} />);

    const offline = await screen.findByRole("button", { name: /后端未连接/ });
    expect(offline).not.toHaveProperty("disabled", true);
    fireEvent.click(offline);
    await waitFor(() => expect(getHealth).toHaveBeenCalledTimes(2));
  });

  it("passes an explicit health timeout to the client", async () => {
    const getHealth = vi.fn(async () => ({
      status: "ok" as const,
      service: BACKEND_SERVICE_NAME,
      version: "0.2.0",
    }));
    const client: BackendApiClient = {
      baseUrl: "http://127.0.0.1:8642",
      getHealth,
    };

    render(
      <BackendStatusIndicator
        client={client}
        healthTimeoutMs={2750}
        pollIntervalMs={60_000}
      />,
    );

    await screen.findByRole("button", { name: /后端在线/ });
    expect(getHealth).toHaveBeenCalledWith({
      signal: expect.any(AbortSignal),
      timeoutMs: 2750,
    });
  });
});
