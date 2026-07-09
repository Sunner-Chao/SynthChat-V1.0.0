import { memo, useEffect, useRef } from "react";
import { parseMediaSegments } from "../../lib/mediaUtils";
import type { EmojiPathIndexes } from "../../lib/emojiUtils";
import { InlineImage, InlineFile } from "./InlineMedia";

export const MarkdownLite = memo(function MarkdownLite({
  text,
  onImageClick,
  streaming,
  onFirstChar,
  emojiPathIndexes
}: {
  text: string;
  onImageClick?: (path: string) => void;
  streaming?: boolean;
  onFirstChar?: () => void;
  emojiPathIndexes: EmojiPathIndexes;
}) {
  const firstCharFiredRef = useRef(false);

  useEffect(() => {
    if (!streaming) {
      firstCharFiredRef.current = false;
      return;
    }
    if (text.length > 0 && !firstCharFiredRef.current) {
      firstCharFiredRef.current = true;
      onFirstChar?.();
    }
  }, [onFirstChar, streaming, text.length]);

  const segments = parseMediaSegments(text);
  const handleClick = onImageClick ?? (() => {});
  return (
    <>
      {segments.map((seg, i) => {
        if (seg.kind === "image") {
          return <InlineImage key={`img-${seg.path}`} path={seg.path} onClick={handleClick} emojiPathIndexes={emojiPathIndexes} />;
        }
        if (seg.kind === "file") {
          return <InlineFile key={`file-${seg.path}`} path={seg.path} mimeType={seg.mimeType} />;
        }
        const raw = seg.value;
        const blocks = raw.split(/\n{2,}/);
        return blocks.map((block, j) => {
          const trimmed = block.trim();
          if (!trimmed) return null;
          if (trimmed.startsWith("```")) {
            // Prefix key with type so React never patches a <pre> into a <p>
            // or vice versa when the block boundary shifts during streaming.
            return <pre key={`pre-${i}-${j}`}>{trimmed.replace(/^```[a-zA-Z]*\n?/, "").replace(/```$/, "")}</pre>;
          }
          return <p key={`p-${i}-${j}`}>{trimmed}</p>;
        });
      })}
    </>
  );
});
