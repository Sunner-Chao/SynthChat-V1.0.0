# Hermes Agent 0.18.2 terminal/process and execute_code contract

本文锁定 Hermes Agent commit
`3f2a389c7e1f1729cad91ae63c26fb08c7753c74` 的模型侧契约，并定义纯 Rust
实现采用的安全边界。当前 Rust registry 已在 `terminal` Toolset 下注册
`terminal` 和 `process`，并在 `code_execution` Toolset 下注册 `execute_code`；
RunService 通过异步 executor 与持久 tool journal 执行它们。本文同时记录已实现
契约、进程生命周期加固和仍然保留的权限边界。

`terminal`/`process`/`execute_code` 是 Run 内供模型调用的工具，不是新的公开 REST
进程管理或代码执行 API。当前能力报告为 `toolExecution=true`、`toolProgress=true`、
`approvals=true`、`clarifications=true` 和 `asyncToolDelivery=true`（仅在
Run runtime 可用时）；
`codeExecution` 只在 Run/session runtime 与 host Python 3.8+ probe 都可用时为 true。
host Python 只执行用户批准的代码工具，Hermes Agent runtime 仍由 Rust 实现。

## Authority boundary

`Command.current_dir(workspace)` 只设置初始 cwd，不限制 shell 后续访问 `..`、绝对
路径、symlink、网络或宿主进程。没有 OS/container sandbox 时，本地 terminal 必须
明确视为完整宿主权限：

- 每次 `terminal` 调用都要求 durable `once/deny` 审批；
- `process kill/write/submit/close` 每次也要求新的 durable 审批；
- 整段 `execute_code` 脚本要求一次 durable `once/deny` 审批，审批/claim 前不得启动
  guardian；该审批同时覆盖脚本内动态白名单中的 nested mutating RPC，不创建第二个
  公开审批；
- 不得把 Workspace cwd 描述为 filesystem containment；
- approval claim 只承诺单次授权和至多一次 spawn 尝试，不能宣称 OS side effect
  exactly-once；
- 进程可能已启动后不得自动重试，backend 重启不得重放命令。

真正的 Workspace confinement 必须另行实现 Windows sandbox/AppContainer、macOS
sandbox/container 或 Linux namespace/container，并单独建模网络、挂载和宿主控制能力。

## Tool inputs

| Tool | Required | Optional |
| --- | --- | --- |
| `terminal` | `command` | `background=false`, `timeout`, `workdir`, `pty=false`, `notify_on_complete=false`, `watch_patterns` |
| `process` | `action` | `session_id`, `data`, `timeout`, `offset`, `limit` |
| `execute_code` | `code` | 无；mode/timeout/tool-call limit 来自 ProfileConfig |

`process.action` 固定为
`list/poll/log/wait/kill/write/submit/close`。Rust 契约收紧如下：

- `additionalProperties=false`，完整参数不超过 64 KiB；
- command 不超过 32 KiB；Workspace 相对 workdir 不超过 1,024 bytes；
- workdir 拒绝绝对路径、`..` 和非可移植组件；Workspace root 不得变为
  symlink/reparse point，canonical workdir 必须仍位于 root 内且是目录；
- watch pattern 最多 16 个且单项不超过 256 chars；stdin 单次不超过 32 KiB；
- 前台默认 timeout 180 秒、显式上限 600 秒，并取 Run 剩余 deadline 的较小值；
- `pty=true` 当前在 prepare 阶段被拒绝，调用方必须使用
  `pty=false`；尚无 PTY/ConPTY 执行语义；
- `notify_on_complete=true` 或非空 `watch_patterns` 只在
  `asyncToolDelivery=true`、`background=true` 时接受，且二者不得组合；一项
  completion 或 watch 投递记录会与 process 在同一事务创建，不得静默降级。
- `execute_code.code` 为 1..60 KiB、拒绝 NUL 和未知字段；Profile timeout 为
  1..600 秒（默认 300），nested RPC 上限为 1..100 次（默认 50）；
- `execute_code` 只在探测到 Python 3.8+ executable 时注入；WindowsApps alias、旧版本、
  probe 失败或 2 秒超时都视为 unavailable。

