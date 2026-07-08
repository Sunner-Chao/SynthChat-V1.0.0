You are the claude reviewer in a Claude + Codex local coding team.

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

Implementation notes:
**选中能力组**

能力组 G：UI streaming / tool cards / error display。原因是本轮只处理失败/中断后 assistant streaming bubble 清理，以及 stale refresh 不再恢复 ghost bubble。

**发现**

`App.tsx` 现有逻辑符合要求：仅在 `turn_finished` 且 `ok === false` 且不是 WeChat turn 时清理 desktop stream；`isWechatTurnEvent` 覆盖显式 WeChat event 和 active WeChat turn，未破坏 WeChat fallback/defer 时序。

复核时发现一个缺口：inactive conversation 的 assistant stream 会进入 pending incoming，但之前没有稳定标记为 `desktop-stream`，也没有在 pending 清理后移除对应 `streamedAssistantIds`。这会导致 stale refresh 仍有机会保留 ghost bubble。本轮已修正。

**实际修改文件**

- [src/lib/store.ts](D:/pro_sunner/demo_vscode/SynthChat-V1.0.0/src/lib/store.ts)
  - inactive conversation 收到 streaming assistant 时，pending message 标记为 `desktop-stream` 并登记 streamed id。
  - final assistant 到达时解除 streamed id。
  - `clearStreamingAssistantMessages(conversationId)` 现在同时清理 active messages 和 pending incoming messages，并用 pending/active removed ids 一起收敛 `streamedAssistantIds`。

- [src/lib/__tests__/storeMessageMerge.test.ts](D:/pro_sunner/demo_vscode/SynthChat-V1.0.0/src/lib/__tests__/storeMessageMerge.test.ts)
  - 补充 focused regression：失败 pending assistant stream 在 inactive conversation refresh 前被清理，stale merge 不再恢复。

`src/App.tsx` 是工作区已有改动；本轮只复核，未继续修改。

**验证结果**

- `npm test`：通过，12 test files / 143 tests。
- `npm run build`：通过。Vite 仍提示 chunk 大于 500 kB，这是既有构建警告，不属于本轮范围。

**风险和下一阶段输入**

风险较低，改动集中在 desktop assistant streaming 状态收敛。下一阶段建议用 App 事件层或端到端路径补一个 `turn_finished ok=false` 的 UI/event regression，覆盖真实 Tauri event 顺序下的失败流清理。

Review rules:
- Do not edit files.
- Review the current workspace diff against the task.
- Prioritize correctness bugs, regressions, missing tests, and unsafe behavior.
- Include file and line references when possible.
- If no blocking issues are found, say that clearly and mention residual risk.

Workflow-specific instruction:
This is ensemble round 1 of 1. Focus on whether another implementation round is needed.

Output contract for cc-team:
- You are producing the final artifact for claude-ensemble-review-r1 (claude/review).
- You may use Skills, subagents, planning tools, scratch files, or internal notes if your CLI supports them.
- Regardless of any internal delegation, the final answer must be emitted directly to stdout as complete Markdown.
- Do not finish with only a progress/status sentence such as 'writing the plan file' or 'collected enough information'.
- If you create or update a separate plan/artifact file, include its path and the full useful content or a faithful detailed summary in the final artifact.
- The orchestrator will treat status-only output as a failed artifact and may retry or hand off to the other agent.