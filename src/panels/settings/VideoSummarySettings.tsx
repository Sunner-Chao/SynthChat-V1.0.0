import { useEffect, useState } from "react";
import type { VideoSummaryConfig } from "../../lib/types";
import { BackBtn, SecretInput } from "./_shared";

export function defaultVideoSummaryConfig(): VideoSummaryConfig {
  return {
    enabled: true,
    modelsDir: "",
    transcriber: "auto",
    ytDlpCommand: "yt-dlp",
    cookie: "",
    cookieFile: "",
    ffmpegBinPath: "",
    fasterWhisperModel: "small",
    fasterWhisperModelDir: "",
    fasterWhisperDevice: "cpu",
    fasterWhisperComputeType: "int8",
    senseVoiceModelDir: "",
    senseVoiceDevice: "cpu",
    timeoutSeconds: 30,
    ytdlpInfoTimeoutSeconds: 120,
    downloadTimeoutSeconds: 600,
    outputDir: "",
  };
}

export function VideoSummarySettings({
  onBack,
  config,
  onSave,
}: {
  onBack?: () => void;
  config: VideoSummaryConfig;
  onSave: (patch: Partial<VideoSummaryConfig>) => Promise<void>;
}) {
  const [draft, setDraft] = useState<VideoSummaryConfig>(() => ({
    ...defaultVideoSummaryConfig(),
    ...config,
  }));
  useEffect(() => setDraft({ ...defaultVideoSummaryConfig(), ...config }), [config]);

  const update = <K extends keyof VideoSummaryConfig>(key: K, value: VideoSummaryConfig[K]) =>
    setDraft((current) => ({ ...current, [key]: value }));

  const save = () => void onSave(draft);

  return (
    <div className="primary-panel embedded-panel">
      <div className="panel-title action-title">
        <BackBtn onBack={onBack} />
        <div className="panel-title-text"><span>Video</span><strong>视频总结</strong></div>
        <button className="btn-primary" onClick={save} type="button">保存</button>
      </div>
      <div className="settings-form provider-card">
        <label className="checkbox-row">
          <input checked={draft.enabled} type="checkbox"
            onChange={(e) => update("enabled", e.target.checked)} />
          无字幕时启用本地音频转写
        </label>
        <div className="two-column">
          <label>转写引擎
            <select value={draft.transcriber} onChange={(e) => update("transcriber", e.target.value)}>
              <option value="auto">auto</option>
              <option value="faster_whisper">faster-whisper</option>
              <option value="sensevoice">SenseVoice</option>
              <option value="none">关闭</option>
            </select>
          </label>
          <label>模型根目录
            <input value={draft.modelsDir} placeholder="留空自动发现 models 目录"
              onChange={(e) => update("modelsDir", e.target.value)} />
          </label>
        </div>
        <div className="two-column">
          <label>yt-dlp 命令
            <input value={draft.ytDlpCommand} placeholder="yt-dlp"
              onChange={(e) => update("ytDlpCommand", e.target.value)} />
          </label>
          <label>ffmpeg 目录
            <input value={draft.ffmpegBinPath} placeholder="留空使用 PATH"
              onChange={(e) => update("ffmpegBinPath", e.target.value)} />
          </label>
        </div>
        <label>Bilibili / yt-dlp Cookie
          <SecretInput value={draft.cookie} placeholder="SESSDATA=...; bili_jct=...; DedeUserID=..."
            onChange={(v) => update("cookie", v)} />
        </label>
        <label>cookies.txt 文件路径
          <input value={draft.cookieFile} placeholder="Netscape cookies.txt，可由浏览器扩展导出"
            onChange={(e) => update("cookieFile", e.target.value)} />
        </label>
        <div className="two-column">
          <label>请求超时（秒）
            <input min={3} type="number" value={draft.timeoutSeconds}
              onChange={(e) => update("timeoutSeconds", Number(e.target.value))} />
          </label>
          <label>元数据超时（秒）
            <input min={10} type="number" value={draft.ytdlpInfoTimeoutSeconds}
              onChange={(e) => update("ytdlpInfoTimeoutSeconds", Number(e.target.value))} />
          </label>
        </div>
        <div className="two-column">
          <label>音频下载超时（秒）
            <input min={30} type="number" value={draft.downloadTimeoutSeconds}
              onChange={(e) => update("downloadTimeoutSeconds", Number(e.target.value))} />
          </label>
          <label>输出目录
            <input value={draft.outputDir} placeholder="留空使用应用数据目录"
              onChange={(e) => update("outputDir", e.target.value)} />
          </label>
        </div>
      </div>
      <div className="card" style={{ margin: "0 16px 12px" }}>
        <div className="card-header">faster-whisper</div>
        <div className="settings-form">
          <div className="two-column">
            <label>模型名
              <input value={draft.fasterWhisperModel} placeholder="small"
                onChange={(e) => update("fasterWhisperModel", e.target.value)} />
            </label>
            <label>模型目录
              <input value={draft.fasterWhisperModelDir} placeholder="留空使用 models/faster-whisper/small"
                onChange={(e) => update("fasterWhisperModelDir", e.target.value)} />
            </label>
          </div>
          <div className="two-column">
            <label>设备
              <select value={draft.fasterWhisperDevice} onChange={(e) => update("fasterWhisperDevice", e.target.value)}>
                <option value="cpu">cpu</option>
                <option value="cuda">cuda</option>
                <option value="auto">auto</option>
              </select>
            </label>
            <label>计算类型
              <select value={draft.fasterWhisperComputeType} onChange={(e) => update("fasterWhisperComputeType", e.target.value)}>
                <option value="int8">int8</option>
                <option value="float16">float16</option>
                <option value="float32">float32</option>
              </select>
            </label>
          </div>
        </div>
      </div>
      <div className="card" style={{ margin: "0 16px 12px" }}>
        <div className="card-header">SenseVoice</div>
        <div className="settings-form">
          <div className="two-column">
            <label>模型目录
              <input value={draft.senseVoiceModelDir} placeholder="留空使用 models/sensevoice/SenseVoiceSmall"
                onChange={(e) => update("senseVoiceModelDir", e.target.value)} />
            </label>
            <label>设备
              <select value={draft.senseVoiceDevice} onChange={(e) => update("senseVoiceDevice", e.target.value)}>
                <option value="cpu">cpu</option>
                <option value="cuda">cuda</option>
              </select>
            </label>
          </div>
        </div>
        <div className="form-hint">SenseVoice 需要 Python 环境安装 funasr；未安装时 auto 会继续尝试 faster-whisper。</div>
      </div>
    </div>
  );
}
