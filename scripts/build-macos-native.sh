#!/usr/bin/env bash
set -euo pipefail

workspace="$(cd "$(dirname "$0")/.." && pwd)"
cd "$workspace"

if [[ "${SYNTHCHAT_SKIP_NPM_INSTALL:-0}" != "1" ]]; then
  npm ci --prefix frontend
fi

tauri="$workspace/frontend/node_modules/.bin/tauri"
[[ -x "$tauri" ]] || {
  echo "Tauri CLI is missing: $tauri" >&2
  exit 1
}

cd "$workspace/desktop"
"$tauri" build --bundles app,dmg -- --locked

echo "Desktop bundle ready: $workspace/desktop/target/release/bundle"
