You are the codex read-only stage synthesizer for a Claude + Codex local coding team.

Workspace: D:\pro_sunner\demo_vscode\SynthChat-V1.0.0

Task:
你们正在协作完善 D:\pro_sunner\demo_vscode\SynthChat-V1.0.0，这是一个 Tauri + React + Rust 的桌面 agent 应用。

当前阶段：阶段 3：对话链路测试设计。

模式意图：
- Pair read-only。
- Codex 先产出测试设计草案。
- Claude 只做复审。
- Codex 最终综合。
- 不改代码。

上下文约束：
- 不要全仓库递归阅读。
- 必须先读：
  - D:\pro_sunner\demo_vscode\SynthChat-V1.0.0\.multi-agent\runs\2026-07-08T03-25-56-657Z-consensus-你们正在协作完善-d-pro-sunner-demo-vscode-synthchat-v1-0\stage1-synthesis-codex.md
  - D:\pro_sunner\demo_vscode\SynthChat-V1.0.0\.multi-agent\runs\2026-07-08T03-57-08-402Z-consensus-你们正在协作完善-d-pro-sunner-demo-vscode-synthchat-v1-0\codex-plan.compact-retry.md
- 只在需要确认测试落点时 targeted read：
  - src/App.tsx
  - src/panels/ChatExperience.tsx
  - src/lib/store.ts
  - src/lib/api.ts
  - src/lib/__tests__/**
  - src-tauri/src/agent/agent_loop.rs
  - src-tauri/src/agent/tool_dispatch.rs
  - src-tauri/src/mcp.rs
  - src-tauri/src/store.rs

阶段目标：
为 SynthChat 设计真实可执行的 agent 对话链路测试方案，先不要修 bug。

必须覆盖：
1. 普通聊天：用户消息 -> assistant 回复。
2. 流式输出：assistant_stream / assistant_message 合并。
3. thinking/tool 展示：工具事件进入 UI。
4. agent 选择：persona / conversation / agentId 绑定。
5. 工具调用：至少 mock 文件工具、记忆工具、MCP 工具。
6. 审批/拒绝/取消：approval、abort、timeout。
7. 错误恢复：provider error、tool error、stream 中断。
8. 持久化：刷新后 conversation/messages/run 状态一致。
9. 队列：agent queue 入队、执行、完成、失败。
10. UI 可见性：MessageList、ToolMessage、ThinkingCards、ManagedProcessMessage。

产物：
- 测试用例清单。
- 哪些用 Vitest。
- 哪些用 Rust cargo test。
- 哪些需要 Tauri/mock harness。
- 最小可先通过的 MVP 测试集。
- 发现、风险、建议、待测清单、下一阶段输入。

验收：
- 必须引用具体文件路径和函数/模块。
- 不做大范围重写建议。
- 输出要能直接作为阶段 4 的输入。

Collaboration brief:
# codex-analysis-draft：阶段 3 对话链路测试设计草案

只读完成；未修改文件，未运行测试。以下可直接作为阶段 4 测试实施输入。

## Findings

- 主链路是 `ChatExperience.submit()` -> `store.sendMessage()` -> `api.sendChatMessage()` -> Rust `run_chat_turn()` -> Tauri events -> `App.tsx` listener -> `store.upsertIncomingMessage()/refreshChatData()` -> UI。
- 前端最关键风险点：
  - `src/App.tsx:289` `scheduleStreamMessageUpsert()` 对 `assistant_stream` 去抖，final 会立即 flush。
  - `src/App.tsx:492` 监听 `synthchat-chat-event`，在 `assistant_stream/assistant_message/turn_finished` 中决定插入和刷新。
  - `src/lib/store.ts:726` `mergeBackendMessagesWithLiveState()` 负责 stale backend snapshot 与 live stream 合并。
  - `src/lib/store.ts:1843` `upsertIncomingMessage()` 负责 `desktop-stream` 和 final 状态收敛。
  - `src/lib/store.ts:1994` `sendMessage()` 乐观插入 user message，并异步调用 `api.sendChatMessage()`。
- Rust 最关键风险点：
  - `src-tauri/src/agent/agent_loop.rs:1162` `run_chat_turn()` 保证 `turn_started/turn_finished`。
  - `src-tauri/src/agent/agent_loop.rs:335` `desktop_visible_stream_callback()` 产生 thinking/answer stream。
  - `src-tauri/src/agent/agent_loop.rs:2926-2934` final assistant 复用 streaming message id。
  - `src-tauri/src/agent/agent_loop.rs:3070` 发 `assistant_message`，`:3087` 发 final `assistant_stream`。
  - `src-tauri/src/store.rs:11761` `agent_runs()` 和 `:11843` `active_agent_run_for_conversation()` 读路径会执行 timeout 恢复并持久化。
- UI 可见性链路：
  - `src/panels/ChatExperience.tsx:2058` 渲染 `MessageList`。
  - `src/panels/chat/MessageList.tsx:41-84` 对 tool messages 去重、抑制 canceled。
  - `src/panels/chat/MessageRow.tsx:148-175` 分派 `ToolMessage` / `ManagedProcessMessage`。
  - `src/panels/chat/MessageRow.tsx:188` 渲染 `ThinkingCards`。

## 测试用例清单

| ID | 覆盖项 | 测试落点 | 类型 | 核心断言 |
|---|---|---|---|---|
| F-01 | 普通聊天 | `src/lib/store.ts:1994 sendMessage()` | Vitest | 本地 user 立即出现；`api.sendChatMessage()` 参数包含 `conversationId/personaId/agentId/content/providerData.clientMessageId`；后端 user 替换 local user；assistant 最终出现。 |
| R-01 | 普通聊天 | `src-tauri/src/agent/agent_loop.rs:1162 run_chat_turn()` | cargo test | echo/fake provider 下持久化 user + assistant；`AgentRunRecord` 从 running 到 completed；`store.messages()` 返回一致顺序。 |
| F-02 | 流式合并 | `src/lib/store.ts:1843 upsertIncomingMessage()`、`:726 mergeBackendMessagesWithLiveState()` | Vitest | `assistant_stream` 同 id 多次 upsert 后只有一条；stale refresh 不回滚 stream；final 后移除 `desktop-stream` presentation。 |
| H-01 | App 事件顺序 | `src/App.tsx:289`、`:492-648` | Vitest + Tauri mock harness | 依次 emit `turn_started -> assistant_stream -> stale refresh -> assistant_message -> turn_finished`，最终只有一个 assistant，processing 被清除。 |
| R-02 | Rust stream/final | `agent_loop.rs:335`、`:2926-2934`、`:3070-3096` | cargo test 或 Tauri mock app | stream 产生的 message id 被 final assistant 复用；final `assistant_stream` `isLast=true` 不造成重复。 |
| F-03 | thinking/tool 展示 | `messageRenderUtils.ts:251`、`MessageRow.tsx:188` | Vitest | providerData thinking cards 被拆成 thinking render item；assistant 正文不重复展示 thinking summary。 |
| UI-01 | MessageList | `src/panels/chat/MessageList.tsx:41-84` | Vitest component | running/completed tool event 同 key 只展示最高 rank；canceled tool event 被抑制。 |
| UI-02 | ToolMessage | `src/panels/chat/ToolMessage.tsx:22-163` | Vitest component | 文件工具显示 path badge；失败工具显示失败状态和 error；MCP reauth 信息显示 OAuth 状态。 |
| UI-03 | ThinkingCards | `src/panels/chat/ThinkingCards.tsx:5-44` | Vitest component | streaming card 默认展开并显示“思考中”；redacted card 显示“已隐藏”。 |
| UI-04 | ManagedProcessMessage | `src/panels/chat/ManagedProcessMessage.tsx:6-48` | Vitest component | process completed/stopped/watch_match 显示 label、exit code、command、cwd。 |
| F-04 | agent 选择 | `store.ts:2121-2135`、`api.ts:533 setConversationAgent()` | Vitest | 显式 `agentId` 优先；未知 agentId 被过滤为 null；conversation agent 作为 fallback。 |
| R-03 | persona/conversation/agent 绑定 | `agent_loop.rs:3818 resolve_chat_turn_persona_and_agent()`、`store.rs:9870 set_conversation_agent()` | cargo test | request.agentId 优先于 conversation.agent_id；persona 必须优先匹配 agent；切换 conversation agent 会同步 personaId。 |
| R-04 | 文件工具 | `tool_dispatch.rs:1425 execute_recovery_internal_tool()`、`:1600 read_file` | cargo test | mock temp workspace 中 `read_file` 成功产生 `ToolEvent{server_id="__internal", tool_name="read_file", ok=true}`；缺失文件产生 failed event/observation。 |
| R-05 | 记忆工具 | `tool_dispatch.rs:1701-1704 recall_memory/remember_fact/manage_memory/memory` | cargo test | mock memory 写入、检索、失败路径均产生可展示 ToolEvent/observation，并绑定 run_id。 |
| R-06 | MCP 工具 | `src-tauri/src/mcp.rs:2605 list_tools()`、`:2681 call_tool()` | cargo test | mock one-shot/HTTP MCP 成功、tool error、timeout、filter reject；`ToolTraceEntry` 持久化且 error 被净化。 |
| R-07 | 审批 pending | `agent_loop.rs:2250-2331`、`store.rs:13181 append_tool_approval()` | cargo test | risky tool 进入 `pendingApproval`；生成 approval request；assistant 消息为“工具调用正在等待审批”。 |
| H-02 | approve/deny | `api.ts:1146-1149`、`App.tsx:698` | Tauri/mock harness | approve 后 run 继续或完成；deny 后 run failed/aborted，queue 状态和 tool message 同步。 |
| F-05 | abort | `ChatExperience.tsx:1787 stopActiveRun()`、`api.ts:856 abortAgentRun()` | Vitest + API mock | stop 调用 abort，清除 processing，刷新 runs/queue/messages。 |
| R-08 | abort/timeout | `store.rs:12000 abort_agent_run()`、`agent_loop.rs:4236 check_agent_run_interrupted()` | cargo test | abort 级联 child runs；timeout 写 checkpoint、assistant error message、关闭 running tool event。 |
| F-06 | provider/tool/stream error recovery | `store.ts:2226-2254`、`App.tsx:530-554` | Vitest/Harness | provider reject 不插入临时错误气泡；processing 被清理；tool failed 可见；stream 中断后 turn_finished ok=false 不留下 ghost stream。 |
| R-09 | provider error | `agent_loop.rs:1515 save_agent_run()`、`:1528 selected_provider_id()` | cargo test | provider/model 缺失后 run 不应长期 running；应 terminal failed 或无 active run。若失败，阶段 4 记录为 bug。 |
| R-10 | 持久化 reload | `store.rs:355 PersistedState`、`:10204 append_message()`、`:11929 save_agent_run()`、`:12066 enqueue_agent_request()` | cargo test | reload 后 conversation/messages/run/queue/tool approvals 一致；pendingApproval 不丢；terminal run 不重新 active。 |
| F-07 | bootstrap reload | `store.ts:1490 bootstrap()`、`:1674 refreshChatData()` | Vitest + API mock | 刷新后 messages 与 runs/queue 收敛；stale backend 不覆盖 live pending。 |
| R-11 | 队列生命周期 | `store.rs:12066 enqueue_agent_request()`、`:12089 claim_next_agent_request()`、`:12133 complete_agent_queue_item()` | cargo test | pending -> running -> completed/failed；running cancel 不被 complete 覆盖。 |
| F-08 | 前端队列状态 | `store.ts:2556 handleAgentRunEvent()`、`App.tsx:724 synthchat-agent-queue-event` | Vitest/Harness | run event 更新 `activeAgentRuns/agentRuns/agentQueue`；failed/completed 触发刷新。 |

## 哪些用 Vitest

- 扩展 `src/lib/__tests__/storeMessageMerge.test.ts`：stream/final/stale refresh 合并。
- 新增或扩展 store action 测试：`sendMessage()`、`upsertIncomingMessage()`、`refreshChatData()`、`handleAgentRunEvent()`。
- 扩展 `src/lib/__tests__/personaAgentBinding.test.ts`：agent fallback 与无效 agent。
- 扩展 `src/lib/__tests__/toolEventUtils.test.ts`、`messageRenderUtils.test.ts`：tool/thinking/managed process 可见性基础。
- 新增组件测试：`src/panels/chat/MessageList.test.tsx`，覆盖 `MessageList`、`ToolMessage`、`ThinkingCards`、`ManagedProcessMessage`。

## 哪些用 Rust cargo test

- `src-tauri/src/agent/agent_loop.rs`：`run_chat_turn()` 普通聊天、stream/final id、provider error、approval pending、timeout。
- `src-tauri/src/agent/tool_dispatch.rs`：`execute_recovery_internal_tool()` 的 file/memory/tool_call bridge。
- `src-tauri/src/mcp.rs`：`list_tools()` / `call_tool()` 的 mock MCP success/error/timeout/OAuth retry/filter reject。
- `src-tauri/src/store.rs`：`append_message()`、`agent_runs()`、`active_agent_run_for_conversation()`、`abort_agent_run()`、`enqueue/claim/complete/cancel queue`、`tool_approvals()` reload。

## 哪些需要 Tauri/mock harness

- `src/App.tsx` 事件顺序：mock `@tauri-apps/api/event.listen`，捕获 `synthchat-chat-event`、`synthchat-agent-run-event`、`synthchat-agent-queue-event` handler，用 fake timers 验证去抖和刷新。
- `src/panels/ChatExperience.tsx` submit/stop：mock `api`，不连真实 Tauri。
- Rust event emission：如需断言 `turn_started/assistant_stream/assistant_message/turn_finished` 的真实 Tauri payload，使用 Tauri test app/mock `AppHandle`；MVP 可先用 store 状态替代事件断言。

## 最小 MVP 测试集

1. Vitest：`sendMessage()` 普通聊天 + agentId 参数 + local user 替换。
2. Vitest：`assistant_stream -> stale refresh -> assistant_message` 同 id 合并，不重复。
3. Vitest：`handleAgentRunEvent()` 更新 queue running/completed/failed。
4. Vitest component：一组 messages 同时渲染 `MessageList`、`ToolMessage`、`ThinkingCards`、`ManagedProcessMessage`。
5. cargo：`run_chat_turn()` echo/fake provider 成功，持久化 user/assistant/run completed。
6. cargo：`store.rs` queue pending/running/completed/failed/canceled。
7. cargo：`tool_dispatch.rs` mock `read_file`、memory、MCP `call_tool` success/error。
8. cargo：provider error 或 timeout 后不留下 active running run。

## 建议验证命令

```powershell
npm run test -- src/lib/__tests__/storeMessageMerge.test.ts src/lib/__tests__/personaAgentBinding.test.ts src/lib/__tests__/toolEventUtils.test.ts src/lib/__tests__/agentRunUtils.test.ts
npm run test -- src/lib/__tests__/chatConversationChain.test.ts src/panels/chat/MessageList.test.tsx
cd src-tauri; cargo test --lib active_run_timeout -- --test-threads=1
cd src-tauri; cargo test --lib agent_queue -- --test-threads=1
cd src-tauri; cargo test --lib mcp_call_tool -- --test-threads=1
cd src-tauri; cargo test --lib run_chat_turn -- --test-threads=1
```

## Risks

- `agent_loop.rs:1515` 先保存 running run，再在 `:1528` 校验 provider；provider error 测试可能揭示 run 未 terminal 的现有 bug。
- `App.tsx` 组件级 harness 可能较重；如 mount `App` 成本过高，阶段 4 可先抽 test-only event driver，但不要重写业务链路。
- `store.rs:11761` 读 run 会触发 timeout 副作用，测试必须固定时间或构造明确过期/未过期数据。
- UI component 测试若当前 Vitest 未启用 jsdom，需要阶段 4 明确测试环境配置。

## Recommendations

- 阶段 4 先落 MVP，优先锁住 stream/final 合并、普通聊天、agent 绑定、queue/run 状态。
- Rust 侧优先用 fake/echo provider 和 temp store，不先接真实 provider。
- MCP 用本地 mock server 或 `oneShotJson`，不要依赖外部 MCP。
- approval/deny/abort 先测状态机和 API/event 合并，再补真实 UI 操作测试。

## Test Backlog

- P0：普通聊天、stream/final、agentId、queue、provider error、UI 四组件可见性。
- P1：approval approve/deny replay、tool_call bridge 到 internal/MCP、memory provider error。
- P1：reload 后 pendingApproval/running timeout/closed tool message 一致性。
- P2：真实 Tauri AppHandle event payload 顺序、wechat/pet/proactive 来源差异、MCP OAuth reauth 完整 UI。

## 下一阶段输入

- 允许新增测试文件，不改生产逻辑。
- 允许使用 fake LLM/fake provider、temp workspace、本地 mock MCP server。
- 先以 desktop conversation 为验收范围；wechat/pet/proactive 放 P2。
- 对 provider error 测试接受“先红后修”，因为当前代码顺序存在高风险。

Peer read-only review:
Here is my complete review artifact.

---

# claude-analysis-review —阶段 3 测试设计草案复审

**总体判定：草案质量高，行号精度优秀，但有3 处错误引用、5 处覆盖空白、3 处断言不充分、1 处硬阻断须在阶段 4 第一步解决。** 以下所有发现均直接影响测试选址或实现选择，按严重性排序。

---

## 一、引用准确性核查

### 1.1 确认准确（全部精确）

以下 24 处引用已逐一核对，行号全部精确：

`App.tsx:289scheduleStreamMessageUpsert` ✓ · `App.tsx:492 synthchat-chat-event listener` ✓ · `App.tsx:724 synthchat-agent-queue-event` ✓ · `store.ts:726 mergeBackendMessagesWithLiveState` ✓ · `store.ts:1843 upsertIncomingMessage` ✓ · `store.ts:1994 sendMessage` ✓ · `store.ts:1490 bootstrap` ✓ · `store.ts:1674 refreshChatData` ✓ · `store.ts:2556 handleAgentRunEvent` ✓ · `api.ts:533 setConversationAgent` ✓ · `api.ts:856 abortAgentRun` ✓ · `ChatExperience.tsx:1787 stopActiveRun` ✓ · `ChatExperience.tsx:2058 MessageList render` ✓ · 全部 `agent_loop.rs`/`tool_dispatch.rs`/`mcp.rs`/`store.rs` Rust 函数 ✓

### 1.2 错误或误导引用（须在阶段 4 修正）

**[REF-01] `App.tsx:530-554` — 描述为"stream interruption handling"，实际是 `turn_finished` 正常完成处理**

核查结果：530行 gates on `payload.type === "turn_finished"`，531 行清除processing indicator，532-546 是 WeChat 路径，547-553 是非 WeChat 路径的 final upsert，554 行 schedule refresh。整段是正常对话轮次完成逻辑，不是stream 中断处理。

影响：F-06 的测试锚点错误。"stream interrupted → turn_finished ok=false 不留ghost stream"的逻辑在哪里？**需要在阶段 4 实施前先读取 App.tsx:492-648 的 `turn_finished` 分支，确认 `ok=false` 是否有独立分支，以及 stream 异常中止的实际处理位置。** 若 `ok=false` 未做独立处理，F-06 本身就是一个待发现的 bug 用例。

**[REF-02] `messageRenderUtils.ts:251` — 描述为"thinking card rendering"，实际是数据提取包装函数**

核查结果：第 251 行是 `export function messageThinkingCards(message: ChatMessage)` — 一个单行thin wrapper，调用 `thinkingCardsFromProviderData(message.providerData)`。实际的 `ThinkingCard` 对象构造逻辑在 248-249 行的 `thinkingCardsFromProviderData`。渲染本身在 `ThinkingCards.tsx`。

影响：F-03 测试应锚定到 `thinkingCardsFromProviderData`（测试输入→输出 card对象），而非笼统引用 251 行。描述应改为"thinking card 数据解析"。

**[REF-03] `store.ts:2226-2254` — 描述为"provider error handling"，实际是通用send 失败 catch 块，且代码注释明确说明 transient 错误被故意抑制**

核查结果：catch 块里的代码注释明确写明 transient transport errors 不投入chat timeline，这是设计行为，不是遗漏。此外它在 2228-2237 调用 refreshAgentQueue 和 refreshRuns，这才是可验证的副作用。

影响：F-06 的断言"provider reject 不插入临时错误气泡"是对的，但原因是代码 **设计如此**，测试应标注这是回归防护，而非功能验证。更重要的是：**此catch 块缺少测试的关键副作用是refreshAgentQueue + refreshRuns 被调用**，草案没有测试这个。

**[REF-04] `agent_loop.rs:1515-1528` 描述范围偏早**

1515 行是 `store.save_agent_run()`（此时 run 已是 running状态），1528 行才是 `selected_provider_id(...)` 的验证。这个时序是 R-09 风险的核心：run 以 `running` 状态落盘，之后 provider 校验失败，若没有后续 terminal 写入，run 永远是 running。草案将 1515 标注为"provider校验"是误导，应改为"run 在 provider 校验前即被持久化为 running"，这才是需要测试覆盖的顺序风险。

**[REF-05] `providerData.clientMessageId` 无类型保证**

`SendChatRequest.providerData` 类型是 `unknown | null`（`types.ts:1467`），没有 `clientMessageId` 字段的类型定义。F-01 的断言"参数包含 `providerData.clientMessageId`"需要改为在测试中用 `(params.providerData as any).clientMessageId`，并注释这是 runtime convention，非typed contract。若草案将此视为强类型断言，测试会在类型严格检查下通不过。

---

## 二、覆盖空白（须在阶段 4 新增）

### [GAP-01] `turn_finished` ok=false 分支 — F-06 当前未真正覆盖

App.tsx:530-554 处理 `turn_finished`，但草案没有验证 `ok=false` 和 `ok=true` 的实际行为差异。

**新增测试：**
- H-01-b（Harness）：emit `turn_finished { ok: false }`，断言：(a) processing被清除；(b) 不留 `desktop-stream` presentation 的 ghost message；(c) 不触发乐观 assistant 插入。对比 `ok=true` 路径。
- 前提：先读 App.tsx:492-648 确认 `ok=false` 是否有独立分支，若无则这本身是 bug，R-09 扩展测试可同时覆盖。

### [GAP-02] Approval事件发射链路 — R-07 与 H-02 之间有一段断裂

R-07 测试 Rust侧 risky tool 进入 `pendingApproval`，H-02 测试前端 approve/deny。但两者之间缺少：Rust 如何通知前端"有工具等待审批"？这是哪个 Tauri 事件？App.tsx 中的哪个 listener处理它？

**新增测试：**
- H-02-pre（Harness）：触发 risky tool，断言 App.tsx 收到一个包含 `pendingApproval` 信息的 event，且该 event 触发了前端 approval dialog渲染（或相应 store 状态变更）。需要先确认实际 event name和 App.tsx handler 位置。

### [GAP-03] Queue `cancel` for pending items — R-11 漏测

R-11 覆盖了 `pending → running → completed/failed`，以及"running cancel不被complete覆盖"。但没有测试 **pending状态的队列项被取消**（从未被 claim 就取消）。

`complete_agent_queue_item(id, "canceled", error)` 应该是合法调用路径，且 pending 项被取消后队列的 `claim_next_agent_request` 不应该重新激活它。

**新增测试（cargo）：** `test_queue_cancel_pending_item_not_claimable` — enqueue → cancel (status="canceled") → assert claim_next returns None for that id。

### [GAP-04] `sendMessage` catch 块副作用 — F-06漏测

store.ts:2228-2237 的 `refreshAgentQueue` 和 `refreshRuns` 调用是send 失败的核心副作用，确保前端状态与后端收敛。草案的F-06 只验证"不插错误气泡"，没有验证这两个刷新被触发。

**新增断言至 F-06：** mock `api.sendChatMessage` reject，assert `store.refreshAgentQueue()` 和 `store.refreshRuns()` 被调用各至少一次。

### [GAP-05] `turn_started` 前端处理 — 整个草案未覆盖

`turn_started` 是对话轮次的第一个事件，前端应在此时设置 conversation processing状态（这是 processing indicator 显示的前提）。没有任何测试覆盖此事件的处理。

**新增测试至 H-01 或独立 H-01-c：** emit `turn_started`，断言 `processingConversationIds` 包含该conversationId，UI 显示 processing state。

---

## 三、断言不充分（现有用例需加精度）

### [ASSERT-01] F-02 "stale refresh不回滚 stream" — 缺少精确序列和断言对象

当前描述过于笼统。需要明确测试序列：

```
1. emit turn_started
2. emit assistant_stream { id: "msg-1", delta: "hello", isLast: false }
→ store有一条 desktop-stream presentation message
3. 调用 refreshChatData()（模拟 stale backend snapshot，不包含 msg-1）
→断言：store中 msg-1 的 presentation type仍是 desktop-stream，未被删除
4. emit assistant_message { id: "msg-1", final: true }
→ 断言：desktop-stream presentation 被移除，msg-1 转为普通 assistant message
```

测试锚点：`store.ts:726mergeBackendMessagesWithLiveState` 的 `keepLiveStreamIds` 集合逻辑。

### [ASSERT-02] R-09 "provider error 后不留 active run" — 须明确标注"预期红"

由于 `agent_loop.rs:1515` 先持久化 running run、1528 再校验 provider，当前代码可能确实留下永远 running 的 run。R-09 应标注：

> **预期状态：此测试在当前代码可能失败（验证 bug 存在）。阶段 4 允许红，记录为 bug，阶段 5 修复。**

若测试意外通过（run 已有terminal路径），则说明代码已处理，仍有价值。但不应让阶段 4 实施者以为这是绿测试。

### [ASSERT-03] R-08 "abort cascade 断言不完整"

"abort 级联 child runs"需要两个独立断言：
- 断言 1：所有 child `AgentRunRecord` 状态为 `failed`或 `aborted`
- 断言 2：parent `AgentRunRecord` 状态为 `failed` 或 `aborted`
- 断言 3：`active_agent_run_for_conversation()` 返回 None

当前草案只说"级联"，未说明最终状态。

### [ASSERT-04] R-02 "final assistant 复用 streaming message id" — 断言需要反面验证

正面断言"final ChatMessage 的 id = streaming message id"是对的。但还需要：

- **反面断言：** store 中该 conversationId 下没有第二条 id 不同的 assistant message
- 因为 `agent_loop.rs:3087` emit final `assistant_stream { isLast: true }` 可能触发 `upsertIncomingMessage` 重复处理路径

### [ASSERT-05] F-04 `agentId` 三值区分

F-04 测试"显式 agentId 优先，未知 agentId 被过滤为 null"，但 `store.ts:2123-2126` 的 resolution逻辑需要区分三种输入：

| 输入 | 预期 `agentIdForSend` |
|---|---|
| 有效 agentId（在 `state.agents` 中存在） | 该 agentId |
| 无效 agentId（不在 `state.agents` 中） | `null` |
| `null` / `undefined` → fallback 到 conversation.agent_id（有效） | conversation.agent_id |

当前草案只测了前两种，第三种（fallback 路径）缺失。

---

## 四、未标注的风险

### [RISK-01] `store.rs:11761agent_runs()` 读取副作用影响所有 cargo 测试

草案 Risk章节提到了这一点，但没有将它升格为**测试基础设施要求**：所有调用 `agent_runs()` 或 `active_agent_run_for_conversation()` 的 cargo 测试（R-01、R-08、R-09、R-10、R-11 全部受影响）必须使用固定时间戳构造数据，或在 store 初始化时注入 mock clock。不解决这个问题，测试在 CI 与本地的行为不同。

**阶段 4 前提：** 确认 `src-tauri/src/store.rs` 中 timeout 副作用是否可通过注入 `Instant` 或 config 参数来控制；若不能，需要先做一次最小化 refactor 暴露可测接口（这是阶段 4 唯一允许的生产代码小改动，因为不修则无法隔离测试）。

### [RISK-02] App.tsx Harness 挂载成本远高于预估

H-01 和 H-02 需要 mock `@tauri-apps/api/event.listen`。但 App.tsx 的 event handler闭包会捕获 React state dispatcher（来自多个 `useState`/`useRef` 调用）。简单 mock `listen` 返回一个假unlisten 函数不够——handler 需要在有效 React context 中执行才能触发 re-render。

**建议：** 阶段 4 不挂载完整 `<App />`，而是抽取 event handler逻辑（App.tsx:492-648 的 listener body）为可独立测试的纯函数，接受 store action dispatch 作为参数。这是允许的最小重构（只改测试边界暴露，不改业务逻辑）。

### [RISK-03] 多对话并发事件路由 — 草案未识别

`App.tsx:492` 的 `synthchat-chat-event` listener 通过 `payload.conversationId` 路由事件。若两个对话同时活跃，来自对话 A 的 `assistant_stream` 事件理论上可以被误路由到对话 B（若 listener 内部 store state 读取了错误的 active conversation）。

无论实现是否有bug，这个场景当前完全没有测试覆盖，属于高概率潜在 bug 区域。**建议在R-01 或 F-01 中至少添加一个双对话并发场景的Vitest 测试。**

---

## 五、硬阻断：UI 组件测试环境

**[BLOCKER] `src/lib/__tests__/` 下全部是 `.test.ts` 文件（无 `.test.tsx`），无 React 组件测试。**

现有 11 个测试文件均为纯逻辑测试。UI-01 到 UI-04（MessageList、ToolMessage、ThinkingCards、ManagedProcessMessage）需要 React Testing Library + jsdom/happy-dom 的Vitest 环境。

**阶段 4 第一步（必须在写任何 UI 测试前完成）：**
1. 确认 `vitest.config.ts` 的 `environment` 设置（当前可能是 `node`）
2. 安装 `@vitest/browser` 或配置 `jsdom` + `@testing-library/react`
3. 用一个最小 smoke 测试（render `<div>test</div>`）验证环境可工作
4. 之后才能写 UI-01 到 UI-04

MVP 测试集第4 项（UI 四组件渲染）应移到解决此BLOCKER 之后。

---

## 六、现有测试文件未充分利用

阶段 4 实施前须先读取以下文件，避免重复：

| 文件 | 草案假设 | 实际应核查 |
|---|---|---|
| `storeMessageMerge.test.ts` | 草案说"扩展" | 先读：stream/final 合并已测到什么深度？ |
| `toolEventUtils.test.ts` | 草案说"扩展" | 先读：cancel/suppress 是否已有用例？ |
| `agentRunUtils.test.ts` | 草案说"扩展" | 先读：queue lifecycle 是否已部分覆盖？ |
| `personaAgentBinding.test.ts` | 草案说"扩展" | 先读：三值agentId 是否已测？ |

---

## 七、MVP 测试集调整建议

草案 MVP 8 项中，4 项无需调整，4 项需要修改：

| 原MVP 项 | 状态 | 调整 |
|---|---|---|
| 1. Vitest: `sendMessage()` 普通聊天 | ✓ | 补加 `providerData` 为`unknown`的 cast断言 |
| 2. Vitest: stream/final 合并 | ✓ | 补加 [ASSERT-01] 精确序列 |
| 3. Vitest: `handleAgentRunEvent()` | ✓ | 无需修改 |
| 4. Vitest component: UI 四组件 | **阻断** | 移至BLOCKER 解决后；MVP 改为"验证 jsdom 环境可用" |
| 5. cargo: `run_chat_turn()` | ✓ | 须处理 [RISK-01] clock issue |
| 6. cargo: queue全生命周期 | ✓ | 补加 pending cancel 场景（[GAP-03]） |
| 7. cargo: `read_file`/memory/MCP | ✓ | 须先确认 workspace 路径 resolution 机制 |
| 8. cargo: provider error后无active run | **标红** | 标注"预期失败"（[ASSERT-02]），记录为 bug |

---

## 八、阶段 4 输入补充清单

以下内容须在阶段 3 产物中追加，作为阶段 4 实施约束：

1. **前置读取**：`App.tsx:492-648` 完整 event switch，定位 `turn_finished ok=false` 分支和 approval request event名称
2. **前置读取**：`agent_loop.rs` 中触发 approval request 的 event emission位置（非 `append_tool_approval`，是Tauri emit）
3. **前置读取**：现有 4 个 `__tests__` 文件内容（storeMessageMerge、toolEventUtils、agentRunUtils、personaAgentBinding）
4. **基础设施任务（阶段 4 Day 0）**：解决 Vitest jsdom BLOCKER，写最小 smoke test验证
5. **基础设施任务（阶段 4 Day 0）**：确认 `store.rs:11761` timeout 副作用隔离方案
6. **R-09 标注**：此 cargo 测试允许红，记录为已知 bug，阶段 5 修复
7. **新增用例（须加入测试清单）**：H-01-c (turn_started)、H

Rules:
- Do not edit files.
- Produce the requested stage artifact, not an implementation patch.
- Include concrete file paths, functions, modules, test targets, and verification commands when applicable.
- Separate findings, risks, recommendations, test backlog, and input for the next stage.

Output contract for cc-team:
- You are producing the final artifact for codex-analysis-synthesis (codex/plan).
- You may use Skills, subagents, planning tools, scratch files, or internal notes if your CLI supports them.
- Regardless of any internal delegation, the final answer must be emitted through the final CLI response captured by the configured output artifact as complete Markdown.
- Do not finish with only a progress/status sentence such as 'writing the plan file' or 'collected enough information'.
- If you create or update a separate plan/artifact file, include its path and the full useful content or a faithful detailed summary in the final artifact.
- The orchestrator will treat status-only output as a failed artifact and may retry or hand off to the other agent.