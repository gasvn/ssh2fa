#!/usr/bin/env bash
# package-app.sh — build a distributable Auto2FA.app + Auto2FA.dmg.
#
# Pipeline:
#   1. build the universal (arm64 + x86_64 if installed) Rust daemon
#   2. xcodebuild the Release app
#   3. embed the daemon in Auto2FA.app/Contents/Resources
#   4. codesign the embedded daemon, then the .app (hardened runtime +
#      entitlements), preferring a Developer ID Application cert
#   5. build dist/Auto2FA.dmg
#   6. (optional) notarize + staple the app and the dmg
#
# Identity selection (same policy as auto2fa-rs/build-release.sh):
#   AUTO2FA_SIGN_ID   override the signing identity
#   else auto-detect "Developer ID Application" (distributable/notarizable)
#   else "Apple Development" (LOCAL ONLY — Gatekeeper blocks it elsewhere)
#   else ad-hoc ("-", not distributable)
#
# Notarization (needs a paid Apple Developer Program → Developer ID cert):
#   AUTO2FA_NOTARIZE=1
#   AUTO2FA_NOTARY_PROFILE=<name>   created via `xcrun notarytool store-credentials`
# See docs/RELEASE.md.

set -euo pipefail
cd "$(dirname "$0")"                       # auto2fa-mac/
REPO_ROOT="$(cd .. && pwd)"
RS_DIR="$REPO_ROOT/auto2fa-rs"
DIST="$(pwd)/dist"
DD="$(pwd)/.package_dd"                     # xcode derived data (scratch)
APP_NAME="Auto2FA"
DAEMON_IDENTIFIER="com.auto2fa.daemon"
ENTITLEMENTS="$(pwd)/Auto2FA.entitlements"
ARM_TARGET="aarch64-apple-darwin"
X86_TARGET="x86_64-apple-darwin"

export PATH="$HOME/.cargo/bin:$PATH"
rm -rf "$DIST" "$DD"; mkdir -p "$DIST"

# ── Step 1: universal daemon ──────────────────────────────────────────────────
echo "→ building a2fa-daemon (release)"
( cd "$RS_DIR" && cargo build --release --target "$ARM_TARGET" -p a2fa-daemon )
DAEMON_UNIVERSAL="$DIST/a2fa-daemon"
if rustup target list --installed 2>/dev/null | grep -q "^$X86_TARGET"; then
  ( cd "$RS_DIR" && cargo build --release --target "$X86_TARGET" -p a2fa-daemon )
  lipo -create -output "$DAEMON_UNIVERSAL" \
    "$RS_DIR/target/$ARM_TARGET/release/a2fa-daemon" \
    "$RS_DIR/target/$X86_TARGET/release/a2fa-daemon"
  echo "  universal daemon (arm64 + x86_64)"
else
  cp "$RS_DIR/target/$ARM_TARGET/release/a2fa-daemon" "$DAEMON_UNIVERSAL"
  echo "  NOTE: x86_64-apple-darwin not installed — arm64-only daemon."
  echo "        run 'rustup target add x86_64-apple-darwin' for a universal build."
fi
chmod +x "$DAEMON_UNIVERSAL"

# ── Step 2: build the app ─────────────────────────────────────────────────────
# The .xcodeproj is a generated artifact (gitignored); regenerate it from
# project.yml so a fresh clone builds. Requires XcodeGen (`brew install xcodegen`).
if ! command -v xcodegen >/dev/null 2>&1; then
  echo "ERROR: xcodegen not found. Install it: brew install xcodegen"; exit 1
fi
echo "→ xcodegen generate"
xcodegen generate >/dev/null

echo "→ xcodebuild Release (universal)"
# ARCHS + ONLY_ACTIVE_ARCH=NO so the app binary is universal too (xcodebuild
# defaults to the active arch only). Daemon universality alone isn't enough —
# an arm64-only app won't launch on Intel.
xcodebuild -project "$APP_NAME.xcodeproj" -scheme "$APP_NAME" \
  -configuration Release -derivedDataPath "$DD" \
  ARCHS="x86_64 arm64" ONLY_ACTIVE_ARCH=NO \
  CODE_SIGNING_ALLOWED=NO build >/dev/null
APP="$DD/Build/Products/Release/$APP_NAME.app"
[ -d "$APP" ] || { echo "ERROR: build produced no $APP_NAME.app"; exit 1; }

# Work on a copy in dist/ (leave the build product intact).
STAGE_APP="$DIST/$APP_NAME.app"
cp -R "$APP" "$STAGE_APP"

