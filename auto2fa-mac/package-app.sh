#!/usr/bin/env bash
# package-app.sh — build a distributable SSH2FA.app + SSH2FA.dmg.
#
# Pipeline:
#   1. build the universal (arm64 + x86_64 if installed) Rust daemon
#   2. xcodebuild the Release app
#   3. embed the daemon in SSH2FA.app/Contents/Resources
#   4. codesign the embedded daemon, then the .app (hardened runtime +
#      entitlements), preferring a Developer ID Application cert
#   5. build dist/SSH2FA.dmg
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
PROJECT_NAME="Auto2FA"                     # .xcodeproj + scheme name (internal codename)
APP_NAME="SSH2FA"                          # product .app / .dmg / volume name
DAEMON_IDENTIFIER="com.auto2fa.daemon"
ENTITLEMENTS="$(pwd)/Auto2FA.entitlements"
ARM_TARGET="aarch64-apple-darwin"
X86_TARGET="x86_64-apple-darwin"

export PATH="$HOME/.cargo/bin:$PATH"
rm -rf "$DIST" "$DD"; mkdir -p "$DIST"

# ── Step 1: universal daemon ──────────────────────────────────────────────────
echo "→ building a2fa-daemon (release)"
( cd "$RS_DIR" && cargo build --release --target "$ARM_TARGET" -p a2fa-daemon )
DAEMON_UNIVERSAL="$DIST/ssh2fa-daemon"
# The app binary is forced universal (ARCHS below), so the embedded daemon MUST be
# universal too or the .app is broken on Intel. Auto-install the x86_64 target if
# missing; hard-fail if that can't be done (set AUTO2FA_ALLOW_ARM64_ONLY=1 to
# deliberately ship an arm64-only build).
if ! rustup target list --installed 2>/dev/null | grep -q "^$X86_TARGET"; then
  echo "  x86_64-apple-darwin target missing — adding it (needed for a universal daemon)"
  rustup target add "$X86_TARGET" || true
fi
if rustup target list --installed 2>/dev/null | grep -q "^$X86_TARGET"; then
  ( cd "$RS_DIR" && cargo build --release --target "$X86_TARGET" -p a2fa-daemon )
  lipo -create -output "$DAEMON_UNIVERSAL" \
    "$RS_DIR/target/$ARM_TARGET/release/ssh2fa-daemon" \
    "$RS_DIR/target/$X86_TARGET/release/ssh2fa-daemon"
  echo "  universal daemon (arm64 + x86_64)"
elif [ "${AUTO2FA_ALLOW_ARM64_ONLY:-0}" = "1" ]; then
  cp "$RS_DIR/target/$ARM_TARGET/release/ssh2fa-daemon" "$DAEMON_UNIVERSAL"
  echo "  WARNING: shipping an ARM64-ONLY daemon (AUTO2FA_ALLOW_ARM64_ONLY=1). The app will NOT work on Intel."
else
  echo "ERROR: cannot build a universal daemon — x86_64-apple-darwin target unavailable." >&2
  echo "       run 'rustup target add x86_64-apple-darwin', or set AUTO2FA_ALLOW_ARM64_ONLY=1 to ship arm64-only." >&2
  exit 1
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
xcodebuild -project "$PROJECT_NAME.xcodeproj" -scheme "$PROJECT_NAME" \
  -configuration Release -derivedDataPath "$DD" \
  ARCHS="x86_64 arm64" ONLY_ACTIVE_ARCH=NO \
  CODE_SIGNING_ALLOWED=NO build >/dev/null
APP="$DD/Build/Products/Release/$APP_NAME.app"
[ -d "$APP" ] || { echo "ERROR: build produced no $APP_NAME.app"; exit 1; }

# Work on a copy in dist/ (leave the build product intact).
STAGE_APP="$DIST/$APP_NAME.app"
cp -R "$APP" "$STAGE_APP"

# ── Step 3: embed the daemon ──────────────────────────────────────────────────
cp "$DAEMON_UNIVERSAL" "$STAGE_APP/Contents/Resources/ssh2fa-daemon"
chmod +x "$STAGE_APP/Contents/Resources/ssh2fa-daemon"
# Record the unsigned daemon code hash inside the bundle. The app uses this —
# not the GUI app's build number — to decide whether the background service
# actually changed. A UI-only release must not restart a healthy daemon and
# create a needless reconnect window.
DAEMON_CODE_HASH="$(shasum -a 256 "$DAEMON_UNIVERSAL" | awk '{print $1}')"
printf '%s\n' "$DAEMON_CODE_HASH" \
  > "$STAGE_APP/Contents/Resources/ssh2fa-daemon.codehash"
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
  # Our self-signed code-signing cert: a STABLE identity (so the daemon's
  # Keychain "Always Allow" survives updates — see docs) with NO revocation risk.
  # Deliberately NOT falling back to an Apple *Development* cert: it's dev-only,
  # Gatekeeper rejects it anyway, and a revoked one makes macOS 26 delete the app
  # on launch. `-v` is omitted because an untrusted self-signed cert isn't listed
  # as "valid", yet codesign signs with it fine by name.
  SIGN_ID="$(security find-identity -p codesigning 2>/dev/null \
              | awk -F'"' '/SSH2FA Code Signing/{print $2; exit}')"
