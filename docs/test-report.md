# 阶段四测试报告

**状态：Windows 全量回归、OpenAI-compatible Provider 定向回归、12 条 UI E2E（含真实 Chromium 与后台 Terminal 双 Run 流程）、崩溃恢复、日志脱敏和原生密钥链回归通过；阶段四全量验收未完成。**

本报告记录当前 Windows 主机的实际验证。真实外部模型、长时间稳定性、
内存泄漏、跨平台和发布验收仍未通过，不能由本地绿色矩阵外推。

## 已执行验证

### 当前完整正确性矩阵

2026-07-20 使用 MSVC Rust 1.88、Node 22.14.0 和 npm 10.9.2 重新执行：

| 范围 | 实测结果 |
| --- | --- |
| Backend | fmt、all-targets check、clippy `-D warnings` 通过；Windows 串行完整矩阵 493 passed、0 failed、1 ignored，其中 library 364/364、backend binary 2/2、Run HTTP 36/36，全部集成测试二进制通过 |
| Desktop | fmt、check、clippy 通过；21/21 测试通过 |
| Frontend | 31 个测试文件、502/502 测试通过；TypeScript/Vite 生产构建通过 |
| API 契约 | OpenAPI generated-type drift 与 Redocly 2.39 lint 通过 |
| Tauri IPC | AST 门禁扫描 42 个生产 TypeScript 文件并通过内置绕过自测 |
| npm 依赖 | root 与 `frontend/` 的 `npm audit` 均为 0 vulnerabilities |
| Playwright | Node 22.14.0/npm 10.9.2 下 12/12，最新本地运行 43.0 秒；覆盖聊天流、Skills、Workspace write/read/search/patch 双审批与终态泄露审计、Clarification、stdio MCP、前台 Terminal、后台 Terminal 双 Run/异步 delivery/进程回收、真实 Python `execute_code`、真实 Chromium navigate/snapshot/两次一次性审批/CDP/隔离下载、两类崩溃恢复、消息 FTS5/继续对话及 Memory CRUD/搜索 |
| Mixed runtime v2 | verifier 8 项自检通过，含严格 `session_search` 正向闭环与 call/result 负例、8 小时上限、全窗样本和 `backendRssUnavailable`；真实 backend smoke 4/4，覆盖 Profile/Session/Run/SSE/FTS、2/2 工具回路、逐 Run envelope/sequence 与全局守恒，backend/provider/temp 清理通过；60 秒资格测试 170/170 iterations、17/17 工具回路、187/187 Provider 请求通过 |
| Windows NSIS | 当前源码生成 `SynthChat_1.1.0_x64-setup.exe`（26,009,305 bytes，SHA-256 `DFA82F256A0251B025BB78F68EE72FF3C1E622233DA9992D41CAF24E6AC81216`）；7-Zip 完整性、8 项载荷、Desktop Tauri marker 补丁、sidecar 精确哈希、禁入路径和高置信凭据扫描通过；Authenticode 为 `NotSigned` |
| Mixed pilot (旧 text-only workload) | 30 分钟真实 Profile/Session/Run/SSE/FTS；1,094/1,094 successes、1,094 Provider requests、0 failures、356 resource samples、backend/provider/temp clean teardown；RSS 33.71→44.34 MiB，仍需 v2 8 小时 soak |
| Mixed extension (旧 text-only workload) | 60 分钟真实 Profile/Session/Run/SSE/FTS；2,238/2,238 successes、2,238 Provider requests、0 failures；715/719 RSS samples available、0 dropped；RSS 31.95→45.06 MiB、全窗 +11.00 MiB/h、后 30 分钟 +4.70 MiB/h；current-run clean teardown，仍需 v2 8 小时 soak |

为验证 Browser 生命周期及用例间清理，Browser -> Terminal -> approval ->
Skills 最小交错矩阵 4/4（21.8 秒）及 Browser Rust 定向矩阵 9/9 均通过。
扩展后的 Browser UI 用例还以真实 Chromium 完成 navigate、snapshot、一次性
CDP 审批注入唯一下载链接、重新 snapshot 和独立一次性 download 审批；Provider
精确验证 filename、MIME、size、SHA-256 和 scan flags，公开 UI/REST/storage/
request body/console 不含文件体、data URL 或私有路径。该用例单独 1/1（5.2 秒）
并与 Code 组合 2/2（8.7 秒）通过；当前完整 12/12 组合运行 43.0 秒通过。

