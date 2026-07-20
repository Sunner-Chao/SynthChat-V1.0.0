# Release Guide

This is a release runbook, not a claim that a release is ready. A product
artifact may be published only after every applicable gate below has current,
reviewable evidence.

## What is being built

SynthChat has three independently built pieces:

| Directory | Role | Runtime boundary |
| --- | --- | --- |
| `frontend/` | React/Vite desktop UI assets | Node is a build-time dependency, not an Agent runtime. |
| `backend/` | Hermes-compatible HTTP/SSE service | Pure Rust backend; upstream Hermes and Python Agent processes are not launched. Optional host Python can execute an explicitly approved user script only. |
| `desktop/` | Tauri shell and backend lifecycle | Starts the Rust sidecar, transfers the per-launch token over stdin, and owns shutdown. |

The final package must contain the built frontend, Rust backend sidecar, and
desktop shell. It must not contain `synthchat-data/`, local databases, profile
secrets, `.env` files, Node modules, Python Agent/MCP/TTS runtime, or the
removed `src-tauri/` crate.

Ignored local output such as `release-dist/` is not release provenance. Never
reuse an old installer merely because its filename matches the current version;
build a fresh candidate from the reviewed commit and record its hash and
verification evidence.

## Current platform status

| Platform | Available repository path | Current evidence | Release limitation |
| --- | --- | --- | --- |
| Windows x86_64 | `build-one-click.ps1` or `scripts/build-windows-native.ps1` invokes Tauri with `nsis`, `msi`, or `all`. | A current-tree NSIS development artifact passed integrity, payload-lineage, forbidden-path and high-confidence credential-signature checks. | Clean-account install, launch, upgrade/uninstall, signing and malware-review evidence remain required. No signing claim is made here. |
| macOS | `scripts/build-macos-native.sh` invokes Tauri for `app,dmg` on a native macOS host. | A source build path exists. | No macOS bundle was verified by this guide. Universal assembly, signing, notarization, and credential injection are not implemented or evidenced in this repository. |
| Linux | `scripts/build-linux-native.sh` builds native AppImage/deb by default, with RPM opt-in, on a native Linux host. | Script syntax, help, and non-Linux refusal were checked on Windows; CI compiles/tests the desktop shell on Ubuntu. | No native Linux package artifact, clean-account install smoke, signing setup, or release evidence exists yet. Linux distribution is not ready to claim. |

Tauri's generic configuration does not substitute for native-package evidence.
Do not cross-compile a desktop release and call it validated without executing
the relevant native install and process-lifecycle tests.

## Version and dependency locks

The current desktop product version is duplicated intentionally in these files:

- root `package.json`;
- `frontend/package.json`;
- `desktop/Cargo.toml`;
- `desktop/tauri.conf.json`.

Update all four together for a product release, including visible desktop title
text if it embeds the version. `backend/Cargo.toml` has a separate service/API
crate version; change it only when its own compatibility and release policy
requires it. Do not silently use the desktop version to imply an API contract
change.

The required committed dependency locks are:

- root `package-lock.json` for Playwright and release/E2E tooling;
- `frontend/package-lock.json`;
- `backend/Cargo.lock`;
- `desktop/Cargo.lock`.

Use root `npm ci` for Playwright/release tooling and `npm ci --prefix frontend`
for the UI dependency tree. Use Cargo commands with
`--locked` when checking release inputs and building candidate artifacts. Make
an intentional dependency update in its owning directory, review the resulting
lockfile diff, and rerun all relevant tests. Never hand-edit a lockfile or run
a broad update while preparing an unrelated release.

The static, non-mutating input check verifies version alignment, required
lockfiles, delivery documentation, build entrypoints, and tracked-file hygiene:

```powershell
node scripts/verify-release-inputs.mjs
```

It cannot prove that a lock is resolvable. The `npm ci` and `cargo --locked`
commands in the next section provide that evidence.

### Pet asset provenance

The desktop Pet bundles four browser runtime libraries and seven Live2D model
groups outside package-manager lockfiles. Their current inventory is explicitly
**unverified** in [pet-asset-provenance.json](pet-asset-provenance.json). The
ordinary source-input check reports that status without blocking development
builds or tests; it is not a license or redistribution approval.

Before a candidate is signed or presented as releasable, run the strict gate
from a clean checkout:

```powershell
node scripts/verify-release-inputs.mjs --require-clean --release-candidate
```

The gate blocks until each vendored library/model group has documented source
provenance, a license and redistribution evidence, an audit review, and a
matching `sha256-tree-v1` digest. `SYNTHCHAT_RELEASE_CANDIDATE=1` and
`SYNTHCHAT_SIGNING=1` also activate this strict mode. See
[pet-asset-provenance.md](pet-asset-provenance.md) for the required fields and
digest format.

