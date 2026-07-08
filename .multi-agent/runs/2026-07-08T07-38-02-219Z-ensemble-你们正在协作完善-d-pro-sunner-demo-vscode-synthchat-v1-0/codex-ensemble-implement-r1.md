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