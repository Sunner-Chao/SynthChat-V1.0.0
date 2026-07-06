import { useEffect, useMemo, useState } from "react";
import { Bot, PlugZap, RefreshCw, Search, Sparkles } from "lucide-react";
import { filterSkillsByQuery } from "../../lib/skillSearch";
import { useAppStore } from "../../lib/store";
import type { AgentConfig, AgentDefinition, McpServer, SkillSummary } from "../../lib/types";

export function AgentSettings({
  config,
  agents,
  saveConfig,
  skills,
  servers,
  installBuiltinSkills,
  refreshSkills
}: {
  config: AgentConfig;
  agents: AgentDefinition[];
  saveConfig: (config: AgentConfig) => Promise<void>;
  skills: SkillSummary[];
  servers: McpServer[];
  installBuiltinSkills: () => Promise<void>;
  refreshSkills: () => Promise<void>;
}) {
  const [draft, setDraft] = useState(config);
  const [skillSearch, setSkillSearch] = useState("");
  useEffect(() => setDraft(config), [config]);
  const filteredSkills = useMemo(() => filterSkillsByQuery(skills, skillSearch), [skillSearch, skills]);
  const toggleSkill = (id: string) => {
    setDraft((current) => ({
      ...current,
      enabledSkills: current.enabledSkills.includes(id)
        ? current.enabledSkills.filter((item) => item !== id)
        : [...current.enabledSkills, id]
    }));
  };
  const toggleServer = (id: string) => {
    setDraft((current) => ({
      ...current,
      enabledMcpServers: current.enabledMcpServers.includes(id)
        ? current.enabledMcpServers.filter((item) => item !== id)
        : [...current.enabledMcpServers, id]
    }));
  };
  const toggleSetting = (key: "enabled" | "mcpEnabled" | "skillsEnabled" | "allowShell") => {
    setDraft((current) => ({ ...current, [key]: !current[key] }));
  };
  return (
    <div className="primary-panel embedded-panel" style={{ padding: 0 }}>
      {/* Hero Banner */}
      <div className="agent-settings-hero">
        <div className="agent-settings-hero-info">
          <span className="agent-settings-hero-icon"><Bot size={26} /></span>
          <div className="agent-settings-hero-text">
            <strong>Agent 与 Skills</strong>
            <small>{agents.filter((a) => a.enabled).length} 个活跃智能体 · {skills.length} 个技能 · {servers.length} 个 MCP 服务器</small>
          </div>
        </div>
        <button className="btn-primary" type="button" onClick={() => void saveConfig(draft)} style={{ fontSize: 13, padding: "8px 18px", width: "auto", minWidth: "auto", marginLeft: "auto", flexShrink: 0 }}>保存设置</button>
      </div>

      {/* Agent Summary Cards */}
      <div className="agent-settings-section">
        <div className="agent-settings-section-title">
          <Sparkles size={16} /><strong>智能体概览</strong>
          <small>{agents.length} 个智能体</small>
        </div>
        <div className="agent-summary-grid">
          {agents.map((agent) => (
            <div className="agent-summary-item" key={agent.id}>
              <span className="agent-summary-icon indigo"><Bot size={18} /></span>
              <div className="agent-summary-text">
                <strong style={{ fontSize: 14 }}>{agent.name}{agent.isDefault ? " ★" : ""}</strong>
                <small>{agent.llmProvider || "跟随角色"} · {agent.llmModel || "未指定模型"}</small>
              </div>
            </div>
          ))}
        </div>
        <div style={{ marginTop: 12 }}>
          <button className="btn-secondary" type="button" onClick={() => useAppStore.getState().setSection("agents", "agent")} style={{ fontSize: 13, padding: "8px 16px" }}>
            管理智能体
          </button>
        </div>
      </div>

      {/* Capability Toggles */}
      <div className="agent-settings-section">
        <div className="agent-settings-section-title">
          <Sparkles size={16} /><strong>功能开关</strong>
        </div>
        <div className="settings-toggle-row">
          <div className="settings-toggle-info">
            <strong>Agent 能力</strong>
            <small>启用智能体自主规划和执行</small>
          </div>
          <label className="switch-wrap" onClick={(e) => e.stopPropagation()}>
            <input type="checkbox" checked={draft.enabled} onChange={() => toggleSetting("enabled")} />
            <span className="switch-track" />
          </label>
        </div>
        <div className="settings-toggle-row">
          <div className="settings-toggle-info">
            <strong>MCP 工具</strong>
            <small>允许 Agent 调用 MCP 工具</small>
          </div>
          <label className="switch-wrap" onClick={(e) => e.stopPropagation()}>
            <input type="checkbox" checked={draft.mcpEnabled} onChange={() => toggleSetting("mcpEnabled")} />
            <span className="switch-track" />
          </label>
        </div>
        <div className="settings-toggle-row">
          <div className="settings-toggle-info">
            <strong>Skills 加载</strong>
            <small>启用技能系统</small>
          </div>
          <label className="switch-wrap" onClick={(e) => e.stopPropagation()}>
            <input type="checkbox" checked={draft.skillsEnabled} onChange={() => toggleSetting("skillsEnabled")} />
            <span className="switch-track" />
          </label>
        </div>
        <div className="settings-toggle-row">
          <div className="settings-toggle-info">
            <strong>Shell 工具</strong>
            <small>允许执行 Shell 命令</small>
          </div>
          <label className="switch-wrap" onClick={(e) => e.stopPropagation()}>
            <input type="checkbox" checked={draft.allowShell} onChange={() => toggleSetting("allowShell")} />
            <span className="switch-track" />
          </label>
        </div>
      </div>

      {/* Limits */}
      <div className="agent-settings-section">
        <div className="agent-settings-section-title">
          <RefreshCw size={16} /><strong>Agent 调度限制</strong>
        </div>
        <div className="agent-form-row">
          <div className="agent-field">
            <label>最大子 Agent</label>
            <input min={1} max={32} type="number" value={draft.maxSubagents} onChange={(event) => setDraft((current) => ({ ...current, maxSubagents: Number(event.target.value) }))} />
          </div>
          <div className="agent-field">
            <label>最大子层级</label>
            <input min={1} max={4} type="number" value={draft.maxSubagentDepth ?? 1} onChange={(event) => setDraft((current) => ({ ...current, maxSubagentDepth: Number(event.target.value) }))} />
          </div>
        </div>
        <div className="agent-form-row single" style={{ marginTop: 12 }}>
          <div className="agent-field">
            <label>Skills 目录</label>
            <input value={draft.skillsDir} onChange={(event) => setDraft((current) => ({ ...current, skillsDir: event.target.value }))} placeholder="留空使用内置 skills（项目目录或打包资源目录）" />
          </div>
        </div>
        <div style={{ display: "flex", gap: 8, marginTop: 12 }}>
          <button className="btn-secondary" type="button" onClick={() => void installBuiltinSkills()} style={{ fontSize: 13, padding: "8px 16px" }}>安装默认 Skills</button>
          <button className="btn-secondary-outline" type="button" onClick={() => void refreshSkills()} style={{ fontSize: 13, padding: "8px 16px", border: "1px solid var(--divider)", borderRadius: "var(--radius-sm)", background: "transparent", color: "var(--text-2)", cursor: "pointer" }}>刷新 Skills</button>
        </div>
      </div>

      {/* MCP Servers */}
      <div className="agent-settings-section">
        <div className="agent-settings-section-title">
          <PlugZap size={16} /><strong>MCP 服务器白名单</strong>
          <small>{draft.enabledMcpServers.length}/{servers.length} 已启用</small>
        </div>
        {servers.length === 0 ? (
          <p className="form-hint">暂无 MCP Server</p>
        ) : (
          <div className="agent-toggle-grid">
            {servers.map((server) => (
              <button className={`agent-toggle-item ${draft.enabledMcpServers.includes(server.id) ? "active" : ""}`} key={server.id} type="button" onClick={() => toggleServer(server.id)}>
                <span className="agent-toggle-item-label"><PlugZap size={16} /><span>{server.name}</span></span>
                <span className="agent-toggle-dot" />
              </button>
            ))}
          </div>
        )}
      </div>

      {/* Skills */}
      <div className="agent-settings-section">
        <div className="agent-settings-section-title">
          <Sparkles size={16} /><strong>Skills</strong>
          <small>
            {draft.enabledSkills.length}/{skills.length} 已启用
            {skillSearch.trim() ? ` · ${filteredSkills.length} 匹配` : ""}
          </small>
        </div>
        {skills.length === 0 ? (
          <p className="form-hint">暂无 Skills</p>
        ) : (
          <>
            <div className="search-bar" style={{ marginBottom: 12 }}>
              <Search size={16} />
              <input
                value={skillSearch}
                onChange={(event) => setSkillSearch(event.target.value)}
                placeholder="搜索技能名称 / ID / 描述"
              />
            </div>
            {filteredSkills.length === 0 ? (
              <p className="form-hint">没有匹配的 Skills</p>
            ) : (
              <div className="agent-toggle-grid">
                {filteredSkills.map((skill) => (
                  <button className={`agent-toggle-item ${draft.enabledSkills.includes(skill.id) ? "active" : ""}`} key={skill.id} type="button" onClick={() => toggleSkill(skill.id)}>
                    <span className="agent-toggle-item-label"><Sparkles size={16} /><span>{skill.name}</span></span>
                    <span className="agent-toggle-dot" />
                  </button>
                ))}
              </div>
            )}
          </>
        )}
      </div>
    </div>
  );
}
