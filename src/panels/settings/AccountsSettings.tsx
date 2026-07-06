import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  AlertTriangle,
  CheckCircle2,
  ChevronRight,
  Loader2,
  Plus,
  RefreshCw,
  Settings,
  Smartphone,
  XCircle
} from "lucide-react";
import { api } from "../../lib/api";
import { formatTime } from "../../lib/formatters";
import type { AccountConfig, Persona, WechatConfig, WechatQrStartResult } from "../../lib/types";
import { BackBtn } from "./_shared";

function normalizeQrBaseUrl(value?: string | null) {
  const text = value?.trim() ?? "";
  if (!text) return "";
  const trimmed = text.replace(/\/+$/, "");
  if (/^https?:\/\//i.test(trimmed)) return trimmed;
  if (trimmed.startsWith("//")) return `https:${trimmed}`;
  return `https://${trimmed.replace(/^\/+/, "")}`;
}

function cleanNativeError(error: unknown) {
  return String(error).replace(/^bad request:\s*/i, "").trim();
}

export function AccountsSettings({
  onBack,
  accounts,
  personas,
  refreshAccounts,
  saveAccounts
}: {
  onBack?: () => void;
  accounts: AccountConfig[];
  personas: Persona[];
  refreshAccounts: () => Promise<void>;
  saveAccounts: (accounts: AccountConfig[]) => Promise<void>;
}) {
  const [wechatConfig, setWechatConfig] = useState<WechatConfig>({ baseUrl: "", timeoutSeconds: 35 });
  const [qr, setQr] = useState<WechatQrStartResult | null>(null);
  const [qrError, setQrError] = useState("");
  const [busy, setBusy] = useState(false);
  const [showQrSheet, setShowQrSheet] = useState(false);
  const [pendingNoteId, setPendingNoteId] = useState("");
  const [noteDraft, setNoteDraft] = useState("");
  const [detailId, setDetailId] = useState("");
  const [bindDraft, setBindDraft] = useState("");
  const [pollStatus, setPollStatus] = useState("");
  const [qrStatusText, setQrStatusText] = useState("");
  const [checking, setChecking] = useState(false);
  const [scanSuccess, setScanSuccess] = useState(false);
  const [pollingDetail, setPollingDetail] = useState(false);
  const qrPollingRef = useRef(false);
  const detail = accounts.find((account) => account.id === detailId) ?? null;

  useEffect(() => {
    void api.getWechatConfig().then(setWechatConfig);
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listen<{ accountId?: string; error?: string }>("synthchat-wechat-poll-error", (event) => {
      const accountId = event.payload?.accountId ?? "";
      if (detailId && accountId && accountId !== detailId) return;
      const error = event.payload?.error || "微信后台连接失败";
      setPollStatus(`后台轮询失败：${cleanNativeError(error)}`);
    }).then((handler) => {
      unlisten = handler;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, [detailId]);

  const pollAccountOnce = async (account: AccountConfig, options?: { quietEmpty?: boolean }) => {
    if (!account.linkedPersona?.trim()) {
      setPollStatus("已登录，但还没有绑定角色；保存角色后微信端才能连接。");
      return;
    }
    setPollingDetail(true);
    setPollStatus("正在测试微信连接...");
    try {
      const result = await api.wechatPollOnce(account.id);
      await refreshAccounts();
      const failed = result.processed.filter((item) => !item.delivered || item.deliveryError);
      if (failed.length > 0) {
        const firstError = failed.find((item) => item.deliveryError)?.deliveryError;
        setPollStatus(firstError ? `微信已连接，但回复发送失败：${firstError}` : "微信已连接，但有消息处理失败。");
        return;
      }
      if (result.receivedCount) {
        setPollStatus(`微信连接正常，收到 ${result.receivedCount} 条，已处理 ${result.processed.length} 条，跳过 ${result.skippedCount} 条。`);
      } else if (!options?.quietEmpty) {
        setPollStatus("微信连接正常，暂无新消息。");
      } else {
        setPollStatus("微信连接正常。");
      }
    } catch (error) {
      await refreshAccounts().catch(() => {});
      setPollStatus(`微信连接失败：${cleanNativeError(error)}`);
    } finally {
      setPollingDetail(false);
    }
  };

  const checkQrOnce = async () => {
    if (!qr?.qrcode || qrPollingRef.current || scanSuccess) return;
    const activeBaseUrl = normalizeQrBaseUrl(qr.baseUrl);
    qrPollingRef.current = true;
    setChecking(true);
    try {
      const status = await api.checkWechatQrStatus(qr.qrcode, activeBaseUrl || qr.baseUrl);
      const redirectedBaseUrl = normalizeQrBaseUrl(status.host);
      if (redirectedBaseUrl && redirectedBaseUrl !== activeBaseUrl) {
        setQr((current) => (
          current?.qrcode === qr.qrcode
            ? { ...current, baseUrl: redirectedBaseUrl }
            : current
        ));
      }
      const normalizedStatus = (status.status || "").trim().toLowerCase();
      if (status.account) {
        const account = status.account;
        setQrError("");
        setQrStatusText("登录成功");
        setScanSuccess(true);
        await refreshAccounts();
        setTimeout(() => {
          setShowQrSheet(false);
          setScanSuccess(false);
          setDetailId(account.id);
          setBindDraft(account.linkedPersona || "");
          if (account.linkedPersona) {
            void pollAccountOnce(account, { quietEmpty: true });
          } else {
            setPollStatus("已登录，但还没有绑定角色；保存角色后微信端才能连接。");
          }
        }, 1200);
      } else if (normalizedStatus === "wait") {
        setQrError("");
        setQrStatusText("等待扫码");
      } else if (normalizedStatus === "scaned") {
        setQrError("");
        setQrStatusText("已扫码，待确认");
      } else if (normalizedStatus === "scaned_but_redirect") {
        setQrError("");
        setQrStatusText("已扫码，正在确认");
      } else if (normalizedStatus === "expired") {
        setQrError("二维码已过期");
        setQrStatusText("二维码已过期");
      } else if (status.message?.trim()) {
        setQrError(status.message.trim());
        setQrStatusText("状态异常");
      }
    } catch (error) {
      const message = String(error);
      if (!message.includes("failed to request wechat QR status") && !message.includes("error sending request for url")) {
        setQrError(message);
        setQrStatusText("状态异常");
      }
    } finally {
      qrPollingRef.current = false;
      setChecking(false);
    }
  };

  useEffect(() => {
    if (!showQrSheet || !qr?.qrcode || scanSuccess) return;
    void checkQrOnce();
    const timer = window.setInterval(() => {
      void checkQrOnce();
    }, 2500);
    return () => window.clearInterval(timer);
  }, [showQrSheet, qr?.qrcode, qr?.baseUrl, scanSuccess]);

  const saveWechat = async (patch: Partial<WechatConfig>) => {
    const saved = await api.saveWechatConfig({ ...wechatConfig, ...patch });
    setWechatConfig(saved);
  };

  const startQr = async () => {
    setBusy(true);
    setQrError("");
    setQr(null);
    setQrStatusText("正在获取二维码");
    setScanSuccess(false);
    setShowQrSheet(true);
    try {
      const saved = await api.saveWechatConfig(wechatConfig);
      setWechatConfig(saved);
      setQr(await api.startWechatQr(saved.baseUrl));
      setQrStatusText("等待扫码");
    } catch (error) {
      setQrStatusText("");
      setQrError(String(error));
    } finally {
      setBusy(false);
    }
  };

  const add = () => {
    void saveAccounts([
      ...accounts,
      {
        id: crypto.randomUUID(),
        note: "未命名账号",
        linkedPersona: "",
        online: false,
        createdAt: new Date().toISOString(),
        botToken: "",
        ilinkUserId: "",
        getUpdatesBuf: "",
        loginBaseUrl: "",
        lastLoginAt: ""
      }
    ]);
  };

  const savePendingNote = async () => {
    if (!pendingNoteId) return;
    const latestAccounts = await api.listAccounts();
    await saveAccounts(latestAccounts.map((account) => (
      account.id === pendingNoteId ? { ...account, note: noteDraft.trim() || account.note } : account
    )));
    setPendingNoteId("");
    setNoteDraft("");
  };

  const saveDetailNote = async () => {
    if (!detail) return;
    const input = document.getElementById("acct-note") as HTMLInputElement | null;
    const note = input?.value.trim() ?? "";
    const latestAccounts = await api.listAccounts();
    const nextAccounts = latestAccounts.map((account) => {
      if (account.id === detail.id) return { ...account, note, linkedPersona: bindDraft };
      if (bindDraft && account.linkedPersona === bindDraft) return { ...account, linkedPersona: "" };
      return account;
    });
    await saveAccounts(nextAccounts);
    setPollStatus("");
    setDetailId("");
  };

  const pollDetailOnce = async () => {
    if (!detail) return;
    await pollAccountOnce(detail);
  };

  if (detail) {
    return (
      <div className="primary-panel embedded-panel">
        <div className="panel-title action-title">
          <button className="icon-only-btn" onClick={() => setDetailId("")} title="返回" type="button"><ChevronRight size={19} style={{ transform: "rotate(180deg)" }} /></button>
          <div className="panel-title-text"><span>Account</span><strong>账号详情</strong></div>
        </div>
        {/* Status + Test Connection */}
        <div className="card" style={{ margin: "0 16px 12px" }}>
          <div className="card-header" style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
            <span>连接状态</span>
            <button className="btn-primary-outline" disabled={pollingDetail} onClick={() => void pollDetailOnce()} type="button" style={{ padding: "4px 12px", fontSize: 12 }}>
              {pollingDetail ? "测试中..." : "测试连接"}
            </button>
          </div>
          <div className="form-group" style={{ padding: "8px 16px 12px" }}>
            <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: "8px 16px" }}>
              <div className="detail-row"><span>Bot ID</span><strong>{detail.id}</strong></div>
              <div className="detail-row">
                <span>状态</span>
                <strong className={detail.online ? "status-online" : "status-offline"}>
                  {detail.online ? "● 在线" : "● 离线"}
                </strong>
              </div>
              <div className="detail-row"><span>链接角色</span><strong>{personas.find((persona) => persona.id === detail.linkedPersona)?.name || detail.linkedPersona || "未链接"}</strong></div>
              <div className="detail-row"><span>iLink 用户</span><strong>{detail.ilinkUserId || "未记录"}</strong></div>
              <div className="detail-row"><span>创建时间</span><strong>{detail.createdAt ? formatTime(detail.createdAt) : "未知"}</strong></div>
              <div className="detail-row"><span>最后登录</span><strong>{detail.lastLoginAt ? formatTime(detail.lastLoginAt) : "未记录"}</strong></div>
            </div>
            {pollStatus ? (
              <div style={{
                marginTop: 10,
                padding: "8px 12px",
                borderRadius: "var(--radius-md)",
                fontSize: 13,
                display: "flex",
                alignItems: "center",
                gap: 8,
                background: pollStatus.includes("失败") ? "rgba(239, 68, 68, 0.08)" : pollStatus.includes("正常") || pollStatus.includes("收到") ? "rgba(34, 197, 94, 0.08)" : pollStatus.includes("测试") ? "var(--primary-light)" : "rgba(234, 179, 8, 0.08)",
                border: `1px solid ${pollStatus.includes("失败") ? "rgba(239, 68, 68, 0.15)" : pollStatus.includes("正常") || pollStatus.includes("收到") ? "rgba(34, 197, 94, 0.15)" : pollStatus.includes("测试") ? "rgba(8, 145, 178, 0.15)" : "rgba(234, 179, 8, 0.15)"}`,
                color: pollStatus.includes("失败") ? "var(--danger)" : pollStatus.includes("正常") || pollStatus.includes("收到") ? "#16a34a" : pollStatus.includes("测试") ? "var(--primary)" : "#a16207",
              }}>
                {pollStatus.includes("失败") ? <XCircle size={15} /> : pollStatus.includes("正常") || pollStatus.includes("收到") ? <CheckCircle2 size={15} /> : pollStatus.includes("测试") ? <Loader2 size={15} className="spin" /> : <AlertTriangle size={15} />}
                <span>{pollStatus}</span>
              </div>
            ) : null}
          </div>
        </div>

        {/* Edit Config */}
        <div className="card" style={{ margin: "0 16px 12px" }}>
          <div className="card-header">配置</div>
          <div className="form-group">
            <div className="form-row">
              <label>备注名</label>
              <input id="acct-note" defaultValue={detail.note || ""} placeholder="为账号设置一个备注" />
            </div>
            <div className="form-row">
              <label>链接角色</label>
              <select value={bindDraft} onChange={(event) => setBindDraft(event.target.value)}>
                <option value="">未链接</option>
                {personas.map((persona) => <option key={persona.id} value={persona.id}>{persona.name}</option>)}
              </select>
            </div>
          </div>
          <div className="form-hint" style={{ padding: "0 16px 10px" }}>
            保存链接角色后，后台会自动轮询该微信账号并把手机消息送入对应角色会话。
          </div>
        </div>

        {/* Actions */}
        <div className="card" style={{ margin: "0 16px 12px" }}>
          <div className="form-actions">
            <button className="btn-primary" onClick={() => void saveDetailNote()} type="button">保存配置</button>
            <button className="btn-danger" onClick={() => { if (window.confirm("确定要删除此账号吗？")) { void saveAccounts(accounts.filter((account) => account.id !== detail.id)); setDetailId(""); } }} type="button">删除账号</button>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="primary-panel embedded-panel">
      <div className="panel-title action-title">
        <BackBtn onBack={onBack} />
        <div className="panel-title-text"><span>Accounts</span><strong>微信账号</strong></div>
        <button className="icon-only-btn" onClick={() => void startQr()} title="添加账号" type="button" disabled={busy}><Plus size={19} /></button>
      </div>
      {accounts.length === 0 ? (
        <div className="empty-state compact">
          <div className="empty-icon-wrap"><Smartphone size={48} strokeWidth={1.5} /></div>
          <p>没有已登录的微信账号</p>
          <button className="btn-primary" onClick={() => void startQr()} type="button">添加账号</button>
        </div>
      ) : (
        <div className="account-list">
          {accounts.map((account) => (
            <div className="card account-card" key={account.id}>
              <button className="card-row clickable-row" onClick={() => { setDetailId(account.id); setBindDraft(account.linkedPersona || ""); }} type="button">
                <span className="row-icon green"><Smartphone size={18} /></span>
                <div className="account-info">
                  <div className="account-name">
                    <strong>{account.note || `Bot: ${account.id.slice(0, 12)}...`}</strong>
                    <span className={`status-dot ${account.online ? "online" : "offline"}`}>●</span>
                    <span className="status-text">{account.online ? "在线" : "离线"}</span>
                  </div>
                  {account.note ? <div className="account-id">{account.id.slice(0, 20)}...</div> : null}
                  {account.linkedPersona ? <div className="account-linked">已链接到：{personas.find((persona) => persona.id === account.linkedPersona)?.name || account.linkedPersona}</div> : null}
                  {account.createdAt ? <div className="account-time">创建于 {formatTime(account.createdAt)}</div> : null}
                </div>
                <ChevronRight size={18} className="row-arrow" />
              </button>
            </div>
          ))}
        </div>
      )}
      <div style={{ padding: "0 16px 16px" }}>
        <details className="card" style={{ margin: 0, overflow: "hidden" }}>
          <summary className="card-header" style={{ cursor: "pointer", userSelect: "none", display: "flex", alignItems: "center", justifyContent: "flex-start", gap: 6 }}>
            <Settings size={14} />
            <span>高级接口设置</span>
          </summary>
          <div className="form-group" style={{ padding: "4px 16px 8px" }}>
            <div className="form-row">
              <label>微信接口 Base URL</label>
              <input value={wechatConfig.baseUrl} onChange={(event) => setWechatConfig({ ...wechatConfig, baseUrl: event.target.value })} placeholder="http://localhost:3000" />
            </div>
            <div className="form-row">
              <label>轮询超时（秒）</label>
              <input min={5} type="number" value={wechatConfig.timeoutSeconds} onChange={(event) => setWechatConfig({ ...wechatConfig, timeoutSeconds: Number(event.target.value) })} />
            </div>
          </div>
          <div className="form-actions" style={{ padding: "0 16px 12px" }}>
            <button className="btn-secondary" onClick={() => void saveWechat({})} type="button">保存接口</button>
            <button className="btn-secondary" onClick={add} type="button">手动添加测试账号</button>
          </div>
        </details>
      </div>
      {showQrSheet ? (
        <div className="sheet-backdrop" onClick={() => setShowQrSheet(false)}>
          <div className="action-sheet" onClick={(event) => event.stopPropagation()}>
            <div className="sheet-title">扫码登录微信</div>
            {busy ? <div className="empty-state compact"><RefreshCw size={30} /><p>正在获取二维码...</p></div> : null}
            {scanSuccess ? (
              <div className="qr-success-wrap">
                <div className="qr-success-check">
                  <svg viewBox="0 0 52 52" className="qr-success-svg">
                    <circle className="qr-success-circle" cx="26" cy="26" r="24" fill="none" />
                    <path className="qr-success-path" fill="none" d="M14 27l8 8 16-16" />
                  </svg>
                </div>
                <div className="qr-success-text">登录成功</div>
              </div>
            ) : (
              <>
                {qr?.qrImage ? <img className="qr-sheet-img" alt="QR Code" src={qr.qrImage} /> : null}
                {qrError ? <div className="qr-error">{qrError}</div> : null}
                {!qr?.qrImage && qr?.qrcode ? (
                  <div className="qr-raw">
                    <span>接口已返回二维码内容，但图片未生成</span>
                    <code>{qr.qrcode}</code>
                  </div>
                ) : null}
                <div className="qr-status">{qrError ? "二维码状态异常" : qrStatusText || (qr?.qrImage ? "等待扫码" : "正在获取二维码")}</div>
                {!busy && !qr?.qrImage ? <button className="qr-check-btn" onClick={() => void startQr()} type="button">重新获取二维码</button> : null}
              </>
            )}
            <button className="btn-text" onClick={() => setShowQrSheet(false)} type="button">取消</button>
          </div>
        </div>
      ) : null}
      {pendingNoteId ? (
        <div className="sheet-backdrop">
          <div className="action-sheet note-sheet">
            <div className="sheet-title">设置账号备注</div>
            <p className="form-hint">为新添加的账号设置一个易于识别的名称</p>
            <input value={noteDraft} onChange={(event) => setNoteDraft(event.target.value)} placeholder="输入备注名" />
            <div className="inline-actions">
              <button onClick={() => setPendingNoteId("")} type="button">跳过</button>
              <button onClick={() => void savePendingNote()} type="button">保存</button>
            </div>
          </div>
        </div>
      ) : null}
    </div>
  );
}
