/**
 * Simulation tests for WeChat state-machine safety guards (BUG-1).
 *
 * When `turn_started` fires for a WeChat conversation but `turn_finished`
 * never arrives (network drop, backend crash, etc.), `activeWechatTurnRef`
 * and `visibleWechatUserRef` would block all future WeChat messages for that
 * conversation indefinitely.
 *
 * The fix registers a 5-minute safety-net timeout at `turn_started` that
 * force-clears the Sets, matching the same logic applied in `turn_finished`.
 *
 * These tests simulate the data-structure and timer mechanics directly.
 * Uses globalThis.setTimeout / clearTimeout so they work in both jsdom and
 * Node (where `window` is not defined).
 */

import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";

const gSetTimeout = (handler: () => void, ms: number): number =>
  (globalThis as typeof globalThis & { setTimeout: typeof setTimeout }).setTimeout(handler, ms) as unknown as number;
const gClearTimeout = (id: number): void =>
  (globalThis as typeof globalThis & { clearTimeout: typeof clearTimeout }).clearTimeout(id);

// ---------------------------------------------------------------------------
// Minimal harness that mirrors the App.tsx WeChat guard data structures
// ---------------------------------------------------------------------------

type ConversationId = string;

type WechatGuardState = {
  activeWechatTurnRef: Set<ConversationId>;
  visibleWechatUserRef: Set<ConversationId>;
  safetyTimers: Map<ConversationId, number>;
  /** Messages deferred because no visible user message has arrived yet */
  deferredMessages: Map<ConversationId, Array<{ id: string }>>;
  flushedConversations: ConversationId[];
  refreshedConversations: ConversationId[];
};

function createState(): WechatGuardState {
  return {
    activeWechatTurnRef: new Set(),
    visibleWechatUserRef: new Set(),
    safetyTimers: new Map(),
    deferredMessages: new Map(),
    flushedConversations: [],
    refreshedConversations: [],
  };
}

const SAFETY_TIMEOUT_MS = 5 * 60 * 1000;

/**
 * Reproduces the turn_started handler from App.tsx for WeChat events.
 */
function handleTurnStarted(
  state: WechatGuardState,
  conversationId: string,
  personaId: string | null
): void {
  // Clear stale deferred state from prior turn
  const prevTimer = state.safetyTimers.get(conversationId);
  if (prevTimer !== undefined) gClearTimeout(prevTimer);
  state.deferredMessages.delete(conversationId);
  state.activeWechatTurnRef.add(conversationId);

  // Safety-net timeout: force-clears the Sets if turn_finished never arrives
  const safetyTimer = gSetTimeout(() => {
    state.safetyTimers.delete(conversationId);
    state.activeWechatTurnRef.delete(conversationId);
    state.visibleWechatUserRef.delete(conversationId);
    // Flush any deferred messages accumulated during the missed turn
    const pending = state.deferredMessages.get(conversationId);
    if (pending) {
      state.flushedConversations.push(conversationId);
      state.deferredMessages.delete(conversationId);
    }
    state.refreshedConversations.push(conversationId);
  }, SAFETY_TIMEOUT_MS);
  state.safetyTimers.set(conversationId, safetyTimer);
  void personaId; // used in production to pass to scheduleChatRefresh
}

/**
 * Reproduces the turn_finished handler from App.tsx for WeChat events.
 */
