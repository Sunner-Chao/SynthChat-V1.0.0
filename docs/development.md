# Development Guide

## Prerequisites

- Node.js `>=22.14.0 <23` and npm `>=10.9.2 <11` for development; CI and
  release-evidence verification intentionally require exact Node `22.14.0` and
  npm `10.9.2`
- Rust 1.88 or newer with `rustfmt` and `clippy`
- Platform dependencies required by Tauri 2
- An available OS keychain for credential-backed Provider runs
- Optional Python 3.8 or newer for the `execute_code` user-code tool

Install root Playwright/E2E tooling with `npm ci` and the frontend with
`npm ci --prefix frontend`. Rust dependencies are resolved from
`backend/Cargo.lock` and `desktop/Cargo.lock`.

## Run the application

For the complete authenticated flow on Windows:

```powershell
npm run desktop
```

The desktop shell starts the backend on a loopback port, passes a random
256-bit token through stdin and exposes only the narrow connection command to
the frontend. Closing the desktop shell terminates the managed backend.

For backend-only diagnostics:

```powershell
$tokenBytes = New-Object byte[] 48
$rng = [System.Security.Cryptography.RandomNumberGenerator]::Create()
try { $rng.GetBytes($tokenBytes) } finally { $rng.Dispose() }
$env:SYNTHCHAT_DESKTOP_TOKEN = [Convert]::ToBase64String($tokenBytes)
$env:SYNTHCHAT_ALLOWED_ORIGINS = "http://127.0.0.1:1421"
cargo run --manifest-path backend/Cargo.toml
```

Do not store a development token in source, Vite variables or config files.

### Desktop backend supervision

The desktop process loads its backend supervision settings once at startup and
reuses them for every backend generation. A dedicated supervisor worker owns all
launch, probe, backoff and stop operations; Tauri setup returns immediately, and
the connection/status commands only read an in-memory lifecycle snapshot. The
snapshot reports `starting`, `ready`, `stopping`, `backoff`, `failed` or
`shuttingDown`. Invalid values are reported through the status command and
prevent the sidecar from starting. Duration values are integer milliseconds and
all ranges are inclusive:

| Environment variable | Default | Valid range |
| --- | ---: | ---: |
| `SYNTHCHAT_DESKTOP_STARTUP_TIMEOUT_MS` | 8000 | 100..120000 |
| `SYNTHCHAT_DESKTOP_PROBE_TIMEOUT_MS` | 250 | 10..10000 |
| `SYNTHCHAT_DESKTOP_MONITOR_INTERVAL_MS` | 500 | 10..60000 |
| `SYNTHCHAT_DESKTOP_PROCESS_POLL_INTERVAL_MS` | 20 | 1..1000 |
| `SYNTHCHAT_DESKTOP_SHUTDOWN_GRACE_TIMEOUT_MS` | 2000 | 10..60000 |
| `SYNTHCHAT_DESKTOP_TERMINATION_TIMEOUT_MS` | 1000 | 10..30000 |
| `SYNTHCHAT_DESKTOP_RESTART_BACKOFF_INITIAL_MS` | 250 | 10..60000 |
| `SYNTHCHAT_DESKTOP_RESTART_BACKOFF_MAX_MS` | 8000 | 10..300000 |
| `SYNTHCHAT_DESKTOP_STABLE_WINDOW_MS` | 30000 | 100..3600000 |
| `SYNTHCHAT_DESKTOP_DIAGNOSTIC_TIMEOUT_MS` | 250 | 10..5000 |
| `SYNTHCHAT_DESKTOP_STDERR_MAX_BYTES` | 4096 | 256..65536 |

The initial restart backoff must not exceed the maximum. A generation must stay
ready for the configured stable window before its failure count is reset, so a
fast ready/crash loop continues to increase the bounded exponential backoff.
The worker publishes `stopping` before termination and never schedules or starts
the next generation until the old process is confirmed stopped. Failure to
confirm termination disables automatic restart, preventing two generations from
using the same Hermes home. The default backend address remains `127.0.0.1:0`,
so each generation receives an OS-assigned loopback port and a newly generated
256-bit session token.

