# SynthChat Hermes Backend API Contract v1

- 状态：`Draft for review`
- 机器可读定义：[`openapi.yaml`](./openapi.yaml)
- 基础地址：`http://127.0.0.1:8642`
- 公共前缀：`/api/v1`

本文是前端与纯 Rust 后端之间的规范。上游 Hermes Agent 的 `/v1/*`、Dashboard `/api/*`、TUI JSON-RPC、SQLite schema 和 CLI 仅作为兼容参考或一次性迁移输入，既不是生产内部 transport，也不得由前端直接依赖。

## 1. 契约原则

1. JSON 字段统一 `camelCase`；上游 `snake_case` 只存在于 adapter 内。
2. 所有 ID 都是 opaque string。前端不得解析 ID 前缀、时间或数据库主键。
3. 时间统一为带时区的 RFC 3339 字符串。
4. 除 `GET /health` 和浏览器 CORS `OPTIONS` 预检外，所有请求都必须携带 `Authorization: Bearer <desktop-session-token>`。
5. Secret write-only：请求可提交 secret，任何响应、日志、SSE 和错误都不得返回 secret value。
6. 历史与可增长流列表统一 cursor pagination；Profiles、Provider catalog 和每类硬上限 2,000 条的本地产品目录是明确例外。
7. OpenAPI 标记需要跨重试去重的创建操作必须使用显式 `Idempotency-Key`；条件写使用 ETag/`If-Match` 防止覆盖。产品目录创建当前不承诺重放幂等，前端必须抑制重复提交。
8. Chat 使用 REST 创建 Run、SSE 接收事件、REST 回答 approval/clarification。v1 不公开 WebSocket；纯 Rust backend 也不依赖上游 `/api/ws`。
9. 前端只处理本契约定义的 DTO，不读取 `providerData: unknown` 或字符串化 ToolEvent。

## 2. 鉴权与本地通信

- 生产只监听 `127.0.0.1`，不得绑定 `0.0.0.0`。
- Tauri 壳每次启动生成至少 256-bit 随机 desktop session token，通过继承 pipe 交给 Rust backend，再通过窄化 `desktopBridge` 交给前端；重启后旧 token 失效。
- token 只保存在各进程内存，不写入命令行、localStorage、URL、磁盘、日志或崩溃报告。browser-only 开发通过 Vite loopback proxy 注入 Node 进程内 token，不提供生产免鉴权开关。
- CORS 只允许打包后的 Tauri origin 与明确配置的开发 origin。
- SSE 使用基于 `fetch` 的客户端，以便发送 Authorization、`Last-Event-ID` 和 AbortSignal；不要使用无法设置 Authorization header 的原生 EventSource。
- `GET /health` 只返回版本和状态，不返回路径、配置、端口、Profile 或错误堆栈。

## 3. 通用 HTTP 约定

### 3.1 Headers

| Header | 用途 |
| --- | --- |
| `Authorization` | 除 `/health` 和 CORS `OPTIONS` 预检外必需 |
| `X-Request-Id` | 客户端可传；错误时以 `Problem.requestId` 返回，服务端也可回传同名响应头 |
| `Idempotency-Key` | OpenAPI 标记该 header 的创建操作必须提供 |
| `If-Match` | Session PATCH/DELETE、Profile metadata PATCH、config/toolset/skill PATCH、全部 Memory 写操作，以及产品目录更新/删除/评论/点赞的条件写 revision |
| `ETag` | 单资源、条件写成功响应和 Memory page 的带引号 strong revision；产品目录的单项 GET、创建和成功 mutation 返回对应 ETag，列表只在各 item body 返回数值 `revision` |
| `Last-Event-ID` | Run SSE 断线重放 |

### 3.2 分页

```json
{
  "items": [],
  "nextCursor": "opaque-or-null"
}
```

- `limit` 默认 30，最大 100；
- cursor 与筛选条件绑定，变更 `profileId`、`q` 或排序后必须从头请求；
- cursor 是不可解析、可防篡改且不包含正文的服务端 token；格式错误、篡改、过期或用于不同筛选条件时返回 400 `invalid_cursor`；
- Memory cursor 还绑定 target revision；分页期间 canonical 文件发生变化时返回 409 `revision_conflict` 并携带当前 ETag，不能把两个 revision 的条目拼成一页链；
- 无下一页时 `nextCursor` 为 `null`。

### 3.3 错误

错误使用 `application/problem+json`：

```json
{
  "type": "urn:synthchat:error:profile-not-found",
  "title": "Profile not found",
  "status": 404,
  "detail": "The requested profile does not exist.",
  "instance": "/api/v1/profiles/work",
  "code": "profile_not_found",
  "requestId": "req_...",
  "retryable": false
}
```

稳定错误码至少包括：

| HTTP | code | 含义 |
| --- | --- | --- |
| 400 | `validation_failed` | 请求字段错误 |
| 400 | `invalid_if_match` | `If-Match` 不是单个带引号的 strong ETag |
| 400 | `invalid_cursor` | cursor 无效、已过期或与本次筛选条件不匹配 |
| 401 | `unauthorized` | token 缺失或失效 |
| 404 | `resource_not_found` | 资源不存在或不属于当前 Profile |
| 404 | `product_not_found` | Persona、Worldbook、Moment 或 Moment comment 不存在 |
| 404 | `approval_not_found` | approval 不存在或不属于指定 Run |
| 409 | `revision_conflict` | ETag 过期 |
| 409 | `idempotency_conflict` | 同一幂等键被用于不同请求内容 |
| 409 | `idempotency_resource_gone` | POST 幂等记录指向已删除的 MCP 等资源，不得复活 |
| 409 | `mcp_server_exists` | 同一 Profile 已存在同名 MCP server |
| 409 | `mcp_config_invalid` | 磁盘中的 MCP 配置无法安全投影，例如含明文 env/header secret |
| 409 | `memory_storage_drift` | builtin Markdown 不能无损 round-trip，写入被拒绝以避免覆盖外部修改 |
| 410 | `idempotent_resource_deleted` | 幂等创建对应的资源已被显式删除，不得复活 |
| 409 | `profile_delete_conflict` | 尝试删除 default 或 active Profile |
| 409 | `session_busy` | 当前策略不允许排队 |
| 409 | `session_archived` | 归档 Session 在恢复前不能创建新 Run |
| 409 | `event_history_expired` | SSE 重放窗口已过期 |
| 409 | `approval_choice_not_offered` | 决策不在当前 pending approval 的 `choices` 中 |
| 409 | `approval_decision_conflict` | 同一 approval 已持久化不同的决策 payload |
| 409 | `approval_expired` | approval 已按服务端时钟过期并 fail closed |
| 409 | `approval_no_longer_pending` | 取消或其他状态迁移已使 approval 不再可决策 |
| 404 | `hermes_state_not_found` | Profile 下不存在可导入的 Hermes `state.db` |
| 409 | `hermes_import_source_changed` | POST 指定的预检快照已发生变化 |
| 409 | `hermes_import_conflict` | 来源映射或目标 Session 已变化；整个导入未写入 |
| 413 | `payload_too_large` | 文件或请求体超限 |
| 413 | `product_catalog_limit` | 某 Profile 的某类产品条目已达 2,000，或某 Moment 已达 1,000 comments |
| 413 | `hermes_import_too_large` | Hermes 快照超过固定导入上限 |
| 415 | `unsupported_media_type` | 请求没有且仅有一个 endpoint 要求的 Content-Type |
| 422 | `engine_capability_missing` | 当前 Rust engine 能力未实现或未启用 |
| 422 | `provider_configuration_invalid` | 当前 Profile 的 Provider、模型或 Base URL 配置无效 |
| 422 | `memory_provider_unsupported` | Profile 当前不是 builtin Memory provider；管理路由不会模拟外部 provider CRUD |
| 422 | `memory_content_blocked` | Memory 内容命中 strict prompt-injection/exfiltration threat scan |
| 422 | `memory_capacity_exceeded` | 规范化后的完整 target 超过当前字符预算 |
| 422 | `hermes_schema_unsupported` | `state.db` schema 不是锁定的 v21 |
| 422 | `hermes_import_source_invalid` | `state.db` 缺列、损坏或含无效值 |
| 422 | `hermes_attachments_require_policy` | 快照含附件，但请求未显式允许省略 |
| 422 | `session_search_unavailable` | 本地会话搜索能力尚未初始化 |
| 428 | `precondition_required` | 条件写缺少 `If-Match` |
| 429 | `capacity_exceeded` | Run 并发/队列上限 |
| 429 | `provider_rate_limited` | 外部模型 Provider 对请求限流 |
| 502 | `engine_unavailable` | Rust inference engine 或外部 Provider 不可用 |
| 502 | `provider_authentication_failed` | 外部模型 Provider 拒绝了配置的凭据 |
| 502 | `provider_request_rejected` | 外部模型 Provider 拒绝了模型或请求参数 |
| 502 | `provider_stream_failed` | 外部模型 Provider 在流式过程中返回错误 |
| 502 | `provider_response_invalid` | 外部模型 Provider 返回了不完整或不兼容的流 |
| 503 | `secret_storage_unavailable` | OS keychain 被锁定、不可用或拒绝访问 |
| 503 | `session_storage_busy` | SQLite 超过 busy timeout，客户端可退避重试 |
| 503 | `session_storage_unavailable` | 会话库未初始化、损坏或迁移失败 |
| 503 | `hermes_state_unavailable` | Hermes 来源库暂时无法只读打开或读取 |
| 503 | `product_catalog_unavailable` | 本地产品目录 SQLite 无法初始化、加锁或提交事务 |
| 504 | `engine_timeout` | Rust engine 等待外部 Provider/tool 超时 |

`detail` 必须经过脱敏，不得包含上游响应正文中的 key、token 或本地绝对路径。OpenAPI response component 的 `x-error-codes` 是该 HTTP 状态当前已锁定值的非穷尽机器可读列表；未来可新增错误码，但不得改变既有码的语义。

幂等约定：

- `POST Profile`、`POST Session`、Hermes Session 导入、文件上传、创建 Run、安装 Skill、创建 Memory 和创建 MCP server 必须携带 `Idempotency-Key`；
- key 的作用域是本机安装实例内的 `(HTTP method, canonical path, key)`，服务端至少持久保留 24 小时并跨后端重启生效；
- 同一 key 与相同请求指纹重试时返回同一资源/Operation ID，不得重复产生用户消息、文件或配置项；
- 同一 key 与不同请求指纹重用时返回 409 `idempotency_conflict`；multipart 请求指纹包含文件内容摘要和非文件字段；
- 未要求 key 的 PUT/PATCH/DELETE 或 approval/clarification action 必须按资源 ID 实现幂等重复提交。

## 4. Endpoint 总览

### 4.1 System

| Method | Path | 说明 |
| --- | --- | --- |
| GET | `/health` | Rust 服务健康；无需认证 |
| GET | `/api/v1/capabilities` | Rust contract、engine 和已实现功能 |
| GET | `/api/v1/bootstrap` | 一致性首屏快照：runtime、profiles、active profile、sessions 首屏 |
| GET | `/api/v1/providers` | 可配置 Provider catalog；不包含 key |
| GET | `/api/v1/web/providers` | 已由 Rust 实现的 Web provider catalog；当前只含 Tavily，不包含 key |
| GET | `/api/v1/operations/{operationId}` | 查询 Skill 安装/卸载等异步操作 |

`capabilities.engine.features` 是稳定布尔集合：`runStreaming`、`reasoningStreaming`、`toolProgress`、`approvals`、`clarifications`、`asyncToolDelivery`、`profileManagement`、`skillManagement`、`memoryWrite`、`mcpManagement`、`oauthAccounts`。值为 false 时 UI 必须禁用对应操作并说明不可用，不能通过探测隐藏 endpoint 猜测能力。

`capabilities.extensions` 当前必须包含 `activeRunDiscovery`、`runQueue`、`toolsetManagement`、`toolExecution`、`codeExecution`、`workspaceManagement`、`skillDiscovery`、`skillEnablement`、`webSearch`、`webExtract`、`browserAutomation`、`browserCdp`、`browserDownloads`、`mcpStdio`、`mcpStreamableHttp`、`mcpSse`、`wechatAccounts`、`wechatMessaging`、`plugins`、`personas`、`moments` 和 `worldbooks`。`personas/moments/worldbooks=true` 表示下述 Profile-scoped Rust 产品目录及条件写路由可用；Persona 可通过 `CreateRun.personaId` 参与 Run，启用且绑定的 Worldbook section 随 Persona 快照注入，Moments 仍不主动触发模型。`wechatAccounts=true` 表示配置、扫码、Persona 唯一绑定和 keychain-backed account metadata 可用；`wechatMessaging=true` 表示显式、有限且脱敏的 poll/send adapter 可用，不表示后台轮询或自动消息到 Run。`plugins=true` 只表示本地 manifest 登记、启停和移除登记可用，绝不表示执行插件代码。`codeExecution=true` 只在 Rust Run engine 可监督 host Python 3.8+ 子进程并完整执行本文的审批、环境剥离、动态 RPC 白名单、取消和输出边界时返回；它不表示 OS/container sandbox，也不表示 Python Hermes Agent 成为运行时依赖。Web-first 实现通过路由、Rust executor、安全边界和 Run E2E 验收后，`webSearch=true`、`webExtract=true`；这两个值只表示 engine 已实现对应域，不表示任意 Profile 已选择可用 provider，Profile 实际状态必须读取 `WebConfig.effectiveSearch/effectiveExtract`。`mcpStdio=true`、`mcpStreamableHttp=true` 和 `mcpSse=true` 分别表示对应 transport 已通过真实 fixture 与 Run E2E；它们不表示每个 Profile 的远端 URL、secret 或网络均处于可用状态。`browserAutomation`、`browserCdp` 与 `browserDownloads` 只有在 Rust Run engine 可用且本机发现受支持 Chromium-family binary 时才同为 true；它们表示 per-Profile/Run 隔离浏览器、loopback CDP、受控 egress proxy、bounded AX/image output、owner-bound approval 及下述隔离下载元数据工作流已可用，不表示任意浏览器、自动打开或 Workspace 文件写入权限。

