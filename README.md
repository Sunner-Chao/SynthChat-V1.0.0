# SynthChat Hermes Rust Migration

SynthChat 正在渐进迁移为前后端分离的纯 Rust Agent 架构。React UI、Rust HTTP/SSE backend 与最小 Tauri 壳分别位于独立目录；上游 Hermes Desktop/Agent 只作为行为兼容参考，不是运行时依赖。

## 目录

```text
frontend/   React 18 + TypeScript + Vite UI
backend/    Rust + axum 本地 API 与完整 Rust Agent runtime
desktop/    最小 Tauri 窗口壳和 backend 生命周期
docs/       架构、OpenAPI 契约与迁移审计
scripts/    开发、构建和检查入口
```

固定的上游参考版本见 [`docs/upstream-lock.json`](docs/upstream-lock.json)，分阶段证据与剩余门槛见 [`docs/migration-status.md`](docs/migration-status.md)。前端 API 规范见 [`docs/openapi.yaml`](docs/openapi.yaml)；已实现 terminal/process 的权限、进程状态与平台边界见 [`docs/terminal-process-contract.md`](docs/terminal-process-contract.md)。

## 当前状态

- `GET /health` 已实现且无需鉴权；standalone 开发默认示例为 `http://127.0.0.1:8642/health`，Desktop 运行时使用 OS 分配的 loopback 端口。
- `GET /api/v1/capabilities` 已实现 desktop Bearer 鉴权，并按实际实现报告能力。
- Profile/config、OS keychain secret、SQLite Session/FTS5、Hermes v21 导入、Run/SSE 文本推理、Toolset 管理、Skills 发现/搜索/启停/安装/卸载及 builtin Memory 已可用。Skills 安装与卸载通过持久 Operation、owner lease、崩溃恢复和 handle-relative 存储完成。
- 聊天工作区支持历史继续、流式文本/推理、Markdown、Token 用量、取消和 `Last-Event-ID` 重连。单 Session 的后续 Run 可进入持久 FIFO 队列；重启后 queued Run 会恢复推进，前端可发现 active Run 并从持久 SSE journal 恢复草稿、工具进度和 pending action。
- Persona、Moments 与 Worldbook 已作为 Profile-scoped Rust SQLite 产品目录接入 Desktop UI。Session 或 Run 可显式绑定 Persona；首个模型 turn 会冻结角色资料及所有启用且绑定的 Worldbook section，后续产品资料修改只影响新 Run。
- 微信设置已接入 Rust iLink adapter：支持非敏感配置、扫码登录、凭据仅写 OS keychain、同 Profile Persona 唯一绑定，以及用户显式触发的有界消息拉取和发送。后台自动轮询、自动 Session/Run 和自动回复仍保持关闭。
- Plugins 页面已接入本地 manifest-only Rust 目录，可登记、启停和移除插件 metadata；它不加载 entry point、不注入 Run 工具，也不恢复 Python/Node/旧 Agent 插件执行 runtime。
- 旧 `src-tauri` Agent runtime、旧前端 Agent IPC/event 和 bundled Python/MCP/Skills seed 已从主工程移除。
- Windows NSIS 构建已验证会打包 Rust backend sidecar，桌面退出时后端同步回收。
- Rust 工具循环已注册 Profile 隔离的 `session_search`、`skills_list`、`skill_view`、`read_file`、`search_files`、`write_file`、`patch`、`terminal`、`process`、`clarify`、`memory`、`web_search`、`web_extract` 和 `execute_code`，并持久化工具进度。四个文件工具仅在 Run 绑定到同一 Profile 下已注册且当前可用的 Workspace、同时 Profile 的 `file` Toolset 已启用时注入 Provider；`write_file`、`patch` 与 `memory` 写入还要求 durable `once/deny` 审批与单次执行 claim。
- Workspace 文件工具对路径穿越、Workspace 逸出、符号链接/重解析点逸出、Windows 非可移植路径、敏感文件、二进制/超限内容和输出上限采取 fail-closed 策略。`write_file` 采用同目录原子替换；`patch` 支持九策略 fuzzy replace 与 V4A Update/Add/Delete/Move，审批前完成全量预检，审批后复核 SHA-256 precondition，并保留 BOM、CRLF 和权限。JSON/YAML/TOML 在触盘前解析；V4A 每个文件原子提交，但跨文件 apply 失败可能留下明确报告的 partial state。
- `terminal`/`process` 属于 `terminal` Toolset；`terminal` 还要求 Run 绑定可用 Workspace，且每次执行都需 durable `once/deny` 审批。`process list/poll/log/wait` 只读，`kill/write/submit/close` 每次要求新审批。审批 UI 使用单行脱敏且带参数 hash 的知情摘要，公开 SSE/Message 只保留不含命令正文的通用摘要。当前 Session schema 为 v13：v8 引入 owner-bound approval ledger，v9 引入绑定 Run/call/checkpoint/原始参数 SHA-256 的 immutable clarification ledger，v10 为 `execute_code` 的私有 nested RPC journal 增加 immutable `origin`、`parent_call_id` 和 `rpc_sequence` 绑定，v11 引入持久 Run queue 与 epoch-fenced runtime lease，v12 为后台 terminal 的一次性异步交付增加私有持久记录，v13 为 `sessions` 与 `session_versions` 增加受约束的 `persona_id`，持久固定 Session/Run 的 Persona 归属。
- `clarifications=true`。`clarify` 请求、`waitingClarification` pending action 与 `clarification.required` 在同一事务提交；用户回答原文只进入私有 ledger 和 Provider continuation，公开 SSE、Message、Problem 与日志不回显。相同回答可幂等重放，不同回答冲突；有 choices 时必须精确匹配。取消或任意非取消终止中断都会先持久解决 clarification 并写入 `tool.failed`，再进入 Run 的取消/失败链。
- `memoryWrite=true`。builtin Memory 管理面使用 Profile 下的 `MEMORY.md`/`USER.md`、target-scoped ETag、持久幂等记录和 strict threat scan；前端可搜索、分页和 CRUD。模型写入经审批后才落盘，同一 Run 始终复用冻结快照，写入只影响下一 Run；公开 Run/SSE/Message 不含正文。
- MCP 已实现 Profile-scoped `config.yaml -> mcp_servers` 配置 CRUD，以及纯 Rust stdio、Streamable HTTP 和 legacy SSE runtime 的 `initialize`、分页 `tools/list`、动态 `mcp__<server>__<tool>` 投影和 Run 注入。远端请求逐次固定到已验证 DNS 地址、手动验证 redirect，并将 Bearer/session 限定在正确 origin；所有动态 MCP 工具要求 durable `once/deny` 审批，结果进入 Provider 前脱敏。`mcpManagement=true`，细分能力为 `mcpStdio=true`、`mcpStreamableHttp=true`、`mcpSse=true`。
- 后台进程元数据按 Profile + Session 持久化，`BEGIN IMMEDIATE` 内原子预留全局 64 个活动进程容量。guardian 在 PID/强 identity 持久化并完整校验 launch frame 后才启动 shell，script 不进入 argv；tool result 持久化后的 launch commit、父 pipe 断连回收、独立有界 stdin writer、进程树优先清理与限时 pipe drain 共同封闭常规启动/取消窗口。输出仅在内存中保留，裁剪边界带 4 KiB 脱敏 guard，backend 重启后不可恢复。
- `webSearch=true`、`webExtract=true`。Tavily Web Search/Extract 使用 Profile 独立 readiness、OS keychain 中的 `TAVILY_API_KEY`、严格参数 schema、取消/期限/并发限制、URL/全 DNS 地址预检、有界不可信输出和公开事件脱敏。Browser 是独立的本地 Chromium readiness 能力；可用时 `browserAutomation/browserCdp/browserDownloads=true`，Profile 仍须显式启用 Browser Toolset。
- `execute_code` 已实现。`codeExecution=true` 只在探测到非 WindowsApps alias 的 Python >= 3.8 时报告；可选 host Python 仅执行用户脚本，Hermes Agent 行为仍由 Rust runtime 实现。整段脚本必须先通过 durable `once/deny` 审批，审批前不会启动进程；guardian 负责取消和进程树回收。子进程使用 scrubbed environment，nested Hermes RPC 调用只写入私有 journal，代码、stdout/stderr、文件内容和 nested 参数不进入公开 Run/SSE/Message。
- 后台 `terminal` 在 `background=true` 时可二选一请求 `notify_on_complete=true` 或 1..16 个 `watch_patterns`；它们生成一次性、可重启恢复的 `tool.delivery`，公开事件不含 command、pattern 或 output。Terminal 的 Workspace 只是初始 cwd，`execute_code` 的 project/strict 模式也都不是 OS/container sandbox；审批后命令或脚本具有宿主机权限，guardian 不能回滚已发生的外部副作用或提供 exactly-once。当前拒绝 `pty=true`。`browser_download` 仅在当前 snapshot 与 durable `once` 审批后临时接收一个有界文件，完成文件名/MIME/size/SHA-256 检查后只返回元数据并删除内容；它不会自动打开或导入 Files/Workspace。完整限制见 API 与 terminal/process 契约。

