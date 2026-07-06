import { memo, useState } from "react";
import { ChevronRight, FileText, Zap } from "lucide-react";
import { managedProcessEventLabel, managedProcessEventText } from "../../lib/toolEventUtils";
import type { ManagedProcessEvent } from "../../lib/types";

export const ManagedProcessMessage = memo(function ManagedProcessMessage({ event }: { event: ManagedProcessEvent }) {
  const detail = event.detail ?? {};
  const exitCode = typeof detail.exitCode === "number" ? detail.exitCode : null;
  const line = typeof detail.line === "string" ? detail.line : "";
  const reason = typeof detail.reason === "string" ? detail.reason : "";
  const hasDetails = Boolean(line || reason || event.command || event.cwd);
  const [expanded, setExpanded] = useState(false);

  return (
    <div className="claw-tool-message">
      <div className={`claw-tool-card${expanded ? " claw-tool-card--expanded" : ""}`}>
        <div
          className="claw-tool-head"
          onClick={() => hasDetails && setExpanded((v) => !v)}
          role={hasDetails ? "button" : undefined}
          tabIndex={hasDetails ? 0 : undefined}
          onKeyDown={(e) => { if (hasDetails && (e.key === "Enter" || e.key === " ")) { e.preventDefault(); setExpanded((v) => !v); } }}
        >
          <Zap size={15} />
          <strong>{managedProcessEventLabel(event.type)}</strong>
          <small>{event.label || event.processId}{exitCode !== null ? ` · exit ${exitCode}` : ""}</small>
          {hasDetails ? (
            <span className={`claw-tool-chevron${expanded ? " claw-tool-chevron--open" : ""}`}>
              <ChevronRight size={14} />
            </span>
          ) : null}
        </div>
        <div className={`claw-tool-body${expanded ? " claw-tool-body--open" : ""}`}>
          <div className="claw-tool-body-inner">
            <p>{managedProcessEventText(event)}</p>
            {event.command ? <pre>{event.command}</pre> : null}
            {event.cwd ? (
              <div className="claw-tool-path">
                <FileText size={14} />
                <code>{event.cwd}</code>
              </div>
            ) : null}
          </div>
        </div>
      </div>
    </div>
  );
});
