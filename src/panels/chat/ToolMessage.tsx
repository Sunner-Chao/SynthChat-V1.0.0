import { memo, useState, useRef } from "react";
import { useSharedNowMs } from "../../lib/useSharedNowMs";
import { AlertCircle, ChevronRight, Code2, FileText, FolderOpen, Loader2, Terminal, Wrench } from "lucide-react";
import { LocalAssetImage } from "../../components/common";
import { api } from "../../lib/api";
import {
  toolEventReauthInfo,
  toolEventElapsedLabel,
  toolEventPathBadge,
  rawString,
  rawNumber,
  parseTerminalOutput,
  toolEventPayload,
  firstTerminalParts,
  parseInlineTerminalCommand,
  terminalCommandLabel,
} from "../../lib/toolDisplayUtils";
import { normalizeToolDetailText, previewText } from "../../lib/messageRenderUtils";
import type { ToolEvent } from "../../lib/types";

const DEFAULT_MESSAGE_PREVIEW_CHARS = 6_000;

export const ToolMessage = memo(function ToolMessage({ event }: { event: ToolEvent }) {
  const [expanded, setExpanded] = useState(false);
  const fallbackStartedAtMsRef = useRef(Date.now());
  const isRunning = event.status === "running";
  const nowMs = useSharedNowMs(isRunning);
  const canOpen = Boolean(event.path && event.exists);
  const isToolImage = canOpen && (event.eventType === "screenshot" || event.eventType === "image" || Boolean(event.mimeType?.startsWith("image/")));
  const reauthInfo = toolEventReauthInfo(event);
  const summaryText = event.summary?.trim() ?? "";
  const bodyText = event.text?.trim() ?? "";
  const errorText = event.error?.trim() ?? "";
  const payload = toolEventPayload(event);
  const bodyTerminalParts = parseTerminalOutput(bodyText);
  const summaryTerminalParts = parseTerminalOutput(summaryText);
  const errorTerminalParts = parseTerminalOutput(errorText);
  const inlineTerminalParts = firstTerminalParts(
    parseInlineTerminalCommand(bodyText),
    parseInlineTerminalCommand(summaryText),
    parseInlineTerminalCommand(errorText)
  );
  const terminalParts = firstTerminalParts(bodyTerminalParts, summaryTerminalParts, errorTerminalParts, inlineTerminalParts);
  const commandText = rawString(payload.command) || terminalParts.command || "";
  const payloadCwd = rawString(payload.cwd) || rawString(payload.workdir);
  const terminalExitCode = rawNumber(payload.exitCode) ?? terminalParts.exitCode;
  const terminalCwd = payloadCwd || terminalParts.cwd || "";
  const terminalStdout = terminalParts.stdout?.trim() ?? "";
  const terminalStderr = terminalParts.stderr?.trim() ?? "";
  const terminalFallbackOutput = terminalParts.raw && !terminalStdout && !terminalStderr && terminalParts.raw !== commandText ? terminalParts.raw : "";
  const isTerminalTool = event.toolName === "terminal" || Boolean(commandText || terminalParts.cwd || terminalParts.exitCode !== undefined);
  const isFailed = !isRunning && (!event.ok || Boolean(event.error) || (typeof terminalExitCode === "number" && terminalExitCode !== 0));
  const cardTitle = isTerminalTool
    ? `终端命令${commandText ? ` · ${terminalCommandLabel(commandText)}` : ""}`
    : event.title || `${event.serverId}.${event.toolName}`;
  const displaySummary = isTerminalTool && commandText
    ? (isFailed ? "命令执行失败，展开查看命令、工作目录和输出。" : "命令执行完成，展开查看命令、工作目录和输出。")
    : summaryText;
  const pathBadge = toolEventPathBadge(event);
  const elapsedLabel = toolEventElapsedLabel(event, nowMs, fallbackStartedAtMsRef.current);
  const duplicateBody = Boolean(displaySummary && bodyText && normalizeToolDetailText(displaySummary) === normalizeToolDetailText(bodyText));
  const duplicateError = Boolean(errorText && (normalizeToolDetailText(errorText) === normalizeToolDetailText(displaySummary) || normalizeToolDetailText(errorText) === normalizeToolDetailText(bodyText)));
  const hasTerminalDetails = Boolean(commandText || terminalCwd || terminalStdout || terminalStderr || terminalFallbackOutput || terminalExitCode !== undefined);
  const hasDetails = Boolean(displaySummary || event.path || isToolImage || canOpen || (bodyText && !duplicateBody) || (errorText && !duplicateError) || reauthInfo || hasTerminalDetails);
  const statusMeta = [
    isRunning ? "执行中..." : isFailed ? "失败" : "成功",
    elapsedLabel
  ].filter(Boolean).join(" · ");

  return (
    <div className="claw-tool-message">
      <div className={`claw-tool-card${isRunning ? " claw-tool-card--running" : ""}${isFailed ? " claw-tool-card--failed" : ""}${expanded ? " claw-tool-card--expanded" : ""}`}>
        <div
          className="claw-tool-head"
          onClick={() => hasDetails && setExpanded((v) => !v)}
          role={hasDetails ? "button" : undefined}
          tabIndex={hasDetails ? 0 : undefined}
          onKeyDown={(e) => { if (hasDetails && (e.key === "Enter" || e.key === " ")) { e.preventDefault(); setExpanded((v) => !v); } }}
        >
          {isTerminalTool ? <Terminal size={15} /> : <Wrench size={15} />}
          <strong>{cardTitle}</strong>
          <small>{statusMeta}</small>
          {hasDetails ? (
            <span className={`claw-tool-chevron${expanded ? " claw-tool-chevron--open" : ""}`}>
              <ChevronRight size={14} />
            </span>
          ) : null}
        </div>
        <div className={`claw-tool-body${expanded ? " claw-tool-body--open" : ""}`}>
          <div className="claw-tool-body-inner">
            {displaySummary ? <p>{displaySummary}</p> : null}
            {isTerminalTool && commandText ? (
              <div className="claw-tool-command">
                <div className="claw-tool-command-head">
                  <Code2 size={14} />
                  <span>command</span>
                  {typeof terminalExitCode === "number" ? (
                    <span className={`claw-tool-path-badge claw-tool-path-badge--${terminalExitCode === 0 ? "success" : "danger"}`}>{terminalExitCode === 0 ? "成功" : "失败"}</span>
                  ) : null}
                </div>
                <code>{commandText}</code>
              </div>
            ) : null}
            {isTerminalTool && terminalCwd ? (
              <div className="claw-tool-path">
                <FileText size={14} />
                <code>{terminalCwd}</code>
                <span>cwd</span>
              </div>
            ) : null}
            {event.path ? (
              <div className="claw-tool-path">
                <FileText size={14} />
                <code>{event.path}</code>
                <span className={`claw-tool-path-badge claw-tool-path-badge--${pathBadge.tone}`}>{pathBadge.label}</span>
              </div>
            ) : null}
            {isToolImage && event.path ? (
              <LocalAssetImage className="claw-tool-image" src={event.path} alt="tool output" />
            ) : null}
            {canOpen && event.path ? (
              <div className="claw-tool-actions">
                <button onClick={() => void api.openLocalFile(event.path || "")} type="button">打开</button>
                <button onClick={() => void api.revealLocalFile(event.path || "")} type="button"><FolderOpen size={13} />定位</button>
              </div>
            ) : null}
            {isTerminalTool && terminalStdout ? (
              <div className="claw-tool-output">
                <span>stdout</span>
                <pre>{previewText(terminalStdout, DEFAULT_MESSAGE_PREVIEW_CHARS)}</pre>
              </div>
            ) : null}
            {isTerminalTool && terminalStderr ? (
              <div className="claw-tool-output claw-tool-output--stderr">
                <span>stderr</span>
                <pre>{previewText(terminalStderr, DEFAULT_MESSAGE_PREVIEW_CHARS)}</pre>
              </div>
            ) : null}
            {isTerminalTool && terminalFallbackOutput ? (
              <div className={`claw-tool-output${isFailed ? " claw-tool-output--stderr" : ""}`}>
                <span>output</span>
                <pre>{previewText(terminalFallbackOutput, DEFAULT_MESSAGE_PREVIEW_CHARS)}</pre>
              </div>
            ) : null}
            {bodyText && !duplicateBody && !isTerminalTool ? <pre>{previewText(bodyText, DEFAULT_MESSAGE_PREVIEW_CHARS)}</pre> : null}
            {errorText && !duplicateError ? <p className="claw-error-text">{errorText}</p> : null}
            {reauthInfo ? (
              <div className="claw-tool-path">
                <AlertCircle size={14} />
                <code>OAuth {reauthInfo.state}</code>
                <span>{reauthInfo.cacheState} · {reauthInfo.refreshRisk}</span>
              </div>
            ) : null}
          </div>
        </div>
      </div>
    </div>
  );
});
