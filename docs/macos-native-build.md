# macOS Native Build

SynthChat uses Tauri release builds for native macOS packaging.

macOS packages must be built on macOS. Use a physical Mac, a Mac VM that has
Apple build tools, or a CI runner such as GitHub Actions `macos-latest`.
Building a signed `.app` or `.dmg` from Windows is not practical because the
bundle step depends on Apple tools such as `codesign`, `hdiutil`, and Xcode
Command Line Tools.

## Prerequisites

- macOS with Xcode Command Line Tools:

```bash
xcode-select --install
```

- Node.js and npm.
- Rust toolchain with Cargo and rustup.
- Project dependencies installed, or let the build script install them.

For universal Intel + Apple Silicon builds, the script installs these Rust
targets automatically:

```bash
rustup target add aarch64-apple-darwin x86_64-apple-darwin
```

## Build

From the repository root on macOS:

```bash
npm run desktop:build:macos
```

Equivalent direct command:

```bash
bash scripts/build-macos-native.sh
```

Run preflight checks only:

```bash
npm run desktop:build:macos -- --preflight-only
```

Build a universal `.dmg`:

```bash
npm run desktop:build:macos -- --bundle dmg --target universal
```

Build only the `.app` bundle:

```bash
npm run desktop:build:macos -- --bundle app
```

Artifacts are copied to:

```bash
release-dist/macos
```

The raw Tauri outputs remain under:

```bash
src-tauri/target/release/bundle
src-tauri/target/<target>/release/bundle
```

## Signing And Notarization

Unsigned builds are fine for local testing, but other Macs may show Gatekeeper
warnings. For distribution, sign and notarize with Apple Developer credentials.
Tauri reads the standard Apple signing/notarization environment variables.
Common variables include:

```bash
export APPLE_SIGNING_IDENTITY="Developer ID Application: Your Name (TEAMID)"
export APPLE_ID="apple-id@example.com"
export APPLE_PASSWORD="app-specific-password"
export APPLE_TEAM_ID="TEAMID"
```

Then run:

```bash
npm run desktop:build:macos -- --bundle dmg --target universal
```

If you use certificate-file based signing in CI, configure the matching Tauri
Apple certificate environment variables in the CI secret store before invoking
the same script.

## Notes

- macOS does not use Windows WebView2 packaging.
- `bundle.resources` must include `skills`, `public/pet`, `data/tts`, and
  `data/emoji`; the script checks these before building.
- The installer does not bundle external heavyweight ChatTTS model/runtime
  directories. Configure those paths on the target Mac if voice features need
  them.
- If the script can detect a GitHub `origin` remote and a `.dmg` or `.zip`
  artifact, it writes `release-dist/macos/update-manifest.json`.
