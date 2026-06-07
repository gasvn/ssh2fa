#!/usr/bin/env bash
# build-release.sh — compile release binaries and copy to dist/
#
# Output (dist/):
#   auto2fa-daemon   ← from a2fa-daemon
#   auto2fa          ← from a2fa-cli
#   auto2fa-tui      ← from a2fa-tui
#
# Universal binary support:
#   Requires: rustup target add x86_64-apple-darwin
#   If the x86_64 target is not installed, arm64-only binaries are produced
#   and a note is printed. To enable universal builds later, run:
#     rustup target add x86_64-apple-darwin
#   then re-run this script — no other changes needed.

set -euo pipefail
cd "$(dirname "$0")"

DIST="$(pwd)/dist"
mkdir -p "$DIST"

ARM_TARGET="aarch64-apple-darwin"
X86_TARGET="x86_64-apple-darwin"

# Pairs: <cargo binary name>:<dist output name>
BINS="a2fa-daemon:auto2fa-daemon a2fa-cli:auto2fa a2fa-tui:auto2fa-tui"

# ── Step 1: arm64 build (always) ──────────────────────────────────────────────
echo "→ cargo build --release (arm64)"
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --release --target "$ARM_TARGET"

# ── Step 2: x86_64 build (optional) ──────────────────────────────────────────
HAVE_X86=0
if rustup target list --installed | grep -q "^$X86_TARGET"; then
  echo "→ cargo build --release (x86_64)"
  cargo build --release --target "$X86_TARGET"
  HAVE_X86=1
else
  echo "NOTE: x86_64-apple-darwin target not installed — building arm64-only."
  echo "      To enable universal binaries: rustup target add x86_64-apple-darwin"
fi

# ── Step 3: assemble dist/ ────────────────────────────────────────────────────
echo "→ assembling dist/"
for pair in $BINS; do
  cargo_name="${pair%%:*}"
  dist_name="${pair##*:}"
  arm_bin="target/$ARM_TARGET/release/$cargo_name"

  if [ "$HAVE_X86" -eq 1 ]; then
    x86_bin="target/$X86_TARGET/release/$cargo_name"
    echo "  lipo → $dist_name (universal)"
    lipo -create -output "$DIST/$dist_name" "$arm_bin" "$x86_bin"
  else
    echo "  copy → $dist_name (arm64)"
    cp "$arm_bin" "$DIST/$dist_name"
  fi
  chmod +x "$DIST/$dist_name"
done

# ── Summary ───────────────────────────────────────────────────────────────────
echo ""
echo "dist/ contents:"
ls -lh "$DIST/"
echo ""
if [ "$HAVE_X86" -eq 1 ]; then
  echo "Build type: universal (arm64 + x86_64)"
else
  echo "Build type: arm64-only"
  echo "  (run 'rustup target add x86_64-apple-darwin' then re-run for universal)"
fi
