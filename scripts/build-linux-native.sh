#!/usr/bin/env bash
# Build native Linux packages for the Tauri desktop shell and Rust backend sidecar.
# This script intentionally rejects cross-platform packaging: a package must be
# assembled and inspected on the Linux host that will run its Rust target.

set -euo pipefail
IFS=$'\n\t'

usage() {
  cat <<'EOF'
Usage: bash scripts/build-linux-native.sh [options]

Builds locked Linux AppImage and Debian packages by default. The build has three
explicit steps: build the frontend, compile and install the Rust backend
sidecar, then compile/package the Tauri desktop shell.

Options:
  --bundles <list>       Comma-separated bundle formats: appimage, deb, rpm.
                         Default: appimage,deb (or SYNTHCHAT_BUNDLES).
  --target <triple>      Native Rust target triple. It must exactly match the
                         rustc host triple (or SYNTHCHAT_TARGET).
  --skip-npm-install     Reuse frontend/node_modules instead of running npm ci.
  --preflight            Validate the native host and locked inputs without
                         compiling the frontend, backend, or desktop shell.
  -h, --help             Show this help text.

Environment:
  SYNTHCHAT_BUNDLES=appimage,deb[,rpm]
  SYNTHCHAT_TARGET=<native-rust-target>
  SYNTHCHAT_SKIP_NPM_INSTALL=1

The script creates no package on non-Linux hosts and never treats a
cross-compiled binary as a native Linux release artifact.
EOF
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

require_command() {
  local command_name="$1"
  command -v "$command_name" >/dev/null 2>&1 \
    || die "required command is not available: $command_name"
}

require_file() {
  local path="$1"
  [[ -f "$path" ]] || die "required file is missing: $path"
}

require_directory() {
  local path="$1"
  [[ -d "$path" ]] || die "required directory is missing: $path"
}

require_version() {
  local label="$1"
  local version="$2"
  local required_major="$3"
  local required_minor="$4"
  local major
  local minor

  if [[ ! "$version" =~ ^([0-9]+)\.([0-9]+)(\.[0-9]+)?([-.].*)?$ ]]; then
    die "could not parse $label version: $version"
  fi
  major="${BASH_REMATCH[1]}"
  minor="${BASH_REMATCH[2]}"
  if (( major < required_major || (major == required_major && minor < required_minor) )); then
    die "$label $required_major.$required_minor or newer is required; found $version"
  fi
}

has_bundle() {
  local expected="$1"
  local bundle
  for bundle in "${bundle_list[@]}"; do
    [[ "$bundle" == "$expected" ]] && return 0
  done
  return 1
}

require_pkg_config() {
  local package_name="$1"
  pkg-config --exists "$package_name" \
    || die "missing Linux development package exposed to pkg-config: $package_name"
}

assert_tree_has_no_forbidden_payload() {
  local tree="$1"
  local label="$2"
  local forbidden

  forbidden="$(find "$tree" -type l -print -quit)"
  [[ -z "$forbidden" ]] || die "$label contains a symlink, which is not allowed in a packaged payload: $forbidden"

  forbidden="$(find "$tree" -type f \\
    \( -iname '*.py' -o -iname '*.pyc' -o -iname '*.pyo' -o -iname '*.db' \\
      -o -iname '*.sqlite' -o -iname '*.sqlite3' -o -iname '.env' -o -iname '.env.*' \) \\
    -print -quit)"
  [[ -z "$forbidden" ]] || die "$label contains forbidden runtime data or Python payload: $forbidden"

  forbidden="$(find "$tree" -type d \\
    \( -iname 'synthchat-data' -o -iname '.hermes' -o -iname 'node_modules' \\
      -o -iname 'src-tauri' -o -iname '.venv' -o -iname 'venv' \\
      -o -iname 'python' -o -iname 'python3' -o -iname '__pycache__' \\
      -o -iname 'mcp_servers' -o -iname 'tts' \\
      -o -iname 'agent' -o -iname 'agents' \) \\
    -print -quit)"
  [[ -z "$forbidden" ]] || die "$label contains a forbidden Agent or user-data directory: $forbidden"
}

