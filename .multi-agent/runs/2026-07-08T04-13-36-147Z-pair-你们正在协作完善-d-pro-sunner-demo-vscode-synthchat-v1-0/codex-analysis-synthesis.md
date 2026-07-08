# codex-analysis-synthesis：阶段 3 对话链路测试设计最终产物

范围：只读综合。已读取指定的 Stage 1 与 compact plan 文件，并按要求 targeted read 关键前端、Rust、测试文件。未修改文件，未运行测试。

## Findings

主链路确认：

`src/panels/ChatExperience.tsx:1740 submit()` -> `src/lib/store.ts:1994 sendMessage()` -> `src/lib/api.ts:553 sendChatMessage()` -> `src-tauri/src/agent/agent_loop.rs:1162 run_chat_turn()` -> Tauri event -> `src/App.tsx:492 synthchat-chat-event` / `:698 synthchat-agent-run-event` / `:724 synthchat-agent-queue-event` -> `src/lib/store.ts:1843 upsertIncomingMessage()` / `:1674 refreshChatData()` -> `src/panels/ChatExperience.tsx:2058 MessageList`。

关键更正：

- `src/App.tsx:530-555` 是 `turn_finished` 通用处理，不是专门的 stream interruption handling。Rust 在 `src-tauri/src/agent/agent_loop.rs:1236-1241` 发 `turn_finished { ok: result.is_ok() }`，但前端未看到 `ok=false` 独立分支。阶段 4 应把它作为 harness 用例，可能先红。
- `src/lib/messageRenderUtils.ts:251 messageThinkingCards()` 是 thin wrapper；thinking 数据解析锚点应是 `thinkingCardsFromProviderData()` 的构造逻辑，实际渲染在 `src/panels/chat/ThinkingCards.tsx:5`。
- `src/lib/store.ts:2226-2254` 是 `sendMessage()` catch 块，设计上故意不插入 transient 错误气泡；关键可测副作用是 `:2228-2237` 调度 `refreshAgentQueue()` 和 `refreshAgentRuns()`。
- `src-tauri/src/agent/agent_loop.rs:1515` 先保存 `running` run，`:1528` 才校验 provider。provider error 测试应标注为可能红测，用于暴露 run 永久 running 风险。
- `src/lib/types.ts:1467-1473 SendChatRequest.providerData` 是 `unknown | null`。前端测试断言 `clientMessageId` 时需要 cast，例如 `(request.providerData as any).clientMessageId`。
- 审批 UI 不在 `App.tsx` 或 `ChatExperience.tsx`，而在 `src/panels/ToolPanels.tsx:1138 approvals`、`:1292 refreshApprovals()`、`:1506 approve()`、`:1608 deny()`、`:2891 pendingApprovals render`。`App.tsx` 只通过 `synthchat-agent-run-event` 更新 run/message 状态。
- UI 组件测试当前有硬前置：`vitest.config.ts:5` 是 `environment: "node"`，`package.json` 无 `jsdom` / `@testing-library/react`。`MessageList.test.tsx` 之前必须先做测试环境 smoke。

## 测试用例清单

