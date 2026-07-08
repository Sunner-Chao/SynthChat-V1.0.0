You are the codex lead implementation agent in a Claude + Codex local coding team.

Workflow: Ensemble (ensemble)
Workflow source inspiration: ensemble

Workspace: D:\pro_sunner\demo_vscode\SynthChat-V1.0.0

Task:
你们正在协作完善 D:\pro_sunner\demo_vscode\SynthChat-V1.0.0，这是一个 Tauri + React + Rust 的桌面 agent 应用。

当前阶段：阶段 5，能力组 G：UI streaming / tool cards / error display。

背景：
- 阶段 4 已建立 TypeScript/Vitest 测试闭环。
- 阶段 5 上一轮 Codex 未能产出最终 artifact，但已经落盘了一个窄修复：当 `turn_finished ok=false` 时清理非 WeChat desktop assistant stream 残留，避免 stale refresh 继续保留 ghost bubble。
- 当前工作区已有改动，请不要回滚用户或前序 agent 改动。

只读/实现范围：
- 只读：
  - src/App.tsx
  - src/lib/store.ts
  - src/lib/__tests__/storeMessageMerge.test.ts
  - src/lib/__tests__/chatConversationChain.test.ts
  - src/lib/__tests__/chatTestHarness.ts
- 只处理能力组 G 中的一个问题：失败/中断后的 assistant streaming bubble 清理和 stale refresh 收敛。
- 不要处理 MCP、记忆、文件/终端工具、provider fallback、agent queue 等其他能力组。

任务：
1. 复核现有改动是否正确：
   - `clearStreamingAssistantMessages(conversationId)` 是否会清理 active messages 和 pending incoming messages 中的 streaming assistant。
   - `App.tsx` 是否只在非 WeChat `turn_finished ok=false` 时清理 desktop stream，避免破坏 WeChat fallback/defer 时序。
2. 如有必要，做最小修正或补充一个 focused 回归测试。
3. 运行验证：
   - npm test
   - npm run build

输出必须包含：
- 选中能力组和原因。
- 发现。
- 实际修改文件。
- 验证结果。
- 风险和下一阶段输入。

Collaboration brief:
# Planning skipped

Planning was skipped by `--skip-planning`.

Use the task text and any referenced prior-stage artifacts as the coordination brief.

Task:

你们正在协作完善 D:\pro_sunner\demo_vscode\SynthChat-V1.0.0，这是一个 Tauri + React + Rust 的桌面 agent 应用。

当前阶段：阶段 5，能力组 G：UI streaming / tool cards / error display。

背景：
- 阶段 4 已建立 TypeScript/Vitest 测试闭环。
- 阶段 5 上一轮 Codex 未能产出最终 artifact，但已经落盘了一个窄修复：当 `turn_finished ok=false` 时清理非 WeChat desktop assistant stream 残留，避免 stale refresh 继续保留 ghost bubble。
- 当前工作区已有改动，请不要回滚用户或前序 agent 改动。

只读/实现范围：
- 只读：
  - src/App.tsx
  - src/lib/store.ts
  - src/lib/__tests__/storeMessageMerge.test.ts
  - src/lib/__tests__/chatConversationChain.test.ts
  - src/lib/__tests__/chatTestHarness.ts
- 只处理能力组 G 中的一个问题：失败/中断后的 assistant streaming bubble 清理和 stale refresh 收敛。
- 不要处理 MCP、记忆、文件/终端工具、provider fallback、agent queue 等其他能力组。

任务：
1. 复核现有改动是否正确：
   - `clearStreamingAssistantMessages(conversationId)` 是否会清理 active messages 和 pending incoming messages 中的 streaming assistant。
   - `App.tsx` 是否只在非 WeChat `turn_finished ok=false` 时清理 desktop stream，避免破坏 WeChat fallback/defer 时序。
2. 如有必要，做最小修正或补充一个 focused 回归测试。
3. 运行验证：
   - npm test
   - npm run build

输出必须包含：
- 选中能力组和原因。
- 发现。
- 实际修改文件。
- 验证结果。
- 风险和下一阶段输入。

Implementation rules:
- Make the smallest useful set of edits that satisfies the task.
- Preserve unrelated user changes.
- Follow the repository's existing style.
- Run focused verification when practical.
- Leave a concise final note with changed files and verification.

Workflow-specific instruction:
This is ensemble round 1 of 1. Use prior reviews in this run directory as coordination messages.

Output contract for cc-team:
- You are producing the final artifact for codex-ensemble-implement-r1 (codex/implement).
- You may use Skills, subagents, planning tools, scratch files, or internal notes if your CLI supports them.
- Regardless of any internal delegation, the final answer must be emitted through the final CLI response captured by the configured output artifact as complete Markdown.
- Do not finish with only a progress/status sentence such as 'writing the plan file' or 'collected enough information'.
- If you create or update a separate plan/artifact file, include its path and the full useful content or a faithful detailed summary in the final artifact.
- The orchestrator will treat status-only output as a failed artifact and may retry or hand off to the other agent.