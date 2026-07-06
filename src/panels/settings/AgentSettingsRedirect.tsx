import { Bot, Sparkles } from "lucide-react";
import type { AgentDefinition, AppSection } from "../../lib/types";

export function AgentSettingsRedirect({
  agents,
  serversCount,
  setSection,
  skillsCount,
}: {
  agents: AgentDefinition[];
  serversCount: number;
  setSection: (section: AppSection, settingsView?: string) => void;
  skillsCount: number;
}) {
  const enabledAgents = agents.filter((agent) => agent.enabled).length;
  const defaultAgent = agents.find((agent) => agent.isDefault) ?? agents[0] ?? null;

  return (
    <div className="primary-panel embedded-panel" style={{ padding: 0 }}>
      <div className="agent-settings-hero">
        <div className="agent-settings-hero-info">
          <span className="agent-settings-hero-icon"><Bot size={26} /></span>
          <div className="agent-settings-hero-text">
            <strong>Agent 配置</strong>
            <small>{enabledAgents}/{agents.length} 个启用 · {skillsCount} 个 Skills · {serversCount} 个 MCP 服务</small>
          </div>
        </div>
        <button
          className="btn-primary"
          type="button"
          style={{ fontSize: 13, padding: "8px 18px", width: "auto", minWidth: "auto", marginLeft: "auto", flexShrink: 0 }}
          onClick={() => setSection("agents")}
        >
          打开 Agent 管理
        </button>
      </div>
      <div className="agent-settings-section">
        <div className="agent-settings-section-title">
          <Sparkles size={16} /><strong>统一配置位置</strong>
        </div>
        <p className="form-hint">
          Agent 管理页负责模型 fallback、MCP/Skills、Shell 权限和子 Agent 限制；最大工具迭代由通讯录/角色编辑里的工具策略主导。
        </p>
        <div className="agent-summary-grid" style={{ marginTop: 12 }}>
          {defaultAgent ? (
            <div className="agent-summary-item">
              <span className="agent-summary-icon indigo"><Bot size={18} /></span>
              <div className="agent-summary-text">
                <strong style={{ fontSize: 14 }}>{defaultAgent.name}{defaultAgent.isDefault ? " ★" : ""}</strong>
                <small>Agent fallback 预算：{defaultAgent.maxToolIterations ?? 90} 次；实际以角色工具策略为准</small>
              </div>
            </div>
          ) : (
            <p className="form-hint">暂无 Agent，请先创建一个。</p>
          )}
        </div>
      </div>
    </div>
  );
}