## Web 联调

安装前端依赖：

```powershell
npm ci --prefix frontend
```

终端一启动 Rust backend：

```powershell
$env:SYNTHCHAT_DESKTOP_TOKEN = "01234567890123456789012345678901"
$env:SYNTHCHAT_ALLOWED_ORIGINS = "http://127.0.0.1:1421"
cargo run --manifest-path backend/Cargo.toml
```

终端二启动 UI：

```powershell
npm run dev
```

访问 [http://127.0.0.1:1421](http://127.0.0.1:1421)。

受保护 API 需要 desktop 壳签发的随机 token；浏览器单独打开只用于 `/health` 与 unavailable 状态检查。完整聊天联调使用 `npm run desktop`。

## Windows 桌面联调

以下命令先构建 backend，再由最小 Tauri 壳让 backend 原子绑定随机 loopback 端口，通过有界握手取得实际地址，并经 stdin 传递随机 session token 管理其生命周期：

```powershell
npm run desktop
```

## Windows 发布构建

```powershell
.\build-one-click.ps1
```

NSIS 产物位于 `desktop/target/release/bundle/nsis/`。构建钩子会按当前 Rust target triple 编译 backend sidecar；安装包不包含 Python、Electron、旧 MCP/TTS runtime 或历史用户数据。

```powershell
.\scripts\verify-nsis-artifact.ps1 `
  -InstallerPath .\desktop\target\release\bundle\nsis\SynthChat_1.1.0_x64-setup.exe
```

2026-07-20 历史记录中的源码曾生成并审计 26,009,305-byte 的 Windows NSIS 开发包，
SHA-256 为 `DFA82F256A0251B025BB78F68EE72FF3C1E622233DA9992D41CAF24E6AC81216`。
载荷只含 NSIS 插件、Desktop 和 Rust backend sidecar，路径/凭据扫描通过；该包
仍为 `NotSigned`，不能作为正式发布候选。

## 验证

2026-07-21 本轮非压力验证基线为 backend 517/517（library 377、backend binary 2、其余为 integration；0 failures；1 个需 `SYNTHCHAT_RUN_NATIVE_KEYCHAIN_TESTS` 明示授权的 Windows keychain test ignored）、frontend 37 个测试文件 551/551、desktop 21/21。Backend/desktop fmt、all-targets check、`clippy -D warnings`、OpenAPI lint/type drift、TypeScript/Vite production build、release-input self-check 与 `git diff --check` 均通过。本轮未重跑 Playwright、npm audit、mixed pilot 或压力/长稳；2026-07-20 的 Playwright 12/12、npm audit、30/60 分钟 mixed 结果保留为历史证据，签名/公证、跨平台原生打包和资产许可证仍是独立 release gate。

```powershell
npm run build
npm test
npm --prefix frontend run api:check
npm --prefix frontend run api:lint
npm run verify:mixed-runtime
npm run test:e2e
cargo fmt --manifest-path backend/Cargo.toml -- --check
cargo clippy --locked --manifest-path backend/Cargo.toml --all-targets -- -D warnings
cargo test --locked --manifest-path backend/Cargo.toml --all-targets
cargo fmt --manifest-path desktop/Cargo.toml -- --check
cargo clippy --locked --manifest-path desktop/Cargo.toml --all-targets -- -D warnings
cargo test --locked --manifest-path desktop/Cargo.toml --all-targets
```

Release candidates additionally require the canonical eight-hour mixed-runtime
report and reviewed manifest described in
[`docs/release-evidence/README.md`](docs/release-evidence/README.md). Daily
development checks intentionally run only the bounded verifier self-test.

## 安全提示

`synthchat-data/` 包含历史运行数据，当前已从 Git 索引移除且不得重新加入 Git 或安装包。所有曾写入仓库历史的凭据仍须轮换；历史重写与远端强制更新必须单独协调并经维护者批准。
