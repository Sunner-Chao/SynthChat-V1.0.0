现在开始写产物。已读完阶段 1 完整产物、hermes audit 前100 行及关键Rust 文件，信息足够产出矩阵。

---

# SynthChat 阶段 2：可吸收能力矩阵

**产物路径：** `.multi-agent/runs/2026-07-08T03-57-08-402Z-.../claude-plan.md`（本文件即产物，直接作为 stdout 输出）

---

## Understanding

阶段 1 已确认：SynthChat 是功能密度极高的已有实现，不是空白项目。能力矩阵的价值在于**精准识别哪些概念仍有差距、可以用最小代价补齐**，而非重复已有功能。

核心发现（来自阶段 1 产物 + 源码核查）：

- `src-tauri/src/agent/agent_loop.rs`：主循环已有 Planner→Executor→Reviewer 三阶段、streaming callback、turn lock、iteration budget、tool approval。
- `src-tauri/src/agent/workflow_graph.rs`：已有 checkpoint wait/resume、human-gate、delegate task生命周期常量。
- `src-tauri/src/agent/context_compression.rs`：已有 LLM summary + failover cooldown。
- `src-tauri/src/agent/approval_gateway.rs`：已有 risky/always/never 三档及 MCP retry。
- `src-tauri/src/agent/shell_hooks.rs`：已有 pre/post LLM + tool transform Python hooks。
- `src-tauri/src/agent/delegation*.rs` + `acp_server.rs`：已有并发subagent + JSON-RPC ACP。
- `src-tauri/src/agent/memory.rs` + `memory_manager.rs`：已有多provider记忆。
- `src-tauri/src/mcp.rs`：已有 OAuth / circuit breaker / keepalive / session 全套。
- `src/lib/__tests__/`：11 个 Vitest 文件，缺 event顺序测试和 store action 完整测试。
- Hermes audit 明确：full `cargo test` 延后到 release hardening；focused tests 当前优先。

---

## 可吸收能力矩阵

>说明：每行"最小功能"只描述一个具体可操作的改动，锚定到 SynthChat 实际文件。"不应现阶段实现"只列本阶段明确不做的内容。

---

### 1. Claude Code — Subagents /子代理隔离

| 字段 | 内容 |
|---|---|
| **参考对象** | Claude Code subagents |
| **核心机制** | 每个子代理拥有独立的上下文窗口、token budget和工具集策略；父代理通过结构化 JSON 传递任务、接收结果；子代理完成后销毁上下文 |
| **SynthChat 当前实现** | **已有（部分）**：`agent/delegation.rs`、`delegation_acp.rs`、`delegation_synthchat.rs`实现并发 subagent；`agent/iteration_budget.rs` 有迭代 budget 枚举；`agent_loop.rs:1308` 的 15 参数入口含 `delegation_policy`、`iteration_budget` 两个 `Option` 参数 |
| **应该借鉴的最小功能** | 在 `agent_loop.rs:1308` 的 `run_chat_turn_with_toolset_policy_and_iteration_limit` 调用侧增加参数完整性断言：当delegation 来源的 subagent 调用缺失 `iteration_budget` 时，明确用 `IterationBudget::default_subagent()` 而非 `None`，避免子代理无限循环。具体位置：`delegation.rs` 中调`run_chat_turn_with_toolset_policy_and_iteration_limit` 处，补充 `Some(IterationBudget::SubAgent { max: 20})` 默认值 |
| **不应现阶段实现** | 跨进程隔离（容器/VM级别subagent 沙箱）；子代理完整内存隔离 |
| **对话链路收益** | 防止 subagent 无iteration limit 导致父会话长时间卡在`delegating` 状态，blocking `processingConversationIds` 永不清除（对应阶段 1 高风险断点BP-07）|
| **测试方式** | `src-tauri/src/agent/tests.rs`：新增 `test_subagent_default_iteration_budget`，mock LLM 返回循环 tool_call，断言在 `max_iterations` 后 run 结束而非无限循环 |
| **优先级** | **P1** |

---

### 2. Claude Code — Hooks / 结构化钩子

