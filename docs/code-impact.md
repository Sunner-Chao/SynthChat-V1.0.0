# Hermes Rust 后端迁移：代码影响范围清单

- 状态：`Phase-two cleanup applied`
- 审计基线：`82cb696860d6fdd6b95fcafd901bdf7accda16f9`
- 说明：本清单以阶段一基线描述影响范围；阶段二已按本清单执行运行时代码清理。

阶段二执行结果（2026-07-16）：

- 旧 `src-tauri/` Agent crate、旧前端 IPC/event/store/panels 已从主工程移除；
- bundled Python MCP/TTS 与旧 Skills seed 已移除，默认 emoji 已迁入 `frontend/public/emoji/`；
- `synthchat-data/` 与 `.multi-agent/runs/` 已取消 Git 跟踪，磁盘副本保留；
- 新 `frontend/`、`backend/`、`desktop/` 可独立构建，Windows NSIS sidecar 打包和退出回收已验证；
- 原始 Git 历史清理与历史凭据轮换尚需维护者批准和外部协调。

## 1. 阶段一基线工程画像

阶段一基线工程不是前后端分离项目，而是一个 React/Tauri 单体：

```text
React UI
  -> src/lib/api.ts (Tauri invoke / browser mock)
  -> src-tauri/src/lib.rs (228 commands)
  -> AppStore + custom Agent/LLM/MCP/Tools
  -> synthchat-data/state.json
  -> Tauri events
  -> Zustand store/UI
```

| 项目 | 审计结果 |
| --- | --- |
| 前端 | React 18、Vite 6、TypeScript 5.7、Zustand 5 |
| 桌面 | Tauri 2，主窗口 + 独立桌宠窗口 |
| Rust | `src-tauri` 单 crate；约 100 个源码文件、11.16 MiB |
| 旧 Agent 核心 | `src-tauri/src/agent/` 共 79 文件、约 239,475 行、8.96 MiB（9.40 MB） |
| IPC 面 | Tauri 注册 228 个 command；`src/lib/api.ts` 约 210 个唯一 command 调用 |
| 前端状态 | `src/lib/store.ts` 单一巨型 store；bootstrap 并发读取 24 类资源 |
| 当前写源 | `synthchat-data/state.json`，不是 Hermes `state.db` |
| 当前 HTTP 客户端 | 无；前端没有业务 `fetch`、EventSource 或 WebSocket |
| 当前 keychain | 无 `keyring`/系统密钥链依赖；Provider key 可进入 JSON 明文状态 |

## 2. P0：迁移前必须处理的仓库数据

`synthchat-data/` 已在 `.gitignore`，但 Git 仍跟踪其中 1,732 个文件，总计 434,007,396 bytes（413.9 MiB）。目录包含：

- `state.json`、`accounts.json`、`config/providers.json` 等配置；
- 数据库、会话拆分文件和附件；
- 浏览器/工具产物；
- 662 个桌面 TTS 产物；
- 复制后的 Skills 和公共资源。

仅按字段名和非空状态审计，已确认 tracked 配置中存在非空 password/token/accessToken/refreshToken/secret/apiKey 类字段。没有读取或记录字段值。

进入阶段二前的处理顺序：

1. 假设所有进入 Git 历史的凭据已泄漏，先在各服务商处轮换/吊销。
2. 将需要保留的用户数据复制到仓库外、访问受控的位置，并验证备份。
3. 从当前索引移除运行数据；仅 `git rm --cached` 不能清除历史。
4. 使用 `git filter-repo` 等方式清理所有相关历史，并协调强制推送/重新 clone。此操作有破坏性，应单独审批和执行。
5. 增加 secret scanning、生成物检查和 CI 防回归规则。
6. 清理后再次扫描 Git 全历史，确认没有明文凭据和个人会话数据。

archive branch/tag 只能从清洗后的代码快照创建，不能保留指向含运行数据的旧 commit。若因取证或迁移必须暂存原始仓库，只能放在仓库外的加密、访问受控存储中，不能推送为 Git ref。

本阶段不自动执行上述轮换、历史重写或删除。

## 3. 后端代码分类

