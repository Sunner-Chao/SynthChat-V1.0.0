## 1. 阻断问题

1. **缺 provider/model 的失败路径会留下 running run，且 UI 没有可见错误消息。**

- `src-tauri/src/agent/agent_loop.rs:1422`-`1448` 先持久化 user message。
- `src-tauri/src/agent/agent_loop.rs:1488`-`1517` 创建并保存 `state = "running"` 的 `AgentRunRecord`，并 emit run event。
- 但 provider/model 校验发生在之后：`src-tauri/src/agent/agent_loop.rs:1528`-`1533`，这里直接 `return Err(...)`。
- 外层 `run_chat_turn()` 只 emit `turn_finished ok=false`，不会写入 assistant error，也不会把刚保存的 run 标记为 `failed`：`src-tauri/src/agent/agent_loop.rs:1204`-`1242`。
- 前端 catch 路径还明确“不把 transient transport errors 放进 chat timeline”：`src/lib/store.ts:2400`-`2428`。`App.tsx` 对 `turn_finished ok=false` 也只是清 stream 并 refresh：`src/App.tsx:547`-`553`。

影响：首次配置不完整或模型 ID 为空时，用户会看到本地 user 消息结束 processing，但没有对话内错误；同时 agent run 会短期保持 running，直到读路径超时恢复。发版前这属于真实桌面 agent MVP 阻断。

2. **最终验收仍没有真实桌面 E2E 证明“输入 -> Rust runtime -> LLM/tool -> Tauri event -> UI final”闭环。**

当前链路代码是可达的：`ChatExperience.submit()` 调 `sendMessage`（`src/panels/ChatExperience.tsx:1740`-`1775`），`api.sendChatMessage()` 走 Tauri invoke（`src/lib/api.ts:249`-`255`, `553`-`561`），Rust command 调 `agent::run_chat_turn()`（`src-tauri/src/lib.rs:2445`-`2452`），最终通过 `synthchat-chat-event` 回前端（`src-tauri/src/agent/agent_loop.rs:3068`-`3095`, `src/App.tsx:497`-`670`）。

但现有新增测试是 store/mock 级：`src/lib/__tests__/chatConversationChain.test.ts:96`-`333` mock 了 `api.sendChatMessage`、`api.listMessages`、agent run event。没有 Tauri event harness、没有真实 Rust fake LLM/tool run、没有 Windows 桌面 smoke。阶段 7 要求判断“成熟桌面 agent MVP”，这个证据不足。

## 2. 非阻断问题

1. **README 明显滞后，会误导验收。**  
`README.md` 仍写 `src/lib/api.ts` 是 Mock、后续才连接后端；但当前 `src/lib/api.ts:253`-`258` 已经在 Tauri 环境调用 `invoke()`，`send_chat_message` 也注册在 `src-tauri/src/lib.rs:7753`-`7815`。这是文档问题，不直接阻断 runtime，但会误导发布说明。

2. **stream cleanup 依赖 active work 信号正确收敛。**  
Stage 6 的清理逻辑集中在 `src/lib/store.ts:535`-`565`, `762`-`823`, `869`-`894`，能在无 active work 时清 orphan stream。但如果 `processingConversationIds` 或 `agentRuns` 因后端状态滞留保持 active，`mergeBackendMessagesForState()` 会继续保留 stream。相关 UI/Tauri 顺序没有组件或 E2E 覆盖。

3. **安全能力有实现迹象，但缺端到端验收。**  
危险命令 guard 存在：`src-tauri/src/agent/command_guard.rs:8`-`107` 覆盖格式化磁盘、shutdown、raw device write、递归删除、敏感路径写入、远程脚本管道等。密钥脱敏也有 `src-tauri/src/agent/redact.rs`。但本阶段没有验证“LLM tool call -> approval/guard -> UI trace”的真实链路，不能作为发布级安全验收通过依据。

## 3. 建议补测

1. Rust focused test：provider 缺失、model 缺失发生在 run 保存之后时，断言 run 进入 `failed`，写入 `desktop-agent-error` assistant message，并 emit `turn_finished ok=false`。

2. Tauri/App event harness：模拟 `turn_started -> assistant_stream -> turn_finished ok=false`，断言 `src/App.tsx` 清 pending stream、清 `streamedAssistantIds`、UI 不残留 ghost bubble，同时错误消息能从 backend refresh 出现。

3. 桌面 E2E smoke：至少覆盖普通成功、provider 失败、abort、retry、一个危险 terminal command 被阻断、一个 tool event 可见。

4. UI 回归：`MessageList`/`ThinkingCards`/`ToolMessage` 在失败、取消、重试后不重复消息、不残留 loading、不吞掉最终 assistant。

## 4. 是否达到“成熟桌面 agent MVP”

**否。**

当前可以说：对话链路代码可达，Stage 6 的前端 store 层 stream/orphan cleanup 有针对性测试，agent runtime 不是纯 mock。

但发版前“成熟桌面 agent MVP”还没达到：存在具体失败路径会留下 running run 且不显示错误；没有真实 Tauri/Rust/UI 端到端验收；安全/审批/危险命令只看到模块实现和局部测试迹象，缺完整链路验证。