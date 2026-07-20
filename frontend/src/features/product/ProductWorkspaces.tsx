import { useState, type ReactNode } from "react";
import {
  BookOpen,
  Bot,
  Camera,
  ChevronRight,
  CircleSlash2,
  Compass,
  Image,
  Info,
  MessageSquareText,
  MonitorCog,
  Network,
  Newspaper,
  Palette,
  Plus,
  PlugZap,
  Puzzle,
  Search,
  Settings,
  ShieldCheck,
  Smartphone,
  Smile,
  Sparkles,
  Upload,
  UserRoundCog,
  Users,
  Video,
  Wand2,
  type LucideIcon,
} from "lucide-react";
import { ProfilesWorkspace } from "../profiles/ProfilesWorkspace";
import "./product.css";

export type ProductDestination =
  | "chat"
  | "contacts"
  | "discover"
  | "personas"
  | "moments"
  | "memory"
  | "worldbooks"
  | "plugins"
  | "tools"
  | "skills"
  | "settings";

interface ProductWorkspaceProps {
  onNavigate: (destination: ProductDestination) => void;
}

function CapabilityPill({ available = false }: { available?: boolean }) {
  return (
    <span
      className={available ? "product-capability-pill is-available" : "product-capability-pill"}
      data-capability-state={available ? "available" : "disabled"}
    >
      {available ? "已接入" : "未启用"}
    </span>
  );
}

function PanelHeading({
  eyebrow,
  icon: Icon,
  title,
}: {
  eyebrow: string;
  icon: LucideIcon;
  title: string;
}) {
  return (
    <header className="product-panel-heading">
      <div className="product-panel-heading-copy">
        <Icon aria-hidden="true" size={18} />
        <div>
          <span>{eyebrow}</span>
          <h2>{title}</h2>
        </div>
      </div>
      <CapabilityPill />
    </header>
  );
}

function CapabilityUnavailable({
  compact = false,
  icon: Icon = CircleSlash2,
  title,
}: {
  compact?: boolean;
  icon?: LucideIcon;
  title: string;
}) {
  return (
    <div
      className={compact ? "product-unavailable is-compact" : "product-unavailable"}
      data-capability-state="disabled"
    >
      <span className="product-unavailable-icon"><Icon aria-hidden="true" size={26} /></span>
      <strong>{title}</strong>
      <small>当前 Rust 后端未提供此能力</small>
    </div>
  );
}

function ProductMenuRow({
  disabled = false,
  icon: Icon,
  label,
  meta,
  onClick,
}: {
  disabled?: boolean;
  icon: LucideIcon;
  label: string;
  meta: string;
  onClick?: () => void;
}) {
  return (
    <button
      className="product-menu-row"
      disabled={disabled}
      onClick={onClick}
      type="button"
    >
      <span className="product-menu-icon"><Icon aria-hidden="true" size={17} /></span>
      <span className="product-menu-copy"><strong>{label}</strong><small>{meta}</small></span>
      <ChevronRight aria-hidden="true" size={17} />
    </button>
  );
}

export function ContactsWorkspace({ onNavigate }: ProductWorkspaceProps) {
  return (
    <section className="product-split" aria-label="通讯录产品面板">
      <aside className="product-list-pane">
        <div className="product-list-heading">
          <div><span>CONTACTS</span><strong>通讯录</strong></div>
          <div className="product-heading-actions">
            <button aria-label="导入角色" disabled title="导入角色" type="button"><Upload size={16} /></button>
            <button aria-label="新建角色" disabled title="新建角色" type="button"><Plus size={16} /></button>
          </div>
        </div>
        <label className="product-search-field">
          <Search aria-hidden="true" size={16} />
          <input aria-label="搜索通讯录" disabled placeholder="搜索" type="search" />
        </label>
        <CapabilityUnavailable compact icon={Users} title="角色目录未启用" />
      </aside>

      <article className="product-detail-pane">
        <PanelHeading eyebrow="CONTACT PROFILE" icon={Users} title="角色详情" />
        <div className="product-contact-profile">
          <span className="product-avatar"><Users aria-hidden="true" size={28} /></span>
          <h3>未选择角色</h3>
          <p>通讯录能力不可用</p>
          <div className="product-menu-card">
            <ProductMenuRow disabled icon={MessageSquareText} label="发消息" meta="未启用" />
            <ProductMenuRow disabled icon={Smartphone} label="链接微信" meta="未启用" />
            <ProductMenuRow icon={BookOpen} label="世界书" meta="查看产品壳" onClick={() => onNavigate("worldbooks")} />
            <ProductMenuRow icon={UserRoundCog} label="角色设置" meta="查看产品壳" onClick={() => onNavigate("personas")} />
          </div>
        </div>
      </article>
    </section>
  );
}

