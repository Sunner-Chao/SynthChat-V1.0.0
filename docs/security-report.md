# 阶段四安全报告

**状态：代码、运行时、依赖和本地历史审计已执行；发现的历史凭据尚未整改，因此不是发布安全签核。**

本报告区分源码可证实的控制、实际运行验证以及仍需人工或跨平台验证的风险。macOS/Linux 密钥链、渗透测试、签名/跨平台发布制品和远端隐藏引用仍未验证。

## 已审计控制

| 范围 | 源码证据 | 结论 |
| --- | --- | --- |
| 本地监听 | `backend/src/config.rs:19` 定义通用开发默认值；`parse_bind_addr`（`:132`）拒绝非 loopback 地址 | 后端配置层不接受 `0.0.0.0` 或公开 IP bind；Desktop 和验证器使用环境变量请求 `127.0.0.1:0`。 |
| 桌面 token | `validate_token`（`backend/src/config.rs:147`）校验长度和可见 ASCII；`require_bearer`（`backend/src/api.rs:241`）保护 API；`authorized`（`:275`）使用 `ct_eq` | 除 `/health` 外的 API 路由需要单一有效 Bearer token，比较路径避免普通字符串早停比较。 |
| CORS | `parse_allowed_origins`（`backend/src/config.rs:177`）解析明确 origin 且拒绝 `*`；`cors_layer`（`backend/src/api.rs:201`）限制 origin、方法和请求头 | CORS 是显式 origin 白名单，不是认证替代品。 |
| 密钥存储 | `ProfileService::with_system_store`（`backend/src/profiles.rs:295`）获取系统 store；`put_secret`（`:800`）写入；平台选择在 `system_credential_store`（`:2727`） | Profile secret 的值设计为 OS keychain/credential store 数据，而不是 YAML/SQLite 明文；当前 Windows 用户的真实写入、精确值核对、重建读取、删除和磁盘扫描已实测通过。 |
| 子进程环境 | `backend/src/processes/guardian.rs:429` 和 `backend/src/processes/direct.rs:132` 先 `env_clear()`；`backend/src/processes/guardian.rs:87`、`:113`、`:176` 调试输出脱敏 | 直接/guardian 进程不会无选择继承 backend 环境，调试结构避免打印环境值、脚本和 RPC token。 |
| 输出与 token 脱敏 | `SecretMasker`（`backend/src/processes/manager.rs:1815`）覆盖精确 secret、常见 token 和 Bearer 模式；`redact_json`（`backend/src/mcp.rs:2433`）递归脱敏 MCP JSON | 终端与 MCP 结果存在显式脱敏层；已覆盖主要边界，仍需扩展所有错误路径。 |
| MCP 子进程 | `backend/src/mcp.rs:1666` 清理环境，`:1669` 丢弃 stderr，`:2433` 处理结果脱敏 | MCP 凭据只投影到需要的子进程环境，结果返回前经过 secret 替换。 |
| 请求体与日志 | `backend/src/api.rs` 设置通用 body limit；`backend/src/main.rs` 配置 tracing filter；`backend/tests/runtime_log_redaction.rs` 启动真实后端 | `RUST_LOG=trace` 下已验证请求凭据与 Profile secret 不进入 stdout/stderr；Provider、工具、MCP 和 panic 错误矩阵仍需扩展。 |
| 桌宠第三方资产 | `docs/pet-asset-provenance.json` 完整列出四个 runtime library 和七个 Live2D model group；`scripts/verify-release-inputs.mjs --release-candidate` 校验来源、许可证、证据和 tree hash | 当前清单明确为 `unverified`。审计已匹配两项 MIT 库、Cubism Core 5-r.4、Cubism 2.1 runtime 与官方 sample-data 来源，但本地 LICENSE/NOTICE、专有许可/再分发凭证仍缺失；Natori 当前条款禁止商业使用、修改和再分发，是明确发布阻断。 |
| Windows 安装包边界 | `scripts/verify-nsis-artifact.ps1` 使用 7-Zip 静态解包，校验 Desktop 的 Tauri NSIS marker 补丁、sidecar 精确哈希、禁入路径、高置信凭据特征和 Authenticode 状态 | 当前源码的 NSIS 开发包审计通过；8 项载荷不含旧 Agent/Python/Node/user data，凭据特征 0 命中。包与两个 payload 均为 `NotSigned`，严格签名门按设计失败。 |