Provider 仅在 Profile 启用 `terminal` Toolset 时看到这两个工具；
`terminal` 还要求 Run 绑定同 Profile 下已注册且当前可用的
Workspace。`process` 的所有查询和变更均固定到当前 Run 的
`(profile_id, session_id)` owner。`execute_code` 另要求 `code_execution`
Toolset enabled、`codeExecution=true`，其 nested schema 只列出同一 Run
实际可用的固定七项上限子集。

## Dynamic risk

Registry 为 `process` 保留保守的静态 `ApprovalRequired` fallback；实际
`prepare()` 会先严格解析 action，再生成动态风险：

| Operation | Risk |
| --- | --- |
| `terminal` foreground/background | ApprovalRequired |
| `process list/poll/log/wait` | ReadOnly |
| `process kill/write/submit/close` | ApprovalRequired |
| `execute_code` whole script | ApprovalRequired |

灾难性命令或显式 deny policy 是 hard deny，不能靠用户审批绕过。prepared call 必须
绑定 Run ID、tool/call、invocation checkpoint、原始参数 SHA-256、
Profile/Session/Workspace owner 和一次性 approval claim。

审批显示与公开事件使用两个明确分离的摘要边界：

- `input_summary` 是不含 command/stdin/code 正文的通用摘要，可进入 `tool.started`、
  `approval.required` SSE、Message 和事件回放；
- `approval_summary` 仅用于当前 pending approval：command/stdin/code preview 先按 Profile
  secret 与常见 token/Bearer 模式脱敏，再转为单行、转义控制字符，并把 preview 正文
  限制为 240 chars，
  并附加参数 SHA-256 短摘要。它可进入 approval ledger 与 `Run.pendingAction` 供审批 UI
  展示，但不会复制到 `approval.required` 事件或 Message；
- 当前 Session schema v10 保留 v8 引入的 approval ledger，持久化完整参数 SHA-256 与 owner 快照；durable claim 同时核对
  Run ID、Profile/Session/Workspace、call/tool、invocation checkpoint、当前 invocation 和
  prepared binding，不能用显示用短摘要替代执行绑定。

Schema v10 保留 v9 引入的 bound immutable clarification ledger。它不扩大 terminal/process/code
权限，但与 approval 共用相同的 Run/call/checkpoint/原始参数 SHA-256 绑定原则；私有 answer
只交给 single-use Provider continuation，不进入公开 SSE、Message、Problem 或日志。取消按
`clarification.resolved -> tool.failed -> run.cancelled` 收敛；deadline、Provider/本地错误、
backend 重启等任意非取消终止中断均按 `resolvedBy=failure` 在 `run.failed` 前结束
clarification 和 tool，而不是只在重启恢复时处理。

## Results and lifecycle

前台 terminal 返回 `{output, exit_code, error}`。非零退出和请求级 timeout 是有效
ToolOutput，允许 Provider 继续推理；timeout 使用 `exit_code=124`。timeout、Run cancel
和 backend shutdown 都通过已拥有的 Windows Job Object 或 Unix process group 收敛所跟踪
进程树，root 退出时也先清理该树，再等待输出 drain。公开取消状态走
`tool.failed -> run.cancelled`。这保证实现管理范围内的回收顺序，不等同于 OS sandbox，
也不能回滚命令已经产生的文件、网络或其他宿主副作用。

后台 terminal 立即返回 `session_id`、PID 和启动状态。后台进程状态机为：

```text
starting -> running -> exited | killed | lost
        \-> failed_start | lost
```

SQLite 中已写入的终态不可变，以 CAS 使自然退出、kill 和 cancel 最多有一个
状态迁移成功。终态写入失败会最多重试三次并重新读取；仍失败时内存投影 fail closed
为 `lost`，后续重启 reconciliation 再依据强 identity 处理持久候选项。持久记录至少绑定
`processId/profileId/sessionId/workspaceId/creatorRunId/callId`、命令 hash、脱敏预览、
PID、强 OS identity、时间、状态、completion reason 和通知配置。所有查询都限制为
同一 Profile + Session；不属于调用方的 ID 返回 `not_found`，避免所有权枚举。

