/**
 * Simulation tests for:
 *
 * 1. conversationScrollPositionCacheRef 500-entry LRU cap (ChatExperience fix)
 *    Previously the Map grew unbounded as users browsed many conversations;
 *    the fix evicts the oldest entry (JS Map insertion order) when size >= 500.
 *
 * 2. selectedPersonaIdRef polling-effect stability (polling fix)
 *    The ref always reflects the latest persona ID so the polling setInterval
 *    does not need `selectedPersona?.id` in its dependency array; previously
 *    a persona change caused the interval to tear down and recreate, briefly
 *    pausing polling.
 */

import { describe, expect, it } from "vitest";

// ---------------------------------------------------------------------------
// 1. Scroll position cache 500-entry cap
// ---------------------------------------------------------------------------

type ScrollMemory = { top: number; anchorMessageId?: string; anchorOffset?: number };

/**
 * Reproduces the corrected saveCurrentScrollPosition from ChatExperience.tsx.
 */
function saveScrollPosition(
  cache: Map<string, ScrollMemory>,
  conversationId: string,
  memory: ScrollMemory
): void {
  if (cache.size >= 500 && !cache.has(conversationId)) {
    const firstKey = cache.keys().next().value;
    if (firstKey !== undefined) cache.delete(firstKey);
  }
  cache.set(conversationId, memory);
}

describe("conversationScrollPositionCacheRef 500-entry cap", () => {
  it("allows up to 500 entries without eviction", () => {
    const cache = new Map<string, ScrollMemory>();
    for (let i = 0; i < 500; i++) {
      saveScrollPosition(cache, `conv-${i}`, { top: i * 10 });
    }
    expect(cache.size).toBe(500);
    // All entries present
    expect(cache.has("conv-0")).toBe(true);
    expect(cache.has("conv-499")).toBe(true);
  });

  it("evicts the oldest entry when size reaches 500 and a new conversation is added", () => {
    const cache = new Map<string, ScrollMemory>();
    for (let i = 0; i < 500; i++) {
      saveScrollPosition(cache, `conv-${i}`, { top: i * 10 });
    }
    // Add a 501st entry — should evict conv-0 (oldest)
    saveScrollPosition(cache, "conv-500", { top: 5000 });

    expect(cache.size).toBe(500);
    expect(cache.has("conv-0")).toBe(false);  // evicted
    expect(cache.has("conv-500")).toBe(true); // new entry present
    expect(cache.has("conv-499")).toBe(true); // recent entries kept
  });

  it("does NOT evict when updating an existing entry at cap", () => {
    const cache = new Map<string, ScrollMemory>();
    for (let i = 0; i < 500; i++) {
      saveScrollPosition(cache, `conv-${i}`, { top: i * 10 });
    }
    // Update existing entry — no eviction should occur
    saveScrollPosition(cache, "conv-0", { top: 9999 });

    expect(cache.size).toBe(500);
    expect(cache.has("conv-0")).toBe(true);
    expect(cache.get("conv-0")?.top).toBe(9999);
  });

  it("preserves insertion order: successive evictions remove the oldest each time", () => {
    const cache = new Map<string, ScrollMemory>();
    for (let i = 0; i < 500; i++) {
      saveScrollPosition(cache, `conv-${i}`, { top: i });
    }
    // Add 3 more entries → 3 evictions
    saveScrollPosition(cache, "new-1", { top: 1001 });
    saveScrollPosition(cache, "new-2", { top: 1002 });
    saveScrollPosition(cache, "new-3", { top: 1003 });

    expect(cache.has("conv-0")).toBe(false); // evicted 1st
    expect(cache.has("conv-1")).toBe(false); // evicted 2nd
    expect(cache.has("conv-2")).toBe(false); // evicted 3rd
    expect(cache.has("conv-3")).toBe(true);  // still present
    expect(cache.has("new-1")).toBe(true);
    expect(cache.has("new-2")).toBe(true);
    expect(cache.has("new-3")).toBe(true);
    expect(cache.size).toBe(500);
  });
});

// ---------------------------------------------------------------------------
// 2. selectedPersonaIdRef polling-effect stability
// ---------------------------------------------------------------------------

/**
 * Simulates the ref-sync pattern from ChatExperience.tsx.
 * The key invariant: the ref always holds the latest persona ID without
 * needing to be in the interval's dependency array.
 */
function createPollingHarness() {
  const personaIdRef = { current: undefined as string | undefined };
  let refreshCallCount = 0;
  let intervalCount = 0;

  function syncPersonaRef(personaId: string | undefined) {
    personaIdRef.current = personaId;
  }

  // Simulates the polling interval reading from the ref
  function pollTick() {
    refreshCallCount += 1;
    return personaIdRef.current;
  }

  // Simulates creating/destroying the interval (each call = one interval lifecycle)
  function createInterval() {
    intervalCount += 1;
    return { tick: pollTick };
  }

  return { personaIdRef, syncPersonaRef, createInterval, refreshCallCount: () => refreshCallCount, intervalCount: () => intervalCount };
}

describe("selectedPersonaIdRef polling-effect stability", () => {
  it("ref always reflects the latest persona ID without interval recreation", () => {
    const harness = createPollingHarness();

    // Interval created once
    const interval = harness.createInterval();
    expect(harness.intervalCount()).toBe(1);

    // Persona changes: only the ref is updated, NOT the interval
    harness.syncPersonaRef("persona-1");
    expect(interval.tick()).toBe("persona-1");

    harness.syncPersonaRef("persona-2");
    expect(interval.tick()).toBe("persona-2");

    harness.syncPersonaRef("persona-3");
    expect(interval.tick()).toBe("persona-3");

    // Interval was NOT recreated during persona changes
    expect(harness.intervalCount()).toBe(1);
  });

  it("ref starts as the initial persona value", () => {
    const harness = createPollingHarness();
    harness.syncPersonaRef("initial-persona");
    const interval = harness.createInterval();
    expect(interval.tick()).toBe("initial-persona");
  });

  it("ref correctly handles undefined (no persona selected)", () => {
    const harness = createPollingHarness();
    const interval = harness.createInterval();
    expect(interval.tick()).toBeUndefined();

    harness.syncPersonaRef("persona-x");
    expect(interval.tick()).toBe("persona-x");

    harness.syncPersonaRef(undefined);
    expect(interval.tick()).toBeUndefined();
  });
});