| ID | 覆盖 | 落点 | 类型 | 核心断言 |
|---|---|---|---|---|
| F-01 | 普通聊天 | `src/lib/store.ts:1994 sendMessage()` | Vitest | local user 立即出现；`api.sendChatMessage()` 收到 `conversationId/personaId/agentId/content/providerData.clientMessageId`；后端 user 替换 local user；assistant 最终合入。 |
| R-01 | 普通聊天 | `src-tauri/src/agent/agent_loop.rs:1162 run_chat_turn()` | cargo test | fake/echo provider 下持久化 user + assistant；run 从 `running` 到 `completed`；`store.messages()` 顺序一致。 |
| F-02 | 流式合并 | `src/lib/store.ts:726 mergeBackendMessagesWithLiveState()`、`:1843 upsertIncomingMessage()` | Vitest | `assistant_stream msg-1` 后 stale refresh 不删除 `desktop-stream`；`assistant_message msg-1` 后只有一条 final assistant，`streamedAssistantIds` 清理。 |
| H-01 | App 事件顺序 | `src/App.tsx:289 scheduleStreamMessageUpsert()`、`:492-648 chat listener` | Tauri/mock harness | emit `turn_started -> assistant_stream -> stale refresh -> assistant_message -> turn_finished ok=true`，最终无重复 assistant，processing 清除。 |
| H-01-b | stream 中断 | `App.tsx:530-555` + `agent_loop.rs:1236-1241` | Tauri/mock harness | emit `turn_finished ok=false`，processing 清除；不插入空 assistant；不留下 `desktop-stream` ghost。当前可能暴露 bug。 |
| H-01-c | turn_started | `App.tsx:511-528` | Tauri/mock harness | `processingConversationIds` 包含 conversationId；外部 source 时 section 切换规则正确。 |
| R-02 | stream/final id | `agent_loop.rs:2926-2934`、`:3070-3096` | cargo / Tauri test app | final assistant 复用 streaming message id；反向断言同 conversation 下没有第二条不同 id assistant。 |
| F-03 | thinking 数据 | `src/lib/messageRenderUtils.ts thinkingCardsFromProviderData()`、`MessageRow.tsx:155-188` | Vitest | providerData thinking cards 生成 render item；assistant 正文通过 `stripThinkingCardsFromText()` 不重复显示 thinking summary。 |
| UI-01 | MessageList | `src/panels/chat/MessageList.tsx:41-84` | Vitest component | running/completed tool 同 key 只显示最高 rank；canceled tool 被抑制。 |
| UI-02 | ToolMessage | `src/panels/chat/ToolMessage.tsx:22-163` | Vitest component | 文件工具显示 path badge；失败工具显示失败状态/error；MCP OAuth reauth 信息可见。 |
| UI-03 | ThinkingCards | `src/panels/chat/ThinkingCards.tsx:5-44` | Vitest component | streaming card 默认展开并显示“思考中”；redacted card 显示“已隐藏”。 |
| UI-04 | ManagedProcessMessage | `src/panels/chat/ManagedProcessMessage.tsx:6-48` | Vitest component | completed/stopped/watch_match 显示 label、exit code、command、cwd。 |
| F-04 | agent 选择 | `store.ts:2121-2126`、`api.ts:533 setConversationAgent()` | Vitest | 有效显式 `agentId` 优先；无效 `agentId` 发 `null`；`null/undefined` fallback 到有效 `conversation.agentId`。 |
| R-03 | persona/conversation/agent 绑定 | `agent_loop.rs:3818 resolve_chat_turn_persona_and_agent()`、`store.rs set_conversation_agent()` | cargo test | request.agentId 优先于 conversation.agent_id；persona 优先匹配 agent；切换 conversation agent 同步 personaId。 |
| R-04 | 文件工具 | `tool_dispatch.rs:1425 execute_recovery_internal_tool()`、`:1600 read_file` | cargo test | temp workspace mock `read_file` 成功产生 `ToolEvent{server_id="__internal", tool_name="read_file", ok=true}`；缺失文件产生 failed event/observation。 |
| R-05 | 记忆工具 | `tool_dispatch.rs:1701-1704 recall_memory/remember_fact/manage_memory/memory` | cargo test | mock memory 写入、检索、失败路径均产生可展示 ToolEvent/observation，并绑定 run_id。 |
| R-06 | MCP 工具 | `src-tauri/src/mcp.rs:2605 list_tools()`、`:2681 call_tool()` | cargo test | oneShotJson/local mock server 覆盖 success、tool error、timeout、filter reject、OAuth retry；`ToolTraceEntry` 持久化且 error 净化。 |
| R-07 | approval pending | `agent_loop.rs:2290-2331`、`store.rs:13181 append_tool_approval()` | cargo test | risky tool 进入 `pendingApproval`；产生 approval request；assistant 消息为“工具调用正在等待审批”。 |
| H-02 | approve/deny UI | `api.ts:1146-1149`、`ToolPanels.tsx:1506/1608/2891` | Tauri/mock harness | approve 后刷新 approvals/runs/queue/messages；deny 后 run failed/aborted，approval 历史和 queue 状态同步。 |
| F-05 | abort | `ChatExperience.tsx:1787 stopActiveRun()`、`api.ts:856 abortAgentRun()` | Vitest + API mock | stop 调用 abort；清除 processing；刷新 runs/queue/messages。 |
| R-08 | abort/timeout | `store.rs:12000 abort_agent_run()`、`agent_loop.rs:4232 check_agent_run_interrupted()` | cargo test | parent 与 child runs 均 terminal；`active_agent_run_for_conversation()` 返回 None；running tool event 被关闭。 |
| F-06 | error recovery | `store.ts:2226-2254`、`App.tsx:530-555` | Vitest/Harness | provider reject 不插临时错误气泡；processing 清理；`refreshAgentQueue/refreshRuns` 被调度；tool failed 可见；stream 中断不留 ghost。 |
| R-09 | provider error | `agent_loop.rs:1515`、`:1528` | cargo test，预期可能红 | provider/model 缺失后不应留下 active running run；若失败，阶段 4 记录 bug，阶段 5 修。 |
| R-10 | 持久化 reload | `store.rs:355 PersistedState`、`:10204 append_message()`、`:11929 save_agent_run()`、`:12066 enqueue_agent_request()` | cargo test | reload 后 conversation/messages/run/queue/tool approvals 一致；pendingApproval 不丢；terminal run 不 active。 |
| F-07 | bootstrap reload | `store.ts:1490 bootstrap()`、`:1674 refreshChatData()` | Vitest + API mock | 刷新后 messages/runs/queue 收敛；stale backend 不覆盖 live pending。 |
| R-11 | queue 生命周期 | `store.rs:12066 enqueue`、`:12089 claim`、`:12133 complete`、`:12156 cancel` | cargo test | pending -> running -> completed/failed/canceled；running cancel 不被 complete 覆盖；补断言 pending canceled 后不可 claim。 |
| F-08 | 前端 queue/run | `store.ts:2556 handleAgentRunEvent()`、`App.tsx:698/724` | Vitest/Harness | run event 更新 `activeAgentRuns/agentRuns/agentQueue`；completed/failed/aborted 触发刷新。 |
| H-03 | 双对话路由 | `App.tsx:492 chat listener`、`store.ts:1843 upsertIncomingMessage()` | Harness | conversation A/B 同时 stream，事件只进入各自 conversation，不误用 active conversation。 |

