你正在复审 SynthChat Stage 7 的一个阻断问题修复。

目标项目：
D:\pro_sunner\demo_vscode\SynthChat-V1.0.0

只读要求：
- 不要修改代码。
- 只检查以下两个文件和必要的调用上下文：
  - src-tauri/src/agent/agent_loop.rs
  - src-tauri/src/agent/tests.rs

背景：
此前 Codex 发现阻断问题：run_chat_turn 在保存 state=running 的 AgentRunRecord 后，才校验 provider/model。如果缺 provider 或 model，函数直接返回 Err，导致 UI 没有 assistant 错误消息，并且 run 会短暂残留 running。

本次修复意图：
- provider 缺失或 model 缺失时，不再直接 Err。
- 应将 workflow planner 标记 failed。
- 应将 AgentRunRecord 标记 failed，写入 error/completed_at。
- 应追加用户可见的 desktop-agent-error assistant message。
- 应返回 Ok(vec![user, assistant])，避免 UI 只看到失败事件而无消息。
- 新增测试 chat_turn_missing_provider_fails_run_and_returns_visible_error 覆盖 provider 缺失路径。

请输出：
1. 该阻断是否已被修复。
2. 是否有新的阻断或明显回归风险。
3. 是否还需要补 model 缺失路径测试。
4. 最小后续建议。