fi
if [ -z "$SIGN_ID" ]; then
  # HARD FAIL rather than silently signing ad-hoc.
  #
  # An ad-hoc signature has a cdhash-based designated requirement, i.e. a NEW
  # code identity for every single build. macOS ties a Keychain item's
  # authorization to the identity of the binary that reads it, so an ad-hoc
  # daemon makes every user re-authorize EVERY saved credential on EVERY
  # release — the "why does it keep asking for my Keychain password" complaint.
  # A stable certificate makes that prompt a one-time event instead.
  #
  # This already happened once (v1.0.0 shipped ad-hoc after a cert was revoked),
  # and it was silent: the script just printed "signing identity: -" and carried
  # on. Releasing must now be a deliberate choice.
  echo "ERROR: no code-signing identity found." >&2
  echo "       Expected a 'Developer ID Application' cert, or the self-signed" >&2
  echo "       'SSH2FA Code Signing' cert in your login keychain." >&2
  echo "" >&2
  echo "       Refusing to sign ad-hoc: ad-hoc gives every build a different code" >&2
  echo "       identity, which forces every user to re-authorize every saved" >&2
  echo "       Keychain item on every update." >&2
  echo "" >&2
  echo "       Set AUTO2FA_ALLOW_ADHOC=1 to override (local testing only —" >&2
  echo "       never for a build you publish)." >&2
  if [ "${AUTO2FA_ALLOW_ADHOC:-0}" != "1" ]; then
    exit 1
  fi
  echo "       AUTO2FA_ALLOW_ADHOC=1 set — continuing with an ad-hoc signature." >&2
  SIGN_ID="-"
fi
echo "→ signing identity: $SIGN_ID  (developer-id=$IS_DEVELOPER_ID)"

# Hardened runtime + secure timestamp + entitlements are ONLY for a real
# Developer ID (a build headed for notarization). Ad-hoc AND the self-signed cert
# are signed plain: hardened runtime on an un-notarized build tends to refuse to
# launch, and a self-signed cert can't satisfy team-scoped entitlements. The
# self-signed cert still gives a STABLE designated requirement (cert-based, not
# cdhash), which is all the Keychain ACL needs to stop re-prompting on updates.
SIGN_EXTRA=()
[ "$IS_DEVELOPER_ID" = "1" ] && SIGN_EXTRA=( --options runtime --timestamp )

# Sign inside-out: the embedded daemon first (pinned identifier → stable
# Keychain ACL), then the app bundle with entitlements.
# `${arr[@]+"${arr[@]}"}` expands to nothing when the array is empty (ad-hoc:
# SIGN_EXTRA=()) instead of tripping `set -u`'s "unbound variable" on bash 3.2.
codesign --force --sign "$SIGN_ID" --identifier "$DAEMON_IDENTIFIER" ${SIGN_EXTRA[@]+"${SIGN_EXTRA[@]}"} \
  "$STAGE_APP/Contents/Resources/ssh2fa-daemon"
echo "  signed embedded daemon"

# The daemon's designated requirement is LOAD-BEARING, not cosmetic.
#
# macOS locks each saved Keychain item to the code allowed to read it. Verified
# against a real Keychain: two different builds sharing one identifier and one
# signing certificate share one designated requirement, so macOS recognises the
# rebuilt daemon and never re-asks. Change either half and every existing user
# is challenged again for every saved credential — and each "Always Allow"
# makes them type their Mac login password.
#
# So: fail the build if the daemon did not come out with a stable, certificate
# based requirement. A cdhash-based one (ad-hoc) means a new identity per build.
DAEMON_DR="$(codesign -d -r- "$STAGE_APP/Contents/Resources/ssh2fa-daemon" 2>&1 | sed -n 's/^designated => //p')"
case "$DAEMON_DR" in
  *"identifier \"$DAEMON_IDENTIFIER\""*"certificate leaf"*)
    echo "  daemon requirement is stable across rebuilds: $DAEMON_DR" ;;
  *)
    echo "ERROR: the daemon's designated requirement is not certificate-based." >&2
    echo "       got: ${DAEMON_DR:-<none>}" >&2
    echo "       Every user would have to re-authorize every saved credential" >&2
    echo "       after this release. Sign with a real certificate." >&2
    [ "${AUTO2FA_ALLOW_ADHOC:-0}" = "1" ] || exit 1
    echo "       AUTO2FA_ALLOW_ADHOC=1 set — continuing anyway." >&2 ;;
esac

APP_SIGN_EXTRA=( ${SIGN_EXTRA[@]+"${SIGN_EXTRA[@]}"} )
[ "$IS_DEVELOPER_ID" = "1" ] && APP_SIGN_EXTRA+=( --entitlements "$ENTITLEMENTS" )
codesign --force --sign "$SIGN_ID" ${APP_SIGN_EXTRA[@]+"${APP_SIGN_EXTRA[@]}"} "$STAGE_APP"
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

# Homebrew cask sha256 — paste into Casks/ssh2fa.rb when cutting a release.
echo ""; echo "DMG sha256 (for Casks/ssh2fa.rb):"
shasum -a 256 "$DMG" | awk '{print "  "$1}'
rm -rf "$DD"
