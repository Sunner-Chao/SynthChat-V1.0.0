# Hermes Rust Migration Status

Status date: 2026-07-20

The evidence-based, workload-weighted overall estimate is **91%**. Percentages
measure the original five-phase acceptance scope, not code volume. Formal team
review and release sign-off are separate gates, and release readiness remains
below 45%.

| Phase | Progress | Evidence | Remaining gate |
| --- | ---: | --- | --- |
| 1. Research and contract | 92% | Pinned upstream commits, architecture report, OpenAPI/API contract, impact inventory | Recorded team approval of ADR and contract |
| 2. Cleanup and skeleton | 90% | Separate `frontend/`, `backend/`, `desktop/`; pure Rust runtime; health/auth/API client; archive branch | Freeze the large migration worktree into reviewable commits |
| 3. Core replacement | 99% | Profile/keychain, Session/FTS5/import, Run/SSE, OpenAI-compatible HTTP/SSE Provider, approvals/clarifications, persistent FIFO Run queue and active Run recovery, Files write/read/search/patch with two-once-approval UI E2E, terminal/Web/`execute_code` tools, Browser Rust lifecycle/CDP/download slice with real Chromium navigate/snapshot/two-once-approval download UI E2E, Skills discovery/toggle/install/uninstall with durable Operations, builtin Memory, MCP config CRUD plus stdio/Streamable HTTP/legacy SSE discovery, calls and Run injection, async terminal delivery with foreground/background UI E2E | Live external-credential Provider smoke; macOS/Linux keychain evidence |
| 4. Integration and performance | 92% | Full local Rust/frontend/desktop matrix, unified Run task registry/shared shutdown deadline, runtime log-redaction test, Desktop crash recovery, Playwright 12/12 (43.0 seconds), Terminal 2/2 plus the background two-Run flow repeated 3/3 with zero residue, 4/4 real-backend mixed smoke, 30- and 60-minute real mixed pilots with 3,332/3,332 total successful iterations and clean teardown, real Files and Chromium workflows, Windows keychain, dependency audit and credential-history scan | Complete the 8-hour soak/leak review (the 60-minute full-window RSS slope was +11.00 MiB/h), three-platform package/crash/process tests, credential rotation and history remediation |
| 5. Delivery | 52% | Locked Windows/macOS/Linux/sidecar Cargo build paths, root/frontend npm locks, release-input verifier, build scripts and development/architecture/release documentation; current-tree Windows NSIS development artifact with integrity, payload, forbidden-path and credential-signature audit | Track all required release inputs, freeze reviewable commits, clean-account install/upgrade/uninstall, verify Pet provenance, signing/notarization, native macOS/Linux delivery and release CI |

## Latest completed slice

Run shutdown now uses one tracked-task registry for admission, worker ownership,
first-writer shutdown mode and bounded waiting. The gate closes before the
shutdown snapshot; already admitted create/queue work must either register or
leave admission, and no later operation can enter. Drain requests durable Run
cancellation, while PreserveRuns stops local workers without terminalizing their
rows; both share one total deadline, stop foreground/background process launch,
and release the fenced runtime lease explicitly. Targeted never-responding
Provider and successor-runtime tests are included in the complete backend
matrix.

The real model transport is now a Rust-owned OpenAI-compatible streaming slice.
`OpenAiCompatibleProvider` sends authenticated `/chat/completions` requests,
decodes bounded SSE incrementally, projects text, reasoning and usage events,
assembles validated tool calls, and distinguishes cancellation, timeout,
transport and malformed-response failures. Run preparation resolves the
configured Provider, model, base URL and OS-keychain secret without persisting
or logging credentials. The focused Provider matrix passes 14/14 tests, and
the HTTP-backed Web/Run integration matrix passes 5/5 against deterministic
local Provider and Tavily fixtures. A live third-party call remains an
environment acceptance check because the repository intentionally carries no
real API credential.

Skills lifecycle, persistent queueing and active Run discovery are now complete
Rust-owned slices. Install and uninstall execute through durable Operations with
owner leases, crash recovery and capability-relative storage. A Session accepts
at most one active Run while later Runs enter a persistent FIFO queue; the
runtime lease is epoch-fenced, queued work resumes after restart, and the UI can
discover the active Run before replaying its persistent SSE journal.

Background terminal delivery is now implemented as a durable Rust vertical
slice. A `background=true` terminal call may select exactly one of
`notify_on_complete=true` or bounded `watch_patterns`; the request is recorded
atomically with the owned terminal process. Run completion remains replayable
while a delivery is pending. A scheduler recovery scan polls only the existing
owner-scoped process record and atomically appends at most one public
`tool.delivery` event. The public event contains only stable IDs, delivery kind,
terminal status, optional exit code and watch match count; command, pattern and
output remain private. Explicit `process` polling and cancellation retain their
existing semantics.