## Candidate verification gate

Run from a clean source checkout on the target platform. These commands do not
delete caches or user data. `npm ci` changes only the local dependency install;
Cargo may populate its normal local cache. Although development supports the
declared Node 22 range, release evidence and CI require exact Node `22.14.0` and
npm `10.9.2`; verify both before running the gate.

```powershell
node scripts/verify-release-inputs.mjs --require-clean --release-candidate
npm ci
npm ci --prefix frontend
npm --prefix frontend run api:check
npm --prefix frontend run api:lint
npm --prefix frontend run build
npm --prefix frontend test
npm exec -- playwright install chromium
npm run test:e2e
npm run verify:mixed-runtime
npm run verify:mixed-runtime-evidence
cargo fmt --manifest-path backend/Cargo.toml -- --check
cargo check --locked --manifest-path backend/Cargo.toml --all-targets
cargo clippy --locked --manifest-path backend/Cargo.toml --all-targets -- -D warnings
cargo test --locked --manifest-path backend/Cargo.toml --all-targets
cargo fmt --manifest-path desktop/Cargo.toml -- --check
cargo check --locked --manifest-path desktop/Cargo.toml --all-targets
cargo clippy --locked --manifest-path desktop/Cargo.toml --all-targets -- -D warnings
cargo test --locked --manifest-path desktop/Cargo.toml --all-targets
```

On Windows, also run the short runtime boundary verification with a fresh build:

```powershell
pwsh -NoProfile -File .\scripts\verify-backend-runtime.ps1 `
  -DurationSeconds 10 -Concurrency 4 -SampleIntervalSeconds 1 `
  -IncludeFaultChecks `
  -ResultPath .\logs\phase4\runtime-release-candidate.json
```

The current runtime script covers loopback startup, authentication, CORS,
controlled shutdown/restart, and short read-only load. It does not replace
browser UI end-to-end, provider streaming, tool, keychain, soak, leak, or
native crash/process testing. Record the exact command, commit, host platform,
artifact hash, and outcome for every release candidate.

Also run the bounded real-backend mixed smoke and retain its JSON result:

```powershell
node scripts/verify-mixed-runtime.mjs --smoke `
  --output .\logs\phase4\mixed-runtime-release-candidate.json
```

The schema v2 smoke creates real Profile/Session/Run/SSE/FTS traffic against a
local deterministic Provider, includes periodic read-only `session_search` tool
continuations, validates every SSE envelope/sequence/tool lifecycle plus global
event conservation, and verifies cleanup. The historical 60-minute extension
passed 2,238/2,238 iterations with the earlier text-only workload; neither it nor
the smoke replaces the eight-hour soak/leak gate.

Build the candidate backend from the clean candidate commit before starting the
eight-hour workload. Do not reuse a debug or stale binary. On Windows, use the
exact build and argument order below:

```powershell
cargo +1.88.0-x86_64-pc-windows-msvc build --locked --release `
  --manifest-path backend/Cargo.toml --bin synthchat-hermes-backend
node scripts/verify-mixed-runtime.mjs `
  --duration-seconds 28800 --concurrency 2 --cycle-delay-ms 3000 `
  --max-failures 25 --latency-sample-limit 5000 --provider-delay-ms 10 `
  --resource-interval-ms 5000 --resource-sample-limit 5762 --resource-samples `
  --tool-every-iterations 10 `
  --backend-bin backend/target/release/synthchat-hermes-backend.exe --skip-build `
  --output docs/release-evidence/mixed-runtime-8h.json
```

On macOS or Linux, use the pinned Unix toolchain and the extensionless release
binary; all verifier arguments remain in the same order:

```bash
cargo +1.88.0 build --locked --release \
  --manifest-path backend/Cargo.toml --bin synthchat-hermes-backend
node scripts/verify-mixed-runtime.mjs \
  --duration-seconds 28800 --concurrency 2 --cycle-delay-ms 3000 \
  --max-failures 25 --latency-sample-limit 5000 --provider-delay-ms 10 \
  --resource-interval-ms 5000 --resource-sample-limit 5762 --resource-samples \
  --tool-every-iterations 10 \
  --backend-bin backend/target/release/synthchat-hermes-backend --skip-build \
  --output docs/release-evidence/mixed-runtime-8h.json