## 实际运行验证

`logs/phase4/runtime-short-2026-07-18.json` 是 2026-07-18 早期 schema v1 的短时结果，只保留为历史 HTTP 边界/性能证据。当前 schema v2 运行器随后完成了单独的加固回归：每一代都以 `SYNTHCHAT_BACKEND_ADDR=127.0.0.1:0` 让 OS 原子分配 loopback 端口，限长读取并严格校验 stdout readiness handshake，然后才执行 health/auth。测试 token 只写入 stdin，启动器显式从后端子进程环境删除继承的 `SYNTHCHAT_DESKTOP_TOKEN`。

- 未认证 `/health` 返回 `200`；未认证受保护 capabilities 返回 `401` 并含 Bearer challenge。
- 使用生成 token 的 capabilities 请求返回 `200`。
- `tauri://localhost` preflight 获得 CORS 放行；`https://untrusted.example.invalid` preflight 没有 `Access-Control-Allow-Origin`。
- 受控终止先关闭 stdin 并等待，超时才强杀；独立 HTTP client 对旧端点的有界连续探测均失败。
- fault restart 生成不同 token；新 token 的 capabilities 返回 `200`，上一代 token 返回 `401`。本次 OS 分配的端口也发生变化；报告只保留 `tokenRotated`、`portChanged` 和相应认证/可用性布尔值，不保留实际端口。
- 测试前放入父环境的错误 token 未被后端采用，证明 child-env 删除生效。
- 结果写盘或返回前先在内存中序列化，并扫描初始与重启代的全部生成 token 及 Bearer pattern；本次捕获结果未发现父环境值、测试 token 或 Bearer 值。

同日还在当前 Windows 用户会话中实际执行了 opt-in 原生 Credential Manager 回归：

- 测试使用临时 Hermes home 和随机唯一 Profile、Idempotency-Key、secret name/value，并仅通过 `ProfileService::with_system_store` 写入业务凭据。
- 写入后 public secret status 为 `configured=true`；销毁并重建 service 后仍为 true，证明新实例可从 Credential Manager 读取条目。
- 临时 Hermes home 在写入后、重建后和删除后逐文件扫描，均未发现明文 secret。
- 删除后 public status 与独立原生 entry probe 均为 false；RAII Drop 守卫在成功或 panic 解栈时尽最大努力再次删除 secret、Profile 和临时目录。
- 测试默认 `#[ignore]`，还要求 `SYNTHCHAT_RUN_NATIVE_KEYCHAIN_TESTS=1`；默认 Cargo 测试实测为 `0 passed; 1 ignored`，显式回归为 `1 passed; 0 failed`。测试没有输出 secret。

真实后端日志回归还在 `RUST_LOG=trace` 下覆盖 public health、合法和错误
Bearer、凭据样式请求头及 Profile secret PUT/DELETE。生成值只用于内存断言，
失败消息不回显值；Windows keychain 写入由 RAII 守卫兜底清理。

当前源码还生成了 Windows NSIS 开发包
`SynthChat_1.1.0_x64-setup.exe`（26,009,305 bytes，SHA-256
`DFA82F256A0251B025BB78F68EE72FF3C1E622233DA9992D41CAF24E6AC81216`）。
静态审计只发现六个 NSIS plugin、Desktop 和 Rust sidecar；禁入路径与
高置信凭据特征均为 0，sidecar 与构建输入 SHA-256 精确一致，Desktop 仅有
Tauri 官方 NSIS bundle-type 三字节补丁。该证据不运行安装包，也不替代
malware scan、clean-account 安装/卸载或签名验证。

## 依赖与凭据审计

- RustSec 官方快照 `b5fc89b8be99e96f79194d8a6f11e9b4143b99f0`
  含 1,166 条 advisory。Backend 的 307 个依赖和 Desktop 的 470 个依赖
  均为 0 个 vulnerability 级命中。
- Backend 已将 yanked `spin 0.9.8` 精确更新为非 yanked `0.9.9`；
  all-targets check 及重新审计均通过且不再产生 warning。