扩展后的 Files UI 用例在同一个 Run 内顺序执行 `write_file`、`read_file`、
`search_files` 和 `patch`。write 与 patch 使用两个不同的一次性审批；Provider
逐步精确验证四个私有结果，patch 审批前磁盘仍为原文，终态文件为新内容。
UI、Run/events/messages、storage、approval response 和 console 均不含原文、
补丁内容、raw arguments 或绝对 Workspace/target 路径，原子写临时文件、backend
与 E2E runtime 残留均为 0。该用例锁定工具链定向运行 1/1、3.0 秒通过。

阶段五清理后曾在同一源码上隔离复验 Browser + Code，2/2、15.6 秒通过。
此前一次全量复跑在宿主压力下分别超过 Browser 30 秒和通用 15 秒 UI 等待，
且外层 15 分钟期限短于 runner 自身 20 分钟全局期限；runner 最终仍清除了
backend、子进程和 runtime temp。该次不计为新的全量通过，保留为测试时限
抖动证据；随后的 2/2 证明当前 Browser/CDP 与 guardian/Python 路径可完成。

后台 Terminal 定向 E2E 连续执行 3 次均通过，测试体分别为 8.8、7.0、
7.0 秒；每次结束后 `terminal-fixture`、backend 进程及
`synthchat-hermes-e2e-*` runtime 目录计数均为 0。Terminal 完整文件 2/2
运行 13.7 秒通过。Windows 后端全矩阵使用以下稳定门禁，避免真实 Chromium、
MCP 与 guardian 测试争抢同一宿主进程资源；测试自身的产品 deadline 和断言不变：

统一 Run task registry、admission gate 与共享 shutdown deadline 已纳入该
backend 矩阵。定向用例覆盖忽略取消的 Provider 被 Drain 收敛、PreserveRuns
快速停止本地 worker 后由 successor runtime 终结旧 running Run 并推进 queued
Run，以及关闭开始后拒绝新的前台 Terminal launch；这些结果关闭了旧的 lease
TTL 等待和未跟踪 Run worker 缺口，但不替代长时间并发/进程树 soak。

```powershell
$env:SYNTHCHAT_CODE_EXECUTION_PYTHON = 'D:\python\python.exe'
cargo +1.88.0-x86_64-pc-windows-msvc test --quiet --all-targets -- --test-threads=1
```

日志脱敏进程测试显式清空其隔离 backend 的 `PATH`，使该安全用例不依赖
宿主 Python 冷探测；其原始 token/header/secret 脱敏断言保持不变并通过 1/1。

2026-07-19 又执行了真实 Rust HTTP/SSE Provider 的定向矩阵：

| 范围 | 实测结果 |
| --- | --- |
| `providers::openai_compatible` | 14/14；覆盖请求序列化、分片 UTF-8/SSE、reasoning/usage、工具调用拼装与边界、取消、空闲超时、HTTP 错误及响应脱敏 |
| `web_run_http` | 5/5；通过本地 HTTP Provider/Tavily fixture 覆盖真实网络传输、私有工具续轮、公开事件脱敏和取消 |

这些测试验证生产 Provider 传输实现而不是注入的 trait mock。仓库不保存真实
第三方凭据，因此仍需在受控环境执行至少一次 live Provider smoke 才能形成外部
服务兼容性的发布证据。

`backend/tests/runtime_log_redaction.rs` 还以真实后端进程、动态端口、
临时 `HERMES_HOME` 和 `RUST_LOG=trace` 验证 stdout/stderr 不包含生成的
desktop token、错误 Bearer、凭据样式请求头或 Profile secret。

### 历史性能基线（schema v1）

以下 10 秒、4 worker 数据由加固前的 schema v1 运行器产生，仅保留为历史
性能基线。它不能证明当前 schema v2 运行器的 stdin token、动态端口握手或
跨代 token 扫描；这些控制由后续加固回归单独验证。

执行时间：2026-07-18 07:45:23 至 07:45:46（Asia/Shanghai；原始 UTC 时间见结果文件）。

执行命令：

```powershell
pwsh -NoProfile -File .\scripts\verify-backend-runtime.ps1 `
  -SkipBuild -DurationSeconds 10 -Concurrency 4 -SampleIntervalSeconds 1 `
  -IncludeFaultChecks `
  -ResultPath .\logs\phase4\runtime-short-2026-07-18.json
