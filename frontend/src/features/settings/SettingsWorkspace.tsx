import { useEffect, useMemo, useState, type ReactNode } from "react";
import {
  Check,
  CircleHelp,
  Compass,
  Image,
  Info,
  MessageSquareText,
  MonitorCog,
  Network,
  Palette,
  Search,
  Settings2,
  ShieldCheck,
  Smile,
  Smartphone,
  Sparkles,
  Video,
  Wand2,
  type LucideIcon,
} from "lucide-react";
import {
  profilesApi,
  type ProfileConfig,
  type ProfileConfigPatch,
  type ProfileSummary,
  type SecretStatus,
  type Versioned,
} from "../../api/profiles";
import { ProfilesWorkspace } from "../profiles/ProfilesWorkspace";
import { ToolsWorkspace } from "../tools/ToolsWorkspace";
import { WechatSettingsPanel } from "./WechatSettingsPanel";
import "./settings.css";

type SettingsView =
  | "profiles"
  | "accounts"
  | "providers"
  | "imageProviders"
  | "videoProviders"
  | "searchProviders"
  | "visionProviders"
  | "browserProviders"
  | "videoSummary"
  | "chat"
  | "reply"
  | "theme"
  | "emoji"
  | "network"
  | "about"
  | "privacy"
  | "agreement";

type EntryStatus = "ready" | "configurable" | "planned";
type SettingsGroup = "个人" | "模型与能力" | "对话与外观" | "系统";

interface SettingsEntry {
  group: SettingsGroup;
  id: SettingsView;
  label: string;
  icon: LucideIcon;
  status: EntryStatus;
}

const SETTINGS_ENTRIES: readonly SettingsEntry[] = [
  { group: "个人", id: "profiles", label: "Profile 与密钥", icon: Settings2, status: "ready" },
  { group: "个人", id: "accounts", label: "微信账号", icon: Smartphone, status: "ready" },
  { group: "模型与能力", id: "providers", label: "模型服务", icon: Wand2, status: "ready" },
  { group: "模型与能力", id: "imageProviders", label: "图像服务", icon: Image, status: "configurable" },
  { group: "模型与能力", id: "videoProviders", label: "视频服务", icon: Video, status: "configurable" },
  { group: "模型与能力", id: "searchProviders", label: "搜索服务", icon: Search, status: "ready" },
  { group: "模型与能力", id: "visionProviders", label: "视觉服务", icon: Sparkles, status: "configurable" },
  { group: "模型与能力", id: "browserProviders", label: "浏览器服务", icon: Compass, status: "ready" },
  { group: "模型与能力", id: "videoSummary", label: "视频总结", icon: Video, status: "configurable" },
  { group: "对话与外观", id: "chat", label: "对话设置", icon: MessageSquareText, status: "ready" },
  { group: "对话与外观", id: "reply", label: "回复设置", icon: Settings2, status: "ready" },
  { group: "对话与外观", id: "theme", label: "主题", icon: Palette, status: "ready" },
  { group: "对话与外观", id: "emoji", label: "表情包", icon: Smile, status: "ready" },
  { group: "系统", id: "network", label: "网络", icon: Network, status: "ready" },
  { group: "系统", id: "about", label: "关于", icon: Info, status: "ready" },
  { group: "系统", id: "privacy", label: "隐私政策", icon: ShieldCheck, status: "ready" },
  { group: "系统", id: "agreement", label: "用户协议", icon: MonitorCog, status: "ready" },
] as const;

const SETTINGS_GROUPS: readonly SettingsGroup[] = ["个人", "模型与能力", "对话与外观", "系统"];

const EXTENSION_DEFINITIONS = {
  imageProviders: {
    title: "图像服务",
    extensionKey: "imageProvider",
    secretName: "IMAGE_API_KEY",
    providerPlaceholder: "openai-compatible",
    note: "配置会安全保存到当前 Profile；图像生成执行器将在对应 Rust adapter 接入后启用。",
  },
  videoProviders: {
    title: "视频服务",
    extensionKey: "videoProvider",
    secretName: "VIDEO_API_KEY",
    providerPlaceholder: "openai-compatible",
    note: "配置会安全保存到当前 Profile；视频生成执行器尚未纳入当前 Run registry。",
  },
  visionProviders: {
    title: "视觉服务",
    extensionKey: "visionProvider",
    secretName: "VISION_API_KEY",
    providerPlaceholder: "openai-compatible",
    note: "配置会安全保存到当前 Profile；视觉输入 adapter 会复用文件权限与审批边界。",
  },
  videoSummary: {
    title: "视频总结",
    extensionKey: "videoSummary",
    secretName: "VIDEO_SUMMARY_API_KEY",
    providerPlaceholder: "openai-compatible",
    note: "配置会安全保存到当前 Profile；视频总结执行器接入后才会出现在 Run 工具目录。",
  },
} as const;