### Frontend runtime configuration

The Desktop webview loads frontend operational settings once, before the first
React render, through the no-argument `get_frontend_runtime_config` Tauri
command. The returned object contains only bounded polling/reconnect values and
Pet resource path candidates; the frontend then enforces their same-origin and
file-suffix rules. It never contains the backend address, desktop session token,
Provider credentials, API keys or arbitrary command data. The snapshot is
contract-checked and frozen by the frontend before installation.

Set these variables before starting the Desktop process. Integer ranges are
inclusive:

| Environment variable | Default | Valid range or constraint |
| --- | ---: | --- |
| `SYNTHCHAT_FRONTEND_BACKEND_HEALTH_TIMEOUT_MS` | 4000 | 100..120000 ms |
| `SYNTHCHAT_FRONTEND_BACKEND_STATUS_POLL_INTERVAL_MS` | 15000 | 1000..3600000 ms |
| `SYNTHCHAT_FRONTEND_CHAT_RECONNECT_INITIAL_DELAY_MS` | 250 | 10..60000 ms |
| `SYNTHCHAT_FRONTEND_CHAT_RECONNECT_MAX_ATTEMPTS` | 30 | 0..10000 |
| `SYNTHCHAT_FRONTEND_CHAT_RECONNECT_MAX_DELAY_MS` | 8000 | 10..300000 ms |
| `SYNTHCHAT_FRONTEND_CHAT_RUN_STATUS_POLL_INTERVAL_MS` | 2000 | 500..60000 ms |
| `SYNTHCHAT_FRONTEND_SKILL_OPERATION_MAX_POLLS` | 30 | 1..1000 |
| `SYNTHCHAT_FRONTEND_SKILL_OPERATION_INITIAL_BACKOFF_MS` | 250 | 1..60000 ms |
| `SYNTHCHAT_FRONTEND_SKILL_OPERATION_MAX_BACKOFF_MS` | 2000 | 1..300000 ms |
| `SYNTHCHAT_FRONTEND_PET_FRAME_URL` | `pet/index.html` | Non-empty, at most 2048 bytes; frontend requires same-origin `.html` |
| `SYNTHCHAT_FRONTEND_PET_MODEL_URL` | `pet/model/Hiyori/Hiyori.model3.json` | Non-empty, at most 2048 bytes; frontend requires same-origin `.model3.json` |
| `SYNTHCHAT_FRONTEND_PET_STATUS_POLL_INTERVAL_MS` | 5000 | 1000..3600000 ms |

The Chat reconnect initial delay must not exceed its maximum delay. The Skill
Operation initial backoff must not exceed its maximum backoff. Invalid values
produce a value-redacted startup configuration error in the webview; backend
supervision and its separately validated `SYNTHCHAT_DESKTOP_*` settings remain
independent. Non-Tauri browser builds do not call the bridge and retain the
preinstalled `globalThis.__SYNTHCHAT_RUNTIME_CONFIG__`, `VITE_SYNTHCHAT_*`, then
generic-default precedence used by frontend development and tests.

Startup stderr is drained in the background, retained only up to the configured
byte limit and redacted before it reaches the status snapshot. Configuration
variable names such as `SYNTHCHAT_SKILL_REGISTRY_INDEX_URL` remain visible for
diagnosis, while session tokens, authorization values and secret-like values are
removed. Desktop shutdown notifies and joins the supervisor worker; no restart
is allowed after shutdown begins.

The Web runtime uses `https://api.tavily.com` by default. A trusted deployment
can set `SYNTHCHAT_TAVILY_BASE_URL` before backend startup to use a public HTTPS
gateway, including a simple path prefix. Userinfo, query strings, fragments,
private/special IP literals and unsafe DNS results are rejected. This setting
is process-level; Profiles can select Tavily but cannot override its endpoint.

