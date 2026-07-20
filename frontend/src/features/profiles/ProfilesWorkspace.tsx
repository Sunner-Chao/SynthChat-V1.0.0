import {
  CircleSlash2,
  LoaderCircle,
  Plus,
  Power,
  Trash2,
  UserRoundCog,
  X,
} from "lucide-react";
import {
  useEffect,
  useRef,
  useState,
  type FormEvent,
} from "react";
import { DesktopConnectionError } from "../../api/desktopConnection";
import {
  ProfileApiError,
  profilesApi,
  type CreateProfileInput,
  type ProfileConfig,
  type ProfileConfigPatch,
  type ProfileMetadata,
  type ProfilePatch,
  type ProfileSummary,
  type ProfilesApi,
  type Provider,
  type SecretStatus,
  type Versioned,
} from "../../api/profiles";
import { ProfileConfigForm } from "./ProfileConfigForm";
import { SecretRow } from "./SecretRow";
import "./profiles.css";

type WorkspaceState =
  | { phase: "loading" }
  | { phase: "desktop-required" }
  | { phase: "unavailable" }
  | { phase: "error"; message: string }
  | { phase: "ready" };

type ResourceState =
  | { phase: "idle" }
  | { phase: "loading" }
  | { phase: "error"; message: string }
  | {
    phase: "ready";
    metadata: Versioned<ProfileMetadata>;
    config: Versioned<ProfileConfig>;
    secrets: SecretStatus[];
    secretError: string | null;
  };

interface CreateAttempt {
  fingerprint: string;
  idempotencyKey: string;
}

function isAbortError(error: unknown): boolean {
  return error instanceof DOMException && error.name === "AbortError";
}

function safeErrorMessage(error: unknown, fallback: string): string {
  if (error instanceof ProfileApiError) {
    return error.requestId ? `${error.message} 请求 ID：${error.requestId}` : error.message;
  }
  return fallback;
}

