import type { EmojiGroup } from "./types";

export type EmojiPathIndexes = {
  byPath: Map<string, string>;
  byFile: Map<string, string>;
};

export function fileNameFromLocalPath(path: string) {
  return path.split(/[\\/]/).pop() || "attachment";
}

export function fileNameFromPath(path: string) {
  return path.split(/[\\/]/).pop() || path;
}

export function normalizeEmojiPathKey(path: string): string {
  return path.replace(/\//g, "\\").toLowerCase();
}

export function isEmojiAssetPath(path: string): boolean {
  return normalizeEmojiPathKey(path).includes("\\emoji\\");
}

export function buildEmojiPathIndexes(groups: EmojiGroup[]): EmojiPathIndexes {
  const byPath = new Map<string, string>();
  const byFile = new Map<string, string>();
  for (const group of groups) {
    const imagePaths = Object.values(group.emotionImages ?? {}).flat();
    const candidates = imagePaths.length > 0 ? imagePaths : group.images;
    for (const imagePath of candidates) {
      byPath.set(normalizeEmojiPathKey(imagePath), imagePath);
      const normalized = normalizeEmojiPathKey(imagePath);
      const markerIndex = normalized.indexOf("\\emoji\\");
      if (markerIndex < 0) continue;
      const segments = normalized
        .slice(markerIndex + "\\emoji\\".length)
        .split("\\")
        .filter(Boolean);
      if (segments.length < 3) continue;
      const [groupId, emotionId, fileName] = segments;
      byFile.set(`${groupId}::${emotionId}::${fileName}`, imagePath);
    }
  }
  return { byPath, byFile };
}

export function repairEmojiAssetPath(path: string, indexes: EmojiPathIndexes): string {
  const normalized = normalizeEmojiPathKey(path);
  const exact = indexes.byPath.get(normalized);
  if (exact) return exact;
  const marker = "\\emoji\\";
  const markerIndex = normalized.indexOf(marker);
  if (markerIndex < 0) return path;
  const segments = normalized
    .slice(markerIndex + marker.length)
    .split("\\")
    .filter(Boolean);
  if (segments.length < 3) return path;
  const [groupId, emotionId, fileName] = segments;
  return indexes.byFile.get(`${groupId}::${emotionId}::${fileName}`) ?? path;
}
