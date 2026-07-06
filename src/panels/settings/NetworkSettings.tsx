import { useEffect, useState } from "react";
import { RefreshCw } from "lucide-react";
import { BackBtn, SecretInput } from "./_shared";

interface NetworkConfig {
  port: number;
  password: string;
  publicEnabled: boolean;
  publicPort: number;
  publicSecret: string;
}

interface WeatherConfig {
  qweatherApiKey: string;
  qweatherApiHost: string;
  defaultLocation: string;
  timeoutSeconds: number;
}

export function NetworkSettings({
  onBack,
  config,
  weather,
  onSave,
  onSaveWeather,
}: {
  onBack?: () => void;
  config: NetworkConfig;
  weather: WeatherConfig;
  onSave: (patch: Partial<NetworkConfig>) => Promise<void>;
  onSaveWeather: (patch: Partial<WeatherConfig>) => Promise<void>;
}) {
  const [draft, setDraft] = useState(config);
  const [weatherDraft, setWeatherDraft] = useState(weather);
  useEffect(() => setDraft(config), [config]);
  useEffect(() => setWeatherDraft(weather), [weather]);

  const save = () => {
    void onSave(draft);
    void onSaveWeather(weatherDraft);
  };

  const regenerate = () => {
    const publicPort = 30000 + Math.floor(Math.random() * 30000);
    const publicSecret = crypto.randomUUID().replace(/-/g, "").slice(0, 16);
    setDraft((d) => ({ ...d, publicPort, publicSecret }));
    void onSave({ publicPort, publicSecret });
  };

  return (
    <div className="primary-panel embedded-panel">
      <div className="panel-title action-title">
        <BackBtn onBack={onBack} />
        <div className="panel-title-text"><span>Network</span><strong>网络设置</strong></div>
        <button className="btn-primary" onClick={save} type="button">保存</button>
      </div>
      <div className="settings-form">
        <label>
          本地端口
          <input min={1} type="number" value={draft.port}
            onChange={(e) => setDraft((d) => ({ ...d, port: Number(e.target.value) }))} />
        </label>
        <label>
          公网访问密码
          <SecretInput value={draft.password}
            onChange={(value) => setDraft((d) => ({ ...d, password: value }))}
            placeholder="8位以上，含大小写字母和数字" />
        </label>
        <label className="checkbox-row">
          <input checked={draft.publicEnabled} type="checkbox"
            onChange={(e) => setDraft((d) => ({ ...d, publicEnabled: e.target.checked }))} />
          对公网开放（实验性）
        </label>
        <div className="two-column">
          <label>
            公网端口
            <input min={1} type="number" value={draft.publicPort}
              onChange={(e) => setDraft((d) => ({ ...d, publicPort: Number(e.target.value) }))} />
          </label>
          <label>
            随机路径
            <input value={draft.publicSecret}
              onChange={(e) => setDraft((d) => ({ ...d, publicSecret: e.target.value }))} />
          </label>
        </div>
        <button onClick={regenerate} type="button">
          <RefreshCw size={15} />重新生成端口和路径
        </button>
        <p className="form-hint">公网访问存在风险；请只在充分理解网络暴露风险时开启。</p>

        <div className="settings-divider" />
        <div className="panel-title-text">
          <span>Weather</span>
          <strong>天气服务</strong>
        </div>
        <label>
          和风天气 API Key
          <SecretInput value={weatherDraft.qweatherApiKey}
            onChange={(value) => setWeatherDraft((d) => ({ ...d, qweatherApiKey: value }))}
            placeholder="QWeather API Key" />
        </label>
        <label>
          和风天气 API Host
          <input value={weatherDraft.qweatherApiHost}
            onChange={(e) => setWeatherDraft((d) => ({ ...d, qweatherApiHost: e.target.value }))}
            placeholder="https://devapi.qweather.com" />
        </label>
        <div className="two-column">
          <label>
            默认城市
            <input value={weatherDraft.defaultLocation}
              onChange={(e) => setWeatherDraft((d) => ({ ...d, defaultLocation: e.target.value }))}
              placeholder="上海" />
          </label>
          <label>
            超时秒数
            <input min={3} max={30} type="number" value={weatherDraft.timeoutSeconds}
              onChange={(e) => setWeatherDraft((d) => ({ ...d, timeoutSeconds: Number(e.target.value) }))} />
          </label>
        </div>
        <p className="form-hint">用于内置天气查询工具；未填写 Key 时会明确提示配置，不会伪造天气。</p>
      </div>
    </div>
  );
}