Skill registry installs use these process-level endpoint settings:

- `SYNTHCHAT_SKILL_REGISTRY_INDEX_URL`, defaulting to
  `https://hermes-agent.nousresearch.com/docs/api/skills-index.json`;
- `SYNTHCHAT_SKILL_GITHUB_API_BASE_URL`, defaulting to
  `https://api.github.com/`;
- `SYNTHCHAT_SKILL_GITHUB_RAW_BASE_URL`, defaulting to
  `https://raw.githubusercontent.com/`.

Set them before backend startup only when a trusted deployment uses a public
HTTPS mirror or gateway. Endpoint values cannot contain userinfo, queries,
fragments, encoded delimiters, private/special IP literals, or unsafe path
segments. The backend joins repository paths beneath the configured bases,
rejects the request if any DNS answer is non-public, pins validated addresses,
uses no proxy, and follows no automatic redirects. Profile YAML, Profile API
requests, and frontend input cannot override these values. The official Hermes
Agent repository remains pinned to the backend's locked source commit even when
the transport endpoints are changed.

`execute_code` is enabled only when the Run/session runtime is available and the
backend probes a usable Python 3.8+ executable. Discovery checks the absolute
`SYNTHCHAT_CODE_EXECUTION_PYTHON` override first, then active virtual/Conda
environments and `PATH`; WindowsApps aliases and failed, old or timed-out probes
are rejected. Python is optional and is not the Agent runtime: without it the
Hermes inference/tool loop remains Rust, while `extensions.codeExecution` and
the `code_execution` Toolset's `configured` field are false until backend
restart and a successful probe.

## Product-facing Rust extensions

Persona, Worldbook, and Moments data is stored by the Rust product catalog at
`HERMES_HOME/.synthchat/product-catalog-v1.db`; the frontend uses only the
authenticated REST client and strong product ETags. A Session or Run may select
a same-Profile Persona. The first model turn freezes that Persona and every
enabled Worldbook section bound to it, so editing catalog data never mutates an
already-started Run snapshot.

The WeChat settings panel uses the Rust iLink adapter for non-sensitive Profile
configuration, QR login, unique Persona binding, and explicit bounded poll/send
operations. Bot credentials are written only to the OS keychain. There is no
background polling, automatic Session/Run creation, or automatic reply loop.

The Plugins page manages bounded `plugin.json` manifests under
`HERMES_HOME/.synthchat/plugins`. Enablement is catalog metadata only: the
backend does not load an entry point, expose declared environment values, inject
plugin tools into Runs, or restore a Python/Node/legacy Agent plugin runtime.

## Configure a text Run

Use the Profile UI to select a supported OpenAI-compatible Provider and a
non-empty model. Store the Provider secret through its write-only secret field;
the value is written to the OS keychain and is never returned. `lmstudio` may
run without a secret and is useful for local compatibility testing.

