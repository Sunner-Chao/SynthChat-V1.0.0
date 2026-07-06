import { memo, useState, useEffect } from "react";
import { api } from "../../lib/api";
import { fileNameFromPath } from "../../lib/emojiUtils";
import type { EmojiGroup } from "../../lib/types";

export const STANDARD_EMOJIS = [
  "😀","😃","😄","😁","😆","😅","😂","🤣","😊","😇",
  "🙂","🙃","😉","😌","😍","🥰","😘","😗","😙","😚",
  "😋","😛","😜","🤪","😝","🤑","🤗","🤭","🤫","🤔",
  "🤐","🤨","😐","😑","😶","😏","😒","🙄","😬","🤥",
  "😎","🤓","🥸","🧐","😕","😟","🙁","☹️","😮","😯",
  "😲","😳","🥺","🥹","😦","😧","😨","😰","😥","😢",
  "😭","😱","😖","😣","😞","😓","😩","😪","🤤","😴",
  "😷","🤒","🤕","🤢","🤮","🤧","🥵","🥶","🥴","😵",
  "😡","😠","🤬","😈","👿","💀","💩","🤡","👻","👽",
  "🤖","😺","😸","😹","😻","😼","😽","🙀","😿","😾",
  "👍","👎","👊","✊","🤛","🤜","👏","🙌","👐","🤲",
  "🤝","🙏","✌️","🤞","🤟","🤘","👌","🤌","👈","👉",
  "👆","👇","☝️","👋","🤙","💪","🦵","🦶","👂","👀",
  "❤️","🧡","💛","💚","💙","💜","🖤","🤍","🤎","💔",
  "💕","💞","💓","💗","💖","💘","💝","💌","💯","💢",
  "💥","💫","💦","💨","🔥","⭐","🌟","✨","🎉","🎈",
  "🎁","🎀","🏆","🏅","🥇","🥈","🥉","⚽","🎵","🎶",
  "🐶","🐱","🐭","🐹","🐰","🦊","🐻","🐼","🐨","🐯",
  "🦁","🐮","🐷","🐸","🐵","🐒","🐔","🐧","🐦","🦅",
  "🌹","🌻","🌷","🌸","🌺","🍀","🍃","🍁","🍂","🌴",
  "🍉","🍊","🍋","🍌","🍍","🍎","🍐","🍑","🍒","🍓",
  "☕","🍵","🍺","🍻","🥂","🍷","🍸","🍹","🍔","🍕"
];

export const EMOJI_TAB_ID = "__emoji__";

export const EmojiPicker = memo(function EmojiPicker({
  groups,
  onEmoji,
  onPick
}: {
  groups: { id: string; name: string; emotionImages?: Record<string, string[]>; images: string[] }[];
  onEmoji: (emoji: string) => void;
  onPick: (path: string) => void;
}) {
  const firstGroupId = groups[0]?.id ?? "";
  const [groupId, setGroupId] = useState(EMOJI_TAB_ID);
  useEffect(() => {
    if (groupId !== EMOJI_TAB_ID && !groups.some((group) => group.id === groupId)) setGroupId(firstGroupId || EMOJI_TAB_ID);
  }, [firstGroupId, groupId, groups]);
  const group = groups.find((item) => item.id === groupId) ?? groups[0];
  const emotionImages = group?.emotionImages && Object.keys(group.emotionImages).length > 0
    ? group.emotionImages
    : (group?.images ?? []).reduce<Record<string, string[]>>((acc, path) => {
        const parts = path.split(/[\\/]/);
        const emotion = parts.length > 1 ? parts[parts.length - 2] : "default";
        acc[emotion] = [...(acc[emotion] ?? []), path];
        return acc;
      }, {});
  return (
    <div className="claw-emoji-picker">
      <div className="claw-emoji-tabs">
        <button className={groupId === EMOJI_TAB_ID ? "active" : ""} onClick={() => setGroupId(EMOJI_TAB_ID)} type="button">
          Emoji
        </button>
        {groups.map((item) => (
          <button className={item.id === groupId ? "active" : ""} key={item.id} onClick={() => setGroupId(item.id)} type="button">
            {item.name}
          </button>
        ))}
      </div>
      <div className="claw-emoji-scroll">
        {groupId === EMOJI_TAB_ID ? (
          <div className="claw-standard-emoji-grid">
            {STANDARD_EMOJIS.map((emoji, index) => (
              <button key={`${emoji}-${index}`} onClick={() => onEmoji(emoji)} type="button">
                {emoji}
              </button>
            ))}
          </div>
        ) : group ? (
          Object.entries(emotionImages).map(([emotion, images]) => images.length > 0 ? (
            <div className="claw-emoji-section" key={emotion}>
              <strong>{emotion}</strong>
              <div className="claw-emoji-grid">
                {images.map((path) => (
                  <button key={path} onClick={() => onPick(path)} type="button" title={fileNameFromPath(path)}>
                    <img src={api.assetUrl(path)} alt={fileNameFromPath(path)} />
                  </button>
                ))}
              </div>
            </div>
          ) : null)
        ) : <small>暂无表情包</small>}
      </div>
    </div>
  );
});
