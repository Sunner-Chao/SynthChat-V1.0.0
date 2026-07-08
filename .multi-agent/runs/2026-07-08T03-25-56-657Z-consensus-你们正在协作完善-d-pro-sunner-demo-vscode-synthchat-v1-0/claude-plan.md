现在我已掌握足够信息，输出完整分析产物。

---

# SynthChat V1.0.0 全量理解报告（阶段 1）

## Understanding

### 技术栈
- **前端**: React 18.3+ Zustand 5 + TypeScript 5.7，Vite 6构建，Vitest 测试
- **桥接层**: Tauri 2（`@tauri-apps/api` v2.1），双向通信：`invoke`（命令调用） + `listen/emit`（事件总线）
- **后端**: Rust 2021 edition，Tokio async runtime，`reqwest 0.12`（HTTP），`tokio-tungstenite`（WebSocket），SQLite/JSON 文件持久化
- **目标平台**: Windows（主）+ macOS，`windows-sys` 依赖仅在 win32 条件下编译

---

## 产物 1：项目架构图

```
┌─────────────────────────────────────────────────────┐
│                          React Frontend(src/)                                     │
│                                                                                     │
│  App.tsx ──────┬──全局事件总线 (listen: synthchat-chat-event, agent-run-event,   │
│                │agent-queue-event, agent-goal-event, managed-process-event,      │
│                │   skills-changed, persona-event)│
│                │                                                │
│                ├── ChatExperience.tsx (2510行，主聊天UI)                            │
│                │   ├── 消息列表 (MessageList / MessageRow)                          │
│                │   ├── 流式消息合并 (scheduleStreamMessageUpsert / flush)           │
│                │   ├── 轮询刷新 (active=1.5s, idle=3s)                              │
│                │   ├── ThinkingCards /ToolSteps / ArtifactPreview                 │
│                │   └── 语音输入 (SpeechRecognition + MediaRecorder fallback)       │
│                │                                                                    │
│                └── Panels: Settings/Agents/MCP/Skills/Persona/Memory/etc.          │
│                                                                                lib/store.ts  (Zustand)                                                            │
│  ├── bootstrap() → 并行加载 config/personas/providers/skills/conversations│
│  ├── sendMessage() → api.sendChatMessage() → Tauri invoke                │
│  ├── upsertIncomingMessage() → stream merge + PROCESSING_MARK_GRACE_MS(1500ms)     │
│  └── mergeBackendMessagesWithLiveState() → 防止 stale-backend覆盖 live-stream     │
│                                                                                     │
│  lib/api.ts    (invoke wrapper, ~100Tauri命令)                                    │
│  lib/types.ts  (所有 TS 类型定义)                                                  │
└─────────────────────────┬───────────────────────────────────────────────────────────┘          │ Tauri 2 IPCinvoke(cmd)  │  emit(event)▼
┌─────────────────────────────────────────────────────┐
│                   Tauri Bridge(src-tauri/src/lib.rs, ~286KB)                      │
│  #[tauri::command]注册：send_chat_message, abort_agent_run, approve_tool_call,│
│  list_conversations, list_messages, list_agents, list_mcp_servers,│
│  list_skills, save_persona, list_memories, tick_scheduled_agent_jobs, etc.          │
│                                                                                     │
│  AppStore (Arc<Mutex>)贯穿所有命令处理器│
└────────────┬───────────────────┬──────────────────┬────────────────┬───────────────┘│                   │                  │                │▼                   ▼                  ▼                ▼
┌────────────────┐┌───────────────────┐  ┌──────────────┐  ┌──────────────────┐
│  agent.rs      │  │  llm.rs           │  │  mcp.rs      │  │  skills.rs       │
│  (agent_loop   │  │  ├─ anthropic     │  │  子进程管理   │  │  文件系统发现     │
│  +80+子模块)  │  │  ├─ openai        │  │  HTTP会话     │  │  YAML/Markdown   │
│                │  │  ├─ bedrock       │  │  断路器       │  │  bundled catalog │
│  run_chat_turn │  │  ├─ gemini        │  │  keepalive    │  │  Agent绑定       │
│  ─────────────▶│  │  └─ responses     │  │  OAuth刷新锁  │  └──────────────────┘
│  Workflow      │  │                │  └──────────────┘
│  ├─ Planner    │  │  LlmDeltaCallback │         │
│  ├─ Executor   │  │  (Arc<dyn Fn>│  call_mcp_tool_with_retry
│  └─ Reviewer   │  │streaming SSE)  │
└───────┬────────┘└───────────────────┘
        │
        ├── store.rs(SQLite/JSON persistence: conversations, messages, memories,
        │                   agent_runs, tool_events, skills, providers, config)
        ├── models.rs       (所有 Rust 数据结构)
        ├── model_catalog.rs (模型能力/价格目录)
        ├── error.rs        (AppError / AppResult)
        ├── hermes_auth.rs  (Bitwarden/credential resolution)
        └── plugins.rs      (Python插件运行时)
```