Run 始终只发送“Profile 已启用、Rust registry 已注册且全部先决条件满足”的严格工具定义。现有 executors 是 Profile 隔离的 `session_search`、`skills_list`、`skill_view`、`read_file`、`search_files`、`write_file`、`patch`、`terminal`、`process`、`clarify`、`memory`、`web_search`、`web_extract`，以及 Browser 的 `browser_navigate`、`browser_snapshot`、`browser_click`、`browser_download`、`browser_type`、`browser_scroll`、`browser_back`、`browser_press`、`browser_get_images`、`browser_vision`、`browser_console`、`browser_cdp`、`browser_dialog`，并包括动态发现的 `mcp__<server>__<tool>`。Browser definitions 还要求 runtime Browser readiness；`navigate/snapshot/images/console` 是有界读取，click/download/type/scroll/back/press/dialog 以及限定为 `Runtime.evaluate` 的 CDP 均要求当前 snapshotId、一次 durable `once/deny` approval 和同 Run/Profile/Session/Call/参数 SHA-256 claim。`execute_code` 仅在 host Python 3.8+ 可用且 `code_execution` Toolset 已启用时注入，schema 描述只列出同一 Run 实际可用的七类内部 RPC 工具子集，刻意不包含 Browser RPC。`code_execution` 的 `Toolset.configured=true` 只表示当前后端进程已检测到可用 host Python 3.8+；UI 仍必须同时要求全局 `capabilities.extensions.codeExecution=true` 与 Profile 的 `enabled=true`，不得把 `configured` 单独解释为可执行。四个文件工具只在 Run 绑定到同一 Profile 下已注册且当前可用的 Workspace、并且 Profile 的 `file` Toolset 已启用时注入；Web 工具还要求对应 provider ready。`write_file`、`patch`、`terminal`、`memory`、整个 `execute_code` 脚本和所有动态 MCP 工具要求 durable `once/deny` 审批。Skills 列表/搜索、启停和带持久 Operation 的安装/卸载可用，因此 `skillManagement=true`。Workspace 注册只接受一次性的绝对路径写入，Rust canonicalize 后内部保存，响应只返回 opaque ID/显示名/可用状态。`approvals=true`、`clarifications=true`、`memoryWrite=true`、`activeRunDiscovery=true`、`runQueue=true`、`mcpManagement=true`、`asyncToolDelivery=true`；`oauthAccounts` 继续为 false。

`browser_download` 输入必须且只能包含 `selector` 与当前 `snapshotId`。审批 claim 成功前 Chromium 始终保持 `Browser.setDownloadBehavior=deny`；批准后只对本次动作短暂切换为 `allowAndName`，目标是由 Profile/Session/Run owner 唯一持有的随机私有临时目录。每次只接受一个扁平普通文件，单文件上限 8 MiB、单 Run 累计上限 16 MiB 且最多四次；建议文件名必须是 1..128 字符的单一非隐藏名称，禁止 `/`、反斜杠、冒号、控制字符、路径穿越、symbolic link 和 Windows reparse point。只接受扩展名与 magic/UTF-8 一致的 PDF、PNG/JPEG/GIF/WebP、ZIP/DOCX/XLSX、JSON 及文本类 MIME；未知或 executable/binary 内容 fail closed。完成后以 no-follow capability handle 复核精确目录内容、长度、MIME 和 SHA-256，公开/Provider 投影只含 `name/mimeType/sizeBytes/sha256/scan`，绝不含绝对路径、文件正文、可打开 URI、Files ID 或 Workspace path。成功、拒绝、超额、取消、deadline、CDP 错误均先恢复 `deny`，再删除临时内容；Run 终态清理 Browser 进程和目录，backend 启动仅回收递归验证无 link/reparse 的 stale run 目录。当前没有下载内容导入 API，也不宣称 malware/antivirus 扫描。

`execute_code` 锁定 pinned Hermes 的 Python programmatic-tool-calling 语义：输入必须且只能含 1..60 KiB 的 `code`，默认 300 秒、50 次内部工具调用、50 KB stdout 和 10 KB stderr；Profile `code_execution.mode` 为 `project|strict`，并通过 ProfileConfig 的 `codeExecution` 读写。Rust backend 仅接受探测成功的 Python 3.8+ executable，并拒绝 WindowsApps alias；该 host interpreter 只执行用户批准的代码工具，不承载 Hermes Agent runtime。脚本写入私有临时 staging，经 direct guardian 启动；`project` 在有 Workspace 时以其为 cwd，`strict` 以 staging 为 cwd。子进程环境从最小 allowlist 重建，不继承 API key、token、secret、password、credential、auth、DSN 或 webhook 类变量。一次性 loopback RPC port/token 通过 guardian 的强类型 bootstrap 单独注入，generic environment 不接受这些凭据；RPC 服务端强制动态工具白名单、参数 schema、64 KiB request/response 和调用次数。脚本可直接使用 Python 文件、网络、`subprocess`/`ctypes` 等宿主能力，因此这两个 mode 都不是安全沙箱；durable `once` 审批覆盖整段脚本及其中的 nested mutating RPC，审批/claim 前不得启动 guardian，deny 不得产生 nested invocation。内部受管工具仍执行现有路径、SSRF、硬拒绝命令、deadline、取消和输出规则，但不会创建第二个公开审批。

Session schema v10 引入并由当前 v13 保留的边界把顶层 Provider 调用与 code RPC 子调用分开：`tool_invocations.origin` 只能是 `provider|codeRpc`；`codeRpc` 必须以 `parent_call_id` 绑定同 Run/turn 中仍在 running 的 Provider `execute_code`，并以 1..100 的 `rpc_sequence` 严格递增，三项绑定创建后不可变。nested 参数、结果和 checkpoint 只进入私有 journal；Provider continuation 的顶层 tool-call 列表只读取 `origin=provider`，公开 Run/SSE/Message 同样不投影 nested row。stdout 使用 50 KB head-tail retention，stderr 独立使用 10 KB head-only retention，二者经 ANSI/控制字符清理及 Profile secret/token/Bearer 脱敏后才写入私有结果；公开面只保留有界摘要与参数 digest。cancel、deadline、backend disconnect 或 RAII drop 都要求 guardian 收敛 Job Object/process group 中的受管 Python 树；Run 已是 `cancelling` 时只允许 invocation 失败终结，晚到成功不得覆盖取消。

Workspace 工具只允许相对路径下的有界访问：Workspace 注册层打开唯一 ambient root authority，后续目录和文件访问全部经 `cap-std` capability、逐级 no-follow 目录句柄及最终 no-follow 文件句柄完成；路径穿越、Workspace 逸出、符号链接/重解析点逸出、Windows 非可移植组件和敏感文件均被拒绝或排除。`read_file` 拒绝二进制、非 UTF-8 或超过 2 MiB 的内容，`search_files` 排除二进制、非 UTF-8 或超过 1 MiB 的候选内容，并受 10,000 条目、16 MiB 扫描量、100 个结果及 60 KiB 输出上限约束，截断时显式分页。`write_file` 接受不超过 60 KiB 的 UTF-8 文本。`patch` 接受 pinned Hermes 的 `mode=replace|patch`：replace 使用九策略 fuzzy matcher；V4A 支持 Update/Add/Delete/Move、最多 64 个 operation/256 个 hunk、单目标 2 MiB 和聚合快照 16 MiB。Add 采用本地更安全的 must-not-exist 规则。JSON/YAML/TOML 候选在触盘前 fail closed 校验；BOM、CRLF 与既有权限被保留。

每个 `write_file`/`patch` 调用在审批前生成仅驻内存的参数绑定 plan 与 Existing/Missing SHA-256 precondition，不产生副作用。只有持久化 `once` 决策的单次 claim CAS 成功后，执行器才会获取全部目标的排序锁、复核原始参数 hash 和所有 precondition，再逐文件提交并复读验证；`deny`、过期、取消、外部变化和重启恢复均在首个提交前 fail closed。V4A 保证全批预检，但不宣称跨文件事务：后续单项提交失败时，provider 结果以 `success=false` 和明确 error 报告 partial state。真实 bounded diff 仅进入内部 tool journal/provider continuation；公开 Run、Message 与 SSE 只含相对路径和有界摘要。文件工具复用 Workspace 注册、`CreateRun.workspaceId` 与 Toolset 配置契约；审批通过既有 Run action 与 SSE 契约完成。

`terminal` 每次前台或后台调用都需 durable `once/deny` 审批；`process list/poll/log/wait` 是只读操作，`kill/write/submit/close` 每次都需要新审批。RunService 将这两个工具路由到异步 executor，持久化 tool progress 与 approval journal；后台 process metadata 另持久化到 schema v7 引入、当前 Session schema v13 的 `terminal_processes` 和私有 async-delivery record，owner 固定为 `(profile_id, session_id, creator_run_id, call_id)`，终态不可逆且由 CAS 保护。全局 64 个 `starting|running` 槽通过单个 `BEGIN IMMEDIATE` 事务内的 count + insert 原子预留。它们是 Run 内的模型工具，v1 不新增 REST process-management endpoint。

Terminal 的 Workspace 仅是初始 cwd 边界，不是 filesystem confinement 或 OS/container sandbox；命令在审批后具有宿主机权限。参数、command、stdin、timeout 和输出均有固定上限。backend 只先启动 guardian，在 PID/平台强 identity 已持久化并收到完整合法 launch frame 后才启动 shell；script 不进入 argv，父控制 pipe 断开会终止受管命令树。后台 tool result/event 先持久化，随后 commit launch lease；commit 前的 cancel/deadline 默认回滚并 kill。stdin 控制使用独立有界 writer 与 deadline；root exit/kill 先收敛 Job Object/process group，再限时 drain stdout/stderr。输出执行 ANSI/CRLF 清理、secret redaction、4 KiB 裁剪 guard 和 provider 上限；后台输出仅位于内存，backend 重启后不可恢复。shell 环境从最小 allowlist 重建，不继承 secret/proxy/agent socket/backend 变量。

重启恢复、shutdown 和 detached kill 都要求强 identity：Windows 使用 creation FILETIME，Linux 使用 boot ID + start ticks，macOS 使用 `proc_pidinfo` 启动时间；无效或无法验证的候选项标记为 `lost`，detached `killed` 只在平台终止成功且 tracked root identity 退出后写入。公开事件与 Message 的 `inputSummary` 只使用不含 command/stdin 正文的通用摘要；单行转义、脱敏、限长且附参数 digest 的知情摘要只进入 pending approval，`approval.required` SSE/Message 回放仍使用通用摘要。当前 Session schema 为 v13：v8 approval ledger 持久化 owner 与完整参数 SHA-256，执行 claim 重新绑定 Run ID、Profile/Session/Workspace、call/tool 和 invocation checkpoint；v9 clarification ledger 以同一原始参数 hash 绑定 Run/call/checkpoint，并把私有回答交给 single-use continuation claim；v10 增加上述 code RPC invocation origin/parent/sequence 约束；v11 增加持久 Run queue 与 epoch-fenced runtime lease；v12 增加 owner-bound async delivery record；v13 为 `sessions` 与 `session_versions` 增加受约束的 `persona_id`，使 Session 选择可作为未显式指定 `CreateRun.personaId` 时的持久默认值。

当前 `pty=true` 被拒绝，Windows ConPTY 尚未实现，长期授权也仍未开放，choices 只有 `once/deny`。后台 `terminal` 可选择且只能选择一个异步模式：`notify_on_complete=true` 在 process 进入任一终态时投递一次，或 1..16 个 `watch_patterns` 在当前有界、已脱敏 output snapshot 首次匹配时投递一次。两种模式都要求 `background=true`，不得组合；pattern、command、stdin、raw result 和 output 只存在私有 journal/内存，公开 `tool.delivery` 只包含稳定 `callId`/`processId`、delivery kind、状态、可选 exit code 与 watch 匹配数量。投递 event 与 delivered CAS 在同一事务提交，重启重扫未决 record；同一 process 无论并发 scheduler 或重连都最多出现一个 `tool.delivery`。原始 Run 已终态但仍有未决 delivery 时，SSE 保持可重连，直到投递或未匹配 watch 在 process 终态时被持久结算；显式 `process` poll/log/wait/kill 保持可用且 owner 隔离。`toolExecution=true` 不代表 OS sandbox、对已发生外部副作用的回滚或 exactly-once shell 事务；完整边界以 `docs/terminal-process-contract.md` 为准。

