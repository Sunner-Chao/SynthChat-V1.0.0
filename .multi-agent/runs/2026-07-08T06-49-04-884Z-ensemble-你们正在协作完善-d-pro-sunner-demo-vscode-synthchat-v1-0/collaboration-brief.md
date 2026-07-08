# Planning skipped

Planning was skipped by `--skip-planning`.

Use the task text and any referenced prior-stage artifacts as the coordination brief.

Task:

你们正在协作完善 D:\pro_sunner\demo_vscode\SynthChat-V1.0.0，这是一个 Tauri + React + Rust 的桌面 agent 应用。

本阶段目标：
阶段 5：真实模拟所有 agent 功能。本轮只处理一个能力组，基于阶段 4 的测试 harness，真实模拟 SynthChat 的 agent 功能链路。

阶段衔接要求：
- 先读取 D:\pro_sunner\demo_vscode\SynthChat-V1.0.0\.multi-agent\runs 中最近的阶段 1、阶段 2、阶段 3、阶段 4 产物。
- 只选择一个最高 P0 风险能力组处理；如果前序产物不足，默认选择 G. UI streaming / tool cards / error display。
- 不要同时处理多个能力组。

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

能力组：
A. 记忆与 persona
B. MCP 与 tool search
C. 文件/终端/浏览器工具
D. approval/abort/safety
E. agent queue / run management
F. provider transport / fallback
G. UI streaming / tool cards / error display

工作要求：
- 先写失败测试或复现脚本。
- 再做最小修复。
- 不允许为了过测试删除真实逻辑。
- 修复后补充回归用例。
- 不做大范围重写。
- 每次修改都必须有可运行验证。
- 对话链路优先于边缘功能。
- 输出必须包含：发现、风险、建议、待测清单、下一阶段输入。

验收：
- 必须引用具体文件路径和函数/模块。
- 必须说明选中的能力组、原因、实际修改文件和验证结果。