temporary_files=()
cleanup() {
  local path
  for path in "${temporary_files[@]}"; do
    [[ -n "$path" ]] && rm -f -- "$path"
  done
}
trap cleanup EXIT

skip_npm_install=false
preflight=false
target="${SYNTHCHAT_TARGET:-}"
bundles="${SYNTHCHAT_BUNDLES:-appimage,deb}"

case "${SYNTHCHAT_SKIP_NPM_INSTALL:-0}" in
  0|'') ;;
  1) skip_npm_install=true ;;
  *) die 'SYNTHCHAT_SKIP_NPM_INSTALL must be 0 or 1' ;;
esac

while (($# > 0)); do
  case "$1" in
    --bundles)
      (($# >= 2)) || die '--bundles requires a value'
      bundles="$2"
      shift 2
      ;;
    --target)
      (($# >= 2)) || die '--target requires a value'
      target="$2"
      shift 2
      ;;
    --skip-npm-install)
      skip_npm_install=true
      shift
      ;;
    --preflight)
      preflight=true
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      die "unknown option: $1"
      ;;
  esac
done

script_directory="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
workspace="$(cd -- "$script_directory/.." && pwd -P)"
frontend="$workspace/frontend"
backend="$workspace/backend"
desktop="$workspace/desktop"
backend_manifest="$backend/Cargo.toml"
desktop_manifest="$desktop/Cargo.toml"
desktop_config="$desktop/tauri.conf.json"
tauri_cli="$frontend/node_modules/.bin/tauri"

[[ "$(uname -s)" == 'Linux' ]] \
  || die 'native Linux packaging must run on a Linux host; this script does not cross-package desktop releases'

for command_name in node npm cargo rustc pkg-config cc file sha256sum find grep awk sed mktemp; do
  require_command "$command_name"
done

require_directory "$frontend"
require_directory "$backend"
require_directory "$desktop"
for required_file in \
  "$frontend/package.json" \
  "$frontend/package-lock.json" \
  "$backend_manifest" \
  "$backend/Cargo.lock" \
  "$desktop_manifest" \
  "$desktop/Cargo.lock" \
  "$desktop_config"; do
  require_file "$required_file"
done

node_version="$(node --version)"
node_version="${node_version#v}"
require_version 'Node.js' "$node_version" 22 0

rust_version_line="$(rustc --version)"
[[ "$rust_version_line" =~ ^rustc[[:space:]]+([0-9]+\.[0-9]+\.[0-9]+) ]] \
  || die "could not parse Rust version: $rust_version_line"
rust_version="${BASH_REMATCH[1]}"
require_version 'Rust' "$rust_version" 1 88

rust_details="$(rustc -vV)"
host_target="$(awk '/^host:/{print $2; exit}' <<< "$rust_details")"
[[ "$host_target" =~ ^[A-Za-z0-9_.-]+$ ]] \
  || die 'rustc did not report a safe host target triple'
target="${target:-$host_target}"
[[ "$target" =~ ^[A-Za-z0-9_.-]+$ ]] \
  || die "unsafe Rust target triple: $target"
[[ "$target" == *linux* ]] \
  || die "target is not a Linux target triple: $target"
[[ "$target" == "$host_target" ]] \
  || die "target $target does not match native Rust host $host_target; build that package on a matching Linux host"
target_list="$(rustc --print target-list)"
if ! grep -Fx -- "$target" <<< "$target_list" >/dev/null; then
  die "rustc does not recognize target triple: $target"
fi
target_libdir="$(rustc --print target-libdir --target "$target")"
[[ -d "$target_libdir" ]] || die "Rust standard library is not installed for target: $target"

IFS=',' read -r -a bundle_list <<< "$bundles"
((${#bundle_list[@]} > 0)) || die 'at least one bundle format is required'
declare -A seen_bundles=()
for bundle in "${bundle_list[@]}"; do
  [[ -n "$bundle" ]] || die 'bundle list contains an empty entry'
  [[ "$bundle" != *[[:space:]]* ]] || die "bundle format must not contain whitespace: $bundle"
  case "$bundle" in
    appimage|deb|rpm) ;;
    *) die "unsupported Linux bundle format: $bundle (expected appimage, deb, or rpm)" ;;
  esac
  [[ -z "${seen_bundles[$bundle]:-}" ]] || die "bundle format was requested more than once: $bundle"
  seen_bundles[$bundle]=1
done

# Tauri/Wry currently requires GTK 3 and one of the WebKitGTK pkg-config names
# shipped by supported Linux distributions. librsvg and appindicator are used by
# the desktop bundle/tooling path as well.
require_pkg_config 'gtk+-3.0'
webkit_package=''
for candidate in webkit2gtk-4.1 webkit2gtk-4.0; do
  if pkg-config --exists "$candidate"; then
    webkit_package="$candidate"
    break
  fi
done
[[ -n "$webkit_package" ]] \
  || die 'missing WebKitGTK development package (expected webkit2gtk-4.1 or webkit2gtk-4.0)'
require_pkg_config 'librsvg-2.0'
if pkg-config --exists 'ayatana-appindicator3-0.1'; then
  appindicator_package='ayatana-appindicator3-0.1'
elif pkg-config --exists 'appindicator3-0.1'; then
  appindicator_package='appindicator3-0.1'
else
  die 'missing appindicator development package (expected ayatana-appindicator3-0.1 or appindicator3-0.1)'
fi

if has_bundle deb; then
  require_command dpkg-deb
fi
if has_bundle rpm; then
  require_command rpmbuild
  require_command rpm
fi
if has_bundle appimage; then
  # Read-only AppImage inspection is mandatory before a package is reported.
  require_command unsquashfs
fi

# Verify that the declared Tauri payload cannot silently reintroduce runtime
# data, Node dependencies, or the retired Python/Agent tree.
node - "$desktop_config" <<'NODE'
const fs = require('node:fs');

const configPath = process.argv[2];
const config = JSON.parse(fs.readFileSync(configPath, 'utf8'));
const bundle = config.bundle ?? {};
const externalBin = bundle.externalBin;
if (
  !Array.isArray(externalBin)
  || externalBin.length !== 1
  || externalBin[0] !== 'binaries/synthchat-hermes-backend'
) {
  throw new Error('Tauri bundle.externalBin must contain only binaries/synthchat-hermes-backend');
}
if (bundle.resources !== undefined) {
  throw new Error('Tauri bundle.resources is not allowed until each additional payload has a reviewed package boundary');
}
const forbidden = /(^|[\\/])(synthchat-data|\.hermes|node_modules|src-tauri|\.venv|venv|python[0-9.]*|__pycache__|mcp_servers|tts|agent|agents)([\\/]|$)|(^|[\\/])\.env(?:[\\/]|$)/iu;
if (forbidden.test(JSON.stringify(bundle))) {
  throw new Error('Tauri bundle configuration references a forbidden Agent, Python, dependency, or user-data payload');
}
NODE

backend_target_directory="$(
  cargo metadata --locked --no-deps --format-version 1 --manifest-path "$backend_manifest" \
    | node -e 'let input=""; process.stdin.setEncoding("utf8"); process.stdin.on("data", chunk => input += chunk); process.stdin.on("end", () => { const value = JSON.parse(input).target_directory; if (typeof value !== "string" || value.length === 0) process.exit(1); process.stdout.write(value); });'
)"
desktop_target_directory="$(
  cargo metadata --locked --no-deps --format-version 1 --manifest-path "$desktop_manifest" \
    | node -e 'let input=""; process.stdin.setEncoding("utf8"); process.stdin.on("data", chunk => input += chunk); process.stdin.on("end", () => { const value = JSON.parse(input).target_directory; if (typeof value !== "string" || value.length === 0) process.exit(1); process.stdout.write(value); });'
)"
[[ "$backend_target_directory" == /* ]] \
  || die "Cargo reported a non-absolute backend target directory: $backend_target_directory"
[[ "$desktop_target_directory" == /* ]] \
  || die "Cargo reported a non-absolute desktop target directory: $desktop_target_directory"

printf 'Linux native package preflight\n'
printf '  workspace: %s\n' "$workspace"
printf '  target:    %s\n' "$target"
printf '  backend target dir: %s\n' "$backend_target_directory"
printf '  desktop target dir: %s\n' "$desktop_target_directory"
printf '  bundles:   %s\n' "$(IFS=,; printf '%s' "${bundle_list[*]}")"
printf '  WebKit:    %s (%s)\n' "$webkit_package" "$(pkg-config --modversion "$webkit_package")"
printf '  indicator: %s\n' "$appindicator_package"

if [[ "$skip_npm_install" == false ]]; then
  printf '%s\n' 'Installing locked frontend dependencies with npm ci...'
  npm ci --prefix "$frontend"
fi

[[ -x "$tauri_cli" ]] \
  || die "Tauri CLI is missing: $tauri_cli (run npm ci --prefix frontend or omit --skip-npm-install)"

if [[ "$preflight" == true ]]; then
  cargo metadata --locked --no-deps --format-version 1 --manifest-path "$backend_manifest" >/dev/null
  cargo metadata --locked --no-deps --format-version 1 --manifest-path "$desktop_manifest" >/dev/null
  printf '%s\n' 'Preflight passed. No frontend, backend, or desktop package was built.'
  exit 0
fi

printf '%s\n' 'Building locked frontend assets...'
npm --prefix "$frontend" run build
[[ -s "$frontend/dist/index.html" ]] \
  || die "frontend build did not create $frontend/dist/index.html"
assert_tree_has_no_forbidden_payload "$frontend/dist" 'frontend distribution'

printf '%s\n' 'Building locked Rust backend sidecar...'
export CARGO_INCREMENTAL=0
cargo build --locked --manifest-path "$backend_manifest" --release --target "$target"
backend_binary="$backend_target_directory/$target/release/synthchat-hermes-backend"
[[ -f "$backend_binary" ]] \
  || die "backend build did not create expected sidecar: $backend_binary"
[[ -x "$backend_binary" ]] \
  || die "backend sidecar is not executable: $backend_binary"
backend_file_type="$(file --brief "$backend_binary")"
if [[ "$backend_file_type" != *ELF* ]]; then
  die "backend sidecar is not an ELF executable: $backend_binary"
fi

desktop_real="$(cd -- "$desktop" && pwd -P)"
sidecar_directory="$desktop/binaries"
[[ ! -L "$sidecar_directory" ]] \
  || die "refusing to write the sidecar through a symlink: $sidecar_directory"
mkdir -p -- "$sidecar_directory"
sidecar_directory_real="$(cd -- "$sidecar_directory" && pwd -P)"
case "$sidecar_directory_real" in
  "$desktop_real"/*) ;;
  *) die "sidecar directory escaped the desktop workspace: $sidecar_directory_real" ;;
esac

sidecar="$sidecar_directory/synthchat-hermes-backend-$target"
sidecar_staging="$(mktemp "$sidecar_directory/.synthchat-hermes-backend-$target.XXXXXX")"
temporary_files+=("$sidecar_staging")
cp -- "$backend_binary" "$sidecar_staging"
chmod 0755 -- "$sidecar_staging"
mv -f -- "$sidecar_staging" "$sidecar"
backend_hash="$(sha256sum -- "$backend_binary" | awk '{print $1}')"
sidecar_hash="$(sha256sum -- "$sidecar" | awk '{print $1}')"
[[ "$backend_hash" == "$sidecar_hash" ]] \
  || die 'backend sidecar checksum changed while copying it into the desktop payload'

# A marker makes the post-build artifact list specific to this invocation even
# when a developer has old bundles in the configured Cargo target directory.
mkdir -p -- "$desktop_target_directory"
build_marker="$(mktemp "$desktop_target_directory/.synthchat-linux-package-start.XXXXXX")"
temporary_files+=("$build_marker")

printf '%s\n' 'Building and packaging the native Tauri desktop shell...'
printf '%s\n' 'The Tauri beforeBuildCommand is disabled for this invocation because the frontend and locked sidecar were prepared above.'
tauri_config_override='{"build":{"beforeBuildCommand":""}}'
tauri_args=(build --target "$target" --bundles)
tauri_args+=("${bundle_list[@]}")
tauri_args+=(--config "$tauri_config_override" -- --locked)
(
  cd -- "$desktop"
  TAURI_ENV_TARGET_TRIPLE="$target" "$tauri_cli" "${tauri_args[@]}"
)

bundle_root="$desktop_target_directory/$target/release/bundle"
[[ -d "$bundle_root" ]] \
  || die "Tauri completed without the expected bundle directory: $bundle_root"

declare -a artifacts=()
declare -A artifact_formats=()
declare -A found_bundles=()
for bundle in "${bundle_list[@]}"; do
  found_bundles[$bundle]=0
done

while IFS= read -r -d '' artifact; do
  lower_artifact="${artifact,,}"
  artifact_format=''
  case "$lower_artifact" in
    *.appimage) artifact_format='appimage' ;;
    *.deb) artifact_format='deb' ;;
    *.rpm) artifact_format='rpm' ;;
    *) continue ;;
  esac
  [[ -n "${found_bundles[$artifact_format]:-}" ]] || continue
  artifacts+=("$artifact")
  artifact_formats[$artifact]="$artifact_format"
  found_bundles[$artifact_format]=1
done < <(find "$bundle_root" -type f -newer "$build_marker" -print0)

for bundle in "${bundle_list[@]}"; do
  [[ "${found_bundles[$bundle]}" == 1 ]] \
    || die "Tauri did not produce a newly built .$bundle package under $bundle_root"
done

for artifact in "${artifacts[@]}"; do
  artifact_format="${artifact_formats[$artifact]}"
  listing="$(mktemp "${TMPDIR:-/tmp}/synthchat-linux-package-list.XXXXXX")"
  temporary_files+=("$listing")
  case "$artifact_format" in
    deb)
      dpkg-deb --contents "$artifact" >"$listing"
      ;;
    rpm)
      rpm -qlp -- "$artifact" >"$listing"
      ;;
    appimage)
      unsquashfs -ll "$artifact" >"$listing"
      ;;
  esac

  if grep -E -i \
    '(^|[[:space:]./])(synthchat-data|\.hermes|node_modules|src-tauri|\.venv|venv|python[0-9.]*|__pycache__|mcp_servers|tts|agent|agents)([[:space:]/]|$)|(^|[[:space:]./])\.env([[:space:]/]|$)|\.(py|pyc|pyo|db|sqlite|sqlite3)$' \
    "$listing" >/dev/null; then
    die "package inspection found forbidden Python, Agent, dependency, or user-data payload in $artifact"
  fi
  grep -Fq 'synthchat-hermes-backend' "$listing" \
    || die "package inspection did not find the Rust backend sidecar in $artifact"
done

printf '%s\n' 'Native Linux packages built and inspected:'
for artifact in "${artifacts[@]}"; do
  printf '  %s  %s\n' \
    "$(sha256sum -- "$artifact" | awk '{print $1}')" \
    "$(readlink -f -- "$artifact")"
done
printf 'Bundle root: %s\n' "$bundle_root"
printf '%s\n' 'These are unsigned development artifacts until native install, launch, upgrade, uninstall, and release-signing gates are recorded.'