后台启动采用 reservation + guardian + launch lease：

1. `BEGIN IMMEDIATE` 事务在全库统计 `starting|running`，并在同一事务插入
   `starting` 记录；全局 64 个活动进程是原子容量边界。
2. backend 只 spawn guardian，取得 PID、平台 lifetime handle 与强 identity 并持久化
   `running`；实际 shell script 尚未执行。
3. guardian 完整验证带 magic/version/length 的有界 launch frame 后才启动 shell；script
   通过控制 pipe 传递，不出现在命令行。frame 截断、字段未知、路径/env/大小非法均在
   shell spawn 前拒绝。
4. 后台 terminal 的 tool result/event 先持久化，RunService 随后发送 `commit_launch`。
   commit 前的 Run cancel、deadline 或 supervisor 断开会回滚 lease 并终止进程树；
   guardian 观察到父控制 pipe 断开时也终止命令树。

launch lease 解决的是受管进程交接与取消顺序，不是数据库事务包裹的外部副作用：shell
收到合法 frame 后、commit 前仍可能已经执行部分命令，因此不能宣称 exactly-once 或回滚。

process 结果语义：

- `list` 返回 owner 范围内的 process metadata 和脱敏 preview；
- `poll` 返回 `running|exited`、尾部输出和可选终态字段；
- `log` 默认末 200 行并支持 offset/limit；
- `wait` 返回 `exited|timeout|interrupted|not_found`，等待超时或取消不杀后台进程；
- `kill` 返回 `killed|already_exited|not_found|error`；受管路径完成 tree termination、
  root wait、tree cleanup 与有界 pipe drain 后才持久化 `killed`。重启后的 detached 路径
  要求强 identity，平台终止成功且该 identity 不再匹配后才写入 `killed`；
- `write/submit/close` 仅允许 running 且 stdin 可用的进程。

## execute_code direct process

`execute_code` 不经 shell 拼接用户代码，也不启动 Python Agent service。Rust 将原始脚本和
按实际可用工具生成的 `hermes_tools.py` 写入私有 staging，再以 direct guardian frame 传递
Python executable、脚本路径 argv、cwd、清理后的环境和可选 `CodeRpcBootstrap`。代码正文
不进入 argv；generic environment 拒绝 token-bearing 和 `SYNTHCHAT_*` 名称，只有 guardian
能在 `env_clear()` 后把每次执行的 loopback RPC port/token 投影到 direct child。bootstrap
token 在 Debug 中脱敏并在 Drop 时 zeroize。

解释器发现只接受探测成功的 Python 3.8+ 常规 executable，并拒绝 WindowsApps alias。
`project` mode 在 Run 有 Workspace 时以其为 cwd，否则回退 staging；`strict` 始终使用
staging。两者都允许脚本直接使用宿主 filesystem、network、`subprocess` 和 `ctypes`，所以
mode 只是 cwd 策略，不是 OS/container sandbox。子进程环境使用 terminal 相同的最小
allowlist，并额外移除 `PYTHONSTARTUP`、`PYTHONINSPECT`、`PYTHONBREAKPOINT` 等注入点；
Profile/API key、token、password、proxy、agent socket、数据库连接和未列入项不继承。

RPC listener 只绑定 IPv4 loopback，以每次执行随机 token 常量时间认证；请求/响应各限制
64 KiB。Rust 根据当前 Profile Toolset、Workspace 和 Web readiness 动态计算允许集合，
再与固定的七项上限求交：`web_search`、`web_extract`、`read_file`、`write_file`、
`search_files`、`patch` 和 foreground-only `terminal`。nested 调用逐项执行原有严格 schema、
Workspace、SSRF、hard-deny、deadline、取消和输出策略；`terminal` 的 background、PTY、
notify 和 watch mode 在 RPC 边界拒绝。脚本整体 durable `once` claim 作为 nested mutating
调用的授权，不产生新的 pending approval。

