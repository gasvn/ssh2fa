#!/bin/bash
set -e

echo "📦 Installing Auto2FA Dependencies..."

# Check for Homebrew
if ! command -v brew &> /dev/null; then
    echo "❌ Homebrew is required but not found."
    exit 1
fi

echo "🔍 Checking for FUSE-T (Required for SSHFS on macOS)..."

# 1. Uninstall legacy/conflicting packages if present
if brew list --cask macfuse &>/dev/null; then
    echo "⚠️  Removing conflicting 'macfuse'..."
    brew uninstall --cask macfuse
fi

if brew list sshfs &>/dev/null; then
    echo "⚠️  Removing legacy 'sshfs'..."
    brew uninstall sshfs
fi

if brew list gromgit/fuse/sshfs-mac &>/dev/null; then
    echo "⚠️  Removing legacy 'sshfs-mac'..."
    brew uninstall gromgit/fuse/sshfs-mac
fi

# 2. Install FUSE-T (Kext-less FUSE for macOS)
if ! brew list fuse-t &>/dev/null; then
    echo "⬇️  Installing fuse-t (tap: macos-fuse-t/homebrew-cask)..."
    brew tap macos-fuse-t/homebrew-cask
    brew install fuse-t
else
    echo "✅ fuse-t already installed."
fi

# 3. Install SSHFS (FUSE-T version)
if ! brew list fuse-t-sshfs &>/dev/null; then
    echo "⬇️  Installing fuse-t-sshfs..."
    brew install fuse-t-sshfs
else
    echo "✅ fuse-t-sshfs already installed."
fi

# 4. Cleanup Symlinks (Fix for 'Library not loaded' errors)
# Sometimes the binary looks for /usr/local/lib/libfuse.2.dylib even on Apple Silicon
if [ "$(uname -m)" = "arm64" ]; then
    if [ ! -f /usr/local/lib/libfuse.2.dylib ]; then
        echo "🔧 Fixing library links for Apple Silicon..."
        sudo mkdir -p /usr/local/lib
        # FUSE-T usually puts libs in /usr/local/lib automatically, but if not:
        # We rely on fuse-t installation to have handled this, or the user manual intervention.
        # But commonly we need to link from /opt/homebrew/lib/libfuse.2.dylib if it exists there.
        if [ -f /opt/homebrew/lib/libfuse.2.dylib ] && [ ! -L /usr/local/lib/libfuse.2.dylib ]; then
             sudo ln -s /opt/homebrew/lib/libfuse.2.dylib /usr/local/lib/libfuse.2.dylib
             echo "   Linked /opt/homebrew/lib/libfuse.2.dylib -> /usr/local/lib"
        fi
    fi
fi

echo "🎉 Operations complete! You can now use Auto2FA with mounting support."
