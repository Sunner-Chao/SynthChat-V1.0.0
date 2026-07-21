import { CheckCircle2, Inbox, LoaderCircle, QrCode, RefreshCw, Save, Send, Smartphone } from "lucide-react";
import { useEffect, useRef, useState, type FormEvent } from "react";
import {
  productCatalogApi,
  type Persona,
  type ProductCatalogApi,
} from "../../api/productCatalog";
import { profilesApi, type ProfileSummary, type ProfilesApi } from "../../api/profiles";
import {
  wechatApi,
  type VersionedWechatConfig,
  type WechatApi,
  type WechatInboundMessage,
  type WechatQrStartResult,
} from "../../api/wechat";

type ProfileClient = Pick<ProfilesApi, "getCapabilities" | "listProfiles">;
type CatalogClient = Pick<ProductCatalogApi, "listPersonas">;
type AccountDraft = { peer: string; text: string };
type AccountPollSummary = { receivedCount: number; skippedCount: number };

export function WechatSettingsPanel({
  client = wechatApi,
  catalogClient = productCatalogApi,
  profileClient = profilesApi,
}: {
  client?: WechatApi;
  catalogClient?: CatalogClient;
  profileClient?: ProfileClient;
}) {
  const [available, setAvailable] = useState<boolean | null>(null);
  const [messagingAvailable, setMessagingAvailable] = useState(false);
  const [personasAvailable, setPersonasAvailable] = useState(false);
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [personas, setPersonas] = useState<Persona[]>([]);
  const [profileId, setProfileId] = useState<string | null>(null);
  const [config, setConfig] = useState<VersionedWechatConfig | null>(null);
  const [loadEpoch, setLoadEpoch] = useState(0);
  const [baseUrl, setBaseUrl] = useState("");
  const [timeoutSeconds, setTimeoutSeconds] = useState("35");
  const [qr, setQr] = useState<WechatQrStartResult | null>(null);
  const [phase, setPhase] = useState<"loading" | "ready" | "error">("loading");
  const [busy, setBusy] = useState<"save" | "qr" | "poll" | "link" | "receive" | "send" | null>(null);
  const [busyAccountId, setBusyAccountId] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [accountCursors, setAccountCursors] = useState<Record<string, string>>({});
  const [accountMessages, setAccountMessages] = useState<Record<string, WechatInboundMessage[]>>({});
  const [accountPollSummaries, setAccountPollSummaries] = useState<Record<string, AccountPollSummary>>({});
  const [accountDrafts, setAccountDrafts] = useState<Record<string, AccountDraft>>({});
  const pollEpoch = useRef(0);

  useEffect(() => {
    const controller = new AbortController();
    void Promise.all([
      profileClient.getCapabilities({ signal: controller.signal }),
      profileClient.listProfiles({ signal: controller.signal }),
    ])
      .then(([capabilities, items]) => {
        if (controller.signal.aborted) return;
        setAvailable(capabilities.extensions.wechatAccounts === true);
        setMessagingAvailable(capabilities.extensions.wechatMessaging === true);
        setPersonasAvailable(capabilities.extensions.personas === true);
        setProfiles(items);
        setProfileId(items.find((item) => item.isActive)?.id ?? items[0]?.id ?? null);
      })
      .catch(() => {
        if (!controller.signal.aborted) setPhase("error");
      });
    return () => controller.abort();
  }, [profileClient]);

  useEffect(() => {
    pollEpoch.current += 1;
    setQr(null);
    setMessage(null);
    setPersonas([]);
    setAccountCursors({});
    setAccountMessages({});
    setAccountPollSummaries({});
    setAccountDrafts({});
    if (!available || !profileId) {
      setConfig(null);
      setPhase(available === false || profiles.length === 0 ? "ready" : "loading");
      return undefined;
    }
    const controller = new AbortController();
    setPhase("loading");
    void Promise.all([
      client.getConfig(profileId, { signal: controller.signal }),
      personasAvailable
        ? catalogClient.listPersonas(profileId, undefined, { signal: controller.signal })
        : Promise.resolve([]),
    ])
      .then(([next, personaItems]) => {
        if (controller.signal.aborted) return;
        setConfig(next);
        setPersonas(personaItems);
        setBaseUrl(next.value.baseUrl);
        setTimeoutSeconds(String(next.value.timeoutSeconds));
        setPhase("ready");
      })
      .catch((error: unknown) => {
        if (controller.signal.aborted) return;
        setMessage(error instanceof Error ? error.message : "无法加载微信配置。");
        setPhase("error");
      });
    return () => controller.abort();
  }, [available, catalogClient, client, loadEpoch, personasAvailable, profileId, profiles.length]);

  useEffect(() => {
    if (!qr || !profileId) return undefined;
    const epoch = ++pollEpoch.current;
    let timer: number | undefined;
    const controller = new AbortController();
    const poll = async () => {
      if (pollEpoch.current !== epoch) return;
      setBusy("poll");
      try {
        const result = await client.checkQr(profileId, {
          qrcode: qr.qrcode,
          baseUrl: qr.baseUrl,
        }, { signal: controller.signal });
        if (controller.signal.aborted || pollEpoch.current !== epoch) return;
        if (result.account) {
          const refreshed = await client.getConfig(profileId, { signal: controller.signal });
          if (controller.signal.aborted || pollEpoch.current !== epoch) return;
          setConfig(refreshed);
          setQr(null);
          setMessage(`微信账号 ${result.account.note} 已登录，凭据已写入系统密钥链。`);
          return;
        }
        setMessage(result.message?.trim() || `扫码状态：${result.status}`);
        timer = window.setTimeout(() => void poll(), 2500);
      } catch (error) {
        if (!controller.signal.aborted && pollEpoch.current === epoch) {
          setMessage(error instanceof Error ? error.message : "微信扫码状态检查失败。");
          timer = window.setTimeout(() => void poll(), 4000);
        }
      } finally {
        if (!controller.signal.aborted && pollEpoch.current === epoch) setBusy(null);
      }
    };
    void poll();
    return () => {
      controller.abort();
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [client, profileId, qr]);

  const saveConfig = async (event: FormEvent) => {
    event.preventDefault();
    if (!profileId || !config || busy) return;
    const timeout = Number(timeoutSeconds);
    if (!Number.isInteger(timeout) || timeout < 5 || timeout > 60) {
      setMessage("超时必须是 5 到 60 秒之间的整数。");
      return;
    }
    setBusy("save");
    setMessage(null);
    try {
      const updated = await client.updateConfig(profileId, {
        baseUrl: baseUrl.trim(),
        timeoutSeconds: timeout,
      }, config.etag);
      setConfig(updated);
      setBaseUrl(updated.value.baseUrl);
      setTimeoutSeconds(String(updated.value.timeoutSeconds));
      setMessage("微信连接配置已保存。");
    } catch (error) {
      setMessage(error instanceof Error ? error.message : "微信配置保存失败。");
    } finally {
      setBusy(null);
    }
  };

  const startQr = async () => {
    if (!profileId || !config || busy) return;
    setBusy("qr");
    setMessage(null);
    try {
      const challenge = await client.startQr(profileId, { baseUrl: baseUrl.trim() || null });
      setQr(challenge);
      setMessage("请使用微信扫描二维码；页面会自动检查登录状态。");
    } catch (error) {
      setMessage(error instanceof Error ? error.message : "无法启动微信扫码登录。");
    } finally {
      setBusy(null);
    }
  };

  const updateAccountLink = async (accountId: string, linkedPersonaId: string | null) => {
    if (!profileId || !config || busy) return;
    setBusy("link");
    setBusyAccountId(accountId);
    setMessage(null);
    try {
      const updated = await client.updateAccountLink(
        profileId,
        accountId,
        { linkedPersonaId },
        config.etag,
      );
      setConfig(updated);
      const persona = personas.find((item) => item.id === linkedPersonaId);
      setMessage(persona ? `已绑定角色 ${persona.name}。` : "已解除角色绑定。");
    } catch (error) {
      setMessage(error instanceof Error ? error.message : "微信角色绑定更新失败。");
    } finally {
      setBusy(null);
      setBusyAccountId(null);
    }
  };

  const pollAccountMessages = async (accountId: string) => {
    if (!profileId || !messagingAvailable || busy) return;
    setBusy("receive");
    setBusyAccountId(accountId);
    setMessage(null);
    try {
      const result = await client.pollMessages(profileId, accountId, {
        cursor: accountCursors[accountId] ?? null,
      });
      if (result.nextCursor !== null) {
        setAccountCursors((value) => ({ ...value, [accountId]: result.nextCursor! }));
      }
      setAccountMessages((value) => ({ ...value, [accountId]: result.messages }));
      setAccountPollSummaries((value) => ({
        ...value,
        [accountId]: {
          receivedCount: result.receivedCount,
          skippedCount: result.skippedCount,
        },
      }));
      const firstPeer = result.messages[0]?.peer;
      if (firstPeer) {
        setAccountDrafts((value) => ({
          ...value,
          [accountId]: {
            peer: value[accountId]?.peer || firstPeer,
            text: value[accountId]?.text ?? "",
          },
        }));
      }
      setMessage(`已拉取 ${result.receivedCount} 条，显示 ${result.messages.length} 条文本消息。`);
    } catch (error) {
      setMessage(error instanceof Error ? error.message : "微信消息拉取失败。");
    } finally {
      setBusy(null);
      setBusyAccountId(null);
    }
  };

  const updateAccountDraft = (accountId: string, patch: Partial<AccountDraft>) => {
    setAccountDrafts((value) => ({
      ...value,
      [accountId]: {
        peer: value[accountId]?.peer ?? "",
        text: value[accountId]?.text ?? "",
        ...patch,
      },
    }));
  };

  const sendAccountMessage = async (event: FormEvent, accountId: string) => {
    event.preventDefault();
    if (!profileId || !messagingAvailable || busy) return;
    const draft = accountDrafts[accountId] ?? { peer: "", text: "" };
    setBusy("send");
    setBusyAccountId(accountId);
    setMessage(null);
    try {
      const result = await client.sendMessage(profileId, accountId, draft);
      updateAccountDraft(accountId, { text: "" });
      setMessage(result.messageId ? `微信消息已发送（${result.messageId}）。` : "微信消息已发送。");
    } catch (error) {
      setMessage(error instanceof Error ? error.message : "微信消息发送失败。");
    } finally {
      setBusy(null);
      setBusyAccountId(null);
    }
  };

  if (phase === "loading" || available === null) {
    return <div className="settings-inline-state" role="status"><LoaderCircle className="spin" size={18} />正在加载微信账号服务</div>;
  }
  if (available === false) {
    return <div className="settings-inline-state">当前 Rust 后端未启用微信账号管理能力。</div>;
  }
  if (profiles.length === 0) {
    return <div className="settings-inline-state">暂无 Profile，请先创建一个 Profile。</div>;
  }
  if (phase === "error" || !config || !profileId) {
    return <div className="settings-inline-state is-error">{message ?? "无法加载微信账号服务。"}</div>;
  }

  return (
    <div className="wechat-settings">
      <form className="wechat-settings__config" onSubmit={(event) => void saveConfig(event)}>
        <label className="settings-field">
          <span>Profile</span>
          <select disabled={busy !== null} value={profileId} onChange={(event) => setProfileId(event.target.value)}>
            {profiles.map((profile) => <option key={profile.id} value={profile.id}>{profile.displayName}</option>)}
          </select>
        </label>
        <div className="settings-form-grid">
          <label className="settings-field settings-field--wide"><span>iLink Base URL</span><input disabled={busy !== null} value={baseUrl} onChange={(event) => setBaseUrl(event.target.value)} /></label>
          <label className="settings-field"><span>状态超时（秒）</span><input disabled={busy !== null} min={5} max={60} type="number" value={timeoutSeconds} onChange={(event) => setTimeoutSeconds(event.target.value)} /></label>
        </div>
        <div className="settings-actions">
          <button className="settings-secondary-button" disabled={busy !== null} onClick={() => void startQr()} type="button"><QrCode size={16} />扫码登录</button>
          <button className="settings-primary-button" disabled={busy !== null} type="submit"><Save size={16} />保存接口</button>
        </div>
      </form>

      {qr ? (
        <section className="wechat-settings__qr" aria-label="微信扫码登录">
          <img alt="微信登录二维码" src={qr.qrImage} />
          <div><strong>等待扫码确认</strong><small>{busy === "poll" ? "正在检查状态…" : "二维码状态轮询中"}</small></div>
        </section>
      ) : null}

      {message ? <p className="settings-save-message" role="status">{message}</p> : null}

      <section className="wechat-settings__accounts" aria-labelledby="wechat-account-list-title">
        <header><div><small>ACCOUNTS</small><h3 id="wechat-account-list-title">已登录账号</h3></div><button aria-label="刷新微信账号" disabled={busy !== null} onClick={() => setLoadEpoch((value) => value + 1)} title="刷新" type="button"><RefreshCw size={15} /></button></header>
        {config.value.accounts.length === 0 ? <div className="settings-inline-state">尚未登录微信账号。</div> : (
          <ul>
            {config.value.accounts.map((account) => {
              const drafts = accountDrafts[account.id] ?? { peer: "", text: "" };
              const messages = accountMessages[account.id];
              const summary = accountPollSummaries[account.id];
              const linkedPersonaMissing = account.linkedPersonaId !== null
                && !personas.some((persona) => persona.id === account.linkedPersonaId);
              const linkedByAnotherAccount = new Set(config.value.accounts
                .filter((item) => item.id !== account.id && item.linkedPersonaId !== null)
                .map((item) => item.linkedPersonaId));
              const accountBusy = busyAccountId === account.id;
              return (
                <li key={account.id}>
                  <div className="wechat-settings__account-summary">
                    <span className="wechat-settings__account-icon"><Smartphone size={17} /></span>
                    <span><strong>{account.note}</strong><small>{account.ilinkUserId || account.id}</small></span>
                    <span className={account.credentialConfigured ? "is-ready" : "is-warning"}>{account.credentialConfigured ? <CheckCircle2 size={14} /> : null}{account.credentialConfigured ? "密钥链已保存" : "凭据缺失"}</span>
                  </div>

                  <div className="wechat-settings__account-controls">
                    <label className="settings-field">
                      <span>绑定角色</span>
                      <select
                        aria-label={`绑定角色 ${account.note}`}
                        disabled={busy !== null || !personasAvailable}
                        onChange={(event) => void updateAccountLink(account.id, event.target.value || null)}
                        value={account.linkedPersonaId ?? ""}
                      >
                        <option value="">不绑定</option>
                        {linkedPersonaMissing ? <option value={account.linkedPersonaId!}>已删除角色</option> : null}
                        {personas.map((persona) => (
                          <option disabled={linkedByAnotherAccount.has(persona.id)} key={persona.id} value={persona.id}>{persona.name}</option>
                        ))}
                      </select>
                    </label>
                    <button
                      className="settings-secondary-button"
                      disabled={busy !== null || !messagingAvailable || !account.credentialConfigured}
                      onClick={() => void pollAccountMessages(account.id)}
                      type="button"
                    >
                      {accountBusy && busy === "receive" ? <LoaderCircle className="spin" size={15} /> : <Inbox size={15} />}
                      拉取消息
                    </button>
                  </div>

                  {messages ? (
                    <section className="wechat-settings__messages" aria-label={`${account.note} 收件消息`}>
                      <header><strong>最近拉取</strong><small>收到 {summary?.receivedCount ?? 0} · 跳过 {summary?.skippedCount ?? 0}</small></header>
                      {messages.length === 0 ? <p>没有新的文本消息。</p> : (
                        <ol>{messages.map((item) => <li key={item.id}><span>{item.peer}</span><p>{item.text}</p></li>)}</ol>
                      )}
                    </section>
                  ) : null}

                  <form className="wechat-settings__composer" onSubmit={(event) => void sendAccountMessage(event, account.id)}>
                    <label className="settings-field"><span>接收方 ID</span><input aria-label={`接收方 ID ${account.note}`} disabled={busy !== null || !messagingAvailable} onChange={(event) => updateAccountDraft(account.id, { peer: event.target.value })} value={drafts.peer} /></label>
                    <label className="settings-field"><span>消息</span><textarea aria-label={`消息 ${account.note}`} disabled={busy !== null || !messagingAvailable} maxLength={16000} onChange={(event) => updateAccountDraft(account.id, { text: event.target.value })} value={drafts.text} /></label>
                    <button className="settings-primary-button" disabled={busy !== null || !messagingAvailable || !account.credentialConfigured || !drafts.peer.trim() || !drafts.text.trim()} type="submit">
                      {accountBusy && busy === "send" ? <LoaderCircle className="spin" size={15} /> : <Send size={15} />}
                      发送
                    </button>
                  </form>
                </li>
              );
            })}
          </ul>
        )}
      </section>
    </div>
  );
}
