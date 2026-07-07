import { memo, useEffect } from "react";
import { X } from "lucide-react";
import { LocalAssetImage } from "../../components/common";
import { api } from "../../lib/api";
import { fileNameFromPath } from "../../lib/emojiUtils";

export const ImagePreviewModal = memo(function ImagePreviewModal({ src, onClose }: { src: string; onClose: () => void }) {
  useEffect(() => {
    const handler = (e: KeyboardEvent) => { if (e.key === "Escape") onClose(); };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [onClose]);
  return (
    <div className="image-preview-backdrop" onClick={onClose} role="presentation">
      <div className="image-preview-dialog" onClick={(e) => e.stopPropagation()} role="dialog" aria-modal="true">
        <div className="image-preview-head">
          <strong>{fileNameFromPath(src)}</strong>
          <div>
            <button onClick={() => void api.openLocalFile(src)} type="button">打开</button>
            <button onClick={onClose} title="关闭" type="button"><X size={15} /></button>
          </div>
        </div>
        <LocalAssetImage src={src} alt={fileNameFromPath(src)} />
      </div>
    </div>
  );
});
