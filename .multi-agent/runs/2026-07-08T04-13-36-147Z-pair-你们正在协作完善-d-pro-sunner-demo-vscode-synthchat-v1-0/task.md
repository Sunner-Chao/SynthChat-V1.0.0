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