Only one Run per Session executes at a time. Later sends are accepted into the
persistent FIFO with their user Message and idempotency record, then start when
the preceding Run becomes terminal. Profile-scoped Toolset listing and
enablement are available through `extensions.toolsetManagement`. The persisted
tool loop currently exposes `session_search`, `skills_list`, `skill_view`,
`read_file`, `search_files`, approval-gated `write_file`/`patch`, `terminal`,
`process`, `clarify`, approval-gated `memory`, `web_search`, `web_extract` and
conditional `execute_code` when their Toolsets and prerequisites are enabled.
Tool
execution carries a shared cancellation flag and absolute deadline; Workspace
operations poll both while traversing or writing. Workspace access is rooted
in a `cap-std` directory capability, with no-follow directory traversal and
final file opens.
The file tools are injected only when the Run is also bound to a registered,
currently available Workspace for that Profile. Workspace-relative paths are
checked fail-closed for traversal, root escape, symlink/reparse escape,
sensitive files, binary or oversized content, and bounded output. `write_file`
and `patch` require durable once/deny approval and bind execution to the
approval-time SHA-256 precondition. Both validate JSON/YAML/TOML before touching
disk. `patch` provides fuzzy replace plus bounded V4A Update/Add/Delete/Move;
V4A preflights the entire batch, then commits per file and explicitly reports
any partial apply failure. Terminal/process execution is available within the
documented host-authority boundary; PTY and long-lived approval scopes remain
unavailable. Async completion/watch delivery is available only when the Run
runtime reports `asyncToolDelivery=true`. `clarify` uses the v9 bound
immutable ledger retained by the current schema v13: the pending request is
durable, answers are preserved exactly but stay private to Provider
continuation, and cancel or any terminal
failure resolves the clarification before ending the tool/Run. Installed Skills
can be listed, searched, enabled/disabled, installed and uninstalled through the
Skills API. Install/uninstall use durable Operations, owner leases, crash
recovery and capability-relative storage. Tavily `web_search`/`web_extract` are
available through independently evaluated Profile readiness and the `web`
Toolset. Browser automation/CDP becomes available only after local
Chromium-family discovery and Profile Browser Toolset enablement. Browser
downloads are available only under the same runtime readiness and an
owner-bound approval. Later Runs for a busy Session enter a persistent FIFO
queue, and active Run discovery plus SSE journal replay restores the in-progress
UI after reload or backend restart.

Enable the `code_execution` Toolset and configure `codeExecution.mode`,
`timeoutSeconds` and `maxToolCalls` in the Profile UI before using
`execute_code`. `project` uses the Run Workspace as cwd when one is bound;
`strict` uses a private staging directory. Both modes execute approved user code
with host filesystem, network, subprocess and native-library authority, so
neither is a sandbox. The whole 1..60 KiB script receives one durable
`once/deny` decision; deny does not start a guardian or create nested journal
rows. Once approved, the same claim covers nested mutating RPC calls from the
dynamically advertised subset of `web_search`, `web_extract`, `read_file`,
`write_file`, `search_files`, `patch` and foreground-only `terminal`; nested
calls do not open a second public approval action.

Rust writes the script and generated `hermes_tools.py` to private staging, then
launches Python through the direct guardian with a typed, per-execution loopback
RPC bootstrap. The child environment is rebuilt from a minimal allowlist and
does not inherit Profile/API secrets. stdout is retained as a 50 KB head-tail
window and stderr as an independent 10 KB head-only window; both are sanitized
and secret/token/Bearer-redacted before entering the private provider result.
Schema v10 records nested invocations as immutable `origin=codeRpc` rows bound
by `parent_call_id` and monotonic `rpc_sequence`; these rows and their nested
arguments/results are excluded from top-level Provider tool calls. Full code,
output and nested private data remain limited to internal journal/Provider
continuation and are excluded from public Run/SSE/Message projections; pending
approval shows only a bounded, redacted code preview plus digest. Cancel,
deadline, backend disconnect and supervisor drop terminate the managed Job
Object/process group. Once a Run is
`cancelling`, only a failed invocation terminal is accepted, so late success
cannot overwrite cancellation.

Tavily performs the final remote fetch for extract targets. The backend rejects
credentials, sensitive query keys, private/special address families and every
unsafe local DNS result before dispatch, but cannot pin Tavily's remote DNS
resolution. The separately configured Tavily API endpoint is resolved locally
and pinned to the complete validated address set before the key is sent. Treat
only the extract target's remote rebinding TOCTOU as a provider-boundary risk.

Builtin Memory is implemented and reports `features.memoryWrite=true`. The
management API is intentionally
builtin-only: it projects `memories/MEMORY.md` and `memories/USER.md`; a Profile
configured with any other memory provider receives
`memory_provider_unsupported` from every Memory route. Do not add a fallback
empty projection. Memory mutations require target ETag preconditions, and
creates also require an idempotency key. Model-driven writes additionally
require durable once approval. A Run freezes its threat-scanned Memory/User
prompt snapshot at model preparation, so writes during that Run become visible
only to a later Run. Raw entry content may be returned by the authenticated
Memory management API, but must not enter public Run events, Messages, Problems,
approval summaries or logs.