`bootstrap.sessions` 固定等价于 `GET /sessions?profileId=<captured activeProfileId>&archived=false&limit=30` 的空查询首屏。其 `nextCursor` 只能以相同 `profileId`、`archived=false`、空 `q` 交给 `GET /sessions` 续页；active Profile 在 bootstrap 之后变化不改变这个已签发 cursor 的筛选归属。

### 4.2 Profile、配置与密钥

| Method | Path | 说明 |
| --- | --- | --- |
| GET | `/api/v1/profiles` | 列出 Profiles |
| POST | `/api/v1/profiles` | 创建 Profile，可选择 clone |
| GET | `/api/v1/profiles/{profileId}` | 读取 Profile |
| PATCH | `/api/v1/profiles/{profileId}` | 修改展示名、颜色、头像元数据 |
| DELETE | `/api/v1/profiles/{profileId}` | 删除非 default Profile |
| PUT | `/api/v1/profiles/{profileId}/active` | 幂等切换 active Profile |
| GET | `/api/v1/profiles/{profileId}/config` | 读取非敏感配置 |
| PATCH | `/api/v1/profiles/{profileId}/config` | 受约束 Merge patch；保留未知 YAML 字段，不直接写 skills/platforms |
| GET | `/api/v1/profiles/{profileId}/web` | 读取非敏感 Web provider 配置及 search/extract effective readiness；ETag 为共享 config revision |
| PATCH | `/api/v1/profiles/{profileId}/web` | `If-Match` 条件更新 Web provider 选择与 extract 字符预算 |
| GET/POST | `/api/v1/profiles/{profileId}/workspaces` | 列出或注册显式批准的本地目录根；响应不返回路径 |
| DELETE | `/api/v1/profiles/{profileId}/workspaces/{workspaceId}` | 删除未被 Run 引用的注册；绝不删除实际目录 |
| GET | `/api/v1/profiles/{profileId}/secrets` | 只返回 secret status |
| PUT | `/api/v1/profiles/{profileId}/secrets/{secretName}` | 写入 OS keychain |
| DELETE | `/api/v1/profiles/{profileId}/secrets/{secretName}` | 删除 keychain secret |
| POST | `/api/v1/profiles/{profileId}/models/discover` | 使用已保存 secret 探测模型 |
| GET | `/api/v1/profiles/{profileId}/usage` | 聚合 Token/费用统计 |

Profile ID 规则：`default` 或 `^[a-z0-9_][a-z0-9_-]{0,63}$`。`default` 是保留 ID，不能通过创建接口提交。展示名可使用 Unicode，最大 80 个 Unicode scalar，不作为路径。

Profile 生命周期与 clone 规则：

- 安装始终恰有一个 `default` 和一个 active Profile；缺少 `active_profile` 等价于 active=`default`；
- 删除 `default` 或当前 active Profile 返回 409 `profile_delete_conflict`，必须先切换；删除已不存在的合法命名 Profile 返回 204；
- 删除顺序固定为：检查 default/active 约束，读取 SynthChat secret index，幂等删除索引中的 OS keychain entries，确认全部成功后再删除 Profile 文件和索引；若索引非空且 keychain 不可用或拒绝访问，返回 503 `secret_storage_unavailable`，Profile 文件和完整索引均保留以便安全重试。删除途中部分 keychain entry 已不存在也视为成功；没有 indexed secret 时不依赖 keychain 可用性；
- clone 复制源 Profile 的 `config.yaml`（包括未知 YAML 键），但不复制 OS keychain secret、Session、Run、数据库或运行锁；
- `GET /profiles`、`GET /providers` 和 secret status 是本地有界 catalog，不适用 cursor 分页规则；
- secret status 返回 Rust model/Web Provider catalog 声明的全部 secret 名称及本安装索引中的自定义名称，未配置项仍返回 `configured=false`。

Secret name 必须匹配 `^[A-Z][A-Z0-9_]{0,127}$`；Provider catalog、secret status 与 secret path 参数复用同一规则。Provider 默认 URL、Profile model URL 与模型发现覆盖 URL 非空时均只接受包含 host 的 `http` 或 `https` URL，且不得包含 userinfo、query 或 fragment，避免凭据和请求参数进入 YAML。

### 4.2.1 Persona、Worldbook 与 Moments 产品目录

产品目录是 Profile-scoped 的纯 Rust SQLite 扩展，用于恢复现有 Desktop UI 所需的 Persona、Worldbook 和 Moments 数据。它不重新引入 Agent runtime，也不把旧 Python/Agent binding 当作执行依赖。数据库固定位于 `HERMES_HOME/.synthchat/product-catalog-v1.db`，使用 WAL、事务和进程内锁；数据库初始化、锁超时或提交失败返回 503 `product_catalog_unavailable`。

| Method | Path | 说明 |
| --- | --- | --- |
| GET | `/api/v1/profiles/{profileId}/personas?q=` | 列出 Persona；可选 `q` 最多 200 个 Unicode scalar，按名称和序列化字段做不区分大小写子串搜索 |
| POST | `/api/v1/profiles/{profileId}/personas` | 创建 Persona；`application/json`，成功 201 + ETag |
| GET | `/api/v1/profiles/{profileId}/personas/{personaId}` | 读取 Persona snapshot；成功 200 + ETag |
| PATCH | `/api/v1/profiles/{profileId}/personas/{personaId}` | 使用 Persona strong ETag 全量替换输入字段；成功 200 + ETag |
| DELETE | `/api/v1/profiles/{profileId}/personas/{personaId}` | 使用 Persona strong ETag 删除未被 Worldbook 绑定的 Persona；成功 204 |
| GET | `/api/v1/profiles/{profileId}/worldbooks?q=` | 列出 Worldbook；可选 `q` 规则同 Persona |
| POST | `/api/v1/profiles/{profileId}/worldbooks` | 创建 Worldbook；成功 201 + ETag |
| GET | `/api/v1/profiles/{profileId}/worldbooks/{worldbookId}` | 读取 Worldbook snapshot；成功 200 + ETag |
| PATCH | `/api/v1/profiles/{profileId}/worldbooks/{worldbookId}` | 使用 Worldbook strong ETag 全量替换输入字段；成功 200 + ETag |
| DELETE | `/api/v1/profiles/{profileId}/worldbooks/{worldbookId}` | 使用 Worldbook strong ETag 删除；成功 204 |
| GET | `/api/v1/profiles/{profileId}/moments` | 列出 Moments，按 updatedAt descending、ID ascending 排序 |
| POST | `/api/v1/profiles/{profileId}/moments` | 创建 Moment；成功 201 + ETag |
| GET | `/api/v1/profiles/{profileId}/moments/{momentId}` | 读取 Moment、likes 和 comments snapshot；成功 200 + ETag |
| PATCH | `/api/v1/profiles/{profileId}/moments/{momentId}` | 使用 Moment strong ETag 替换 author/body/cover；保留 likes/comments；成功 200 + ETag |
| DELETE | `/api/v1/profiles/{profileId}/moments/{momentId}` | 使用 Moment strong ETag 删除；成功 204 |
| POST | `/api/v1/profiles/{profileId}/moments/{momentId}/comments` | 使用 Moment strong ETag 新增 comment；返回更新后的 Moment + ETag |
| DELETE | `/api/v1/profiles/{profileId}/moments/{momentId}/comments/{commentId}` | 使用 Moment strong ETag 删除 comment；返回更新后的 Moment + ETag，并清除指向它的 replyTo |
| PUT | `/api/v1/profiles/{profileId}/moments/{momentId}/like` | 使用 Moment strong ETag 设置一个 actor 的 `liked` 状态；返回更新后的 Moment + ETag |

OpenAPI 中的 `PersonaInput`、`WorldbookInput`、`MomentInput`、`MomentCommentInput` 和 `MomentLikeInput` 都使用 `additionalProperties=false`。字段边界如下：

- Persona 的 `name` 最多 120，`avatar` 最多 4,096，四个 prompt/instruction 字段各最多 64,000，`provider/model` 各最多 256；`temperature` 为 0..2，`maxTokens` 为 1..1,000,000。未提交字段使用 schema 中的默认值；`legacyAgentId` 只保留迁移元数据，绝不恢复旧 Agent 执行路径。
- Worldbook 的 `name` 最多 120，`description` 最多 8,000，`sections` 最多 200，`boundPersonaIds` 最多 200；每个 section 的 `key` 最多 300、`content` 最多 64,000。更新是完整输入替换，提交的 sections 会重新生成 opaque section ID。
- Moment 的 `authorId` 最多 120，`body` 最多 16,000；`coverFileId` 可为空。Comment 的 `text` 最多 16,000，单个 Moment 最多 1,000 comments；`replyTo` 必须为空或当前 Moment 中已有的 comment ID。Like 的 `actorId` 最多 120，`liked` 是必需 boolean。
- 每个 Profile、每一种 product kind 最多 2,000 条。超过条目或 comment 上限返回 413 `product_catalog_limit`；超过通用请求体上限返回 413 `payload_too_large`。`q` 中的控制字符、文本字段中的 NUL、空白必填字段、非法 opaque reference、未知 JSON 字段和超出字段边界均返回 400 `validation_failed`。

#### 产品 revision 与条件写

- 新建资源的 body `revision=1`，响应 `ETag: "product-persona-1"`、`"product-worldbook-1"` 或 `"product-moment-1"`。成功的资源更新、comment、like 都使对应资源 revision 加一并返回同格式 ETag；body 中的 revision 始终是不带引号的数字。
- 单项 GET 发送与 body `revision` 一致的 quoted strong ETag；列表不发送聚合 ETag，各 item 仍在 body 暴露数值 `revision`。客户端可直接使用单项 GET 的 ETag，或从列表项的 kind/revision 构造相同值，例如 `If-Match: "product-moment-2"`；不得把 Profile config ETag 与 product ETag 混用。
- Persona PATCH/DELETE 只接受 `"product-persona-N"`；Worldbook PATCH/DELETE 只接受 `"product-worldbook-N"`；Moment 更新、删除、comment 和 like 只接受 `"product-moment-N"`。缺 header 返回 428 `precondition_required`，弱 ETag、`*`、多个值、错误 kind 或非法数字返回 400 `invalid_if_match`，旧 revision 返回 409 `revision_conflict`。当前 product conflict Problem 不承诺携带新的 ETag，客户端必须重新 GET。
- PATCH 不是 RFC 7396 Merge Patch，Content-Type 必须是 `application/json`。Persona/Worldbook/Moment 输入中省略的字段会按对应 Input 默认值处理；因此前端更新前必须发送完整的意图快照，不能把省略理解为“保留旧值”。
- 删除不存在或 Profile 不存在分别返回 404 `product_not_found` 或 `profile_not_found`。删除仍被 Worldbook `boundPersonaIds` 引用的 Persona 返回 400 `validation_failed`。产品数据库错误返回 503 `product_catalog_unavailable`；所有产品 route 仍受桌面 Bearer 鉴权和 `X-Request-Id` 约定约束。

Moments 不会自动发布、轮询或触发模型。Persona/Worldbook 的 Run 绑定由显式契约完成：`CreateRun.personaId` 必须引用 Session 所属 Profile 的 Persona；准备首个模型 turn 时 Rust 冻结 Persona 与所有启用且绑定的 Worldbook section，并将有界角色字段作为 system context。Persona 的非空 provider/model 可覆盖 Profile 默认模型，显式 `CreateRun.modelOverride` 优先级更高；`toolsEnabled=false` 与 `memoryEnabled=false` 分别禁止该 Run 注入工具与 Memory。保存产品资料本身不会修改已经开始的 Run 快照。

### 4.2.2 微信账号与显式消息适配器

| Method | Path | 说明 |
| --- | --- | --- |
| GET/PATCH | `/api/v1/profiles/{profileId}/wechat` | 读取或条件更新非敏感 iLink 配置；共用 ProfileConfig ETag |
| POST | `/api/v1/profiles/{profileId}/wechat/qr` | 创建扫码登录 challenge；本地渲染 QR SVG |
| POST | `/api/v1/profiles/{profileId}/wechat/qr/status` | 查询扫码状态；确认后只把 bot credential 写入 OS keychain |
| PATCH | `/api/v1/profiles/{profileId}/wechat/accounts/{accountId}` | 使用 ProfileConfig ETag 唯一绑定或解绑同 Profile Persona |
| POST | `/api/v1/profiles/{profileId}/wechat/accounts/{accountId}/poll` | 按客户端 cursor 显式拉取最多 100 条规范化文本消息 |
| POST | `/api/v1/profiles/{profileId}/wechat/accounts/{accountId}/messages` | 显式发送一条最多 16,000 字符的文本消息 |

账号响应只包含非敏感 metadata、`credentialConfigured` 和可空 `linkedPersonaId`；bot credential、派生 keychain name、请求认证头及原始上游载荷均不返回。一个 Persona 在同一 Profile 最多绑定一个账号。Poll cursor 最多 16 KiB，由 Desktop 客户端持有并显式续传；服务端最多接受 100 条上游记录，只返回含稳定 ID、peer 与有界文本的规范化子集，并报告 `receivedCount/skippedCount`。当前不启用后台自动 poll、自动 Session/Run 或自动回复，因为这些工作流还需要持久 cursor、消息 ID 和 peer→Session 幂等账本。

### 4.2.3 Manifest-only 插件目录