| 字段 | 内容 |
|---|---|
| **参考对象** | Claude Code hooks（PreToolUse / PostToolUse / Stop） |
| **核心机制** | 钩子以 matcher 字符串匹配工具名，收到结构化 JSON 输入（含 `tool_input`），输出 `exit(2)` 可阻断工具调用；钩子失败不崩溃主循环 |
| **SynthChat 当前实现** | **已有（Python级别）**：`agent/shell_hooks.rs` 实现 pre/post LLM、tool transform 钩子；但钩子执行结果（stdout/stderr）仅写入 run events，UI 无结构化展示；PreToolUse 无 exit-code阻断路径 |
| **应该借鉴的最小功能** | 在 `shell_hooks.rs` 的 `run_pre_tool_call_hooks` 中，读取 hook 进程的 exit code：当 exit code == 2 时，返回 `Err(AppError::HookBlocked)` 而非继续执行工具；该错误在 `tool_dispatch.rs` 中被捕获并记录为 `tool_event_kind::cancelled` 的工具事件，发出 `synthchat-agent-run-event` 通知前端。这是 Claude Code 钩子阻断语义的最小移植 |
| **不应现阶段实现** | 可视化钩子管理 UI；钩子市场/分发；钩子沙箱隔离 |
| **对话链路收益** | 允许用户级 pre-tool 钩子拦截危险工具调用（文件写入、终端命令），比现有 `approval_gateway.rs` 的 risky 模式更灵活（不需要人工审批，可自动化阻断） |
| **测试方式** | `src-tauri/src/agent/tests.rs`：新增 `test_pre_tool_hook_exit2_blocks_tool`，用 fake Python脚本 `exit2`，断言工具未执行且 tool event 为 cancelled |
| **优先级** | **P2** |

---

### 3. Claude Code — Memory / 文件级记忆精度

| 字段 | 内容 |
|---|---|
| **参考对象** | Claude Code memory（文件 frontmatter + MEMORY.md 索引） |
| **核心机制** | 每条记忆是独立 `.md` 文件，frontmatter 含 `name`/`description`/`type`；MEMORY.md 作为索引，session 开始时只加载 MEMORY.md，按需读具体文件；记忆有 `[[link]]` 相互引用 |
| **SynthChat 当前实现** | **已有（不同方式）**：`agent/memory.rs` + `memory_manager.rs` 实现 `remember_fact`/`recall`/`manage_memory`；`store.rs` 的 `PersistedState` 中 memory 是 JSON entries；turn start时 `prompt_builder.rs` 注入 memory context；无MEMORY.md 索引机制，每次全量注入 |
| **应该借鉴的最小功能** | 在 `memory_manager.rs` 的 turn-start memory prefetch 中引入 relevance threshold：只注入 `relevance_score > threshold` 的记忆，而非全量注入；threshold 可从 agent config 读取。参考点：`skills.rs:1621` `prompt_blocks_for_request()` 已有 `MAX_SKILL_PROMPT_CHARS` 和最多 6 个的截断——将类似 budget 机制引入 memory 注入 |
| **不应现阶段实现** | 完整 MEMORY.md 索引文件系统；跨 session 的文件级记忆 sync；memory `[[link]]` 图遍历 |
| **对话链路收益** | 减少 prompt 被 memory 撑大导致 context compression提前触发（阶段 1 高风险断点 BP-04：压缩 failover 路径无测试覆盖）|
| **测试方式** | `src/lib/__tests__/` 新增 `memoryInjection.test.ts`：mock store 有 10 条 memory，其中 3 条高相关度，断言只注入 3 条；现有 Rust `src-tauri/src/agent/tests.rs` 补`test_memory_budget_truncation` |
| **优先级** | **P2** |

---

### 4. Claude Code — AGENTS.md 层级解析

