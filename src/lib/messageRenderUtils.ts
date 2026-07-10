import type { ChatMessage, LlmProvider } from "./types";
import { fileNameFromPath } from "./emojiUtils";
import { unwrapFinalAnswerEnvelope } from "./messageText";

// ── Types ────────────────────────────────────────────────────────────────────

export type ArtifactTarget = {
  path: string;
  title: string;
  kind: "image" | "file";
  source: string;
};

export type ThinkingCard = {
  key: string;
  provider: string;
  kind: string;
  title: string;
  summary: string;
  redacted: boolean;
  encrypted: boolean;
  streaming: boolean;
};

export type MessageRenderMode = "normal" | "thinking" | "content";

export type MessageRenderItem = {
  key: string;
  elementId: string;
  message: ChatMessage;
  mode: MessageRenderMode;
  cards?: ThinkingCard[];
};

// ── Small utilities ───────────────────────────────────────────────────────────

export function clampCount(
  value: number | undefined,
  fallback: number,
  min: number,
  max: number
) {
  if (!Number.isFinite(value)) return fallback;
  return Math.min(max, Math.max(min, Math.floor(value ?? fallback)));
}

export function previewText(text: string, limit: number) {
  if (text.length <= limit) return text;
  return `${text.slice(0, limit)}\n\n[内容过长，界面仅预览前 ${limit} 个字符；完整内容仍保存在本地数据中。]`;
}

export function composerErrorText(error: unknown) {
  const raw =
    error instanceof Error
      ? error.message
      : typeof error === "string"
        ? error
        : String(error ?? "");
  const text = raw.replace(/^bad request:\s*/i, "").trim();
  if (!text) return "发送失败。";
  return `发送失败：${text.length > 80 ? `${text.slice(0, 80)}...` : text}`;
}

export function hasFileDragData(dataTransfer: DataTransfer | null) {
  if (!dataTransfer) return false;
  if (dataTransfer.files.length > 0) return true;
  return (
    Array.from(dataTransfer.types).includes("Files") ||
    Array.from(dataTransfer.items).some((item) => item.kind === "file")
  );
}

// ── Token / text utilities ────────────────────────────────────────────────────

export function normalizeToolDetailText(text: string) {
  return text.trim().replace(/\s+/g, " ");
}

export function estimateMessageTokens(text: string): number {
  if (!text) return 0;
  let tokens = 0;
  const chars = Array.from(text);
  let i = 0;
  while (i < chars.length) {
    const ch = chars[i];
    const code = ch.codePointAt(0)!;
    if (/\s/.test(ch)) {
      tokens += 0.25;
      i++;
    } else if (/[a-zA-Z]/.test(ch)) {
      const start = i;
      while (i < chars.length && /[a-zA-Z]/.test(chars[i])) i++;
      tokens += Math.ceil((i - start) / 3.5) || 1;
    } else if (/\d/.test(ch)) {
      const start = i;
      while (i < chars.length && /\d/.test(chars[i])) i++;
      tokens += Math.ceil((i - start) / 2.5) || 1;
    } else if (code < 128) {
      tokens += 1;
      i++;
    } else {
      if (
        (code >= 0x4e00 && code <= 0x9fff) ||
        (code >= 0x3400 && code <= 0x4dbf) ||
        (code >= 0xf900 && code <= 0xfaff)
      ) {
        tokens += 1.5;
      } else if (
        (code >= 0x3000 && code <= 0x303f) ||
        (code >= 0xff00 && code <= 0xffef)
      ) {
        tokens += 1;
      } else {
        tokens += 2;
      }
      i++;
    }
  }
  return Math.max(1, Math.ceil(tokens));
}

export function formatTokenK(tokens: number) {
  return `${Math.max(1, Math.round(tokens / 1000))}K`;
}

// ── Provider / model utilities ────────────────────────────────────────────────

export function providerModelOptions(providers: LlmProvider[]) {
  return providers
    .filter((provider) => provider.enabled)
    .map((provider) => ({
      key: `${provider.id}::${provider.model}`,
      providerId: provider.id,
      model: provider.model,
      label: provider.model || "未配置模型"
    }));
}

