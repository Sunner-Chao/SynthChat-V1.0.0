import { ChangeEvent, useRef, useState } from "react";
import { Plus, Smile } from "lucide-react";
import { api } from "../../lib/api";
import type { EmojiGroup } from "../../lib/types";
import { BackBtn } from "./_shared";

export function EmojiSettings({
  onBack,
  groups,
  saveGroups,
  uploadImage,
}: {
  onBack?: () => void;
  groups: EmojiGroup[];
  saveGroups: (groups: EmojiGroup[]) => Promise<void>;
  uploadImage: (groupId: string, emotion: string, file: File) => Promise<void>;
}) {
  const fileInput = useRef<HTMLInputElement | null>(null);
  const [uploadGroupId, setUploadGroupId] = useState("");
  const [uploadEmotion, setUploadEmotion] = useState("");

  const addGroup = async () => {
    const name = window.prompt("分组名称", "新分组")?.trim();
    if (!name) return;
    const next = await api.createEmojiGroup(name);
    await saveGroups(next);
  };

  const addEmotion = async (groupId: string) => {
    const emotion = window.prompt("情绪分类名称", "happy")?.trim();
    if (!emotion) return;
    const next = await api.createEmojiEmotion(groupId, emotion);
    await saveGroups(next);
  };

  const renameGroup = async (group: EmojiGroup) => {
    const newName = window.prompt("新的分组名称", group.name)?.trim();
    if (!newName || newName === group.id) return;
    const next = await api.renameEmojiGroup(group.id, newName);
    await saveGroups(next);
  };

  const renameEmotion = async (groupId: string, emotion: string) => {
    const newName = window.prompt("新的情绪分类名称", emotion)?.trim();
    if (!newName || newName === emotion) return;
    const next = await api.renameEmojiEmotion(groupId, emotion, newName);
    await saveGroups(next);
  };

  const deleteGroup = async (groupId: string) => {
    if (!window.confirm("删除该表情包分组？")) return;
    const next = await api.deleteEmojiGroup(groupId);
    await saveGroups(next);
  };

  const deleteEmotion = async (groupId: string, emotion: string) => {
    if (!window.confirm("删除该情绪分类及其中图片？")) return;
    const next = await api.deleteEmojiEmotion(groupId, emotion);
    await saveGroups(next);
  };

  const deleteImage = async (groupId: string, emotion: string, path: string) => {
    const fileName = path.split(/[\\/]/).pop() || "";
    if (!fileName || !window.confirm(`删除图片 ${fileName}？`)) return;
    const next = await api.deleteEmojiImage(groupId, emotion, fileName);
    await saveGroups(next);
  };

  const renameImage = async (groupId: string, emotion: string, path: string) => {
    const fileName = path.split(/[\\/]/).pop() || "";
    const newName = window.prompt("新的图片文件名", fileName)?.trim();
    if (!fileName || !newName || newName === fileName) return;
    const next = await api.renameEmojiImage(groupId, emotion, fileName, newName);
    await saveGroups(next);
  };

  const onFile = async (event: ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(event.target.files ?? []);
    event.target.value = "";
    if (uploadGroupId && uploadEmotion) {
      for (const file of files) {
        await uploadImage(uploadGroupId, uploadEmotion, file);
      }
    }
  };

  return (
    <div className="primary-panel embedded-panel">
      <div className="panel-title action-title">
        <BackBtn onBack={onBack} />
        <div className="panel-title-text"><span>Emoji</span><strong>表情包管理</strong></div>
        <button className="btn-primary" onClick={addGroup} type="button"><Plus size={15} />新建分组</button>
      </div>
      <input accept="image/*" className="hidden-input" multiple onChange={onFile} ref={fileInput} type="file" />

      {groups.length === 0 ? (
        <div className="empty-state compact">
          <div className="empty-icon-wrap"><Smile size={48} strokeWidth={1.5} /></div>
          <p>没有表情包分组</p>
          <button className="btn-primary" onClick={addGroup} type="button">新建分组</button>
        </div>
      ) : (
        <div className="emoji-list">
          {groups.map((group) => (
            <div className="card emoji-card" key={group.id}>
              <div className="emoji-header">
                <div className="emoji-info">
                  <strong>{group.name}</strong>
                  <span className="emoji-meta">{group.emotions.length} 个情绪分类 · {group.images.length} 张图片</span>
                </div>
                <div className="emoji-actions">
                  <button className="btn-secondary-outline" type="button" onClick={() => void addEmotion(group.id)}>新建情绪</button>
                  <button className="btn-secondary-outline" type="button" onClick={() => void renameGroup(group)}>重命名</button>
                  <button className="btn-danger-outline-sm" type="button" onClick={() => void deleteGroup(group.id)}>删除</button>
                </div>
              </div>
              <div className="emoji-emotion-list">
                {group.emotions.map((emotion) => {
                  const images = group.emotionImages?.[emotion] ?? [];
                  return (
                    <div className="emoji-emotion" key={emotion}>
                      <div className="emoji-emotion-head">
                        <strong>{emotion}</strong>
                        <span>{images.length} 张</span>
                        <button className="btn-secondary-outline-sm" type="button"
                          onClick={() => { setUploadGroupId(group.id); setUploadEmotion(emotion); fileInput.current?.click(); }}>
                          上传
                        </button>
                        <button className="btn-secondary-outline-sm" type="button"
                          onClick={() => void renameEmotion(group.id, emotion)}>重命名</button>
                        <button className="btn-danger-outline-sm" type="button"
                          onClick={() => void deleteEmotion(group.id, emotion)}>删除</button>
                      </div>
                      <div className="emoji-image-grid">
                        {images.map((path) => (
                          <div className="emoji-image-item" key={path}>
                            <img src={api.assetUrl(path)} alt={path.split(/[\\/]/).pop() || emotion} />
                            <div>
                              <button className="btn-secondary-outline-sm" type="button"
                                onClick={() => void renameImage(group.id, emotion, path)}>改名</button>
                              <button className="btn-danger-outline-sm" type="button"
                                onClick={() => void deleteImage(group.id, emotion, path)}>删除</button>
                            </div>
                          </div>
                        ))}
                      </div>
                    </div>
                  );
                })}
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