| 字段 | 内容 |
|---|---|
| **参考对象** | Claude Code / Codex AGENTS.md 层级指令解析 |
| **核心机制** | 从当前工作目录向上遍历，合并所有 `AGENTS.md` / `CLAUDE.md`；子目录的指令覆盖父目录；系统 prompt 最终合并所有层级 |
| **SynthChat 当前实现** | **已有（单级）**：项目根目录 `CLAUDE.md` 中的 `@AGENTS.md` 被读取；`agent/workspace.rs` 负责工作区发现；`skills.rs` 的 `prompt_blocks_for_request()` 注入 skill prompt；但多级目录合并逻辑不明确 |
| **应该借鉴的最小功能** | 在 `workspace.rs` 或 `prompt_builder.rs` 中，当 agent 有`workdir` 时，从 workdir 向上最多 3 级查找 `AGENTS.md`，合并为一个 instruction block，注入 system prompt前缀。每级 `AGENTS.md` 的大小限制复用 `MAX_SKILL_PROMPT_CHARS=16000` |
| **不应现阶段实现** | 完整的 `.clauderules` / `.windsurfrules` 等多格式支持；全局 user-level 指令与 project-level 的冲突解决UI |
| **对话链路收益** | 工作于不同子目录的 agent 自动继承正确的项目约束，避免 agent 在子目录写文件时违反上级AGENTS.md 中的规则（直接影响 `file_tools.rs` 的 write/patch 安全） |
| **测试方式** | `src-tauri/src/agent/tests.rs`：`test_agents_md_hierarchy_merge`，创建 3 级临时目录，每级一个 AGENTS.md，断言合并后 system prompt 包含所有层级内容，且子级在父级之后 |
| **优先级** | **P1** |

---

### 5. Codex — Sandbox approval（明确拒绝/单次批准/全局批准）

| 字段 | 内容 |
|---|---|
| **参考对象** | OpenAI Codex sandbox/approval（拒绝 / 本次批准 / 本次 session 批准 / 全局批准） |
| **核心机制** | 工具调用前展示 diff/命令预览；用户可选择 deny / approve-once / approve-session / approve-always；approval 状态持久化到 session store |
| **SynthChat 当前实现** | **已有（功能完整）**：`tool_policy.rs` 有 `ToolApprovalMode::Risky/Always/Never`；`approval_gateway.rs` 有 pending/replay/deny；`store.rs:13181` `append_tool_approval()`；但 approve-session（本次运行内自动批准）与 approve-always（持久化）的边界不如Codex 清晰 |
| **应该借鉴的最小功能** | 在 `tool_policy.rs` 中区分 `ApprovalScope::Session`（存于当前 `AgentRunRecord.session_approvals`，run 结束后销毁）和 `ApprovalScope::Persistent`（存于 `store.rs:tool_approvals()`，跨 run 有效）。前端`src/panels/ChatExperience.tsx` 审批 UI 增加对应按钮标签区分 |
| **不应现阶段实现** | approval 策略的 UI 可视化编辑器；基于文件路径 pattern 的规则引擎 |
| **对话链路收益** | 用户不会在同一 run 内被反复打断同类工具审批；session-scoped approval 不会污染跨 run 的持久化策略（当前阶段 1 断点 BP-14：approval pending 与 queue cancel 误匹配风险）|
| **测试方式** | `src-tauri/src/agent/tests.rs`：`test_session_approval_does_not_persist`，先 approve-session 一个工具，run 结束后新建 run，断言 `tool_approvals()` 中不含该条目 |
| **优先级** | **P1** |

---

### 6. Codex — /review 显式审查模式