function newIdempotencyKey(): string {
  if (typeof globalThis.crypto?.randomUUID === "function") {
    return globalThis.crypto.randomUUID();
  }
  const bytes = new Uint8Array(16);
  globalThis.crypto.getRandomValues(bytes);
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

function mergeSecretCatalog(
  statuses: SecretStatus[],
  providers: Provider[],
  providerId: string,
): SecretStatus[] {
  const knownNames = new Set(providers.flatMap((provider) => provider.secretNames));
  const currentNames = new Set(
    providers.find((provider) => provider.id === providerId)?.secretNames ?? [],
  );
  const byName = new Map(statuses.map((status) => [status.name, status]));
  for (const name of currentNames) {
    if (!byName.has(name)) {
      byName.set(name, { name, configured: false, storage: "osKeychain" });
    }
  }
  return [...byName.values()]
    .filter((status) => (
      status.configured || currentNames.has(status.name) || !knownNames.has(status.name)
    ))
    .sort((left, right) => left.name.localeCompare(right.name));
}

export function ProfilesWorkspace({ client = profilesApi }: { client?: ProfilesApi }) {
  const [workspace, setWorkspace] = useState<WorkspaceState>({ phase: "loading" });
  const [providers, setProviders] = useState<Provider[]>([]);
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [selectedProfileId, setSelectedProfileId] = useState<string | null>(null);
  const [resource, setResource] = useState<ResourceState>({ phase: "idle" });
  const [workspaceEpoch, setWorkspaceEpoch] = useState(0);
  const [resourceEpoch, setResourceEpoch] = useState(0);
  const [actionBusy, setActionBusy] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [showCreate, setShowCreate] = useState(false);
  const [createId, setCreateId] = useState("");
  const [createName, setCreateName] = useState("");
  const [cloneFrom, setCloneFrom] = useState("");
  const [createError, setCreateError] = useState<string | null>(null);
  const createAttemptRef = useRef<CreateAttempt | null>(null);

  useEffect(() => {
    const controller = new AbortController();
    setWorkspace({ phase: "loading" });
    setActionError(null);

    void (async () => {
      try {
        const capabilities = await client.getCapabilities({ signal: controller.signal });
        if (!capabilities.engine.features.profileManagement) {
          setWorkspace({ phase: "unavailable" });
          return;
        }
        const [providerList, profileList] = await Promise.all([
          client.listProviders({ signal: controller.signal }),
          client.listProfiles({ signal: controller.signal }),
        ]);
        if (controller.signal.aborted) return;
        setProviders(providerList);
        setProfiles(profileList);
        setSelectedProfileId((current) => (
          current && profileList.some((profile) => profile.id === current)
            ? current
            : profileList.find((profile) => profile.isActive)?.id ?? profileList[0]?.id ?? null
        ));
        setWorkspace({ phase: "ready" });
      } catch (error) {
        if (controller.signal.aborted || isAbortError(error)) return;
        if (error instanceof DesktopConnectionError && error.kind === "desktop_unavailable") {
          setWorkspace({ phase: "desktop-required" });
          return;
        }
        setWorkspace({
          phase: "error",
          message: safeErrorMessage(error, "无法加载 Profile 服务。"),
        });
      }
    })();

    return () => controller.abort();
  }, [client, workspaceEpoch]);

  useEffect(() => {
    if (workspace.phase !== "ready" || !selectedProfileId) {
      setResource({ phase: "idle" });
      return undefined;
    }

    const controller = new AbortController();
    setResource({ phase: "loading" });
    void (async () => {
      try {
        const [metadata, config, secretResult] = await Promise.all([
          client.getProfile(selectedProfileId, { signal: controller.signal }),
          client.getProfileConfig(selectedProfileId, { signal: controller.signal }),
          client.listSecretStatuses(selectedProfileId, { signal: controller.signal })
            .then((value) => ({ value, error: null as unknown }))
            .catch((error: unknown) => ({ value: [] as SecretStatus[], error })),
        ]);
        if (controller.signal.aborted) return;
        const secretError = secretResult.error instanceof ProfileApiError
          && secretResult.error.status === 503
          ? "系统密钥链当前不可用。"
          : secretResult.error
            ? safeErrorMessage(secretResult.error, "无法读取系统密钥链状态。")
            : null;
        setResource({
          phase: "ready",
          metadata,
          config,
          secrets: secretError
            ? []
            : mergeSecretCatalog(secretResult.value, providers, config.value.model.provider),
          secretError,
        });
      } catch (error) {
        if (controller.signal.aborted || isAbortError(error)) return;
        setResource({
          phase: "error",
          message: safeErrorMessage(error, "无法加载 Profile 配置。"),
        });
      }
    })();
    return () => controller.abort();
  }, [client, providers, resourceEpoch, selectedProfileId, workspace.phase]);

  const selectedProfile = profiles.find((profile) => profile.id === selectedProfileId) ?? null;

  const refreshProfiles = async (preferredId?: string) => {
    const profileList = await client.listProfiles();
    setProfiles(profileList);
    setSelectedProfileId((current) => {
      if (preferredId && profileList.some((profile) => profile.id === preferredId)) return preferredId;
      if (current && profileList.some((profile) => profile.id === current)) return current;
      return profileList.find((profile) => profile.isActive)?.id ?? profileList[0]?.id ?? null;
    });
  };

  const createProfile = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (actionBusy) return;
    const input: CreateProfileInput = {
      id: createId.trim(),
      displayName: createName.trim(),
      cloneFromProfileId: cloneFrom || null,
    };
    const fingerprint = JSON.stringify(input);
    if (!createAttemptRef.current || createAttemptRef.current.fingerprint !== fingerprint) {
      createAttemptRef.current = { fingerprint, idempotencyKey: newIdempotencyKey() };
    }

    setActionBusy(true);
    setCreateError(null);
    try {
      await client.createProfile(input, createAttemptRef.current.idempotencyKey);
      await refreshProfiles(input.id);
      createAttemptRef.current = null;
      setCreateId("");
      setCreateName("");
      setCloneFrom("");
      setShowCreate(false);
    } catch (error) {
      setCreateError(safeErrorMessage(error, "创建 Profile 失败。"));
    } finally {
      setActionBusy(false);
    }
  };

  const activateSelected = async () => {
    if (!selectedProfile || selectedProfile.isActive || actionBusy) return;
    setActionBusy(true);
    setActionError(null);
    try {
      const activated = await client.activateProfile(selectedProfile.id);
      setProfiles((current) => current.map((profile) => (
        profile.id === activated.id ? activated : { ...profile, isActive: false }
      )));
    } catch (error) {
      setActionError(safeErrorMessage(error, "切换活动 Profile 失败。"));
    } finally {
      setActionBusy(false);
    }
  };

  const deleteSelected = async () => {
    if (!selectedProfile || selectedProfile.isDefault || selectedProfile.isActive || actionBusy) return;
    setActionBusy(true);
    setActionError(null);
    try {
      await client.deleteProfile(selectedProfile.id);
      await refreshProfiles();
    } catch (error) {
      setActionError(safeErrorMessage(error, "删除 Profile 失败。"));
    } finally {
      setActionBusy(false);
    }
  };

  const saveMetadata = async (patch: ProfilePatch, metadataEtag: string) => {
    if (!selectedProfileId) return;
    const targetProfileId = selectedProfileId;
    const updated = await client.updateProfile(targetProfileId, patch, metadataEtag);
    setResource((current) => current.phase === "ready" && current.metadata.value.id === targetProfileId
      ? { ...current, metadata: updated }
      : current);
    setProfiles((current) => current.map((profile) => profile.id === updated.value.id
      ? {
        ...profile,
        displayName: updated.value.displayName,
        color: updated.value.color,
        avatarFileId: updated.value.avatarFileId,
        updatedAt: updated.value.updatedAt,
      }
      : profile));
  };

  const saveConfig = async (patch: ProfileConfigPatch, configEtag: string) => {
    if (!selectedProfileId) return;
    const targetProfileId = selectedProfileId;
    const updated = await client.updateProfileConfig(targetProfileId, patch, configEtag);
    setResource((current) => current.phase === "ready" && current.metadata.value.id === targetProfileId
      ? { ...current, config: updated }
      : current);
    setProfiles((current) => current.map((profile) => profile.id === targetProfileId
      ? { ...profile, configRevision: updated.value.revision }
      : profile));
  };

  const putSecret = async (secretName: string, value: string) => {
    if (!selectedProfileId) return;
    const targetProfileId = selectedProfileId;
    const updated = await client.putSecret(targetProfileId, secretName, value);
    setResource((current) => current.phase === "ready" && current.metadata.value.id === targetProfileId
      ? {
        ...current,
        secrets: current.secrets.map((secret) => secret.name === updated.name ? updated : secret),
      }
      : current);
  };

  const deleteSecret = async (secretName: string) => {
    if (!selectedProfileId) return;
    const targetProfileId = selectedProfileId;
    await client.deleteSecret(targetProfileId, secretName);
    setResource((current) => current.phase === "ready" && current.metadata.value.id === targetProfileId
      ? {
        ...current,
        secrets: current.secrets.map((secret) => secret.name === secretName
          ? { name: secret.name, configured: false, storage: "osKeychain" }
          : secret),
      }
      : current);
  };

  if (workspace.phase === "loading") {
    return (
      <div className="profile-state" aria-busy="true">
        <LoaderCircle aria-hidden="true" className="spin" size={28} />
        <h2>正在连接 Profile 服务</h2>
      </div>
    );
  }

  if (workspace.phase === "desktop-required") {
    return (
      <div className="profile-state" data-testid="desktop-required">
        <UserRoundCog aria-hidden="true" size={30} />
        <h2>请在 SynthChat Desktop 中打开</h2>
        <p>Profile、配置与系统密钥链仅通过桌面后端连接提供。</p>
      </div>
    );
  }

  if (workspace.phase === "unavailable") {
    return (
      <div className="profile-state" data-testid="profile-unavailable">
        <CircleSlash2 aria-hidden="true" size={30} />
        <h2>Profile 暂不可用</h2>
        <p>当前 Rust 后端尚未启用 Profile 管理能力。</p>
      </div>
    );
  }

  if (workspace.phase === "error") {
    return (
      <div className="profile-state">
        <CircleSlash2 aria-hidden="true" size={30} />
        <h2>Profile 服务连接失败</h2>
        <p role="alert">{workspace.message}</p>
        <button className="secondary-button" onClick={() => setWorkspaceEpoch((value) => value + 1)} type="button">
          重试
        </button>
      </div>
    );
  }

  return (
    <div className="workspace-panel profiles-panel">
      <aside className="profile-list-pane" aria-label="Profile 列表">
        <div className="profile-list-heading">
          <div>
            <span>PROFILES</span>
            <strong>{profiles.length}</strong>
          </div>
          <button
            aria-label={showCreate ? "关闭创建 Profile" : "创建 Profile"}
            className="icon-button"
            onClick={() => setShowCreate((value) => !value)}
            title={showCreate ? "关闭" : "创建 Profile"}
            type="button"
          >
            {showCreate ? <X aria-hidden="true" size={17} /> : <Plus aria-hidden="true" size={17} />}
          </button>
        </div>

        {showCreate ? (
          <form className="create-profile-form" onSubmit={(event) => void createProfile(event)}>
            <label>
              <span>标识</span>
              <input
                disabled={actionBusy}
                maxLength={64}
                onChange={(event) => setCreateId(event.target.value)}
                pattern="[a-z0-9_][a-z0-9_-]{0,63}"
                placeholder="work"
                required
                value={createId}
              />
            </label>
            <label>
              <span>显示名称</span>
              <input
                disabled={actionBusy}
                onChange={(event) => setCreateName(event.target.value)}
                required
                value={createName}
              />
            </label>
            <label>
              <span>复制配置</span>
              <select disabled={actionBusy} onChange={(event) => setCloneFrom(event.target.value)} value={cloneFrom}>
                <option value="">不复制</option>
                {profiles.map((profile) => (
                  <option key={profile.id} value={profile.id}>{profile.displayName}</option>
                ))}
              </select>
            </label>
            {createError ? <p className="form-error" role="alert">{createError}</p> : null}
            <button className="primary-button" disabled={actionBusy} type="submit">
              <Plus aria-hidden="true" size={16} />
              创建
            </button>
          </form>
        ) : null}

        <ul className="profile-list">
          {profiles.map((profile) => (
            <li key={profile.id}>
              <button
                aria-label={`${profile.displayName} (${profile.id})`}
                aria-current={profile.id === selectedProfileId ? "true" : undefined}
                className={profile.id === selectedProfileId ? "profile-list-item active" : "profile-list-item"}
                onClick={() => setSelectedProfileId(profile.id)}
                type="button"
              >
                <span className="profile-color" style={{ backgroundColor: profile.color ?? "#aab4c0" }} />
                <span className="profile-list-copy">
                  <strong>{profile.displayName}</strong>
                  <small>{profile.id}</small>
                </span>
                <span className="profile-badges">
                  {profile.isActive ? <small className="active-badge">活动</small> : null}
                  {profile.isDefault ? <small>默认</small> : null}
                </span>
              </button>
            </li>
          ))}
        </ul>
      </aside>

      <section className="profile-detail-pane" aria-label="Profile 详情">
        {selectedProfile ? (
          <header className="profile-detail-toolbar">
            <div>
              <span>SELECTED PROFILE</span>
              <strong>{selectedProfile.displayName}</strong>
            </div>
            <div className="toolbar-actions">
              <button
                className="secondary-button"
                disabled={actionBusy || selectedProfile.isActive}
                onClick={() => void activateSelected()}
                type="button"
              >
                <Power aria-hidden="true" size={16} />
                {selectedProfile.isActive ? "当前活动" : "设为活动"}
              </button>
              <button
                aria-label="删除 Profile"
                className="icon-button danger"
                disabled={actionBusy || selectedProfile.isDefault || selectedProfile.isActive}
                onClick={() => void deleteSelected()}
                title="删除 Profile"
                type="button"
              >
                <Trash2 aria-hidden="true" size={16} />
              </button>
            </div>
          </header>
        ) : null}
        {actionError ? <p className="profile-action-error" role="alert">{actionError}</p> : null}

        {resource.phase === "loading" || resource.phase === "idle" ? (
          <div className="profile-detail-state" aria-busy="true">
            <LoaderCircle aria-hidden="true" className="spin" size={24} />
            <span>正在加载配置</span>
          </div>
        ) : resource.phase === "error" ? (
          <div className="profile-detail-state">
            <p role="alert">{resource.message}</p>
            <button className="secondary-button" onClick={() => setResourceEpoch((value) => value + 1)} type="button">
              重新加载
            </button>
          </div>
        ) : (
          <div className="profile-detail-scroll">
            <ProfileConfigForm
              config={resource.config}
              key={resource.metadata.value.id}
              metadata={resource.metadata}
              onReload={() => setResourceEpoch((value) => value + 1)}
              onSaveConfig={saveConfig}
              onSaveMetadata={saveMetadata}
              providers={providers}
            />

            <section className="profile-form-section secret-section" aria-labelledby="profile-secrets-title">
              <div className="profile-section-heading">
                <div>
                  <p>OS KEYCHAIN</p>
                  <h2 id="profile-secrets-title">密钥</h2>
                </div>
              </div>
              {resource.secretError ? <p className="form-error secret-section-error" role="alert">{resource.secretError}</p> : null}
              <div className="secret-list">
                {resource.secrets.map((status) => (
                  <SecretRow
                    key={`${selectedProfileId}:${status.name}`}
                    onDelete={() => deleteSecret(status.name)}
                    onPut={(value) => putSecret(status.name, value)}
                    status={status}
                  />
                ))}
                {!resource.secretError && resource.secrets.length === 0 ? (
                  <p className="empty-state">当前 Provider 不需要密钥。</p>
                ) : null}
              </div>
            </section>
          </div>
        )}
      </section>
    </div>
  );
}
