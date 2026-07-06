import { useEffect, useState } from "react";
import { BackBtn } from "./_shared";

interface ReplyConfig {
  typingDelayEnabled?: boolean;
  typingSpeed: number;
  typingSpeedRandomMin: number;
  typingSpeedRandomMax: number;
  splitByNewline: boolean;
  showTypingIndicator: boolean;
  typingIndicatorRefreshSeconds?: number;
}

export function ReplySettings({
  onBack,
  config,
  onSave,
}: {
  onBack?: () => void;
  config: ReplyConfig;
  onSave: (patch: Partial<ReplyConfig>) => Promise<void>;
}) {
  const [splitByNewline, setSplitByNewline] = useState(config.splitByNewline);
  const [delayEnabled, setDelayEnabled] = useState(config.typingDelayEnabled !== false);
  const [typingSpeed, setTypingSpeed] = useState(config.typingSpeed);
  const [randomMin, setRandomMin] = useState(config.typingSpeedRandomMin);
  const [randomMax, setRandomMax] = useState(config.typingSpeedRandomMax);
  const [showTyping, setShowTyping] = useState(config.showTypingIndicator);
  const [typingRefreshSeconds, setTypingRefreshSeconds] = useState(config.typingIndicatorRefreshSeconds ?? 2);

  useEffect(() => {
    setSplitByNewline(config.splitByNewline);
    setDelayEnabled(config.typingDelayEnabled !== false);
    setTypingSpeed(config.typingSpeed);
    setRandomMin(config.typingSpeedRandomMin);
    setRandomMax(config.typingSpeedRandomMax);
    setShowTyping(config.showTypingIndicator);
    setTypingRefreshSeconds(config.typingIndicatorRefreshSeconds ?? 2);
  }, [config]);

  const save = () => void onSave({
    splitByNewline,
    typingDelayEnabled: delayEnabled,
    typingSpeed,
    typingSpeedRandomMin: randomMin,
    typingSpeedRandomMax: randomMax,
    showTypingIndicator: showTyping,
    typingIndicatorRefreshSeconds: typingRefreshSeconds,
  });

  return (
    <div className="primary-panel embedded-panel">
      <div className="panel-title action-title">
        <BackBtn onBack={onBack} />
        <div className="panel-title-text"><span>Reply</span><strong>回复设置</strong></div>
        <button className="btn-primary" onClick={save} type="button">保存</button>
      </div>
      <div className="card" style={{ margin: "0 16px 12px" }}>
        <div className="card-header">回复拆分</div>
        <div className="form-group">
          <div className="form-row">
            <label>按换行拆分</label>
            <label className="switch-wrap">
              <input type="checkbox" checked={splitByNewline}
                onChange={(e) => setSplitByNewline(e.target.checked)} />
              <span className="switch-track" />
            </label>
          </div>
        </div>
        <div className="form-hint">将 AI 回复按换行符拆分为多条消息存储和发送</div>
      </div>
      <div className="card" style={{ margin: "0 16px 12px" }}>
        <div className="card-header">打字模拟</div>
        <div className="form-group">
          <div className="form-row">
            <label>消息间延迟</label>
            <label className="switch-wrap">
              <input type="checkbox" checked={delayEnabled}
                onChange={(e) => setDelayEnabled(e.target.checked)} />
              <span className="switch-track" />
            </label>
          </div>
        </div>
        <div className="form-hint">开启后，相邻消息之间按打字速度模拟延迟</div>
        <div style={{ opacity: delayEnabled ? 1 : 0.45, pointerEvents: delayEnabled ? "auto" : "none" }}>
          <div className="form-group">
            <div className="form-row">
              <label>打字速度</label>
              <div className="slider-wrap">
                <input type="range" min={0.05} max={1} step={0.05} value={typingSpeed}
                  onChange={(e) => setTypingSpeed(Number(e.target.value))} />
                <span className="slider-val">{typingSpeed.toFixed(2)}</span>
              </div>
            </div>
          </div>
          <div className="form-group">
            <div className="form-row">
              <label>随机下限</label>
              <div className="slider-wrap">
                <input type="range" min={0.01} max={0.5} step={0.01} value={randomMin}
                  onChange={(e) => setRandomMin(Number(e.target.value))} />
                <span className="slider-val">{randomMin.toFixed(2)}</span>
              </div>
            </div>
          </div>
          <div className="form-group">
            <div className="form-row">
              <label>随机上限</label>
              <div className="slider-wrap">
                <input type="range" min={0.01} max={0.5} step={0.01} value={randomMax}
                  onChange={(e) => setRandomMax(Number(e.target.value))} />
                <span className="slider-val">{randomMax.toFixed(2)}</span>
              </div>
            </div>
          </div>
          <div className="form-hint">延迟 = 字数 × (打字速度 + 随机值)，限制在 0.5~8 秒</div>
        </div>
        <div className="form-group">
          <div className="form-row">
            <label>输入指示器</label>
            <label className="switch-wrap">
              <input type="checkbox" checked={showTyping}
                onChange={(e) => setShowTyping(e.target.checked)} />
              <span className="switch-track" />
            </label>
          </div>
        </div>
        <div style={{ opacity: showTyping ? 1 : 0.45, pointerEvents: showTyping ? "auto" : "none" }}>
          <div className="form-group">
            <div className="form-row">
              <label>续期间隔</label>
              <div className="slider-wrap">
                <input type="range" min={1} max={10} step={1} value={typingRefreshSeconds}
                  onChange={(e) => setTypingRefreshSeconds(Number(e.target.value))} />
                <span className="slider-val">{typingRefreshSeconds}s</span>
              </div>
            </div>
          </div>
        </div>
        <div className="form-hint">模型思考和回复时显示"对方正在输入"，并按续期间隔刷新，直到桌面端结束"正在思考"。</div>
      </div>
    </div>
  );
}
