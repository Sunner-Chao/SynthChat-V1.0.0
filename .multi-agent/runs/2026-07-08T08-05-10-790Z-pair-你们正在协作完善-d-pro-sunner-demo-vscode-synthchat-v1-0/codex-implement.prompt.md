You are the codex lead implementation agent in a Claude + Codex local coding team.

Workflow: Pair (pair)
Workflow source inspiration: claude-consensus + local handoff

Workspace: D:\pro_sunner\demo_vscode\SynthChat-V1.0.0

Task:
你们正在协作完善 D:\pro_sunner\demo_vscode\SynthChat-V1.0.0，这是一个 Tauri + React + Rust 的桌面 agent 应用。

当前阶段：阶段 6，成熟桌面 agent 应用优化。本轮只做一个 P0 小切片，避免大范围发散。

阶段上下文：
- 阶段 4 已建立 TypeScript/Vitest 测试闭环。
- 阶段 5-G 已修复 `turn_finished ok=false` 时非 WeChat assistant streaming ghost bubble 清理。
- Claude 对阶段 5-G 的复审指出一个低风险残留：如果 Tauri/后端/agent 进程异常中断，前端可能收不到 `turn_finished`，那么 live `desktop-stream` assistant bubble 可能被后续 stale refresh 继续保留。

必须先读：
- package.json / README.md
- src/App.tsx
- src/panels/ChatExperience.tsx
- src/lib/store.ts
- src/lib/__tests__/storeMessageMerge.test.ts
- src/lib/__tests__/chatConversationChain.test.ts
- src/lib/__tests__/chatTestHarness.ts
- D:\pro_sunner\demo_vscode\SynthChat-V1.0.0\.multi-agent\runs\2026-07-08T07-38-02-219Z-ensemble-你们正在协作完善-d-pro-sunner-demo-vscode-synthchat-v1-0\codex-ensemble-implement-r1.md
- D:\pro_sunner\demo_vscode\SynthChat-V1.0.0\.multi-agent\runs\2026-07-08T07-38-02-219Z-ensemble-你们正在协作完善-d-pro-sunner-demo-vscode-synthchat-v1-0\claude-review.md

本轮目标：
1. 先输出 P0/P1/P2 路线图，覆盖稳定性、安全性、可观测性、用户体验、记忆系统、扩展性、测试成熟度、打包发布。
2. 只实际执行一个 P0：异常中断/恢复/刷新场景下，不允许无活跃运行的 assistant `desktop-stream` 残留被 refresh/merge 无限保留。

范围限制：
- 不要实现 MCP、记忆、文件/终端/浏览器工具、provider fallback、agent queue 大改、Tauri Rust 改造。
- 优先只改：
  - src/lib/store.ts
  - src/lib/__tests__/storeMessageMerge.test.ts 或 src/lib/__tests__/chatConversationChain.test.ts
- 不做大范围重写，不删除真实逻辑。
- 正常流式输出正在运行时，仍必须能跨 stale refresh 保留 live assistant stream。

建议技术方向，但请以实际代码为准：
- `mergeBackendMessagesWithLiveState` 当前通过 `streamedAssistantIds` 保留 live assistant stream。
- 可以引入一个明确的“是否允许保留 live assistant stream”的条件，例如当前 conversation 仍在 `processingConversationIds`、`agentQueue`、`agentRuns`、`activeAgentRuns` 中有活跃工作时才保留。
- 如果 conversation 已没有活跃工作且 backend 也没有最终 assistant，应清理该 conversation 的 `desktop-stream`/`streamedAssistantIds`，避免 ghost bubble。
- 注意不要破坏阶段 5-G 已有测试：正常 streaming stale refresh 仍保留，失败 cleanup 仍清理。

必须补测试：
- 正常 active processing 下：assistant stream 经过 stale refresh 仍保留。
- 无 active processing / no active agent work 下：orphan assistant stream 经过 refresh/merge 被移除，`streamedAssistantIds` 同步收敛。
- 如果已有测试覆盖第一条，只补第二条并必要调整第一条即可。

验证命令：
- npm test
- npm run build

输出必须包含：
- P0/P1/P2 路线图。
- 本轮实际执行的 P0。
- 发现。
- 实际修改文件。
- 验证结果。
- 风险、待测清单、下一阶段输入。

Collaboration brief:
# Planning skipped

Planning was skipped by `--skip-planning`.

Use the task text and any referenced prior-stage artifacts as the coordination brief.

Task:

