# Stage 7 阻断修复记录

## 背景

Stage 7 Codex 复审指出一个发版前阻断问题：`run_chat_turn` 在已经保存 `state = "running"` 的 `AgentRunRecord` 之后才校验 provider/model。缺少 provider 或 model 时，代码直接返回 `Err`，前端只能收到失败事件或刷新结果，无法在对话流中看到明确 assistant 错误消息，run 状态也可能短暂残留为 running。

原复审文件：

- `.multi-agent/runs/2026-07-08T09-09-53-014Z-review-你们正在协作完善-d-pro-sunner-demo-vscode-synthchat-v1-0/codex-review.md`

Claude 复审说明：

- Claude probe 曾通过，但正式复审失败于外部 403/Cloudflare DNS 限制，不是项目代码错误。
- 因用户要求避免浪费 Claude token，本轮未继续依赖 Claude 长任务。

## 修复内容

变更文件：

- `src-tauri/src/agent/agent_loop.rs`
- `src-tauri/src/agent/tests.rs`

核心修复：

- 新增 `fail_chat_turn_before_llm(...)`，用于 LLM 前置配置错误的统一失败路径。
- 缺 provider 时返回用户可见的 `desktop-agent-error` assistant message。
- 缺 model 时也走同一失败路径。
- 失败路径会：
  - 将 workflow planner 标记为 `failed`。
  - 将 `AgentRunRecord.state` 设置为 `failed`。
  - 写入 `error`、`completed_at`、`updated_at`。
  - 运行 session finished hooks。
  - 持久化用户消息与 assistant 错误消息。
  - emit 最新 run record。
  - 返回 `Ok(vec![user, assistant])`。

新增回归测试：

- `chat_turn_missing_provider_fails_run_and_returns_visible_error`
- `chat_turn_missing_model_fails_run_and_returns_visible_error`

这些测试断言：

- 返回 user + assistant 两条消息。
- assistant source 为 `desktop-agent-error`。
- active run 不再存在。
- 持久化 run 状态为 `failed`。
- run `completed_at` 已写入。
- run error 包含 provider/model 配置提示。
- workflow planner 节点为 `failed`，`errorKind` 为 `llm_error`。
- 后端 messages 中存在用户可见错误消息。

## Focused Codex 复审

复审 run：

- `.multi-agent/runs/2026-07-08T09-35-54-707Z-review-你正在复审-synthchat-stage-7-的一个阻断问题修复-目标项目-d-pro-sun/codex-review.md`

结论：

- 未发现新的阻断问题。
- 原阻断已修复。
- Codex 建议补 model 缺失路径测试，以及补 `completed_at` / workflow planner failed 断言；这些建议已完成。

## 验证

已通过：

- `cargo fmt --all`
- `cargo test chat_turn_missing_provider_fails_run_and_returns_visible_error`
- `cargo test chat_turn_missing_`
- `cargo check`
- `npm test`
- `npm run build`

备注：

- `cargo fmt --all` 对多个 Rust 文件产生了格式化 diff，其中逻辑变更集中在 `src-tauri/src/agent/agent_loop.rs` 与 `src-tauri/src/agent/tests.rs`。
- 如后续需要最小化 diff，可单独处理格式化 churn；当前没有执行破坏性 revert。

## 当前状态

这个 Stage 7 阻断已按 Codex 发现点完成修复和本地验证。仍建议补充：

- model 缺失路径的对称 Rust 测试。
- Tauri/App event harness，验证 `turn_started -> assistant_stream -> turn_finished ok=false` 与 backend refresh 后 UI 无 ghost bubble。
- Windows 桌面 smoke，覆盖成功、失败、取消、重试、危险命令阻断和工具事件可见性。
