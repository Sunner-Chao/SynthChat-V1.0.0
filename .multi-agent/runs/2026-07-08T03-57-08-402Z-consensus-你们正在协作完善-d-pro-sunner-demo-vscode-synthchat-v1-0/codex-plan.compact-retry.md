# SynthChat 可吸收能力矩阵

## 1. 失败感知说明

Codex 之前失败的直接原因是原始提示超过上下文窗口。本阶段产物改用压缩后的 official source notes，并结合 Stage 1 综合产物与 Claude 输出，不再重复做广泛 web 搜索，也不重新扫全仓库。

## 2. Findings

- SynthChat 已不是简单 Mock 前端，而是 `React/Zustand -> Tauri invoke/event -> Rust agent runtime -> LLM/MCP/tools/store -> streaming UI` 的完整链路。
- 关键路径是：`ChatExperience.tsx:1740 submit()` -> `store.ts:1994 sendMessage()` -> `api.ts:554 sendChatMessage()` -> `lib.rs:2446 send_chat_message()` -> `agent_loop.rs:1162 run_chat_turn()` -> `App.tsx:492/698` 事件消费 -> `store.ts:1843 upsertIncomingMessage()` / `store.ts:1674 refreshChatData()`。
- 外部项目可吸收的重点不是“再造 agent 框架”，而是增强这条链路里的选择、审批、检查点、事件、持久化和恢复不变量。
- SynthChat 已有大量类似能力：`workflow_graph.rs` checkpoint/human gate/delegate，`tool_dispatch.rs` internal/MCP bridge，`mcp.rs` OAuth/circuit/keepalive，`skills.rs:1621 prompt_blocks_for_request()`，`store.rs:11761 agent_runs()`。

## 3. Capability Matrix

| Reference object | Core mechanism | SynthChat 是否已有类似实现 | Minimal feature to borrow now | Complex feature to avoid now | Dialogue-chain benefit | Test method | Priority |
|---|---|---|---|---|---|---|---|
| Claude Code | Subagents 独立上下文、独立权限、hooks、memory、session resume | 部分已有：`agent/delegation.rs`、`delegation_acp.rs`、`workflow_graph.rs:51-53` delegate events、`shell_hooks.rs`、`approval_gateway.rs` | 给 delegation subagent 强制默认 `IterationBudget` 与结构化 lifecycle event，锚点是 `agent_loop.rs:1308 run_chat_turn_with_toolset_policy_and_iteration_limit()` 和 `runtime_events.rs:207` | 容器级子代理隔离、完整插件市场、复杂 hooks UI | 避免子代理循环卡住父 run，保证 `processingConversationIds` 能从 `turn_started` 收敛到 `turn_finished` | Rust fake LLM 返回循环 tool call，断言 subagent 到达 budget 后写入 failed/completed run event | P1 |
| OpenAI Codex | OS sandbox + approval policy；read-only review；workspace-write；`AGENTS.md` 项目指令 | 已有：`tool_policy.rs`、`approval_gateway.rs`、`store.rs:13173 tool_approvals()`、`store.rs:13181 append_tool_approval()`、`file_tools.rs` 安全写入 | 区分 `ApprovalScope::Once / Session / Persistent`，session scope 写入当前 `AgentRunRecord`，persistent 继续进入 `store.rs`；前端审批 UI 在 `ChatExperience.tsx` 明示作用域 | 完整规则引擎、路径 pattern policy editor、OS sandbox 重构 | 减少重复审批，同时避免一次性授权污染后续 run；强化 user message -> tool call -> approval -> replay -> persistence | `test_session_approval_does_not_persist`：同一 run 自动放行，下一 run 重新要求审批 | P0 |
| Windsurf/Cascade | Chat/Write 模式；global/workspace/conversation scoped memories/rules | 部分已有：`AgentDefinition` toolset、`ChatConfig.auto_approve_mode`、`skills.rs:1621` 最多 6 个 skill prompt、`memory_manager.rs` | 增加显式 `AgentMode::ChatOnly | Agent`：ChatOnly 在 `agent_loop.rs:1308` 跳过 tool registry/workflow，只走 `llm.rs:152 complete_chat_with_options()` 与 streaming callback | mid-turn 动态模式切换、完整 IDE clone、file-pattern rules 引擎 | 简单问答不进入工具路径，降低延迟和无工具场景的 run 卡死概率 | `test_chat_only_mode_skips_tool_init`；前端验证 agent 选择后 `sendMessage()` 仍正常流式展示 | P0 |
| LangGraph | Checkpointer 持久化 thread state；interrupt 暂停、JSON payload、resume input | 已有：`workflow_graph.rs:573 WorkflowCheckpointNode`、`:1020 checkpoint_waiting()`、`:1428 resume_requested_from_current()`、human gate schema、`store.rs:11761 agent_runs()` | 把每次 LLM/tool 前后的 compact checkpoint 摘要写入 `AgentRunRecord`，并在 `refreshChatData()` 后可恢复 UI 状态 | Time travel UI、任意图可视化编辑器、完整 LangGraph runtime 引入 | 提升 crash/reload 后的 error recovery：stream/final/tool event 不靠内存状态才能恢复 | 模拟 tool 前崩溃，重载 store 后断言 run 状态、checkpoint summary、UI processing 状态一致 | P0 |
| AutoGen | Conversable agents；role/task/message orchestration；human feedback | 已有：`delegation*.rs`、`acp_server.rs`、`workflow_graph.rs` delegate lifecycle、`tool_dispatch.rs:1460` tool_call bridge | 统一 delegate task envelope：`role`、`task`、`expected_output`、`parent_run_id`、`child_run_id`，落到 agent-run event 与 `store.rs:11942` run upsert | 引入 AutoGen runtime、自由群聊式 agent swarm、复杂 speaker selection | 子代理输出更容易被 reviewer 汇总，UI 能把父子 run 关系稳定展示 | Rust 测 `delegate_task_envelope_roundtrip`；前端测 `handleAgentRunEvent()` 合并父子 run | P1 |
| CrewAI | Crew/Task/Flow；guardrails；memory/knowledge；observability traces | 部分已有：`workflow_graph.rs` completion gate、`context_compression.rs`、`memory.rs`、`agentRunUtils.test.ts` | 给 reviewer/guardrail retry 增加结构化 `guardrail_result` event：包含 reason、retry_count、final_action，走 `runtime_events.rs` 到 `App.tsx:698` | 完整 Crew/Flow DSL、外部 observability 平台、复杂 task marketplace | 让 final answer 被拒绝、重试、降级时在 UI 和持久化中可解释，减少“看似卡住” | fake LLM 输出 raw tool-looking final answer，断言 `WorkflowPlannerRoute::ReviewFinal` 前后产生 guardrail event | P1 |
| OpenHands | Explicit runtime/sandbox；命令、文件、server 都在可追踪执行环境中运行 | 部分已有：`execution.rs` terminal/process、`browser_tools.rs`、`file_tools.rs`、`mcp.rs:2681 call_tool()`、approval | 在每个 tool event 增加 `execution_environment` 字段：`local_workspace`、`managed_process`、`browser_session`、`mcp_server:{id}`；由 `tool_dispatch.rs:1425 execute_recovery_internal_tool()` 统一填充 | Docker/remote sandbox 立即落地、任意代码执行环境重构 | UI 和 store 能解释工具在哪里运行，便于失败恢复、审批判断和审计 | `toolEventUtils.test.ts` 增加 environment label；Rust 测 internal/MCP/browser tool event 均带 environment | P1 |

