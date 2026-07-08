你们正在协作完善 D:\pro_sunner\demo_vscode\SynthChat-V1.0.0，这是一个 Tauri + React + Rust 的桌面 agent 应用。

本阶段目标：
阶段 1：全量理解项目。请全面理解 SynthChat-Dev 的架构，不要改代码。

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

工作要求：
- 先只读分析，再提出计划。
- 不做大范围重写。
- 每次修改都必须有可运行验证。
- 对话链路优先于边缘功能：用户消息 -> agent 选择 -> LLM/工具调用 -> 流式事件 -> UI 展示 -> 持久化 -> 错误恢复。
- 输出必须包含：发现、风险、建议、待测清单、下一阶段输入。

产物：
1. 项目架构图：前端、Tauri bridge、Rust agent runtime、LLM transport、MCP、skills、store、chat UI。
2. 对话链路时序图：用户输入到 assistant 消息落 UI 的全过程。
3. agent 能力地图：模型、工具、记忆、审批、队列、运行记录、MCP、技能、插件、浏览器/终端/文件能力。
4. 当前测试覆盖地图。
5. 最大 20 个高风险断点。

验收：
- 必须引用具体文件路径和函数/模块。
- 不得提出泛泛建议。