```

`-SkipBuild` 表示这次运行使用已在同一工作区构建的 debug 二进制。后续从源代码复测时应省略该参数，以先执行 `cargo build`。原始、无 token 的 JSON 结果在 `logs/phase4/runtime-short-2026-07-18.json`；二进制 SHA-256 为 `18D5B954F44D2BC99954309E04152A0516A63517C0511FABAE6276375AD18B83`。

环境：Windows NT 10.0.26200.0，PowerShell 7.5.8，20 个逻辑处理器，backend 0.1.0。该次 schema v1 运行创建临时 `HERMES_HOME`、预选随机 `127.0.0.1` 端口和一次性测试 token；完成后移除临时目录。当前运行器已移除该预选端口流程。

| 检查 | 实测结果 |
| --- | --- |
| 未认证 `GET /health` | `200`，返回 `status=ok` |
| 未认证 `GET /api/v1/capabilities` | `401`，含 Bearer challenge |
| 认证 `GET /api/v1/capabilities` | `200`，`contractVersion=v1` |
| `tauri://localhost` CORS preflight | `200`，返回允许 origin |
| 非信任 Origin preflight | `200`，未返回 `Access-Control-Allow-Origin` |
| `/health` 并发只读负载 | 1,544 请求，0 失败，154.4 req/s |
| `/api/v1/capabilities` 并发只读负载 | 966 请求，0 失败，96.6 req/s |
| 受控停止后的旧端口 | 新 HTTP client 连接失败，符合预期 |
| 使用同一临时数据目录重启 | `/health=200`，认证 capabilities=`200` |
| 结果文件 token 扫描 | 通过；未发现测试 token 或 Bearer 值 |

该脚本不会创建会话、消息、Run 或外部 Provider 请求；因此并发结果仅覆盖启动、HTTP middleware、认证、CORS 和 capabilities 的只读路径。

### schema v2 运行器加固回归

2026-07-18 在当前 Windows 主机上显式使用
`1.88.0-x86_64-pc-windows-msvc` 执行了 10 秒、1 worker、含 fault restart
的短测。测试前在父环境放入了错误的 `SYNTHCHAT_DESKTOP_TOKEN`；后端仍以
stdin 收到的新 token 完成认证，证明 verifier 启动子进程时删除了继承值。

```powershell
$env:SYNTHCHAT_DESKTOP_TOKEN = [Guid]::NewGuid().ToString('N')
pwsh -NoProfile -File .\scripts\verify-backend-runtime.ps1 `
  -SkipBuild -CargoExecutable cargo `
  -RustToolchain 1.88.0-x86_64-pc-windows-msvc `
  -DurationSeconds 10 -Concurrency 1 -SampleIntervalSeconds 1 `
  -IncludeFaultChecks
```

| 检查 | 实测结果 |
| --- | --- |
| `/health` 与认证 capabilities workload | 两段均 0 失败 |
| 每代 bind | `SYNTHCHAT_BACKEND_ADDR=127.0.0.1:0`；严格解析有界 stdout handshake 后才发起 HTTP |
| 受控停止 | 先关闭 stdin，旧端点连续探测均不可用 |
| fault restart | `tokenRotated=true`；本次 `portChanged=true` |
| 跨代认证 | 新 token 成功；旧 token 返回 `401` |
| 捕获结果凭据扫描 | 未发现父环境值、任一代生成 token 或 Bearer 值 |

当前 PowerShell 7.5.8 与内置 Windows PowerShell 5.1.26100.8875 还分别实际
执行了 1 秒、1 worker 的 schema v2 smoke（无故障注入），两段 workload 均
0 失败。两种 shell 的 Parser AST 检查也通过。这证明当前脚本可在本机两种
PowerShell 运行，不代表 macOS/Linux 或旧 Windows 版本已验证。

### Windows Credential Manager 原生回归

2026-07-18 18:48（Asia/Shanghai）在当前 Windows 用户会话中使用
`rustc 1.88.0 (x86_64-pc-windows-msvc)` 实际执行了
`backend/tests/windows_system_keychain.rs`。该测试以
`ProfileService::with_system_store` 连接真实 Windows Credential Manager；每次运行创建临时
Hermes home，并分别生成随机、合法且唯一的 Profile ID、Idempotency-Key、secret name 和
secret value。

实际执行命令：

```powershell
$env:SYNTHCHAT_RUN_NATIVE_KEYCHAIN_TESTS = '1'
try {
  cargo +1.88.0-x86_64-pc-windows-msvc test `
    --manifest-path backend/Cargo.toml `
    --test windows_system_keychain `
    windows_credential_manager_round_trip_is_persistent_and_disk_safe `
    -- --ignored --exact
} finally {
  Remove-Item Env:SYNTHCHAT_RUN_NATIVE_KEYCHAIN_TESTS -ErrorAction SilentlyContinue
}
```

