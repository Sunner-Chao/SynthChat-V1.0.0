import {
  FrontendRuntimeConfigError,
  readIntegerSetting,
  readStringSetting,
  runtimeConfigSection,
} from "./common";

export interface PetBuildEnvironment {
  BASE_URL?: unknown;
  VITE_SYNTHCHAT_PET_FRAME_URL?: unknown;
  VITE_SYNTHCHAT_PET_MODEL_URL?: unknown;
  VITE_SYNTHCHAT_PET_STATUS_POLL_INTERVAL_MS?: unknown;
}

type PetImportMeta = ImportMeta & { env?: PetBuildEnvironment };

export interface PetRuntimeConfig {
  frameUrl: string;
  modelUrl: string;
  statusPollIntervalMs: number;
}

const DEFAULT_FRAME_PATH = "pet/index.html";
const DEFAULT_MODEL_PATH = "pet/model/Hiyori/Hiyori.model3.json";
const URL_SENTINEL = "https://synthchat.invalid/";

export const DEFAULT_PET_RUNTIME_CONFIG: Readonly<PetRuntimeConfig> = Object.freeze({
  frameUrl: `/${DEFAULT_FRAME_PATH}`,
  modelUrl: `/${DEFAULT_MODEL_PATH}`,
  statusPollIntervalMs: 5_000,
});

function sameOriginResourceUrl(
  value: string,
  baseUrl: string,
  setting: string,
  expectedSuffix: string,
): string {
  if (value.length > 2_048 || /[\\\u0000-\u001f\u007f]/u.test(value)) {
    throw new FrontendRuntimeConfigError(`${setting} must be a valid same-origin resource path.`);
  }

  let resolvedBase: URL;
  let resolved: URL;
  try {
    resolvedBase = new URL(baseUrl, URL_SENTINEL);
    resolved = new URL(value, resolvedBase);
  } catch (cause) {
    throw new FrontendRuntimeConfigError(
      `${setting} must be a valid same-origin resource path: ${String(cause)}`,
    );
  }
  if (resolvedBase.origin !== new URL(URL_SENTINEL).origin || resolved.origin !== resolvedBase.origin) {
    throw new FrontendRuntimeConfigError(`${setting} must be a same-origin resource path.`);
  }
  if (!resolved.pathname.endsWith(expectedSuffix)) {
    throw new FrontendRuntimeConfigError(`${setting} must reference a ${expectedSuffix} resource.`);
  }
  return `${resolved.pathname}${resolved.search}${resolved.hash}`;
}

export function resolvePetRuntimeConfig(
  runtime: Record<string, unknown> | undefined,
  overrides: Partial<PetRuntimeConfig>,
  build: PetBuildEnvironment | undefined,
): PetRuntimeConfig {
  const rawBaseUrl = build?.BASE_URL;
  const baseUrl = typeof rawBaseUrl === "string" && rawBaseUrl.trim()
    ? rawBaseUrl.trim()
    : "/";
  const frameUrl = overrides.frameUrl ?? readStringSetting({
    name: "pet.frameUrl",
    runtimeValue: runtime?.frameUrl,
    buildValue: build?.VITE_SYNTHCHAT_PET_FRAME_URL,
    defaultValue: DEFAULT_FRAME_PATH,
  });
  const modelUrl = overrides.modelUrl ?? readStringSetting({
    name: "pet.modelUrl",
    runtimeValue: runtime?.modelUrl,
    buildValue: build?.VITE_SYNTHCHAT_PET_MODEL_URL,
    defaultValue: DEFAULT_MODEL_PATH,
  });

  return {
    frameUrl: sameOriginResourceUrl(frameUrl, baseUrl, "pet.frameUrl", ".html"),
    modelUrl: sameOriginResourceUrl(modelUrl, baseUrl, "pet.modelUrl", ".model3.json"),
    statusPollIntervalMs: readIntegerSetting({
      name: "pet.statusPollIntervalMs",
      runtimeValue: runtime?.statusPollIntervalMs,
      buildValue: build?.VITE_SYNTHCHAT_PET_STATUS_POLL_INTERVAL_MS,
      defaultValue: DEFAULT_PET_RUNTIME_CONFIG.statusPollIntervalMs,
      minimum: 1_000,
      maximum: 3_600_000,
    }),
  };
}

export function readPetRuntimeConfig(
  overrides: Partial<PetRuntimeConfig> = {},
  build = (import.meta as PetImportMeta).env,
): PetRuntimeConfig {
  return resolvePetRuntimeConfig(runtimeConfigSection("pet"), overrides, build);
}
