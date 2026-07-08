你们正在协作完善 D:\pro_sunner\demo_vscode\SynthChat-V1.0.0，这是一个 Tauri + React + Rust 的桌面 agent 应用。

本阶段目标：
阶段 2：主流 agent 能力借鉴。基于官方文档和 SynthChat 现状，产出“可吸收能力矩阵”。重点吸收能力，不要照搬实现，不改代码。

阶段衔接要求：
- 先读取 D:\pro_sunner\demo_vscode\SynthChat-V1.0.0\.multi-agent\runs 中最近的阶段 1 产物。
- 如果阶段 1 产物缺失或不完整，先基于当前代码库自洽推进，并明确缺失信息。
- 需要联网检索官方文档时，只使用官方文档或项目官方仓库/文档。

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

参考对象：
- Claude Code：subagents、hooks、memory、MCP、permissions、sessions。
- Codex：本地读写运行代码、sandbox/approval、/review、AGENTS.md 指令。
- Windsurf/Cascade：memories/rules、skills、workflows、AGENTS.md、MCP、Write/Chat 模式。
- LangGraph：durable execution、streaming、human-in-the-loop、checkpoint。
- AutoGen/CrewAI：多 agent 对话、任务/角色/crew、memory、guardrails、observability。
- OpenHands：sandbox/runtime、工具执行隔离、文件/终端/浏览器执行环境。

工作要求：
- 先只读分析，再提出计划。
- 不做大范围重写。
- 对话链路优先于边缘功能：用户消息 -> agent 选择 -> LLM/工具调用 -> 流式事件 -> UI 展示 -> 持久化 -> 错误恢复。
- 输出必须包含：发现、风险、建议、待测清单、下一阶段输入。

产物：
可吸收能力矩阵，每行包含：
- 参考对象
- 核心机制
- SynthChat 当前是否已有类似实现
- 应该借鉴的最小功能
- 不应现阶段实现的复杂功能
- 对话链路收益
- 测试方式
- 优先级：P0/P1/P2

验收：
- 必须引用具体文件路径和函数/模块。
- 不得提出泛泛建议。
- 不得改代码。
