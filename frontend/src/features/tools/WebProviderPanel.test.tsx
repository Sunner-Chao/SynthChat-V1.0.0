// @vitest-environment jsdom

import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ProfilesApi, SecretStatus } from "../../api/profiles";
import {
  WebApiError,
  type VersionedWebConfig,
  type WebApi,
  type WebConfig,
  type WebProvider,
} from "../../api/web";
import { WebProviderPanel } from "./WebProviderPanel";

const NOW = "2026-07-17T08:00:00Z";
const PROVIDER: WebProvider = {
  id: "tavily",
  displayName: "Tavily",
  supportsSearch: true,
  supportsExtract: true,
  secretNames: ["TAVILY_API_KEY"],
  defaultBaseUrl: "https://api.tavily.com",
  customEndpointSupported: false,
};

const CONFIG: WebConfig = {
  revision: "config-1",
  sharedProvider: null,
  searchProvider: null,
  extractProvider: null,
  extractCharLimit: 15_000,
  effectiveSearch: {
    providerId: "tavily",
    status: "ready",
    missingSecretNames: [],
  },
  effectiveExtract: {
    providerId: "tavily",
    status: "missingSecret",
    missingSecretNames: ["TAVILY_API_KEY"],
  },
};

const UNCONFIGURED_SECRET: SecretStatus = {
  name: "TAVILY_API_KEY",
  configured: false,
  storage: "osKeychain",
  updatedAt: null,
};

const CONFIGURED_SECRET: SecretStatus = {
  ...UNCONFIGURED_SECRET,
  configured: true,
  updatedAt: NOW,
};

type SecretClient = Pick<
  ProfilesApi,
  "listSecretStatuses" | "putSecret" | "deleteSecret"
>;

function versioned(
  value: WebConfig = CONFIG,
  etag = `"${value.revision}"`,
): VersionedWebConfig {
  return { value, etag };
}

function webClient(overrides: Partial<WebApi> = {}): WebApi {
  return {
    listProviders: vi.fn(async () => [PROVIDER]),
    getWebConfig: vi.fn(async () => versioned()),
    updateWebConfig: vi.fn(async (_profileId, patch) => {
      const value: WebConfig = { ...CONFIG, ...patch, revision: "config-2" };
      return versioned(value);
    }),
    ...overrides,
  };
}

function secretClient(overrides: Partial<SecretClient> = {}): SecretClient {
  return {
    listSecretStatuses: vi.fn(async () => [UNCONFIGURED_SECRET]),
    putSecret: vi.fn(async () => CONFIGURED_SECRET),
    deleteSecret: vi.fn(async () => undefined),
    ...overrides,
  };
}

afterEach(cleanup);