| 字段 | 内容 |
|---|---|
| **参考对象** | OpenAI Codex `/review` 命令 |
| **核心机制** | `/review` 触发只读分析pass：agent 读取文件/代码，输出审查意见，不执行写操作；工具集被限制为 read-only |
| **SynthChat 当前实现** | **已有（内置于 workflow）**：`workflow_graph.rs` 有 `ReviewFinal` 阶段，`WorkflowPlannerRoute::ReviewFinal` 在 agent_loop.rs:2811；但这是自动触发的Planner-Reviewer阶段，不是用户可显式调用的只读模式 |
| **应该借鉴的最小功能** | 在 `agent/control_commands.rs` 中注册 `/review` 控制命令，触发时将 `toolset_policy` 设为 `read_only_toolset`（只保留 `read_file`/`search_files`/`web_search`/`session_search` 等非变更工具），然后执行一次 `run_chat_turn_with_toolset_policy_and_iteration_limit` 单轮 |
| **不应现阶段实现** | review 结果的结构化报告格式；review 与 PR 系统的集成 |
| **对话链路收益** | 用户可在不触发写操作的情况下让 agent 分析代码/文件，降低误操作风险；review pass 的 tool events 更清晰（全部是 read-only 类型） |
| **测试方式** | `src-tauri/src/agent/tests.rs`：`test_review_command_no_write_tools`，执行 `/review` 后 mock LLM 试图调用 `write_file`，断言该工具调用被`toolset_policy` 过滤，run 内无 `write_file` tool event |
| **优先级** | **P2** |

---

### 7. Windsurf/Cascade — Chat 模式 vs Write 模式

| 字段 | 内容 |
|---|---|
| **参考对象** | Windsurf Cascade Chat 模式 / Write 模式 |
| **核心机制** | Chat 模式：只LLM 对话，不执行工具；Write 模式：agent 有文件写入权限。模式切换改变 toolset 和 approval 策略 |
| **SynthChat 当前实现** | **已有（粗粒度）**：`AgentDefinition` 有 toolset 字段控制可用工具；`ChatConfig.auto_approve_mode` 控制审批；但没有显式的"无工具 chat-only"模式，每次 turn 都会初始化完整 toolset |
| **应该借鉴的最小功能** | 在 `agent_loop.rs` 的 `run_chat_turn_with_toolset_policy_and_iteration_limit` 入口，检查 agent config 新增的 `mode: ChatOnly | Agent` 字段：当 `ChatOnly` 时，直接跳过 `tool_registry` 初始化和 `WorkflowDriver`，只做单轮 LLM 调用并返回。`ChatExperience.tsx` 的 agent 选择 UI 增加模式标识 |
| **不应现阶段实现** | 动态运行时模式切换（mid-turn）；模式历史记录和可视化 |
| **对话链路收益** | 简单问答不再进入 tool init → workflow → tool dispatch完整路径，`turn_started` → `turn_finished` 延迟显著降低；减少无工具场景下`processingConversationIds` 被意外卡住的概率 |
| **测试方式** | `src-tauri/src/agent/tests.rs`：`test_chat_only_mode_skips_tool_init`，agent config 设为 ChatOnly，断言 `tool_registry::build()` 未被调用；计时断言 ChatOnly 比 Agent 模式快 |
| **优先级** | **P1** |

---

### 8. Windsurf/Cascade — Rules/Skills 按 scope 注入

| 字段 | 内容 |
|---|---|
| **参考对象** | Windsurf memories/rules、Cascade .windsurfrules |
| **核心机制** | 规则按 global / workspace / conversation 三级scope 存储；注入时按 scope 优先级合并；workspace 级规则仅在相关文件被涉及时注入 |
| **SynthChat 当前实现** | **已有（单级）**：`skills.rs:1621` `prompt_blocks_for_request()` 从全量 enabled skills 中选最多 6 个注入；无 global/workspace/conversation 三级区分；`agent/skills.rs` 有 `skill_view`/`skill_manage` 但 scope 字段不明确 |
| **应该借鉴的最小功能** | 在 `skills.rs` 的 `SkillSummary`/`SkillPromptBlock` 中增加 `scope: Global | Workspace | Conversation` 字段（数据模型层）；`prompt_blocks_for_request()` 优先注入 Conversation scope，再Workspace，再 Global，总量仍不超6 个 + `MAX_SKILL_PROMPT_CHARS`；旧 skill 默认为 Global scope |
| **不应现阶段实现** | 细粒度的 file-pattern trigger（只有访问特定文件时才注入某规则）；规则冲突自动解决 |
| **对话链路收益** | 对话上下文相关的规则优先进入 prompt，避免无关全局规则消耗 skill prompt budget，间接缓解 `context_compression.rs` 压力 |
| **测试方式** | `src/lib/__tests__/skillSearch.test.ts`扩
