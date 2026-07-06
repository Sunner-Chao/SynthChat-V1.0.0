import { describe, it, expect } from "vitest";
import {
  parseToolEvent,
  toolEventStartKey,
  managedProcessEventLabel,
  managedProcessEventText,
  eventStatusLabel,
  isCanceledToolEvent,
  toolEventRank,
} from "../toolEventUtils";
import type { ManagedProcessEvent, ToolEvent } from "../types";

// ---------------------------------------------------------------------------
// parseToolEvent
// ---------------------------------------------------------------------------

describe("parseToolEvent", () => {
  it("returns null for invalid JSON", () => {
    expect(parseToolEvent("not json")).toBeNull();
    expect(parseToolEvent("")).toBeNull();
  });

  it("returns null when type is not toolEvent", () => {
    expect(parseToolEvent(JSON.stringify({ type: "other", event: {} }))).toBeNull();
  });

  it("parses a valid tool event envelope", () => {
    const event = { toolName: "web_search", status: "running" };
    const envelope = { type: "toolEvent", event };
    expect(parseToolEvent(JSON.stringify(envelope))).toEqual(event);
  });

  it("returns null when event field is missing", () => {
    expect(parseToolEvent(JSON.stringify({ type: "toolEvent" }))).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// toolEventStartKey
// ---------------------------------------------------------------------------

describe("toolEventStartKey", () => {
  it("uses callId when available", () => {
    const event = { callId: "call-123", referenceId: "", serverId: "mcp", toolName: "search" };
    expect(toolEventStartKey(event)).toBe("call:call-123");
  });

  it("uses referenceId when callId is absent", () => {
    const event = { callId: "", referenceId: "ref-456", serverId: "mcp", toolName: "search" };
    expect(toolEventStartKey(event)).toBe("ref:ref-456");
  });

  it("falls back to serverId.toolName", () => {
    const event = { callId: "", referenceId: "", serverId: "my-server", toolName: "my-tool" };
    expect(toolEventStartKey(event)).toBe("my-server.my-tool");
  });
});

// ---------------------------------------------------------------------------
// managedProcessEventLabel
// ---------------------------------------------------------------------------

describe("managedProcessEventLabel", () => {
  it("returns Chinese label for known types", () => {
    expect(managedProcessEventLabel("completed")).toBe("进程完成");
    expect(managedProcessEventLabel("stopped")).toBe("进程已停止");
    expect(managedProcessEventLabel("watch_match")).toBe("进程输出匹配");
  });

  it("returns raw type for unknown values", () => {
    expect(managedProcessEventLabel("unknown")).toBe("unknown");
  });
});

// ---------------------------------------------------------------------------
// managedProcessEventText
// ---------------------------------------------------------------------------

describe("managedProcessEventText", () => {
  it("includes processId when label is missing", () => {
    const event: ManagedProcessEvent = {
      processId: "proc-1",
      type: "completed",
      detail: {},
    } as ManagedProcessEvent;
    expect(managedProcessEventText(event)).toContain("proc-1");
  });

  it("uses label over processId", () => {
    const event = {
      processId: "proc-1",
      label: "My Process",
      type: "completed",
      detail: { exitCode: 0 },
    } as unknown as ManagedProcessEvent;
    const result = managedProcessEventText(event);
    expect(result).toContain("My Process");
    expect(result).toContain("exit 0");
  });
});

// ---------------------------------------------------------------------------
// eventStatusLabel
// ---------------------------------------------------------------------------

describe("eventStatusLabel", () => {
  it("returns label for known statuses", () => {
    expect(eventStatusLabel({ status: "running" } as ToolEvent)).toBeTruthy();
    expect(eventStatusLabel({ status: "ok" } as ToolEvent)).toBeTruthy();
    expect(eventStatusLabel({ status: "error", ok: false } as ToolEvent)).toBeTruthy();
  });
});

// ---------------------------------------------------------------------------
// isCanceledToolEvent
// ---------------------------------------------------------------------------

describe("isCanceledToolEvent", () => {
  it("returns false for running events", () => {
    expect(isCanceledToolEvent({ status: "running", ok: true } as ToolEvent)).toBe(false);
  });

  it("returns false for successful events", () => {
    expect(isCanceledToolEvent({ status: "ok", ok: true } as ToolEvent)).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// toolEventRank
// ---------------------------------------------------------------------------

describe("toolEventRank", () => {
  it("returns a numeric rank for any event", () => {
    const rank = toolEventRank({ status: "running" } as ToolEvent);
    expect(typeof rank).toBe("number");
  });
});
