# SynthChat Hermes Backend

This directory is the standalone pure Rust Hermes-compatible Agent backend.
Node.js, Electron, Python Agent code, and upstream Hermes processes are not
runtime dependencies. The optional `execute_code` tool uses a detected host
Python >= 3.8 only to execute an approved user script; the backend remains
fully usable with that capability unavailable.
Implemented API domains:

- `GET /health` without authentication;
- capabilities and Provider catalog;
- Profile/config CRUD and write-only OS keychain secrets;
- SQLite Session CRUD, FTS5 search, messages and Hermes v21 import;
- text Run creation, persistent per-Session FIFO queue, active Run recovery,
  cancellation and replayable SSE;
- OpenAI-compatible streaming transport with reasoning and usage parsing;
- Profile-scoped Toolset management and complete Skills
  discovery/search/enable/install/uninstall lifecycle with durable Operations;
- Profile-scoped persistent MCP configuration CRUD with strong ETags,
  durable POST idempotency and keychain-only secret references;
- opaque Workspace registration and capability-relative `read_file`,
  `search_files`, approval-gated atomic `write_file`, and Hermes-compatible
  fuzzy/V4A `patch` execution;
- Run-owned `terminal` and `process` model tools, including foreground commands
  and owner-scoped background process lifecycle management. These tools do not
  add public REST process-management endpoints;
- approval-gated `execute_code` with optional host-Python discovery, direct
  guardian supervision, bounded output, and allowlisted nested Hermes RPC;
- durable `clarify` suspension and REST answers, including private answer
  continuation, cancellation, and fail-closed interruption recovery;
- builtin Memory CRUD/search with ETag and threat-scan boundaries;
- Tavily Web Search/Extract plus a readiness-gated Rust Browser
  automation/CDP/download runtime;
- persistent, single-delivery background terminal completion/watch events.

The current Session schema is v12. Schema v8 introduced the
owner/attempt-bound durable once/deny approval ledger; v9 added a bound
immutable clarification ledger. A
clarification request binds the Run, call, invocation checkpoint, and raw
argument SHA-256. Its pending action and public required event commit in the
same transaction. The exact user answer remains private to the ledger and
Provider continuation; public events, Messages, Problems, and logs omit it.
Identical answers replay idempotently, conflicting answers fail closed, and a
continuation claim is single-use. Cancellation and any non-cancellation
terminal interruption resolve the ledger and tool before the Run reaches its
terminal state. A Run also stores its provider/tool journal and at most 2,048
replayable events in the Rust-owned SQLite database.
Schema v10 distinguishes `provider` and private `codeRpc` tool invocations and
immutably binds every nested invocation to its parent `execute_code` call and
monotonic RPC sequence. Nested arguments and results remain in the private
journal and never become top-level Provider calls or public SSE projections.
Schema v11 adds the persistent Run queue and epoch-fenced runtime lease; v12
adds owner-bound durable async-delivery records for background terminal tools.

The server listens on `127.0.0.1:8642` by default and refuses non-loopback
addresses.

The desktop shell supplies the session token as one bounded line on stdin. For
standalone development, set `SYNTHCHAT_DESKTOP_TOKEN` instead:

```powershell
$env:SYNTHCHAT_DESKTOP_TOKEN = '<32-to-128-visible-ASCII-characters>'
cargo run
```

Optional environment variables:

- `SYNTHCHAT_BACKEND_ADDR`: loopback socket address, for example
  `127.0.0.1:9000` or `[::1]:8642`;
- `SYNTHCHAT_ALLOWED_ORIGINS`: comma-separated development origins appended to
  the built-in Tauri origins, for example `http://localhost:1420`;
- `SYNTHCHAT_TAVILY_BASE_URL`: trusted deployment-level Tavily base URL. It
  defaults to `https://api.tavily.com` and accepts a public HTTPS origin with an
  optional simple path prefix, but no userinfo, query string, or fragment;
- `SYNTHCHAT_SKILL_REGISTRY_INDEX_URL`: trusted deployment-level Skill registry
  index URL. It defaults to
  `https://hermes-agent.nousresearch.com/docs/api/skills-index.json`;
- `SYNTHCHAT_SKILL_GITHUB_API_BASE_URL`: GitHub-compatible API base used only by
  Skill registry installs. It defaults to `https://api.github.com/`;
- `SYNTHCHAT_SKILL_GITHUB_RAW_BASE_URL`: GitHub-compatible raw-content base used
  only by Skill registry installs. It defaults to
  `https://raw.githubusercontent.com/`.

`SYNTHCHAT_ALLOWED_ORIGINS` does not accept `*`, paths, query strings, or URL
fragments.

`SYNTHCHAT_TAVILY_BASE_URL` is read once at backend startup and cannot be
overridden by Profile YAML or the Profile API. Before sending the Profile's
`TAVILY_API_KEY`, the backend validates every DNS result as public, pins those
addresses into the HTTPS client, and keeps automatic redirects disabled.

The three Skill endpoint variables are also read once at backend startup and
cannot be overridden by Profile YAML, the Profile API, or frontend input. They
accept only public HTTPS URLs without userinfo, query strings, fragments,
encoded path delimiters, or dot-segment traversal. GitHub API and raw paths are
appended as URL path segments under the configured base. Every registry,
GitHub API, and raw-content request validates the complete DNS result set as
public, pins those addresses into a no-proxy HTTPS client, and disables
automatic redirects. Official `NousResearch/hermes-agent` Skills continue to
use the source commit pinned by the backend rather than a registry-provided
branch head.

## Inference scope