export function DiscoverWorkspace({ onNavigate }: ProductWorkspaceProps) {
  return (
    <section className="product-page" aria-label="发现产品面板">
      <PanelHeading eyebrow="DISCOVER" icon={Compass} title="发现" />
      <div className="product-page-body is-narrow">
        <div className="product-menu-card">
          <ProductMenuRow icon={Camera} label="朋友圈" meta="能力未启用" onClick={() => onNavigate("moments")} />
          <ProductMenuRow icon={BookOpen} label="世界书" meta="能力未启用" onClick={() => onNavigate("worldbooks")} />
        </div>
      </div>
    </section>
  );
}

const PERSONA_TABS = ["基础资料", "角色设定", "行为", "图像", "工具"] as const;

export function PersonasWorkspace() {
  const [tab, setTab] = useState<(typeof PERSONA_TABS)[number]>("基础资料");

  return (
    <section className="product-split" aria-label="角色产品面板">
      <aside className="product-list-pane">
        <div className="product-list-heading">
          <div><span>PERSONAS</span><strong>角色</strong></div>
          <button aria-label="新建角色" disabled title="新建角色" type="button"><Plus size={16} /></button>
        </div>
        <label className="product-search-field">
          <Search aria-hidden="true" size={16} />
          <input aria-label="搜索角色" disabled placeholder="搜索角色" type="search" />
        </label>
        <CapabilityUnavailable compact icon={Bot} title="角色目录未启用" />
      </aside>

      <article className="product-detail-pane">
        <PanelHeading eyebrow="PERSONA EDITOR" icon={Bot} title="角色编辑" />
        <div className="product-tabs" role="tablist" aria-label="角色设置分类">
          {PERSONA_TABS.map((item) => (
            <button
              aria-selected={tab === item}
              className={tab === item ? "is-active" : undefined}
              key={item}
              onClick={() => setTab(item)}
              role="tab"
              type="button"
            >
              {item}
            </button>
          ))}
        </div>
        <div className="product-form-shell" role="tabpanel">
          <div className="product-form-grid">
            <label><span>名称</span><input disabled value="" readOnly /></label>
            <label><span>Profile</span><select disabled value=""><option value="">未选择</option></select></label>
          </div>
          <label><span>{tab === "角色设定" ? "角色提示词" : `${tab}配置`}</span><textarea disabled value="" readOnly /></label>
          <CapabilityUnavailable compact icon={Bot} title="角色配置未启用" />
          <div className="product-form-actions">
            <button disabled type="button">保存角色</button>
          </div>
        </div>
      </article>
    </section>
  );
}

export function MomentsWorkspace() {
  return (
    <section className="product-page" aria-label="朋友圈产品面板">
      <PanelHeading eyebrow="MOMENTS" icon={Newspaper} title="朋友圈" />
      <div className="product-page-body is-feed">
        <div className="product-composer">
          <textarea aria-label="朋友圈正文" disabled placeholder="写一条朋友圈动态..." />
          <button disabled type="button"><Plus aria-hidden="true" size={16} />发布</button>
        </div>
        <CapabilityUnavailable icon={Newspaper} title="朋友圈数据服务未启用" />
      </div>
    </section>
  );
}

export function WorldbooksWorkspace() {
  return (
    <section className="product-page" aria-label="世界书产品面板">
      <PanelHeading eyebrow="WORLDBOOK" icon={BookOpen} title="世界书" />
      <div className="product-page-body">
        <section className="product-form-section" aria-labelledby="worldbook-create-title">
          <div className="product-section-heading">
            <div><span>NEW WORLDBOOK</span><h3 id="worldbook-create-title">新建世界书</h3></div>
            <CapabilityPill />
          </div>
          <div className="product-form-grid">
            <label><span>名称</span><input disabled /></label>
            <label><span>关键词</span><input disabled /></label>
          </div>
          <label><span>说明</span><textarea disabled /></label>
          <button className="product-primary-button" disabled type="button"><Plus size={16} />新建世界书</button>
        </section>
        <section className="product-catalog-section" aria-labelledby="worldbook-list-title">
          <div className="product-section-heading"><div><span>LIBRARY</span><h3 id="worldbook-list-title">世界书列表</h3></div></div>
          <CapabilityUnavailable compact icon={BookOpen} title="世界书目录未启用" />
        </section>
      </div>
    </section>
  );
}

