import { ChangeEvent, useRef, useState } from "react";
import { Palette, Plus, Upload } from "lucide-react";
import { api } from "../../lib/api";
import type { ThemeConfig } from "../../lib/types";
import { BackBtn } from "./_shared";

export function ThemeSettings({
  onBack,
  themes,
  importThemeCss,
  saveThemes,
}: {
  onBack?: () => void;
  themes: ThemeConfig[];
  importThemeCss: (file: File) => Promise<void>;
  saveThemes: (themes: ThemeConfig[]) => Promise<void>;
}) {
  const [mode, setMode] = useState<"light" | "dark" | "auto">(themes[0]?.mode ?? "light");
  const [exportPath, setExportPath] = useState("");
  const themeInput = useRef<HTMLInputElement | null>(null);

  const add = () => {
    const now = new Date().toISOString();
    void saveThemes([
      ...themes,
      { id: crypto.randomUUID(), name: "新主题", mode, active: false, css: "", createdAt: now, updatedAt: now },
    ]);
  };

  const onThemeFile = async (event: ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    event.target.value = "";
    if (file) await importThemeCss(file);
  };

  const exportCss = async () => {
    setExportPath(
      await api.exportThemesCss(themes.filter((t) => t.active).map((t) => t.id)),
    );
  };

  return (
    <div className="primary-panel embedded-panel">
      <div className="panel-title action-title">
        <BackBtn onBack={onBack} />
        <div className="panel-title-text"><span>Theme</span><strong>主题</strong></div>
        <button onClick={add} type="button"><Plus size={15} />新建</button>
      </div>
      <input accept=".css,text/css" className="hidden-input" onChange={onThemeFile} ref={themeInput} type="file" />

      <div className="card" style={{ margin: "0 16px 12px" }}>
        <div className="card-header">外观模式</div>
        {(["light", "dark", "auto"] as const).map((item) => (
          <button
            className="card-row clickable-row theme-mode-row theme-mode-active"
            key={item}
            type="button"
            onClick={() => {
              setMode(item);
              void saveThemes(themes.map((t) => ({ ...t, mode: item })));
            }}
          >
            <span className={`row-icon ${mode === item ? "primary" : "cyan"}`}><Palette size={18} /></span>
            <span className="row-label">{item === "light" ? "浅色" : item === "dark" ? "深色" : "跟随系统"}</span>
            {mode === item ? <span className="check-mark">✓</span> : null}
          </button>
        ))}
      </div>

      <div className="card" style={{ margin: "0 16px 12px" }}>
        <div className="card-header">主题操作</div>
        <div className="form-actions-horizontal">
          <button className="btn-secondary-outline" type="button" onClick={() => themeInput.current?.click()}>
            <Upload size={15} />导入 CSS
          </button>
          <button className="btn-secondary-outline" type="button" onClick={() => void exportCss()}>
            导出当前主题
          </button>
        </div>
      </div>

      {exportPath ? <p className="form-hint panel-hint">已导出：{exportPath}</p> : null}

      <div className="theme-list">
        {themes.map((theme, index) => (
          <div className="card theme-card" key={`${theme.name}-${index}`}>
            <div className="theme-header">
              <div className="theme-info">
                <strong>{theme.name}</strong>
                <span className="theme-meta">
                  {theme.active ? "正在应用" : "可用主题"} · {theme.css ? "自定义 CSS" : "默认样式"}
                </span>
              </div>
              <div className="theme-actions">
                <button
                  className={theme.active ? "btn-secondary-outline" : "btn-primary-outline"}
                  type="button"
                  onClick={() => void saveThemes(themes.map((item, i) => (
                    i === index ? { ...item, active: !item.active, mode } : item
                  )))}
                >
                  {theme.active ? "移出" : "应用"}
                </button>
                <button
                  className="btn-danger-outline-sm"
                  type="button"
                  onClick={() => void saveThemes(themes.filter((_, i) => i !== index))}
                >
                  删除
                </button>
              </div>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