### 3.1 整体归档后删除

以下代码属于旧自研 Agent runtime，不应迁入新 `backend/`：

| 路径 | 处理 | 原因 |
| --- | --- | --- |
| `src-tauri/src/agent.rs` | 删除 | 旧 Agent 聚合模块和 runtime 入口 |
| `src-tauri/src/agent/**` | 整目录删除 | Agent loop、工作流、审批、工具、MCP、ACP、委派、Memory、平台适配等自研实现 |
| `src-tauri/src/llm.rs`、`src-tauri/src/llm/**` | 删除 | 旧自研 Provider transport；由新 `backend/providers` 纯 Rust 模块重建 |
| `src-tauri/src/mcp.rs` | 删除 | 旧 MCP client/session/runtime |
| `src-tauri/src/skills.rs` | 删除 | 旧 Skills 安装、配置、市场和审计实现 |
| `src-tauri/src/plugins.rs` | 删除 | 旧插件状态模型 |
| `src-tauri/src/hermes_auth.rs` | 删除 | 当前自研 Hermes/provider auth；新后端使用独立 keychain adapter |
| `src-tauri/src/model_catalog.rs` | 删除 | 当前模型目录与探测实现；新后端由 Rust provider catalog 重建 |
| `src-tauri/src/models.rs` | 删除/重建 | 类型与旧 `AppStore` 绑定，不能成为新 API DTO |
| `src-tauri/src/store.rs` | 删除/仅保留迁移读取器 | `state.json` 单体状态库，不是目标存储 |
| `src-tauri/src/threat_patterns.rs` | 随旧 Agent 删除 | 旧工具执行策略内部依赖 |
| `src-tauri/src/wechat_settings.rs` | 产品决策后删除或迁出 | 与旧 Agent/platform runtime 深度耦合 |
| `src-tauri/docs/hermes-agent-capability-audit.md` | 移入 archive | 描述的是 SynthChat MVP 仿 Hermes 实现，不是目标架构 |
| `src-tauri/docs/a2a-group-chat-integration-plan.md` | 移入 archive | 旧 Agent/A2A 计划 |

`src-tauri/src/agent/` 的内部分类：

- ACP：`acp_*.rs`；
- Agent orchestration：`agent_loop.rs`、`decision_parser.rs`、`executor_core.rs`、`run_management.rs`、`workflow_*.rs`；
- delegation/subagent：`delegation*.rs`、`teams_pipeline.rs`；
- tools/runtime：`tool_*.rs`、`execution.rs`、`file_tools.rs`、`browser_*.rs`、`computer_use.rs`、`web_tools.rs`、`media_tools.rs`；
- memory/context：`memory*.rs`、`context_*.rs`；
- integrations/dashboard/platform：`integrations.rs`、`dashboard_*.rs`、`communication.rs`、`cron.rs`、`kanban.rs`；
- tests：`agent/tests.rs` 及所有上述模块测试。

仓库已有 `agent/integrations.rs` 手写的 HTTP/SSE/WS 和 Hermes 风格路由，但它依赖旧 `AppStore` 与旧 Agent loop。可用于需求对照，不可改名冒充新后端骨架。

### 3.2 拆分后保留的桌面壳能力

`src-tauri/src/lib.rs` 同时混合 Agent 业务和桌面壳，不能直接整文件删除。以下能力应迁入精简 `desktop/` 或重建后的 `src-tauri/`：

- 主窗口、桌宠窗口创建与显示；
- tray/menu、窗口拖动、置顶、忽略鼠标事件；
- 文件/目录选择对话框；
- 打开文件、在文件管理器中定位文件；
- 原生文件拖放与窗口间事件；
- 屏幕截图（若产品确认保留桌宠视觉）；
- 应用更新、安装包拉起；
- 后端进程启动、停止、健康检查和临时 token 传递。

这些能力通过窄化的 `desktopBridge` 提供，不得继续承载 Chat、Session、Agent、Tool 或 Config 数据。

可直接移除的旧 CLI 入口：

