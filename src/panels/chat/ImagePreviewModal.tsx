import { memo, useEffect, useRef } from "react";
import { X } from "lucide-react";
import { LocalAssetImage } from "../../components/common";
import { api } from "../../lib/api";
import { fileNameFromPath } from "../../lib/emojiUtils";

export const ImagePreviewModal = memo(function ImagePreviewModal({ src, onClose }: { src: string; onClose: () => void }) {
  const closeButtonRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => { if (e.key === "Escape") onClose(); };
    window.addEventListener("keydown", handler);
    // Move focus into the dialog when it opens so keyboard/screen-reader
    // users can interact with it immediately.
    closeButtonRef.current?.focus();
    return () => window.removeEventListener("keydown", handler);
  }, [onClose]);

  const isLocalPath = !src.startsWith("http://") && !src.startsWith("https://");

  return (
    <div className="image-preview-backdrop" onClick={onClose} role="presentation">
      <div
        className="image-preview-dialog"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label={fileNameFromPath(src)}
      >
        <div className="image-preview-head">
          <strong>{fileNameFromPath(src)}</strong>
          <div>
            {isLocalPath && (
              <button onClick={() => void api.openLocalFile(src)} type="button">打开</button>
            )}
            <button ref={closeButtonRef} onClick={onClose} title="关闭" type="button"><X size={15} /></button>
          </div>
        </div>
        <LocalAssetImage
          src={src}
          alt={fileNameFromPath(src)}
          onError={(e) => { (e.currentTarget as HTMLImageElement).alt = "图片加载失败"; }}
        />
      </div>
    </div>
  );
});
