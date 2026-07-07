const LOCAL_IMAGE_PREVIEW_STORAGE_KEY = "synthchat.localImagePreviews.v1";
const MAX_PREVIEW_ITEMS = 12;
const MAX_PREVIEW_DATA_URL_LENGTH = 5_000_000;

export type LocalImagePreviewEntry = {
  dataUrl: string;
  version: number;
};

const localImagePreviewCache = new Map<string, LocalImagePreviewEntry>();

function normalizePreviewPath(path: string | null | undefined): string {
  const trimmed = String(path ?? "").trim().replace(/^["'`]+|["'`]+$/g, "");
  if (!trimmed) return "";
  let value = trimmed;
  if (/^file:\/\//i.test(value)) {
    try {
      value = decodeURIComponent(new URL(value).pathname);
    } catch {
      value = decodeURI(value.replace(/^file:\/\//i, ""));
    }
  }
  value = value.replace(/\\/g, "/").replace(/^\/([A-Za-z]:\/)/, "$1");
  return value.replace(/^([A-Za-z]):/, (_, drive: string) => `${drive.toUpperCase()}:`);
}

function loadLocalImagePreviews() {
  if (typeof window === "undefined") return;
  try {
    const raw = window.localStorage.getItem(LOCAL_IMAGE_PREVIEW_STORAGE_KEY);
    if (!raw) return;
    const entries = JSON.parse(raw) as Array<[string, string | LocalImagePreviewEntry]>;
    if (!Array.isArray(entries)) return;
    for (const [path, entry] of entries) {
      const key = normalizePreviewPath(path);
      const dataUrl = typeof entry === "string" ? entry : entry?.dataUrl;
      const version = entry && typeof entry === "object" && Number.isFinite(entry.version) ? entry.version : Date.now();
      if (key && typeof dataUrl === "string" && dataUrl.startsWith("data:image/")) {
        localImagePreviewCache.set(key, { dataUrl, version });
      }
    }
  } catch {
    // Ignore corrupt preview cache; it is only an optional UI fallback.
  }
}

function persistLocalImagePreviews() {
  if (typeof window === "undefined") return;
  try {
    const entries = Array.from(localImagePreviewCache.entries())
      .filter(([, entry]) => entry.dataUrl.length <= MAX_PREVIEW_DATA_URL_LENGTH)
      .slice(-MAX_PREVIEW_ITEMS);
    window.localStorage.setItem(LOCAL_IMAGE_PREVIEW_STORAGE_KEY, JSON.stringify(entries));
  } catch {
    // Ignore quota/private-mode failures.
  }
}

loadLocalImagePreviews();

export function rememberLocalImagePreview(path: string | null | undefined, dataUrl: string) {
  const key = normalizePreviewPath(path);
  if (!key || !dataUrl.startsWith("data:image/")) return;
  localImagePreviewCache.set(key, { dataUrl, version: Date.now() });
  if (localImagePreviewCache.size > MAX_PREVIEW_ITEMS) {
    const first = localImagePreviewCache.keys().next().value;
    if (first) localImagePreviewCache.delete(first);
  }
  persistLocalImagePreviews();
}

export function forgetLocalImagePreview(path: string | null | undefined) {
  const key = normalizePreviewPath(path);
  if (key) {
    localImagePreviewCache.delete(key);
    persistLocalImagePreviews();
  }
}

export function localImagePreview(path: string | null | undefined): string {
  return localImagePreviewEntry(path)?.dataUrl ?? "";
}

export function localImagePreviewEntry(path: string | null | undefined): LocalImagePreviewEntry | null {
  const key = normalizePreviewPath(path);
  return key ? localImagePreviewCache.get(key) ?? null : null;
}