---

## 产物 2：对话链路时序图

```
用户 → ChatExperience.tsx → store.ts → api.ts → Tauri IPC → Rust → LLM → 流式回写 → UI

1. 用户按 Enter│ChatExperience.tsx:submit()
   │  - 组装 outbound = [content, attachmentMarkers, attachmentContext].join("\n\n")
   │  - setDraft(""), setAttachments([])
   ▼
2. store.sendMessage(content, personaId, agentId)
   │  - 生成 clientMessageId = crypto.randomUUID()
   │  - 构造 optimistic user message (source="desktop")
   │  - 插入本地状态 (upsertIncomingMessage)
   │  - 调用 api.sendChatMessage({ conversationId, personaId, agentId, content, ... })
   ▼
3. Tauri invoke("send_chat_message", request)
   │  lib.rs #[tauri::command] send_chat_message
   │  - 在 tokio::spawn 中调用 agent::run_chat_turn(store, request, Some(app))
   ▼
4. agent_loop.rs:run_chat_turn()
   │  emit "synthchat-chat-event" { type: "turn_started" }
   │  → App.tsx:listen("synthchat-chat-event")
   │    showConversationProcessing(conversationId)
   │    setConversationProcessing(conversationId, true)
   ▼
5. run_chat_turn_with_app() → run_chat_turn_with_toolset_policy_and_iteration_limit()
   │  - CHAT_TURN_LOCKS 保证同一 conversation 串行
   │  - 加载 Persona / Agent / Memory / Skills / MCP tools
   │  - context_compression:检查是否需要压缩历史消息
   │  - prompt_builder: 组装 system prompt (memory + worldbook + skills + agent config)
   │  - tool_registry: 收集 internal tools + MCP tools + skill tools▼
6. WorkflowDriver (workflow_graph.rs)
   │  根据 agent配置选择模式:
   │  ChatTurn: 直接 Executor
   │  Planner+Executor+Reviewer: 三阶段工作流
   ▼
7. LLM 调用 (llm.rs)
   │  complete_chat_with_provider_failover_options()
   │  - 选择 provider (Anthropic/OpenAI/Bedrock/Gemini)
   │  - 构造 wire messages + tool schemas
   │  - 流式接收: LlmDeltaCallback(kind=Answer|Thinking, delta: &str)
   ▼
8. 流式事件回写 (agent_loop.rs desktop_visible_stream_callback)
   │  每 80ms 或 96B 批次:
   │  emit "synthchat-chat-event" {
   │    type: "assistant_stream" | "assistant_thinking_stream",
   │    conversationId, message, delta, isLast
   │  }
   │  → App.tsx:scheduleStreamMessageUpsert()
   │    →60ms debounce flush → upsertIncomingMessage(streaming=true)
   │    → ChatExperience 渲染流式气泡
   ▼
9. 工具调用 (tool_dispatch.rs)
   │  LLM 输出 tool_use block → decision_parser解析
   │  → 查找 internal_tool / MCP tool / skill tool
   │  → approval_gateway: 检查 toolApprovalMode (risky/always/never)
   │  → 执行工具 (file/browser/shell/kanban/memory/etc.)
   │  emit "synthchat-agent-run-event" { phase, toolEvent, state }
   │  → App.tsx:handleAgentRunEvent() → upsertIncomingMessage(tool message)
   │  → 工具结果注入 LLM 上下文，迭代直到 max_tool_iterations
   ▼
10. 最终 assistant消息
    │  run_chat_turn()返回 Vec<ChatMessage>
    │  emit "synthchat-chat-event" {
    │    type: "turn_finished",
    │    message: assistant_message (截断到 preview_chars)
    │  }
    │  → App.tsx:upsertIncomingMessage(message, { final: true })
    │  → scheduleChatRefresh(180ms) → api.listMessages() → 覆盖为持久化版本
    ▼
11. 持久化
    │  store.rs: save_message(user), save_message(assistant)
    │  store.rs: save_tool_event(ToolTraceEntry)
    │  memory_manager: on_memory_write() if memory tool called▼
12. UI 最终状态
    store.ts:refreshChatData() 读取 backend snapshot
    mergeBackendMessagesWithLiveState(): 防止 stale snapshot覆盖 live stream
    processingConversationIds清除 → 思考气泡消失
```

---

