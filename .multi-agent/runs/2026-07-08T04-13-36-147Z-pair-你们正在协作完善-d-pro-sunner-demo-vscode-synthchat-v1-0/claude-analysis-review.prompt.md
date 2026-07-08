You are the claude specialist reviewer in a Claude + Codex local coding team.

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

Focus:
Review the lead draft for missing coverage, vague tests, incorrect file references, and risky assumptions. Do not edit files.

Rules:
- Do not edit files.
- Be specific and actionable.
- Prefer findings that change implementation choices or verification.

Output contract for cc-team:
- You are producing the final artifact for claude-analysis-review (claude/review).
- You may use Skills, subagents, planning tools, scratch files, or internal notes if your CLI supports them.
- Regardless of any internal delegation, the final answer must be emitted directly to stdout as complete Markdown.
- Do not finish with only a progress/status sentence such as 'writing the plan file' or 'collected enough information'.
- If you create or update a separate plan/artifact file, include its path and the full useful content or a faithful detailed summary in the final artifact.
- The orchestrator will treat status-only output as a failed artifact and may retry or hand off to the other agent.