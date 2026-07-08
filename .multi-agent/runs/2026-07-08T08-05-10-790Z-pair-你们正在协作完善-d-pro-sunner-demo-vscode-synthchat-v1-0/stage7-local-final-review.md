# Stage 7 Local Final Review

## 复审说明

本阶段未完成原计划的双 agent auto-review：

- Codex implementation 子进程在写入代码后遭遇上游 `502 Bad Gateway`，未生成最终 artifact。
- Claude review 探针通过，但正式 review 进程 3 分 45 秒保持 stdout/stderr 0B，已手动终止。

因此本文件是本地只读复审结果，依据当前 diff、前端测试、构建和 Rust `cargo check`。

## 1. 阻断问题

1. 不能宣称达到完整“成熟桌面 agent MVP”。
   - 依据：本轮新增验证集中在前端 store/refresh 层：`src/lib/store.ts`、`src/lib/__tests__/storeMessageMerge.test.ts`、`src/lib/__tests__/chatConversationChain.test.ts`。
   - 缺口：没有真实 Tauri 桌面 E2E 覆盖 `用户输入 -> Rust agent runtime -> LLM/tool -> Tauri event -> UI streaming -> 持久化 -> 恢复`。
   - 当前结论：已达到“前端对话链路 P0 修复可验证”，但未达到“发版级成熟桌面 agent MVP”。

2. Stage 7 原要求的双重复审未完成。
   - 依据：`codex-implement.stderr.txt` 记录 `502 Bad Gateway`；`claude-review` 进程 0B 静默后被终止。
   - 影响：缺少独立第二模型对当前 diff 的最终质量门禁。

## 2. 非阻断问题

1. Rust 编译通过但 warnings 很多。
   - 命令：`cargo check`
   - 结果：通过，但 `src/agent.rs`、`src/agent/delegation.rs`、`src/llm.rs` 等存在大量 unused/dead_code warnings。
   - 影响：不阻断本轮前端修复，但会降低发版前维护信心。

2. 流式残留清理依赖“活跃工作”信号正确收敛。
   - 相关代码：`src/lib/store.ts` 的 `hasActiveConversationWork`、`mergeBackendMessagesForState`、`refreshChatData`。
   - 当前已覆盖：processing active 时保留 stream；无 active work 时清理 orphan stream。
   - 剩余风险：如果 `processingConversationIds` 因某个 UI/event 缺陷长期卡住，store 会按“仍有活跃工作”继续保留 stream。

3. UI 可见性未做截图或浏览器级回归。
   - 相关 UI：`src/panels/ChatExperience.tsx`、`MessageList`、`ThinkingCards`、`ToolMessage`、`ManagedProcessMessage`。
   - 当前验证是 store 级和构建级，没有截图断言 loading、tool card、streaming bubble 消失后的视觉状态。

## 3. 建议补测

1. Tauri event harness：
   - `turn_started -> assistant_stream -> assistant_thinking_stream -> tool_event -> turn_finished ok=false`
   - 断言 `src/App.tsx` 调用 `discardPendingStreamMessagesForConversation` 和 `clearStreamingAssistantMessages`。

2. 异常恢复 E2E：
   - 模拟进程/窗口中断导致没有 `turn_finished`。
   - 下一次 `refreshChatData` 后断言 orphan `desktop-stream` 消失，`streamedAssistantIds` 收敛。

3. Rust focused tests：
   - provider error、tool error、abort、timeout 后，agent queue/run 是否进入 terminal 状态。
   - Rust event payload 是否包含前端需要的 `conversationId`、`ok`、`source`、`message`。

4. UI regression：
   - MessageList 不重复消息。
   - ThinkingCards 不残留 streaming 状态。
   - ToolMessage / ManagedProcessMessage 在失败、取消、重试后可见且不会遮挡。

## 4. 是否达到“成熟桌面 agent MVP”

否。

当前达到的是：

- Stage 4 测试闭环：前端 store/harness 已可测。
- Stage 5-G：失败 `turn_finished ok=false` 的 desktop assistant stream ghost cleanup 已覆盖。
- Stage 6-P0：无活跃工作的 orphan assistant stream 在 refresh/merge 后会被清理，并新增真实 `refreshChatData` 级回归测试。

距离“成熟桌面 agent MVP”仍缺：

- 真实 Tauri/Rust runtime E2E。
- 发版前双 agent 或等价独立复审。
- UI 可见性回归。
- Rust warnings 清理或至少 warnings 基线治理。
- 安全/审批/危险命令/密钥脱敏的端到端验收。

## 已验证命令

- `npx vitest run src/lib/__tests__/chatConversationChain.test.ts src/lib/__tests__/storeMessageMerge.test.ts`：通过，2 files / 14 tests。
- `npm test`：通过，12 files / 146 tests。
- `npm run build`：通过，存在既有 Vite chunk-size warning。
- `cargo check`：通过，存在大量既有 Rust warnings。
- `git diff --check`：通过，仅 Windows CRLF 提示。