type ExtensionView = keyof typeof EXTENSION_DEFINITIONS;

function statusLabel(status: EntryStatus): string {
  switch (status) {
    case "ready": return "已接入";
    case "configurable": return "可配置";
    case "planned": return "开发中";
  }
}

function StatusPill({ status }: { status: EntryStatus }) {
  return (
    <span className={`settings-status settings-status--${status}`} data-status={status}>
      {statusLabel(status)}
    </span>
  );
}

function SettingsHeader({
  icon: Icon,
  title,
  status,
  eyebrow = "DESKTOP SETTINGS",
}: {
  icon: LucideIcon;
  title: string;
  status: EntryStatus;
  eyebrow?: string;
}) {
  return (
    <header className="settings-hub-header">
      <div className="settings-hub-heading-icon"><Icon aria-hidden="true" size={21} /></div>
      <div>
        <small>{eyebrow}</small>
        <h2>{title}</h2>
      </div>
      <StatusPill status={status} />
    </header>
  );
}

function readLocalValue(key: string, fallback: string): string {
  try {
    return window.localStorage.getItem(key) ?? fallback;
  } catch {
    return fallback;
  }
}

function writeLocalValue(key: string, value: string): void {
  try {
    window.localStorage.setItem(key, value);
    window.dispatchEvent(new CustomEvent("synthchat-settings-changed", { detail: { key, value } }));
  } catch {
    // Private browsing or a locked storage area should not break the settings page.
  }
}

function LocalSettingsPanel({ view }: { view: "chat" | "reply" | "theme" | "emoji" | "network" }) {
  const config = {
    chat: {
      title: "对话设置",
      icon: MessageSquareText,
      rows: [
        ["synthchat.settings.chat.enterToSend", "Enter 发送消息", "toggle"],
        ["synthchat.settings.chat.showReasoning", "显示推理过程", "toggle"],
        ["synthchat.settings.chat.compactComposer", "紧凑输入区", "toggle"],
      ] as const,
    },
    reply: {
      title: "回复设置",
      icon: Settings2,
      rows: [
        ["synthchat.settings.reply.autoScroll", "流式回复自动滚动", "toggle"],
        ["synthchat.settings.reply.showUsage", "显示 Token 用量", "toggle"],
        ["synthchat.settings.reply.markdown", "启用 Markdown 渲染", "toggle"],
      ] as const,
    },
    theme: {
      title: "主题",
      icon: Palette,
      rows: [["synthchat.settings.theme.mode", "界面主题", "theme"]] as const,
    },
    emoji: {
      title: "表情包",
      icon: Smile,
      rows: [
        ["synthchat.settings.emoji.enabled", "在输入区显示表情按钮", "toggle"],
        ["synthchat.settings.emoji.skinTone", "默认肤色", "tone"],
      ] as const,
    },
    network: {
      title: "网络",
      icon: Network,
      rows: [
        ["synthchat.settings.network.proxyMode", "连接模式", "network"] as const,
        ["synthchat.settings.network.healthTimeout", "后端健康检查超时（毫秒）", "number"] as const,
      ] as const,
    },
  }[view];
  const [values, setValues] = useState<Record<string, string>>(() => Object.fromEntries(
    config.rows.map(([key, , kind]) => [
      key,
      readLocalValue(key, kind === "toggle" ? "true" : kind === "number" ? "6500" : kind === "theme" ? "system" : kind === "network" ? "desktop" : "default"),
    ]),
  ));

  const setValue = (key: string, value: string) => {
    setValues((current) => ({ ...current, [key]: value }));
    writeLocalValue(key, value);
  };

  return (
    <section className="settings-hub-panel" aria-labelledby={`settings-${view}-title`}>
      <SettingsHeader icon={config.icon} title={config.title} status="ready" />
      <div className="settings-hub-body settings-preference-list">
        {config.rows.map(([key, label, kind]) => {
          const value = values[key] ?? "";
          if (kind === "toggle") {
            const checked = value !== "false";
            return (
              <label className="settings-preference-row" key={key}>
                <span><strong>{label}</strong><small>仅影响本地 Desktop UI，不会修改模型或密钥。</small></span>
                <input checked={checked} onChange={(event) => setValue(key, String(event.target.checked))} type="checkbox" />
              </label>
            );
          }
          if (kind === "theme") {
            return (
              <label className="settings-preference-row" key={key}>
                <span><strong>{label}</strong><small>系统主题会跟随 Windows 外观设置。</small></span>
                <select value={value} onChange={(event) => setValue(key, event.target.value)}>
                  <option value="system">跟随系统</option>
                  <option value="light">浅色</option>
                  <option value="dark">深色</option>
                </select>
              </label>
            );
          }
          if (kind === "tone") {
            return (
              <label className="settings-preference-row" key={key}>
                <span><strong>{label}</strong><small>用于默认表情选择。</small></span>
                <select value={value} onChange={(event) => setValue(key, event.target.value)}>
                  <option value="default">默认</option>
                  <option value="light">浅色</option>
                  <option value="medium">中等</option>
                  <option value="dark">深色</option>
                </select>
              </label>
            );
          }
          if (kind === "network") {
            return (
              <label className="settings-preference-row" key={key}>
                <span><strong>{label}</strong><small>当前桌面壳管理本机 loopback Rust 后端。</small></span>
                <select value={value} onChange={(event) => setValue(key, event.target.value)}>
                  <option value="desktop">Desktop 管理（推荐）</option>
                  <option value="standalone">Standalone 诊断</option>
                </select>
              </label>
            );
          }
          return (
            <label className="settings-preference-row" key={key}>
              <span><strong>{label}</strong><small>仅保存本地偏好；运行时仍以 Desktop bridge 的安全上限为准。</small></span>
              <input min={1000} max={30000} step={500} type="number" value={value} onChange={(event) => setValue(key, event.target.value)} />
            </label>
          );
        })}
      </div>
    </section>
  );
}