The first transport slice supports explicit OpenAI-compatible Provider IDs,
including `openai-api`, `openrouter`, `custom`, `lmstudio`, `deepseek`, `xai`,
Qwen-compatible endpoints and the other compatible catalog entries. Native
Anthropic, Gemini, Copilot, MiniMax-Anthropic and Azure Foundry transports are
not advertised as available yet.

The selected Profile must contain a non-empty model and a supported Provider.
Providers requiring credentials read the first configured catalog alias from
the OS keychain. Credentials are held only for that Run and are never written
to config, SQLite, events, errors or logs.

Additional Runs for one Session enter the schema v11 persistent FIFO together
with their user Message, opaque queue item and idempotency record. The runtime
reports `runQueue=true` and `activeRunDiscovery=true`. Profile-scoped Toolset catalog listing and
enablement management report `extensions.toolsetManagement=true`; GET and PATCH
share the ProfileConfig ETag, and PATCH accepts only `{ "enabled": boolean }`.
`toolExecution`, `toolProgress`, `workspaceManagement`, `skillDiscovery`,
`skillEnablement`, `approvals`, and `clarifications` are available. Workspace
mutations require an outstanding server-issued approval with the exact `once`
or `deny` choices.
`patch` implements the pinned nine-strategy replace chain and bounded V4A
Update/Add/Delete/Move preflight; raw arguments, file content, terminal output,
and bounded provider diffs stay internal while public events expose summaries
only.

`terminal` and `process` are registered under the Profile's `terminal` Toolset.
`terminal` additionally requires the Run to bind a currently available
Workspace, and every foreground or background command requires a durable
`once/deny` approval. `process list/poll/log/wait` are read-only;
`kill/write/submit/close` each require a fresh durable approval. Generic public
tool summaries omit command/stdin bodies; a separate redacted, escaped,
single-line approval summary with an argument digest is exposed only through
the pending approval. Schema v8's approval ledger snapshots the full argument
hash and owner; execution claim rebinds those values to the exact Run, call,
tool, and invocation checkpoint. Schema v9 additionally stores the
bound clarification request, immutable private resolution, and single-use
continuation claim; schema v10 adds the private code-RPC origin and parent
binding described above.

The async Run executor persists lifecycle metadata in `terminal_processes`,
scoped by `(profile_id, session_id)`. A `BEGIN IMMEDIATE` reservation makes the
global 64-active-process limit atomic. The backend spawns a guardian first; the
actual shell starts only after PID/strong identity persistence and validation
of the complete bounded launch frame, and the script never appears in argv.
Background launch remains leased until the tool result is durable and
RunService commits it. Cancellation/deadline before commit, supervisor loss,
or guardian parent-pipe disconnect terminates the managed process tree.

stdin control runs in a separate bounded writer with fixed write deadlines, so
backpressure cannot block lifecycle supervision. Root exit first converges the
Job Object/process group, then output-pipe drain is bounded. Sanitized output
remains memory-only and is unavailable after a backend restart; a 4 KiB guard
around retention boundaries prevents supported Profile secrets from being cut
before redaction. The shell environment is rebuilt from a minimal allowlist.

A Workspace supplies only the initial cwd; it is not an OS or container
sandbox, so an approved command has full host authority. The current runtime
rejects `pty=true`; Windows ConPTY is not implemented. A background command may
select exactly one of `notify_on_complete=true` or 1..16 non-empty
`watch_patterns`. Its owner-bound record survives restart and appends at most
one public, redacted `tool.delivery` event. Windows creation FILETIME, Linux
boot/start ticks, and macOS `proc_pidinfo` start time provide strong identity;
recovery, shutdown, and detached kill fail closed without a match. A detached
`killed` transition requires platform termination success and tracked root
identity exit.

`execute_code` is registered under the Profile's `code_execution` Toolset and
is advertised only when a real Python >= 3.8 interpreter is detected; Windows
Store `WindowsApps` aliases are rejected. The complete script is one durable
`once/deny` approval unit and no child starts before approval. It runs through
the direct guardian protocol with process-tree cancellation and a scrubbed
environment that excludes Profile secrets. Stdout keeps a bounded 50 KB
head/tail projection and stderr a separate 10 KB head-only projection; both are
sanitized. Allowlisted nested tool calls are durable private journal entries,
while public Run/SSE/Message projections omit source, output, file content, and
nested arguments.

Guardian and launch leasing do not make host side effects transactional or
exactly-once, and native three-platform crash/long-running tree tests remain a
release requirement. Builtin Memory CRUD, strict prompt scanning, frozen Run
snapshots, and approval-gated model writes are implemented. Tavily-backed
`web_search`/`web_extract` are available when the Profile Web toolset and the
corresponding keychain-backed provider readiness are enabled. Browser
automation/CDP is available only when a local Chromium-family binary is
discovered and the Profile Browser Toolset is enabled; it uses a contained
Rust-owned process, loopback CDP and egress proxy. Approved downloads accept one
bounded private file, validate its name/MIME/magic/size/SHA-256, return metadata
only, and delete the content without importing it into Files or Workspace.
Persistent Run queue, active Run discovery, and async tool delivery are
implemented. `clarifications=true`; `asyncToolDelivery=true` is authoritative
when the Run runtime is available. See
`../docs/terminal-process-contract.md` for the complete implemented contract
and residual limits.

## Verification

The 2026-07-20 complete backend run passed 493/493 tests: 364 library tests,
2 backend-binary tests and every integration binary. Formatting, all-targets
check and `clippy -D warnings` pass. Playwright passes 12/12, including real
Chromium navigation/snapshot and approved CDP; Browser download UI coverage,
30-60 minute mixed load, eight-hour soak/leak evidence, native three-platform
crash/process coverage and security release sign-off remain gates.

```powershell
cargo fmt -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
```