Static symlinks and Windows reparse points are rejected, but a hostile
same-user process racing a verified parent-directory replacement still needs a
handle-relative storage refactor and native Windows regression coverage. Treat
that as a phase-four security gate rather than a completed containment claim.

## Verification

```powershell
npm ci
npm ci --prefix frontend
npm --prefix frontend run api:check
npm --prefix frontend run api:lint
npm --prefix frontend run build
npm --prefix frontend test
node scripts/verify-mixed-runtime.mjs --self-test
node scripts/verify-mixed-runtime-evidence.mjs --self-test
npm run test:e2e
node scripts/verify-frontend-tauri-bridges.mjs --self-test
cargo fmt --manifest-path backend/Cargo.toml -- --check
cargo clippy --locked --manifest-path backend/Cargo.toml --all-targets -- -D warnings
cargo test --locked --manifest-path backend/Cargo.toml --all-targets
cargo fmt --manifest-path desktop/Cargo.toml -- --check
cargo clippy --locked --manifest-path desktop/Cargo.toml --all-targets -- -D warnings
cargo test --locked --manifest-path desktop/Cargo.toml --all-targets
```

### Standalone backend runtime verifier

The phase-four runtime verifier exercises health, authentication, CORS,
concurrent read traffic, process metrics and an optional stop/restart probe:

```powershell
pwsh -NoProfile -File .\scripts\verify-backend-runtime.ps1 `
  -RustToolchain 1.88.0-x86_64-pc-windows-msvc `
  -DurationSeconds 10 -Concurrency 4 -SampleIntervalSeconds 1 `
  -IncludeFaultChecks `
  -ResultPath .\logs\phase4\runtime-short.json
```

Omit `-RustToolchain` to use the repository `rust-toolchain.toml`. The explicit
override is useful on Windows hosts whose rustup default host is not MSVC.
`-CargoExecutable` has highest precedence for Cargo discovery, followed by
`SYNTHCHAT_VERIFY_CARGO`, `CARGO` and `PATH`. `-RustToolchain` similarly takes
precedence over `SYNTHCHAT_VERIFY_RUST_TOOLCHAIN`. Cargo metadata supplies the
effective `target_directory`, so `CARGO_TARGET_DIR` and the current platform's
executable suffix are respected. Use `-BackendBinary` only for an explicit
prebuilt artifact; use `-SkipBuild` only after building the same source and
toolchain.

Every timeout, polling interval and fault-probe attempt count is a bounded
parameter; PowerShell rejects values outside these inclusive ranges:

| Parameter | Default | Valid range |
| --- | ---: | ---: |
| `DurationSeconds` | 15 | 1..86400 |
| `Concurrency` | 4 | 1..64 |
| `SampleIntervalSeconds` | 1 | 1..60 |
| `StartupTimeoutSeconds` | 30 | 1..120 |
| `RequestTimeoutSeconds` | 10 | 1..120 |
| `CargoTimeoutSeconds` | 300 | 1..3600 |
| `StartupPollMilliseconds` | 50 | 10..2000 |
| `WorkerPollMilliseconds` | 100 | 10..2000 |
| `StartupHandshakeMaxBytes` | 128 | 64..4096 |
| `ShutdownGraceSeconds` | 5 | 1..120 |
| `KillTimeoutSeconds` | 5 | 1..120 |
| `FaultProbeTimeoutSeconds` | 2 | 1..30 |
| `FaultProbeAttempts` | 3 | 1..20 |
| `FaultProbeDelayMilliseconds` | 100 | 10..5000 |
| `LatencySampleLimit` | 2048 | 128..16384 |

