import { memo, useEffect, useState } from "react";
import { Eye, FileText } from "lucide-react";
import { api } from "../../lib/api";
import {
  fileNameFromPath,
  isEmojiAssetPath,
  normalizeEmojiPathKey,
  repairEmojiAssetPath,
  type EmojiPathIndexes
} from "../../lib/emojiUtils";

export function ChevronIcon() {
  return <Eye size={14} />;
}

export const InlineImage = memo(function InlineImage({
  path,
  onClick,
  emojiPathIndexes
}: {
  path: string;
  onClick: (path: string) => void;
  emojiPathIndexes: EmojiPathIndexes;
}) {
  const isEmojiAsset = isEmojiAssetPath(path);
  const repairedPath = isEmojiAsset ? repairEmojiAssetPath(path, emojiPathIndexes) : path;
  const repairedKnown = emojiPathIndexes.byPath.has(normalizeEmojiPathKey(repairedPath));
  const [failedPath, setFailedPath] = useState<string | null>(null);
  useEffect(() => {
    setFailedPath(null);
  }, [repairedPath]);
  if (isEmojiAsset && !repairedKnown) return null;
  if (failedPath === repairedPath) return null;
  return (
    <div className="claw-inline-image" onClick={() => onClick(repairedPath)} role="button" tabIndex={0}
      onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") onClick(repairedPath); }}>
      <img
        src={api.assetUrl(repairedPath)}
        alt={fileNameFromPath(repairedPath)}
        loading="lazy"
        onError={() => setFailedPath(repairedPath)}
      />
    </div>
  );
});

export const InlineFile = memo(function InlineFile({ path, mimeType }: { path: string; mimeType: string }) {
  return (
    <button className="claw-inline-file" onClick={() => void api.openLocalFile(path)} type="button">
      <span><FileText size={18} /></span>
      <strong>{fileNameFromPath(path)}</strong>
      <small>{mimeType || "application/octet-stream"}</small>
    </button>
  );
});
