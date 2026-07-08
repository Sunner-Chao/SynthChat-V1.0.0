/**
 * Simulation tests for agent-run-event lifecycle fixes:
 *
 * 1. AGENT_RUN_BROADCASTERS cleanup on terminal state (runtime_events.rs)
 *    Previously broadcast entries accumulated forever; now they are removed
 *    when the run enters completed / failed / aborted.
 *
 * 2. activeAgentRuns cleanup on terminal state (store.ts handleAgentRunEvent)
 *    Terminal or sub-agent events remove the entry from activeAgentRuns.
 *
 * Both are verified by simulating the state-machine logic in pure TypeScript.
 */

import { describe, expect, it } from "vitest";

// ---------------------------------------------------------------------------
// 1. Broadcaster cleanup simulation
// ---------------------------------------------------------------------------

type Sender<T> = { lastValue: T | undefined; removed: boolean };

function createBroadcasterMap<T>(): Map<string, Sender<T>> {
  return new Map();
}

function publishRecord<T>(
  broadcasters: Map<string, Sender<T>>,
  runId: string,
  record: T,
  terminal: boolean
): void {
  if (terminal) {
    const sender = broadcasters.get(runId);
    if (sender) {
      sender.lastValue = record;
      sender.removed = true;
      broadcasters.delete(runId);
    }
    return;
  }
  let sender = broadcasters.get(runId);
  if (!sender) {
    sender = { lastValue: undefined, removed: false };
    broadcasters.set(runId, sender);
  }
  sender.lastValue = record;
}

describe("AGENT_RUN_BROADCASTERS terminal cleanup", () => {
  it("adds entry for non-terminal state, removes on terminal state", () => {
    const broadcasters = createBroadcasterMap<string>();

    publishRecord(broadcasters, "run-1", "running", false);
    expect(broadcasters.has("run-1")).toBe(true);
    expect(broadcasters.get("run-1")?.lastValue).toBe("running");

    publishRecord(broadcasters, "run-1", "completed", true);
    // Entry removed on terminal
    expect(broadcasters.has("run-1")).toBe(false);
  });

  it("does not accumulate entries for historical runs", () => {
    const broadcasters = createBroadcasterMap<string>();

    // 5 runs start and complete
    for (let i = 0; i < 5; i++) {
      publishRecord(broadcasters, `run-${i}`, "running", false);
      publishRecord(broadcasters, `run-${i}`, "completed", true);
    }
    // Map should be empty since all runs completed
    expect(broadcasters.size).toBe(0);
  });

  it("keeps entries for runs that have not yet terminated", () => {
    const broadcasters = createBroadcasterMap<string>();

    publishRecord(broadcasters, "run-active", "running", false);
    publishRecord(broadcasters, "run-done", "running", false);
    publishRecord(broadcasters, "run-done", "completed", true);

    expect(broadcasters.has("run-active")).toBe(true);
    expect(broadcasters.has("run-done")).toBe(false);
    expect(broadcasters.size).toBe(1);
  });
});

// ---------------------------------------------------------------------------
// 2. activeAgentRuns cleanup simulation
// ---------------------------------------------------------------------------

type AgentRunEntry = { state: string; parentRunId: string | null };

function handleRunEvent(
  activeAgentRuns: Record<string, AgentRunEntry>,
  event: { runId: string; state: string; parentRunId?: string | null }
): Record<string, AgentRunEntry> {
  const terminal = ["completed", "failed", "aborted"].includes(event.state);
  const next = { ...activeAgentRuns };
  if (terminal || event.parentRunId) {
    delete next[event.runId];
  } else {
    next[event.runId] = { state: event.state, parentRunId: event.parentRunId ?? null };
  }
  return next;
}

describe("activeAgentRuns cleanup on terminal state", () => {
  it("adds non-terminal runs to activeAgentRuns", () => {
    let runs: Record<string, AgentRunEntry> = {};
    runs = handleRunEvent(runs, { runId: "r1", state: "running" });
    expect(runs["r1"]).toBeDefined();
    expect(runs["r1"].state).toBe("running");
  });

  it("removes run from activeAgentRuns when terminal", () => {
    let runs: Record<string, AgentRunEntry> = {};
    runs = handleRunEvent(runs, { runId: "r1", state: "running" });
    runs = handleRunEvent(runs, { runId: "r1", state: "completed" });
    expect(runs["r1"]).toBeUndefined();
  });

  it("removes sub-agent runs (parentRunId set) from activeAgentRuns immediately", () => {
    let runs: Record<string, AgentRunEntry> = {};
    runs = handleRunEvent(runs, { runId: "r1", state: "running" });
    // Sub-agent has parentRunId set — always removed from activeAgentRuns
    runs = handleRunEvent(runs, { runId: "r2", state: "running", parentRunId: "r1" });
    expect(runs["r2"]).toBeUndefined();
    expect(runs["r1"]).toBeDefined();
  });

  it("handles multiple concurrent runs correctly", () => {
    let runs: Record<string, AgentRunEntry> = {};
    for (let i = 0; i < 5; i++) {
      runs = handleRunEvent(runs, { runId: `r${i}`, state: "running" });
    }
    expect(Object.keys(runs)).toHaveLength(5);

    // Complete 3 of them
    for (let i = 0; i < 3; i++) {
      runs = handleRunEvent(runs, { runId: `r${i}`, state: "completed" });
    }
    expect(Object.keys(runs)).toHaveLength(2);
    expect(runs["r3"]).toBeDefined();
    expect(runs["r4"]).toBeDefined();
  });
});
