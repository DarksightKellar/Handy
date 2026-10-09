#!/usr/bin/env bash
# Build this fork of Handy and install it in place of /Applications/Handy.app.
#
# The bundle id stays com.pais.handy, so your settings, history and learned
# fixes stay where they are.
#
# Usage: scripts/install-local.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
KEY_FILE="${HANDY_SIGNING_KEY:-$HOME/.tauri/handy-fork.key}"
APP_NAME="Handy.app"
TARGET="/Applications/$APP_NAME"

if [[ ! -f "$KEY_FILE" ]]; then
  echo "No updater signing key at $KEY_FILE." >&2
  echo "Make one with: ./node_modules/.bin/tauri signer generate --ci --password \"\" -w $KEY_FILE" >&2
  echo "Then put the contents of $KEY_FILE.pub in src-tauri/tauri.conf.json (plugins.updater.pubkey)." >&2
  exit 1
fi

cd "$ROOT"
bun install --frozen-lockfile

export TAURI_SIGNING_PRIVATE_KEY="$(cat "$KEY_FILE")"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD=""
export CMAKE_POLICY_VERSION_MINIMUM=3.5
bun run tauri build --bundles app

BUILT="$ROOT/src-tauri/target/release/bundle/macos/$APP_NAME"
if [[ ! -d "$BUILT" ]]; then
  echo "The build did not produce $BUILT." >&2
  exit 1
fi

echo "Quitting Handy..."
osascript -e 'tell application id "com.pais.handy" to quit' >/dev/null 2>&1 || true
sleep 2

if [[ -d "$TARGET" ]]; then
  BACKUP="$HOME/.Trash/Handy-before-fork-$(date +%Y%m%d-%H%M%S).app"
  echo "Moving the old app to $BACKUP"
  mv "$TARGET" "$BACKUP"
fi
cp -R "$BUILT" "$TARGET"
echo "Installed $TARGET"

cat <<'EOF'

Next steps (macOS ties the Accessibility permission to the app's signature,
and a local build has a new signature):

  1. Run this command. It removes only Handy's old Accessibility entry:
       tccutil reset Accessibility com.pais.handy
  2. Open Handy:
       open -a Handy
  3. When macOS asks, allow Handy in
     System Settings > Privacy & Security > Accessibility.

Handy needs this permission to paste text and to see the fixes you make.
EOF