Each backend generation binds
`127.0.0.1:0`; the verifier accepts only a bounded, exact
`SYNTHCHAT_BACKEND_READY 127.0.0.1:<port>` stdout line before issuing HTTP.
It removes inherited `SYNTHCHAT_DESKTOP_TOKEN`, writes a fresh token through
stdin, keeps that pipe open for lifetime supervision, then closes stdin and
waits before any forced termination. A fault restart receives a different
token. Reports contain only boolean rotation/port assertions and are scanned
against every generation token and Bearer pattern before write or return.

The script is parser- and runtime-tested on the current PowerShell 7 and
Windows PowerShell 5.1 host. That evidence does not replace macOS/Linux native
runtime checks or the eight-hour soak gate.

The real Windows Credential Manager regression is deliberately excluded from
the default suite. It mutates the current user's native credential store for
the duration of the test, so run it only from an explicitly authorized Windows
session:

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

`backend/tests/windows_system_keychain.rs` has both `#[ignore]` and the
environment opt-in. It passes a temporary Hermes home directly to
`ProfileService::with_system_store`, generates unique legal identifiers and a
unique secret, reconstructs the service to prove native persistence, scans the
entire temporary tree for plaintext, and verifies deletion. A Drop guard makes
best-effort idempotent cleanup through both `ProfileService` and the native
store even if an assertion unwinds. The test must never print the secret or use
the real `~/.hermes` directory.

The Run HTTP integration tests use a random loopback mock Provider and a
temporary Hermes home. Tests must never read or modify the real `~/.hermes`.

The Desktop shell does not assume the standalone development port. It starts
the backend with `SYNTHCHAT_BACKEND_ADDR=127.0.0.1:0`, validates the bounded
child stdout handshake, then exposes the actual address only through the
reviewed Tauri connection bridge. Set `SYNTHCHAT_BACKEND_ADDR` explicitly only
when a standalone diagnostic run needs a stable loopback port.

The latest bounded, non-stress worktree baseline on 2026-07-21 is backend
`517/517` (377 library, 2 backend-binary and the remaining integration tests;
zero failures and one explicitly unauthorized native Windows keychain test
ignored), frontend `551/551` across 37 test files, and desktop `21/21`.
Backend/desktop formatting, all-targets checks and `clippy -D warnings`, OpenAPI
drift/lint, TypeScript, the Vite production build, release-input self-check and
`git diff --check` also pass. Playwright, npm audit, mixed-runtime pilots and
pressure/long-stability runs were not rerun in this baseline; the dated
2026-07-20 results below remain historical evidence. The unified Run task registry now
closes admission before a shared shutdown deadline, drains or preserves tracked
workers, gates terminal launches and releases the fenced runtime lease without
waiting for its TTL. The earlier text-only 60-minute mixed extension passes;
schema v2 eight-hour soak/leak testing and the native Windows/macOS/Linux package/process
matrix remain phase-four gates.

The historical 2026-07-20 12/12 Playwright run also covers one real Workspace sequence across
`write_file`, `read_file`, `search_files` and `patch`. Write and patch require
separate once approvals; private contents and absolute paths remain absent from
the terminal Run, SSE, Message and UI surfaces.

## Data and security

The backend owns `.hermes/.synthchat/sessions-v1.db` at Session schema v13. v8
introduced the owner-bound approval ledger; v9 added the bound immutable
clarification ledger and its single-use continuation claim; v10 adds immutable
`provider|codeRpc` invocation origin plus `parent_call_id`/`rpc_sequence`
bindings for private nested code-tool calls; v11 adds the persistent Run queue
and epoch-fenced runtime lease; v12 adds durable single-delivery records for
background terminal completion/watch notifications; v13 adds constrained
`persona_id` columns to `sessions` and `session_versions` so Session/Run Persona
ownership survives reload and restart. Upstream Hermes
`state.db` is opened only through the locked v21 read-only importer. Never add
runtime databases, `synthchat-data/`, `.env`, logs, coverage output or secrets
to Git. Historical credentials from the original repository still require
rotation and an approved history rewrite before release.
