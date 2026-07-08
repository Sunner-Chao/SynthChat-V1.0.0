你们正在协作完善 D:\pro_sunner\demo_vscode\SynthChat-V1.0.0，这是一个 Tauri + React + Rust 的桌面 agent 应用。

本阶段目标：
阶段 7：最终验收。请进行发版前双重复审，判断是否达到“成熟桌面 agent MVP”。

阶段衔接要求：
- 先读取 D:\pro_sunner\demo_vscode\SynthChat-V1.0.0\.multi-agent\runs 中最近的阶段 1 至阶段 6 产物。
- 不改代码，只做最终复审。

必须先读：
- package.json / README.md
- src/App.tsx
- src/panels/ChatExperience.tsx
- src/lib/**
- src-tauri/src/agent.rs
- src-tauri/src/agent/**
- src-tauri/src/llm/**
- src-tauri/src/mcp.rs
- src-tauri/src/skills.rs
- src-tauri/docs/hermes-agent-capability-audit.md
- 现有测试：src/lib/__tests__/**

重点检查：
- 对话链路是否真的从输入到 UI 完成。
- agent 功能是否只是 mock 通过，还是真实 runtime 可达。
- 测试是否覆盖成功、失败、取消、重试。
- 是否有敏感路径/密钥/危险命令风险。
- 是否有 UI 状态错乱、重复消息、流式残留。
- 是否有 Windows 桌面/Tauri 特有问题。

工作要求：
- 只读复审，不改代码。
- 优先列阻断问题。
- 必须引用具体文件路径和函数/模块。
- 不得提出泛泛建议。

输出：
1. 阻断问题
2. 非阻断问题
3. 建议补测
4. 是否达到“成熟桌面 agent MVP”