## Vitest 范围

优先扩展：

- `src/lib/__tests__/storeMessageMerge.test.ts`：补 desktop `assistant_stream -> stale refresh -> assistant_message/final` 精确序列。
- 新增 `src/lib/__tests__/chatConversationChain.test.ts`：测 `sendMessage()`、agentId 三值、catch 副作用、`handleAgentRunEvent()`。
- 扩展 `src/lib/__tests__/personaAgentBinding.test.ts`：保留 helper 测试，同时补 store 发送层 fallback 行为。
- 扩展 `src/lib/__tests__/messageRenderUtils.test.ts`：thinking card 数据解析与 strip 正文。
- 扩展 `src/lib/__tests__/toolEventUtils.test.ts`：tool event cancel/rank/path/OAuth/managed process 基础。
- UI 环境准备后新增 `src/panels/chat/MessageList.test.tsx`，覆盖 `MessageList`、`ToolMessage`、`ThinkingCards`、`ManagedProcessMessage`。

## Rust cargo test 范围

- `src-tauri/src/agent/agent_loop.rs`：`run_chat_turn()` 普通聊天、stream/final id、provider error、approval pending、timeout。
- `src-tauri/src/agent/tool_dispatch.rs`：`execute_recovery_internal_tool()` 的 file/memory/tool_call bridge。
- `src-tauri/src/mcp.rs`：`list_tools()` / `call_tool()` success/error/timeout/OAuth/filter reject。
- `src-tauri/src/store.rs`：`append_message()`、`agent_runs()` timeout 副作用、`active_agent_run_for_conversation()`、`abort_agent_run()`、queue enqueue/claim/complete/cancel、tool approvals reload。

## Tauri/mock harness 范围

- `src/App.tsx` event harness：mock `@tauri-apps/api/event.listen`，捕获 `synthchat-chat-event`、`synthchat-agent-run-event`、`synthchat-agent-queue-event` handlers，用 fake timers 验证 60ms debounce、final flush、refresh scheduling。
- `src/panels/ChatExperience.tsx`：mock `api`，测 submit/stop，不连真实 Tauri。
- `src/panels/ToolPanels.tsx`：mock approval API 与 refresh 函数，测 pending approval 显示、approve/deny 后刷新。
- Rust event payload 如需真实 `AppHandle` 断言，再使用 Tauri test app；MVP 可先用 store 状态替代真实窗口事件。

## MVP 测试集

