import { lazy, Suspense, useState } from "react";
import {
  BookOpen,
  Bot,
  Brain,
  CircleSlash2,
  Compass,
  History,
  MessageSquareText,
  Newspaper,
  PlugZap,
  Puzzle,
  Settings,
  Users,
  Wand2,
  type LucideIcon,
} from "lucide-react";
import { BackendStatusIndicator } from "./components/BackendStatusIndicator";
import { DesktopPetToggle } from "./features/pet/DesktopPetToggle";
import { ChatRunProvider } from "./features/chat/ChatRunProvider";
import type { ProductDestination } from "./features/product/ProductWorkspaces";
import "./styles.css";

const LazyChatWorkspace = lazy(async () => {
  const module = await import("./features/chat/ChatWorkspace");
  return { default: module.ChatWorkspace };
});

const LazyMemoryWorkspace = lazy(async () => {
  const module = await import("./features/memory/MemoryWorkspace");
  return { default: module.MemoryWorkspace };
});

const LazyProductWorkspaces = {
  Contacts: lazy(async () => {
    const module = await import("./features/product/ProductWorkspaces");
    return { default: module.ContactsWorkspace };
  }),
  Discover: lazy(async () => {
    const module = await import("./features/product/ProductWorkspaces");
    return { default: module.DiscoverWorkspace };
  }),
  Moments: lazy(async () => {
    const module = await import("./features/product/ProductWorkspaces");
    return { default: module.MomentsWorkspace };
  }),
  Personas: lazy(async () => {
    const module = await import("./features/product/ProductWorkspaces");
    return { default: module.PersonasWorkspace };
  }),
  Plugins: lazy(async () => {
    const module = await import("./features/product/ProductWorkspaces");
    return { default: module.PluginsWorkspace };
  }),
  Settings: lazy(async () => {
    const module = await import("./features/product/ProductWorkspaces");
    return { default: module.SettingsWorkspace };
  }),
  Worldbooks: lazy(async () => {
    const module = await import("./features/product/ProductWorkspaces");
    return { default: module.WorldbooksWorkspace };
  }),
} as const;

const LazySessionsWorkspace = lazy(async () => {
  const module = await import("./features/sessions/SessionsWorkspace");
  return { default: module.SessionsWorkspace };
});

const LazyToolsWorkspace = lazy(async () => {
  const module = await import("./features/tools/ToolsWorkspace");
  return { default: module.ToolsWorkspace };
});

export type PhaseTwoSectionId =
  | "chat"
  | "sessions"
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

export interface PhaseTwoSection {
  id: PhaseTwoSectionId;
  label: string;
  eyebrow: string;
  icon: LucideIcon;
  unavailableTitle: string;
  unavailableMessage: string;
  services: readonly string[];
}

export const PHASE_TWO_SECTIONS: readonly PhaseTwoSection[] = [
  {
    id: "chat",
    label: "聊天",
    eyebrow: "RUN STREAM",
    icon: MessageSquareText,
    unavailableTitle: "聊天暂不可用",
    unavailableMessage: "聊天服务尚未启用。",
    services: ["消息", "流式响应", "工具进度"],
  },
  {
    id: "sessions",
    label: "会话",
    eyebrow: "SESSION STORE",
    icon: History,
    unavailableTitle: "会话暂不可用",
    unavailableMessage: "会话服务尚未启用。",
    services: ["历史列表", "全文搜索", "继续对话"],
  },
  {
    id: "contacts",
    label: "通讯录",
    eyebrow: "CONTACTS",
    icon: Users,
    unavailableTitle: "通讯录暂不可用",
    unavailableMessage: "角色目录服务尚未启用。",
    services: ["角色目录", "会话入口", "账号关联"],
  },
  {
    id: "discover",
    label: "发现",
    eyebrow: "DISCOVER",
    icon: Compass,
    unavailableTitle: "发现暂不可用",
    unavailableMessage: "发现服务尚未启用。",
    services: ["朋友圈", "世界书"],
  },
  {
    id: "personas",
    label: "角色",
    eyebrow: "PERSONAS",
    icon: Bot,
    unavailableTitle: "角色暂不可用",
    unavailableMessage: "角色配置服务尚未启用。",
    services: ["人设", "行为", "模型绑定"],
  },
  {
    id: "moments",
    label: "朋友圈",
    eyebrow: "MOMENTS",
    icon: Newspaper,
    unavailableTitle: "朋友圈暂不可用",
    unavailableMessage: "朋友圈数据服务尚未启用。",
    services: ["动态", "评论", "媒体"],
  },
  {
    id: "memory",
    label: "记忆",
    eyebrow: "PROFILE MEMORY",
    icon: Brain,
    unavailableTitle: "记忆暂不可用",
    unavailableMessage: "记忆服务尚未启用。",
    services: ["长期记忆", "用户信息", "用量与安全"],
  },
  {
    id: "worldbooks",
    label: "世界书",
    eyebrow: "WORLDBOOK",
    icon: BookOpen,
    unavailableTitle: "世界书暂不可用",
    unavailableMessage: "世界书服务尚未启用。",
    services: ["目录", "条目", "角色绑定"],
  },
  {
    id: "plugins",
    label: "插件",
    eyebrow: "PLUGINS",
    icon: Puzzle,
    unavailableTitle: "插件暂不可用",
    unavailableMessage: "插件服务尚未启用。",
    services: ["目录", "安装", "启停"],
  },
  {
    id: "tools",
    label: "工具 / MCP",
    eyebrow: "CAPABILITIES",
    icon: PlugZap,
    unavailableTitle: "工具暂不可用",
    unavailableMessage: "工具服务尚未启用。",
    services: ["Toolsets", "Web", "MCP"],
  },
  {
    id: "skills",
    label: "技能",
    eyebrow: "SKILLS",
    icon: Wand2,
    unavailableTitle: "技能暂不可用",
    unavailableMessage: "技能服务尚未启用。",
    services: ["发现", "安装", "启停"],
  },
  {
    id: "settings",
    label: "设置",
    eyebrow: "DESKTOP SETTINGS",
    icon: Settings,
    unavailableTitle: "设置暂不可用",
    unavailableMessage: "设置服务尚未启用。",
    services: ["Profile", "服务", "桌面偏好"],
  },
];

