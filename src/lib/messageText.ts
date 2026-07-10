export function stripToolDirectiveBlocks(content: string) {
  const match = /(^|\n)\s*<(?:tool_call|tool_calls|function=|function_call|function_calls|tool_result)(?:\s|>|=)/i.exec(content);
  if (!match || match.index < 0) return content;
  return content.slice(0, match.index).trimEnd();
}

export function isAttachmentContextLine(line: string) {
  const trimmed = line.trim();
  if (!trimmed.startsWith("{") || !trimmed.includes("\"attachment\"")) return false;
  try {
    const parsed = JSON.parse(trimmed) as { type?: string };
    return parsed?.type === "attachment";
  } catch {
    return false;
  }
}

export function isMediaDirectiveLine(line: string) {
  const trimmed = line.trim();
  return trimmed.includes("[media attached:") || /^`?MEDIA:\s*(?:"[^"]+"|'[^']+'|`[^`]+`|.+)`?$/i.test(trimmed);
}

const FINAL_ANSWER_ACTIONS = new Set([
  "final",
  "answer",
  "respond",
  "finish",
  "done"
]);

function finalAnswerJsonCandidate(content: string) {
  const trimmed = content.trim();
  const fenced = /^```(?:json)?\s*([\s\S]*?)\s*```$/i.exec(trimmed);
  return fenced ? fenced[1].trim() : trimmed;
}

function finalAnswerTextFromValue(value: unknown): string | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const record = value as Record<string, unknown>;
  const rawAction = [record.action, record.type, record.decision]
    .find((candidate) => typeof candidate === "string");
  const action = typeof rawAction === "string"
    ? rawAction.trim().toLowerCase()
    : "";
  if (!FINAL_ANSWER_ACTIONS.has(action)) return null;
  const text = [record.content, record.answer, record.message]
    .find((candidate) => typeof candidate === "string");
  return typeof text === "string" ? text : null;
}

type PartialJsonStringField = {
  index: number;
  value: string;
};

function extractPartialJsonStringField(
  raw: string,
  key: string
): PartialJsonStringField | null {
  const needle = `"${key}"`;
  const keyIndex = raw.indexOf(needle);
  if (keyIndex < 0 || raw[keyIndex - 1] === "\\") return null;
  const colonIndex = raw.indexOf(":", keyIndex + needle.length);
  if (colonIndex < 0) return null;
  let cursor = colonIndex + 1;
  while (cursor < raw.length && /\s/.test(raw[cursor])) cursor++;
  if (raw[cursor] !== "\"") return null;
  cursor++;

  let value = "";
  let escaped = false;
  while (cursor < raw.length) {
    const char = raw[cursor++];
    if (!escaped) {
      if (char === "\\") {
        escaped = true;
      } else if (char === "\"") {
        return { index: keyIndex, value };
      } else {
        value += char;
      }
      continue;
    }
    escaped = false;
    if (char === "u") {
      const hex = raw.slice(cursor, cursor + 4);
      if (/^[0-9a-f]{4}$/i.test(hex)) {
        value += String.fromCharCode(Number.parseInt(hex, 16));
        cursor += 4;
      }
      continue;
    }
    const escapes: Record<string, string> = {
      "\"": "\"",
      "\\": "\\",
      "/": "/",
      b: "\b",
      f: "\f",
      n: "\n",
      r: "\r",
      t: "\t"
    };
    value += escapes[char] ?? char;
  }
  return { index: keyIndex, value };
}

function partialFinalAnswerText(raw: string): string | null {
  const actionField = ["action", "type", "decision"]
    .map((key) => extractPartialJsonStringField(raw, key))
    .filter((field): field is PartialJsonStringField => field !== null)
    .sort((left, right) => left.index - right.index)[0];
  if (
    !actionField ||
    actionField.index > 512 ||
    !FINAL_ANSWER_ACTIONS.has(actionField.value.trim().toLowerCase())
  ) {
    return null;
  }
  const contentField = ["content", "answer", "message"]
    .map((key) => extractPartialJsonStringField(raw, key))
    .filter((field): field is PartialJsonStringField => field !== null)
    .filter((field) => field.index > actionField.index)
    .sort((left, right) => left.index - right.index)[0];
  return contentField?.value ?? null;
}

export function unwrapFinalAnswerEnvelope(content: string) {
  const candidate = finalAnswerJsonCandidate(content);
  if (!candidate.startsWith("{")) return content;
  try {
    const parsed = JSON.parse(candidate);
    const text = finalAnswerTextFromValue(parsed);
    return text === null ? content : text;
  } catch {
    return partialFinalAnswerText(candidate) ?? content;
  }
}

export function renderTextForMessage(content: string) {
  return stripToolDirectiveBlocks(content.trim())
    .split(/\r?\n/)
    .filter((line) => !isAttachmentContextLine(line))
    .join("\n")
    .trim();
}

export function displayTextForMessage(content: string) {
  return stripToolDirectiveBlocks(content)
    .split(/\r?\n/)
    .filter((line) => !isAttachmentContextLine(line) && !isMediaDirectiveLine(line))
    .join("\n")
    .trim();
}

export function speechTextForMessage(content: string) {
  const text = renderTextForMessage(content)
    .replace(/\[\[audio_as_voice\]\]/gi, "")
    .replace(/```[\s\S]*?```/g, " ")
    .replace(/`([^`\n]{1,120})`/g, "$1")
    .split(/\r?\n/)
    .filter((line) => !isMediaDirectiveLine(line))
    .map((line) => line.replace(/^>\s?/, "").trim())
    .filter(Boolean)
    .join(" ");
  return sanitizeSpeechText(text);
}

/** Maximum character count for TTS speech text before clipping to a sentence boundary. */
const SPEECH_TEXT_MAX_CHARS = 420;

export function sanitizeSpeechText(value: string, limit = SPEECH_TEXT_MAX_CHARS) {
  const withoutUrls = value.replace(/https?:\/\/\S+/gi, " ");
  const withoutMarkdown = withoutUrls
    .replace(/!\[[^\]]*]\([^)]+\)/g, " ")
    .replace(/\[([^\]]+)]\([^)]+\)/g, "$1")
    .replace(/[*_~#>|]+/g, " ");
  const withoutEmoji = Array.from(withoutMarkdown)
    .filter((ch) => {
      const code = ch.codePointAt(0) ?? 0;
      return !(
        (code >= 0x1F000 && code <= 0x1FAFF) ||
        (code >= 0x2600 && code <= 0x27BF) ||
        code === 0xFE0F
      );
    })
    .join("");
  const punctuationMap: Record<string, string> = {
    "、": "，",
    "；": "；",
    "：": "："
  };
  const cleaned = withoutEmoji
    .replace(/\[[^\]]{0,32}(?:表情|图片|文件|附件|语音|动作|media|emoji)[^\]]{0,32}\]/gi, " ")
    .replace(/[“”]/g, "\"")
    .replace(/[‘’]/g, "'")
    .replace(/[（]/g, "(")
    .replace(/[）]/g, ")")
    .replace(/[，、；：]/g, (match) => punctuationMap[match] ?? match)
    .replace(/[!?！？。,.，、；;：:]{3,}/g, (match) => match[0])
    .replace(/\s+/g, " ")
    .trim();
  if (cleaned.length <= limit) return cleaned;
  const clipped = cleaned.slice(0, limit);
  const sentenceEnd = Math.max(
    clipped.lastIndexOf("。"),
    clipped.lastIndexOf("！"),
    clipped.lastIndexOf("？"),
    clipped.lastIndexOf("."),
    clipped.lastIndexOf("!"),
    clipped.lastIndexOf("?")
  );
  return (sentenceEnd > 80 ? clipped.slice(0, sentenceEnd + 1) : clipped).trim();
}
