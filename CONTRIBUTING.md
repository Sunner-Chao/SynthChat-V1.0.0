# Contributing to SynthChat

SynthChat keeps the desktop UI in `frontend/`, the Hermes-compatible runtime in
`backend/`, and the narrow Tauri desktop shell in `desktop/`. The backend is a
pure Rust process. Upstream Hermes sources are compatibility references, not a
runtime dependency. Do not reintroduce the removed Agent IPC, Python Agent
runtime, bundled MCP runtime, or generated user data as a shortcut.

## Before you start

- Use Node.js 22, Rust 1.88 or later, and the Tauri prerequisites for the host
  platform. See [the development guide](docs/development.md) for the exact
  local commands.
- Work from a clean, reviewable change set. Build output, local databases,
  `synthchat-data/`, logs, `.env` files, credential exports, and
  `desktop/binaries/` must not be committed.
- Treat existing uncommitted files as another contributor's work. Do not reset,
  clean, delete, or regenerate unrelated files while preparing a change.
- Use the desktop app for authenticated end-to-end development. A manually set
  backend token is only for local diagnostics and must never be placed in a
  source file, Vite variable, test fixture, issue, or pull request log.

## Local workflow

Install the locked frontend dependencies once:

```powershell
npm ci --prefix frontend
```

For the normal desktop flow:

```powershell
npm run desktop
```

The Tauri shell launches the loopback Rust backend and passes a per-launch
token over stdin. It owns backend shutdown. Do not start an external Agent
process or add a second frontend-to-desktop bridge.

For a backend-only diagnostic session, follow the constrained command in
[docs/development.md](docs/development.md). Use a newly generated local token
and clear it from the shell after the diagnostic session.

## Change boundaries

Keep changes in the owning layer:

- `frontend/` consumes the versioned HTTP/SSE contract through its API client.
  It must not reach into the Rust filesystem or use arbitrary Tauri commands.
- `backend/` owns API behavior, persisted state, credential access, tool
  execution, and migrations. A contract change starts by updating the OpenAPI
  document and its generated frontend types.
- `desktop/` owns process lifecycle and the deliberately small connection
  bridge. It does not contain Agent logic.
- `docs/` records guarantees and evidence. State an unverified result as a
  pending gate, not as a completed release claim.

For a new or changed endpoint, update `docs/openapi.yaml`, regenerate the
frontend declaration, and run `npm --prefix frontend run api:check` before
requesting review. Preserve the fail-closed behavior of local authentication,
approval, filesystem, process, and secret boundaries.

## Required checks

Run the checks affected by a change. A full pre-release baseline is documented
in [docs/release.md](docs/release.md).

```powershell
npm --prefix frontend run api:check
npm --prefix frontend run build
npm --prefix frontend test
cargo fmt --manifest-path backend/Cargo.toml -- --check
cargo clippy --manifest-path backend/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path backend/Cargo.toml --all-targets
cargo fmt --manifest-path desktop/Cargo.toml -- --check
cargo clippy --manifest-path desktop/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path desktop/Cargo.toml --all-targets
node scripts/verify-release-inputs.mjs
```

Use `--locked` with Cargo when validating a release candidate. The version and
lockfile procedure is in the release guide; do not hand-edit lockfiles.

## Reporting bugs and reviews

For ordinary defects, use the project's normal issue tracker. Include the
application version, platform and architecture, reproduction steps, expected
and actual behavior, and redacted logs. Never attach a profile secret, bearer
token, session database, user workspace, or `synthchat-data/` export.

Security vulnerabilities must not be filed as public bugs. Follow
[SECURITY.md](SECURITY.md) instead.

## Pull requests

Describe the user-visible and contract impact, the tests actually run, and any
remaining platform or security uncertainty. Keep generated output out of the
change unless it is an explicitly committed contract artifact. A reviewer must
be able to distinguish implementation evidence from a proposed future test.