export function PhaseTwoWorkspace({ section }: { section: PhaseTwoSection }) {
  const Icon = section.icon;

  return (
    <div className="workspace-panel phase-two-panel">
      <div className="phase-two-content-grid">
        <aside className="phase-two-rail" aria-label={`${section.label}接入状态`}>
          <div className="phase-two-rail-heading">
            <span>{section.eyebrow}</span>
            <strong>{section.label}</strong>
          </div>

          <div className="phase-two-pending-list">
            {section.services.map((item) => (
              <div className="phase-two-pending-row" key={item}>
                <span>{item}</span>
                <small>未启用</small>
              </div>
            ))}
          </div>

          <dl className="phase-two-rail-status">
            <div><dt>接口版本</dt><dd className="is-isolated">v1</dd></div>
            <div><dt>功能状态</dt><dd>未启用</dd></div>
          </dl>
        </aside>

        <article className="phase-two-unavailable" aria-live="polite">
          <div className="phase-two-unavailable-icon" aria-hidden="true">
            <Icon size={30} strokeWidth={1.7} />
          </div>
          <div className="phase-two-unavailable-code">
            <CircleSlash2 size={14} aria-hidden="true" />
            <span>未启用</span>
          </div>
          <h2>{section.unavailableTitle}</h2>
          <p>{section.unavailableMessage}</p>
        </article>
      </div>
    </div>
  );
}

function WorkspaceLoadingFallback() {
  return (
    <div
      aria-busy="true"
      aria-label="工作区加载中"
      className="workspace-panel"
      role="status"
    />
  );
}

function AppWorkspace() {
  const [activeSection, setActiveSection] = useState<PhaseTwoSectionId>("chat");
  const [continuation, setContinuation] = useState<{ id: string; title: string } | null>(null);
  const section = PHASE_TWO_SECTIONS.find((item) => item.id === activeSection)
    ?? PHASE_TWO_SECTIONS[0]!;
  const navigateProduct = (destination: ProductDestination) => setActiveSection(destination);

  return (
    <main className="app-shell product-shell">
      <aside className="sidebar">
        <div className="brand" title="SynthChat">
          <div className="brand-mark">
            <img alt="" aria-hidden="true" src="/icon/Icon-SynthChat.png" />
          </div>
          <div>
            <strong>SynthChat</strong>
            <span>Desktop</span>
          </div>
        </div>

        <nav className="nav-list" aria-label="主导航">
          {PHASE_TWO_SECTIONS.map((item) => {
            const Icon = item.icon;
            const active = item.id === activeSection;
            return (
              <button
                aria-label={item.label}
                aria-current={active ? "page" : undefined}
                className={active ? "nav-item active" : "nav-item"}
                key={item.id}
                onClick={() => setActiveSection(item.id)}
                title={item.label}
                type="button"
              >
                <Icon aria-hidden="true" size={20} strokeWidth={active ? 2.2 : 1.8} />
                <span>{item.label}</span>
              </button>
            );
          })}
        </nav>

        <div className="phase-two-sidebar-footer">
          <span>LOCAL RUST</span>
          <strong>v1</strong>
        </div>
      </aside>

      <section className="workspace">
        <header className="workspace-header">
          <div>
            <p>{section.eyebrow}</p>
            <h1>{section.label}</h1>
          </div>
          <div className="workspace-header-actions">
            <DesktopPetToggle />
            <BackendStatusIndicator />
          </div>
        </header>

        <section className="workspace-content" aria-label={`${section.label}工作区`}>
          <Suspense fallback={<WorkspaceLoadingFallback />}>
            {section.id === "sessions" ? (
              <LazySessionsWorkspace onContinue={(session) => {
                setContinuation({ id: session.id, title: session.title });
                setActiveSection("chat");
              }} />
            ) : section.id === "memory" ? (
              <LazyMemoryWorkspace />
            ) : section.id === "chat" ? (
              <LazyChatWorkspace continuation={continuation} />
            ) : section.id === "contacts" ? (
              <LazyProductWorkspaces.Contacts onNavigate={navigateProduct} />
            ) : section.id === "discover" ? (
              <LazyProductWorkspaces.Discover onNavigate={navigateProduct} />
            ) : section.id === "personas" ? (
              <LazyProductWorkspaces.Personas />
            ) : section.id === "moments" ? (
              <LazyProductWorkspaces.Moments />
            ) : section.id === "worldbooks" ? (
              <LazyProductWorkspaces.Worldbooks />
            ) : section.id === "plugins" ? (
              <LazyProductWorkspaces.Plugins />
            ) : section.id === "settings" ? (
              <LazyProductWorkspaces.Settings />
            ) : section.id === "tools" || section.id === "skills" ? (
              <LazyToolsWorkspace key={section.id} />
            ) : (
              <PhaseTwoWorkspace key={section.id} section={section} />
            )}
          </Suspense>
        </section>
      </section>
    </main>
  );
}

export function App() {
  return (
    <ChatRunProvider>
      <AppWorkspace />
    </ChatRunProvider>
  );
}
