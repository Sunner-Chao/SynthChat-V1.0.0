# 阶段四性能报告

**结论：30 分钟只读基线、30/60 分钟真实 mixed pilot 与 4/4 mixed smoke 通过；60 分钟后半窗斜率已明显下降，但仍要求 8 小时 soak，尚没有足够证据证明无泄漏或容量目标。**

## 实测基线

数据来源：`logs/phase4/runtime-short-2026-07-18.json`。执行环境是 Windows NT 10.0.26200.0、PowerShell 7.5.8、20 个逻辑处理器、backend 0.1.0，二进制 SHA-256：`18D5B954F44D2BC99954309E04152A0516A63517C0511FABAE6276375AD18B83`。该文件由 schema v1 运行器产生；下列数值只作为历史短时性能基线，不作为当前 schema v2 启动安全控制的证据。加固后的 schema v2 已另行完成 10 秒、1 worker、含 restart 的正确性短测且两段 workload 均 0 失败，但该次没有作为容量样本使用。

另有 schema v2 的 `runtime-pilot-30m-2026-07-18.json`：`/health` 与认证
capabilities 各顺序运行 900 秒、8 workers，分别完成 99,233 与 90,486 个
请求且均为 0 failure。最终 post-cleanup 证据
`mixed-runtime-final-2026-07-20.json` 使用真实 backend 和本地确定性 Provider
完成 4/4 Profile/Session/Run/SSE/FTS 循环且清理通过；总命令约 49.7 秒，实际
四次 workload 约 1.25 秒，其余主要为 build/start。前者仍是只读顺序负载，
后者仅是有界正确性 smoke；二者都不能证明真实
混合长稳、泄漏趋势或容量上限。

当前 Backend/Desktop/Frontend 完整测试矩阵与 Playwright 12/12（最新完整本地组合运行
43.0 秒）均已通过；扩展后的 Files 四工具单用例 1/1（3.0 秒）、Browser
download 单用例 1/1（5.2 秒）、与 Code
组合 2/2（8.7 秒）、最小交错矩阵 4/4（21.8 秒）及 Rust 定向矩阵 9/9 也通过。
但这些只证明正确性和测试间生命周期
清理，不能作为吞吐、泄漏或长稳结论。

### 后台 Terminal 关闭回归

2026-07-19 在同一 Windows 主机上连续执行新的双 Run 后台 Terminal E2E：

| 运行 | Playwright 测试体 | 含构建、启动和清理的命令总时长 | 残留 |
| --- | ---: | ---: | --- |
| 1 | 8.8 s | 22.8 s | backend 0、fixture 0、runtime 目录 0 |
| 2 | 7.0 s | 19.8 s | backend 0、fixture 0、runtime 目录 0 |
| 3 | 7.0 s | 18.6 s | backend 0、fixture 0、runtime 目录 0 |

修复前的失败运行曾在 teardown 停留 708.8 秒；修复后 3 次均快速完成，
并证明 pending delivery 的 SSE 在进程终止、通知持久化和 sender 关闭之间不再
互相等待。Terminal 两条用例组合运行 2/2、13.7 秒通过，完整 Playwright
12/12、43.0 秒通过。该数据是生命周期正确性与回归时长证据，不是并发吞吐
或内存泄漏结论。

### 30 分钟只读 pilot 与 mixed smoke

| 证据 | 工作量 | 结果 | 资源趋势/限制 |
| --- | --- | --- | --- |
| `/health` pilot | 900 秒、8 workers、99,233 请求 | 0 failure，110.259 req/s | Working Set +2,097,152 B，Private Bytes +1,896,448 B，handles +2 |
| capabilities pilot | 900 秒、8 workers、90,486 请求 | 0 failure，100.54 req/s | Working Set +28,672 B，Private Bytes -118,784 B，handles +5 |
| mixed smoke | 4 次 Run，Profile/Session/SSE/FTS | 4/4，Provider/workload 0 failure | 总命令约 49.7 秒、workload 约 1.25 秒；只证明运行器与基本混合路径正确，不能做泄漏拟合 |

### 30 分钟真实 mixed pilot（2026-07-20）

结果文件：`logs/phase4/mixed-runtime-pilot-30m-2026-07-20.json`，文件 SHA-256
为 `D0822FF9A5978B8F763D5A07CBEABACB8CE18C8329F6B94168B700CC20C8DD60`。
配置为 Node 22.14.0/npm 10.9.2、1800 秒、2 个并发 worker、3 秒 cycle
delay、5 秒资源采样。实际 workload 窗口为 1,803.24 秒。

| 指标 | 结果 |
| --- | ---: |
| iterations started/completed/successes | 1,094 / 1,094 / 1,094 |
| workload/provider failures | 0 / 0 |
| Provider requests / max active | 1,094 / 2 |
| message.started/completed、run.started/completed | 1,094 / 1,094、1,094 / 1,094 |
| resource samples / dropped / skipped | 356 / 0 / 4 |
| backend RSS first / last / peak | 33.71 / 44.34 / 48.32 MiB |
| backend RSS slope | full window +20.87 MiB/h; last 10 min +11.61 MiB/h |
| cleanup | forced=false; backend/provider stopped; temp removed |

