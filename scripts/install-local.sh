#!/usr/bin/env bash
# Build this fork of Handy and install it in place of /Applications/Handy.app.
#
# The bundle id stays com.pais.handy, so your settings, history, models and
# learned fixes stay where they are.
#
# Backups: when /Applications/Handy.app is an official release (signed by its
# developer), the script saves it as a zip in ~/Handy-backups before it
# installs the fork. A zip keeps the official signature intact, and macOS does
# not see a zipped app as a second Handy. When /Applications/Handy.app is an
# earlier build of this fork, the script moves it to the Trash, because a
# rebuild makes it again.
#
# Usage:
#   scripts/install-local.sh              build, back up, install
#   scripts/install-local.sh --no-build   install the last build without a rebuild
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
KEY_FILE="${HANDY_SIGNING_KEY:-$HOME/.tauri/handy-fork.key}"
BACKUP_DIR="${HANDY_BACKUP_DIR:-$HOME/Handy-backups}"
APP_NAME="Handy.app"
TARGET="/Applications/$APP_NAME"
BUILT="$ROOT/src-tauri/target/release/bundle/macos/$APP_NAME"

BUILD=1
for arg in "$@"; do
  case "$arg" in
    --no-build) BUILD=0 ;;
    *)
      echo "Unknown option: $arg" >&2
      exit 64
      ;;
  esac
done

if [[ "$BUILD" == 1 ]]; then
  if [[ ! -f "$KEY_FILE" ]]; then
    echo "No updater signing key at $KEY_FILE." >&2
    echo "Make one with: ./node_modules/.bin/tauri signer generate --ci --password \"\" -w $KEY_FILE" >&2
    echo "Then put the contents of $KEY_FILE.pub in src-tauri/tauri.conf.json (plugins.updater.pubkey)." >&2
    exit 1
  fi
  cd "$ROOT"
  bun install --frozen-lockfile
  TAURI_SIGNING_PRIVATE_KEY="$(cat "$KEY_FILE")"
  export TAURI_SIGNING_PRIVATE_KEY
  export TAURI_SIGNING_PRIVATE_KEY_PASSWORD=""
  export CMAKE_POLICY_VERSION_MINIMUM=3.5
  bun run tauri build --bundles app
fi

if [[ ! -d "$BUILT" ]]; then
  echo "There is no build at $BUILT. Run the script without --no-build." >&2
  exit 1
fi

# The official release is signed with a developer team id; a local build of
# this fork has an ad-hoc signature with no team id.
team_id() {
  codesign -dv "$1" 2>&1 | sed -n 's/^TeamIdentifier=//p'
}

app_version() {
  defaults read "$1/Contents/Info.plist" CFBundleShortVersionString 2>/dev/null || echo unknown
}

echo "Quitting Handy..."
osascript -e 'tell application id "com.pais.handy" to quit' >/dev/null 2>&1 || true
for _ in 1 2 3 4 5 6 7 8 9 10; do
  pgrep -xq handy || break
  sleep 1
done
if pgrep -xq handy; then
  echo "Handy is still running. Quit it from the menu bar icon, then run this again." >&2
  exit 1
fi

if [[ -d "$TARGET" ]]; then
  TEAM="$(team_id "$TARGET")"
  if [[ -n "$TEAM" && "$TEAM" != "not set" ]]; then
    mkdir -p "$BACKUP_DIR"
    ZIP="$BACKUP_DIR/Handy-$(app_version "$TARGET")-official-$(date +%Y%m%d-%H%M%S).zip"
    echo "Backing up the official Handy $(app_version "$TARGET") to $ZIP"
    ditto -c -k --sequesterRsrc --keepParent "$TARGET" "$ZIP"
    # Check the zip before the original goes.
    if ! unzip -tq "$ZIP" >/dev/null; then
      echo "The backup zip failed its check. Nothing was changed." >&2
      exit 1
    fi
    rm -rf "$TARGET"
  else
    echo "Moving the earlier fork build to the Trash"
    mv "$TARGET" "$HOME/.Trash/Handy-fork-$(date +%Y%m%d-%H%M%S).app"
  fi
fi

ditto "$BUILT" "$TARGET"
echo "Installed the fork build at $TARGET"

cat <<EOF

Next steps. macOS ties the Accessibility permission to the app's signature,
and this build has a new signature:

  1. Remove Handy's old Accessibility entry:
       tccutil reset Accessibility com.pais.handy
  2. Open the new Handy:
       open "$TARGET"
  3. When macOS asks, allow Handy in
     System Settings > Privacy & Security > Accessibility.

To go back to the official Handy, quit Handy and run:
  rm -rf "$TARGET" && ditto -x -k "<the zip in $BACKUP_DIR>" /Applications
EOF
