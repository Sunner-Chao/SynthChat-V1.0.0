import {
  BookOpen,
  Bot,
  Check,
  Heart,
  LoaderCircle,
  MessageSquareText,
  Newspaper,
  Plus,
  RefreshCw,
  Save,
  Search,
  Send,
  Smartphone,
  Trash2,
  UserRoundCog,
  Users,
} from "lucide-react";
import {
  useEffect,
  useMemo,
  useState,
  type FormEvent,
  type ReactNode,
} from "react";
import {
  productCatalogApi,
  type Moment,
  type Persona,
  type PersonaInput,
  type ProductCatalogApi,
  type Worldbook,
  type WorldbookInput,
} from "../../api/productCatalog";
import {
  profilesApi,
  type Capabilities,
  type ProfileSummary,
  type ProfilesApi,
} from "../../api/profiles";
import { sessionsApi, type SessionsApi } from "../../api/sessions";
import type { ProductDestination } from "./ProductWorkspaces";
import "./catalog.css";

type ProfileClient = Pick<ProfilesApi, "getCapabilities" | "listProfiles">;

interface ProductClients {
  client?: ProductCatalogApi;
  profileClient?: ProfileClient;
}

interface ContactsWorkspaceProps extends ProductClients {
  onNavigate: (destination: ProductDestination) => void;
  onOpenSession?: (session: { id: string; title: string; personaId: string }) => void;
  sessionClient?: Pick<SessionsApi, "createSession">;
}

type CatalogCapability = "personas" | "moments" | "worldbooks";
type ProductKind = "persona" | "moment" | "worldbook";

interface CatalogRuntime {
  available: boolean | null;
  error: string | null;
  loading: boolean;
  profileId: string | null;
  profiles: ProfileSummary[];
  refreshEpoch: number;
  selectProfile(profileId: string): void;
  refresh(): void;
}

function useCatalogRuntime(
  capability: CatalogCapability,
  profileClient: ProfileClient,
): CatalogRuntime {
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [profileId, setProfileId] = useState<string | null>(null);
  const [available, setAvailable] = useState<boolean | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [refreshEpoch, setRefreshEpoch] = useState(0);

  useEffect(() => {
    const controller = new AbortController();
    setLoading(true);
    setError(null);
    void Promise.all([
      profileClient.getCapabilities({ signal: controller.signal }),
      profileClient.listProfiles({ signal: controller.signal }),
    ])
      .then(([capabilities, items]) => {
        if (controller.signal.aborted) return;
        setAvailable(capabilityEnabled(capabilities, capability));
        setProfiles(items);
        setProfileId((current) => (
          current && items.some((item) => item.id === current)
            ? current
            : items.find((item) => item.isActive)?.id ?? items[0]?.id ?? null
        ));
      })
      .catch((cause: unknown) => {
        if (!controller.signal.aborted) {
          setError(errorMessage(cause, "无法加载产品目录能力。"));
        }
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false);
      });
    return () => controller.abort();
  }, [capability, profileClient]);

  return {
    available,
    error,
    loading,
    profileId,
    profiles,
    refreshEpoch,
    selectProfile: setProfileId,
    refresh: () => setRefreshEpoch((value) => value + 1),
  };
}

function capabilityEnabled(capabilities: Capabilities, key: CatalogCapability): boolean {
  return (capabilities.extensions as Record<string, unknown>)[key] === true;
}

function errorMessage(cause: unknown, fallback: string): string {
  return cause instanceof Error && cause.message.trim() ? cause.message : fallback;
}

function productEtag(kind: ProductKind, revision: number): string {
  return `"product-${kind}-${revision}"`;
}

function idempotencyKey(prefix: string): string {
  return `${prefix}-${crypto.randomUUID()}`;
}

function CatalogToolbar({
  busy,
  eyebrow,
  onRefresh,
  onSelectProfile,
  profileId,
  profiles,
  title,
}: {
  busy: boolean;
  eyebrow: string;
  onRefresh(): void;
  onSelectProfile(profileId: string): void;
  profileId: string | null;
  profiles: ProfileSummary[];
  title: string;
}) {
  return (
    <header className="catalog-toolbar">
      <div><small>{eyebrow}</small><h2>{title}</h2></div>
      <div className="catalog-toolbar__actions">
        <label><span>Profile</span><select aria-label={`${title} Profile`} disabled={busy || profiles.length < 2} onChange={(event) => onSelectProfile(event.target.value)} value={profileId ?? ""}>{profiles.map((profile) => <option key={profile.id} value={profile.id}>{profile.displayName}</option>)}</select></label>
        <button aria-label={`刷新${title}`} disabled={busy} onClick={onRefresh} title="刷新" type="button"><RefreshCw className={busy ? "spin" : undefined} size={16} /></button>
        <span className="product-capability-pill is-available"><Check size={12} />已接入</span>
      </div>
    </header>
  );
}

