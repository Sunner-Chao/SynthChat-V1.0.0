#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

BUNDLE="dmg"
TARGET="native"
OUTPUT_ROOT="$REPO_ROOT/release-dist/macos"
UPDATE_MANIFEST_URL="${SYNTHCHAT_UPDATE_MANIFEST_URL:-}"
SKIP_NPM_INSTALL=0
PREFLIGHT_ONLY=0
FAST_INCREMENTAL=0
OPEN_OUTPUT=0

usage() {
  cat <<'EOF'
Usage: scripts/build-macos-native.sh [options]

Options:
  --bundle <all|app|dmg>          Bundle type to build. Default: dmg.
  --target <native|universal|aarch64|x86_64>
                                  Build target. Default: native.
  --output <dir>                  Artifact copy destination. Default: release-dist/macos.
  --update-manifest-url <url>     Inject SYNTHCHAT_UPDATE_MANIFEST_URL for this build.
  --skip-npm-install              Do not install node dependencies when node_modules is missing.
  --preflight-only                Run checks and exit before building.
  --fast-incremental              Enable Cargo release incremental mode.
  --open-output                   Open output directory after copying artifacts.
  -h, --help                      Show this help.

Examples:
  bash scripts/build-macos-native.sh
  bash scripts/build-macos-native.sh --bundle dmg --target universal
  bash scripts/build-macos-native.sh --preflight-only
EOF
}

die() {
  echo "ERROR: $*" >&2
  exit 1
}

step() {
  printf '\n==> %s\n' "$*"
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "Required command not found: $1"
}

require_path() {
  local path="$1"
  local label="$2"
  [[ -e "$path" ]] || die "$label is missing: $path"
}

json_value() {
  local file="$1"
  local expr="$2"
  node -e "const fs=require('fs'); const v=JSON.parse(fs.readFileSync(process.argv[1],'utf8')); const out=($expr); if (out !== undefined && out !== null) process.stdout.write(String(out));" "$file"
}

get_app_version() {
  local version
  version="$(json_value "$REPO_ROOT/src-tauri/tauri.conf.json" "v.version" || true)"
  if [[ -z "${version// }" ]]; then
    version="$(json_value "$REPO_ROOT/package.json" "v.version" || true)"
  fi
  printf '%s' "$version"
}

github_asset_base_url() {
  local remote owner repo
  remote="$(git -C "$REPO_ROOT" remote get-url origin 2>/dev/null || true)"
  [[ -n "$remote" ]] || return 0
  if [[ "$remote" =~ ^git@github\.com:([^/]+)/([^/.]+)(\.git)?$ ]]; then
    owner="${BASH_REMATCH[1]}"
    repo="${BASH_REMATCH[2]}"
  elif [[ "$remote" =~ ^https://github\.com/([^/]+)/([^/.]+)(\.git)?$ ]]; then
    owner="${BASH_REMATCH[1]}"
    repo="${BASH_REMATCH[2]}"
  else
    return 0
  fi
  printf 'https://github.com/%s/%s/releases/latest/download' "$owner" "$repo"
}

write_update_manifest() {
  local installer="$1"
  local asset_base version file_name encoded
  asset_base="$(github_asset_base_url)"
  if [[ -z "$asset_base" ]]; then
    echo "Skipped update-manifest.json: GitHub origin remote not detected."
    return 0
  fi
  version="$(get_app_version)"
  file_name="$(basename "$installer")"
  encoded="$(node -e "process.stdout.write(encodeURIComponent(process.argv[1]))" "$file_name")"
  node <<EOF
const fs = require("fs");
const manifest = {
  latestVersion: "$version",
  downloadUrl: "$asset_base/$encoded",
  releaseUrl: "$asset_base".replace(/\/download$/, ""),
  publishedAt: new Date().toISOString(),
  notes: "SynthChat $version"
};
fs.writeFileSync("$OUTPUT_ROOT/update-manifest.json", JSON.stringify(manifest, null, 2) + "\n");
EOF
  echo "  $OUTPUT_ROOT/update-manifest.json"
}

target_triple() {
  case "$TARGET" in
    native) echo "" ;;
    universal) echo "universal-apple-darwin" ;;
    aarch64) echo "aarch64-apple-darwin" ;;
    x86_64) echo "x86_64-apple-darwin" ;;
    *) die "Unsupported target: $TARGET" ;;
  esac
}