## 产物 3：Agent 能力地图

| 能力维度 | 实现位置 | 状态 |
|---------|---------|------|
| **LLM 模型** | `llm/anthropic_transport.rs`, `openai_transport.rs`, `bedrock_transport.rs`, `gemini_transport.rs`, `responses_transport.rs` | ✅ 5大提供商 + failover |
| **流式输出** | `agent_loop.rs:desktop_visible_stream_callback`, `LlmDeltaCallback` | ✅ 思考流+答案流分离 |
| **工具调用** | `tool_dispatch.rs`, `tool_registry.rs` | ✅ internal + MCP + skill |
| **工具审批** | `approval_gateway.rs`, `tool_policy.rs` | ✅ risky/always/never 模式 |
| **工具护栏** | `tool_guardrails.rs`, `command_guard.rs` | ✅ 文件变更 checkpoint |
| **文件操作** | `file_tools.rs`: read/write/patch/delete/move/search_files | ✅ v4a hunk patch |
| **Shell/终端** | `execution.rs`: terminal_tool/process_tool/execute_code_tool | ✅ 带危险命令检测 |
| **浏览器** | `browser_tools.rs`(nav/click/type/screenshot/snapshot), `browser_plugins.rs` | ✅ CDP |
| **Computer Use** | `computer_use.rs` | ✅ xcap屏幕截图 |
| **Web 工具** | `web_tools.rs`: web_search/web_extract/x_search | ✅ |
| **记忆** | `memory.rs`, `memory_manager.rs`: remember_fact/manage_memory/recall/external | ✅ 多记忆提供商 |
| **上下文压缩** | `context_compression.rs`:短时记忆/token budget | ✅ LLM-based summary |
| **工作流** | `workflow_graph.rs`: Planner→Executor→Reviewer 三阶段 | ✅ |
| **多Agent委托** | `delegation*.rs`, `delegation_acp.rs`, `delegation_synthchat.rs` | ✅ 并发 subagent |
| **ACP 服务器** | `acp_server.rs`, `acp_client.rs`, `acp_session.rs` | ✅ JSON-RPC 外部 agent |
| **Kanban 任务** | `kanban.rs`: create/update/complete/block/decompose | ✅ |
| **目标判断** | `goal_state.rs`, `goal_judge.rs` | ✅ LLM 评估 done/not-done |
| **定时任务** | `cron.rs`, `run_management.rs:spawn_background_chat_turn_for_job` | ✅ |
| **技能(Skills)** | `skills.rs`, `agent/skills.rs`: YAML/Markdown 文件, skill_view/manage | ✅ bundled + user |
| **Python 插件** | `shell_hooks.rs`: pre/post LLM hooks, tool transform hooks | ✅ |
| **音频/TTS/STT** | `media_tools.rs`: text_to_speech/voice_recording/transcribe | ✅ 含 native playback |
| **图像生成** | `media_tools.rs`: image_generate (via provider) | ✅ |
| **Vision** | `media_tools.rs`: vision_analyze | ✅ |
| **混合模型** | `mixture.rs`: mixture_of_agents_tool | ✅ |
| **安全扫描** | `security_tools.rs`: osv_check/security_scan | ✅ |
| **诊断** | `diagnostics.rs`: LSP diagnostics/workspace check | ✅ Go/Python/TS |
| **WeChat 集成** | `integrations.rs`, `teams_pipeline.rs` | ✅ 含 deferred turn逻辑 |
| **MCP 工具集** | `mcp.rs`: stdio + HTTP JSON-RPC, circuit breaker, keepalive | ✅ |
| **状态/Artifact** | `state_tools.rs`: checkpoint/artifact/document/todo/file_state | ✅ |
| **浏览器运行时** | `browser_plugins.rs`, `provider_plugins.rs`, `plugin_runtime.rs` | ✅ |

---

## 产物 4：当前测试覆盖地图

### 前端 Vitest 测试（`src/lib/__tests__/`）

| 测试文件 | 覆盖内容 | 质量 |
|---------|---------|------|
| `toolEventUtils.test.ts` | `parseToolEvent`, `toolEventStartKey`, `managedProcessEventLabel`, `isCanceledToolEvent`, `toolEventRank` | 充分 |
| `storeMessageMerge.test.ts` | `mergeBackendMessagesWithLiveState`: stale backend/optimistic user/streaming bubble合并 | 关键路径，有覆盖 |
| `messageRenderUtils.test.ts` | 消息渲染工具函数 | 有覆盖 |
| `messageText.test.ts` | `displayTextForMessage`, `speechTextForMessage` | 有覆盖 |
| `agentRunUtils.test.ts` | agent run 工具函数 | 有覆盖 |
| `formatters.test.ts` | `maskSecret`, `formatTime`, `providerPresetLabel` | 有覆盖 |
| `mediaUtils.test.ts` | `parseMediaSegments`, `isImagePath` | 有覆盖 |
| `personaAgentBinding.test.ts` | `resolvePersonaAgentBinding`, `resolvePersonaBoundAgent` | 有覆盖 |
| `skillSearch.test.ts` | `skillSearch` 工具函数 | 有覆盖 |
| `toolDisplayUtils.test.ts` | `compactSteps`, `parseTerminalOutput` | 有覆盖 |
| `workflowUtils.test.ts` | workflow graph 工具函数 | 有覆盖 |

