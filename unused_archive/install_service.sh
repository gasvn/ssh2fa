#!/bin/bash

# Auto2FA Daemon Service Installation Script

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLIST_NAME="com.auto2fa.daemon.plist"
PLIST_PATH="$SCRIPT_DIR/$PLIST_NAME"
LAUNCHD_PATH="$HOME/Library/LaunchAgents/$PLIST_NAME"

echo "🔧 Installing Auto2FA Daemon Service..."

# Check if running on macOS
if [[ "$OSTYPE" != "darwin"* ]]; then
    echo "❌ This script is designed for macOS only"
    exit 1
fi

# Check if Python 3 is available
if ! command -v python3 &> /dev/null; then
    echo "❌ Python 3 is required but not installed"
    exit 1
fi

# Check if required Python packages are installed
echo "📦 Checking Python dependencies..."
python3 -c "import pexpect, pyotp" 2>/dev/null || {
    echo "❌ Required Python packages not found. Please run:"
    echo "   pip3 install pexpect pyotp"
    exit 1
}

# Create LaunchAgents directory if it doesn't exist
mkdir -p "$HOME/Library/LaunchAgents"

# Stop existing service if running
if launchctl list | grep -q "com.auto2fa.daemon"; then
    echo "🛑 Stopping existing service..."
    launchctl unload "$LAUNCHD_PATH" 2>/dev/null || true
fi

# Copy plist file
echo "📋 Installing service configuration..."
cp "$PLIST_PATH" "$LAUNCHD_PATH"

# Update Python path in plist to use Homebrew Python
echo "🔧 Updating Python path to use Homebrew Python..."
sed -i '' 's|/usr/bin/python3|/opt/homebrew/bin/python3|g' "$LAUNCHD_PATH"

# Set proper permissions
chmod 644 "$LAUNCHD_PATH"

# Load and start the service
echo "🚀 Starting Auto2FA daemon service..."
launchctl load "$LAUNCHD_PATH"

# Wait a moment for the service to start
sleep 2

# Check if service is running
if launchctl list | grep -q "com.auto2fa.daemon"; then
    echo "✅ Auto2FA daemon service installed and started successfully!"
    echo ""
    echo "📊 Service Status:"
    launchctl list | grep "com.auto2fa.daemon"
    echo ""
    echo "📝 Logs are available at: /tmp/auto2fa_daemon.log"
    echo "🔧 To stop the service: launchctl unload $LAUNCHD_PATH"
    echo "🔧 To restart the service: launchctl unload $LAUNCHD_PATH && launchctl load $LAUNCHD_PATH"
else
    echo "❌ Failed to start Auto2FA daemon service"
    echo "📝 Check logs at: /tmp/auto2fa_daemon.log"
    exit 1
fi