你们正在协作完善 D:\pro_sunner\demo_vscode\SynthChat-V1.0.0，这是一个 Tauri + React + Rust 的桌面 agent 应用。

当前阶段：阶段 6，成熟桌面 agent 应用优化。本轮只做一个 P0 小切片，避免大范围发散。

阶段上下文：
- 阶段 4 已建立 TypeScript/Vitest 测试闭环。
- 阶段 5-G 已修复 `turn_finished ok=false` 时非 WeChat assistant streaming ghost bubble 清理。
- Claude 对阶段 5-G 的复审指出一个低风险残留：如果 Tauri/后端/agent 进程异常中断，前端可能收不到 `turn_finished`，那么 live `desktop-stream` assistant bubble 可能被后续 stale refresh 继续保留。

必须先读：
- package.json / README.md
- src/App.tsx
- src/panels/ChatExperience.tsx
- src/lib/store.ts
- src/lib/__tests__/storeMessageMerge.test.ts
- src/lib/__tests__/chatConversationChain.test.ts
- src/lib/__tests__/chatTestHarness.ts
- D:\pro_sunner\demo_vscode\SynthChat-V1.0.0\.multi-agent\runs\2026-07-08T07-38-02-219Z-ensemble-你们正在协作完善-d-pro-sunner-demo-vscode-synthchat-v1-0\codex-ensemble-implement-r1.md
- D:\pro_sunner\demo_vscode\SynthChat-V1.0.0\.multi-agent\runs\2026-07-08T07-38-02-219Z-ensemble-你们正在协作完善-d-pro-sunner-demo-vscode-synthchat-v1-0\claude-review.md

本轮目标：
1. 先输出 P0/P1/P2 路线图，覆盖稳定性、安全性、可观测性、用户体验、记忆系统、扩展性、测试成熟度、打包发布。
2. 只实际执行一个 P0：异常中断/恢复/刷新场景下，不允许无活跃运行的 assistant `desktop-stream` 残留被 refresh/merge 无限保留。

范围限制：
- 不要实现 MCP、记忆、文件/终端/浏览器工具、provider fallback、agent queue 大改、Tauri Rust 改造。
- 优先只改：
  - src/lib/store.ts
  - src/lib/__tests__/storeMessageMerge.test.ts 或 src/lib/__tests__/chatConversationChain.test.ts
- 不做大范围重写，不删除真实逻辑。
- 正常流式输出正在运行时，仍必须能跨 stale refresh 保留 live assistant stream。

建议技术方向，但请以实际代码为准：
- `mergeBackendMessagesWithLiveState` 当前通过 `streamedAssistantIds` 保留 live assistant stream。
- 可以引入一个明确的“是否允许保留 live assistant stream”的条件，例如当前 conversation 仍在 `processingConversationIds`、`agentQueue`、`agentRuns`、`activeAgentRuns` 中有活跃工作时才保留。
- 如果 conversation 已没有活跃工作且 backend 也没有最终 assistant，应清理该 conversation 的 `desktop-stream`/`streamedAssistantIds`，避免 ghost bubble。
- 注意不要破坏阶段 5-G 已有测试：正常 streaming stale refresh 仍保留，失败 cleanup 仍清理。

必须补测试：
- 正常 active processing 下：assistant stream 经过 stale refresh 仍保留。
- 无 active processing / no active agent work 下：orphan assistant stream 经过 refresh/merge 被移除，`streamedAssistantIds` 同步收敛。
- 如果已有测试覆盖第一条，只补第二条并必要调整第一条即可。

验证命令：
- npm test
- npm run build

输出必须包含：
- P0/P1/P2 路线图。
- 本轮实际执行的 P0。
- 发现。
- 实际修改文件。
- 验证结果。
- 风险、待测清单、下一阶段输入。

Implementation rules:
- Make the smallest useful set of edits that satisfies the task.
- Preserve unrelated user changes.
- Follow the repository's existing style.
- Run focused verification when practical.
- Leave a concise final note with changed files and verification.

Output contract for cc-team:
- You are producing the final artifact for codex-implement (codex/implement).
- You may use Skills, subagents, planning tools, scratch files, or internal notes if your CLI supports them.
- Regardless of any internal delegation, the final answer must be emitted through the final CLI response captured by the configured output artifact as complete Markdown.
- Do not finish with only a progress/status sentence such as 'writing the plan file' or 'collected enough information'.
- If you create or update a separate plan/artifact file, include its path and the full useful content or a faithful detailed summary in the final artifact.
- The orchestrator will treat status-only output as a failed artifact and may retry or hand off to the other agent.