| 检查 | 实测结果 |
| --- | --- |
| 默认测试选择 | `0 passed; 1 ignored`；未启用时不访问或污染系统 keychain |
| Profile 创建与 secret 写入 | 通过；写入状态为 `configured=true`、`storage=osKeychain` |
| 当前 service 列表读取 | 通过；随机 secret 显示为 configured |
| 重建 `ProfileService` 后读取 | 通过；新实例仍从原生 store 读取为 configured |
| 原生值核对 | 独立 Windows Credential Manager probe 精确比对随机 secret 的原始字节 |
| 临时 Hermes home 全树明文扫描 | 写入后、service 重建后及删除后均通过 |
| 删除 | 通过；新 service 和原生 credential entry 均确认未配置 |
| 清理 | Drop 守卫经 service 和原生 store 双路径幂等清理 secret，并删除临时 Profile/home |
| 定向执行 | `1 passed; 0 failed`，测试耗时 `0.09s` |
| 静态门禁 | backend `cargo fmt --check`、`cargo check --all-targets`、`clippy --all-targets -D warnings` 均通过 |

测试和命令输出均不包含随机 secret。该证据覆盖当前 Windows 用户的 create、put、list、
service 重建读取、delete 和磁盘无明文，不代表跨用户权限隔离、重启持久性或其他操作系统。

## 可重复执行

短时回归：

```powershell
pwsh -NoProfile -File .\scripts\verify-backend-runtime.ps1 `
  -RustToolchain 1.88.0-x86_64-pc-windows-msvc `
  -DurationSeconds 10 -Concurrency 4 -SampleIntervalSeconds 1 `
  -IncludeFaultChecks `
  -ResultPath .\logs\phase4\runtime-short.json
```

Mixed verifier 自检与真实 backend smoke：

```powershell
npm run verify:mixed-runtime
node scripts/verify-mixed-runtime.mjs --smoke `
  --output .\logs\phase4\mixed-runtime-smoke.json
```

以下命令是后端 health/capabilities 长稳基线的约 8 小时总时长示例（两个
workload 各运行 4 小时），**截至本报告编写时尚未执行**；它不覆盖 mixed
Run/SSE/SQLite/工具路径：

```powershell
pwsh -NoProfile -File .\scripts\verify-backend-runtime.ps1 `
  -RustToolchain 1.88.0-x86_64-pc-windows-msvc `
  -DurationSeconds 14400 -Concurrency 8 -SampleIntervalSeconds 30 `
  -IncludeFaultChecks `
  -ResultPath .\logs\phase4\runtime-soak-8h.json
```

脚本把延迟样本限制为每 worker 2,048 条并用 reservoir sampling 计算长运行分位数，避免测试工具本身因持续保留全部请求延迟而造成内存增长。结果文件保留进程资源采样、计数、分位数和有限失败摘要，不保存端口、请求 Authorization 头或 token。写盘或返回结果前，脚本在内存中序列化并扫描初始与 fault-restart 的全部生成 token，同时拒绝任何 Bearer credential。

## 未覆盖与验收缺口

- 已执行 12 条 Playwright UI E2E；OpenAI-compatible Provider 已通过本地真实
  HTTP/SSE fixture，审批已覆盖真实 Workspace write/read/search/patch，
  Clarification、stdio MCP、
  前台 Terminal 和真实 Python guardian 均已有 UI 证据。Browser 已覆盖真实
  Chromium navigate/snapshot、两次 owner-bound 一次性审批、`Runtime.evaluate`
  CDP 和 metadata-only 隔离下载；后台
  Terminal 双 Run、唯一 delivery、审批前零副作用和 kill 回收已有 UI 证据。
  仍缺带真实第三方凭据的 Provider smoke。
- 已验证 SSE/Run 的完成态与运行中崩溃恢复；macOS/Linux 和三平台打包
  产物中的进程生命周期仍未验证。
- 已重跑 Rust、Desktop、Frontend 全矩阵及前端生产构建，并生成/静态审计
  当前 Windows NSIS 开发包；clean-account 安装/启动/升级/卸载、签名和公证
  仍未完成。
- 当前 Windows 用户的真实 Credential Manager 已验证；macOS/Linux、
  跨用户权限隔离和系统重启持久性仍未验证。
- 已执行早期 text-only 30 分钟与 60 分钟 backend mixed pilot（分别
  1,094/1,094、2,238/2,238 成功），以及严格 v2 4/4 smoke 和 60 秒
  170/170 资格测试；v2 8 小时 mixed 长稳、最终泄漏趋势和容量上限仍未完成。
  v2 `scripts/verify-mixed-runtime.mjs --duration-seconds 28800` 支持周期
  `session_search`、逐 Run SSE/工具契约和约 5,762 条全窗口资源样本；正式运行
  已启动，但完成报告与人工 RSS review 之前仍是 open gate。统一 task
  registry、立即 lease 释放与共享关闭 deadline
  已通过定向和完整矩阵；长稳仍须验证持续并发 create/queue、Provider、
  Terminal/process 与关闭交错。

因此当前结果证明本机主要功能与安全边界可重复运行，但仍不能关闭阶段四验收门。
