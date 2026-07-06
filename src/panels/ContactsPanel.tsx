import { useEffect, useMemo, useState } from "react";
import {
  BookOpen,
  Brain,
  ChevronRight,
  Edit3,
  MessageSquareText,
  Plus,
  Search,
  Smartphone,
  Upload,
  Users,
} from "lucide-react";
import { api } from "../lib/api";
import { resolvePersonaAgentBinding } from "../lib/personaAgentBinding";
import { useAppStore } from "../lib/store";
import type { Persona } from "../lib/types";
import { Avatar, MenuRow } from "../components/common";
import { PersonaMemoryManager } from "../components/PersonaMemoryManager";

export function ContactsPanel() {
  const {
    personas,
    accounts,
    config,
    memories,
    setSection,
    saveConfig,
    savePersona,
    refreshMemories,
    deleteMemory,
    openPersonaConversation,
    linkWechatAccount,
    unlinkWechatAccount,
    refreshAccounts,
  } = useAppStore();
  const agents = useAppStore((state) => state.agents);
  const llmProviders = useAppStore((state) => state.llmProviders);
  const [query, setQuery] = useState("");
  const [selectedPersonaId, setSelectedPersonaId] = useState(personas[0]?.id ?? "");
  const [detailView, setDetailView] = useState<"profile" | "memory">("profile");
  const [showWechatSheet, setShowWechatSheet] = useState(false);
  const [pollStatus, setPollStatus] = useState("");

  const personaBindings = useMemo(
    () => new Map(personas.map((persona) => [persona.id, resolvePersonaAgentBinding(persona, agents, llmProviders)])),
    [agents, llmProviders, personas],
  );

  const visiblePersonas = personas;
  useEffect(() => {
    if (visiblePersonas.some((persona) => persona.id === selectedPersonaId)) return;
    setSelectedPersonaId(visiblePersonas[0]?.id ?? "");
  }, [selectedPersonaId, visiblePersonas]);

  const filtered = visiblePersonas.filter((persona) =>
    (personaBindings.get(persona.id)?.searchText ?? `${persona.name} ${persona.id}`.toLowerCase()).includes(query.toLowerCase()),
  );
  const selectedPersona = visiblePersonas.find((p) => p.id === selectedPersonaId) ?? visiblePersonas[0] ?? null;
  const linkedAccount = selectedPersona ? accounts.find((account) => account.linkedPersona === selectedPersona.id) : null;
  const selectedBinding = selectedPersona ? personaBindings.get(selectedPersona.id) : null;
  const selectedMemories = selectedPersona ? memories.filter((memory) => memory.personaId === selectedPersona.id) : [];
  const persistentMemories = selectedMemories.filter((memory) => (memory.target ?? "memory") !== "session");
  const sessionMemories = selectedMemories.filter((memory) => (memory.target ?? "memory") === "session");

  const saveChatConfig = async (patch: Partial<NonNullable<typeof config>["chat"]>) => {
    if (!config) return;
    await saveConfig({ ...config, chat: { ...config.chat, ...patch } });
  };

  const updatePersonaMemory = async (memory: NonNullable<Persona["memory"]>) => {
    if (!selectedPersona) return;
    await savePersona({ ...selectedPersona, memory });
  };

  const removeMemoryEntry = async (memoryId: string) => {
    await deleteMemory(memoryId);
    if (selectedPersona) await refreshMemories(selectedPersona.id);
  };

  useEffect(() => {
    if (!selectedPersona) return;
    void refreshMemories(selectedPersona.id);
  }, [refreshMemories, selectedPersona?.id]);

  useEffect(() => {
    setDetailView("profile");
  }, [selectedPersonaId]);

  return (
    <section className="tab-split">
      <aside className="side-panel tab-list-panel">
        <div className="side-title">
          <h3>通讯录</h3>
          <div className="title-actions">
            <button title="导入角色" type="button"><Upload size={16} /></button>
            <button onClick={() => setSection("personas")} title="新建角色" type="button"><Plus size={16} /></button>
          </div>
        </div>
        <div className="search-bar">
          <Search size={17} />
          <input value={query} onChange={(e) => setQuery(e.target.value)} placeholder="搜索" />
        </div>
        <div className="card-list">
          {filtered.map((persona) => {
            const binding = personaBindings.get(persona.id);
            return (
              <button
                className={persona.id === selectedPersonaId ? "contact-row active" : "contact-row"}
                key={persona.id}
                onClick={() => setSelectedPersonaId(persona.id)}
                type="button"
              >
                <Avatar name={persona.name} src={persona.avatarPath ? api.assetUrl(persona.avatarPath) : ""} />
                <span>
                  <strong>{persona.name}</strong>
                  <small>{binding?.infoText ?? "未配置服务商"}</small>
                </span>
              </button>
            );
          })}
        </div>
      </aside>
      <article className="primary-panel">
        <div className="panel-title">
          <span>Contacts</span>
          <strong>{selectedPersona?.name ?? "角色详情"}</strong>
        </div>
        {selectedPersona ? (
          detailView === "memory" ? (
            <div className="contact-memory-detail">
              <div className="panel-title action-title contact-memory-title">
                <button className="icon-only-btn" onClick={() => setDetailView("profile")} title="返回资料" type="button">
                  <ChevronRight size={19} style={{ transform: "rotate(180deg)" }} />
                </button>
                <div className="panel-title-text"><span>{selectedPersona.name}</span><strong>记忆管理</strong></div>
              </div>
              <PersonaMemoryManager
                bindingModel={selectedBinding?.model ?? ""}
                bindingProviderName={selectedBinding?.providerName ?? ""}
                chatConfig={config?.chat ?? null}
                onDeleteMemory={removeMemoryEntry}
                onRefresh={() => void refreshMemories(selectedPersona.id)}
                onSaveChatConfig={saveChatConfig}
                onUpdateMemory={updatePersonaMemory}
                onViewAll={() => setSection("memory")}
                persistentMemories={persistentMemories}
                personaMemory={selectedPersona.memory}
                sessionMemories={sessionMemories}
              />
            </div>
          ) : (
            <div className="profile-detail">
              <Avatar name={selectedPersona.name} src={selectedPersona.avatarPath ? api.assetUrl(selectedPersona.avatarPath) : ""} size="large" />
              <h2>{selectedPersona.name}</h2>
              <p className="persona-id-text">{selectedPersona.id}</p>
              <div className="menu-card">
                <MenuRow icon={MessageSquareText} label="发消息" value="进入会话"
                  onClick={() => void openPersonaConversation(selectedPersona.id).then(() => setSection("chat"))} />
                <MenuRow icon={Smartphone} label="链接微信"
                  value={linkedAccount ? (linkedAccount.note || "已链接") : "未链接"}
                  onClick={() => setShowWechatSheet(true)} iconColor="green" />
                <MenuRow icon={Brain} label="记忆管理" value="长期与会话" onClick={() => setDetailView("memory")} />
                <MenuRow icon={BookOpen} label="世界书" value="绑定与查看" onClick={() => setSection("worldbooks")} />
                <MenuRow icon={Edit3} label="编辑角色" value="人设与模型" onClick={() => setSection("personas")} />
              </div>
              {pollStatus ? <p className="form-hint">{pollStatus}</p> : null}
              {showWechatSheet ? (
                <div className="sheet-backdrop" onClick={() => setShowWechatSheet(false)}>
                  <div className="action-sheet" onClick={(e) => e.stopPropagation()}>
                    <div className="sheet-title">链接微信账号</div>
                    {accounts.length === 0 ? (
                      <p className="form-hint">暂无已登录微信账号，请先到设置 &gt; 微信账号扫码登录。</p>
                    ) : (
                      accounts.map((account) => {
                        const occupied = account.linkedPersona && account.linkedPersona !== selectedPersona.id;
                        const occupiedPersona = personas.find((persona) => persona.id === account.linkedPersona);
                        const isDisabled = Boolean(occupied) || !account.online;
                        return (
                          <button className="sheet-item" disabled={isDisabled} key={account.id} type="button"
                            onClick={() => void linkWechatAccount(selectedPersona.id, account.id).then(() => setShowWechatSheet(false))}>
                            <span>{account.note || account.id}</span>
                            <small className={occupied ? "status-text-muted" : account.online ? "status-text-online" : "status-text-muted"}>
                              {occupied ? `已链接到 ${occupiedPersona?.name ?? account.linkedPersona}` : account.online ? "在线" : "离线"}
                            </small>
                          </button>
                        );
                      })
                    )}
                    <div style={{ display: "flex", gap: "12px", padding: "8px 0" }}>
                      {linkedAccount ? (
                        <button className="sheet-cancel btn-danger-text" type="button" style={{ flex: 1 }}
                          onClick={() => void unlinkWechatAccount(selectedPersona.id).then(() => setShowWechatSheet(false))}>
                          断开
                        </button>
                      ) : null}
                      <button className="sheet-cancel" onClick={() => setShowWechatSheet(false)} type="button" style={{ flex: 1 }}>取消</button>
                    </div>
                  </div>
                </div>
              ) : null}
            </div>
          )
        ) : (
          <div className="empty-state">
            <Users size={36} />
            <h2>还没有角色</h2>
            <button onClick={() => setSection("personas")} type="button">新建角色</button>
          </div>
        )}
      </article>
    </section>
  );
}