The pilot proves sustained correctness and bounded cleanup for this workload,
not absence of leaks. RSS growth can reflect allocator caches, load shape, or a
real leak, so the 8-hour soak must repeat the mixed Run/SSE/SQLite/tool/shutdown
interleaving and review per-hour resource slopes before any release claim.

旧的 30 分钟 pilot 两段负载顺序执行，总窗口约 30 分钟；新的 mixed pilot
已从 30 分钟扩展到 60 分钟并持续覆盖真实 Run/SSE/SQLite 路径，但仍须完成
8 小时 soak，才能关闭泄漏趋势与容量上限审查。

### 60 分钟真实 mixed extension（2026-07-20）

结果文件：`logs/phase4/mixed-runtime-pilot-60m-2026-07-20.json`，文件 SHA-256
为 `8551D96F6D133564A70BFE37E625777DE3C74DC0069C144593D2D12E2827D211`，
大小 123,935 bytes。配置为 Node 22.14.0、Rust
`1.88.0-x86_64-pc-windows-msvc`、3,600 秒、2 个并发 worker、3 秒 cycle delay
和 5 秒资源采样；实际 workload 窗口为 3,602.595 秒。

| 指标 | 结果 |
| --- | ---: |
| iterations started/completed/successes | 2,238 / 2,238 / 2,238 |
| workload/provider failures | 0 / 0 |
| Provider requests / max active | 2,238 / 2 |
| resource samples present / RSS available / unavailable | 719 / 715 / 4 |
| resource dropped / skipped | 0 / 1 |
| backend RSS first / last / peak | 31.95 / 45.06 / 51.70 MiB |
| backend RSS slope | full window +11.00 MiB/h; last 30 min +4.70 MiB/h |
| final-window sensitivity | last 20 min -5.35 MiB/h; last 10 min +3.02 MiB/h |
| Run SSE p99 / max | 352.12 / 4,088.70 ms |
| Session create p99 / max | 37.25 / 3,065.08 ms |
| cleanup | forced=false; backend/provider stopped; current-run temp removed |

斜率使用所有 `backendRssBytes != null` 的样本对 elapsed hours 与 MiB 做普通
最小二乘拟合；缺失的 4 个 RSS 点不作为 0 或稳定值。相比 30 分钟全窗
+20.87 MiB/h，60 分钟全窗与后半窗斜率明显下降，且最后 20/10 分钟在正负间
波动，符合 allocator/cache 平台化的可能性；但单个 60 分钟窗口、51.70 MiB
峰值以及 SSE/Session create 的孤立长尾仍不足以关闭 8 小时泄漏与容量门。
该 60 分钟文件生成时尚未单列缺失计数，`4` 是从 719 个原始样本中结构化
统计得到；随后运行器已增加 `resources.backendRssUnavailable`，并通过自测和
4/4 smoke，后续 8 小时结果会直接记录该值。

### 历史短时只读基线明细

负载在同一个本机 loopback backend 上顺序执行两段，每段 10 秒、4 个并发 worker、请求超时 10 秒。未访问外部网络或 Provider，未创建会话/Run。每秒采样一次 Working Set、Private Bytes 和 Windows handle count；延迟是每个请求的端到端 HTTP client 耗时。

| 路径 | 请求/成功/失败 | req/s | p50 | p95 | p99 | 最大值 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `GET /health` | 1,544 / 1,544 / 0 | 154.4 | 14.259 ms | 86.886 ms | 157.849 ms | 442.740 ms |
| `GET /api/v1/capabilities`（Bearer） | 966 / 966 / 0 | 96.6 | 31.488 ms | 95.287 ms | 196.265 ms | 314.535 ms |

| 路径 | 采样数 | Working Set 首/末/峰值 | Private Bytes 首/末/峰值 | Handle 首/末/峰值 |
| --- | ---: | ---: | ---: | ---: |
| `/health` | 11 | 23,916,544 / 24,350,720 / 24,563,712 B | 4,984,832 / 5,210,112 / 5,439,488 B | 206 / 204 / 207 |
| `/api/v1/capabilities` | 11 | 24,408,064 / 24,854,528 / 24,895,488 B | 5,267,456 / 5,726,208 / 5,836,800 B | 206 / 206 / 208 |

短时间窗口中的 Working Set 变化分别为 +434,176 B 与 +446,464 B，Private Bytes 变化分别为 +225,280 B 与 +458,752 B，句柄净变化为 -2 与 0。这是有限时间内的观测值，不是泄漏结论。

## 方法与限制

运行器为 `scripts/verify-backend-runtime.ps1`，其运行时边界如下：

