# Linux Native Build

The Linux desktop package contains only three built inputs:

- `frontend/`: React/Vite assets embedded by the Tauri desktop shell;
- `backend/`: the `synthchat-hermes-backend` Rust sidecar;
- `desktop/`: the Tauri shell that starts and owns that sidecar.

No Node runtime, Python Agent/MCP/TTS runtime, `synthchat-data/`, Hermes
profile data, local databases, `.env` files, or legacy `src-tauri/` tree is a
permitted package payload.

## Native Host Requirement

Run the packaging command on Linux. The script reads the `rustc` host triple
and refuses a requested target that differs from it, so a cross-compiled binary
cannot be labelled as a tested Linux desktop package. Build each Linux
architecture on a native runner for that architecture.

## Prerequisites

- Node.js 22 or newer and npm;
- Rust 1.88 or newer, with the native host target installed;
- a C compiler, `pkg-config`, GTK 3, WebKitGTK (`webkit2gtk-4.1` or
  `webkit2gtk-4.0`), librsvg, and Ayatana/AppIndicator development files;
- `dpkg-deb` for `.deb`, `unsquashfs` from `squashfs-tools` for AppImage
  inspection, and `rpmbuild` plus `rpm` only when building `.rpm`.

For example, a Debian/Ubuntu development host normally needs packages in the
following families: `build-essential`, `pkg-config`, `libgtk-3-dev`,
`libwebkit2gtk-4.1-dev`, `libayatana-appindicator3-dev`, `librsvg2-dev`,
`dpkg-dev`, and `squashfs-tools`. Package names vary by distribution; the
script checks the installed commands and `pkg-config` entries before it writes
any build outputs.

## Build

From the repository root on a native Linux host:

```bash
bash scripts/build-linux-native.sh
```

The default formats are `appimage,deb`. Add RPM only on a host with the RPM
toolchain:

```bash
bash scripts/build-linux-native.sh --bundles appimage,deb,rpm
```

The normal path runs `npm ci --prefix frontend`, then uses locked Cargo builds
for both Rust crates. To reuse an existing, already reviewed frontend dependency
tree, use either `--skip-npm-install` or `SYNTHCHAT_SKIP_NPM_INSTALL=1`.

Use the preflight command to validate the host, locks, target, package tools,
and Tauri CLI without compiling a package:

```bash
bash scripts/build-linux-native.sh --preflight
```

The script writes artifacts below the target-specific directory
`desktop/target/<native-target>/release/bundle/`, prints absolute artifact paths
and SHA-256 hashes, and inspects each resulting package. It requires the Rust
sidecar to be present and rejects paths for Python, legacy Agent, Node
dependencies, profile/user data, databases, and environment files.

The resulting packages are unsigned development artifacts. A release still
requires a native clean-account install/launch/upgrade/uninstall smoke test,
backend lifecycle verification, artifact provenance and signing evidence.
