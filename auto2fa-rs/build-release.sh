#!/usr/bin/env bash
# build-release.sh — build optimized, signed, (optionally notarized) release
# artifacts for the auto2fa Rust binaries.
#
# Output (dist/):
#   auto2fa-daemon   ← from a2fa-daemon   (signed, identifier com.auto2fa.daemon)
#   auto2fa          ← from a2fa-cli
#   auto2fa-tui      ← from a2fa-tui
#   auto2fa-rs-<arch>.zip  ← zipped binaries for distribution
#
# Build profile: release (see Cargo.toml [profile.release] — LTO + strip +
# codegen-units=1; panic stays "unwind", required by the daemon's catch_unwind
# stability model — do NOT change that).
#
# Signing (lessons from the 2026-06-07 Keychain saga):
#   * The daemon reads SSH passwords / OTP secrets from the macOS Keychain. macOS
#     pins Keychain access to the binary's DESIGNATED REQUIREMENT (identifier +
#     signing cert). We sign the daemon with a PINNED identifier
#     (com.auto2fa.daemon) + a STABLE cert so an "Always Allow" grant persists
#     across rebuilds. An ad-hoc signature (or a filename-derived identifier)
#     changes every build → re-prompt storm.
#   * --options runtime (hardened runtime) is required for notarization.
#
# Distribution / notarization (needs a PAID Apple Developer Program membership →
# a "Developer ID Application" cert; "Apple Development" cannot be notarized):
#   AUTO2FA_NOTARIZE=1                 enable notarization
#   AUTO2FA_NOTARY_PROFILE=<name>     a notarytool keychain profile created via
#                                     `xcrun notarytool store-credentials`
#   The script auto-detects a "Developer ID Application" identity when present
#   and prefers it; otherwise it falls back to "Apple Development" (local use)
#   and skips notarization with a clear note.
#
# Universal binaries: `rustup target add x86_64-apple-darwin` then re-run.

set -euo pipefail
cd "$(dirname "$0")"

DIST="$(pwd)/dist"
DAEMON_IDENTIFIER="com.auto2fa.daemon"
ARM_TARGET="aarch64-apple-darwin"
X86_TARGET="x86_64-apple-darwin"
# Pairs: <cargo binary name>:<dist output name>
BINS="a2fa-daemon:auto2fa-daemon a2fa-cli:auto2fa a2fa-tui:auto2fa-tui"

export PATH="$HOME/.cargo/bin:$PATH"
rm -rf "$DIST"; mkdir -p "$DIST"

# ── Step 1: build (arm64 always; x86_64 if installed) ─────────────────────────
echo "→ cargo build --release (arm64, optimized profile)"
cargo build --release --target "$ARM_TARGET"

HAVE_X86=0
if rustup target list --installed | grep -q "^$X86_TARGET"; then
  echo "→ cargo build --release (x86_64)"
  cargo build --release --target "$X86_TARGET"
  HAVE_X86=1
else
  echo "NOTE: x86_64-apple-darwin not installed — arm64-only."
fi

# ── Step 2: assemble dist/ ────────────────────────────────────────────────────
echo "→ assembling dist/"
for pair in $BINS; do
  cargo_name="${pair%%:*}"; dist_name="${pair##*:}"
  arm_bin="target/$ARM_TARGET/release/$cargo_name"
  if [ "$HAVE_X86" -eq 1 ]; then
    lipo -create -output "$DIST/$dist_name" "$arm_bin" "target/$X86_TARGET/release/$cargo_name"
    echo "  lipo → $dist_name (universal)"
  else
    cp "$arm_bin" "$DIST/$dist_name"; echo "  copy → $dist_name (arm64)"
  fi
  chmod +x "$DIST/$dist_name"
done

# ── Step 3: choose signing identity ───────────────────────────────────────────
# Prefer an explicit override, then a Developer ID (distributable/notarizable),
# then Apple Development (local only), then ad-hoc.
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
if [ -z "$SIGN_ID" ]; then
  echo "NOTE: no signing identity found — ad-hoc (Keychain will re-prompt; not distributable)."
  SIGN_ID="-"
fi
echo "→ signing identity: $SIGN_ID  (developer-id=$IS_DEVELOPER_ID)"

# ── Step 4: code-sign ─────────────────────────────────────────────────────────
# Daemon gets a PINNED identifier (stable Keychain ACL). All get hardened runtime.
for pair in $BINS; do
  dist_name="${pair##*:}"; bin="$DIST/$dist_name"
  extra=( --options runtime --timestamp )
  [ "$SIGN_ID" = "-" ] && extra=()   # ad-hoc: no runtime/timestamp
  if [ "$dist_name" = "auto2fa-daemon" ]; then
    codesign --force --sign "$SIGN_ID" --identifier "$DAEMON_IDENTIFIER" "${extra[@]}" "$bin" \
      && echo "  signed $dist_name (identifier=$DAEMON_IDENTIFIER)" || echo "  WARN: sign $dist_name failed"
  else
    codesign --force --sign "$SIGN_ID" "${extra[@]}" "$bin" \
      && echo "  signed $dist_name" || echo "  WARN: sign $dist_name failed"
  fi
  codesign --verify --strict "$bin" 2>/dev/null && echo "    verify ok" || echo "    WARN: verify failed"
done

# ── Step 5: package ───────────────────────────────────────────────────────────
ARCH_TAG=$([ "$HAVE_X86" -eq 1 ] && echo "universal" || echo "arm64")
ZIP="$DIST/auto2fa-rs-$ARCH_TAG.zip"
( cd "$DIST" && zip -q -j "$ZIP" auto2fa-daemon auto2fa auto2fa-tui )
echo "→ packaged $ZIP"

# ── Step 6: notarize (optional; Developer ID only) ────────────────────────────
if [ "${AUTO2FA_NOTARIZE:-0}" = "1" ]; then
  if [ "$IS_DEVELOPER_ID" -ne 1 ]; then
    echo "SKIP notarize: needs a 'Developer ID Application' cert (paid Apple Developer Program); current identity can't be notarized."
  elif [ -z "${AUTO2FA_NOTARY_PROFILE:-}" ]; then
    echo "SKIP notarize: set AUTO2FA_NOTARY_PROFILE (create via 'xcrun notarytool store-credentials')."
  else
    echo "→ notarizing $ZIP (profile: $AUTO2FA_NOTARY_PROFILE)"
    xcrun notarytool submit "$ZIP" --keychain-profile "$AUTO2FA_NOTARY_PROFILE" --wait
    # Bare binaries can't be stapled (only .app/.dmg/.pkg); the notarization
    # ticket is published online and Gatekeeper checks it on first run.
    echo "  notarized (ticket published; bare binaries are not stapled)."
  fi
fi

# ── Summary ───────────────────────────────────────────────────────────────────
echo ""; echo "dist/:"; ls -lh "$DIST/"
echo ""; echo "Build: $ARCH_TAG | identity: $SIGN_ID | notarize: ${AUTO2FA_NOTARIZE:-0}"