- Desktop 仍有 `RUSTSEC-2024-0429`（glib 0.18.5）1 个 unsound warning
  和 16 个 unmaintained warning。最新稳定 Tauri/wry/WebKitGTK 仍约束
  GTK/glib 0.18，这是 Linux 依赖图的上游阻塞项。
- Node 22.14.0/npm 10.9.2 下，root 与 `frontend/` 的 `npm audit` 均报告
  0 vulnerabilities。
- 本地全历史扫描覆盖 9 个 refs、86 个可达提交、1 个仅 reflog 可达提交、
  3,126 个历史 blob、当前 index 及工作树。legacy `synthchat-data` JSON
  中发现 10 个不同的高置信 OpenAI-compatible 凭据，3 个仍存在于忽略的
  工作树运行数据；当前 index 和 `.env*` 均为 0。
- 历史/当前 `app.db` 和旧 `synthchat.db` 已用只读 SQLite 专项流程扫描，
  未增加任何凭据命中，源数据库、sidecar 和 index 哈希保持不变。
- 另有 2,229 个不可达 blob（解压约 11.19 GB）不具备可靠路径映射。
  旧对象库不能作为清洁副本保留；所有 10 个凭据必须先撤销/轮换，随后
  在受限 mirror 中经审批移除 `synthchat-data` 历史并重新发布干净远端。

这些结果证明本地运行中的基本通信边界，不证明任何远程部署、浏览器扩展、桌面 WebView 或攻击者拥有同一用户权限时的整体安全性。

## 仍未关闭的风险

- 当前 Windows 用户的真实 Credential Manager 写入、service 重建读取和删除已验证；macOS/Linux、跨用户权限隔离、系统重启恢复和三平台打包产物中的行为仍未验证。
- 后端使用 loopback HTTP + bearer token，不是 TLS 远程服务模型。若未来开放非 loopback 监听、代理或远程访问，必须重新设计认证、TLS、origin、token 轮换和访问控制。
- 基本 `RUST_LOG=trace` 请求与 Profile secret 路径已验证；Provider 错误、
  工具失败、MCP 失败、panic/崩溃恢复仍需 secret-in-log 动态矩阵。
- 工作区/终端授权边界不是 OS sandbox；`docs/terminal-process-contract.md` 已声明批准的命令仍拥有宿主权限。需要独立的 sandbox/container 方案才能形成隔离保证。
- 本地历史和依赖扫描已完成，但 10 个凭据尚未轮换，历史尚未重写；
  远端隐藏 refs/forks/caches、签名/跨平台发布制品、公证、桌面 IPC 人工审计和
  第三方渗透测试仍未完成。
- 桌宠四项 vendored runtime 和七个 Live2D 模型仍全部为 `unverified`。
  PixiJS 6.5.10 与 `pixi-live2d-display@0.4.0-beta.2` 已匹配 MIT 上游，
  但本地缺 LICENSE/NOTICE；Cubism Core 已匹配 SDK 5-r.4，但缺适用的专有
  许可和再分发凭证；Cubism 2.1 runtime 缺权威历史许可证据。六个非 Natori
  模型已映射到官方固定提交，但 Mao 为本地派生修改、Wanko 为上游子集；Natori
  当前条款明确禁止商业使用、修改和再分发，是硬性发布阻断。它们不得随
  候选发布或签名流转，直到 [pet-asset-provenance.md](pet-asset-provenance.md)
  定义的严格门禁和发布主体的法律审查通过。
- Browser 隔离下载已覆盖 owner-bound approval、no-follow/reparse、filename/MIME/size/quota、metadata-only 投影、取消和终态清理；真实 Chromium UI E2E 还验证 CDP 与 download 使用两个不同的 once approval，文件体、data URL 和私有路径不进入 UI、REST、浏览器存储、请求体或 console。它是结构安全检查而非 malware/antivirus 引擎，且当前没有 Files/Workspace 导入路径。macOS/Linux keychain、真实三平台进程回归及完整发布产物审计仍未完成；已实现的 MCP、Browser CDP/download 和异步终端投递也不得因本报告而被视为安全可发布。

发布前至少应撤销/轮换 10 个历史凭据并完成获批的历史整改，补齐
macOS/Linux keychain 与 Windows 跨用户/重启回归、全量错误路径日志脱敏、
发布产物扫描、跨平台本地通信验证与人工安全评审。