## 4. Risks

- `App.tsx:289 scheduleStreamMessageUpsert()` 与 final event 顺序错，会造成 streaming 覆盖 final 或重复 assistant 消息。
- `store.ts:726 mergeBackendMessagesWithLiveState()` 是 UI/stream/store 收敛核心，stale backend snapshot 仍可能回滚 live state。
- `agent_loop.rs:1308` 参数过多，`toolset_policy`、`delegation_policy`、`iteration_budget` 漏传会静默改变能力面。
- approval pending/replay/deny 与 queue/run 绑定复杂，`approval_gateway.rs` 和 `store.rs:13181` 需要更清晰作用域。
- MCP 已有 OAuth/circuit/keepalive，但 `mcp.rs:2681 call_tool()` 的 reauth、retry、circuit 状态需要更强 UI 显示与恢复测试。
- `store.rs:11761 agent_runs()` 读路径有 timeout 恢复副作用，轮询可能影响 run 状态判断。

## 5. Recommendations

1. P0 先做两件事：Codex-style approval scope、Windsurf-style ChatOnly mode。
2. P0 同步补 LangGraph-style checkpoint 恢复测试，不急着引入新 runtime。
3. P1 再做 delegate envelope、guardrail event、tool execution environment 三个结构化事件增强。
4. P2 才考虑 Claude hooks 阻断、AGENTS.md 多级合并、memory relevance threshold 等扩展项。
5. 所有改动都应围绕同一验收链路：`sendMessage()` -> LLM/tool -> streaming event -> UI display -> `append_message()` -> refresh/error recovery。

## 6. Test Backlog

- `src/lib/__tests__/storeMessageMerge.test.ts`：补 `assistant_stream -> stale refresh -> final -> refresh` 顺序。
- `src/lib/__tests__/toolEventUtils.test.ts`：补 `execution_environment` 与 approval/retry/cancel 展示。
- `src/lib/__tests__/agentRunUtils.test.ts`：补 parent/child delegate run、guardrail retry、checkpoint waiting/resumed。
- Rust：`test_session_approval_does_not_persist`。
- Rust：`test_chat_only_mode_skips_tool_init`。
- Rust：`test_checkpoint_survives_reload_before_tool_result`。
- Rust：`test_delegate_task_envelope_roundtrip`。
- Rust：`test_guardrail_retry_event_is_persisted`。

## 7. Next-stage Input For Stage 3

Stage 3 应直接进入 P0 实施设计，输入如下：

- 目标 1：实现 `ApprovalScope::Once / Session / Persistent`，锚定 `tool_policy.rs`、`approval_gateway.rs`、`store.rs:13173`、`ChatExperience.tsx`。
- 目标 2：实现 `AgentMode::ChatOnly | Agent`，锚定 `agent_loop.rs:1308`、`llm.rs:152`、`store.ts:1994`、`App.tsx:492`。
- 目标 3：定义 checkpoint 恢复不变量，锚定 `workflow_graph.rs`、`store.rs:11761`、`store.ts:1674`。
- 验收标准：普通消息、ChatOnly 消息、需审批工具、拒绝审批、MCP 失败、stream/final 合并、应用重载后 run 恢复全部有 focused tests。