| Method | Path | 说明 |
| --- | --- | --- |
| GET | `/api/v1/plugins` | 列出最多 512 个本地登记项；返回 plugin catalog ETag |
| POST | `/api/v1/plugins/install` | 登记插件根目录直接子项中的有界 `plugin.json`；初始 disabled |
| PATCH | `/api/v1/plugins/{pluginId}` | 使用 catalog ETag 启用或停用登记项 |
| DELETE | `/api/v1/plugins/{pluginId}` | 使用 catalog ETag 移除登记；不删除源目录 |

插件根固定为 `HERMES_HOME/.synthchat/plugins`。source path 解析后必须是该目录的直接子项；外部路径、穿越、symlink/reparse point、超过 64 KiB 或带未知字段的 manifest 均被拒绝。公开 DTO 固定 `execution=manifestOnly`，最多声明 128 个 tool name 与 128 个环境变量名称。`enabled` 仅是目录 metadata，不加载 entry point、不注入 Run 工具、不读取所需环境变量，也不恢复 Python/Node/旧 Agent 插件运行时。

### 4.3 Session 与消息

| Method | Path | 说明 |
| --- | --- | --- |
| GET | `/api/v1/sessions` | 按 `profileId` 列表；`q` 触发 FTS |
| POST | `/api/v1/sessions` | 创建空 Session，固定 `profileId` |
| GET | `/api/v1/sessions/{sessionId}` | 读取 Session |
| PATCH | `/api/v1/sessions/{sessionId}` | 改标题、archive 状态 |
| DELETE | `/api/v1/sessions/{sessionId}` | 幂等删除 |
| GET | `/api/v1/sessions/{sessionId}/messages` | cursor 历史，按 `sequence` 升序返回当前页 |
| GET | `/api/v1/profiles/{profileId}/session-imports/hermes-v21` | 只读预检锁定 v21 `state.db`，不返回路径 |
| POST | `/api/v1/profiles/{profileId}/session-imports/hermes-v21` | 按预检指纹原子导入 v21 快照 |

Session 生命周期与条件写：

- `POST /sessions` 必须携带 `Idempotency-Key`，body 必须显式指定已存在的 `profileId`；创建后 `profileId` 不可修改。切换 active Profile 只影响后续默认创建，既有 Session、Message 和 Run 始终沿用 Session 创建时的 `profileId`；
- `POST`、`GET /sessions/{id}` 和成功的 `PATCH` 返回 `ETag: "<Session.revision>"`。列表项携带同一个不带引号的 `revision`，可直接用于后续 `If-Match`；
- `PATCH` 只允许 `title` 和 `archived`，必须携带当前 Session strong ETag。空 patch 无效；匹配 revision 的 no-op 返回 200 与原 revision，不写库；旧 revision 返回 409 `revision_conflict` 和当前 `ETag`；
- `DELETE` 对仍存在的 Session 要求 `If-Match`；若 Session 已不存在，则缺少 header 或携带任一语法合法的 strong ETag 都返回 204，便于安全重放；显式提供的畸形 header 始终先返回 400 `invalid_if_match`，不能借资源不存在绕过输入校验。只要存在任一非终态 Run（queued、running、waiting approval、waiting clarification 或 cancelling）就返回 409 `session_busy`，不会隐式取消 Run；
- 删除 Session 在一个事务中删除其 Message、FTS 行和用量明细；已由其他资源保留的 File 不被删除。终态 Run 变为不再可从 Session 导航的 tombstone，其 event journal 仍保留到既定重放期限后再由 GC 删除，因此 Session 删除不会破坏已经承诺给 SSE 客户端的重放窗口。归档不删除数据，但归档 Session 创建 Run 返回 409 `session_archived`，客户端须先条件 PATCH 恢复；
- 相同创建幂等键与相同规范化 body 在至少 24 小时内返回同一 Session 的当前表示和当前 ETag，不创建副本；若期间 Session 已被显式删除，返回 410 `idempotent_resource_deleted`，该 key 不得使其复活。

列表与搜索约定：

- `GET /sessions` 始终要求 `profileId`，不会返回其他 Profile 的摘要或命中；`archived=false`（默认）只返回未归档项，`archived=true` 只返回归档项；
- `q` 缺失、空或 trim 后为空时执行普通列表；非空时以字面量搜索 title、session ID 和已提交 message text，仍使用同一 endpoint 和分页形状；
- `capabilities.sessionSearch.mode` 报告启动时实测的有效模式：`fts5`、`trigram`、`like` 或 `unavailable`。FTS5/trigram 初始化失败时必须显式降级为参数化 escaped `LIKE`；只有连 LIKE 也不可用时才报告 `unavailable`，非空 `q` 返回 422 `session_search_unavailable`，普通列表仍可用；
- `capabilities.sessionStorage` 独立报告 Rust 会话库是否可用、当前 schema version，以及固定版本 Hermes importer 是否已经实现；不得用 `sessionSearch.mode` 代替整个存储域的健康状态。数据库初始化失败时 storage `available=false`、schemaVersion=`null`，所有 Session 路由返回 503，但 `/health`、Profile 与配置仍可用；
- 用户输入绝不作为原始 FTS `MATCH` 语法执行；引号、`*`、`NEAR` 等按字面量处理。LIKE 模式对 `%`、`_` 和 escape 字符转义并绑定参数；
- Session 列表默认按 `(updatedAt DESC, id DESC)` 稳定排序，cursor 固定该排序位置；
- 首次请求捕获数据库 session-change 高水位；后续 cursor 同时绑定规范化 `profileId/q/archived`、该高水位和独占排序位置。后端用版本化 Session summary 和带 commit change-sequence 的 Message 执行 as-of 查询：高水位之后新建的 Session 不插入当前遍历，之后更新的既有 Session 仍以快照时表示出现在原排序位置，不得因更新而遗漏或重复；遍历期间被删除的项可缺席；
- snippet 使用纯文本和明确的 match ranges，不把 HTML 高亮片段交给前端；range 使用 JavaScript UTF-16 code unit offset，`start` 包含、`end` 不包含，并按 start 升序且互不重叠。
- `q` 为空时 `Session.match=null`；`q` 非空时 `match.field` 标识 `title`、`id` 或 `message`，正文命中还返回 `messageId`。同一 Session 多处命中时按 title、ID、最新 Message 的固定优先级选择一个摘要。

Message 读取边界：

- 每个 Session 的已提交 Message 拥有从 1 开始、严格递增且永不复用的 `sequence`；它与 Run event `sequence` 是两个独立序列。导入旧 Hermes 数据时按显式 adapter 的稳定顺序分配该值；
- 不带 cursor 的首次请求在一个读事务内捕获 `snapshotLastSequence`，返回不大于该值的最新一页；页内始终按 `sequence ASC`。`nextCursor` 同时携带该快照上界和当前页最早 sequence 的独占下界，后续请求严格返回更早的一页，前端将其 prepend；
- `MessagePage.firstSequence/lastSequence` 是当前页边界，空页均为 null；`snapshotLastSequence=0` 表示该 Session 尚无 Message。相同 cursor 的结果稳定，不受之后追加消息影响；刷新时不带 cursor 才捕获新快照；
- 历史接口只返回事务提交完成的 Message。创建 Run 时 user Message、Run 与幂等记录在同一事务提交；assistant Message 必须先提交再发送 `message.completed`。尚在流式生成的临时 assistant 文本只存在于 Run event journal，不以半成品 Message 出现在历史中；
- Message 不提供单独修改/删除接口；Session 删除是唯一批量删除边界。这样 FTS 行、`messageCount`、usage 和会话摘要可与消息事务保持一致。

Hermes v21 导入边界：

- API 不接受文件路径。default Profile 只解析 `HERMES_HOME/state.db`，命名 Profile 只解析 `HERMES_HOME/profiles/{profileId}/state.db`；Profile ID、direct-child、普通文件、symlink/reparse point 和 SQLite `NOFOLLOW` 检查必须在 Profile 锁持有期间完成；
- `GET` 只读预检返回 `absent` 或 `ready`。`ready` 包含 adapter ID、`referenceCommit`、schema version、快照指纹、Session/Message/model usage/附件/rewind 计数和聚合 warning；不得返回绝对路径、附件引用、billing base URL 或畸形原文；
- `POST` body 必须包含预检得到的 `expectedSnapshotFingerprint` 和 `allowAttachmentOmission`。服务端重新读取单一 WAL 一致快照；指纹变化返回 409，附件数非零且未显式允许省略返回 422；
- 来源连接固定使用 `mode=ro`、`query_only=ON`、`SQLITE_OPEN_NOFOLLOW` 和一个 read transaction，不运行 Hermes 初始化、repair、migration、checkpoint 或 FTS DDL；旧库 `active IS NULL` 按锁定上游 `UPDATE ... SET active=1` 的只读等价语义解释并产生有界 warning；
- target Session/Message ID 由 `profileId + adapterId + upstream source-key digest` 确定性派生，不能使用会变化的 snapshot fingerprint，也不能复用原始上游 ID；Message sequence 按每个 Session 的 `(timestamp ASC, upstream ID ASC)` 分配；
- `active=0, compacted=0` 不导入；`active=0, compacted=1` 写入历史/FTS但内部 `contextEligible=false`；原生消息与 active 导入消息为 true。`session_model_usage` 单独写入 provenance，不能与 Session aggregate 重复累计；upstream `token_count` 不能可靠拆分 prompt/completion，因此导入 Message 的公开 `usage=null`；
- 导入是单一 `BEGIN IMMEDIATE` 事务：Session、Message、FTS、usage provenance、映射、batch、warning 和幂等记录共同提交，且整个快照只分配一个 session change sequence。任何来源 row digest 冲突、已映射目标删除/修改、或 mapped Session 出现新增来源行都返回有界冲突报告并全量回滚；
- 相同 key/body 跨重启返回持久结果且 `disposition=replayed`，不同 body 返回 `idempotency_conflict`；新 key + 完全相同且目标未变化的快照返回 `unchanged`。幂等重放不重新读取来源；删除映射目标后用新 key 导入不得复活；
- tool arguments 不进入公开 summary；有可靠匹配 tool-result 的历史调用为 `completed`，没有可靠结果的为 `unknown`。附件引用只保留内部来源摘要；允许省略时报告明确的 `omittedAttachmentCount`，不伪造 FilePart。

### 4.4 文件

| Method | Path | 说明 |
| --- | --- | --- |
| POST | `/api/v1/files` | `multipart/form-data` 上传且必须恰好包含一个 `file` part，返回 opaque `fileId` |
| GET | `/api/v1/files/{fileId}/content` | 读取不可变文件快照；响应为 `no-store`、`nosniff` |
| DELETE | `/api/v1/files/{fileId}` | 显式删除文件；目标已不存在时仍返回 204 |

文件内容固定上限 8 MiB，multipart envelope 另保留 64 KiB 边界开销；实际 MIME allowlist 由 `/capabilities.files.allowedMimeTypes` 返回，HTML/SVG 等主动内容不在 allowlist。文件名只作为受校验元数据保存，不能包含路径分隔符、盘符或控制字符。`fileId` 是 `file_[0-9a-f]{32}` 形式的随机 opaque ID，客户端只能整体传递，不能从中推导路径或时间。

上传必须携带 `Idempotency-Key`。幂等 fingerprint 覆盖规范化 MIME、文件名和完整字节：相同请求跨重启返回同一个 `FileRef` 与 `createdAt`，同 key 不同内容返回 409 `idempotency_conflict`；文件显式删除后重放原 key 返回 409 `idempotency_resource_gone`，不会复活内容。对象在 `HERMES_HOME/.synthchat/files` 下以目录级原子提交持久化，所有目录和文件都通过 capability/no-follow 句柄读取；路径穿越、symlink、Windows reparse point、未知目录项、size/SHA-256/metadata 不一致均 fail closed。API、Problem 与日志不返回宿主机绝对路径。

### 4.5 Chat Run

| Method | Path | 说明 |
| --- | --- | --- |
| GET | `/api/v1/runs?profileId={profileId}&state=active&sessionId={sessionId?}` | 从 SQLite 发现当前 Profile、可选 Session 的非终态 Run |
| POST | `/api/v1/sessions/{sessionId}/runs` | 接受用户消息并返回 `202 RunAccepted` |
| GET | `/api/v1/runs/{runId}` | 读取可轮询 Run 状态 |
| GET | `/api/v1/runs/{runId}/events` | SSE；支持 `Last-Event-ID` |
| POST | `/api/v1/runs/{runId}/cancel` | 幂等取消 running/queued Run |
| POST | `/api/v1/runs/{runId}/approvals/{approvalId}` | `once/session/always/deny` |
| POST | `/api/v1/runs/{runId}/clarifications/{requestId}` | 回答澄清问题 |

请求示例：

```json
{
  "clientRequestId": "018f...",
  "message": {
    "text": "分析这两个文件的差异",
    "fileIds": ["file_01", "file_02"]
  },
  "modelOverride": null,
  "reasoningEffort": "medium"
}
```

`202` 示例：

```json
{
  "run": {
    "id": "run_01",
    "sessionId": "session_01",
    "profileId": "default",
    "status": "running",
    "lastSequence": 1,
    "createdAt": "2026-07-16T09:00:00Z",
    "updatedAt": "2026-07-16T09:00:00Z"
  },
  "disposition": "started",
  "queueItemId": null,
  "sessionRevision": "session_rev_after_user_01",
  "userMessage": {
    "id": "msg_user_01",
    "sessionId": "session_01",
    "sequence": 13,
    "role": "user",
    "parts": [{"type": "text", "text": "分析这两个文件的差异"}],
    "reasoning": null,
    "toolCalls": [],
    "usage": null,
    "createdAt": "2026-07-16T09:00:00Z"
  }
}
```