function InfoPanel({ view }: { view: "about" | "privacy" | "agreement" }) {
  const content = {
    about: {
      title: "关于",
      icon: Info,
      paragraphs: [
        "SynthChat Desktop 由 React UI、Tauri 窗口壳和本地 Rust backend 组成。",
        "聊天、会话、工具、Skills、Memory 和 Profile 数据通过受保护的 loopback API 连接。",
        "本版本不托管 Python Hermes Agent runtime。",
      ],
    },
    privacy: {
      title: "隐私政策",
      icon: ShieldCheck,
      paragraphs: [
        "聊天、会话和 Profile 数据默认保存在本机用户目录。",
        "API Key 只写入操作系统密钥链；UI、日志和 SQLite 公开投影不保存明文密钥。",
        "发送到第三方模型或工具 Provider 的内容由当前 Profile 和具体 Run 决定。",
      ],
    },
    agreement: {
      title: "用户协议",
      icon: MonitorCog,
      paragraphs: [
        "你需要为自己配置的模型、搜索、浏览器、微信及其他第三方服务承担使用责任。",
        "外部 Provider 的可用性、计费、内容政策和账号风险由对应服务商决定。",
        "实验性微信桥接和第三方桌宠资源在正式发布前仍需单独完成授权核验。",
      ],
    },
  }[view];
  return (
    <section className="settings-hub-panel" aria-labelledby={`settings-${view}-title`}>
      <SettingsHeader icon={content.icon} title={content.title} status="ready" />
      <div className="settings-hub-body settings-info-body">
        {content.paragraphs.map((paragraph) => <p key={paragraph}>{paragraph}</p>)}
      </div>
    </section>
  );
}

function AccountsPanel() {
  return (
    <section className="settings-hub-panel" aria-labelledby="settings-accounts-title">
      <SettingsHeader icon={Smartphone} title="微信账号" status="ready" />
      <div className="settings-hub-body"><WechatSettingsPanel /></div>
    </section>
  );
}

interface ExtensionPanelProps {
  definition: (typeof EXTENSION_DEFINITIONS)[ExtensionView];
}

