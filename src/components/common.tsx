import { ChevronRight, type LucideIcon } from "lucide-react";
import { useEffect, useState, type ImgHTMLAttributes, type ReactNode } from "react";
import { api, isTauri } from "../lib/api";
import { localImagePreviewEntry } from "../lib/localImagePreview";

type LocalAssetImageProps = Omit<ImgHTMLAttributes<HTMLImageElement>, "src"> & {
  src?: string;
  fallback?: ReactNode;
};

export function LocalAssetImage({ src, fallback = null, onError, ...props }: LocalAssetImageProps) {
  const preview = localImagePreviewEntry(src);
  const [renderSrc, setRenderSrc] = useState(src ? preview?.dataUrl || api.assetUrl(src) : "");
  const [fallbackTried, setFallbackTried] = useState(false);
  const [finalFailed, setFinalFailed] = useState(false);

  useEffect(() => {
    const initialSrc = src ? preview?.dataUrl || api.assetUrl(src) : "";
    setRenderSrc(initialSrc);
    setFallbackTried(false);
    setFinalFailed(false);
    if (!src || preview?.dataUrl || !isTauri() || /^(data:|blob:|https?:)/i.test(src)) {
      return;
    }
    let cancelled = false;
    void api.localAssetDataUrl(src)
      .then((dataUrl: string) => {
        if (!cancelled && dataUrl) setRenderSrc(dataUrl);
      })
      .catch(() => {
        // Keep the asset-protocol URL as the first fallback.
      });
    return () => {
      cancelled = true;
    };
  }, [src, preview?.version]);

  if (!src || finalFailed) return <>{fallback}</>;
  return (
    <img
      {...props}
      src={renderSrc}
      onError={(event) => {
        const isLocalSource = !/^(data:|blob:|https?:)/i.test(src);
        if (!fallbackTried && src && isLocalSource && !/^data:/i.test(renderSrc)) {
          setFallbackTried(true);
          void api.localAssetDataUrl(src)
            .then((dataUrl: string) => {
              if (dataUrl) setRenderSrc(dataUrl);
              else {
                setFinalFailed(true);
                onError?.(event);
              }
            }).catch(() => {
              setFinalFailed(true);
              onError?.(event);
            });
          return;
        }
        onError?.(event);
        setFinalFailed(true);
      }}
    />
  );
}

export function Avatar({ name, src, size }: { name: string; src?: string; size?: "large" }) {
  const fallback = (name || "?").slice(0, 1).toUpperCase();
  return (
    <span className={size === "large" ? "avatar-chip avatar-large" : "avatar-chip"}>
      {src ? <LocalAssetImage alt="" src={src} fallback={fallback} /> : fallback}
    </span>
  );
}

export function MenuRow({
  icon: Icon,
  label,
  value,
  onClick,
  iconColor
}: {
  icon: LucideIcon;
  label: string;
  value?: string;
  onClick?: () => void;
  iconColor?: "green" | "orange" | "blue" | "red" | "indigo" | "yellow" | "cyan" | "peach" | "purple" | "neutral" | "primary";
}) {
  return (
    <button className="menu-row" onClick={onClick} type="button">
      <span className={`row-icon ${iconColor || "primary"}`}><Icon size={17} /></span>
      <span className="row-label">{label}</span>
      {value ? <span className="row-value">{value}</span> : null}
      <ChevronRight size={16} strokeWidth={2} style={{ color: "var(--text-3)", opacity: 0.6 }} />
    </button>
  );
}
