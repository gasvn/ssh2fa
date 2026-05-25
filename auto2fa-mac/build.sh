#!/usr/bin/env bash
# One-shot script: regenerate Xcode project, build the .app.
#
# Prerequisites (one-time):
#   - Xcode installed from the App Store
#   - sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
#   - brew install xcodegen   (xcodegen auto-installs if missing here)
#
# Usage:
#   ./build.sh           # debug build, opens DerivedData app
#   ./build.sh release   # release build under ./build/Build/Products/Release
#   ./build.sh run       # build then launch the .app

set -euo pipefail
cd "$(dirname "$0")"

CONFIG="Debug"
RUN_AFTER=0
case "${1:-}" in
  release) CONFIG="Release" ;;
  run)     RUN_AFTER=1 ;;
  "")      ;;
  *) echo "usage: $0 [release|run]"; exit 1 ;;
esac

# 1. Ensure xcodegen
if ! command -v xcodegen >/dev/null 2>&1; then
  echo "→ installing xcodegen via brew (one-time)"
  brew install xcodegen
fi

# 2. Verify Xcode (not just CommandLineTools) is selected
if ! xcrun --sdk macosx --show-sdk-path >/dev/null 2>&1; then
  cat <<'EOM' >&2
ERROR: xcrun cannot find the macOS SDK. You are probably pointing at
CommandLineTools rather than a full Xcode install.

Fix:
  1. Install Xcode from the App Store if you haven't.
  2. sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
  3. Re-run this script.
EOM
  exit 1
fi

# 3. (Re)generate the Xcode project from project.yml
echo "→ xcodegen generate"
xcodegen generate

# 4. Build
echo "→ xcodebuild ($CONFIG)"
xcodebuild \
  -project Auto2FA.xcodeproj \
  -scheme Auto2FA \
  -configuration "$CONFIG" \
  -derivedDataPath build \
  build

APP_PATH="build/Build/Products/$CONFIG/Auto2FA.app"
echo "→ built: $APP_PATH"

if [ $RUN_AFTER -eq 1 ]; then
  echo "→ launching"
  open "$APP_PATH"
fi
