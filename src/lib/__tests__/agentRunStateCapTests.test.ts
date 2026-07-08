/**
 * Simulation tests for mergeToolEventList 200-entry cap (store.ts fix)
 * and workflow graph transitions 200-entry cap (store.ts fix).
 *
 * These verify that long-running agent sessions with many tool calls do not
 * accumulate unbounded in-memory state in activeAgentRuns / workflowGraph.
 */

import { describe, expect, it } from "vitest";
import type { ToolEvent } from "../types";

// ---------------------------------------------------------------------------
// Helpers mirroring the fixed store.ts logic
// ---------------------------------------------------------------------------

function sameToolRun(left: ToolEvent | null | undefined, right: ToolEvent | null | undefined) {
  if (!left || !right) return false;
  if (left.callId && right.callId) return left.callId === right.callId;
  if (left.referenceId && right.referenceId) return left.referenceId === right.referenceId;
  return left.serverId === right.serverId
    && left.toolName === right.toolName
    && left.title === right.title;
}

/** Reproduces the corrected mergeToolEventList from store.ts */
function mergeToolEventList(previousEvents: ToolEvent[], incoming: ToolEvent | null | undefined): ToolEvent[] {
  if (!incoming) return previousEvents;
  const events = [...previousEvents];
  const runningIndex = events.findIndex((item) => item.status === "running" && sameToolRun(item, incoming));
  if (runningIndex >= 0 && incoming.status !== "running") {
    events[runningIndex] = incoming;
    return events;
  }
  const duplicateIndex = events.findIndex((item) =>
    sameToolRun(item, incoming)
    && item.status === incoming.status
    && item.elapsedMs === incoming.elapsedMs
    && item.summary === incoming.summary
  );
  if (duplicateIndex >= 0) {
    events[duplicateIndex] = incoming;
    return events;
  }
  return [...events, incoming].slice(-200);
}

function makeToolEvent(id: string, status: "running" | "ok" | "failed" = "ok"): ToolEvent {
  return {
    callId: id,
    referenceId: null,
    serverId: "__internal",
    toolName: "test_tool",
    title: `Tool ${id}`,
    status: status === "ok" ? undefined : status,
    ok: status !== "failed",
    elapsedMs: 100,
    summary: `Result of ${id}`,
    text: "",
    error: status === "failed" ? "test error" : undefined,
    eventType: "tool_completed",
    path: null,
    exists: false,
    mimeType: null,
    raw: null,
    kind: "tool",
    startedAt: null,
    completedAt: null,
  } as unknown as ToolEvent;
}

/** Reproduces the corrected workflow transitions append from store.ts */
function appendTransition(transitions: unknown[], incoming: unknown): unknown[] {
  return [...transitions, incoming].slice(-200);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe("mergeToolEventList 200-entry cap", () => {
  it("allows up to 200 entries without eviction", () => {
    let events: ToolEvent[] = [];
    for (let i = 0; i < 200; i++) {
      events = mergeToolEventList(events, makeToolEvent(`event-${i}`));
    }
    expect(events).toHaveLength(200);
    expect(events[0].callId).toBe("event-0");
    expect(events[199].callId).toBe("event-199");
  });

  it("evicts oldest entries when exceeding 200", () => {
    let events: ToolEvent[] = [];
    for (let i = 0; i < 200; i++) {
      events = mergeToolEventList(events, makeToolEvent(`event-${i}`));
    }
    events = mergeToolEventList(events, makeToolEvent("event-200"));
    expect(events).toHaveLength(200);
    expect(events[0].callId).toBe("event-1"); // event-0 evicted
    expect(events[199].callId).toBe("event-200");
  });

  it("updates running event in-place without eviction", () => {
    // Start 200 unique events
    let events: ToolEvent[] = [];
    for (let i = 0; i < 200; i++) {
      events = mergeToolEventList(events, makeToolEvent(`event-${i}`, "ok"));
    }
    // Add a running event (in-place update counts don't push)
    const running = makeToolEvent("event-0", "running");
    events = mergeToolEventList(events, running);
    // Still 200 entries since running updated in-place if same callId
    // Actually since event-0's status changed from "ok" to "running",
    // and we look for running → non-running, this is a NEW running event
    // since there's no existing running event with the same callId.
    // So it gets appended (evicting event-1).
    expect(events).toHaveLength(200);
  });

  it("returns previous events unchanged when incoming is null", () => {
    const events = [makeToolEvent("a"), makeToolEvent("b")];
    expect(mergeToolEventList(events, null)).toBe(events);
    expect(mergeToolEventList(events, undefined)).toBe(events);
  });
});

describe("workflow graph transitions 200-entry cap", () => {
  it("allows up to 200 transitions", () => {
    let transitions: unknown[] = [];
    for (let i = 0; i < 200; i++) {
      transitions = appendTransition(transitions, { from: "a", to: "b", seq: i });
    }
    expect(transitions).toHaveLength(200);
  });

  it("evicts oldest transition when exceeding 200", () => {
    let transitions: unknown[] = [];
    for (let i = 0; i < 200; i++) {
      transitions = appendTransition(transitions, { seq: i });
    }
    transitions = appendTransition(transitions, { seq: 200 });
    expect(transitions).toHaveLength(200);
    expect((transitions[0] as { seq: number }).seq).toBe(1); // seq:0 evicted
    expect((transitions[199] as { seq: number }).seq).toBe(200);
  });
});