export function PluginsWorkspace() {
  return (
    <section className="product-page" aria-label="插件产品面板">
      <PanelHeading eyebrow="PLUGINS" icon={Puzzle} title="插件管理" />
      <div className="product-page-body">
        <div className="product-catalog-toolbar">
          <label className="product-search-field">
            <Search aria-hidden="true" size={16} />
            <input aria-label="搜索插件" disabled placeholder="搜索插件" type="search" />
          </label>
          <button disabled type="button"><Plus aria-hidden="true" size={16} />安装插件</button>
        </div>
        <CapabilityUnavailable icon={PlugZap} title="插件目录未启用" />
      </div>
    </section>
  );
}

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

interface SettingsEntry {
  group: "个人" | "模型与能力" | "对话与外观" | "系统";
  icon: LucideIcon;
  id: SettingsView;
  label: string;
  available?: boolean;
}

const SETTINGS_ENTRIES: readonly SettingsEntry[] = [
  { group: "个人", id: "profiles", label: "Profile 与密钥", icon: UserRoundCog, available: true },
  { group: "个人", id: "accounts", label: "微信账号", icon: Smartphone },
  { group: "模型与能力", id: "providers", label: "模型服务", icon: Wand2 },
  { group: "模型与能力", id: "imageProviders", label: "图像服务", icon: Image },
  { group: "模型与能力", id: "videoProviders", label: "视频服务", icon: Video },
  { group: "模型与能力", id: "searchProviders", label: "搜索服务", icon: Search },
  { group: "模型与能力", id: "visionProviders", label: "视觉服务", icon: Camera },
  { group: "模型与能力", id: "browserProviders", label: "浏览器服务", icon: Compass },
  { group: "模型与能力", id: "videoSummary", label: "视频总结", icon: Sparkles },
  { group: "对话与外观", id: "chat", label: "对话设置", icon: MessageSquareText },
  { group: "对话与外观", id: "reply", label: "回复设置", icon: Settings },
  { group: "对话与外观", id: "theme", label: "主题", icon: Palette },
  { group: "对话与外观", id: "emoji", label: "表情包", icon: Smile },
  { group: "系统", id: "network", label: "网络", icon: Network },
  { group: "系统", id: "about", label: "关于", icon: Info },
  { group: "系统", id: "privacy", label: "隐私政策", icon: ShieldCheck },
  { group: "系统", id: "agreement", label: "用户协议", icon: MonitorCog },
] as const;

const SETTINGS_GROUPS: readonly SettingsEntry["group"][] = ["个人", "模型与能力", "对话与外观", "系统"];

function SettingsUnavailable({ entry }: { entry: SettingsEntry }) {
  const Icon = entry.icon;
  return (
    <section className="product-settings-unavailable" aria-labelledby={`settings-${entry.id}-title`}>
      <div className="product-settings-unavailable-heading">
        <span><Icon aria-hidden="true" size={20} /></span>
        <div><small>SETTINGS</small><h2 id={`settings-${entry.id}-title`}>{entry.label}</h2></div>
        <CapabilityPill />
      </div>
      <CapabilityUnavailable icon={Icon} title={`${entry.label}未启用`} />
    </section>
  );
}

export function SettingsWorkspace() {
  const [view, setView] = useState<SettingsView>("profiles");
  const selected = SETTINGS_ENTRIES.find((entry) => entry.id === view) ?? SETTINGS_ENTRIES[0];

  return (
    <section className="product-settings" aria-label="设置产品面板">
      <aside className="product-settings-menu" aria-label="设置分类">
        <div className="product-list-heading">
          <div><span>SETTINGS</span><strong>设置</strong></div>
        </div>
        <div className="product-settings-menu-scroll">
          {SETTINGS_GROUPS.map((group) => (
            <div className="product-settings-group" key={group}>
              <h2>{group}</h2>
              {SETTINGS_ENTRIES.filter((entry) => entry.group === group).map((entry) => {
                const Icon = entry.icon;
                return (
                  <button
                    aria-current={view === entry.id ? "page" : undefined}
                    className={view === entry.id ? "is-active" : undefined}
                    key={entry.id}
                    onClick={() => setView(entry.id)}
                    type="button"
                  >
                    <Icon aria-hidden="true" size={16} />
                    <span>{entry.label}</span>
                    <small>{entry.available ? "已接入" : "未启用"}</small>
                  </button>
                );
              })}
            </div>
          ))}
        </div>
      </aside>
      <div className="product-settings-detail">
        {view === "profiles" ? <ProfilesWorkspace /> : <SettingsUnavailable entry={selected} />}
      </div>
    </section>
  );
}

export function ProductWorkspaceFrame({ children }: { children: ReactNode }) {
  return <div className="product-workspace-frame">{children}</div>;
}