- 当前 schema v2 运行器为每代设置 `SYNTHCHAT_BACKEND_ADDR=127.0.0.1:0`，由 OS 在 bind 时原子分配端口；它限长读取并严格校验 `SYNTHCHAT_BACKEND_READY 127.0.0.1:<port>` 后才执行 health/auth，不再“先选端口再释放”。
- 每代 token 只经子进程 stdin 和内存中的 Authorization header 传递；后端子进程环境显式删除继承的 `SYNTHCHAT_DESKTOP_TOKEN`。fault restart 使用新 token，结果只记录轮换/端口变化等布尔断言。
- 写盘或返回前会序列化并扫描所有代 token 和 Bearer pattern。报告不记录实际端口、Authorization header 或 token。
- 临时 `HERMES_HOME` 在默认路径下运行结束后删除；`-KeepHermesHome` 仅用于明确授权的故障诊断。
- 连续延迟会保留每 worker 最多 2,048 条 reservoir sample；本次短测总样本量小于限制，p50/p95/p99 来自全部观测。
- 资源指标来自 backend 主进程，不包括前端、桌面 shell、keychain 服务、Provider、浏览器或任何外部进程。
- 两个 workload 顺序而非混合执行，不能反映真实聊天与工具调用混合流量。
- 未显式配置 Python 时，首次 capabilities 请求仍可能同步扫描较长的 Windows
  `PATH`；本轮安全测试通过隔离 `PATH` 消除了无关抖动，但生产探测应在启动期
  预热并让请求路径只读缓存结果。

## 仍需执行

30 和 60 分钟 mixed pilot 已完成，但二者使用的是早期 text-only Provider
workload。当前 schema v2 驱动器在每 10 个全局 iteration 中加入一次只读
`session_search` 工具回路，严格校验唯一工具定义、Provider continuation、当前
Session 的完整结果、逐 Run SSE id/envelope/连续 sequence/工具生命周期，以及
跨 Run 的 Provider 和事件计数守恒。v2 自测、4/4 real-backend smoke 和 60 秒
资格测试均通过；60 秒测试完成 170/170 iterations、17/17 工具回路和 187/187
Provider 请求，零失败且完整清理。历史 30/60 分钟 RSS 仍是有效的旧 workload
观测，但不能与 v2 工具 workload 的曲线直接同比。

下一项本机性能门是 8 小时 v2 mixed 长稳，用于验证包含
Run/SSE/SQLite/FTS/周期只读工具的测试驱动器、资源采样与错误摘要能否持续
稳定，并判断 RSS 后半窗是否真正平台化。

以下命令用于仅 health/capabilities 的约 8 小时总时长基线（两个 workload 各
4 小时），但尚无实际结果，不能据此声明 mixed 长稳通过：

```powershell
pwsh -NoProfile -File .\scripts\verify-backend-runtime.ps1 `
  -RustToolchain 1.88.0-x86_64-pc-windows-msvc `
  -DurationSeconds 14400 -Concurrency 8 -SampleIntervalSeconds 30 `
  -IncludeFaultChecks `
  -ResultPath .\logs\phase4\runtime-soak-8h.json
```

当前 Windows 开发树 v2 长稳使用以下 debug-binary 命令；5 秒采样保留 5,762
条全窗口资源记录。2026-07-20 已启动该命令，但在报告完成与 RSS 趋势复核前
不得记为通过，也不得将它当作 release-candidate evidence：

```powershell
node scripts/verify-mixed-runtime.mjs `
  --duration-seconds 28800 --concurrency 2 --cycle-delay-ms 3000 `
  --max-failures 25 --latency-sample-limit 5000 --provider-delay-ms 10 `
  --resource-interval-ms 5000 --resource-sample-limit 5762 --resource-samples `
  --tool-every-iterations 10 `
  --backend-bin backend/target/debug/synthchat-hermes-backend.exe --skip-build `
  --output docs/release-evidence/mixed-runtime-8h.json
```

v2 raw 报告在后端启动前绑定平台、Node/Rust、Git HEAD/tree/dirty 状态、验证器
和后端 SHA-256，以及有效配置、argv 和相关环境覆盖的脱敏哈希。完成后还必须
由 `scripts/verify-mixed-runtime-evidence.mjs` 交叉校验 canonical raw、候选
manifest、资源可用率/时间轴和人工 RSS review；dirty 开发树结果不能冒充
release-candidate。正式候选必须先在 clean commit 上 locked 构建 release
backend，再按 [release.md](release.md) 的目标平台命令重跑，并把 backend hash
与 sidecar/包内 attestation 绑定。长稳执行后应审查：请求失败率、
p99 漂移、每小时 Working Set/Private Bytes/handle 趋势、进程退出/重启、临时
目录清理，以及是否存在工具或 Provider 失败。该 workload 已覆盖流式聊天、
SSE、SQLite/FTS 与只读 `session_search`，但仍需单独测量 SSE 重放、队列/重启、
文件写入、终端/代码执行、MCP 与浏览器能力。

统一 Run task registry、PreserveRuns 的 fenced lease 释放、前台 Terminal
admission gate 和共享 shutdown deadline 已实现并通过定向/完整矩阵。长稳仍需
覆盖持续 create/queue、忽略取消的 Provider、前后台 Terminal/process、异步
delivery 与 shutdown 的交错，确认该边界在小时级压力下保持有界。