同一 Session 的 Run 默认串行。首次接受时 `disposition` 为 `started` 或 `queued`；同一个 `Idempotency-Key` 重试必须返回同一个 Run、同一个 user Message 和当前 `sessionRevision`，不得重复写用户消息。重放使用 `disposition=replayed`，`run` 返回当前状态（包括终态），而不是伪造原先的 running/queued 快照；若 Session 已删除则返回 410 `idempotent_resource_deleted`。

`GET /api/v1/runs` 是 active Run discovery 的持久恢复面。`profileId` 与 `state=active` 必填，`state` 只接受精确值 `active`，`sessionId` 是可选 opaque ID；未知字段、重复或畸形值返回 400 `validation_failed`，不存在的 Profile 返回 404。后端在单个 SQLite 读事务内读取 `queued/running/waitingApproval/waitingClarification/cancelling`，严格按 Profile owner、可选 Session owner 过滤，并按 `(createdAt ASC, runId ASC)` 稳定排序。响应为 `ActiveRunList { items }`，硬上限 16；每项包含完整 `Run`、仅 queued 状态可非空的 `queueItemId`、创建该 Run 的 user Message 和当前 `sessionRevision`。超过上限或 owner/Message/queue invariant 损坏时 fail closed，不返回截断快照。Chat UI 只在 `activeRunDiscovery=true` 时请求，并再次校验 Profile/Session/Message owner；活动 Run 从 sequence 1 重放持久 journal 后转 live，因而可恢复未提交的 assistant 草稿、工具进度和 pending action。切换 Profile/Session 会 abort 旧发现请求；重放窗口过期时按 4.8 的 REST 对账规则降级。完成这些门槛后 `capabilities.extensions.activeRunDiscovery=true`。

队列是独立能力，当前 Rust runtime 报告 `runQueue=true`。同一 Session 已有 executor 状态 Run 时，后续创建在一个 `BEGIN IMMEDIATE` 事务内持久化 user Message、`queued` Run、opaque queue item、原始创建请求和幂等记录；queued 状态不得调用 Provider。前一 Run 终态后，持有当前 runtime lease 的实例按 `(createdAt, runId)` FIFO claim 下一项，在同一事务删除 queue row、切换为 running 并追加 `run.started`。queued Run 使用现有 `/runs/{runId}/cancel` 原子删除 queue row 并写入 `run.cancelled`，从不发送 `run.started`。schema v11 的单一 runtime lease 使用单调 epoch fencing；新实例接管后，旧实例的 Run 写事务必须失败。重启把无法安全恢复的 running/waiting/cancelling Run 按既有中断语义终结，但保留并继续 queued items。

Run 的 `profileId` 必须从 Session 固定归属复制，创建请求不能覆盖；user Message、Run、Session 摘要/revision 和幂等记录在同一 SQLite 事务提交后才返回 202。归档 Session 返回 409 `session_archived`，不存在的 Session 返回 404，均不写入 Message 或幂等成功记录。

`RunAccepted` 组合必须满足：

- `disposition=started` 时 `run.status=running` 且 `queueItemId=null`；
- `disposition=queued` 时 `run.status=queued` 且 `queueItemId` 为非空 opaque ID；
- `disposition=replayed` 时 `run` 可处于任一当前状态，且不得重新分配 Run、Message 或 queue item；
- queued Run 在真正开始执行时才发送 `run.started`；在队列中取消时不发送 `run.started`，但仍必须发送该 Run 的 `run.cancelled` 终态。

`Run.pendingAction` 是 SSE 重放窗口之外的恢复面：`status=waitingApproval` 时必须是 `PendingApprovalAction`，`status=waitingClarification` 时必须是 `PendingClarificationAction`，其他状态必须为 null。它携带继续操作所需的 action ID、choices 和有界摘要，但不包含未经脱敏的完整工具参数。`RunAccepted.sessionRevision` 是 user Message 提交后的 revision；后续 `message.completed.data.sessionRevision` 是 assistant Message 提交后的 revision，前端必须用它更新当前 Session 的条件写缓存。

当前 `approvals=true`、`clarifications=true`。`POST /runs/{runId}/approvals/{approvalId}` 实现 durable once/deny 决策；`POST /runs/{runId}/clarifications/{requestId}` 实现持久回答与 Provider continuation。UI 仍须按 capabilities 分别启用入口。下述审批语义现已生效：

- `ApprovalDecision.decision` 必须属于该 `PendingApprovalAction.choices`，不能只因属于全局 enum 就被接受。不在本次 choices 中返回 409 `approval_choice_not_offered`，且 pending action 保持不变；首批危险工具的 choices 固定且仅为 `["once", "deny"]`，`session/always` 只保留为后续策略扩展；
- 第一次接受决策时，后端在同一持久事务内写入 immutable decision ledger、清除 `pendingAction`、更新 Run 状态并追加唯一的 `approval.resolved`。相同 `(runId, approvalId)` 与完全相同的规范化 decision payload（包含 nullable `reason`）可跨重启幂等重放为 200 `{"accepted":true}`，不得追加第二个事件或再次授权；不同 payload 返回 409 `approval_decision_conflict`；未知或不属于该 Run 的 approval ID 返回 404；
- `expiresAt` 使用服务端时钟且为硬截止时间。决策线性化时若 `now >= expiresAt`，后端必须先以 `decision=deny, resolvedBy=expiry` 持久化 fail-closed 结果，绝不执行工具；该请求返回 409 `approval_expired`。定时器、重启恢复或 action 请求均可触发同一幂等过期事务；
- approval 决策与 Run cancel 通过同一个持久状态机串行化。若 cancel 先提交，则以 `decision=deny, resolvedBy=cancellation` 解除 pending action，后到的决策返回 409 `approval_no_longer_pending`，工具不得产生副作用；若决策先提交，则后到的 cancel 按普通取消处理。executor 在启动副作用前必须再次检查持久 Run 状态，已提交 cancel 时不得启动；已经开始的外部副作用只能 best effort 中断，UI 不得显示为已回滚；
- 显式 `deny` 与 cancel 竞态遵循同一先提交者规则：deny 先提交时重复相同 deny 仍为幂等 200，cancel 可随后把 running Run 转为 cancelling；cancel 先提交时 deny 返回 `approval_no_longer_pending`，不会覆写 cancellation 产生的 resolution。

澄清语义由 schema v10 保留的 v9 bound immutable ledger 负责：

- `clarify` 的 question 为 1..2,000 chars；choices 为 0..4 个互不相同的 1..500 chars 字符串，空数组表示自由回答。ledger 请求绑定 `runId`、`callId`、invocation checkpoint 和原始 tool arguments SHA-256；同一事务写入 ledger、`waitingClarification` pending action 与 `clarification.required`；
- `ClarificationAnswer.answer` 为 1..10,000 chars，后端原样保存且不 trim。choices 非空时必须逐字精确匹配其中一项，否则返回 409 `clarification_choice_not_offered`，pending 状态不变。首次回答原子写入 immutable resolution、清除 pending 并把 Run 恢复为 running；相同回答可幂等重放，不追加事件或重复 claim，不同回答返回 409 `clarification_answer_conflict`；未知/错 Run ID 返回 404 `clarification_not_found`，已由取消或失败解决则返回 409 `clarification_no_longer_pending`；
- resolved user answer 只有在 ledger、当前 invocation 和 executor binding 的 Run/call/checkpoint/参数 SHA-256 全部一致时，才能被 single-use continuation claim 消费。answer 只存在于私有 ledger、claim 返回和 Provider continuation/tool-result journal；`Run`、SSE、Message、Problem 与日志均不得回显；
- cancel 先提交时，同一事务按 `clarification.resolved(resolvedBy=cancellation) -> tool.failed` 解除 pending 并把 Run 转为 cancelling。任何非取消终止中断，包括 deadline、Provider/本地执行失败、backend 重启或恢复失败，都统一按 `resolvedBy=failure` 先解决 ledger 和 tool，再追加 `run.failed`；该规则不是仅用于重启恢复。

工具数据分为内部执行 journal 与公开投影。经过校验的原始参数、Provider tool-call payload、完整结果、stdout/stderr 和执行 checkpoint 只写入 Rust 后端内部的持久 journal，v1 API 不提供读取入口。公开 `Run`、`Message`（包括 tool role）、`Message.toolCalls` 与所有 SSE event 只能携带已经脱敏的投影，包括长度受限的 `inputSummary`、`resultSummary`、安全进度文本、`Problem` 和 opaque `FileRef`；不得返回原始参数/结果、secret、绝对路径或未经筛选的上游正文。事件 journal 必须直接持久化已脱敏的公开 payload，重放不得从内部原始数据重新生成，以免脱敏策略变化造成泄露。

### 4.6 Toolset、Skills、Memory 与 MCP

| Method | Path | 说明 |
| --- | --- | --- |
| GET | `/api/v1/profiles/{profileId}/toolsets` | 动态数组，不硬编码数量；响应 ETag 为 config revision |
| PATCH | `/api/v1/profiles/{profileId}/toolsets/{toolsetId}` | 仅更新 `enabled`；以 config revision 条件写 |
| GET | `/api/v1/profiles/{profileId}/skills` | 列表和搜索；返回当前 ProfileConfig 强 `ETag` 供启停 PATCH 使用 |
| POST | `/api/v1/profiles/{profileId}/skills/install` | URL/registry/file ref 安装 |
| PATCH | `/api/v1/profiles/{profileId}/skills/{skillId}` | 启停/配置 |
| DELETE | `/api/v1/profiles/{profileId}/skills/{skillId}` | 卸载外部 Skill |
| GET | `/api/v1/profiles/{profileId}/memories?target=memory\|user` | builtin target 的 revision-consistent cursor page；返回 ETag |
| POST | `/api/v1/profiles/{profileId}/memories` | `Idempotency-Key` + `If-Match` 条件新增 builtin entry；201 + ETag |
| PATCH | `/api/v1/profiles/{profileId}/memories/{memoryId}` | `If-Match` 条件替换 entry 全文；200 + ETag |
| DELETE | `/api/v1/profiles/{profileId}/memories/{memoryId}` | `If-Match` 条件删除 builtin entry |
| GET | `/api/v1/profiles/{profileId}/mcp/servers` | 列表 |
| POST | `/api/v1/profiles/{profileId}/mcp/servers` | `Idempotency-Key` 创建；201 + Profile config ETag |
| PATCH | `/api/v1/profiles/{profileId}/mcp/servers/{serverId}` | `If-Match` 条件修改/启停；200 + ETag |
| DELETE | `/api/v1/profiles/{profileId}/mcp/servers/{serverId}` | `If-Match` 条件幂等删除；204 + ETag |

MCP 配置与 Rust runtime 遵循以下边界：

