/**
 * Shared primitives used across all Settings sub-panels.
 * Import from here rather than duplicating in each panel file.
 */

import { useState } from "react";
import { ChevronRight, Eye, EyeOff } from "lucide-react";

export function BackBtn({ onBack }: { onBack?: () => void }) {
  if (!onBack) return null;
  return (
    <button className="icon-only-btn" onClick={onBack} title="返回" type="button">
      <ChevronRight size={19} style={{ transform: "rotate(180deg)" }} />
    </button>
  );
}

export function SecretInput({
  value,
  onChange,
  placeholder,
  autoComplete = "off",
}: {
  value: string;
  onChange: (value: string) => void;
  placeholder?: string;
  autoComplete?: string;
}) {
  const [visible, setVisible] = useState(false);
  return (
    <div className="secret-input-row">
      <input
        autoComplete={autoComplete}
        type={visible ? "text" : "password"}
        value={value}
        onChange={(event) => onChange(event.target.value)}
        placeholder={placeholder}
      />
      <button
        aria-label={visible ? "隐藏密钥" : "显示密钥"}
        className="secret-toggle-btn"
        onClick={() => setVisible((current) => !current)}
        title={visible ? "隐藏" : "显示"}
        type="button"
      >
        {visible ? <EyeOff size={16} /> : <Eye size={16} />}
      </button>
    </div>
  );
}
