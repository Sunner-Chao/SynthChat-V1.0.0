import { memo, useCallback, useEffect, useRef, useState } from "react";
import { CheckCircle2, Copy, Sparkles } from "lucide-react";
import { formatTime } from "../../lib/agentRunUtils";
import {
  isCanceledToolEvent,
  materializeToolEvent,
  parseManagedProcessEvent,
  parseToolEvent,
  withToolEventStartedAt
} from "../../lib/toolEventUtils";
import { renderTextForMessage } from "../../lib/messageText";
import {
  messageThinkingCards,
  previewText,
  stripThinkingCardsFromText,
  type MessageRenderMode,
  type ThinkingCard
} from "../../lib/messageRenderUtils";
import type { EmojiPathIndexes } from "../../lib/emojiUtils";
import type { ChatMessage } from "../../lib/types";
import { Avatar } from "../../components/common";
import { ThinkingCards } from "./ThinkingCards";
import { ToolMessage } from "./ToolMessage";
import { ManagedProcessMessage } from "./ManagedProcessMessage";
import { ImagePreviewModal } from "./ImagePreviewModal";
import { MarkdownLite } from "./MarkdownLite";

const MAX_REVEAL_TEXT_CHARS = 8_000;

export type ShortMemoryMessageStat = {
  label: string;
  tone: "tokens" | "messages";
};

function useRevealedText(
  text: string,
  enabled: boolean,
  charsPerSecond: number,
  onDone?: () => void
) {
  const [visibleText, setVisibleText] = useState(enabled ? "" : text);
  const targetTextRef = useRef(text);
  const onDoneRef = useRef(onDone);
  const completedTextRef = useRef("");
  const visibleCountRef = useRef(enabled ? 0 : text.length);
  // Track pending onDone timeout IDs so they can be cancelled when the
  // component unmounts or the effect re-runs, preventing post-unmount state
  // updates on the parent component.
  const onDoneTimerRef = useRef<number | null>(null);

  const scheduleOnDone = (ref: React.MutableRefObject<number | null>) => {
    if (ref.current !== null) window.clearTimeout(ref.current);
    ref.current = window.setTimeout(() => {
      ref.current = null;
      onDoneRef.current?.();
    }, 0);
  };

  useEffect(() => {
    if (!enabled) {
      targetTextRef.current = text;
      visibleCountRef.current = text.length;
      setVisibleText(text);
      if (completedTextRef.current !== text) {
        completedTextRef.current = text;
        scheduleOnDone(onDoneTimerRef);
      }
      return;
    }
    targetTextRef.current = text;
    if (!text) {
      completedTextRef.current = "";
      visibleCountRef.current = 0;
      setVisibleText("");
      onDoneRef.current?.();
      return;
    }
    setVisibleText((current) => {
      const next = text.startsWith(current) ? current : "";
      visibleCountRef.current = next.length;
      if (next && next.length >= text.length && completedTextRef.current !== text) {
        completedTextRef.current = text;
        scheduleOnDone(onDoneTimerRef);
      }
      return next;
    });
  }, [enabled, text]);

  useEffect(() => () => {
    // Cancel any pending onDone callback when the component unmounts.
    if (onDoneTimerRef.current !== null) {
      window.clearTimeout(onDoneTimerRef.current);
      onDoneTimerRef.current = null;
    }
  }, []);

  useEffect(() => {
    onDoneRef.current = onDone;
  }, [onDone]);

  useEffect(() => {
    if (!enabled) return;
    const stepMs = 48;
    let lastTickAt = performance.now();
    const timer = window.setInterval(() => {
      const now = performance.now();
      const elapsedSeconds = Math.max(0.016, (now - lastTickAt) / 1000);
      lastTickAt = now;
      const charsPerStep = Math.max(1, Math.ceil(charsPerSecond * elapsedSeconds));
      setVisibleText((current) => {
        const target = targetTextRef.current;
        visibleCountRef.current = Math.max(current.length, visibleCountRef.current);
        const nextCount = Math.min(target.length, visibleCountRef.current + charsPerStep);
        visibleCountRef.current = nextCount;
        const next = target.slice(0, nextCount);
        if (nextCount >= target.length && completedTextRef.current !== target) {
          completedTextRef.current = target;
        }
        return next;
      });
      // Side effect (setTimeout) must live outside the updater — React can call
      // updaters multiple times in StrictMode and the extra clearTimeout +
      // setTimeout calls would corrupt the pending timer.
      const target = targetTextRef.current;
      if (
        visibleCountRef.current >= target.length &&
        completedTextRef.current === target
      ) {
        scheduleOnDone(onDoneTimerRef);
      }
    }, stepMs);
    return () => {
      window.clearInterval(timer);
      // Cancel any onDone timer that was scheduled by this effect instance
      // so it does not fire after charsPerSecond or enabled changes.
      if (onDoneTimerRef.current !== null) {
        window.clearTimeout(onDoneTimerRef.current);
        onDoneTimerRef.current = null;
      }
    };
  }, [charsPerSecond, enabled]);

  return visibleText;
}

