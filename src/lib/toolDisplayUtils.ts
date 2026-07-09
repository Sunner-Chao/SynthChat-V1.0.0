import { formatDurationMs } from "./agentRunUtils";
import { isCanceledToolEvent, toolEventStartedAt } from "./toolEventUtils";
import type { ToolEvent } from "./types";

export interface CompactStep {
  key: string;
  title: string;
  count: number;
  allOk: boolean;
  anyRunning: boolean;
  anyFailed: boolean;
  totalMs: number;
  lastEvent: ToolEvent;
}

export type TerminalOutputParts = {
  cwd?: string;
  exitCode?: number;
  command?: string;
  stdout?: string;
  stderr?: string;
  raw?: string;
};

export function rawObject(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

export function rawString(value: unknown) {
  return typeof value === "string" && value.trim() ? value.trim() : "";
}

export function rawNumber(value: unknown): number | undefined {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (typeof value === "string" && /^-?\d+$/.test(value.trim())) return Number(value.trim());
  return undefined;
}

export function parseTerminalOutput(value: string): TerminalOutputParts {
  const text = value.trim();
  if (!text) return {};
  const match = text.match(/(?:^|\n)cwd:\s*(.*?)\n(?:(?:transport|backend|target|sandbox|mode|sync|sessionCwd|image):.*?\n)*exitCode:\s*(-?\d+|unknown)\nstdout:\n([\s\S]*?)\nstderr:\n([\s\S]*)$/);
  if (!match) return {};
  const exitCode = match[2] === "unknown" ? undefined : Number(match[2]);
  return {
    cwd: match[1]?.trim() || undefined,
    exitCode: Number.isFinite(exitCode) ? exitCode : undefined,
    stdout: match[3]?.trimEnd() || "",
    stderr: match[4]?.trimEnd() || "",
    raw: text
  };
}

export function toolEventPayload(event: ToolEvent): Record<string, unknown> {
  const raw = rawObject(event.raw);
  return rawObject(raw.payload);
}

export function firstTerminalParts(...items: TerminalOutputParts[]): TerminalOutputParts {
  return items.find((item) => Boolean(item.raw)) ?? {};
}

export function parseInlineTerminalCommand(value: string): TerminalOutputParts {
  const text = value.trim();
  if (!text) return {};
  const match = text.match(/^([\s\S]*?)\s+[·-]\s+exit\s+(-?\d+)\s*$/i);
  if (!match) return {};
  const command = match[1]?.trim() || "";
  if (!command) return {};
  return {
    command,
    exitCode: Number(match[2]),
    raw: command
  };
}

export function terminalCommandLabel(command: string) {
  const first = command.trim().split(/\s+/)[0] || "terminal";
  if (/yt-dlp(?:\.exe)?$/i.test(first)) return "yt-dlp 下载";
  if (/npx(?:\.cmd|\.exe)?$/i.test(first)) return "npx";
  if (/powershell(?:\.exe)?$/i.test(first) || /pwsh(?:\.exe)?$/i.test(first)) return "PowerShell";
  if (/cmd(?:\.exe)?$/i.test(first)) return "cmd";
  return first;
}

export function compactSteps(events: ToolEvent[]): CompactStep[] {
  const result: CompactStep[] = [];
  // Hard cap so very long agent runs (hundreds of tool calls) don't flood the
  // DOM with unvirtualized nodes and make the UI unresponsive.
  const MAX_COMPACT_STEPS = 200;
  for (const event of events.filter((item) => !isCanceledToolEvent(item))) {
    if (result.length >= MAX_COMPACT_STEPS) break;
    const title = event.title || `${event.serverId}.${event.toolName}`;
    const prev = result[result.length - 1];
    if (prev && prev.title === title && !prev.anyRunning && !event.status) {
      prev.count++;
      prev.allOk = prev.allOk && event.ok;
      prev.anyFailed = prev.anyFailed || (!event.ok && event.status !== "running");
      prev.totalMs += event.elapsedMs;
      prev.lastEvent = event;
    } else {
      result.push({
        // Use a stable key that does NOT include elapsedMs — that value
        // changes on every streaming update while a tool is running, which
        // causes the TimelineStep component to unmount/remount on every tick,
        // resetting expanded state and the elapsed-time baseline ref.
        key: `${event.serverId}:${event.toolName}:${event.callId ?? event.referenceId ?? result.length}`,
        title,
        count: 1,
        allOk: event.ok,
        anyRunning: event.status === "running",
        anyFailed: !event.ok && event.status !== "running",
        totalMs: event.elapsedMs,
        lastEvent: event
      });
    }
  }
  return result;
}

export function toolEventReauthInfo(event: ToolEvent): { state: string; cacheState: string; refreshRisk: string } | null {
  const raw = event.raw as Record<string, any> | null | undefined;
  const errorJson = raw?.errorJson as Record<string, any> | null | undefined;
  const needsReauth = raw?.needsReauth === true || errorJson?.needsReauth === true || errorJson?.needs_reauth === true;
  if (!needsReauth) return null;
  const oauthStatus = errorJson?.oauthStatus as Record<string, any> | null | undefined;
  const tokenStatus = oauthStatus?.tokenStatus as Record<string, any> | null | undefined;
  return {
    state: String(oauthStatus?.state ?? "needs_reauth"),
    cacheState: String(tokenStatus?.cacheState ?? "n/a"),
    refreshRisk: String(tokenStatus?.refreshRisk ?? "n/a")
  };
}

export function toolEventElapsedLabel(event: ToolEvent, nowMs = Date.now(), fallbackStartedAtMs?: number): string {
  if (event.status === "running") {
    const startedAt = toolEventStartedAt(event);
    const startedMs = startedAt ? new Date(startedAt).getTime() : NaN;
    const fallbackMs = Number.isFinite(fallbackStartedAtMs) ? Number(fallbackStartedAtMs) : nowMs;
    const liveElapsedMs = Number.isFinite(startedMs)
      ? Math.max(0, nowMs - startedMs)
      : Math.max(1, nowMs - fallbackMs);
    const elapsedMs = Math.max(event.elapsedMs, liveElapsedMs);
    return formatDurationMs(Math.max(1, elapsedMs));
  }
  if (event.elapsedMs > 0) {
    return formatDurationMs(event.elapsedMs);
  }
  if (!event.ok || event.status === "failed") {
    return "即时返回";
  }
  return "0ms";
}

export function toolEventPathBadge(event: ToolEvent): { label: string; tone: "neutral" | "success" | "warning" | "danger" } {
  const errorText = `${event.summary ?? ""} ${event.error ?? ""}`.toLowerCase();
  if (event.status === "running") {
    return { label: "检查中", tone: "warning" };
  }
  if (typeof event.exists === "boolean") {
    return event.exists
      ? { label: "存在", tone: "success" }
      : { label: "文件不存在", tone: "danger" };
  }
  if (errorText.includes("file registry stale check failed")) {
    return { label: "读状态已失效", tone: "warning" };
  }
  if (errorText.includes("cannot read current file") || errorText.includes("os error 2")) {
    return { label: "无法读取", tone: "danger" };
  }
  if (!event.ok || event.status === "failed") {
    return { label: "未校验", tone: "neutral" };
  }
  return { label: "未提供状态", tone: "neutral" };
}