Session schema v10 为 `tool_invocations` 增加 immutable `origin=provider|codeRpc`、
`parent_call_id` 和 `rpc_sequence`。`codeRpc` row 只允许绑定同一 Run/turn 中 still-running、
`origin=provider` 的 `execute_code` parent；sequence 必须在 1..100 内严格递增，
`(run_id,parent_call_id,rpc_sequence)` 唯一。nested 参数、结果和 checkpoint 只进入私有
journal；Provider continuation 的顶层 tool-call 查询显式筛选 `origin=provider`，公开
Run/SSE/Message 也不生成 nested tool events。deny 在 spawn 前终结并保持 nested row 为零。

direct supervisor 在 handoff 前取得 PID、强 identity 和平台 lifetime handle，输出 drain 与
guardian control pipe 都由其持有。cancel、Run deadline、script timeout 和 backend disconnect
收敛 Windows Job Object 或 Unix process group；未 settle 的 RAII Drop 同步发起强 identity
tree termination。Run 已进入 `cancelling` 后，journal 只接受 failed terminal，不接受晚到
completed 覆盖取消。该保证只覆盖受管树，不能回滚脚本此前已经产生的文件或网络副作用。

## Output and environment

stdout/stderr 必须并发 drain。前台保留 50,000-byte 的 40/60 head-tail window；后台
保留最后 200,000 bytes。增量 UTF-8 decoder 在 chunk boundary 保真，非法序列替换，
CRLF 规范化，清理 ANSI，再执行 secret redaction 和 provider 64 KiB 二次上限。
每个可见 retention boundary 额外保留 4 KiB redaction guard；live poll/log 在 drain
仍活动时扣留尾部 guard，防止最长 2,560-byte Profile secret 跨 chunk 或裁剪边界泄漏。
command、list preview、log、wait 和 kill 输出全部脱敏；不得复刻上游 `list` 未脱敏的
缺陷。root process 结束并完成 tree cleanup 后，pipe drain 最多等待 2 秒，超时则 abort
drain task，继承 pipe 的 descendant 不能无限阻塞 Run deadline 或 supervisor。

`execute_code` 使用同一 sanitizer/redactor，但 stdout 与 stderr 分开捕获：stdout 是
50,000-byte 的 40/60 head-tail，stderr 是独立 10,000-byte head-only。两个流都先完成
UTF-8/ANSI/CRLF/控制字符清理，再按 Profile secret 和常见 token/Bearer 模式脱敏；stderr
只在脚本非零退出时附加到 provider output。原始 code、stdout/stderr、nested 参数/结果和
文件内容只进入私有 invocation/provider continuation，不进入公开 SSE、Message、Problem
或普通日志。

后台输出只在 `ProcessManager` 内存中保留，不写入 SQLite。backend
重启后 process metadata 仍持久存在，但重启前的 `poll/log/wait` 输出不可
恢复，stdin supervisor 也不可恢复。

stdin 不由 lifecycle supervisor 直接写入：`write/submit/close` 经容量 4 的独立 writer
queue 发送最大 32 KiB 的 `write/close` 控制帧，每次 write/flush 最多 2 秒，外层 process
control 最多 10 秒。stdin backpressure 因此不会阻塞 kill、child exit 或 launch rollback。

环境按 `(profile, session, workspace)` 隔离，并以最小 allowlist 从 `env_clear()` 后重建：
只保留 PATH、用户/主目录、临时目录、locale、时区、CA 证书路径及 Windows 必需 runtime
变量，强制 `TERM=dumb`、`NO_COLOR=1`。token、key、password、credential、代理、agent
socket、数据库连接和 `SYNTHCHAT_*` 等未列入项不会继承；Windows 环境名按大小写不敏感
比较。显式 secret passthrough 是后续独立能力，不能默认复制宿主环境。

## Restart and platforms

- 前台 terminal 和 `execute_code` direct process 与 Run 绑定；后台进程在 launch commit 前
  也与该 Run 的 cancel/deadline 绑定。正常 shutdown 对内存 entry 和持久恢复候选项都只在
  强 identity 匹配时终止；direct process 不恢复、不重放，父连接关闭由 guardian 收敛树。