ensure_rust_targets() {
  case "$TARGET" in
    native) return 0 ;;
    universal)
      rustup target add aarch64-apple-darwin x86_64-apple-darwin
      ;;
    aarch64)
      rustup target add aarch64-apple-darwin
      ;;
    x86_64)
      rustup target add x86_64-apple-darwin
      ;;
  esac
}

collect_artifacts() {
  local triple="$1"
  local roots=()
  local found=0
  roots+=("$REPO_ROOT/src-tauri/target/release/bundle")
  if [[ -n "$triple" ]]; then
    roots+=("$REPO_ROOT/src-tauri/target/$triple/release/bundle")
  fi

  rm -rf "$OUTPUT_ROOT"
  mkdir -p "$OUTPUT_ROOT"

  for root in "${roots[@]}"; do
    [[ -d "$root" ]] || continue
    while IFS= read -r -d '' item; do
      local base="$OUTPUT_ROOT/$(basename "$item")"
      if [[ -d "$item" ]]; then
        rm -rf "$base"
        ditto "$item" "$base"
      else
        cp -f "$item" "$base"
      fi
      found=1
    done < <(find "$root" \( -name "*.dmg" -o -name "*.app" -o -name "*.zip" -o -name "*.tar.gz" -o -name "*.json" -o -name "*.sig" \) -print0)
  done

  [[ "$found" -eq 1 ]] || die "No macOS artifacts found under src-tauri/target/*/release/bundle"

  echo
  echo "Copied artifacts:"
  find "$OUTPUT_ROOT" -maxdepth 1 -mindepth 1 -print | sort | sed 's/^/  /'

  local installer
  installer="$(find "$OUTPUT_ROOT" -maxdepth 1 -type f \( -name "*.dmg" -o -name "*.zip" \) | sort | head -n 1 || true)"
  if [[ -n "$installer" ]]; then
    write_update_manifest "$installer"
  else
    echo "Skipped update-manifest.json: no .dmg or .zip artifact found."
  fi
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --bundle)
      [[ $# -ge 2 ]] || die "--bundle requires a value"
      BUNDLE="$2"; shift 2 ;;
    --bundle=*)
      BUNDLE="${1#*=}"; shift ;;
    --target)
      [[ $# -ge 2 ]] || die "--target requires a value"
      TARGET="$2"; shift 2 ;;
    --target=*)
      TARGET="${1#*=}"; shift ;;
    --output)
      [[ $# -ge 2 ]] || die "--output requires a value"
      OUTPUT_ROOT="$2"; shift 2 ;;
    --output=*)
      OUTPUT_ROOT="${1#*=}"; shift ;;
    --update-manifest-url)
      [[ $# -ge 2 ]] || die "--update-manifest-url requires a value"
      UPDATE_MANIFEST_URL="$2"; shift 2 ;;
    --update-manifest-url=*)
      UPDATE_MANIFEST_URL="${1#*=}"; shift ;;
    --skip-npm-install)
      SKIP_NPM_INSTALL=1; shift ;;
    --preflight-only)
      PREFLIGHT_ONLY=1; shift ;;
    --fast-incremental)
      FAST_INCREMENTAL=1; shift ;;
    --open-output)
      OPEN_OUTPUT=1; shift ;;
    -h|--help)
      usage; exit 0 ;;
    *)
      die "Unknown option: $1" ;;
  esac
done