- `src-tauri/src/main.rs` 中 ACP server/check/setup；
- MCP stdio server；
- 旧 Agent queue/platform adapter 的启动循环；
- 旧主动消息、微信轮询、工具 registry keepalive，除非产品明确要求在 Rust 扩展中重新实现。

### 3.3 新建而不是复用

建议新建独立 `backend/` Cargo package：

| 模块 | 职责 |
| --- | --- |
| `api` | `docs/openapi.yaml` 对应的 axum routes、DTO、Problem Details |
| `auth` | loopback bearer token、CORS/origin allowlist |
| `engine` | 纯 Rust inference loop、run scheduler、context、approval、event journal |
| `providers` | 模型 catalog、鉴权、HTTP streaming transport、usage |
| `compat` | 固定 Desktop/Agent 版本的配置与 SQLite 只读 importer/fixture |
| `config` | `HERMES_HOME`、Profile、YAML merge、原子写入、revision |
| `secrets` | Windows Credential Manager、macOS Keychain、Linux Secret Service |
| `session` | 自有 SQLite schema、FTS5/trigram、cursor pagination |
| `run` | per-session serialization、event normalization、重放和取消 |
| `tools` | Rust 动态 tool registry、policy、approval、执行隔离 |
| `mcp` | Rust stdio/HTTP/SSE transports 与 tool discovery |
| `skills` | Rust list/enable/install/uninstall/parser/registry |
| `memory` | builtin Markdown 与 provider capability 的兼容 DTO |
| `files` | opaque file ID、上传限制、临时文件生命周期 |

### 3.4 构建、CI 与打包配置

目录移动不是单纯复制源码。阶段二必须同步迁移以下入口，否则 Tauri 仍会从旧根目录构建或打包旧 Agent 数据：

| 当前文件 | 阶段二归属与处理 |
| --- | --- |
| `package.json`、`package-lock.json` | 前端依赖与脚本移入 `frontend/`；根 package 若保留，只作为 workspace 编排器 |
| `pnpm-lock.yaml`、`pnpm-workspace.yaml` | 当前 CI 使用 npm；阶段二确认单一包管理器后删除另一套锁文件，禁止双锁漂移 |
| `vite.config.ts`、`vitest.config.ts`、`tsconfig.json`、`index.html` | 移入 `frontend/`，修正 alias、coverage、dist 和资源路径 |
| `src-tauri/Cargo.toml`、`Cargo.lock`、`build.rs` | 精简后移入 `desktop/`；Agent 依赖不得随壳层保留 |
| `src-tauri/tauri.conf.json`、capabilities、icons | 移入 `desktop/`；`before*Command`、`frontendDist`、CSP、bundle resources 全部改为新目录 |
| `.github/workflows/ci.yml` | 拆为 frontend、backend、desktop、contract/repository-hygiene jobs，并使用各自 working directory |
| `scripts/build-windows-native.ps1`、`scripts/build-macos-native.sh` | 改为构建 backend/frontend/desktop；发布物禁止下载或打包 Python/Node Hermes runtime，并删除旧 `synthchat-data` 资源假设 |
| `build-one-click.*`、README 中构建命令 | 指向统一的无交互 orchestrator，并从干净 clone 验证 |

### 3.5 根级资源与生成物

| 路径 | 处理边界 |
| --- | --- |
| `skills/**` | 属于旧 SynthChat Skills bundle；默认从主工程删除。需迁移的内容先审计，再作为 Hermes Profile skill 或明确的 bundled skill 重新引入 |
| `data/mcp_servers/**` | 不能原样打包；有效模板转换为 backend MCP catalog，其余删除 |
| `data/tts/**` | 属于待决策 media 扩展；核心 v1 不打包 |
| `data/emoji/**` | 纯 UI 资源移入 `frontend/public/`，不进入 backend/runtime 数据目录 |
| `public/**` | 只保留 UI、桌宠、图标和 Live2D 静态资源；逐项删除旧 runtime 产物 |
| `synthchat-data/**` | 完全仓库外置，既不是 source resource，也不能进入安装包 |
| `dist/**`、`release-dist/**`、`node_modules/**` | 仅构建输出/缓存；不作为迁移输入，不得 Git 跟踪或被 Tauri resource 再打包 |

