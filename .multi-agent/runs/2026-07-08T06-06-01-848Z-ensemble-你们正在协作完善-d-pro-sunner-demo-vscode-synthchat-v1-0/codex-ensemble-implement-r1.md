**实现完成**

本轮只改 TypeScript/Vitest 测试层，未改生产逻辑、未动 Rust。

修改文件：
- [chatTestHarness.ts](D:/pro_sunner/demo_vscode/SynthChat-V1.0.0/src/lib/__tests__/chatTestHarness.ts:40)：新增 deterministic reply helper、mock `ToolEvent`、`AgentRunEvent`、queue/request/message fixtures。
- [chatConversationChain.test.ts](D:/pro_sunner/demo_vscode/SynthChat-V1.0.0/src/lib/__tests__/chatConversationChain.test.ts:95)：新增 store 对话链路测试，覆盖发送成功、agentId 有效/无效/fallback、失败后刷新 runs/queue、重试收敛、agent run event + tool event + abort/cancel terminal 状态。
- [storeMessageMerge.test.ts](D:/pro_sunner/demo_vscode/SynthChat-V1.0.0/src/lib/__tests__/storeMessageMerge.test.ts:139)：补 `assistant_stream -> stale refresh -> assistant final` 合并测试，断言不丢 live stream、不重复 assistant、final 后清理 `streamedAssistantIds`。

验证结果：
- `npm test`：通过，`12 passed / 141 passed`
- `npm run build`：通过，`tsc && vite build` 成功
- build 仍有 Vite chunk size warning，非本轮引入的失败项

未覆盖风险：
- 未引入 `jsdom` / testing-library，因此未做 React 组件测试。
- 未抽取或挂载 `App.tsx` event listener harness，`turn_finished ok=false` 仍未自动覆盖。
- 未改 Rust，未运行 `cargo fmt --all` / `cargo check`。
- provider error 后 Rust run 是否永久 running、approval UI 链路、真实 ChatExperience stop/cancel 交互仍是后续风险。

下一阶段输入：
- 建议先做 App event handler harness 或最小抽取，覆盖 `turn_started`、`turn_finished ok=false`、双会话 stream 路由。
- 再处理 UI 测试环境 smoke，之后补 `MessageList/ToolMessage/ThinkingCards/ManagedProcessMessage`。
- Rust 侧优先补 provider error active-run 红测、queue pending cancel 不可 claim、approval pending/deny 生命周期。