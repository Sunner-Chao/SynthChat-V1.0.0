/**
 * Final comprehensive simulation tests for the complete set of fixed bugs.
 *
 * This file covers the remaining scenarios that weren't tested in earlier suites:
 * 1. Rust static-map capacity eviction logic (pure TypeScript simulation)
 * 2. BUG-7 guard: incomplete response prevents empty-response recovery double-trigger
 * 3. File tool loop tracker cap behavior
 */

import { describe, expect, it } from "vitest";

// ---------------------------------------------------------------------------
// 1. Static map capacity eviction — generic simulation
// ---------------------------------------------------------------------------

/**
 * Generic capacity-guarded map insert matching the pattern used in
 * execution.rs, acp_session.rs, computer_use.rs, file_tools.rs etc.
 */
function capacityGuardedInsert<V>(
  map: Map<string, V>,
  key: string,
  value: V,
  capacity: number,
  evictionFraction = 0.25
): void {
  if (map.size >= capacity && !map.has(key)) {
    const toEvict = Math.ceil(map.size * evictionFraction);
    const keys = Array.from(map.keys()).slice(0, toEvict);
    for (const k of keys) map.delete(k);
  }
  map.set(key, value);
}

describe("capacity-guarded insert (execution.rs / file_tools.rs pattern)", () => {
  it("allows up to capacity entries without eviction", () => {
    const map = new Map<string, number>();
    for (let i = 0; i < 256; i++) {
      capacityGuardedInsert(map, `key-${i}`, i, 256);
    }
    expect(map.size).toBe(256);
    expect(map.has("key-0")).toBe(true);
    expect(map.has("key-255")).toBe(true);
  });

  it("evicts 25% of oldest entries when capacity is reached", () => {
    const map = new Map<string, number>();
    for (let i = 0; i < 256; i++) {
      capacityGuardedInsert(map, `key-${i}`, i, 256);
    }
    capacityGuardedInsert(map, "key-new", 999, 256);

    // 25% of 256 = 64 evicted, map should be 256 - 64 + 1 = 193
    expect(map.size).toBe(193);
    expect(map.has("key-new")).toBe(true);
    // First 64 entries should be evicted
    expect(map.has("key-0")).toBe(false);
    expect(map.has("key-63")).toBe(false);
    expect(map.has("key-64")).toBe(true);
  });

  it("updating an existing key does NOT trigger eviction even at capacity", () => {
    const map = new Map<string, number>();
    for (let i = 0; i < 256; i++) {
      capacityGuardedInsert(map, `key-${i}`, i, 256);
    }
    // Update existing key — no eviction
    capacityGuardedInsert(map, "key-0", 42, 256);

    expect(map.size).toBe(256);
    expect(map.get("key-0")).toBe(42);
    expect(map.has("key-255")).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// 2. BUG-7 guard: incomplete finish_reason prevents empty-response recovery
// ---------------------------------------------------------------------------

type RecoveryAttempt = {
  triggered: boolean;
  kind: string;
};

/**
 * Simulates the BUG-7-fixed recovery logic from agent_loop.rs and approval_gateway.rs.
 * When finish_reason === "incomplete", the empty-response recovery must NOT fire.
 */
function attemptRecovery(
  content: string,
  finishReason: string | null,
  incompleteAttempted: boolean,
  emptyAttempts: number
): { incomplete: RecoveryAttempt; empty: RecoveryAttempt } {
  const MAX_EMPTY = 3;

  // Incomplete path (always before empty check)
  let incompleteTriggered = false;
  if (finishReason === "incomplete" && !incompleteAttempted) {
    incompleteTriggered = true;
    // Would `continue` in real code — doesn't reach empty check
    return {
      incomplete: { triggered: true, kind: "incomplete_response" },
      empty: { triggered: false, kind: "" }
    };
  }

  // BUG-7 fix: skip empty-response recovery when finish_reason is "incomplete"
  const isEmpty = content.trim().length === 0;
  let emptyTriggered = false;
  if (isEmpty && finishReason !== "incomplete") {
    if (emptyAttempts < MAX_EMPTY) {
      emptyTriggered = true;
    }
  }

  return {
    incomplete: { triggered: incompleteTriggered, kind: incompleteTriggered ? "incomplete_response" : "" },
    empty: { triggered: emptyTriggered, kind: emptyTriggered ? "empty_response" : "" }
  };
}

describe("BUG-7: incomplete finish_reason prevents empty-response double-trigger", () => {
  it("incomplete first occurrence triggers incomplete recovery only", () => {
    const result = attemptRecovery("", "incomplete", false, 0);
    expect(result.incomplete.triggered).toBe(true);
    expect(result.empty.triggered).toBe(false);
  });

  it("incomplete second occurrence (already attempted) skips both recoveries on empty content", () => {
    // After incomplete_response already attempted, content empty, finish_reason=incomplete
    const result = attemptRecovery("", "incomplete", true, 0);
    expect(result.incomplete.triggered).toBe(false); // already attempted
    expect(result.empty.triggered).toBe(false); // guarded by BUG-7 fix
  });

  it("empty content with non-incomplete finish_reason triggers empty recovery", () => {
    const result = attemptRecovery("", "stop", false, 0);
    expect(result.incomplete.triggered).toBe(false);
    expect(result.empty.triggered).toBe(true);
  });

  it("non-empty content with incomplete finish_reason only triggers incomplete recovery", () => {
    const result = attemptRecovery("some content", "incomplete", false, 0);
    expect(result.incomplete.triggered).toBe(true);
    expect(result.empty.triggered).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// 3. File tool loop tracker — per-run cap behavior
// ---------------------------------------------------------------------------

type LoopState = { lastKey: string | null; consecutive: number };

function trackLoopCall(
  tracker: Map<string, LoopState>,
  runId: string,
  key: string,
  capacity: number
): { warning: boolean; blocked: boolean } {
  // Capacity guard
  if (tracker.size >= capacity && !tracker.has(runId)) {
    const evict = Array.from(tracker.keys()).slice(0, Math.ceil(tracker.size * 0.25));
    for (const k of evict) tracker.delete(k);
  }

  const state = tracker.get(runId) ?? { lastKey: null, consecutive: 0 };
  if (state.lastKey === key) {
    state.consecutive += 1;
  } else {
    state.lastKey = key;
    state.consecutive = 1;
  }
  tracker.set(runId, state);

  return {
    warning: state.consecutive === 3,
    blocked: state.consecutive >= 4
  };
}

describe("file tool loop tracker capacity and loop detection", () => {
  it("detects consecutive repeated calls", () => {
    const tracker = new Map<string, LoopState>();
    const r1 = trackLoopCall(tracker, "run-1", "key-a", 512);
    const r2 = trackLoopCall(tracker, "run-1", "key-a", 512);
    const r3 = trackLoopCall(tracker, "run-1", "key-a", 512);
    const r4 = trackLoopCall(tracker, "run-1", "key-a", 512);

    expect(r1.warning).toBe(false);
    expect(r2.warning).toBe(false);
    expect(r3.warning).toBe(true);
    expect(r4.blocked).toBe(true);
  });

  it("resets consecutive count when key changes", () => {
    const tracker = new Map<string, LoopState>();
    trackLoopCall(tracker, "run-1", "key-a", 512);
    trackLoopCall(tracker, "run-1", "key-a", 512);
    trackLoopCall(tracker, "run-1", "key-a", 512);

    // New key — counter resets
    const r = trackLoopCall(tracker, "run-1", "key-b", 512);
    expect(r.warning).toBe(false);
    expect(r.blocked).toBe(false);
  });

  it("evicts old run entries when capacity reached", () => {
    const tracker = new Map<string, LoopState>();
    // Fill to capacity
    for (let i = 0; i < 512; i++) {
      tracker.set(`run-${i}`, { lastKey: "k", consecutive: 1 });
    }
    trackLoopCall(tracker, "run-new", "key", 512);
    // 25% evicted = 128, so size = 512 - 128 + 1 = 385
    expect(tracker.has("run-new")).toBe(true);
    expect(tracker.size).toBeLessThan(512);
  });
});