阶段二必须删除 `tauri.conf.json` 当前对整个 `../skills`、`../public`、`../data` 的宽泛映射，改成最小显式资源清单。

## 4. 前端代码分类

### 4.1 可直接保留的 UI

下列文件主要是展示或通用交互，可在移动到 `frontend/` 后保留：

- `src/styles.css`、`src/panels-beautiful.css`；
- `src/components/ErrorBoundary.tsx`；
- `src/components/PetStartupAwakening.tsx`；
- `src/panels/chat/MarkdownLite.tsx`；
- `src/panels/chat/EmojiPicker.tsx`；
- `src/panels/chat/WelcomePanel.tsx`；
- `public/pet/**`、图标、Live2D 与其他纯静态资源。

保留不代表零改动：凡是接收旧 `ChatMessage.providerData`、绝对文件路径或字符串化 ToolEvent 的组件仍需改类型。

### 4.2 保留界面、重写数据适配

| 文件/区域 | 当前耦合 | 改造目标 |
| --- | --- | --- |
| `src/lib/api.ts` | Tauri `invoke` + browser mock，`api` 为 `Record<string, any>` | 拆为生成/严格类型 REST client、SSE client、独立 `desktopBridge` |
| `src/lib/store.ts` | 单 store 混合 24 类服务端状态 | 拆成 session/chat/profile/tool/skill/settings stores；以 server events 驱动 |
| `src/lib/types.ts` | 旧 AppStore、AgentRun、Workflow 类型 | 由 OpenAPI DTO 生成；仅保留 UI view model |
| `src/App.tsx` | 消费多个 Tauri Agent 事件并轮询对账 | 使用一次 bootstrap + 单 Run SSE；仅保留桌面壳事件 |
| `src/PetWindow.tsx` | 同时消费另一套 chat delta 协议 | 订阅共享 chat store 或规范化 SSE，不再定义第二套流协议 |
| `src/components/common.tsx` | 视觉组件同时调用旧 `assetUrl/localAssetDataUrl` bridge | 保留视觉组件，文件解析改用 opaque `fileId` 或窄化 `desktopBridge` |
| `src/panels/ChatExperience.tsx` | optimistic message + IPC 返回数组 + event 合并 | `POST /sessions/{id}/runs` + stable IDs + cursor history + SSE |
| `MessageList.tsx`、`MessageRow.tsx` | `providerData: unknown`、字符串 ToolEvent | 使用结构化 MessagePart、Reasoning 和 ToolCall DTO |
| `ImagePreviewModal.tsx`、`InlineMedia.tsx` | 直接调用旧 `openLocalFile` | 保留展示层，改用 file content API；仅“在系统中打开”走 desktop bridge |
| `ToolMessage.tsx`、`ToolSteps.tsx`、`ThinkingCards.tsx` | 旧 ToolEvent/Workflow schema | 映射 `tool.*`、`reasoning.delta`、approval 事件 |
| `EnvironmentCheck.tsx` | 旧本机依赖安装命令与 mock | 映射 `/health`、runtime status、Hermes install state |
| Profile/Provider 设置 | SynthChat config schema | 映射 Hermes Profile、模型配置、keychain status |
| `McpExtensionPanel.tsx` | 旧全局/Agent MCP model | 映射 Profile 作用域 MCP server API |
| `SkillsCenterPanel.tsx` | 旧 Agent 绑定和市场 DTO | 映射 Profile 作用域 Skills 与异步安装状态 |
| Memory UI | 自研结构化 MemoryEntry | 显示 provider capabilities；兼容 builtin Markdown CRUD |

当前聊天有两套不兼容流语义：主窗口消费累计快照型 `assistant_stream`，桌宠还消费 delta 型 `assistant_stream_delta`。新实现必须统一为只发送增量的 `message.delta`，由 store 累加。

### 4.3 删除或彻底重写的前端 Agent 语义