- 配置写入 Hermes `config.yaml -> mcp_servers`，与 Profile config/toolset/skill 变更共用 `profiles.lock`、4 MiB 有界 YAML、strong ETag 和同目录原子替换。事务只替换 MCP 已识别键，保留未知顶层字段及 server entry 内未知字段；旧条目没有 ID 时按 `(profileId,name)` 派生稳定 opaque `mcp_<32 hex>`，首次真实变更后写入保留字段 `_synthchat.id`，rename 不改变 ID；
- GET、POST、PATCH、DELETE 都返回当前 Profile config ETag，并统一携带 `Cache-Control: no-store`。PATCH/DELETE 必须携带单个 quoted strong `If-Match`，即使请求是 no-op 或删除已不存在的合法 opaque ID，也先校验 revision；匹配 revision 的 no-op 不重写 YAML。POST 必须携带 8..128 visible-ASCII `Idempotency-Key`，记录与资源在同一 YAML 原子事务提交，作用域绑定 method/canonical Profile path；至少保留 24 小时且有硬上限。同 key 同一规范化请求返回相同 ID 的当前表示；secret name 集合顺序不影响 fingerprint，args 顺序影响。资源删除后重放返回 409 `idempotency_resource_gone`；POST/PATCH 超过全局 body 上限返回 413 `payload_too_large`；
- server name 必须匹配 `[A-Za-z0-9][A-Za-z0-9_-]{0,63}`，每 Profile 最多 64 个。`stdio` 必须提供 direct executable `command`、至多 64 个 args（单项 2,048 bytes、总计 16 KiB）和至多 32 个互异大写 `envSecretNames`；拒绝 shell interpreter、shell command/metacharacter、control character，以及 `--api-key`、`--token=...`、`PASSWORD=...` 等敏感 option/inline assignment，secret 必须改由同名 env 引用。执行器清空子进程环境，只显式注入这些名称；bare executable 可使用父进程 PATH 做一次解析，但 PATH 本身不传给子进程；
- `streamableHttp`/`sse` 只接受不含 userinfo/query/fragment 的 URL；公网必须 HTTPS，loopback 可 HTTP/HTTPS，literal private/metadata/link-local/multicast/unspecified 地址一律拒绝。远端认证只接受 nullable `bearerTokenSecretName`，磁盘表示固定为 `headers.Authorization: "Bearer ${NAME}"`；不接受任意 header map。每次连接及每个 redirect 都重新解析并验证 DNS 的全部地址，并将该 HTTP 请求固定到这组已验证地址；禁用 reqwest 自动 redirect，最多跟随 5 次保持 method 的安全 redirect。Bearer 只发送给配置 URL 的同源目标，`Mcp-Session-Id` 只发送给创建它的 origin，跨 origin redirect 不携带两者；
- 请求和响应从不包含 secret value。合法但尚未配置的引用可以持久化，响应仅在 `missingSecretNames` 返回缺失名称；状态以 OS keychain 实际读取为准，不能信任可能过期的索引。keychain 不可用/拒绝访问时 GET/POST/PATCH 返回 503 `secret_storage_unavailable`；create/update 在 readiness 成功前不得改写 config，因而不会产生“已提交但 HTTP 503”的条件写漂移；
- 旧 YAML 中 `env` 的每个值必须严格等于同名 `${NAME}`；远端 `headers` 最多只能是上述 Authorization 模板。明文、其他 header、冲突 transport/type、重复/非法 ID 或超限结构使整个 MCP 投影 fail closed 为 409 `mcp_config_invalid`，Problem 不回显原值、原 YAML 或绝对路径；
- pinned Hermes Agent 使用 `url` 推断 Streamable HTTP、使用 `transport: sse` 选择 legacy SSE；Desktop/API 遗留的兼容 `type` 若语义一致则保留，冲突时 fail closed。启用且 secret 完整的 server 在 Run 创建前以独立、可回收的纯 Rust 会话执行 `initialize`、`notifications/initialized` 和有界分页 `tools/list`；工具经确定性 `mcp__<server>__<tool>` 名称投影后冻结到该 Run。输入 schema 只接受有界、无远程 `$ref` 的 JSON Schema 子集，调用前再次本地校验；所有动态工具一律要求 durable `once/deny` 审批。`tools/call` 使用新会话，超时、取消、协议错误和正常完成后都释放 transport；结果大小受限并按当前 Profile secrets 递归脱敏后才写入私有 journal/Provider continuation，公开 SSE/Message 只保留通用摘要；
- Streamable HTTP 的 POST 同时声明 `application/json` 与 `text/event-stream`，只接受有界 JSON-RPC response 或 SSE `message` event。初始化响应可建立一个有界、无控制字符的 `Mcp-Session-Id`；后续 POST/DELETE 携带该 session ID 和初始化协商所得 `MCP-Protocol-Version`，session ID 变化、缺失/不支持的 protocol version、错误 response ID 或超限 body 均 fail closed。关闭时对已建立 session 尝试有界 DELETE；
- legacy SSE 以一个持续 GET `text/event-stream` 建立下行通道；首个有效事件必须是 `endpoint`，且解析后必须与最终 GET URL 同源，POST 上行地址才可使用其服务端生成的 query/session 标识。JSON-RPC response 只从有界 `message` event 按 request ID 消费；连接关闭、第二个 endpoint、跨源 endpoint 或非 202/204 上行确认均 fail closed。两种远端 transport 都复用 Run 的 timeout/Abort control 和输出脱敏边界。`mcpManagement=true`、`mcpStdio=true`、`mcpStreamableHttp=true`、`mcpSse=true`；`/test` 与 `/tools` 管理路由仍不存在。

当前 Toolset 管理切片遵循以下边界：

- `GET /toolsets` 返回 `Toolset[]`，响应 `ETag` 等于当前带引号的 ProfileConfig revision；数组和工具数量均来自 Rust catalog，前端不得写死；
- `PATCH /toolsets/{toolsetId}` 必须携带同一 config revision 的 `If-Match`，请求体必须且只能是 `{"enabled": boolean}`。在安全、有界且可脱敏的配置模型落地前不接受通用 `config`；
- 实际启停变化与 `PATCH /config` 共享同一配置 revision，成功后返回新 ETag；目标状态已满足时为 no-op，不重写配置并返回原 ETag。即使目标状态相同，过期 `If-Match` 仍返回 409 `revision_conflict`；
- 合法 Profile 下不存在于 Rust catalog 的 `toolsetId` 返回 404 `resource_not_found`，不得凭空向 YAML 插入未知 Toolset。

Web-first 契约遵循以下独立边界：

- `GET /api/v1/web/providers` 是本地、无副作用、无网络探测的 Rust adapter catalog。当前响应只含 `tavily`，并明确 `supportsSearch=true`、`supportsExtract=true`、`secretNames=["TAVILY_API_KEY"]`、后端启动时生效的 `defaultBaseUrl` 与 `customEndpointSupported=false`。默认 endpoint 是 `https://api.tavily.com`，可信部署可通过 `SYNTHCHAT_TAVILY_BASE_URL` 覆盖；`customEndpointSupported=false` 明确指 Profile 不得提供或覆盖 endpoint。Exa、Parallel、Firecrawl、SearXNG、Brave、DDGS、xAI 及其他上游 provider 在对应 Rust adapter 完成前不得出现在可选择列表，也不得被自动执行；
- `GET /profiles/{profileId}/web` 返回 `WebConfig` 和与 body `revision` 对应的 shared ProfileConfig strong ETag。`sharedProvider/searchProvider/extractProvider` 分别投影 Hermes `web.backend/web.search_backend/web.extract_backend`；`extractCharLimit` 投影 `web.extract_char_limit`，默认 15,000，允许 2,000..500,000。GET 可只读显示既有未知 provider 名，但其 effective status 必须是 `unsupported`，不能静默改写或回退；
- effective provider 优先级固定为 capability-specific override、shared provider、唯一已实现且 keychain secret 已配置的 Tavily shortcut。显式选择永远优先：显式 `tavily` 缺 key 时 status=`missingSecret`，显式未知 provider 时 status=`unsupported`，均不得回退到另一个 provider。`missingSecretNames` 只含名称，不含 value、preview 或长度；readiness 不执行远程或计费探测；
- `PATCH /profiles/{profileId}/web` 使用 `application/merge-patch+json` 和 shared ProfileConfig `If-Match`。只接受 `tavily` 或 null 的 provider 字段以及有界 `extractCharLimit`；null 清除对应 override，未知 provider 返回 400 `validation_failed`。成功、no-op、stale revision 与未知 YAML 保留规则和 config/toolset PATCH 相同；GET/PATCH 因无法安全计算 keychain readiness 时返回 503 `secret_storage_unavailable`；
- Tavily key 继续通过现有 Secret API 写入 OS keychain。`GET /secrets` 的 catalog 必须包含 Rust model 与 Web provider 声明的 secret name，因此未保存的 `TAVILY_API_KEY` 也以 `configured=false` 出现。Web 配置、YAML、日志、Run journal 公开投影和 telemetry 均不得包含 key；
- `webSearch/webExtract=true` 是 engine 实现能力，`EffectiveWebProvider.status=ready` 是 Profile 调用先决条件，两者不可互换。`web` Toolset enabled 但 provider 不 ready 时，Run 不发送相应工具 schema；provider ready 但 Toolset disabled 时同样不发送。Search 与 Extract 分别判断，不得用单一 `Toolset.configured` 推断两者均可用；
- Web 调用是 Run 内部 model tool，不新增公开执行 endpoint。原始 query、URL、完整 provider 响应和网页正文只进入私有 invocation/provider continuation journal；公开 `tool.started/progress/completed/failed`、Run、Message 与 Problem 只含有界脱敏摘要。取消、deadline、invocation checkpoint、参数 SHA-256、single execution claim 和重启 fail-closed 复用通用 Run 规则。

发送给模型的严格工具 schema 为：

```json
{
  "name": "web_search",
  "parameters": {
    "type": "object",
    "additionalProperties": false,
    "properties": {
      "query": { "type": "string", "minLength": 1, "maxLength": 4000 },
      "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 5 }
    },
    "required": ["query"]
  }
}
```

```json
{
  "name": "web_extract",
  "parameters": {
    "type": "object",
    "additionalProperties": false,
    "properties": {
      "urls": {
        "type": "array",
        "minItems": 1,
        "maxItems": 5,
        "items": { "type": "string", "minLength": 1, "maxLength": 8192 }
      },
      "char_limit": { "type": "integer", "minimum": 2000, "maximum": 500000, "default": 15000 }
    },
    "required": ["urls"]
  }
}
```

Web URL、网络与输出安全是能力置 true 的组成部分，而不是后续加固项：

- query 和 URL 在任何网络调用前对当前 Profile 的 secret snapshot、已知 credential 前缀、percent-decoded 形态及 credential-like query 参数名执行 fail-closed 扫描；命中时不向 Tavily发送；
- extract URL 只允许有 host 的 HTTP/HTTPS，拒绝 userinfo、控制字符、非法 IDNA、超限输入和 cloud metadata。DNS 必须检查全部 A/AAAA，任一地址属于 loopback、RFC1918、link-local、CGNAT、benchmark、reserved、multicast、unspecified 或 IPv4-mapped 私网即整项拒绝；DNS 失败同样拒绝。Tavily 返回的 URL 再执行相同校验；
- Tavily endpoint 默认是官方 HTTPS origin；可信运维只能在 backend 启动前通过 `SYNTHCHAT_TAVILY_BASE_URL` 设置公开 HTTPS origin 与简单 path prefix，Profile/YAML/API 均不能覆盖。配置拒绝 userinfo、query、fragment、特殊 hostname 与私网/特殊 IP；请求发送 key 前解析全部 DNS 地址、逐一执行 public-address 检查并把完整结果固定到客户端。客户端关闭 redirect，不把 API key 发送到其他 authority。响应要求 JSON Content-Type，并同时限制 Content-Length、实际流字节、连接/首字节/idle/总 deadline、单 Profile 与全局并发；
- provider 返回的标题、描述与正文是不可信外部数据。进入 provider context 前清理 NUL/控制字符/ANSI、inline base64/data/blob 内容，执行 secret redaction、UTF-8 边界截断和总 provider-content 上限，并显式标记 external-untrusted；网页中的指令不得改变 system/tool policy；
- 首版不支持私网 URL、自定义 provider endpoint、浏览器交互、截图、Cookie、JavaScript、CDP 或下载。不得用 `web_extract` 包装一个 shell/curl 旁路，也不得因 `webSearch/webExtract=true` 改写任一 Browser flag。

Skill install 是异步操作：响应 `202 Operation`，前端轮询 `/api/v1/operations/{operationId}` 或在后续版本订阅全局 resource event。不能把“已启动安装进程”显示为“安装成功”。

Memory 契约、Rust builtin 实现、前端管理面和端到端测试均已落地，`features.memoryWrite=true`；UI 仍必须以能力位而不是路由探测决定是否开放管理入口。其兼容边界如下：

- 管理面只投影 Profile 下的 canonical builtin Markdown：`memories/MEMORY.md` 与 `memories/USER.md`，entry delimiter 为 `"\n§\n"`。`target` 仅允许 `memory|user`，不支持 `session`、`importance` 或虚构的 entry 时间戳。Profile 的 `memoryProvider` 非 `builtin` 时，GET/POST/PATCH/DELETE 全部返回 422 `memory_provider_unsupported`；切回 builtin 后才可管理本地 Markdown，不返回空列表或伪造的 external projection；
- GET 必须指定 target，并支持可选 `q`、`cursor`、`limit`。响应 `MemoryPage` 必含 `items,nextCursor,revision,provider,charsUsed,charLimit,promptSafety,capabilities`；`provider` 固定为 `builtin`，ETag 是 `revision` 的带引号 strong 形式。`charsUsed`/`charLimit` 描述完整 target，不随搜索或分页变化；默认字符预算为 MEMORY 2,200 chars、USER 1,375 chars，可由 `memory.memory_char_limit` / `memory.user_char_limit` 覆盖。启用实现的 builtin page 对应四个 capabilities 均为 true；
- `Memory` 只有 `id,target,content,provider`。ID 是 revision-scoped opaque 值：任意 target revision 变化后，客户端必须丢弃该页所有 ID 和 cursor，再 GET 新页；不得解析 ID、跨 target 使用或配合新 revision 重放。合法但不属于当前 revision 的 ID 不提供兼容映射；
- POST 同时要求 `Idempotency-Key` 和当前 target 的 `If-Match`；PATCH/DELETE 要求 `If-Match`。缺 header 返回 428，非法 strong ETag 返回 400，过期 revision 或 pagination drift 返回 409 并携带当前 ETag。PATCH body 必须且只能含非空 `content`；Create/Patch 单条最多 2,200 chars，规范化后完整 target 超预算返回 422 `memory_capacity_exceeded`；
- content 去除首尾 Unicode whitespace，保留内部换行；`charsUsed` 是以 `"\n§\n"` 连接全部规范化 entry 后的 Unicode scalar value 数量，ID、revision、幂等 fingerprint 和容量检查都基于同一规范形态；
- 幂等键按 installation、method、canonical path、target、规范化 body 和 base revision 绑定并至少保留 24 小时。完全相同重试返回原 201 snapshot，不重复追加；同 key 不同 fingerprint 返回 409 `idempotency_conflict`；对应 entry 已明确删除时返回 410 `idempotent_resource_deleted`，Gone sidecar 必须清除缓存的 entry 正文与旧 ETag。原 snapshot 的 ETag/ID 可能已因后续写入过期，调用方在任何 mutation 前仍须刷新。exact duplicate add 是成功 no-op，返回 201、当前 item 和不变 ETag；
- backend 在跨进程 target lock 内重新读取 bytes、复核 ETag 和 round-trip drift，再以同目录临时文件、flush/sync、atomic replace 写入。PATCH/DELETE 发现非规范内容会返回 409 `memory_storage_drift`，不得悄悄丢弃手工或并发修改；
- POST/PATCH 在持久化前执行 strict threat scan，命中时返回 422 `memory_content_blocked`。每个 Run 在首次 `prepare_model` 时对 MEMORY/USER 再做 strict scan 并捕获一次冻结 prompt snapshot；同一 Run 后续 Provider turn 复用该 snapshot，Run 中发生的写入只影响后续 Run。磁盘已有危险 entry 保留在经鉴权的管理视图中供删除，但从 prompt snapshot 排除，页面 `promptSafety=blocked`；
- 模型调用 `memory` 工具产生的 add/replace/remove 或多 operation 原子 batch 必须走 durable `once|deny` approval、single-use execution claim、取消/重启 fail-closed 和同一 ETag/lock/scan 写路径；batch 以最终规范化 target 计算预算并整批提交或整批不写。未经审批的模型输出不能写 Memory。原始 entry、tool arguments 和完整结果只存在于私有执行 journal；公开 Run、Message、SSE、Problem、approval summary 和普通日志不得泄露正文。经鉴权的 Memory 管理 endpoint 是用户显式查看/编辑正文的唯一公开例外。