```

The raw report is canonical one-line JSON and embeds producer-time platform,
toolchain, Git state, verifier/backend SHA-256, and effective invocation hashes.

After completion, review the full/late-window RSS slopes, latency tails, sample
availability and cleanup, then create the reviewed manifest described in
[release-evidence/README.md](release-evidence/README.md). The strict evidence
verifier requires canonical bytes, matching raw/backend/verifier hashes, exact
argv and no unrecorded environment overrides. A dirty worktree result may inform
development performance review but cannot pass release-candidate provenance.
This workload covers one long-lived backend with Run/SSE/SQLite/FTS and
`session_search`; queue/restart, Terminal/process, Browser, MCP and native
three-platform lifecycle evidence remain separate gates.
The accepted backend digest must also match the release sidecar input and the
packaged sidecar attestation produced by the target platform's artifact verifier;
the soak verifier alone does not prove that packaging used the same bytes.

## Build and inspect artifacts

### Windows

Use a native Windows environment with the prerequisites in
[windows-native-build.md](windows-native-build.md):

```powershell
.\build-one-click.ps1 -Bundle nsis
.\scripts\verify-nsis-artifact.ps1 `
  -InstallerPath .\desktop\target\release\bundle\nsis\SynthChat_1.1.0_x64-setup.exe
```

The expected output root is `desktop/target/release/bundle/`. Choose `-Bundle
msi` only when the MSI path has been tested. The NSIS verifier uses 7-Zip
without running the installer. It checks integrity, compares the packaged
desktop and Rust backend executables with the release build inputs, rejects
legacy runtime/user-data paths, scans for high-confidence credential material,
and reports SHA-256 and Authenticode status. Pass `-RequireSignature` for a
signed release candidate; unsigned development candidates must retain the
reported `NotSigned` status. Then install and verify first launch, backend
startup/shutdown, authenticated UI operation, upgrade, and uninstall on a clean
test account.

The 2026-07-20 current-tree development artifact is
`SynthChat_1.1.0_x64-setup.exe`, 26,009,305 bytes, SHA-256
`DFA82F256A0251B025BB78F68EE72FF3C1E622233DA9992D41CAF24E6AC81216`.
Its eight extracted files were six NSIS plugins plus the Desktop executable and
the exact Rust sidecar; the Desktop differed from the restored release binary
only by Tauri's documented `__TAURI_BUNDLE_TYPE_VAR_UNK` to `..._NSS` patch.
All three executables report `NotSigned`, so this evidence does not close the
signed-candidate or clean-account installation gates.

### macOS

Use a native macOS host with the prerequisites in
[macos-native-build.md](macos-native-build.md):

```bash
./scripts/build-macos-native.sh
```

The script requests `app,dmg` and writes under
`desktop/target/release/bundle/`. A release owner must separately provide a
native artifact hash, launch/install smoke result, architecture result, and
signing/notarization evidence. No secrets for signing or notarization belong in
the source tree.

### Linux

Use a native Linux host with the prerequisites in
[linux-native-build.md](linux-native-build.md):

```bash
./scripts/build-linux-native.sh
```

The script defaults to AppImage and deb; pass its documented bundle option to
request RPM where the native host provides `rpmbuild`. It uses locked frontend
and Cargo builds, validates the backend sidecar, and rejects Python, legacy
Agent, Node runtime, user-data, database, and `.env` paths from package
payloads. It has not yet produced a verified native artifact. Before supporting
Linux distribution, run it on the target distribution, record package hashes,
perform clean-account install/launch/upgrade/uninstall/process-lifecycle
verification, and establish signing/provenance evidence. A successful Ubuntu
CI compile/test does not close this gate.

## Release decision checklist

- [ ] Version fields and dependency locks have been reviewed and the static
      input checker passes.
- [ ] Frontend, backend, desktop, and API-contract checks pass from the locked
      source inputs.
- [ ] The target platform's native package was built from the candidate commit
      and its checksums, installer contents, and smoke results were recorded.
- [ ] Browser UI regression, disconnect/backend-crash recovery, and the
      relevant Rust end-to-end workflows have passed.
- [ ] Load/soak/leak and native process-lifecycle evidence meets the release
      criteria for each supported platform.
- [ ] Security review includes real keychain behavior, artifact/source secret
      scanning, log-redaction paths, and the unresolved-risk review in
      [security-report.md](security-report.md).
- [ ] The strict Pet asset provenance gate passes with an independently
      reviewed source, license, redistribution evidence, and matching hashes
      for every vendored library and model group.
- [ ] Required signing, notarization, provenance, and vulnerability review are
      complete for the selected distribution channel.
- [ ] Release notes describe known capability and platform limitations without
      promoting unverified behavior.

Until every item is checked with current evidence, distribute only as a
development or test build, not as a production release.