# ── Step 3: embed the daemon ──────────────────────────────────────────────────
cp "$DAEMON_UNIVERSAL" "$STAGE_APP/Contents/Resources/a2fa-daemon"
chmod +x "$STAGE_APP/Contents/Resources/a2fa-daemon"
echo "→ embedded daemon in $APP_NAME.app/Contents/Resources"

# ── Step 4: choose identity + sign ────────────────────────────────────────────
SIGN_ID="${AUTO2FA_SIGN_ID:-}"
IS_DEVELOPER_ID=0
if [ -z "$SIGN_ID" ]; then
  SIGN_ID="$(security find-identity -v -p codesigning 2>/dev/null \
              | awk -F'"' '/Developer ID Application/{print $2; exit}')"
  [ -n "$SIGN_ID" ] && IS_DEVELOPER_ID=1
fi
if [ -z "$SIGN_ID" ]; then
  SIGN_ID="$(security find-identity -v -p codesigning 2>/dev/null \
              | awk -F'"' '/Apple Development/{print $2; exit}')"
fi
[ -z "$SIGN_ID" ] && SIGN_ID="-"
echo "→ signing identity: $SIGN_ID  (developer-id=$IS_DEVELOPER_ID)"

SIGN_EXTRA=( --options runtime --timestamp )
[ "$SIGN_ID" = "-" ] && SIGN_EXTRA=()   # ad-hoc: no hardened runtime / timestamp

# Sign inside-out: the embedded daemon first (pinned identifier → stable
# Keychain ACL), then the app bundle with entitlements.
codesign --force --sign "$SIGN_ID" --identifier "$DAEMON_IDENTIFIER" "${SIGN_EXTRA[@]}" \
  "$STAGE_APP/Contents/Resources/a2fa-daemon"
echo "  signed embedded daemon"

APP_SIGN_EXTRA=( "${SIGN_EXTRA[@]}" )
[ "$SIGN_ID" != "-" ] && APP_SIGN_EXTRA+=( --entitlements "$ENTITLEMENTS" )
codesign --force --sign "$SIGN_ID" "${APP_SIGN_EXTRA[@]}" "$STAGE_APP"
codesign --verify --strict --deep "$STAGE_APP" 2>/dev/null \
  && echo "  signed + verified $APP_NAME.app" || echo "  WARN: app verify failed"

# ── Step 5: DMG ───────────────────────────────────────────────────────────────
echo "→ building DMG"
DMG_STAGE="$DIST/dmg"; rm -rf "$DMG_STAGE"; mkdir -p "$DMG_STAGE"
cp -R "$STAGE_APP" "$DMG_STAGE/"
ln -s /Applications "$DMG_STAGE/Applications"      # drag-to-install affordance
DMG="$DIST/$APP_NAME.dmg"
hdiutil create -volname "$APP_NAME" -srcfolder "$DMG_STAGE" \
  -ov -format UDZO "$DMG" >/dev/null
rm -rf "$DMG_STAGE"
[ "$SIGN_ID" != "-" ] && codesign --force --sign "$SIGN_ID" "$DMG"
echo "  → $DMG"

# ── Step 6: notarize + staple ─────────────────────────────────────────────────
if [ "${AUTO2FA_NOTARIZE:-0}" = "1" ]; then
  if [ "$IS_DEVELOPER_ID" -ne 1 ]; then
    echo "SKIP notarize: needs a 'Developer ID Application' cert (paid Apple Developer Program)."
    echo "               current identity '$SIGN_ID' can't be notarized — see docs/RELEASE.md."
  elif [ -z "${AUTO2FA_NOTARY_PROFILE:-}" ]; then
    echo "SKIP notarize: set AUTO2FA_NOTARY_PROFILE (xcrun notarytool store-credentials)."
  else
    echo "→ notarizing $DMG (profile: $AUTO2FA_NOTARY_PROFILE)"
    xcrun notarytool submit "$DMG" --keychain-profile "$AUTO2FA_NOTARY_PROFILE" --wait
    echo "→ stapling"
    xcrun stapler staple "$STAGE_APP"
    xcrun stapler staple "$DMG"
    echo "  notarized + stapled."
  fi
else
  echo "NOTE: notarization off (AUTO2FA_NOTARIZE=1 to enable). DMG runs locally;"
  echo "      Gatekeeper will block it on other Macs until notarized."
fi

echo ""; echo "dist/:"; ls -lh "$DIST/" | grep -v '^total'
echo ""; echo "Identity: $SIGN_ID | developer-id: $IS_DEVELOPER_ID | notarize: ${AUTO2FA_NOTARIZE:-0}"
rm -rf "$DD"