Skill 安装的 `registryId`、`url`、`fileId` 必须且只能提交一种。MCP 的完整配置不变量、条件写、secret reference 和当前 capability 边界见 4.6；PATCH 不能切换 transport，若提交 `transport` 则必须与已有值相同。

## 5. 核心资源

### 5.1 Profile

```json
{
  "id": "default",
  "displayName": "Default",
  "isDefault": true,
  "isActive": true,
  "color": "#3b82f6",
  "avatarFileId": null,
  "engineState": "running",
  "configRevision": "rev_01",
  "createdAt": null,
  "updatedAt": "2026-07-16T09:00:00Z"
}
```

`Profile` 是列表与 active 切换返回的动态摘要。创建、单项读取和 metadata PATCH 返回不含 `isActive`、`engineState`、`configRevision` 的 `ProfileMetadata`，并通过 ETag 暴露独立 metadata revision。

当前按需执行的 Rust runtime 不为每个 Profile 常驻独立进程；`engineState` 表示共享 Run 服务对该 Profile 的可用性。Run 服务可接受请求时为 `running`，会话存储等必需基础设施不可用、导致 Run 服务不可用时为 `failed`。它不表示 Provider 凭据或外部模型端点已通过预检。

`ProfileConfig` 只暴露稳定、非敏感字段：model/provider/baseUrl、reasoning、toolsets、memory provider、`extensions`，以及只读派生的 skills/platforms 状态。`ProfileConfigPatch` 只允许 `model`、`toolsets`、`memoryProvider`、`extensions`；提交 `skills` 或 `platforms` 返回 400 `validation_failed`。Skills 由嵌套配置和目录系统的独立 endpoints 管理；platforms 由 keychain/env 与顶层 `<platform>.enabled: false` override 派生，并由后续独立 endpoints 管理。Rust 写 YAML 时保留本 DTO 未认识的键。

### 5.2 SecretStatus

```json
{
  "name": "OPENROUTER_API_KEY",
  "configured": true,
  "storage": "osKeychain",
  "updatedAt": "2026-07-16T09:00:00Z"
}
```

禁止返回 preview、first4/last4 或长度。删除后 `configured=false`。写入或删除只影响随后创建的 Run/tool call；已开始请求持有的内存 secret 快照在结束后立即 zeroize。

### 5.3 Session

```json
{
  "id": "session_01",
  "profileId": "default",
  "title": "迁移设计",
  "preview": "我们先确定 API 边界...",
  "source": "desktop",
  "model": "provider/model",
  "messageCount": 12,
  "archived": false,
  "revision": "session_rev_01",
  "createdAt": "2026-07-16T08:30:00Z",
  "updatedAt": "2026-07-16T09:00:00Z",
  "match": null
}
```

### 5.4 Message

```json
{
  "id": "msg_01",
  "sessionId": "session_01",
  "sequence": 12,
  "role": "assistant",
  "parts": [
    {"type": "text", "text": "分析完成。"},
    {"type": "file", "fileId": "file_03", "name": "report.md", "mimeType": "text/markdown"}
  ],
  "reasoning": null,
  "toolCalls": [],
  "usage": {"promptTokens": 1200, "completionTokens": 300, "totalTokens": 1500, "cost": null},
  "createdAt": "2026-07-16T09:00:00Z"
}
```

历史中的 reasoning/tool calls 可作为显式字段或 timeline item 返回，不得重新塞进 `content` JSON 字符串。`sequence` 仅在所属 Session 内有序，前端不得拿它与 SSE Run event sequence 比较。

## 6. SSE 规范

### 6.1 Frame

```text
id: run_01:42
event: message.delta
data: {"schemaVersion":1,"sequence":42,"runId":"run_01","sessionId":"session_01","occurredAt":"2026-07-16T09:00:01.123Z","data":{"messageId":"msg_assistant_01","delta":"你好"}}

```

所有 data 共用 envelope：

```ts
interface RunEvent<T> {
  schemaVersion: 1;
  sequence: number;
  runId: string;
  sessionId: string;
  occurredAt: string;
  data: T;
}
```

SSE 的 `event:` 行决定 payload 类型。机器可读映射位于 `openapi.yaml` 的 `streamRunEvents.x-sse-event-schemas`，每个映射值均是完整 envelope schema；前端不得只按任意 JSON 解析 `data`。

### 6.2 事件目录

| event | 必需 data | 约束 |
| --- | --- | --- |
| `run.queued` | `queueItemId: string` | 仅排队时发送，最多一次 |
| `run.started` | `profileId: string` | Run 实际进入执行时恰好一次；队列中取消时不出现 |
| `message.started` | `messageId`, `role=assistant` | 必须早于首个 delta |
| `message.delta` | `messageId`, `delta` | 只发送增量，不发送累计快照 |
| `reasoning.delta` | `messageId`, `delta` | 可选；受配置/Provider 支持限制 |
| `tool.started` | `callId: string`, `name: string`, `inputSummary?: string` | `callId` 生命周期稳定；表示 invocation lifecycle 已建立，不表示副作用已开始 |
| `tool.progress` | `callId: string`, `message?: string`, `progress?: number` | 可重复；progress 在 0..1 |
| `tool.completed` | `callId: string`, `resultSummary?: string`, `artifacts: FileRef[]`, `asyncDeliveryPending?: boolean` | 与 failed 二选一，最多一次；仅安全布尔标记，不含异步参数或结果 |
| `tool.delivery` | `callId`, `processId`, `delivery=completion\|watch`, `status`, `exitCode?`, `matchedPatternCount?` | 后台 terminal 的一次异步投递；不含 command、pattern、output 或 raw result |
| `tool.failed` | `callId: string`, `error: Problem` | 与 completed 二选一，最多一次 |
| `approval.required` | `approvalId`, `callId`, `toolName`, `inputSummary: string|null`, `choices: (once\|session\|always\|deny)[]`, `expiresAt` | 所列字段全部必需；默认 fail closed |
| `approval.resolved` | `approvalId`, `callId`, `decision`, `resolvedBy: user\|expiry\|cancellation` | 每个 approval 恰好一次；也是正常恢复执行的显式信号 |
| `clarification.required` | `requestId`, `question`, `choices: string[]` | 前端通过 REST 回答；允许空 choices 表示自由输入 |
| `clarification.resolved` | `requestId`, `resolvedBy: user\|cancellation\|failure` | 持久解除 clarification pending；公开事件不回显 answer |
| `usage.updated` | `promptTokens`, `completionTokens`, `totalTokens`, `cost?` | Token 数值单调不减 |
| `message.completed` | `message: Message` | 完整规范化快照；每个 assistant message 恰好一次 |
| `run.completed` | final `usage`, `messageId` | Run 终态之一 |
| `run.cancelled` | `reason?` | Run 终态之一 |
| `run.failed` | `error: Problem` | 脱敏；Run 终态之一 |

事件生命周期约束：

- sequence 从 1 开始并严格连续递增；SSE `id` 固定为 `{runId}:{sequence}`；
- `message.started` 必须先于该 `messageId` 的 delta 和 completed；`message.completed` 携带完整快照，用于替换累计中的临时内容；
- 无需审批的调用按 `tool.started -> tool.progress* -> tool.completed|tool.failed` 排序。需要审批时严格按 `tool.started -> approval.required -> approval.resolved -> tool.progress* -> tool.completed|tool.failed` 排序；clarify 按 `tool.started -> clarification.required -> clarification.resolved -> tool.completed|tool.failed` 排序。在相应 resolved 事件持久化前不得发送该 call 的后续 progress/终态；
- `approval.required` 与 `Run.status=waitingApproval`、对应 `pendingAction` 在同一事务内可见。用户决策或过期处理的事务追加 `approval.resolved`、清除 `pendingAction` 并把 Run 从 waitingApproval 转为 running；该事件一旦可见，`GET Run` 必须返回 running 或更晚状态。`resolvedBy=cancellation` 是例外：同一事务转为 cancelling；
- `clarification.required` 与 `Run.status=waitingClarification`、对应 `pendingAction` 在同一事务内可见。回答、取消或任何导致 Run 终止的非取消中断必须在持久事务内追加唯一 `clarification.resolved` 并清除 pending；`resolvedBy=user` 后恢复 running，`cancellation` 后转 cancelling，`failure` 在 `tool.failed` 后紧邻 `run.failed`。failure 包含 deadline、Provider/本地错误、backend 重启及恢复失败，并非仅指重启。任意答案只进入内部 immutable decision/continuation journal，公开 SSE、Message、Problem 与日志不得回显；
- `resolvedBy=user` 且 decision 为本次 choices 中的允许项时才授予相应 scope；首批危险工具只有 `once`。用户 `deny` 或 `resolvedBy=expiry` 都不得执行副作用，并在 `approval.resolved` 后以 `tool.failed`（`Problem.code=tool_execution_denied`）结束该 call。`resolvedBy=cancellation` 后以 `tool.failed`（`Problem.code=tool_execution_cancelled`）结束该 call，再发送 `run.cancelled`；
- 每个 `tool.started` 最终至多一个 `tool.completed` 或 `tool.failed`；Run 成功前所有已开始 tool 必须进入终态；
- `run.completed` 必须晚于最终 `message.completed`；`run.completed`、`run.cancelled`、`run.failed` 三者必须恰好出现一个。没有未决异步投递时，它是该 Run 最后一个有 sequence 的事件；有 `notify_on_complete` 或 `watch_patterns` 的后台 terminal 则可在它之后恰好追加一个 `tool.delivery`。
- 终态发送并 flush 后，若没有未决异步投递，服务端关闭连接。未决 delivery 保持该 Run 的 SSE 可重连，直到投递或 watch 未匹配且 process 到达终态后才关闭。连接层在无业务事件时最多每 15 秒发送 SSE comment `: heartbeat`，不占 sequence；客户端连续 45 秒未收到任何字节可主动重连。

### 6.3 重连与对账

- 每个 Run 在内存或持久事件表中保留最多 2,048 个连续事件；Run 终态后将当前窗口继续保留至少 10 分钟；
- 首次连接不带 `Last-Event-ID` 时必须从 sequence 1 重放再转为 live，消除 `POST Run` 与建立 SSE 之间的竞态；若 sequence 1 已被淘汰则返回 409，而不是静默从窗口中间开始；
- 客户端重连发送 `Last-Event-ID: {runId}:{sequence}`，服务端从下一 sequence 重放；runId 不匹配或 ID 格式错误返回 400；
- 请求的下一 sequence 早于当前窗口时返回 409 `event_history_expired`；客户端随后 `GET /runs/{id}`（含 `lastSequence` 和可恢复的 `pendingAction`）及 `GET /sessions/{id}/messages` 对账。若 Run 正在等待 approval/clarification，UI 从 `pendingAction` 恢复交互；若仍在执行，以该 `lastSequence` 作为 `Last-Event-ID` 重连并等待后续事件；
- 重放事件保持原 `id`、sequence 和 payload，不重新生成时间；
- `approval.required` 与 `approval.resolved` 都属于持久 event journal：若客户端从 required 之前重连，必须按原 sequence 依次收到两者；若从 resolved 之后重连，不得重新合成 required。相同审批的幂等 REST 重试不分配新 sequence；窗口外恢复只以当前 `Run.pendingAction` 为准，已 resolved 的 approval 不得重新显示为 pending；
- 对已终止且没有未决 async delivery 的 Run，连接必须重放到唯一终态后立即关闭；存在未决 delivery 时重放终态后继续等待，直到 delivery 被结算；
- UI store 按 `(runId, sequence)` 去重，按 `messageId` 和 `callId` 更新，不按文本内容猜测。

## 7. 资源一致性

### 7.1 Session

Session 的 `revision` 是其完整单资源表示的 strong revision：title/archive 变更以及已提交 Message 引起的 preview、model、messageCount、updatedAt 变更都会生成新值。ETag 必须等于带引号的 body revision；列表本身不返回一个聚合 ETag。

```http
PATCH /api/v1/sessions/session_01
If-Match: "session_rev_01"
Content-Type: application/merge-patch+json

{"title":"新的标题"}
```