The Desktop E2E now exercises that slice across two Runs in one Session. Run A
starts an approved background terminal command and completes while its durable
delivery remains pending; Run B lists the process without approval and kills it
after a separate once approval. The UI continues to receive Run A's unique
`tool.delivery` after Run B becomes latest, and replay stays idempotent. The
targeted flow passed three consecutive times (8.8, 7.0 and 7.0 seconds) with no
backend, fixture-process or `synthchat-hermes-e2e-*` residue. Shutdown now keeps
redacted output until delivery settlement, closes pending SSE only after the
terminal notification is durable, stops recovery candidates concurrently and
uses exact Job/process identity for cleanup.

Browser now has a pure Rust vertical slice. `BrowserManager` discovers a local
Chromium-family executable, starts one contained headless browser per
Profile/Session/Run with a temporary user-data directory and random loopback
CDP port, and routes browser traffic through a Rust HTTP/CONNECT egress proxy.
Navigation and proxy connections resolve then reject private, loopback,
link-local and special targets. Downloads stay denied until a current-snapshot,
owner-bound once approval temporarily accepts one bounded file into private
per-Run storage. The Run registry injects thirteen Browser definitions only when both the runtime and
Profile Toolset are ready. Interactive, dialog and bounded `Runtime.evaluate`
actions require the existing owner-bound durable once approval plus a current
accessibility snapshot ID. AX snapshots, console output and JPEG screenshots
are bounded. Download filename/MIME/magic/size/SHA-256 checks return metadata
only, delete content after scanning, and never expose a path or Files/Workspace
import. Deterministic fake CDP, real headless Chrome and Run fixtures cover
protocol dispatch, navigation, snapshot, approval, download and cleanup.

MCP now has pure Rust runtime coverage for stdio, Streamable HTTP and legacy
SSE. Remote requests pin each validated DNS resolution, follow bounded manual
redirects, scope bearer/session credentials to their origin, negotiate
Streamable HTTP protocol/session headers, and retain bounded JSON-RPC/SSE
messages only. Local transport fixtures and Run E2E cover discovery, dynamic
tool injection, approval, private continuation and public projection redaction.

The `execute_code` slice now provides Profile configuration, readiness-based
capabilities, a durable whole-script `once/deny` approval, optional host Python
>= 3.8 execution, direct guardian supervision, process-tree cancellation,
bounded sanitized output, and allowlisted nested Hermes tool RPC. Session
schema v10 records nested invocations with immutable `codeRpc` origin, parent
call, and monotonic sequence while keeping source, output, file content, and
nested arguments out of public Run/SSE/Message projections. Run E2E proves
approval-before-spawn, nested `read_file`, secret-free child environments,
deny-with-zero-start, cancellation/tree cleanup, and public-data redaction.

## Latest verification

- Backend: MSVC Rust 1.88 fmt, all-targets check, `clippy -D warnings` and the
  complete serial Windows test matrix pass: 493 passed, 0 failed and one
  intentionally ignored native-keychain test. This includes 364 library tests,
  2 backend-binary tests and every integration binary.
  The real-process `runtime_log_redaction` test passes, and the opt-in Windows
  Credential Manager test passes separately 1/1 with exact native-value checks.
- Desktop: fmt, check, clippy and 21/21 tests pass. The independent supervisor,
  dynamic port/token generations, stop-before-restart behavior and production
  frontend runtime-config bridge are covered.
- Frontend: Node 22.14.0, 502/502 tests and the production build pass.
  OpenAPI generated-type drift, Redocly 2.39 lint, the AST-based Tauri bridge
  gate, and root/frontend `npm audit` all pass with 0 vulnerabilities.
- Playwright passes 12/12 in the latest 43.0-second local run under Node 22.14.0
  and npm 10.9.2: Profile/Session/Run/SSE/usage,
  Toolset/Skill lifecycle, a real Rust Workspace write/read/search/patch sequence
  with two different once approvals, strict private Provider results, patch
  precondition checks and redacted UI progress, a Clarification answer retained
  only in the private continuation,
  stdio MCP config/discovery/approval/call/delete with a private result,
  approved foreground and background terminal commands with private stdout,
  durable cross-Run delivery and process cleanup, an approved real Python
  `execute_code` with private source/output,
  a real Chromium navigation/snapshot flow, an owner-bound once-approved bounded
  CDP `Runtime.evaluate` that injects a unique download link, a fresh snapshot,
  and a separate once-approved isolated download whose content, URL and path
  remain private. Completed-Run recovery, in-flight backend crash recovery,
  message FTS5 search/continuation and builtin Memory CRUD/search are also covered.
  The expanded Browser test passes independently 1/1 (5.2 seconds) and together
  with the real-Python Code path 2/2 (8.7 seconds); both runs and the full suite
  left zero backend or E2E temp residue. The Browser -> Terminal -> approval ->
  Skills order passes 4/4 (21.8 seconds), and the Browser Rust targeted matrix
  passes 9/9. An earlier intervening host-pressure run
  exceeded Browser/Code UI waits and an incorrectly shorter 15-minute outer
  timeout; the runner still removed its backend/process/temp resources. It is
  retained as a timing-flake observation, not counted as another full pass.
