# Windows native build

The Windows package contains three independently built parts:

- `frontend/`: the React web assets;
- `backend/`: `synthchat-hermes-backend.exe`, bundled as a Tauri sidecar;
- `desktop/`: the minimal Tauri process and window shell.

## Prerequisites

- Node.js 22 or newer
- Rust stable with the `x86_64-pc-windows-msvc` target
- Visual Studio 2022 Build Tools with the Desktop C++ workload
- WebView2 Runtime
- 7-Zip 25.x for non-installing NSIS payload verification

## Build

From the repository root:

```powershell
.\build-one-click.ps1
```

The script installs the locked frontend dependencies, then Tauri runs the
`build:desktop-assets` hook. That hook builds the frontend and compiles the Rust
backend for the current target triple before bundling it as an external binary.

Useful variants:

```powershell
.\build-one-click.ps1 -PreflightOnly
.\build-one-click.ps1 -Bundle msi
.\build-one-click.ps1 -SkipNpmInstall -OpenOutput
```

Artifacts are written under `desktop/target/release/bundle/`. The package must
not contain `synthchat-data/`, the removed Python TTS/MCP runtime, or the legacy
`src-tauri/` crate.

Verify an NSIS artifact without installing it:

```powershell
.\scripts\verify-nsis-artifact.ps1 `
  -InstallerPath .\desktop\target\release\bundle\nsis\SynthChat_1.1.0_x64-setup.exe
```

The verifier checks NSIS integrity, packaged executable lineage, forbidden
paths, high-confidence credential signatures, SHA-256, and Authenticode status.
Use `-RequireSignature` only for a signed release candidate.