**覆盖率缺口**：
- `store.ts` 除`mergeBackendMessagesWithLiveState` 外的其余 action 无测试（`bootstrap`, `sendMessage`, `refreshChatData`, `handleAgentRunEvent`）
- `api.ts` 无mock 测试
- `ChatExperience.tsx` 无组件测试
- 所有 event listener逻辑（`App.tsx` 中的 6 个 `listen` 调用）无测试

### Rust 测试（`src-tauri/src/`）

| 模块 | 测试内容 |
|------|---------|
| `agent.rs` 内联`#[cfg(test)]` | `parse_delete_memory_candidates_accepts_fenced_json` (1个) |
| `src-tauri/src/agent/tests.rs` | 模块已声明，需读取内容 |

**覆盖率缺口（Rust）**：
- `run_chat_turn` 主链路无集成测试
- LLM transport 无 mock 测试
- MCP 调用无单元测试  
- store 持久化层无测试
- `context_compression` 无测试
- tool dispatch 路由无测试
- approval gateway 无测试

---

## 产物 5：最大20 个高风险断点

### CRITICAL 级

**BP-01** `ChatExperience.tsx:submit()` —— 乐观 UI 与 stale backend竞争  
文件: `src/panels/ChatExperience.tsx:1740-1785`, `src/lib/store.ts:mergeBackendMessagesWithLiveState`
风险：用户发送消息后，refreshChatData 若比turn_finished 先返回，会用 stale snapshot 覆盖本地 user 消息，导致消息闪失。`storeMessageMerge.test.ts` 覆盖了基本场景，但不覆盖并发多轮竞争。

**BP-02** `agent_loop.rs:CHAT_TURN_LOCKS`静态 Mutex  
文件: `src-tauri/src/agent/agent_loop.rs:35-37`  
风险：每个 conversationId 有一把异步锁，若 turn中panic 导致锁泄漏，同一 conversation 后续所有 turn 都会死锁。Rust 异步锁无毒丸自动释放。

**BP-03** `run_chat_turn_with_toolset_policy_and_iteration_limit` —— 14 参数入口  
文件: `src-tauri/src/agent/agent_loop.rs:1308`  
风险：参数过多，调用侧极易漏传`None`，导致功能静默退化（如delegation 策略、iteration budget 均为 `None`）。无参数校验层。

**BP-04** `context_compression.rs:fallback_short_context_summary`  
文件: `src-tauri/src/agent/context_compression.rs`  
风险：压缩失败时的 fallback 路径（`record_summary_failure` + `summary_failure_cooldown_remaining_seconds`）在 failover 链路上无测试覆盖。若所有 provider 均失败，agent 可能因 context 超长而崩溃。

**BP-05** MCP 子进程死亡后的 circuit breaker  
文件: `src-tauri/src/mcp.rs` — `MCP_CIRCUIT_BREAKERS`, `MCP_PERSISTENT_SESSIONS`  
风险：断路器阈值=3次连续错误，冷却60秒。若 MCP 服务器在 tool 执行中途崩溃，当前 agent turn 的工具调用结果为 `Err`，但 agent 不会立即感知进程已死，需等到下次 keepalive 超时（最长 60s）。子进程僵尸泄漏风险。

### HIGH 级

**BP-06** 流式消息 debounce timer与 final事件顺序  
文件: `src/App.tsx:scheduleStreamMessageUpsert()`, `src/App.tsx:289-309`  
风险：`streamFlushTimerRef`（60ms）可能在 `final=true` 事件之后才flush，导致 final 消息先到，debounce 里的 streaming=true 消息后到，覆盖已final 状态的气泡，引发内容闪烁。代码中有 `if (options.final) flushPendingStreamMessages()` 保护，但 racing 情况下 previous.final 状态累计逻辑需验证。

**BP-07** WeChat turn 的 deferred message 逻辑  
文件: `src/App.tsx:359-401`, `activeWechatTurnRef`, `visibleWechatUserRef
