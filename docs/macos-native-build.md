# macOS native build

The macOS package uses the same three-directory layout as Windows. The Rust
backend is compiled for the active target triple and bundled as a Tauri sidecar;
no Python or Node Agent runtime is shipped.

## Prerequisites

- macOS with Xcode Command Line Tools
- Node.js 22 or newer
- Rust stable with the target required by the release architecture

## Build

```bash
./scripts/build-macos-native.sh
```

Set `SYNTHCHAT_SKIP_NPM_INSTALL=1` to reuse an existing `frontend/node_modules`
directory. The script creates the `app` and `dmg` bundles under
`desktop/target/release/bundle/`.

For universal distribution, run the build in the release pipeline for both
Apple target triples and combine/sign the resulting binaries before the Tauri
bundle step. Signing, notarization, and credential injection belong in CI and
must not be stored in this repository.