export const MessageRow = memo(function MessageRow({
  message,
  mode,
  elementId,
  thinkingCardsOverride,
  profileName,
  profileAvatar,
  personaName,
  personaAvatar,
  copied,
  onCopy,
  previewCharLimit,
  onFirstStreamChar,
  animateText,
  streamCharsPerSecond,
  onAnimationDone,
  memoryStat,
  runStates,
  emojiPathIndexes
}: {
  message: ChatMessage;
  mode: MessageRenderMode;
  elementId: string;
  thinkingCardsOverride?: ThinkingCard[];
  profileName: string;
  profileAvatar: string;
  personaName: string;
  personaAvatar: string;
  copied: boolean;
  onCopy: (message: ChatMessage) => void;
  previewCharLimit: number;
  onFirstStreamChar?: () => void;
  animateText: boolean;
  streamCharsPerSecond: number;
  onAnimationDone: (messageId: string) => void;
  memoryStat: ShortMemoryMessageStat | null;
  runStates: Map<string, string>;
  emojiPathIndexes: EmojiPathIndexes;
}) {
  const [previewSrc, setPreviewSrc] = useState<string | null>(null);
  const parsedToolEvent = mode !== "thinking" && message.role === "tool" ? parseToolEvent(message.content) : null;
  const toolEvent = parsedToolEvent
    ? materializeToolEvent(withToolEventStartedAt(parsedToolEvent, message.createdAt), parsedToolEvent.runId ? runStates.get(parsedToolEvent.runId) : null)
    : null;
  const processEvent = mode !== "thinking" && message.role === "tool" ? parseManagedProcessEvent(message.content) : null;
  const isUser = message.role === "user";
  const isAgentError = message.source === "desktop-agent-error";
  const rawThinkingCards = thinkingCardsOverride ?? messageThinkingCards(message);
  const thinkingCards = mode !== "content" ? rawThinkingCards : [];
  const visibleText = !isUser && rawThinkingCards.length > 0
    ? stripThinkingCardsFromText(message.content.trim(), rawThinkingCards)
    : message.content.trim();
  const text = mode === "thinking" ? "" : previewText(renderTextForMessage(visibleText), previewCharLimit);
  const canRevealText = mode !== "thinking" && !isUser && !toolEvent && !processEvent;
  const isLiveStreaming = canRevealText && message.source === "desktop-stream";
  const [settlingAfterStream, setSettlingAfterStream] = useState(isLiveStreaming);
  useEffect(() => {
    if (isLiveStreaming) setSettlingAfterStream(true);
  }, [isLiveStreaming]);
  const revealText = canRevealText && text.length <= MAX_REVEAL_TEXT_CHARS && (isLiveStreaming || animateText || settlingAfterStream);
  const handleRevealDone = useCallback(() => {
    if (!isLiveStreaming) setSettlingAfterStream(false);
    onAnimationDone(message.id);
  }, [isLiveStreaming, message.id, onAnimationDone]);
  const displayText = useRevealedText(text, revealText, streamCharsPerSecond, handleRevealDone);
  if (toolEvent && isCanceledToolEvent(toolEvent)) return null;
  if (toolEvent) return <ToolMessage event={toolEvent} />;
  if (processEvent) return <ManagedProcessMessage event={processEvent} />;
  if (!text && thinkingCards.length === 0) return null;
  return (
    <div className={isUser ? "claw-message-row user" : "claw-message-row assistant"} data-message-id={elementId}>
      <Avatar
        name={isUser ? profileName : personaName}
        src={isUser && profileAvatar ? profileAvatar : !isUser && personaAvatar ? personaAvatar : ""}
      />
      <div className="claw-message-content">
        <div className="claw-message-meta">
          <span>{isUser ? profileName : personaName}</span>
          <small>{formatTime(message.createdAt)}{message.source === "wechat" ? " · 微信" : ""}</small>
        </div>
        {thinkingCards.length > 0 ? <ThinkingCards cards={thinkingCards} /> : null}
        {text ? (
          <div
            className={
              isUser
                ? "claw-bubble user"
                : isAgentError
                  ? "claw-bubble assistant error"
                  : revealText
                    ? "claw-bubble assistant streaming"
                    : "claw-bubble assistant"
            }
          >
            <MarkdownLite
              text={displayText}
              onImageClick={setPreviewSrc}
              streaming={revealText}
              onFirstChar={onFirstStreamChar}
              emojiPathIndexes={emojiPathIndexes}
            />
          </div>
        ) : null}
        {!isUser && mode !== "thinking" ? (
          <div className="claw-message-actions">
            {memoryStat ? (
              <span className={`claw-memory-stat ${memoryStat.tone}`}>
                <Sparkles size={12} />
                {memoryStat.label}
              </span>
            ) : null}
            <button className="claw-copy" onClick={() => onCopy(message)} type="button">
              {copied ? <CheckCircle2 size={13} /> : <Copy size={13} />}
              {copied ? "已复制" : "复制"}
            </button>
          </div>
        ) : null}
      </div>
      {previewSrc ? <ImagePreviewModal src={previewSrc} onClose={() => setPreviewSrc(null)} /> : null}
    </div>
  );
});