- `src/panels/AgentsManagerPanel.tsx`：Hermes 的执行隔离单元是 Profile，不是现有 `AgentDefinition`；应改为 Profile 管理页或删除。
- `src/panels/settings/AgentSettings.tsx`、`AgentSettingsRedirect.tsx`：旧 Agent 配置。
- `src/lib/agentRunUtils.ts`：旧 run lifecycle。
- `src/lib/workflowUtils.ts`：自研 workflow graph 在 Hermes API 中无 1:1 对应。
- `src/lib/personaAgentBinding.ts`：现有 Persona 与 Agent 独立组合，Hermes Profile/SOUL 模型不支持原样映射。
- `src/lib/toolEventUtils.ts`：不再把 JSON 序列化到 `ChatMessage.content`。
- 对应 `agentRun*`、workflow、personaAgentBinding、旧 stream merge 测试；用契约测试替换。

阶段一基线路由当时仍使用 `src/panels/ToolPanels.tsx` 中的 Memory、Worldbook 与 Plugins，因此该文件不能在清理开始时整文件删除；其中未被路由引用的旧 `McpPanel`、`AgentsPanel`、`SkillsPanel` 应先拆分再删除。该旧文件及路由现已由新 `frontend/` 工作区替代。

## 5. 阶段一 UI 功能与当前能力映射

下表前两列保留阶段一的功能审计语义；“建议”和“评审状态”已按 2026-07-21 的 Rust 实现决议更新。

| 当前功能 | Hermes 对应 | 建议 | 评审状态 |
| --- | --- | --- | --- |
| Chat、历史、Markdown、Tool progress | 有 | 保留并对接 Run/SSE | 可进入实现 |
| Agent 管理 | Profile + SOUL + model | 改名/改造为 Profile | 待确认文案与迁移 |
| Persona | SOUL/Profile 部分对应 | 迁入 Rust 产品目录，以 Session/`CreateRun.personaId` 绑定并冻结注入 Run | 已实现 |
| Memory | builtin Markdown + providers | 保留，但按 provider capabilities 降级 UI | 可进入设计 |
| MCP | 有 | Profile 作用域重写 | 可进入实现 |
| Skills | 有 | Profile 作用域重写，安装异步化 | 可进入实现 |
| Providers/model | 有 | 映射 Hermes config + keychain | 可进入实现 |
| Worldbook | 无直接等价 | 作为 Rust 产品目录；启用且绑定 Persona 的 section 随角色快照注入 Run | 已实现 |
| Moments | 无；阶段一 API 是内存 mock | 作为不主动触发模型的独立 Rust 产品域 | 已实现 |
| 微信账号绑定 | Hermes 有消息平台，但模型不同 | Rust iLink adapter 提供配置、扫码、Persona 唯一绑定和显式 poll/send；自动 Run 关闭 | 已实现显式适配器 |
| 主动消息 | Hermes cron/messaging 可组合 | 重新设计，不能沿用旧 Agent loop | 延后 |
| 桌宠窗口/动画 | 无后端依赖 | 保留在 desktop/UI |
| 桌宠视觉识别 | 可借助 vision tool，但非标准 Desktop 行为 | 单独扩展，默认延后 | 阻塞决策 |
| 语音/STT/TTS | Hermes 有相关能力 | 作为可选 tool/media 扩展 | 延后 |
| Plugins 页面 | Hermes Dashboard plugins 不等于当前插件模型 | 重定义为本地 manifest-only Rust catalog；不执行插件代码 | 已实现 metadata 管理 |
| 主题/emoji | UI 域 | 移到前端本地持久化，不进 Agent API | 可进入实现 |

## 6. 类型和协议迁移

| 当前类型/行为 | 新契约 |
| --- | --- |
| `Conversation` | `Session`，强制包含 `profileId`、cursor、messageCount |
| `ChatMessage.content: string` | `Message.parts[]`；文本、文件和工具结果结构化 |
| `providerData: unknown` | 删除；字段进入显式 DTO 或后端私有扩展 |
| `AgentDefinition` | 删除；使用 `Profile` + `ProfileConfig` |
| `Persona.agentId` | 不再作为运行时路由键；产品确认后迁移到 Profile/SOUL |
| `AgentRunRecord/WorkflowGraph` | `Run` + versioned SSE events |
| `ToolEventEnvelope` JSON 文本 | `tool.started/progress/completed/failed` 事件和 `ToolCall` |
| Tauri `synthchat-chat-event` | `/api/v1/runs/{runId}/events` SSE |
| 多个 run/queue/tool events | 同一 Run SSE；非 Run 资源通过专用刷新或全局事件流扩展 |
| `listMessages(limit)` 逐步放大 | opaque cursor pagination |
| 本地标题 substring 搜索 | `/sessions?q=`，SQLite FTS5/LIKE fallback |
| JSON byte array 上传 | multipart upload + opaque `fileId` |