case "$BUNDLE" in
  all|app|dmg) ;;
  *) die "Unsupported bundle: $BUNDLE" ;;
esac

TRIPLE="$(target_triple)"

cd "$REPO_ROOT"

step "Checking macOS build host"
[[ "$(uname -s)" == "Darwin" ]] || die "macOS packaging must run on macOS. Use a Mac or a macOS CI runner."
require_command node
require_command npm
require_command cargo
require_command rustc
require_command rustup
require_command git
require_command xcode-select
require_command codesign
require_command hdiutil
require_command ditto
xcode-select -p >/dev/null 2>&1 || die "Xcode Command Line Tools are missing. Run: xcode-select --install"

step "Checking bundled resources"
require_path "$REPO_ROOT/package.json" "npm package manifest"
require_path "$REPO_ROOT/src-tauri/tauri.conf.json" "Tauri config"
require_path "$REPO_ROOT/src-tauri/icons/icon.icns" "macOS app icon"
require_path "$REPO_ROOT/public/pet/index.html" "pet static entry"
require_path "$REPO_ROOT/public/pet/pet.js" "pet static script"
require_path "$REPO_ROOT/data/tts/chattts_synth.py" "bundled ChatTTS synthesis script"
require_path "$REPO_ROOT/data/emoji/default" "bundled default emoji pack"
require_path "$REPO_ROOT/skills" "bundled skills directory"

node <<'EOF'
const fs = require("fs");
const config = JSON.parse(fs.readFileSync("src-tauri/tauri.conf.json", "utf8"));
const resources = Object.values(config.bundle?.resources || {});
const required = ["skills", "public/pet", "data/tts", "data/emoji"];
const missing = required.filter((item) => !resources.includes(item));
if (missing.length) {
  console.error(`Tauri bundle.resources is missing: ${missing.join(", ")}`);
  process.exit(1);
}
EOF

if [[ -n "${UPDATE_MANIFEST_URL// }" ]]; then
  export SYNTHCHAT_UPDATE_MANIFEST_URL="$UPDATE_MANIFEST_URL"
  echo "Using update manifest: $SYNTHCHAT_UPDATE_MANIFEST_URL"
fi

if [[ "$PREFLIGHT_ONLY" -eq 1 ]]; then
  echo "Preflight complete."
  exit 0
fi

if [[ "$SKIP_NPM_INSTALL" -eq 0 && ! -d "$REPO_ROOT/node_modules" ]]; then
  step "Installing frontend dependencies"
  if [[ -f "$REPO_ROOT/package-lock.json" ]]; then
    npm ci
  else
    npm install
  fi
fi

step "Preparing Rust target"
ensure_rust_targets

if [[ "$FAST_INCREMENTAL" -eq 1 ]]; then
  export CARGO_INCREMENTAL=1
  export CARGO_PROFILE_RELEASE_INCREMENTAL=true
  if [[ -z "${CARGO_BUILD_JOBS:-}" ]]; then
    CARGO_BUILD_JOBS="$(sysctl -n hw.ncpu 2>/dev/null || echo 4)"
    export CARGO_BUILD_JOBS
  fi
  echo "Fast incremental mode: Cargo release incremental enabled, jobs=$CARGO_BUILD_JOBS"
fi

step "Building SynthChat native macOS package"
TAURI_ARGS=(run tauri -- build)
if [[ "$BUNDLE" != "all" ]]; then
  TAURI_ARGS+=(--bundles "$BUNDLE")
fi
if [[ -n "$TRIPLE" ]]; then
  TAURI_ARGS+=(--target "$TRIPLE")
fi
npm "${TAURI_ARGS[@]}"

step "Collecting macOS artifacts"
collect_artifacts "$TRIPLE"

if [[ "$OPEN_OUTPUT" -eq 1 ]]; then
  open "$OUTPUT_ROOT"
fi

echo
echo "Done."
echo "Output: $OUTPUT_ROOT"
