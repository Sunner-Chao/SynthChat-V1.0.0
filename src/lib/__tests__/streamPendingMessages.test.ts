/**
 * Simulation tests for stream message pending-map logic:
 *
 * BUG-2: stream key collision when message.id is empty
 *   Old key: `${conversationId}:${role}`  → multiple messages share the same
 *   bucket; a later non-final delta overwrites the final flag of an earlier one.
 *
 *   Fix key: `${conversationId}:${role}:${createdAt}` → each stream epoch gets
 *   its own bucket even when id is empty, preventing cross-contamination.
 *
 * BUG-6: agent-run-event double-write clears streaming state
 *   Old path: scheduleStreamMessageUpsert(message, {}, false) always writes
 *   streaming=false, terminating the animation mid-stream.
 *
 *   Fix: preserveStreaming option — streaming=true is kept when the prior entry
 *   had streaming=true and the new call passes preserveStreaming:true.
 */

import { describe, expect, it } from "vitest";

/** Mirrors the ChatMessage fields used by the key and pending-map logic. */
type MinimalMessage = {
  id: string;
  conversationId: string;
  role: string;
  createdAt: string;
};

type PendingEntry = {
  message: MinimalMessage;
  streaming: boolean;
  final: boolean;
};

/** Reproduces the key generation from App.tsx after the BUG-2 fix. */
function streamKey(message: MinimalMessage): string {
  return message.id.trim() || `${message.conversationId}:${message.role}:${message.createdAt}`;
}

/** Reproduces scheduleStreamMessageUpsert's Map mutation after both BUG-2 and BUG-6 fixes. */
function applyStreamUpsert(
  map: Map<string, PendingEntry>,
  message: MinimalMessage,
  options: { streaming?: boolean; final?: boolean; preserveStreaming?: boolean }
): void {
  const key = streamKey(message);
  const previous = map.get(key);
  map.set(key, {
    message,
    streaming: Boolean(options.preserveStreaming
      ? (previous?.streaming || options.streaming)
      : options.streaming),
    final: Boolean(options.final || previous?.final)
  });
}

// ---------------------------------------------------------------------------
// BUG-2 regression suite
// ---------------------------------------------------------------------------
describe("BUG-2: stream key collision when message.id is empty", () => {
  it("two messages with the same conv/role but different createdAt get distinct keys", () => {
    const msgA: MinimalMessage = { id: "", conversationId: "conv-1", role: "assistant", createdAt: "2026-07-08T10:00:00.000Z" };
    const msgB: MinimalMessage = { id: "", conversationId: "conv-1", role: "assistant", createdAt: "2026-07-08T10:00:01.000Z" };

    expect(streamKey(msgA)).not.toBe(streamKey(msgB));
  });

  it("two messages with the same conv/role and SAME createdAt share a key (same stream epoch)", () => {
    const msgA: MinimalMessage = { id: "", conversationId: "conv-1", role: "assistant", createdAt: "2026-07-08T10:00:00.000Z" };
    const msgB: MinimalMessage = { id: "", conversationId: "conv-1", role: "assistant", createdAt: "2026-07-08T10:00:00.000Z" };

    // Same epoch → same stream bucket (correct deduplication)
    expect(streamKey(msgA)).toBe(streamKey(msgB));
  });

  it("a non-final delta for stream B does NOT clear the final flag set by stream A", () => {
    const pending = new Map<string, PendingEntry>();

    const streamA: MinimalMessage = { id: "", conversationId: "conv-1", role: "assistant", createdAt: "T1" };
    const streamB: MinimalMessage = { id: "", conversationId: "conv-1", role: "assistant", createdAt: "T2" };

    // Stream A finishes
    applyStreamUpsert(pending, streamA, { streaming: false, final: true });
    // Stream B sends a non-final delta
    applyStreamUpsert(pending, streamB, { streaming: true, final: false });

    const entryA = pending.get(streamKey(streamA));
    const entryB = pending.get(streamKey(streamB));

    // Stream A's final flag is intact and stored under a separate key
    expect(entryA?.final).toBe(true);
    expect(entryB?.streaming).toBe(true);
    expect(entryB?.final).toBe(false);
  });

  it("messages with a stable non-empty id always use the id as key regardless of createdAt", () => {
    const msgA: MinimalMessage = { id: "msg-abc", conversationId: "conv-1", role: "assistant", createdAt: "T1" };
    const msgB: MinimalMessage = { id: "msg-abc", conversationId: "conv-1", role: "assistant", createdAt: "T2" };

    // Same id → same key (stable identity)
    expect(streamKey(msgA)).toBe("msg-abc");
    expect(streamKey(msgB)).toBe("msg-abc");
  });
});

// ---------------------------------------------------------------------------
// BUG-6 regression suite
// ---------------------------------------------------------------------------
describe("BUG-6: preserveStreaming prevents agent-run-event from clearing streaming state", () => {
  it("without preserveStreaming, a {} write clears streaming=true to false", () => {
    const pending = new Map<string, PendingEntry>();
    const msg: MinimalMessage = { id: "m1", conversationId: "conv-1", role: "assistant", createdAt: "T" };

    // Assistant stream delta arrives first (streaming=true)
    applyStreamUpsert(pending, msg, { streaming: true, final: false });
    expect(pending.get("m1")?.streaming).toBe(true);

    // agent-run-event writes with empty options (old behaviour — the bug)
    applyStreamUpsert(pending, msg, {});
    // streaming is cleared to false → animation terminates prematurely (BUG)
    expect(pending.get("m1")?.streaming).toBe(false);
  });

  it("with preserveStreaming=true, a write does NOT clear an existing streaming=true", () => {
    const pending = new Map<string, PendingEntry>();
    const msg: MinimalMessage = { id: "m1", conversationId: "conv-1", role: "assistant", createdAt: "T" };

    // Assistant stream delta arrives first (streaming=true)
    applyStreamUpsert(pending, msg, { streaming: true, final: false });

    // agent-run-event write with preserveStreaming (fixed behaviour)
    applyStreamUpsert(pending, msg, { preserveStreaming: true });

    // streaming is preserved — animation continues
    expect(pending.get("m1")?.streaming).toBe(true);
  });

  it("preserveStreaming does not resurrect streaming=false once the stream has ended", () => {
    const pending = new Map<string, PendingEntry>();
    const msg: MinimalMessage = { id: "m1", conversationId: "conv-1", role: "assistant", createdAt: "T" };

    // Stream ends with final=true, streaming cleared
    applyStreamUpsert(pending, msg, { streaming: false, final: true });
    expect(pending.get("m1")?.streaming).toBe(false);
    expect(pending.get("m1")?.final).toBe(true);

    // agent-run-event arrives after stream end — should not re-enable streaming
    applyStreamUpsert(pending, msg, { preserveStreaming: true });
    expect(pending.get("m1")?.streaming).toBe(false);
    expect(pending.get("m1")?.final).toBe(true);
  });

  it("final flag accumulates correctly across multiple writes via OR", () => {
    const pending = new Map<string, PendingEntry>();
    const msg: MinimalMessage = { id: "m1", conversationId: "conv-1", role: "assistant", createdAt: "T" };

    applyStreamUpsert(pending, msg, { streaming: true, final: false });
    applyStreamUpsert(pending, msg, { streaming: false, final: true });
    // A subsequent non-final write should not clear the accumulated final flag
    applyStreamUpsert(pending, msg, { preserveStreaming: true });
    expect(pending.get("m1")?.final).toBe(true);
  });
});