function handleTurnFinished(
  state: WechatGuardState,
  conversationId: string
): void {
  const safetyTimer = state.safetyTimers.get(conversationId);
  if (safetyTimer !== undefined) {
    gClearTimeout(safetyTimer);
    state.safetyTimers.delete(conversationId);
  }
  gSetTimeout(() => {
    state.activeWechatTurnRef.delete(conversationId);
    state.visibleWechatUserRef.delete(conversationId);
    state.refreshedConversations.push(conversationId);
  }, 750);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe("BUG-1: WeChat state-machine safety-net timeout", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.clearAllTimers();
    vi.useRealTimers();
  });

  it("normal path: turn_finished clears Sets via fallback timer after 750ms", () => {
    const state = createState();
    handleTurnStarted(state, "conv-1", null);

    expect(state.activeWechatTurnRef.has("conv-1")).toBe(true);
    expect(state.safetyTimers.has("conv-1")).toBe(true);

    handleTurnFinished(state, "conv-1");

    // Safety timer is cancelled immediately
    expect(state.safetyTimers.has("conv-1")).toBe(false);

    // Sets still populated until the 750ms fallback fires
    expect(state.activeWechatTurnRef.has("conv-1")).toBe(true);

    vi.advanceTimersByTime(750);

    // Now they are cleared
    expect(state.activeWechatTurnRef.has("conv-1")).toBe(false);
    expect(state.visibleWechatUserRef.has("conv-1")).toBe(false);
    expect(state.refreshedConversations).toContain("conv-1");
  });

  it("safety-net path: turn_finished never arrives → Sets are cleared after 5 minutes", () => {
    const state = createState();
    handleTurnStarted(state, "conv-2", null);

    expect(state.activeWechatTurnRef.has("conv-2")).toBe(true);
    expect(state.safetyTimers.has("conv-2")).toBe(true);

    // Simulate backend not sending turn_finished for 5 minutes
    vi.advanceTimersByTime(SAFETY_TIMEOUT_MS);

    // Safety timer fires and clears the guard state
    expect(state.activeWechatTurnRef.has("conv-2")).toBe(false);
    expect(state.visibleWechatUserRef.has("conv-2")).toBe(false);
    expect(state.safetyTimers.has("conv-2")).toBe(false);
    expect(state.refreshedConversations).toContain("conv-2");
  });

  it("safety-net clears before 5 minutes when turn_finished arrives", () => {
    const state = createState();
    handleTurnStarted(state, "conv-3", null);

    // turn_finished arrives after 30 seconds (well before 5-minute timeout)
    vi.advanceTimersByTime(30_000);
    handleTurnFinished(state, "conv-3");

    // Safety timer should be gone
    expect(state.safetyTimers.has("conv-3")).toBe(false);

    // Advance past the 5-minute mark to confirm the safety timer doesn't double-fire
    vi.advanceTimersByTime(SAFETY_TIMEOUT_MS);

    // Only one refresh should have been scheduled (from the fallback, not the safety net)
    const refreshCount = state.refreshedConversations.filter((id) => id === "conv-3").length;
    expect(refreshCount).toBe(1);
  });

  it("second turn_started before turn_finished resets the safety timer", () => {
    const state = createState();

    // First turn starts
    handleTurnStarted(state, "conv-4", null);
    const firstTimer = state.safetyTimers.get("conv-4");
    expect(firstTimer).toBeDefined();

    // Advance 2 minutes
    vi.advanceTimersByTime(2 * 60 * 1000);

    // Second turn starts (without first turn_finished)
    handleTurnStarted(state, "conv-4", null);
    const secondTimer = state.safetyTimers.get("conv-4");
    expect(secondTimer).toBeDefined();
    // A new timer should be registered (old one was cleared)
    expect(secondTimer).not.toBe(firstTimer);

    // The original timer should not fire at the 5-minute mark from the first turn
    // (i.e., at 3 more minutes from now = 5 total from first turn start)
    vi.advanceTimersByTime(3 * 60 * 1000);
    // Should still be active since the second timer hasn't reached 5 minutes yet
    expect(state.activeWechatTurnRef.has("conv-4")).toBe(true);

    // Advance to the full 5 minutes from the second turn start
    vi.advanceTimersByTime(2 * 60 * 1000);
    expect(state.activeWechatTurnRef.has("conv-4")).toBe(false);
  });

  it("safety-net flushes deferred messages when firing", () => {
    const state = createState();
    handleTurnStarted(state, "conv-5", null);

    // Simulate deferred messages accumulating
    state.deferredMessages.set("conv-5", [{ id: "msg-1" }, { id: "msg-2" }]);

    vi.advanceTimersByTime(SAFETY_TIMEOUT_MS);

    // Deferred messages should be flushed
    expect(state.flushedConversations).toContain("conv-5");
    expect(state.deferredMessages.has("conv-5")).toBe(false);
  });
});
