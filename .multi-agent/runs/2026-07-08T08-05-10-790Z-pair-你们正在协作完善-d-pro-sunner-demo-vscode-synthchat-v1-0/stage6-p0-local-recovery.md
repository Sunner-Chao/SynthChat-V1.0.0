# Stage 6 P0 Local Recovery

## 状态

本轮原计划由 Codex lead 实现、Claude review。Codex 在已写入部分代码后遇到上游 `502 Bad Gateway`，未能生成 `codex-implement.md`。随后对已落盘代码进行本地人工复核和补丁收口。

Claude review 先通过 `CLAUDE_OK` 探针，但正式 review 进程 3 分 45 秒保持 stdout/stderr 0B，已手动终止，未作为通过依据。

## P0 / P1 / P2 路线图

P0:
- 稳定性：失败/中断/刷新后不保留无活跃运行的 assistant streaming ghost bubble。
- 用户体验：正常流式输出仍能跨 stale refresh 保留，避免回答闪烁或丢失。
- 测试成熟度：用 store merge 和 `refreshChatData` 级测试覆盖保留与清理两条路径。

P1:
- 可观测性：run timeline 展示 turn_finished 缺失、stream cleanup、refresh merge 决策。
- 安全性：工具调用审批状态与危险命令阻断进入统一 run trace。
- 错误恢复：provider/tool/stream 中断后提供可重试 UI 状态，而不是只清泡泡。

P2:
- 记忆系统：persona 绑定的可编辑长期记忆和短期上下文压缩。
- 扩展性：MCP、skills、provider transport 的插件化注册与回归测试。
- 打包发布：Tauri 配置迁移、崩溃恢复、日志导出和 Windows 打包验收。

## 本轮实际执行的 P0

目标：异常中断或恢复刷新时，如果当前 conversation 已没有 `processingConversationIds`、active queue、active run 或 active agent run，则不再让 `desktop-stream` assistant message 被 refresh/merge 无限保留；如果仍有活跃工作，则继续保留正常 streaming。

## 修改文件

- `src/lib/store.ts`
  - 增加 `mergeBackendMessagesWithLiveStateResult`，同时返回 `messages` 和收敛后的 `streamedAssistantIds`。
  - 增加 `pruneStreamingAssistantMessagesFromLiveState`，在不允许保留 stream 时清理 active messages 与 pending incoming messages。
  - 增加 `hasActiveConversationWork` / `mergeBackendMessagesForState`，让 bootstrap、refresh、select、delete、loadOlder 等刷新路径统一根据活跃工作状态决定是否保留 live assistant stream。
  - 补齐 `deleteAgent` 路径的 `streamedAssistantIds` 同步。

- `src/lib/__tests__/storeMessageMerge.test.ts`
  - 增加 live stream 保留、orphan stream 清理、失败 stream 清理、inactive pending stream 清理回归测试。

- `src/lib/__tests__/chatConversationChain.test.ts`
  - 增加真实 `refreshChatData` 级测试：processing active 时保留 stream；无活跃工作时清理 orphan stream 并收敛 `streamedAssistantIds`。

## 验证结果

- `npx vitest run src/lib/__tests__/chatConversationChain.test.ts src/lib/__tests__/storeMessageMerge.test.ts`：通过，2 files / 14 tests。
- `npm test`：通过，12 files / 146 tests。
- `npm run build`：通过。Vite 仍有既有 chunk-size warning。
- `git diff --check`：通过，仅显示 Windows CRLF 提示。

## 风险

- Claude review 未完成，原因是 CLI 子进程长时间 0B 静默，不作为质量门禁。
- 当前 P0 覆盖的是前端 store/refresh 层；Tauri backend 崩溃恢复、run timeline、日志导出尚未实现。

## 待测清单

- App/Tauri event 顺序集成测试：`turn_started -> assistant_stream -> process/window interruption -> refreshChatData`。
- 后端 agent runtime 在异常退出后是否把 queue/run 标记为 terminal。
- UI 层 MessageList/ThinkingCards 在 orphan cleanup 后是否无残留 loading 状态。

## 下一阶段输入

建议进入 Stage 7 的只读最终复审，但由于 Claude/Codex 网关近期不稳定，优先使用短 prompt、低 effort、严格超时，并先复用本文件与当前 diff，避免重新全仓库扫描。