## 7. 依赖影响

### 7.1 现有 Rust 依赖去向

| 依赖组 | 处理 |
| --- | --- |
| `tauri`、`tauri-build`、`tauri-plugin-dialog`、`embed-resource`、`windows-sys` | 仅按实际窗口/托盘/对话框能力保留到 `desktop/` |
| `serde*`、`thiserror` | desktop 保留最小集合；backend 在自己的 manifest 重新声明 |
| `tokio`、`reqwest`、`futures`、`serde_yaml`、`chrono`、`uuid`、`dirs` | 从旧 crate 移除，按 adapter 的实际需要加入 `backend/` 并锁版本 |
| `tokio-tungstenite` | 旧 TUI/平台 WebSocket 适配随旧 runtime 删除；仅未来明确的外部 connector 需要时重新评审 |
| `image`、`xcap` | 只有产品确认保留桌宠视觉/截图后才留在 desktop extension；否则删除 |
| `qrcode`、`lettre`、`imap`、`mailparse`、`native-tls`、`tokio-native-tls`、`rsa` | 旧 crate 中的登录、邮件、微信/平台依赖已删除；后续 Rust 微信 adapter 仅以 SVG feature 重新声明 `qrcode 0.14`，其余旧邮件/传输依赖不恢复 |
| `aes*`、`ctr`、`cbc`、`hmac`、`md-5`、`sha1` | 随旧自研密钥/协议实现删除，不得替代 OS keychain |
| `zip`、`base64`、`rand`、`hex`、`sha2` | 不从旧 crate 惯性保留；若新上传摘要、token 或安装器确有需要，由对应新 crate 以最小 features 重新加入 |

移动完成后对 `backend/` 和 `desktop/` 分别运行未使用依赖检查（如 `cargo machete`）及 `cargo tree -d` 审查；“Cargo 能编译”不能作为保留旧依赖的理由。

### 7.2 前端移除/新增

- Agent 数据层不再直接使用 `@tauri-apps/api/core.invoke` 和 Agent 事件；
- 保留 Tauri 窗口/事件依赖给 `desktopBridge`；
- 从 `docs/openapi.yaml` 生成 TypeScript client，或在生成器落地前维护严格 interface；
- SSE 使用基于 `fetch` 的 parser，以便发送 Authorization header 和支持 AbortController；原生 EventSource 不支持自定义 Authorization header。

### 7.3 Rust 后端新增候选

- `axum`、`tokio`、`tower-http`；
- `serde`、`serde_json`、`serde_yaml`；
- `reqwest` + SSE parser；
- `rusqlite`（bundled FTS5 需跨平台验证）或 `sqlx`；
- `keyring`；
- `uuid`、`time`/`chrono`、`tracing`；
- `secrecy`/`zeroize`；
- `tempfile`、文件锁与原子写辅助。

依赖版本在阶段二创建 Cargo workspace 时锁定，不在阶段一预写未经构建验证的版本号。

## 8. 阶段二采用的渐进清理顺序

以下是阶段二当时采用的执行顺序：先建立可运行的新骨架，再在该阶段内一次性将旧 Agent runtime 移出主工程。当时尚未由新后端实现的界面显示明确 unavailable 状态，不继续调用旧 runtime，也不以 mock 成功冒充能力；后续阶段已逐域替换这些状态。旧实现只存在于清洗后的 archive ref。