- `If-Match` 只接受单个带引号的 strong ETag；弱 ETag、`*`、多个值或非法格式返回 400 `invalid_if_match`，存在资源时缺失返回 428 `precondition_required`；
- revision 不匹配返回 409 `revision_conflict`，并在 `ETag` 响应头返回当前 revision。服务端不自动覆盖，也不因目标值碰巧相同而绕过 stale 检查；
- SQLite 写事务使用进程内协调、`BEGIN IMMEDIATE`、有限 busy timeout 和参数绑定；超时返回 retryable 503 `session_storage_busy`，不得把锁错误、数据库路径或 SQL 暴露给前端；
- Rust schema 使用版本化 migration、WAL 与 `foreign_keys=ON`，Rust backend 是唯一写入者。固定上游 Hermes `state.db` 只允许经 schema-version adapter 只读导入，禁止双写或与 Python 共享写库。

### 7.2 Profile 与配置

Profile metadata 与 Profile config 是两个独立的条件写资源：

- `POST /profiles` 与 `GET/PATCH /profiles/{id}` 的单资源响应为 `ProfileMetadata`，其 ETag 只标识 `profile-meta.json`；动态的 active/config/runtime 字段只存在于列表/active 响应的 `Profile` 摘要中，因此不会破坏 metadata ETag 语义；
- `Profile.configRevision` 仅指配置 revision，不是 metadata revision；
- `GET /profiles/{id}/config` 的 ETag 必须等于带引号的 body `revision`；`GET /profiles/{id}/toolsets` 的 ETag 必须是同一个 config revision。config、toolset 或 skill PATCH 都以该 ETag 执行条件写，成功响应返回结果 config ETag，实际持久状态变化时使旧 revision 失效；
- `If-Match` 只接受单个带引号的 strong ETag；弱 ETag、`*`、多个值或非法格式返回 400 `invalid_if_match`，缺失返回 428 `precondition_required`。

配置更新示例：

```http
PATCH /api/v1/profiles/default/config
If-Match: "rev_01"
Content-Type: application/merge-patch+json

{"model":{"provider":"openrouter","model":"..."}}
```

- revision 不匹配返回 409 `revision_conflict`，并在 `ETag` 响应头返回当前 revision，不自动覆盖；
- 后端合并结构化 YAML 并保留未知键；
- 写入必须加锁、写临时文件、fsync（平台支持时）并原子替换；
- 写 secret 的 endpoint 不修改普通配置为明文值，只写 secret reference/status。
- `application/merge-patch+json` 在本契约中使用受约束 merge-patch profile：metadata PATCH 只允许 `color=null`、`avatarFileId=null` 清除对应元数据，其他 metadata null 返回 400；
- ProfileConfig patch 只允许 `model.baseUrl=null` 与 `model.reasoningEffort=null` 作为显式 provider-default 状态，不表示删除属性；其他 null（包括 `extensions` 任意嵌套层级）返回 400，null 不承担 RFC 7396 的通用删除语义；
- ProfileConfig 的嵌套 map 是字段级 merge，空 patch/no-op 返回原 revision 且不重写文件；
- `toolsets` 是 `platform_toolsets.cli` 与 `agent.disabled_toolsets` 的规范化布尔视图：读取时 CLI sequence 中的 ID 为 true，再由 disabled sequence 覆盖为 false；ProfileConfig patch 中的布尔值及专用 Toolset PATCH 的 `enabled` 使用相同写入规则，true 会加入 CLI sequence 并移出 disabled sequence，false 执行相反操作，其他未知 YAML 保持不变；
- `skills` 与 `platforms` 在 `ProfileConfig` 中仅为独立子系统的只读派生投影，可在对应子系统尚未实现时返回空对象；不得把 Hermes 的 nested skill config、技能目录或平台凭据状态压平后回写为通用 bool map；
- PATCH 的幂等含义是目标状态可安全重试；首次成功后用旧 `If-Match` 重放仍按并发规则返回 409，不缓存成功响应；
- `ModelConfig.baseUrl=null` 表示使用 Provider 默认 URL；`reasoningEffort=null` 表示使用 Provider 默认推理强度；非空 URL 必须是包含 host 的 `http` 或 `https` URL，且不得包含 userinfo、query 或 fragment；
- `extensions` 只用于非敏感扩展，包含 token/key/password/secret/credential 等敏感键的 patch 返回 400；未知 YAML 键内部保留但不会自动暴露到 `extensions`。

OS keychain 被锁定、不可用或拒绝访问时返回 503 `secret_storage_unavailable`。Secret PUT 对同一 `(profileId, secretName)` 采用 last-write-wins；响应、Problem、日志、YAML、数据库和 telemetry 均不得包含 value、preview 或长度。
Secret value 的跨平台上限为 2560 UTF-8 bytes（同时受 OpenAPI 2560 字符上限约束），以兼容 Windows Generic Credential；更大的 OAuth token 暂不支持，`oauthAccounts=false`，不得通过明文文件或多 entry 分片降级。

## 8. 纯 Rust 实现映射

| 本项目契约 | Rust 权威模块 | 兼容/备注 |
| --- | --- | --- |
| `/health` | `api/system` | Rust service 存活；engine 可用性由 capabilities 分开报告 |
| `/capabilities` | `engine/capabilities` | 只报告已经实现并通过验收的 Rust 能力 |
| Session CRUD/messages | `session` + 自有 SQLite schema | 固定 Hermes `state.db` 只读 importer 用于迁移 |
| Session search | Rust SQLite FTS5/trigram | FTS 缺失时 escaped LIKE |
| Run/events | `engine` + `run` + event journal | Rust 直接调用模型 Provider 并生成规范化 SSE |
| Approval | `tools/policy` + `run` | approval ID 由 Rust 签发并 fail closed |
| Profile create/delete/use | `config/profile` | 兼容 Hermes 配置布局，但不调用 CLI |
| Config | Rust structured YAML | 保留未知字段，不用 ad-hoc 字符串替换 |
| Secrets | Rust keychain adapter | 仅在内存构造 Provider/tool 凭据，不写明文 `.env` |
| Toolsets | Rust dynamic tool registry | 数量与能力动态发现 |
| Web provider/config | `web` + `tools/web` | Tavily 是当前唯一可选择 Rust adapter；密钥来自 keychain |
| `web_search` / `web_extract` | Rust async Web executor | 仅在分域 capability、Toolset 与 Profile readiness 同时满足时注入 |
| Terminal/process | `tools/terminal` + `processes` + `sessions/process_store` | Run 内异步工具；无独立 REST 管理面 |
| `execute_code` | `code_execution` + `processes/{direct,guardian,output}` + schema v10 invocation journal | 可选 host Python 仅执行获批用户代码；Agent runtime 仍为 Rust |
| Skills | Rust parser/registry 与本地 installer/uninstaller | 列表/搜索/启停、持久 Operation、owner lease、崩溃恢复和有界清理已实现；内容目录仍继续做 handle-relative 加固 |
| Memory | Rust builtin/provider trait | 按 provider capabilities 降级 |
| MCP | Rust Profile config CRUD + stdio JSON-RPC runtime | CRUD 原子读写兼容 YAML；stdio 直接完成 initialize/tools/list/tools/call 与 Run 注入，不调用 Dashboard/CLI adapter；远程 HTTP/SSE runtime 仍待实现 |
| Persona/Worldbook/Moments CRUD | `product_catalog` + Rust SQLite | Profile-scoped bounded product data；独立 product ETag；Persona/绑定 Worldbook 由显式 `personaId` 冻结注入 Run |
| WeChat account/message adapter | `wechat` + OS keychain | 配置、扫码、Persona 绑定与显式有限 poll/send；bot credential 不进 YAML/响应，后台自动 Run 仍关闭 |
| Manifest-only plugins | `plugins` + Rust bounded registry | 本地 manifest 登记/启停/移除；不执行插件代码，不恢复旧 Agent runtime |

当前 Run transport 只启用明确标记为 OpenAI-compatible 的 catalog Provider。原生 Anthropic、Gemini、Copilot、MiniMax-Anthropic 与 Azure Foundry adapter 未实现时返回 502 `engine_unavailable`，不得把“存在于 Provider catalog”解释为“已支持推理”。Provider HTTP 认证、限流、请求拒绝、流中错误和不完整响应分别映射为稳定的 `provider_*` 错误码；`detail` 只保留可操作的脱敏说明，不转发上游正文。流式 Provider 必须返回可信 usage；缺失 usage 的成功流按 `provider_response_invalid` 失败，不能以零值伪造 Token 用量。

## 9. 非本契约范围

以下是桌面壳或尚未批准的产品域，不进入核心 Rust API v1：

- 窗口移动、托盘、文件选择、打开/定位本地文件、截图、桌宠命中测试；
- Theme/emoji 等纯 UI 设置；
- SynthChat Plugins 代码执行模型；
- 微信后台自动轮询/消息到 Run、Moments 主动生成/发布、桌宠视觉控制等尚未完成映射评审的扩展；
- Hermes Dashboard 的 200+ 内部管理 routes；
- 远程公网模式、TLS、多用户认证。

这些功能若保留，应进入独立 extension contract，不能通过 `Record<string, any>` 或任意 JSON 偷渡进核心 API。

## 10. 契约验收测试

进入阶段三前至少自动化以下行为：

1. OpenAPI 文件解析通过，operationId 唯一。
2. Rust route 与 OpenAPI path/method 一致。
3. 未认证请求除 `/health` 外全部 401。
4. Profile 切换不会改变既有 Session/Run 的 `profileId`。
5. 配置 ETag 冲突不会丢失未知 YAML 字段。
6. Secret create/read/delete 全流程不在响应和日志出现 secret value。
7. FTS 查询转义引号、`*`、`NEAR`、`%`、`_` 和 escape 字符，无 SQL 注入；FTS 初始化失败时 capability 与 LIKE 降级一致。
8. Session 创建幂等键跨重启不重复；Profile 切换后归属不变；stale Session ETag 不能覆盖或删除新状态。
9. Message 历史在并发追加时按固定 `snapshotLastSequence` 向前翻页，无重复、遗漏或半成品 assistant Message。
10. Run 首 delta 前有稳定 `messageId`，sequence 单调，终态恰好一次。
11. SSE 断线重连无重复文本、无丢失 tool terminal event。
12. cancel、approval、clarification 与重复请求幂等。
13. 上传返回 opaque ID，API 永不暴露绝对路径。
14. Hermes 版本/能力不匹配时返回明确 `engine_capability_missing`，不假成功。
15. Web provider/config GET/PATCH 的 ETag 与 ProfileConfig 同源；stale/no-op/null-clear/未知 legacy provider 均符合约定且不丢失未知 YAML。
16. Web registry 逐工具验证 capability、Toolset、effective provider 与 keychain；缺任一条件均不向模型发送 schema，显式缺 key/unsupported provider 不自动回退。
17. Web URL 测试覆盖混合公私 DNS、IPv4-mapped IPv6、CGNAT、metadata、userinfo、编码 secret、credential query、DNS 失败、返回 URL 重验与 provider redirect 拒绝。
18. Tavily mock E2E 覆盖 search/extract、五 URL 顺序、响应/字符上限、429/5xx/timeout/cancel/restart；公开 Run/SSE/Message/Problem 和日志均不含 key、完整 query/URL 或网页正文。
19. `codeExecution` 仅在 Run/session runtime 与 Python 3.8+ probe 同时 ready 时为 true；WindowsApps alias、旧版本、失败或超时 probe 均不得启用能力或 Toolset `configured`。
20. `execute_code` E2E 覆盖审批前零 spawn、once 后 Python + nested RPC、deny 后零启动/零 nested row、Profile secret 环境剥离、stdout/stderr 边界脱敏，以及 cancel 后 guardian 进程树停止且晚到 success 不覆盖取消。
21. schema v10 验证 `provider|codeRpc` origin、parent/sequence 完整性与不可变性；nested 参数/结果只存在于私有 journal，不进入 Provider 顶层 tool-call、公开 Run/SSE/Message 或重放事件。
22. 产品目录测试覆盖 Profile 隔离、三类 CRUD、搜索、字段/数量上限、Persona 绑定删除保护、Moment comment/reply/like、错误 kind/缺失/过期 strong ETag，以及响应不恢复或调用旧 Agent runtime。
23. Persona Run 测试覆盖同 Profile 校验、角色 prompt、启用且绑定的 Worldbook、模型覆盖优先级、toolsEnabled、memoryEnabled 与首 turn 快照冻结。
24. WeChat fixture 覆盖扫码凭据只写 keychain、Persona 唯一绑定、poll cursor/100 条上限/未知消息跳过、send、上游错误映射及响应/Problem 无 credential/raw payload。
25. 插件测试覆盖根目录约束、manifest 字段/大小/数量限制、symlink/reparse point、catalog ETag 冲突、登记/启停/移除不删除源文件，以及无 entry point/代码执行路径。

截至 2026-07-17 的完整验证基线：backend unit `246/246`、integration `91/91`（合计 `337/337`），frontend `389/389`，desktop `1/1`；backend/desktop fmt 与 `clippy -D warnings`、OpenAPI drift、TypeScript 和 Vite production build 均通过。该基线不包含 Browser UI 自动化、压力/长稳或三平台原生 crash/process 发布矩阵。