function CatalogGate({ runtime, children }: { runtime: CatalogRuntime; children: ReactNode }) {
  if (runtime.loading || runtime.available === null) {
    return <div className="catalog-state" role="status"><LoaderCircle className="spin" size={19} />正在加载 Rust 产品目录</div>;
  }
  if (runtime.error) return <div className="catalog-state is-error" role="alert">{runtime.error}</div>;
  if (!runtime.available) return <div className="catalog-state">当前 Rust 后端未启用此产品目录。</div>;
  if (!runtime.profileId) return <div className="catalog-state">暂无 Profile，请先在设置中创建 Profile。</div>;
  return children;
}

function InitialAvatar({ persona, size = "normal" }: { persona: Persona; size?: "normal" | "large" }) {
  if (persona.avatar) {
    return <img alt="" className={`catalog-avatar is-${size}`} src={persona.avatar} />;
  }
  return <span aria-hidden="true" className={`catalog-avatar is-${size}`}>{Array.from(persona.name.trim())[0] ?? "角"}</span>;
}

export function ContactsWorkspace({
  client = productCatalogApi,
  onNavigate,
  onOpenSession,
  profileClient = profilesApi,
  sessionClient = sessionsApi,
}: ContactsWorkspaceProps) {
  const runtime = useCatalogRuntime("personas", profileClient);
  const [personas, setPersonas] = useState<Persona[]>([]);
  const [query, setQuery] = useState("");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);

  useEffect(() => {
    if (!runtime.available || !runtime.profileId) return undefined;
    const controller = new AbortController();
    setBusy(true);
    setMessage(null);
    void client.listPersonas(runtime.profileId, query.trim() || undefined, { signal: controller.signal })
      .then((items) => {
        if (controller.signal.aborted) return;
        setPersonas(items);
        setSelectedId((current) => items.some((item) => item.id === current) ? current : items[0]?.id ?? null);
      })
      .catch((cause: unknown) => {
        if (!controller.signal.aborted) setMessage(errorMessage(cause, "无法加载通讯录。"));
      })
      .finally(() => {
        if (!controller.signal.aborted) setBusy(false);
      });
    return () => controller.abort();
  }, [client, query, runtime.available, runtime.profileId, runtime.refreshEpoch]);

  const selected = personas.find((persona) => persona.id === selectedId) ?? null;

  const startConversation = async () => {
    if (!selected || !runtime.profileId || busy) return;
    setBusy(true);
    setMessage(null);
    try {
      const session = await sessionClient.createSession({
        profileId: runtime.profileId,
        personaId: selected.id,
        title: `与 ${selected.name} 对话`,
      }, idempotencyKey("persona-session"));
      if (!session.value.personaId) throw new Error("创建的角色会话缺少 Persona 绑定。");
      onOpenSession?.({
        id: session.value.id,
        title: session.value.title,
        personaId: session.value.personaId,
      });
      onNavigate("chat");
    } catch (cause) {
      setMessage(errorMessage(cause, "无法创建角色会话。"));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="product-split catalog-workspace" aria-label="通讯录产品面板">
      <CatalogGate runtime={runtime}>
        <aside className="product-list-pane">
          <div className="product-list-heading"><div><span>CONTACTS</span><strong>通讯录</strong></div><button aria-label="新建角色" onClick={() => onNavigate("personas")} title="新建角色" type="button"><Plus size={16} /></button></div>
          <label className="product-search-field"><Search aria-hidden="true" size={16} /><input aria-label="搜索通讯录" onChange={(event) => setQuery(event.target.value)} placeholder="搜索名称、模型或设定" type="search" value={query} /></label>
          <div className="catalog-list" aria-busy={busy || undefined}>
            {personas.length === 0 ? <div className="catalog-state is-compact"><Users size={22} />还没有角色</div> : personas.map((persona) => (
              <button aria-current={selected?.id === persona.id ? "true" : undefined} className={selected?.id === persona.id ? "is-active" : undefined} key={persona.id} onClick={() => setSelectedId(persona.id)} type="button"><InitialAvatar persona={persona} /><span><strong>{persona.name}</strong><small>{persona.model || "使用 Profile 模型"}</small></span></button>
            ))}
          </div>
        </aside>
        <article className="product-detail-pane">
          <CatalogToolbar busy={busy} eyebrow="CONTACT PROFILE" onRefresh={runtime.refresh} onSelectProfile={runtime.selectProfile} profileId={runtime.profileId} profiles={runtime.profiles} title="角色详情" />
          {selected ? (
            <div className="product-contact-profile">
              <InitialAvatar persona={selected} size="large" />
              <h3>{selected.name}</h3><p>{selected.characterPrompt || selected.systemPrompt}</p>
              <div className="catalog-tags"><span>{selected.provider || "Profile Provider"}</span><span>{selected.model || "Profile Model"}</span>{selected.memoryEnabled ? <span>记忆</span> : null}{selected.toolsEnabled ? <span>工具</span> : null}</div>
              <div className="product-menu-card">
                <button aria-label="发消息" className="product-menu-row" disabled={busy} onClick={() => void startConversation()} type="button"><span className="product-menu-icon"><MessageSquareText size={17} /></span><span className="product-menu-copy"><strong>发消息</strong><small>进入新会话</small></span><Send size={16} /></button>
                <button className="product-menu-row" onClick={() => onNavigate("settings")} type="button"><span className="product-menu-icon"><Smartphone size={17} /></span><span className="product-menu-copy"><strong>微信账号</strong><small>管理登录与连接</small></span></button>
                <button className="product-menu-row" onClick={() => onNavigate("worldbooks")} type="button"><span className="product-menu-icon"><BookOpen size={17} /></span><span className="product-menu-copy"><strong>世界书</strong><small>查看绑定设定</small></span></button>
                <button className="product-menu-row" onClick={() => onNavigate("personas")} type="button"><span className="product-menu-icon"><UserRoundCog size={17} /></span><span className="product-menu-copy"><strong>编辑角色</strong><small>人设与模型参数</small></span></button>
              </div>
              {message ? <p className="catalog-message" role="status">{message}</p> : null}
            </div>
          ) : <div className="catalog-state"><Users size={24} />请选择或新建角色</div>}
        </article>
      </CatalogGate>
    </section>
  );
}

