#!/usr/bin/env bash
# One-time setup for SSH2FA's free Ed25519 update-signing key.
# The private key is created directly in the login Keychain and never printed.

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
MAC_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BUILD_DIR="$(mktemp -d "${TMPDIR:-/tmp}/ssh2fa-update-signer.XXXXXX")"
trap 'rm -rf "$BUILD_DIR"' EXIT

xcrun swiftc -O -module-cache-path "$BUILD_DIR/module-cache" \
  "$MAC_DIR/Auto2FA/UpdateSigningCore.swift" \
  "$SCRIPT_DIR/update-signer.swift" \
  -o "$BUILD_DIR/update-signer"

"$BUILD_DIR/update-signer" initialize