1. 完成凭据轮换、仓库外备份和 Git 全历史清理。
2. 从清洗后的基线创建 archive tag/branch，记录数据迁移策略；不得创建指向未清洗历史的远程 ref。
3. 新建 `frontend/`、`backend/`、`desktop/`，同步迁移 package/Cargo/Tauri/CI/打包配置，保持现有壳和 UI 可启动。
4. backend 实现 `/health`、bearer 鉴权和最小 capabilities；desktop 实现单实例、backend 生命周期和内存 token bridge。
5. frontend 切换到严格 API client，完成 health 联调；所有尚未实现的 Agent 面板进入显式 unavailable 状态。
6. 删除旧 Rust Agent/LLM/MCP/Skills/Store、旧 IPC/events、前端 Agent lifecycle 和相关依赖；移除宽泛 bundle resources。
7. 完成三端构建、契约、仓库卫生和打包 smoke test，满足阶段二退出门槛。
8. 阶段三按 Profile/config/keychain、Session/FTS、Run/SSE、Toolset/Skills/MCP/Memory 的顺序逐域恢复真实能力；每个域完成即做端到端测试。

## 9. 阶段二清理验收

结构与行为：

- `backend/` 可独立构建和启动；`GET /health` 为 200，受保护 endpoint 无 token 为 401；
- `frontend/` 不导入 Agent 业务型 Tauri `invoke`/events，可展示 backend 状态与未实现功能的明确 unavailable 状态；
- `desktop/` 只含窗口、托盘、对话框、文件壳能力、单实例和 backend 生命周期，不含 LLM、Agent、MCP、Skills、Memory 业务；
- 旧 `src-tauri/src/agent/**`、相关顶层模块、旧 IPC/event 名称和旧 Agent 前端语义只存在于清洗后的 archive，不存在主工程；
- CSP 只允许所需 loopback origin，backend CORS allowlist 接受 Tauri/指定开发 origin 并拒绝其他 origin；
- OpenAPI path/method 与实际 route 通过自动契约测试一致；未决定或未实现功能不得返回 mock success。

仓库与安全：

- `git ls-files synthchat-data` 无输出，`git log --all -- synthchat-data` 在重写后的 refs 中无记录；
- 对所有 refs 执行 `gitleaks git --log-opts="--all"` 或等价全历史扫描并保存通过报告；
- CI denylist 阻止 `synthchat-data`、数据库、会话、附件、日志、TTS 产物、`.env` 和生成目录重新进入索引；
- archive ref 已清洗；任何原始备份仅位于仓库外的加密受控存储；
- Tauri resource 清单不再包含整个 `skills/public/data`，安装包扫描不含用户数据、明文凭据或旧 Agent 资源；
- `backend/` 与 `desktop/` 的 manifest 通过未使用依赖检查，无旧邮件、ACP、自研加密或 Agent transport 依赖残留；当前 `qrcode` 与 HTTP 依赖属于重新设计的纯 Rust 微信 adapter，不是旧微信/Agent runtime 残留。

阶段二 CI 命令矩阵：

| 域 | 必须通过的命令/检查 |
| --- | --- |
| frontend | `npm --prefix frontend ci`、`npm --prefix frontend run build`、`npm --prefix frontend test` |
| backend | `cargo fmt --manifest-path backend/Cargo.toml -- --check`、`cargo check --manifest-path backend/Cargo.toml`、`cargo clippy --manifest-path backend/Cargo.toml --all-targets -- -D warnings`、`cargo test --manifest-path backend/Cargo.toml` |
| desktop | `cargo fmt --manifest-path desktop/Cargo.toml -- --check`、`cargo check --manifest-path desktop/Cargo.toml`、`cargo clippy --manifest-path desktop/Cargo.toml --all-targets -- -D warnings` |
| contract | `npx --yes @redocly/cli@latest lint docs/openapi.yaml --extends=minimal`，以及 route/path、401、CORS 自动测试 |
| hygiene | CI `repository-hygiene` job、全历史 secret scan、`cargo machete`/依赖审查 |
| package | Windows/macOS/Linux 至少执行对应 preflight；发布平台执行完整 bundle smoke test，并扫描产物资源清单 |

以上命令必须进入 CI，而不是仅在开发机手工运行。
