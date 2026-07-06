import { ChangeEvent, useEffect, useRef, useState } from "react";
import { api } from "../../lib/api";
import type { ProfileConfig } from "../../lib/types";
import { Avatar } from "../../components/common";
import { BackBtn } from "./_shared";

export function ProfileSettings({
  onBack,
  profile,
  saveProfile,
  uploadAvatar,
  clearAvatar,
}: {
  onBack?: () => void;
  profile: ProfileConfig;
  saveProfile: (profile: ProfileConfig) => Promise<void>;
  uploadAvatar: (file: File) => Promise<void>;
  clearAvatar: () => Promise<void>;
}) {
  const [name, setName] = useState(profile.name);
  useEffect(() => setName(profile.name), [profile.name]);
  const avatarInput = useRef<HTMLInputElement | null>(null);

  const onAvatar = async (event: ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    event.target.value = "";
    if (file) await uploadAvatar(file);
  };

  return (
    <div className="primary-panel embedded-panel">
      <div className="panel-title action-title">
        <BackBtn onBack={onBack} />
        <div className="panel-title-text"><span>Profile</span><strong>个人资料</strong></div>
      </div>
      <div className="card" style={{ margin: "0 16px 12px" }}>
        <div className="profile-detail">
          <Avatar name={profile.name} src={profile.avatarPath ? api.assetUrl(profile.avatarPath) : ""} size="large" />
          <h2>{profile.name}</h2>
          <p>本地用户资料</p>
          <input accept="image/*" className="hidden-input" onChange={onAvatar} ref={avatarInput} type="file" />
          <div className="inline-actions">
            <button className="btn-primary-outline" type="button" onClick={() => avatarInput.current?.click()}>上传头像</button>
            {profile.avatarPath
              ? <button className="btn-secondary-outline" type="button" onClick={() => void clearAvatar()}>清除头像</button>
              : null}
          </div>
        </div>
      </div>
      <div className="card" style={{ margin: "0 16px 12px" }}>
        <div className="card-header">基本信息</div>
        <div className="form-group">
          <div className="form-row">
            <label>昵称</label>
            <input value={name} onChange={(e) => setName(e.target.value)} />
          </div>
        </div>
        <div className="form-hint">修改后点击保存按钮生效</div>
      </div>
      <div style={{ padding: "0 16px 12px" }}>
        <button className="btn-primary" type="button" onClick={() => void saveProfile({ ...profile, name })}>保存资料</button>
      </div>
    </div>
  );
}