- The mixed-runtime verifier self-tests pass, including the eight-hour duration
  bound and adaptive full-window resource retention (5,762 samples at the
  default five-second interval). New reports also count every null backend RSS
  probe as `backendRssUnavailable` instead of leaving missing samples implicit.
  Its bounded real-backend smoke
  completes 4/4 Profile/Session/Run/SSE/FTS iterations with zero workload or
  Provider failures and clean backend/provider/temp teardown. The final
  post-retention evidence is
  `logs/phase4/mixed-runtime-post-retention-2026-07-20.json`.
  This is a correctness smoke; the separate 30- and 60-minute mixed pilots are
  recorded below, while the 8-hour soak remains open.
- The 30-minute mixed pilot passed 1,094/1,094 Profile/Session/Run/SSE/FTS
  iterations with 1,094 Provider requests, zero failures, 356 resource samples,
  and clean backend/provider/temp teardown. Backend RSS moved from 33.71 MiB to
  44.34 MiB with a measured full-window slope of +20.87 MiB/h; this is not a
  leak finding by itself, but it is sufficient reason to keep the 8-hour gate
  open.
- The 60-minute extension passed 2,238/2,238 iterations and 2,238 Provider
  requests with zero failures. Of 719 retained resource samples, 715 included
  backend RSS, four were unavailable, none were dropped and one interval was
  skipped. RSS moved from 31.95 MiB to 45.06 MiB with a 51.70 MiB peak; linear
  slopes were +11.00 MiB/h over the full window and +4.70 MiB/h over the final
  30 minutes. The latter is encouraging but not a no-leak result, so the 8-hour
  gate remains open. The result file is
  `logs/phase4/mixed-runtime-pilot-60m-2026-07-20.json` (SHA-256
  `8551D96F6D133564A70BFE37E625777DE3C74DC0069C144593D2D12E2827D211`).
- The current tree produced
  `desktop/target/release/bundle/nsis/SynthChat_1.1.0_x64-setup.exe` (26,009,305
  bytes, SHA-256
  `DFA82F256A0251B025BB78F68EE72FF3C1E622233DA9992D41CAF24E6AC81216`).
  `scripts/verify-nsis-artifact.ps1` passed 7-Zip integrity/extraction, found
  only six NSIS plugin files plus the Desktop and Rust backend executables,
  rejected zero forbidden paths, found zero high-confidence credential
  signatures, verified the documented Tauri `UNK` to `NSS` desktop marker
  patch, and matched the sidecar exactly. Installer and payloads are
  `NotSigned`; `-RequireSignature` fails closed and leaves no audit temp.
- RustSec reports 0 vulnerability-level findings for both lockfiles. Backend's
  yanked `spin 0.9.8` was replaced by non-yanked `0.9.9`; Desktop retains one
  Linux-only unsound and 16 unmaintained warnings in the current upstream
  Tauri/GTK dependency graph.
- The ordinary release-input verifier still fails while 21 required migration,
  root-lock, E2E and release-verifier files are not Git-tracked. This is
  intentionally not bypassed or auto-staged. Strict candidate mode also blocks
  all 11 unverified Pet asset groups and any dirty worktree.

## Residual risks

- Static symlink and Windows reparse paths are rejected, but eliminating a
  same-user parent-directory replacement race requires handle-relative storage
  I/O and native Windows tests.
- Terminal/process is host-authority execution after approval, not an OS or
  container sandbox and not an exactly-once external transaction.
- `execute_code` also has host authority after whole-script approval. Its
  authenticated RPC service currently processes one connection serially, and
  the malformed-frame/multi-connection matrix and high-escape-density output
  boundary still need broader adversarial coverage.
- The full local history scan found ten distinct high-confidence
  OpenAI-compatible credentials in legacy `synthchat-data` JSON; three also
  remain in ignored working-tree runtime data. The index and `.env*` had no real
  credential hits, and the SQLite follow-up found none. All ten credentials
  still require immediate revocation/rotation and an approved history rewrite.
- The current migration contains thousands of worktree changes and is not yet a
  reviewable commit series. Do not use the percentages as release readiness.
- All 11 vendored Pet runtime/model groups remain `unverified`, so
  release-candidate packaging must continue to fail. PixiJS 6.5.10 and
  `pixi-live2d-display@0.4.0-beta.2` match MIT upstream distributions but lack
  local LICENSE/NOTICE files; Cubism Core matches SDK 5-r.4 without retained
  proprietary-license or redistribution evidence, and the Cubism 2.1 runtime
  lacks authoritative historical license evidence. Six non-Natori model groups
  map to an official immutable sample-data commit, with Mao locally modified
  and Wanko a subset. Natori's current terms prohibit commercial use, modification and
  redistribution, making the bundled model a hard release blocker.
- Desktop's Linux GTK3 graph retains `RUSTSEC-2024-0429` and unmaintained GTK
  bindings. Current stable Tauri/wry/WebKitGTK versions still require glib 0.18;
  remediation is blocked on an upstream migration or a maintained fork.
- Tavily resolves extract targets remotely. Local DNS and address preflight
  rejects known private/special targets, but cannot pin the address Tavily
  ultimately connects to, so remote DNS rebinding TOCTOU remains a documented
  provider-boundary risk.
