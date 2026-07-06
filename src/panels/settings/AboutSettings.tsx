import { useCallback, useEffect, useRef, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { Info, RefreshCw, Sparkles } from "lucide-react";
import { api } from "../../lib/api";
import { formatTime } from "../../lib/formatters";
import type { AppBuildInfo, AppUpdateCheck } from "../../lib/types";
import { BackBtn } from "./_shared";
import { MenuRow } from "../../components/common";

const UPDATE_MANIFEST_STORAGE_KEY = "synthchat.update.manifest.url.v1";

function isSilentInstallAssetUrl(value?: string | null) {
  return /\.(exe|msi|msix)(?:[?#].*)?$/i.test(value ?? "");
}

export function readUpdateManifestUrl(): string {
  if (typeof window === "undefined") return "";
  try {
    return window.localStorage.getItem(UPDATE_MANIFEST_STORAGE_KEY) ?? "";
  } catch {
    return "";
  }
}

export function writeUpdateManifestUrl(value: string) {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(UPDATE_MANIFEST_STORAGE_KEY, value.trim());
  } catch {
    // ignore storage errors
  }
}

export function AboutSettings({
  onBack,
  setView,
}: {
  onBack?: () => void;
  setView: (view: "privacy" | "statement") => void;
}) {
  const [appVersion, setAppVersion] = useState("");
  const [buildInfo, setBuildInfo] = useState<AppBuildInfo | null>(null);
  const [manifestUrl, setManifestUrl] = useState(readUpdateManifestUrl);
  const [updateStatus, setUpdateStatus] = useState("未检查");
  const [updateDetail, setUpdateDetail] = useState("");
  const [checking, setChecking] = useState(false);
  const [installingUpdate, setInstallingUpdate] = useState(false);
  const [availableUpdate, setAvailableUpdate] = useState<AppUpdateCheck | null>(null);
  const autoCheckedRef = useRef(false);

  useEffect(() => {
    let cancelled = false;
    void api.getAppBuildInfo().then((info: AppBuildInfo) => {
      if (cancelled) return;
      setBuildInfo(info);
      setAppVersion(`V${info.version}`);
      if (!readUpdateManifestUrl() && info.updateManifestUrl) {
        setManifestUrl(info.updateManifestUrl);
      }
    }).catch(() => {
      void getVersion().then((version) => {
        if (!cancelled) setAppVersion(`V${version}`);
      }).catch(() => {
        if (!cancelled) setAppVersion("V1.1.0");
      });
    });
    return () => { cancelled = true; };
  }, []);

  const checkUpdates = useCallback(async (urlOverride?: string) => {
    const url = (urlOverride ?? manifestUrl).trim();
    if (!url) {
      setUpdateStatus("未配置更新源");
      setUpdateDetail("请填写可访问的版本清单地址，或在构建时注入 SYNTHCHAT_UPDATE_MANIFEST_URL。");
      setAvailableUpdate(null);
      return;
    }
    setChecking(true);
    setUpdateStatus("正在检查更新...");
    setUpdateDetail("");
    try {
      const result = await api.checkAppUpdate(url) as AppUpdateCheck;
      const normalizedUrl = result.sourceUrl?.trim() || url;
      writeUpdateManifestUrl(normalizedUrl);
      if (normalizedUrl !== manifestUrl) setManifestUrl(normalizedUrl);
      if (result.updateAvailable) {
        setAvailableUpdate(result);
        setUpdateStatus(`发现新版本 ${result.latestVersion}`);
        const detail = result.notes?.trim()
          || (result.publishedAt ? `发布时间 ${formatTime(result.publishedAt)}` : "可点击下方按钮打开下载页。");
        setUpdateDetail(detail);
      } else {
        setAvailableUpdate(null);
        setUpdateStatus("已经是最新版本");
        const checked = result.checkedAt ? `，检查时间 ${formatTime(result.checkedAt)}` : "";
        setUpdateDetail(`当前 ${result.currentVersion}，远端 ${result.latestVersion}${checked}`);
      }
    } catch (error) {
      const message = String(error).replace(/^bad request:\s*/i, "");
      setAvailableUpdate(null);
      if (message.includes("not configured")) {
        setUpdateStatus("未配置更新源");
        setUpdateDetail("请填写可访问的版本清单地址，或在构建时注入 SYNTHCHAT_UPDATE_MANIFEST_URL。");
      } else {
        setUpdateStatus("检查失败");
        setUpdateDetail(message);
      }
    } finally {
      setChecking(false);
    }
  }, [manifestUrl]);

  useEffect(() => {
    if (autoCheckedRef.current) return;
    const url = (manifestUrl || buildInfo?.updateManifestUrl || "").trim();
    if (!url) return;
    autoCheckedRef.current = true;
    void checkUpdates(url);
  }, [buildInfo?.updateManifestUrl, checkUpdates, manifestUrl]);

  const saveManifestUrl = () => {
    writeUpdateManifestUrl(manifestUrl);
    setUpdateStatus("更新源已保存");
    setUpdateDetail("之后进入关于页会自动检查该地址。");
  };

  const openUpdateUrl = async () => {
    const target = availableUpdate?.downloadUrl || availableUpdate?.releaseUrl || manifestUrl.trim();
    if (!target) return;
    try {
      await api.openAppUpdateUrl(target);
    } catch {
      window.open(target, "_blank", "noopener,noreferrer");
    }
  };

  const installUpdateSilently = async () => {
    const target = availableUpdate?.downloadUrl;
    if (!isSilentInstallAssetUrl(target)) {
      setUpdateStatus("无法自动安装");
      setUpdateDetail("当前更新源没有可静默安装的 .exe、.msi 或 .msix 资产，请打开下载页手动安装。");
      return;
    }
    const confirmed = window.confirm("将下载新版本安装包，随后关闭 SynthChat 并静默安装。是否继续？");
    if (!confirmed) return;
    setInstallingUpdate(true);
    setUpdateStatus("正在下载更新安装包...");
    setUpdateDetail("下载完成后应用会自动关闭，并在后台执行安装器。");
    try {
      await api.installAppUpdate(target);
      setUpdateDetail("安装器已启动，SynthChat 即将关闭。");
    } catch (error) {
      setInstallingUpdate(false);
      setUpdateStatus("自动安装失败");
      setUpdateDetail(String(error).replace(/^bad request:\s*/i, ""));
    }
  };

  return (
    <div className="primary-panel embedded-panel about-panel">
      <div className="panel-title action-title" style={{ width: "100%", marginBottom: 4 }}>
        <BackBtn onBack={onBack} />
        <div className="panel-title-text"><span>About</span><strong>关于 SynthChat</strong></div>
      </div>
      <div className="about-hero">
        <div className="brand-mark about-logo"><Sparkles size={32} /></div>
        <h2>SynthChat</h2>
        <p className="about-version">{appVersion}</p>
        <p className="about-subtitle">智能 AI 聊天机器人</p>
      </div>

      <div className="about-section">
        <div className="about-section-title"><RefreshCw size={14} /><span>应用更新</span></div>
        <div className="menu-card flat-card about-card">
          <div className="settings-form" style={{ padding: "12px 14px" }}>
            <label style={{ display: "grid", gap: 4 }}>
              <span style={{ fontSize: "0.75rem", color: "var(--text-3)", fontWeight: 500 }}>更新源地址</span>
              <input value={manifestUrl} onChange={(e) => setManifestUrl(e.target.value)}
                placeholder="GitHub Releases API 或 update.json 地址" style={{ fontSize: 13 }} />
            </label>
            <div className="form-actions" style={{ marginTop: 8 }}>
              <button className="btn-secondary" onClick={saveManifestUrl} type="button">保存更新源</button>
              <button className="btn-primary" onClick={() => void checkUpdates()} disabled={checking} type="button">
                {checking ? "检查中..." : "检查更新"}
              </button>
            </div>
            {(updateStatus && updateStatus !== "未检查") && (
              <div className={`about-update-status ${availableUpdate ? "has-update" : updateStatus === "检查失败" ? "has-error" : "is-latest"}`}>
                <span className="about-update-status-text">{updateStatus}</span>
                {updateDetail && <span className="about-update-detail">{updateDetail}</span>}
              </div>
            )}
            {availableUpdate ? (
              <div className="form-actions" style={{ marginTop: 8 }}>
                <button className="btn-primary" type="button" style={{ width: "100%" }}
                  onClick={() => void openUpdateUrl()}>
                  下载新版本 {availableUpdate.latestVersion}
                </button>
                {isSilentInstallAssetUrl(availableUpdate.downloadUrl) ? (
                  <button className="btn-secondary" type="button" disabled={installingUpdate} style={{ width: "100%" }}
                    onClick={() => void installUpdateSilently()}>
                    {installingUpdate ? "正在准备安装..." : "下载并静默安装"}
                  </button>
                ) : null}
              </div>
            ) : null}
          </div>
        </div>
      </div>

      <div className="about-section">
        <div className="about-section-title"><Info size={14} /><span>更多信息</span></div>
        <div className="menu-card flat-card about-card">
          {buildInfo ? (
            <div className="form-hint" style={{ padding: "10px 14px" }}>
              构建目标 {buildInfo.target} · 应用 ID {buildInfo.identifier}
            </div>
          ) : null}
          <MenuRow icon={Info} label="隐私说明及设置" onClick={() => setView("privacy")} iconColor="neutral" />
          <MenuRow icon={Info} label="软件声明" onClick={() => setView("statement")} iconColor="neutral" />
        </div>
      </div>

      <p className="about-footer">Made with love</p>
    </div>
  );
}
