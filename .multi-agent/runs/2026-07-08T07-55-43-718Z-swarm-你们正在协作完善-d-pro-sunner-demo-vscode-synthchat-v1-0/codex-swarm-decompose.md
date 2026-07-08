**Dependency graph**

P0 roadmap:
- Stability: `refresh/bootstrap/select/loadOlder/delete` 等会触发 `mergeBackendMessagesWithLiveState` 的路径，必须在无 active work 时清理 orphan `desktop-stream`。
- Safety: 不改 Rust/Tauri，不碰 MCP/工具/provider fallback；只限制前端 live stream 保留条件。
- Observability: 本轮只保留测试断言；后续再加 run/turn 诊断日志。
- UX: 正常 active streaming 的 stale refresh 仍保留 live bubble；异常中断恢复时不显示永久 ghost bubble。
- Testing maturity: Store 层补 active preserve + inactive/orphan cleanup 回归。
- Packaging/release: 本轮不改发布链路，只跑 `npm test` / `npm run build`。

P1 roadmap:
- 加 App event 层集成测试，覆盖 `turn_started -> assistant_stream -> turn_finished ok=false`。
- 增加 agent run / queue / processing 状态一致性诊断。
- 优化取消、重试、异常恢复提示文案。

P2 roadmap:
- 记忆系统、MCP、文件/终端/浏览器工具、provider fallback、打包自动更新、跨平台发布矩阵。
- 扩展 E2E/UI 自动化覆盖真实 Tauri 事件顺序。

Core dependency:
`active work snapshot` -> `merge preserve decision` -> `pending/live stream cleanup` -> `streamedAssistantIds convergence` -> `tests/build`.

**Subtasks**

1. 本轮实际执行的 P0: orphan `desktop-stream` cleanup on refresh/merge.
   - In `src/lib/store.ts`, introduce an explicit preserve condition for live assistant streams.
   - Condition should include `processingConversationIds.includes(conversationId)` and active agent work from `activeAgentRuns`, `agentQueue`, `agentRuns`.
   - `processingConversationIds` is needed because normal desktop streaming can start before queue/run refresh catches up.

2. Update merge behavior narrowly.
   - `mergeBackendMessagesWithLiveState` / adjacent helper should not preserve tracked assistant stream messages when preserve condition is false.
   - Pending inactive-conversation stream messages must also be removed, not merely omitted from current active messages.
   - `streamedAssistantIds` must converge by removing IDs for orphan streams that were dropped.

3. Wire refresh callers.
   - For `refreshChatData`, compute active work using the latest fetched `agentQueue` plus current state before merge.
   - For bootstrap scheduler refresh, use fetched `nextRuns` / `nextQueue`.
   - For other navigation/history callers, default behavior can remain preserve-safe unless the implementation centralizes snapshot-based cleanup without broad churn.

4. Tests.
   - Existing active streaming stale-refresh test should explicitly set active processing or active agent work.
   - Add orphan test: create streaming assistant bubble, ensure no `processingConversationIds`, no queue/runs/active run, simulate stale backend refresh/merge, assert message removed and `streamedAssistantIds` empty.
   - Keep existing stage 5-G failure cleanup tests passing.

**Suggested owner for each subtask**

- Codex implementation owner: `src/lib/store.ts` minimal helper/signature changes and call-site wiring.
- Codex test owner: `src/lib/__tests__/storeMessageMerge.test.ts` preferred; use `chatConversationChain.test.ts` only if refresh API mocking is needed.
- Claude review owner: verify no regression to WeChat defer/fallback and normal desktop streaming.
- QA/verifier owner: run `npm test` and `npm run build`, capture failures with exact test names.

**Conflict risks**

- `src/lib/store.ts` is high-conflict central state ownership; serialize writes and avoid formatting churn.
- Do not touch `src/App.tsx` unless implementation proves store-only cleanup cannot cover the refresh path.
- `pendingIncomingMessagesByConversation` is module-level state; tests must call `resetPendingIncomingMessagesForTests`.
- Dropping streams based only on queue/runs is unsafe; active `processingConversationIds` must be part of the preserve condition.
- Do not clear WeChat/pet/user transient messages; target only assistant `desktop-stream` / tracked `streamedAssistantIds`.

**Verification gates**

- Targeted first: `npm test -- src/lib/__tests__/storeMessageMerge.test.ts`
- Full regression: `npm test`
- Build gate: `npm run build`
- Required assertions:
  - Active processing stale refresh preserves assistant `desktop-stream`.
  - No active processing / no active agent work stale refresh removes orphan assistant stream.
  - `streamedAssistantIds` converges after removal.
  - Existing failed stream cleanup tests still pass.
- Decomposition-only note: no files were edited and verification commands were not run in this planning pass.