- 启动时检查 `starting|running` 记录。只有 detached、PID 存活且强
  identity 匹配的候选项保留为 `running`；其他候选项以 CAS 标记为
  `lost`。启动恢复路径不依据裸 PID adopt 或 signal；`lost` 只表示
  metadata 无法安全接管，不证明命令此前没有产生外部副作用。
- Windows 强 identity 使用 creation FILETIME；guardian 使用隐藏窗口，前台和后台
  进程都绑定 `KILL_ON_JOB_CLOSE` Job Object，detached tree kill 使用 `taskkill` 并等待
  tracked identity 退出。Windows terminal
  需要 Git Bash，尚未实现 ConPTY。
- Linux 强 identity 使用 boot ID + `/proc/<pid>/stat` start time；shell 使用
  bash，guardian/command 使用独立 process group，前台 guardian 还设置 parent-death
  signal。
- macOS shell 优先已配置的 `SHELL`，再回退 zsh/bash；guardian/command 使用独立
  process group，强 identity 使用 `proc_pidinfo` 返回的启动时间。恢复和 detached kill
  与其他平台一样要求 identity 匹配。

## Remaining guarantees and limitations

当前 guardian、launch lease、原子容量预留、有界 stdin/pipe、强 identity 和 tree kill
已封闭此前列出的常规生命周期窗口，但以下边界仍然成立：

- `toolExecution=true` 只表示 executor 可用，不是 filesystem confinement 或
  OS/container sandbox。获得审批的命令具有宿主权限；恶意命令主动脱离 Unix process
  group 或利用宿主能力的场景不属于普通 descendant 回收保证。
- guardian 与 launch lease 防止在 PID/identity 持久化前执行 script，并在父连接丢失或
  commit 前取消时终止受管树；它们不能撤销已经发生的外部副作用，也不把 shell 执行变成
  数据库 exactly-once 事务。
- 终态存储会重试并在内存 fail closed，但持续 SQLite 故障时仍不能保证立即写入持久终态；
  恢复后必须依赖 reconciliation，不能根据旧 `running` 字段推断 OS 活性。
- PTY/Windows ConPTY 尚未实现；`notify_on_complete`/`watch_patterns` 已实现为
  owner-bound、可恢复的一次性后台投递，但它们不改变 terminal 的宿主权限边界或
  shell side effect 的 exactly-once 限制。
- code RPC 当前使用单个已认证连接串行处理；多连接、畸形 frame 和高转义密度 50 KB
  stdout 的完整攻击边界矩阵仍需补齐，不能把现有 size check 描述为协议形式化验证。
- `recover_interrupted_runs` 当前保留 running invocation journal；backend crash 虽由 guardian
  收敛受管 direct tree，但 parent 或 nested `codeRpc` row 仍可能遗留 running 状态，后续需要
  独立 reconciliation，不能把进程回收等同于 journal 已完成终结。
- Windows、Linux、macOS 路径均有源码实现；发布前仍需各原生平台的长时间运行、backend
  hard-crash、深层 descendant 和 PID reuse/identity 回归，不能用单一开发平台测试替代。

## Rust module boundary

- `tools/terminal.rs`: schema、严格解析、动态风险、provider result/summary；
- `tools/runtime.rs`: registry、Toolset/Workspace 可用性、prepare 和审批绑定；
- `code_execution.rs`: interpreter probe、staging/helper、loopback RPC、nested executor 和私有结果；
- `processes/direct.rs`: direct guardian handoff、RAII tree lifetime 与 stdout/stderr 分流；
- `processes/manager.rs`: owner 隔离、状态机、action 和 CAS race；
- `processes/guardian.rs`: launch/control frame、父连接生命周期与 shell handoff；
- `sessions/process_store.rs`: schema v7 引入的 process journal（当前 Session schema v13）、spawn intent、completion/restart journal 与 v12 async delivery record；
- `sessions/{schema,run_store}.rs`: v8 approval、v9 clarification、v10 code RPC invocation binding 与 v12 delivery event settlement；
- `processes/output.rs`: bounded capture、UTF-8/ANSI/CRLF 和 redaction；
- `processes/shell.rs`: shell discovery、cwd/env snapshot；
- `processes/platform.rs`: 平台 spawn、identity、lifetime 和 tree kill；
- `runs/service.rs`: async executor、approval/clarification、deadline、cancel 和 journal 编排。