const PERSONA_TABS = ["基础资料", "角色设定", "行为", "模型与工具"] as const;
type PersonaTab = typeof PERSONA_TABS[number];

const EMPTY_PERSONA: PersonaInput = {
  name: "",
  avatar: null,
  systemPrompt: "你正在扮演这个角色，请保持设定一致并自然交流。",
  characterPrompt: "",
  outputExamples: "",
  systemInstructions: "请始终保持角色一致性，结合角色详情、世界书与长期记忆作答。",
  provider: "",
  model: "",
  temperature: 0.8,
  maxTokens: 2048,
  toolsEnabled: true,
  memoryEnabled: true,
  proactiveEnabled: false,
  legacyAgentId: null,
};

function inputFromPersona(persona: Persona): PersonaInput {
  return {
    name: persona.name,
    avatar: persona.avatar,
    systemPrompt: persona.systemPrompt,
    characterPrompt: persona.characterPrompt,
    outputExamples: persona.outputExamples,
    systemInstructions: persona.systemInstructions,
    provider: persona.provider,
    model: persona.model,
    temperature: persona.temperature,
    maxTokens: persona.maxTokens,
    toolsEnabled: persona.toolsEnabled,
    memoryEnabled: persona.memoryEnabled,
    proactiveEnabled: persona.proactiveEnabled,
    legacyAgentId: persona.legacyAgentId,
  };
}