describe("WebProviderPanel", () => {
  it("does not probe Web or secret routes while both capabilities are false", async () => {
    const web = webClient();
    const secrets = secretClient();
    render(
      <WebProviderPanel
        client={web}
        extractAvailable={false}
        profileClient={secrets}
        profileId="default"
        searchAvailable={false}
      />,
    );

    expect(screen.getByText("当前后端未启用 Web Search 或 Web Extract 能力。")).toBeTruthy();
    await Promise.resolve();
    expect(web.listProviders).not.toHaveBeenCalled();
    expect(web.getWebConfig).not.toHaveBeenCalled();
    expect(secrets.listSecretStatuses).not.toHaveBeenCalled();
  });

  it("gates Search and Extract independently and shows missing secret names", async () => {
    const web = webClient({
      getWebConfig: vi.fn(async () => versioned({
        ...CONFIG,
        effectiveSearch: {
          providerId: "tavily",
          status: "ready",
          missingSecretNames: [],
        },
        effectiveExtract: {
          providerId: "tavily",
          status: "missingSecret",
          missingSecretNames: ["TAVILY_API_KEY"],
        },
      })),
    });
    render(
      <WebProviderPanel
        client={web}
        extractAvailable
        profileClient={secretClient()}
        profileId="default"
        searchAvailable={false}
      />,
    );

    expect(await screen.findByText("缺少：TAVILY_API_KEY")).toBeTruthy();
    const searchStatus = screen.getByText("Web Search").closest(".web-readiness-item");
    const extractStatus = screen.getByText("Web Extract").closest(".web-readiness-item");
    expect(searchStatus?.textContent).toContain("后端能力不可用");
    expect(extractStatus?.textContent).toContain("缺少密钥");
    expect((screen.getByRole("combobox", {
      name: "Web Search Provider",
    }) as HTMLSelectElement).disabled).toBe(true);
    expect((screen.getByRole("combobox", {
      name: "Web Extract Provider",
    }) as HTMLSelectElement).disabled).toBe(false);
    expect(web.listProviders).toHaveBeenCalledTimes(1);
  });

  it("aborts stale Profile loads and never renders their late data", async () => {
    let defaultSignal: AbortSignal | undefined;
    let resolveDefault!: (value: VersionedWebConfig) => void;
    const getWebConfig = vi.fn((profileId: string, options?: { signal?: AbortSignal }) => {
      if (profileId === "default") {
        defaultSignal = options?.signal;
        return new Promise<VersionedWebConfig>((resolve) => {
          resolveDefault = resolve;
        });
      }
      return Promise.resolve(versioned({
        ...CONFIG,
        revision: "work-config-1",
        extractCharLimit: 22_000,
      }));
    });
    const web = webClient({ getWebConfig });
    const secrets = secretClient();
    const view = render(
      <WebProviderPanel
        client={web}
        extractAvailable
        profileClient={secrets}
        profileId="default"
        searchAvailable
      />,
    );
    await waitFor(() => expect(getWebConfig).toHaveBeenCalledWith(
      "default",
      { signal: expect.any(AbortSignal) },
    ));

    view.rerender(
      <WebProviderPanel
        client={web}
        extractAvailable
        profileClient={secrets}
        profileId="work"
        searchAvailable
      />,
    );
    await waitFor(() => expect((screen.getByRole("spinbutton", {
      name: "Web Extract 字符上限",
    }) as HTMLInputElement).value).toBe("22000"));
    expect(defaultSignal?.aborted).toBe(true);

    resolveDefault(versioned({ ...CONFIG, extractCharLimit: 99_000 }));
    await waitFor(() => expect((screen.getByRole("spinbutton", {
      name: "Web Extract 字符上限",
    }) as HTMLInputElement).value).toBe("22000"));
  });

  it("serializes provider and char-limit patches with the latest returned ETag", async () => {
    let current = CONFIG;
    let revision = 1;
    const updateWebConfig = vi.fn(async (
      _profileId: string,
      patch: Partial<WebConfig>,
    ) => {
      revision += 1;
      current = { ...current, ...patch, revision: `config-${revision}` };
      return versioned(current);
    });
    const user = userEvent.setup();
    render(
      <WebProviderPanel
        client={webClient({ updateWebConfig })}
        extractAvailable
        profileClient={secretClient()}
        profileId="default"
        searchAvailable
      />,
    );

    await user.selectOptions(
      await screen.findByRole("combobox", { name: "共享 Web Provider" }),
      "tavily",
    );
    await waitFor(() => expect(updateWebConfig).toHaveBeenNthCalledWith(
      1,
      "default",
      { sharedProvider: "tavily" },
      '"config-1"',
      { signal: expect.any(AbortSignal) },
    ));

    await user.selectOptions(screen.getByRole("combobox", {
      name: "Web Search Provider",
    }), "tavily");
    await waitFor(() => expect(updateWebConfig).toHaveBeenNthCalledWith(
      2,
      "default",
      { searchProvider: "tavily" },
      '"config-2"',
      { signal: expect.any(AbortSignal) },
    ));

    await user.selectOptions(screen.getByRole("combobox", {
      name: "Web Extract Provider",
    }), "tavily");
    await waitFor(() => expect(updateWebConfig).toHaveBeenNthCalledWith(
      3,
      "default",
      { extractProvider: "tavily" },
      '"config-3"',
      { signal: expect.any(AbortSignal) },
    ));

    const limit = screen.getByRole("spinbutton", { name: "Web Extract 字符上限" });
    await user.clear(limit);
    await user.type(limit, "20000");
    await user.click(screen.getByRole("button", { name: "保存字符上限" }));
    await waitFor(() => expect(updateWebConfig).toHaveBeenNthCalledWith(
      4,
      "default",
      { extractCharLimit: 20_000 },
      '"config-4"',
      { signal: expect.any(AbortSignal) },
    ));
  });

  it("reloads on 409, keeps a conflict notice, and does not replay the patch", async () => {
    const getWebConfig = vi.fn()
      .mockResolvedValueOnce(versioned())
      .mockResolvedValueOnce(versioned({
        ...CONFIG,
        revision: "config-current",
        sharedProvider: "tavily",
      }));
    const updateWebConfig = vi.fn(async () => {
      throw new WebApiError("http", "Configuration changed", {
        status: 409,
        code: "revision_conflict",
        requestId: "req-conflict",
        etag: '"config-current"',
      });
    });
    const user = userEvent.setup();
    render(
      <WebProviderPanel
        client={webClient({ getWebConfig, updateWebConfig })}
        extractAvailable
        profileClient={secretClient()}
        profileId="default"
        searchAvailable
      />,
    );

    await user.selectOptions(
      await screen.findByRole("combobox", { name: "共享 Web Provider" }),
      "tavily",
    );
    await waitFor(() => expect(getWebConfig).toHaveBeenCalledTimes(2));
    expect(screen.getByRole("alert").textContent).toContain("已重新加载最新状态");
    expect(updateWebConfig).toHaveBeenCalledTimes(1);
    expect((screen.getByRole("combobox", {
      name: "共享 Web Provider",
    }) as HTMLSelectElement).value).toBe("tavily");
  });

  it("stores and deletes a key without echoing it, then refreshes readiness", async () => {
    const missing = {
      ...CONFIG,
      effectiveSearch: {
        providerId: "tavily",
        status: "missingSecret" as const,
        missingSecretNames: ["TAVILY_API_KEY"],
      },
      effectiveExtract: {
        providerId: "tavily",
        status: "missingSecret" as const,
        missingSecretNames: ["TAVILY_API_KEY"],
      },
    };
    const ready = {
      ...missing,
      revision: "config-2",
      effectiveSearch: {
        providerId: "tavily",
        status: "ready" as const,
        missingSecretNames: [],
      },
      effectiveExtract: {
        providerId: "tavily",
        status: "ready" as const,
        missingSecretNames: [],
      },
    };
    const missingAgain = { ...missing, revision: "config-3" };
    const getWebConfig = vi.fn()
      .mockResolvedValueOnce(versioned(missing))
      .mockResolvedValueOnce(versioned(ready))
      .mockResolvedValueOnce(versioned(missingAgain));
    const putSecret = vi.fn(async () => CONFIGURED_SECRET);
    const deleteSecret = vi.fn(async () => undefined);
    const user = userEvent.setup();
    render(
      <WebProviderPanel
        client={webClient({ getWebConfig })}
        extractAvailable
        profileClient={secretClient({ putSecret, deleteSecret })}
        profileId="default"
        searchAvailable
      />,
    );

    const input = await screen.findByLabelText("输入 TAVILY_API_KEY");
    await user.type(input, "top-secret-value");
    await user.click(screen.getByRole("button", { name: "保存 TAVILY_API_KEY" }));
    await waitFor(() => expect(putSecret).toHaveBeenCalledWith(
      "default",
      "TAVILY_API_KEY",
      "top-secret-value",
      { signal: expect.any(AbortSignal) },
    ));
    expect(await screen.findByText("已存储于系统密钥链")).toBeTruthy();
    expect((input as HTMLInputElement).value).toBe("");
    expect(document.body.textContent).not.toContain("top-secret-value");
    expect(screen.getByText("Web Search").closest(".web-readiness-item")?.textContent)
      .toContain("已就绪");

    await user.click(screen.getByRole("button", { name: "删除 TAVILY_API_KEY" }));
    await waitFor(() => expect(deleteSecret).toHaveBeenCalledWith(
      "default",
      "TAVILY_API_KEY",
      { signal: expect.any(AbortSignal) },
    ));
    expect(await screen.findByText("未配置")).toBeTruthy();
    expect(screen.getByText("Web Search").closest(".web-readiness-item")?.textContent)
      .toContain("缺少密钥");
    expect(getWebConfig).toHaveBeenCalledTimes(3);
  });

  it("renders an API error and retries the complete load", async () => {
    const listProviders = vi.fn()
      .mockRejectedValueOnce(new WebApiError("http", "Web catalog unavailable", {
        status: 503,
        code: "web_unavailable",
        requestId: "req-web",
      }))
      .mockResolvedValueOnce([PROVIDER]);
    const user = userEvent.setup();
    render(
      <WebProviderPanel
        client={webClient({ listProviders })}
        extractAvailable
        profileClient={secretClient()}
        profileId="default"
        searchAvailable
      />,
    );

    expect(await screen.findByText(/Web catalog unavailable/)).toBeTruthy();
    expect(screen.getByRole("alert").textContent).toContain("req-web");
    await user.click(screen.getByRole("button", { name: "重新加载 Web 配置" }));
    expect(await screen.findByRole("combobox", { name: "共享 Web Provider" })).toBeTruthy();
    expect(listProviders).toHaveBeenCalledTimes(2);
  });
});
