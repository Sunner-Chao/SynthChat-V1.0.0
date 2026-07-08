import { memo, useState, useRef } from "react";
import { useSharedNowMs } from "../../lib/useSharedNowMs";
import { AlertCircle, CheckCircle2, Clock, Loader2 } from "lucide-react";
import { formatDurationMs } from "../../lib/agentRunUtils";
import { eventStatusLabel } from "../../lib/toolEventUtils";
import { toolEventElapsedLabel, type CompactStep } from "../../lib/toolDisplayUtils";
import type { ToolEvent } from "../../lib/types";

export const ToolStep = memo(function ToolStep({ event }: { event: ToolEvent }) {
  const status = eventStatusLabel(event);
  const isRunning = event.status === "running";
  return (
    <div className={isRunning ? "claw-step active" : event.ok ? "claw-step done" : "claw-step failed"}>
      {isRunning ? <Loader2 size={15} /> : event.ok ? <CheckCircle2 size={15} /> : <AlertCircle size={15} />}
      <span>{event.title || `${event.serverId}.${event.toolName}`}</span>
      <small>{status} · {event.elapsedMs}ms</small>
    </div>
  );
});

export const TimelineStep = memo(function TimelineStep({ step, isLast }: { step: CompactStep; isLast: boolean }) {
  const [expanded, setExpanded] = useState(step.anyRunning);
  const statusClass = step.anyRunning ? "running" : step.anyFailed ? "failed" : "done";
  const statusIcon = step.anyRunning
    ? <Loader2 size={14} className="claw-tl-icon-spin" />
    : step.anyFailed
      ? <AlertCircle size={14} />
      : <CheckCircle2 size={14} />;
  const fallbackStartedAtMsRef = useRef(Date.now());
  const nowMs = useSharedNowMs(step.anyRunning);
  const elapsedLabel = step.anyRunning ? toolEventElapsedLabel(step.lastEvent, nowMs, fallbackStartedAtMsRef.current) : formatDurationMs(step.totalMs);

  return (
    <div className={`claw-tl-node claw-tl-node--${statusClass}${isLast ? " claw-tl-node--last" : ""}`}>
      <div className="claw-tl-dot">{statusIcon}</div>
      <div className="claw-tl-content">
        <div
          className="claw-tl-head"
          onClick={() => setExpanded((v) => !v)}
          role="button"
          tabIndex={0}
          onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); setExpanded((v) => !v); } }}
        >
          <span className="claw-tl-title">
            {step.title}
            {step.count > 1 ? <span className="claw-tl-count">x{step.count}</span> : null}
          </span>
          <span className="claw-tl-meta">
            <Clock size={11} />
            {elapsedLabel}
          </span>
        </div>
        {expanded ? (
          <div className="claw-tl-detail">
            {step.lastEvent.summary ? <p>{step.lastEvent.summary}</p> : null}
            {step.lastEvent.error ? <p className="claw-error-text">{step.lastEvent.error}</p> : null}
          </div>
        ) : null}
      </div>
    </div>
  );
});