1. Vitest：`sendMessage()` 普通聊天，含 local user、`providerData.clientMessageId` cast 断言、有效/无效/fallback agentId。
2. Vitest：`assistant_stream -> stale refresh -> assistant_message` 同 id 合并，不重复、不回滚。
3. Vitest：`handleAgentRunEvent()` 更新 active runs、run list、queue terminal 状态。
4. Vitest Day 0：配置 jsdom 或等价环境，并写最小 `<div>test</div>` smoke。通过后再落 UI-01 到 UI-04。
5. cargo：`run_chat_turn()` fake/echo provider 成功，持久化 user/assistant/run completed。
6. cargo：queue pending/running/completed/failed/canceled；扩展现有 `agent_queue_pending_items_can_be_canceled()`，补 canceled 后 `claim_next_agent_request()` 不会取到它。
7. cargo：`read_file`、memory、MCP `call_tool` success/error。
8. cargo：provider error 后不留 active running run，标注“当前可能红，作为 bug 捕获”。

## 建议验证命令

```powershell
npm run test -- src/lib/__tests__/storeMessageMerge.test.ts src/lib/__tests__/personaAgentBinding.test.ts src/lib/__tests__/toolEventUtils.test.ts src/lib/__tests__/agentRunUtils.test.ts
npm run test -- src/lib/__tests__/chatConversationChain.test.ts
npm run test -- src/panels/chat/MessageList.test.tsx src/panels/ToolPanels.test.tsx
cd src-tauri; cargo test --lib active_run_timeout -- --test-threads=1
cd src-tauri; cargo test --lib agent_queue -- --test-threads=1
cd src-tauri; cargo test --lib mcp_call_tool -- --test-threads=1
cd src-tauri; cargo test --lib run_chat_turn -- --test-threads=1
```

## Risks

- `store.rs:11761 agent_runs()` 与 `:11843 active_agent_run_for_conversation()` 是读路径但会执行 timeout 恢复并 persist。相关 cargo 测试必须构造明确过期/未过期时间，避免本地与 CI 时间漂移。
- `App.tsx` 完整挂载成本高，listener 闭包依赖 React state/ref。阶段 4 可先做最小 test-only event driver 或 jsdom harness，避免大范围业务重构。
- `turn_finished ok=false` 前端无显式分支，H-01-b 可能暴露 ghost stream 或错误 assistant 行为。
- provider error 顺序风险明确：`agent_loop.rs:1515` running 已落盘，`:1528` 才校验 provider。
- UI 组件测试被当前 `node` 环境阻断，必须先解决测试基础设施。
- approval 入口集中在 `ToolPanels.tsx`，不是聊天面板；若产品要求聊天内审批，需要另列后续需求，阶段 4 先测现有路径。

## Recommendations

- 阶段 4 先落 MVP，不先修业务 bug；红测只用于记录 provider error / `ok=false` 风险。
- Rust 侧使用 fake provider、temp store、本地 oneShotJson/mock MCP，不依赖真实 provider 或外部 MCP。
- 前端先锁住 store 收敛不变量：local user 不丢、stream/final 不重复、stale refresh 不覆盖 live、queue/run terminal 状态一致。
- UI 可见性测试先覆盖组件输入输出，不做完整桌面 E2E。
- approval/deny/abort 先测状态机和 API/event 合并，再补真实用户交互。

## Test Backlog

P0：
- 普通聊天 `sendMessage()`。
- stream/final/stale refresh 合并。
- `turn_started` / `turn_finished ok=false` harness。
- agentId 三值选择。
- queue/run 前端状态合并。
- provider error 红测。
- UI jsdom smoke。

P1：
- `ToolPanels` approval approve/deny。
- tool_call bridge 到 internal/MCP。
- file/memory/MCP tool success/error/timeout。
- reload 后 pendingApproval、running timeout、closed tool message 一致性。
- 双 conversation 并发事件路由。

P2：
- 真实 Tauri `AppHandle` event payload 顺序。
- WeChat/pet/proactive source 差异。
- MCP OAuth reauth 完整 UI。
- managed process/browser tool 端到端显示。

## 下一阶段输入

- 允许新增测试文件；生产逻辑不先改。
- 阶段 4 Day 0：确认并配置 jsdom 或等价 React 测试环境，先跑 smoke。
- 阶段 4 Day 0：确认 `store.rs` timeout 测试隔离方式，所有 active run 测试使用明确时间数据。
- 前置读取：`App.tsx:492-648`、`ToolPanels.tsx:1292/1506/1608/2891`、现有 `storeMessageMerge/toolEventUtils/agentRunUtils/personaAgentBinding` 测试文件。
- 验收先限定 desktop conversation；wechat/pet/proactive 放 P2。
- 对 R-09 provider error 和 H-01-b `ok=false` 接受“先红后记录”，作为阶段 5 修复输入。