// ── Artifact utilities ────────────────────────────────────────────────────────

export function artifactKind(
  path: string,
  mimeType?: string | null
): ArtifactTarget["kind"] {
  const lower = path.toLowerCase();
  if (mimeType?.startsWith("image/")) return "image";
  if (/\.(png|jpe?g|webp|gif|bmp|svg)$/i.test(lower)) return "image";
  return "file";
}

export function extractArtifactPaths(text: string): ArtifactTarget[] {
  const targets: ArtifactTarget[] = [];
  const seen = new Set<string>();
  const push = (path: string, source: string) => {
    const clean = path.replace(/[，。；;,.!?]+$/u, "");
    if (!clean || seen.has(clean)) return;
    seen.add(clean);
    targets.push({
      path: clean,
      title: fileNameFromPath(clean),
      kind: artifactKind(clean),
      source
    });
  };
  const mediaMarker =
    /\[media attached:\s*(?:"([^"]+)"|`([^`]+)`|([^\]\(]+?))\s*(?:\(([^)]+)\))?\]/gi;
  let match: RegExpExecArray | null;
  while ((match = mediaMarker.exec(text)) !== null) {
    const path = (match[1] || match[2] || match[3] || "").trim();
    const mimeType = (match[4] || "").trim();
    const clean = path.replace(/[，。；;,.!?]+$/u, "");
    if (!clean || seen.has(clean)) continue;
    seen.add(clean);
    targets.push({
      path: clean,
      title: fileNameFromPath(clean),
      kind: artifactKind(clean, mimeType),
      source: "message"
    });
  }
  const mediaTag =
    /(?:^|\n)\s*`?MEDIA:\s*(?:"([^"\n]+)"|'([^'\n]+)'|`([^`\n]+)`|([A-Za-z]:[\\/][^\n]+|\/[^\n]+|~\/[^\n]+))`?/gi;
  while ((match = mediaTag.exec(text)) !== null) {
    push((match[1] || match[2] || match[3] || match[4] || "").trim(), "message");
  }
  const tagged =
    /(?:MEDIA|media|文件|路径|保存到|saved(?: at| to)?)[：:\s]+[`"]?((?:[A-Za-z]:\\|\/|~\/)[^\s`"'<>]+)[`"]?/g;
  while ((match = tagged.exec(text)) !== null) push(match[1], "message");
  const direct =
    /(?<![\w./:])((?:[A-Za-z]:\\|\/|~\/)[^\s`"'<>]+\.(?:png|jpg|jpeg|webp|gif|bmp|svg|html?|md|txt|json|pdf|xlsx?|csv|zip))/gi;
  while ((match = direct.exec(text)) !== null) push(match[1], "message");
  return targets;
}

// ── Record / array helpers ────────────────────────────────────────────────────

export function recordValue(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

export function arrayValue(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

// ── Thinking card utilities ───────────────────────────────────────────────────

function stripPastePlaceholders(text: string): string {
  return text
    .replace(/\[Pasted text[^\]]*\]/g, "")  // [Pasted text #N +M lines] 占位符
    .replace(/<!--\s*-->/g, "")              // <!-- --> HTML注释占位符
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

export function thinkingCardsFromProviderData(providerData: unknown): ThinkingCard[] {
  const root = recordValue(providerData);
  if (!root) return [];
  const candidates = [
    ...arrayValue(root.thinkingCards),
    ...arrayValue(recordValue(root.responses)?.thinkingCards),
    ...arrayValue(recordValue(root.anthropic)?.thinkingCards)
  ];
  return candidates
    .map((item, index) => {
      const card = recordValue(item);
      if (!card) return null;
      const summary = typeof card.summary === "string" ? stripPastePlaceholders(card.summary).trim() : "";
      const redacted = card.redacted === true;
      const encrypted = card.encrypted === true || card.signature === true;
      const streaming = card.streaming === true;
      if (!summary && !redacted && !encrypted) return null;
      const provider =
        typeof card.provider === "string" && card.provider.trim()
          ? card.provider.trim()
          : "";
      const kind =
        typeof card.kind === "string" && card.kind.trim()
          ? card.kind.trim()
          : "thinking";
      const title =
        typeof card.title === "string" && card.title.trim()
          ? card.title.trim()
          : "模型思考";
      return {
        key: `${provider || "provider"}:${kind}:${index}`,
        provider,
        kind,
        title,
        summary,
        redacted,
        encrypted,
        streaming
      };
    })
    .filter((card): card is ThinkingCard => card !== null);
}

export function messageThinkingCards(message: ChatMessage) {
  return thinkingCardsFromProviderData(message.providerData);
}

export function stripThinkingCardsFromText(
  text: string,
  cards: ThinkingCard[]
): string {
  let output = text;
  for (const card of cards) {
    const summary = card.summary.trim();
    if (summary.length < 8) continue;
    output = output.split(summary).join("");
  }
  return output
    .split(/\n/)
    .map((line) => line.trimEnd())
    .join("\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

export function visibleMessageText(message: ChatMessage): string {
  const base = message.role === "assistant"
    ? unwrapFinalAnswerEnvelope(message.content).trim()
    : message.content.trim();
  if (
    message.role === "user" ||
    message.role === "tool" ||
    message.role === "system"
  )
    return base;
  const cards = messageThinkingCards(message);
  return cards.length > 0 ? stripThinkingCardsFromText(base, cards) : base;
}

// ── Message render item builders ──────────────────────────────────────────────

export function messageRenderItem(
  message: ChatMessage,
  mode: MessageRenderMode = "normal",
  cards?: ThinkingCard[]
): MessageRenderItem {
  const suffix = mode === "normal" ? "" : `:${mode}`;
  return {
    key: `${message.id}${suffix}`,
    elementId: `${message.id}${suffix}`,
    message,
    mode,
    cards
  };
}

export function materializeMessageRenderItem(
  message: ChatMessage
): MessageRenderItem[] {
  if (message.role === "tool") {
    const cards = messageThinkingCards(message);
    return cards.length > 0
      ? [
          messageRenderItem(message, "thinking", cards),
          messageRenderItem(message)
        ]
      : [messageRenderItem(message)];
  }
  if (message.role !== "assistant") return [messageRenderItem(message)];
  const cards = messageThinkingCards(message);
  if (cards.length === 0) return [messageRenderItem(message)];
  const items = [messageRenderItem(message, "thinking", cards)];
  if (visibleMessageText(message)) {
    items.push(messageRenderItem(message, "content"));
  }
  return items;
}

export function thinkingCardsSignature(cards: ThinkingCard[]) {
  return cards
    .map((card) =>
      [
        card.provider,
        card.kind,
        card.summary.trim(),
        card.redacted ? "redacted" : "",
        card.encrypted ? "encrypted" : ""
      ]
        .filter(Boolean)
        .join(":")
    )
    .filter(Boolean)
    .join("|");
}

export function materializeMessageRenderItems(
  messages: ChatMessage[]
): MessageRenderItem[] {
  const items: MessageRenderItem[] = [];
  let lastThinkingSignature = "";
  let previousItemWasThinking = false;
  for (const message of messages) {
    const nextItems = materializeMessageRenderItem(message);
    const first = nextItems[0];
    if (first?.mode === "thinking") {
      const signature = thinkingCardsSignature(first.cards ?? []);
      if (
        signature &&
        (previousItemWasThinking ||
          (first.message.role === "tool" &&
            signature === lastThinkingSignature))
      ) {
        nextItems.shift();
      }
    }
    for (const item of nextItems) {
      items.push(item);
      if (item.mode === "thinking") {
        lastThinkingSignature = thinkingCardsSignature(item.cards ?? []);
        previousItemWasThinking = true;
      } else {
        previousItemWasThinking = false;
        if (item.message.role !== "tool") {
          lastThinkingSignature = "";
        }
      }
    }
  }
  return items;
}
