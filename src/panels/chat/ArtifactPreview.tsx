import { memo } from "react";
import { FileText, X } from "lucide-react";
import { LocalAssetImage } from "../../components/common";
import { api } from "../../lib/api";
import type { ArtifactTarget } from "../../lib/messageRenderUtils";

export const ArtifactPreview = memo(function ArtifactPreview({ target, onClose }: { target: ArtifactTarget; onClose: () => void }) {
  const isImage = target.kind === "image";
  return (
    <div className="claw-artifact-backdrop" onClick={onClose} role="presentation">
      <div className="claw-artifact-dialog" onClick={(event) => event.stopPropagation()} role="dialog" aria-modal="true">
        <div className="claw-artifact-dialog-head">
          <div>
            <span>{target.source}</span>
            <strong>{target.title}</strong>
          </div>
          <div>
            <button onClick={() => void api.openLocalFile(target.path)} type="button">打开</button>
            <button onClick={() => void api.revealLocalFile(target.path)} type="button">定位</button>
            <button onClick={onClose} title="关闭" type="button"><X size={15} /></button>
          </div>
        </div>
        {isImage ? (
          <LocalAssetImage src={target.path} alt={target.title} />
        ) : (
          <div className="claw-artifact-file">
            <FileText size={42} />
            <code>{target.path}</code>
            <p>该文件可通过系统应用打开，或在文件管理器中定位。</p>
          </div>
        )}
      </div>
    </div>
  );
});