function extensionRecord(config: Versioned<ProfileConfig> | null, key: string): Record<string, unknown> {
  const value = config?.value.extensions[key];
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return value as Record<string, unknown>;
}

function safeText(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function ExtensionProviderPanel({ definition }: ExtensionPanelProps) {
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [profileId, setProfileId] = useState<string | null>(null);
  const [config, setConfig] = useState<Versioned<ProfileConfig> | null>(null);
  const [secrets, setSecrets] = useState<SecretStatus[]>([]);
  const [provider, setProvider] = useState("");
  const [model, setModel] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [enabled, setEnabled] = useState(true);
  const [secret, setSecret] = useState("");
  const [phase, setPhase] = useState<"loading" | "ready" | "error">("loading");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);

  const secretStatus = useMemo(
    () => secrets.find((item) => item.name === definition.secretName) ?? null,
    [definition.secretName, secrets],
  );

  useEffect(() => {
    let active = true;
    setPhase("loading");
    void profilesApi.listProfiles()
      .then((items) => {
        if (!active) return;
        setProfiles(items);
        setProfileId((current) => current && items.some((item) => item.id === current)
          ? current
          : items.find((item) => item.isActive)?.id ?? items[0]?.id ?? null);
      })
      .catch(() => { if (active) setPhase("error"); });
    return () => { active = false; };
  }, []);

  useEffect(() => {
    if (!profileId) {
      setConfig(null);
      setSecrets([]);
      setPhase(profiles.length === 0 ? "ready" : "loading");
      return undefined;
    }
    let active = true;
    setPhase("loading");
    void Promise.all([
      profilesApi.getProfileConfig(profileId),
      profilesApi.listSecretStatuses(profileId),
    ])
      .then(([nextConfig, nextSecrets]) => {
        if (!active) return;
        const record = extensionRecord(nextConfig, definition.extensionKey);
        setConfig(nextConfig);
        setSecrets(nextSecrets);
        setProvider(safeText(record.provider));
        setModel(safeText(record.model));
        setBaseUrl(safeText(record.baseUrl));
        setEnabled(record.enabled !== false);
        setSecret("");
        setMessage(null);
        setPhase("ready");
      })
      .catch(() => { if (active) setPhase("error"); });
    return () => { active = false; };
  }, [definition.extensionKey, profileId, profiles.length]);

  const save = async () => {
    if (!profileId || !config || busy) return;
    setBusy(true);
    setMessage(null);
    try {
      const extensionValue = {
        enabled,
        provider: provider.trim(),
        model: model.trim(),
        baseUrl: baseUrl.trim(),
      };
      const patch: ProfileConfigPatch = {
        extensions: { [definition.extensionKey]: extensionValue } as ProfileConfigPatch["extensions"],
      };
      const updated = await profilesApi.updateProfileConfig(profileId, patch, config.etag);
      let nextSecrets = secrets;
      if (secret.trim()) {
        const status = await profilesApi.putSecret(profileId, definition.secretName, secret.trim());
        nextSecrets = secrets.some((item) => item.name === status.name)
          ? secrets.map((item) => item.name === status.name ? status : item)
          : [...secrets, status];
        setSecret("");
      }
      setConfig(updated);
      setSecrets(nextSecrets);
      setMessage("配置已保存到当前 Profile；执行适配器会按后端能力逐步启用。");
    } catch (error) {
      setMessage(error instanceof Error ? error.message : "配置保存失败，请重试。");
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="settings-hub-panel" aria-labelledby={`settings-extension-${definition.extensionKey}`}>
      <SettingsHeader icon={definition.extensionKey === "imageProvider" ? Image : Video} title={definition.title} status="configurable" />
      <div className="settings-hub-body">
        {phase === "loading" ? <p className="settings-inline-state">正在读取 Profile 配置…</p> : null}
        {phase === "error" ? <p className="settings-inline-state is-error">无法读取 Profile 配置，请确认 Desktop 后端在线。</p> : null}
        {phase === "ready" && profiles.length === 0 ? <p className="settings-inline-state">暂无 Profile，请先创建一个 Profile。</p> : null}
        {phase === "ready" && profileId ? (
          <>
            <label className="settings-field">
              <span>Profile</span>
              <select value={profileId} onChange={(event) => setProfileId(event.target.value)}>
                {profiles.map((profile) => <option key={profile.id} value={profile.id}>{profile.displayName}</option>)}
              </select>
            </label>
            <div className="settings-form-grid">
              <label className="settings-field"><span>Provider</span><input placeholder={definition.providerPlaceholder} value={provider} onChange={(event) => setProvider(event.target.value)} /></label>
              <label className="settings-field"><span>模型</span><input placeholder="模型名称" value={model} onChange={(event) => setModel(event.target.value)} /></label>
              <label className="settings-field settings-field--wide"><span>Base URL</span><input placeholder="https://api.example.com/v1" value={baseUrl} onChange={(event) => setBaseUrl(event.target.value)} /></label>
              <label className="settings-field settings-secret-field"><span>{definition.secretName}</span><input placeholder={secretStatus?.configured ? "已保存，输入新值可替换" : "仅写入 OS keychain"} type="password" value={secret} onChange={(event) => setSecret(event.target.value)} /></label>
            </div>
            <label className="settings-preference-row settings-preference-row--inline"><span><strong>允许该配置参与后续执行</strong><small>关闭只保留配置，不会向 Run 注入。</small></span><input checked={enabled} onChange={(event) => setEnabled(event.target.checked)} type="checkbox" /></label>
            <p className="settings-hub-note">{definition.note}</p>
            {message ? <p className="settings-save-message" role="status">{message}</p> : null}
            <div className="settings-actions"><button className="settings-primary-button" disabled={busy} onClick={() => void save()} type="button"><Check aria-hidden="true" size={16} />保存配置</button></div>
          </>
        ) : null}
      </div>
    </section>
  );
}

function WrappedWorkspace({ title, icon: Icon, children }: { title: string; icon: LucideIcon; children: ReactNode }) {
  return (
    <section className="settings-hub-panel settings-hub-panel--workspace" aria-labelledby="settings-workspace-title">
      <SettingsHeader icon={Icon} title={title} status="ready" />
      <div className="settings-hub-workspace">{children}</div>
    </section>
  );
}

export function SettingsWorkspace() {
  const [view, setView] = useState<SettingsView>("profiles");
  const selected = SETTINGS_ENTRIES.find((entry) => entry.id === view) ?? SETTINGS_ENTRIES[0];
  const renderPanel = () => {
    if (view === "profiles") return <ProfilesWorkspace />;
    if (view === "providers") return <WrappedWorkspace title="模型服务" icon={Wand2}><ProfilesWorkspace /></WrappedWorkspace>;
    if (view === "searchProviders") return <WrappedWorkspace title="搜索服务" icon={Search}><ToolsWorkspace /></WrappedWorkspace>;
    if (view === "browserProviders") return <WrappedWorkspace title="浏览器服务" icon={Compass}><ToolsWorkspace /></WrappedWorkspace>;
    if (view === "accounts") return <AccountsPanel />;
    if (view === "about" || view === "privacy" || view === "agreement") return <InfoPanel view={view} />;
    if (view === "chat" || view === "reply" || view === "theme" || view === "emoji" || view === "network") return <LocalSettingsPanel view={view} />;
    return <ExtensionProviderPanel definition={EXTENSION_DEFINITIONS[view as ExtensionView]} />;
  };

  return (
    <section className="product-settings settings-hub" aria-label="设置产品面板">
      <aside className="product-settings-menu" aria-label="设置分类">
        <div className="product-list-heading"><div><span>SETTINGS</span><strong>设置</strong></div></div>
        <div className="product-settings-menu-scroll">
          {SETTINGS_GROUPS.map((group) => (
            <div className="product-settings-group" key={group}>
              <h2>{group}</h2>
              {SETTINGS_ENTRIES.filter((entry) => entry.group === group).map((entry) => {
                const Icon = entry.icon;
                return (
                  <button aria-current={view === entry.id ? "page" : undefined} className={view === entry.id ? "is-active" : undefined} key={entry.id} onClick={() => setView(entry.id)} type="button">
                    <Icon aria-hidden="true" size={16} />
                    <span>{entry.label}</span>
                    <small><StatusPill status={entry.status} /></small>
                  </button>
                );
              })}
            </div>
          ))}
        </div>
      </aside>
      <div className="product-settings-detail settings-hub-detail">
        <div className="settings-hub-selected" data-settings-view={selected.id}>{renderPanel()}</div>
      </div>
    </section>
  );
}
