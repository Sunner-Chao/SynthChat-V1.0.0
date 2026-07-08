import { memo, useMemo } from "react";
import { parseToolEvent, isCanceledToolEvent, materializeToolEvent, toolEventMessageKey, toolEventRank } from "../../lib/toolEventUtils";
import { materializeMessageRenderItems } from "../../lib/messageRenderUtils";
import type { ChatMessage } from "../../lib/types";
import type { EmojiPathIndexes } from "../../lib/emojiUtils";
import { MessageRow, type ShortMemoryMessageStat } from "./MessageRow";

export const MessageList = memo(function MessageList({
  messages,
  profileName,
  profileAvatar,
  personaName,
  personaAvatar,
  copiedMessageId,
  onCopy,
  previewCharLimit,
  onFirstStreamChar,
  animatedMessageIds,
  streamCharsPerSecond,
  onMessageAnimationDone,
  memoryStats,
  runStates,
  emojiPathIndexes,
}: {
  messages: ChatMessage[];
  profileName: string;
  profileAvatar: string;
  personaName: string;
  personaAvatar: string;
  copiedMessageId: string | null;
  onCopy: (message: ChatMessage) => void;
  previewCharLimit: number;
  onFirstStreamChar?: () => void;
  animatedMessageIds: Set<string>;
  streamCharsPerSecond: number;
  onMessageAnimationDone: (messageId: string) => void;
  memoryStats: Map<string, ShortMemoryMessageStat>;
  runStates: Map<string, string>;
  emojiPathIndexes: EmojiPathIndexes;
}) {
  const renderItems = useMemo(() => {
    const sliced = messages;
    const selectedToolMessages = new Map<string, { index: number; event: ReturnType<typeof parseToolEvent>; message: ChatMessage }>();
    const toolKeys = new Map<string, string>();
    const suppressedToolKeys = new Set<string>();

    for (let i = 0; i < sliced.length; i++) {
      const msg = sliced[i];
      if (msg.role !== "tool") continue;
      const evt = parseToolEvent(msg.content);
      if (!evt) continue;
      const materialized = materializeToolEvent(evt, evt.runId ? runStates.get(evt.runId) ?? null : null);
      const key = toolEventMessageKey(materialized);
      toolKeys.set(msg.id, key);
      if (isCanceledToolEvent(materialized)) {
        selectedToolMessages.delete(key);
        suppressedToolKeys.add(key);
        continue;
      }
      suppressedToolKeys.delete(key);
      const previous = selectedToolMessages.get(key);
      if (
        !previous
        || toolEventRank(materialized) > toolEventRank(previous.event!)
        || (toolEventRank(materialized) === toolEventRank(previous.event!) && i > previous.index)
      ) {
        selectedToolMessages.set(key, { index: i, event: materialized, message: msg });
      }
    }

    const deduped: typeof sliced = [];
    for (let i = 0; i < sliced.length; i++) {
      const msg = sliced[i];
      if (msg.role === "tool") {
        const key = toolKeys.get(msg.id);
        if (key && suppressedToolKeys.has(key)) continue;
        if (key) {
          const selected = selectedToolMessages.get(key);
          if (!selected || selected.message.id !== msg.id) continue;
        }
      }
      deduped.push(msg);
    }
    return materializeMessageRenderItems(deduped);
  }, [messages, runStates]);

  return (
    <>
      {renderItems.map((item) => (
        <MessageRow
          key={item.key}
          message={item.message}
          mode={item.mode}
          elementId={item.elementId}
          thinkingCardsOverride={item.cards}
          profileName={profileName}
          profileAvatar={profileAvatar}
          personaName={personaName}
          personaAvatar={personaAvatar}
          copied={item.mode !== "thinking" && copiedMessageId === item.message.id}
          onCopy={onCopy}
          previewCharLimit={previewCharLimit}
          onFirstStreamChar={item.mode === "thinking" ? undefined : onFirstStreamChar}
          animateText={item.mode !== "thinking" && animatedMessageIds.has(item.message.id)}
          streamCharsPerSecond={streamCharsPerSecond}
          onAnimationDone={onMessageAnimationDone}
          memoryStat={item.mode === "thinking" ? null : memoryStats.get(item.message.id) ?? null}
          runStates={runStates}
          emojiPathIndexes={emojiPathIndexes}
        />
      ))}
    </>
  );
});
