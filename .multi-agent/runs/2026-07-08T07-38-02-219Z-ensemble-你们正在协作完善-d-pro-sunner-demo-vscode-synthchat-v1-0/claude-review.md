I've read all the relevant files. Here is the review.

---

## Review: Capability Group G — Streaming Bubble Cleanup on Failure

**Selected because** it is the only in-scope change this round (failed/interrupted assistant streaming, stale refresh convergence). No other capability groups were touched.

---

### Finding 1 — `clearStreamingAssistantMessages` correctness ✅

**Location:** `src/lib/store.ts:1933–1953` and helper `clearPendingStreamingAssistantMessages` at`:545–565`.

The function correctly covers both data locations:

1. **Active-conversation messages** — filtered from `state.messages` via `isStreamingAssistantMessage` (checks `source === "desktop-stream"` OR id in `streamedAssistantIds`).
2. **Pending/inactive-conversation messages** — `clearPendingStreamingAssistantMessages` is called first and removes matching entries from the module-level `pendingIncomingMessagesByConversation` map. The removed IDs are collected into `removedIds` and merged with any active removals.
3. **`streamedAssistantIds` set** — pruned by the union of all removed IDs in both stores.

The path works because `upsertIncomingMessage` already marks inactive-conversation streaming messages with `source: "desktop-stream"` *and* tracks their ID in `streamedAssistantIds` (`:1884–1888`). So both predicates in `isStreamingAssistantMessage` can match them in the pending store.

**One low-severity observation:** when only pending messages are removed (inactive-conversation case), `state.messages.filter()` still creates a new array reference even though the content is unchanged. The function returns `{ messages: newRef, streamedAssistantIds: newSet }`, which triggers an extra Zustand update with structurally-identical message content. This is not a bug — the `streamedAssistantIds` change is legitimate and needs to propagate — but it could be avoided with a `messages.length === state.messages.length ? state.messages : messages` guard before the return. LOW severity, no action required.

---

### Finding 2 — `App.tsx` `turn_finished ok=false` guard ✅

**Location:** `src/App.tsx:547–554`.

```typescript
if (payload.type === "turn_finished" && payload.conversationId) {
  hideConversationProcessing(payload.conversationId);
  if (payload.ok === false && !isWechatTurnEvent) {
    discardPendingStreamMessagesForConversation(payload.conversationId);
    clearStreamingAssistantMessages(payload.conversationId);
    scheduleChatRefresh(payload.conversationId ?? null, payload.personaId ?? null, 180);
    return;
  }
  // WeChat / normal path follows...
}
```

`isWechatTurnEvent` at`:513–517` is the union of two checks:
- **Explicit WeChat event**: `eventSource === "wechat"` or `payload.message?.source === "wechat"`.
- **Active WeChat turn in flight**: `activeWechatTurnRef.current.has(payload.conversationId)`.

The second check is the important one. A WeChat turn that finishes with `ok=false` (e.g. provider error mid-turn) will still have the conversation ID in `activeWechatTurnRef` because `turn_started` added it there and `turn_finished` hasn't removed it yet (removal happens inside `scheduleWechatFallbackRefresh` which is called on the WeChat success path below). So the guard correctly suppresses stream cleanup for WeChat failures.

The `discardPendingStreamMessagesForConversation` call is also necessary and present: it drops any buffered chunks sitting in the `pendingStreamMessagesRef` flush window before they can be upserted into the store. Without it, the subsequent `clearStreamingAssistantMessages` would clean the store but a delayed flush could re-insert the ghost bubble. Both calls together are the complete defense. Correct.

---

### Finding 3 — Regression tests ✅

**Active-conversation test** (`storeMessageMerge.test.ts:195–230`): sets up a streaming message in `state.messages`, calls `clearStreamingAssistantMessages`, confirms removal from both `messages` and `streamedAssistantIds`, then runs a `mergeBackendMessagesWithLiveState` with empty backend and confirms no ghost. Correct and sufficient.

**Inactive-conversation test** (`storeMessageMerge.test.ts:232–270`): sets `activeConversationId: "other-conv"` so that conv-1 is inactive, upserts a streaming message (which lands in `pendingIncomingMessagesByConversation` and `streamedAssistantIds` but not in `state.messages`), verifies it surfaces via `mergeBackendMessagesWithLiveState`, calls `clearStreamingAssistantMessages`, and confirms both the tracked ID and the pending message are gone. This directly exercises the scenario described in the implementation notes.

Both tests follow the AAA pattern, are deterministic (no timers, no async), and cover the state transition that was previously missing.

---

### Build / Test Status

Per the Codex implementation notes: `npm test` passed (143 tests across 12 files) and `npm run build` succeeded. The Vite chunk-size warning is pre-existing and out of scope.

---

### No Blocking Issues

The implementation is correct. The two items worth noting for future work:

**Residual risk (LOW):** The `clearStreamingAssistantMessages` function is called from `App.tsx` only on the `turn_finished ok=false` path for non-WeChat sources. If a streaming turn is interrupted by a Tauri window reload or process crash before `turn_finished` is received, the cleanup path is never triggered and a ghost bubble could survive until the next bootstrap clears the store. This is an edge case outside the scope of this round and would require an explicit "recovery on bootstrap" pass to address.

**Next-stage recommendation:** An event-layer or integration-level regression covering the full Tauri event sequence — `turn_started → [assistant_stream chunks] → turn_finished ok=false` — would give end-to-end confidence that the dispatch chain in `App.tsx` invokes `clearStreamingAssistantMessages` at the right moment under real backend timing. The unit tests cover the store layer; the App layer is currently untested for this path.