terminal 是异步 I/O 工具，不能在 `spawn_blocking` worker 上等待最多 600 秒。

## Verification status

当前 Rust 单元与 HTTP E2E 测试已覆盖：

1. 严格 schema、byte/count bounds、PTY 拒绝、异步投递模式约束和 `process` 动态风险。
2. durable `once` 审批、审批前零 guardian spawn/process row、通用/审批摘要分离、
   preview 脱敏/转义/限长，以及参数 hash 与 owner durable claim 绑定。
3. guardian 完整 frame 前零 shell spawn、script 不进入 argv、launch EOF/父断连回收。
4. 后台启动审批、原子全局容量预留、launch lease commit/取消回滚、只读 poll 与二次审批 kill。
5. 状态迁移 CAS、终态不可逆、存储重试、owner 隔离、重启 fail closed、强 identity 与 detached kill 退出核验。
6. 独立有界 stdin writer、背压时并发 kill、进程树取消、descendant pipe 有界 drain。
7. stdout/stderr 并发 drain、head-tail/rolling limits、UTF-8 chunk、ANSI/CRLF、4 KiB
   redaction guard 与 live-tail withholding。
8. Python version parsing、missing-interpreter capability、direct guardian typed bootstrap、RPC
   credential Debug redaction、动态七项白名单和 foreground-only terminal。
9. `execute_code` 审批后 Python + nested `read_file`、Profile secret 环境剥离、deny 后零
   spawn/零 nested invocation、cancel 后 tree/heartbeat 停止，以及代码、stdout、文件正文
   和 nested 参数不进入公开 Run/SSE/Message。
10. 后台 terminal 的 completion/watch 投递、显式 `process` kill、重启恢复与并发
    scheduler 的一次性结算；公开 `tool.delivery`、Run、Message 和 UI 均不投影 command、
    watch pattern 或 output。

2026-07-17 完整基线为 backend unit `246/246`、integration `91/91`（合计 `337/337`），
frontend `389/389`、desktop `1/1`；backend/desktop fmt 与 `clippy -D warnings`、OpenAPI drift、
TypeScript 和 Vite production build 均通过。这不是 Browser UI、压力/长稳或三平台原生
crash/process 发布矩阵的完成证明。

尚需补齐 PTY/Windows ConPTY，以及 Windows/Linux/macOS 原生发布矩阵中的 hard-crash
与长时间深层 descendant tree-kill 回归。`asyncToolDelivery` 仅在 Run runtime 可用时报告
为 true；`pty` 继续报告为不可用。

## Pinned evidence

本地 Rust 实现与验证：

- `backend/src/tools/{terminal,runtime}.rs`
- `backend/src/code_execution.rs`
- `backend/src/processes/{direct,guardian,manager,output,shell,platform}.rs`
- `backend/src/sessions/{schema,run_store,process_store}.rs`
- `backend/src/runs/service.rs`
- `backend/tests/{code_execution_contract,run_http}.rs`

上游参考：

- `tools/terminal_tool.py:958-979,2028-2056,2134-2158,2291-2351,2597-2625,2649-2818`
- `tools/process_registry.py:54-140,689-926,930-1105,1314-1670,1826-1993,2210-2329`
- `tools/environments/base.py:54-144,681-718,791-905,1048-1108`
- `tools/environments/local.py:130-425,550-624,737-783,961-1010,1167-1314`
- `tools/approval.py:414-451,592-806,2318-2359,2635-2687,2764-3012`
- `tests/tools/test_terminal_foreground_timeout_cap.py`
- `tests/tools/test_process_registry.py`
- `tests/tools/test_notify_on_complete.py`
- `tests/tools/test_watch_patterns.py`
- `tests/tools/test_local_background_child_hang.py`
