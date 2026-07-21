import { lazy, Suspense, useState } from "react";
import {
  BookOpen,
  Bot,
  Brain,
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
    const module = await import("./features/product/ProductCatalogWorkspaces");
    return { default: module.ContactsWorkspace };
  }),
  Discover: lazy(async () => {
    const module = await import("./features/product/ProductWorkspaces");
    return { default: module.DiscoverWorkspace };
  }),
  Moments: lazy(async () => {
    const module = await import("./features/product/ProductCatalogWorkspaces");
    return { default: module.MomentsWorkspace };
  }),
  Personas: lazy(async () => {
    const module = await import("./features/product/ProductCatalogWorkspaces");
    return { default: module.PersonasWorkspace };
  }),
  Plugins: lazy(async () => {
    const module = await import("./features/plugins/PluginWorkspace");
    return { default: module.PluginWorkspace };
  }),
  Settings: lazy(async () => {
    const module = await import("./features/settings/SettingsWorkspace");
    return { default: module.SettingsWorkspace };
  }),
  Worldbooks: lazy(async () => {
    const module = await import("./features/product/ProductCatalogWorkspaces");
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
}

export const PHASE_TWO_SECTIONS: readonly PhaseTwoSection[] = [
  {
    id: "chat",
    label: "聊天",
    eyebrow: "RUN STREAM",
    icon: MessageSquareText,
  },
  {
    id: "sessions",
    label: "会话",
    eyebrow: "SESSION STORE",
    icon: History,
  },
  {
    id: "contacts",
    label: "通讯录",
    eyebrow: "CONTACTS",
    icon: Users,
  },
  {
    id: "discover",
    label: "发现",
    eyebrow: "DISCOVER",
    icon: Compass,
  },
  {
    id: "personas",
    label: "角色",
    eyebrow: "PERSONAS",
    icon: Bot,
  },
  {
    id: "moments",
    label: "朋友圈",
    eyebrow: "MOMENTS",
    icon: Newspaper,
  },
  {
    id: "memory",
    label: "记忆",
    eyebrow: "PROFILE MEMORY",
    icon: Brain,
  },
  {
    id: "worldbooks",
    label: "世界书",
    eyebrow: "WORLDBOOK",
    icon: BookOpen,
  },
  {
    id: "plugins",
    label: "插件",
    eyebrow: "PLUGINS",
    icon: Puzzle,
  },
  {
    id: "tools",
    label: "工具 / MCP",
    eyebrow: "CAPABILITIES",
    icon: PlugZap,
  },
  {
    id: "skills",
    label: "技能",
    eyebrow: "SKILLS",
    icon: Wand2,
  },
  {
    id: "settings",
    label: "设置",
    eyebrow: "DESKTOP SETTINGS",
    icon: Settings,
  },
];

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
  const [continuation, setContinuation] = useState<{
    id: string;
    title: string;
    personaId?: string;
  } | null>(null);
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
                setContinuation({
                  id: session.id,
                  title: session.title,
                  personaId: session.personaId ?? undefined,
                });
                setActiveSection("chat");
              }} />
            ) : section.id === "memory" ? (
              <LazyMemoryWorkspace />
            ) : section.id === "chat" ? (
              <LazyChatWorkspace continuation={continuation} />
            ) : section.id === "contacts" ? (
              <LazyProductWorkspaces.Contacts
                onNavigate={navigateProduct}
                onOpenSession={(session) => setContinuation(session)}
              />
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
              null
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