export function PersonasWorkspace({ client = productCatalogApi, profileClient = profilesApi }: ProductClients) {
  const runtime = useCatalogRuntime("personas", profileClient);
  const [items, setItems] = useState<Persona[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [draft, setDraft] = useState<PersonaInput>(EMPTY_PERSONA);
  const [tab, setTab] = useState<PersonaTab>("基础资料");
  const [query, setQuery] = useState("");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);

  const load = (signal?: AbortSignal) => {
    if (!runtime.profileId) return Promise.resolve();
    setBusy(true);
    return client.listPersonas(runtime.profileId, query.trim() || undefined, { signal })
      .then((personas) => {
        if (signal?.aborted) return;
        setItems(personas);
        const selected = personas.find((item) => item.id === selectedId) ?? personas[0] ?? null;
        setSelectedId(selected?.id ?? null);
        setDraft(selected ? inputFromPersona(selected) : { ...EMPTY_PERSONA });
      })
      .catch((cause: unknown) => {
        if (!signal?.aborted) setMessage(errorMessage(cause, "无法加载角色目录。"));
      })
      .finally(() => {
        if (!signal?.aborted) setBusy(false);
      });
  };

  useEffect(() => {
    if (!runtime.available || !runtime.profileId) return undefined;
    const controller = new AbortController();
    setMessage(null);
    void load(controller.signal);
    return () => controller.abort();
    // selectedId is intentionally excluded: selection should not reload the list.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client, query, runtime.available, runtime.profileId, runtime.refreshEpoch]);

  const selectPersona = (persona: Persona) => {
    setSelectedId(persona.id);
    setDraft(inputFromPersona(persona));
    setMessage(null);
  };

  const startNew = () => {
    setSelectedId(null);
    setDraft({ ...EMPTY_PERSONA });
    setTab("基础资料");
    setMessage(null);
  };

  const save = async (event: FormEvent) => {
    event.preventDefault();
    if (!runtime.profileId || busy || !draft.name.trim()) return;
    setBusy(true);
    setMessage(null);
    try {
      const current = items.find((item) => item.id === selectedId);
      const saved = current
        ? await client.updatePersona(runtime.profileId, current.id, draft, productEtag("persona", current.revision))
        : await client.createPersona(runtime.profileId, draft);
      setItems((previous) => [saved.value, ...previous.filter((item) => item.id !== saved.value.id)]);
      setSelectedId(saved.value.id);
      setDraft(inputFromPersona(saved.value));
      setMessage(current ? "角色已保存。" : "角色已创建。");
    } catch (cause) {
      setMessage(errorMessage(cause, "角色保存失败。"));
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    const current = items.find((item) => item.id === selectedId);
    if (!runtime.profileId || !current || busy) return;
    setBusy(true);
    setMessage(null);
    try {
      await client.deletePersona(runtime.profileId, current.id, productEtag("persona", current.revision));
      const remaining = items.filter((item) => item.id !== current.id);
      setItems(remaining);
      setSelectedId(remaining[0]?.id ?? null);
      setDraft(remaining[0] ? inputFromPersona(remaining[0]) : { ...EMPTY_PERSONA });
      setMessage("角色已删除。" );
    } catch (cause) {
      setMessage(errorMessage(cause, "角色删除失败；请先解除世界书绑定。"));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="product-split catalog-workspace" aria-label="角色产品面板">
      <CatalogGate runtime={runtime}>
        <aside className="product-list-pane">
          <div className="product-list-heading"><div><span>PERSONAS</span><strong>角色</strong></div><button aria-label="新建角色" disabled={busy} onClick={startNew} title="新建角色" type="button"><Plus size={16} /></button></div>
          <label className="product-search-field"><Search size={16} /><input aria-label="搜索角色" onChange={(event) => setQuery(event.target.value)} placeholder="搜索角色" type="search" value={query} /></label>
          <div className="catalog-list">{items.length === 0 ? <div className="catalog-state is-compact"><Bot size={22} />还没有角色</div> : items.map((persona) => <button aria-current={persona.id === selectedId ? "true" : undefined} className={persona.id === selectedId ? "is-active" : undefined} key={persona.id} onClick={() => selectPersona(persona)} type="button"><InitialAvatar persona={persona} /><span><strong>{persona.name}</strong><small>{persona.model || "Profile 模型"}</small></span></button>)}</div>
        </aside>
        <article className="product-detail-pane">
          <CatalogToolbar busy={busy} eyebrow="PERSONA EDITOR" onRefresh={runtime.refresh} onSelectProfile={runtime.selectProfile} profileId={runtime.profileId} profiles={runtime.profiles} title={selectedId ? "编辑角色" : "新建角色"} />
          <div className="product-tabs" role="tablist" aria-label="角色设置分类">{PERSONA_TABS.map((item) => <button aria-selected={tab === item} className={tab === item ? "is-active" : undefined} key={item} onClick={() => setTab(item)} role="tab" type="button">{item}</button>)}</div>
          <form className="product-form-shell catalog-form" onSubmit={(event) => void save(event)}>
            {tab === "基础资料" ? <><div className="product-form-grid"><label><span>名称</span><input aria-label="角色名称" maxLength={120} onChange={(event) => setDraft((value) => ({ ...value, name: event.target.value }))} required value={draft.name} /></label><label><span>头像 URL</span><input aria-label="头像 URL" onChange={(event) => setDraft((value) => ({ ...value, avatar: event.target.value || null }))} value={draft.avatar ?? ""} /></label></div><label><span>系统提示词</span><textarea aria-label="系统提示词" onChange={(event) => setDraft((value) => ({ ...value, systemPrompt: event.target.value }))} value={draft.systemPrompt ?? ""} /></label></> : null}
            {tab === "角色设定" ? <><label><span>角色设定</span><textarea aria-label="角色设定" className="is-tall" onChange={(event) => setDraft((value) => ({ ...value, characterPrompt: event.target.value }))} value={draft.characterPrompt ?? ""} /></label><label><span>输出示例</span><textarea aria-label="输出示例" onChange={(event) => setDraft((value) => ({ ...value, outputExamples: event.target.value }))} value={draft.outputExamples ?? ""} /></label><label><span>约束指令</span><textarea aria-label="约束指令" onChange={(event) => setDraft((value) => ({ ...value, systemInstructions: event.target.value }))} value={draft.systemInstructions ?? ""} /></label></> : null}
            {tab === "行为" ? <><div className="product-form-grid"><label><span>Temperature</span><input aria-label="Temperature" max={2} min={0} onChange={(event) => setDraft((value) => ({ ...value, temperature: Number(event.target.value) }))} step={0.1} type="number" value={draft.temperature ?? 0.8} /></label><label><span>最大 Tokens</span><input aria-label="最大 Tokens" min={1} onChange={(event) => setDraft((value) => ({ ...value, maxTokens: Number(event.target.value) }))} type="number" value={draft.maxTokens ?? 2048} /></label></div><div className="catalog-toggle-grid"><ToggleField checked={draft.memoryEnabled !== false} label="长期记忆" onChange={(checked) => setDraft((value) => ({ ...value, memoryEnabled: checked }))} /><ToggleField checked={draft.proactiveEnabled === true} label="主动消息" onChange={(checked) => setDraft((value) => ({ ...value, proactiveEnabled: checked }))} /></div></> : null}
            {tab === "模型与工具" ? <><div className="product-form-grid"><label><span>Provider 覆盖</span><input aria-label="Provider 覆盖" onChange={(event) => setDraft((value) => ({ ...value, provider: event.target.value }))} placeholder="留空使用 Profile" value={draft.provider ?? ""} /></label><label><span>模型覆盖</span><input aria-label="模型覆盖" onChange={(event) => setDraft((value) => ({ ...value, model: event.target.value }))} placeholder="留空使用 Profile" value={draft.model ?? ""} /></label></div><ToggleField checked={draft.toolsEnabled !== false} label="允许工具" onChange={(checked) => setDraft((value) => ({ ...value, toolsEnabled: checked }))} /></> : null}
            {message ? <p className="catalog-message" role="status">{message}</p> : null}
            <div className="catalog-form-actions">{selectedId ? <button aria-label="删除角色" className="catalog-danger-button" disabled={busy} onClick={() => void remove()} title="删除角色" type="button"><Trash2 size={16} /></button> : null}<button className="product-primary-button" disabled={busy || !draft.name.trim()} type="submit">{busy ? <LoaderCircle className="spin" size={16} /> : <Save size={16} />}保存角色</button></div>
          </form>
        </article>
      </CatalogGate>
    </section>
  );
}

function ToggleField({ checked, label, onChange }: { checked: boolean; label: string; onChange(checked: boolean): void }) {
  return <label className="catalog-toggle"><input checked={checked} onChange={(event) => onChange(event.target.checked)} type="checkbox" /><span>{label}</span></label>;
}

export function MomentsWorkspace({ client = productCatalogApi, profileClient = profilesApi }: ProductClients) {
  const runtime = useCatalogRuntime("moments", profileClient);
  const [moments, setMoments] = useState<Moment[]>([]);
  const [draft, setDraft] = useState("");
  const [commentDrafts, setCommentDrafts] = useState<Record<string, string>>({});
  const [busyId, setBusyId] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  useEffect(() => {
    if (!runtime.available || !runtime.profileId) return undefined;
    const controller = new AbortController();
    setBusyId("load");
    setMessage(null);
    void client.listMoments(runtime.profileId, { signal: controller.signal })
      .then((items) => { if (!controller.signal.aborted) setMoments(items); })
      .catch((cause: unknown) => { if (!controller.signal.aborted) setMessage(errorMessage(cause, "无法加载朋友圈。")); })
      .finally(() => { if (!controller.signal.aborted) setBusyId(null); });
    return () => controller.abort();
  }, [client, runtime.available, runtime.profileId, runtime.refreshEpoch]);

  const publish = async (event: FormEvent) => {
    event.preventDefault();
    if (!runtime.profileId || !draft.trim() || busyId) return;
    setBusyId("create");
    setMessage(null);
    try {
      const created = await client.createMoment(runtime.profileId, { body: draft.trim(), authorId: "user" });
      setMoments((items) => [created.value, ...items]);
      setDraft("");
    } catch (cause) {
      setMessage(errorMessage(cause, "动态发布失败。"));
    } finally {
      setBusyId(null);
    }
  };

  const replaceMoment = (next: Moment) => setMoments((items) => items.map((item) => item.id === next.id ? next : item));
  const setLike = async (moment: Moment) => {
    if (!runtime.profileId || busyId) return;
    setBusyId(moment.id);
    try {
      const result = await client.setMomentLike(runtime.profileId, moment.id, { actorId: "user", liked: !moment.likedBy.includes("user") }, productEtag("moment", moment.revision));
      replaceMoment(result.value);
    } catch (cause) { setMessage(errorMessage(cause, "点赞更新失败。")); } finally { setBusyId(null); }
  };
  const addComment = async (moment: Moment) => {
    const text = commentDrafts[moment.id]?.trim();
    if (!runtime.profileId || !text || busyId) return;
    setBusyId(moment.id);
    try {
      const result = await client.addMomentComment(runtime.profileId, moment.id, { authorId: "user", text }, productEtag("moment", moment.revision));
      replaceMoment(result.value);
      setCommentDrafts((items) => ({ ...items, [moment.id]: "" }));
    } catch (cause) { setMessage(errorMessage(cause, "评论发布失败。")); } finally { setBusyId(null); }
  };
  const removeMoment = async (moment: Moment) => {
    if (!runtime.profileId || busyId) return;
    setBusyId(moment.id);
    try {
      await client.deleteMoment(runtime.profileId, moment.id, productEtag("moment", moment.revision));
      setMoments((items) => items.filter((item) => item.id !== moment.id));
    } catch (cause) { setMessage(errorMessage(cause, "动态删除失败。")); } finally { setBusyId(null); }
  };
  const removeComment = async (moment: Moment, commentId: string) => {
    if (!runtime.profileId || busyId) return;
    setBusyId(moment.id);
    try {
      const result = await client.deleteMomentComment(runtime.profileId, moment.id, commentId, productEtag("moment", moment.revision));
      replaceMoment(result.value);
    } catch (cause) { setMessage(errorMessage(cause, "评论删除失败。")); } finally { setBusyId(null); }
  };

  return (
    <section className="product-page catalog-workspace" aria-label="朋友圈产品面板">
      <CatalogGate runtime={runtime}>
        <CatalogToolbar busy={busyId !== null} eyebrow="MOMENTS" onRefresh={runtime.refresh} onSelectProfile={runtime.selectProfile} profileId={runtime.profileId} profiles={runtime.profiles} title="朋友圈" />
        <div className="product-page-body is-feed catalog-feed">
          <form className="product-composer" onSubmit={(event) => void publish(event)}><textarea aria-label="朋友圈正文" maxLength={16000} onChange={(event) => setDraft(event.target.value)} placeholder="写一条朋友圈动态..." value={draft} /><button disabled={busyId !== null || !draft.trim()} type="submit"><Plus size={16} />发布</button></form>
          {message ? <p className="catalog-message" role="status">{message}</p> : null}
          {moments.length === 0 ? <div className="catalog-state"><Newspaper size={24} />还没有动态</div> : moments.map((moment) => (
            <article className="catalog-moment" key={moment.id}>
              <header><span className="catalog-avatar">{Array.from(moment.authorId)[0] ?? "U"}</span><div><strong>{moment.authorId === "user" ? "我" : moment.authorId}</strong><small>{new Date(moment.createdAt).toLocaleString("zh-CN")}</small></div><button aria-label="删除动态" disabled={busyId !== null} onClick={() => void removeMoment(moment)} title="删除动态" type="button"><Trash2 size={15} /></button></header>
              <p>{moment.body}</p>
              <div className="catalog-moment__actions"><button aria-label={`${moment.likedBy.includes("user") ? "取消点赞" : "点赞"} ${moment.body}`} aria-pressed={moment.likedBy.includes("user")} className={moment.likedBy.includes("user") ? "is-active" : undefined} disabled={busyId !== null} onClick={() => void setLike(moment)} type="button"><Heart fill={moment.likedBy.includes("user") ? "currentColor" : "none"} size={15} />{moment.likedBy.length}</button></div>
              {moment.comments.length > 0 ? <ul className="catalog-comments">{moment.comments.map((comment) => <li key={comment.id}><span><strong>{comment.authorId === "user" ? "我" : comment.authorId}</strong>{comment.text}</span><button aria-label="删除评论" disabled={busyId !== null} onClick={() => void removeComment(moment, comment.id)} title="删除评论" type="button"><Trash2 size={13} /></button></li>)}</ul> : null}
              <div className="catalog-comment-composer"><input aria-label={`评论 ${moment.body}`} maxLength={16000} onChange={(event) => setCommentDrafts((items) => ({ ...items, [moment.id]: event.target.value }))} placeholder="写评论..." value={commentDrafts[moment.id] ?? ""} /><button aria-label={`发送评论 ${moment.body}`} disabled={busyId !== null || !commentDrafts[moment.id]?.trim()} onClick={() => void addComment(moment)} title="发送评论" type="button"><Send size={15} /></button></div>
            </article>
          ))}
        </div>
      </CatalogGate>
    </section>
  );
}

const EMPTY_WORLDBOOK: WorldbookInput = { name: "", description: "", boundPersonaIds: [], sections: [] };

export function WorldbooksWorkspace({ client = productCatalogApi, profileClient = profilesApi }: ProductClients) {
  const runtime = useCatalogRuntime("worldbooks", profileClient);
  const [books, setBooks] = useState<Worldbook[]>([]);
  const [personas, setPersonas] = useState<Persona[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [draft, setDraft] = useState<WorldbookInput>(EMPTY_WORLDBOOK);
  const [sectionKey, setSectionKey] = useState("");
  const [sectionContent, setSectionContent] = useState("");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);

  useEffect(() => {
    if (!runtime.available || !runtime.profileId) return undefined;
    const controller = new AbortController();
    setBusy(true);
    setMessage(null);
    void Promise.all([
      client.listWorldbooks(runtime.profileId, undefined, { signal: controller.signal }),
      client.listPersonas(runtime.profileId, undefined, { signal: controller.signal }),
    ]).then(([nextBooks, nextPersonas]) => {
      if (controller.signal.aborted) return;
      setBooks(nextBooks);
      setPersonas(nextPersonas);
    }).catch((cause: unknown) => {
      if (!controller.signal.aborted) setMessage(errorMessage(cause, "无法加载世界书。"));
    }).finally(() => { if (!controller.signal.aborted) setBusy(false); });
    return () => controller.abort();
  }, [client, runtime.available, runtime.profileId, runtime.refreshEpoch]);

  const editBook = (book: Worldbook) => {
    setSelectedId(book.id);
    setDraft({
      name: book.name,
      description: book.description,
      boundPersonaIds: book.boundPersonaIds,
      sections: book.sections.map((section) => ({ key: section.key, content: section.content, enabled: section.enabled })),
    });
    setSectionKey("");
    setSectionContent("");
    setMessage(null);
  };
  const startNew = () => { setSelectedId(null); setDraft({ ...EMPTY_WORLDBOOK, boundPersonaIds: [], sections: [] }); setSectionKey(""); setSectionContent(""); setMessage(null); };
  const save = async (event: FormEvent) => {
    event.preventDefault();
    if (!runtime.profileId || busy || !draft.name.trim()) return;
    const input: WorldbookInput = {
      ...draft,
      sections: [
        ...(draft.sections ?? []),
        ...(sectionKey.trim() && sectionContent.trim() ? [{ key: sectionKey.trim(), content: sectionContent.trim(), enabled: true }] : []),
      ],
    };
    setBusy(true);
    setMessage(null);
    try {
      const current = books.find((book) => book.id === selectedId);
      const saved = current
        ? await client.updateWorldbook(runtime.profileId, current.id, input, productEtag("worldbook", current.revision))
        : await client.createWorldbook(runtime.profileId, input);
      setBooks((items) => [saved.value, ...items.filter((item) => item.id !== saved.value.id)]);
      editBook(saved.value);
      setMessage(current ? "世界书已保存。" : "世界书已创建。" );
    } catch (cause) { setMessage(errorMessage(cause, "世界书保存失败。")); } finally { setBusy(false); }
  };
  const remove = async (book: Worldbook) => {
    if (!runtime.profileId || busy) return;
    setBusy(true);
    try {
      await client.deleteWorldbook(runtime.profileId, book.id, productEtag("worldbook", book.revision));
      setBooks((items) => items.filter((item) => item.id !== book.id));
      if (selectedId === book.id) startNew();
    } catch (cause) { setMessage(errorMessage(cause, "世界书删除失败。")); } finally { setBusy(false); }
  };

  const bindingSet = useMemo(() => new Set(draft.boundPersonaIds ?? []), [draft.boundPersonaIds]);

  return (
    <section className="product-page catalog-workspace" aria-label="世界书产品面板">
      <CatalogGate runtime={runtime}>
        <CatalogToolbar busy={busy} eyebrow="WORLDBOOK" onRefresh={runtime.refresh} onSelectProfile={runtime.selectProfile} profileId={runtime.profileId} profiles={runtime.profiles} title="世界书" />
        <div className="catalog-worldbook-layout">
          <form className="product-form-section catalog-form" onSubmit={(event) => void save(event)}>
            <div className="product-section-heading"><div><span>{selectedId ? "EDIT WORLDBOOK" : "NEW WORLDBOOK"}</span><h3>{selectedId ? "编辑世界书" : "新建世界书"}</h3></div>{selectedId ? <button className="catalog-icon-button" onClick={startNew} title="新建世界书" type="button"><Plus size={16} /></button> : null}</div>
            <div className="product-form-grid"><label><span>名称</span><input aria-label="世界书名称" maxLength={120} onChange={(event) => setDraft((value) => ({ ...value, name: event.target.value }))} required value={draft.name} /></label><label><span>说明</span><input aria-label="世界书说明" onChange={(event) => setDraft((value) => ({ ...value, description: event.target.value }))} value={draft.description ?? ""} /></label></div>
            <div className="product-form-grid"><label><span>新条目关键词</span><input aria-label="世界书关键词" onChange={(event) => setSectionKey(event.target.value)} value={sectionKey} /></label><label><span>新条目内容</span><textarea aria-label="世界书条目内容" onChange={(event) => setSectionContent(event.target.value)} value={sectionContent} /></label></div>
            {personas.length > 0 ? <fieldset className="catalog-bindings"><legend>绑定角色</legend>{personas.map((persona) => <label key={persona.id}><input checked={bindingSet.has(persona.id)} onChange={(event) => setDraft((value) => ({ ...value, boundPersonaIds: event.target.checked ? [...(value.boundPersonaIds ?? []), persona.id] : (value.boundPersonaIds ?? []).filter((id) => id !== persona.id) }))} type="checkbox" /><span>{persona.name}</span></label>)}</fieldset> : null}
            {(draft.sections?.length ?? 0) > 0 ? <ul className="catalog-section-list">{draft.sections?.map((section, index) => <li key={`${section.key}-${index}`}><span><strong>{section.key}</strong><small>{section.content}</small></span><button aria-label={`移除条目 ${section.key}`} onClick={() => setDraft((value) => ({ ...value, sections: (value.sections ?? []).filter((_, itemIndex) => itemIndex !== index) }))} title="移除条目" type="button"><Trash2 size={14} /></button></li>)}</ul> : null}
            {message ? <p className="catalog-message" role="status">{message}</p> : null}
            <button className="product-primary-button" disabled={busy || !draft.name.trim()} type="submit">{busy ? <LoaderCircle className="spin" size={16} /> : <Save size={16} />}{selectedId ? "保存世界书" : "新建世界书"}</button>
          </form>
          <section className="product-catalog-section" aria-labelledby="worldbook-list-title"><div className="product-section-heading"><div><span>LIBRARY</span><h3 id="worldbook-list-title">世界书列表</h3></div></div>{books.length === 0 ? <div className="catalog-state is-compact"><BookOpen size={22} />暂无世界书</div> : <div className="catalog-book-list">{books.map((book) => <article className={selectedId === book.id ? "is-active" : undefined} key={book.id}><button onClick={() => editBook(book)} type="button"><strong>{book.name}</strong><small>{book.description || "无说明"}</small><span>{book.sections.length} 条目 · {book.boundPersonaIds.length} 角色</span></button><button aria-label={`删除世界书 ${book.name}`} disabled={busy} onClick={() => void remove(book)} title="删除世界书" type="button"><Trash2 size={15} /></button></article>)}</div>}</section>
        </div>
      </CatalogGate>
    </section>
  );
}
