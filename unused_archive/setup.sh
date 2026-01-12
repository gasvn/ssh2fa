#!/bin/bash

# Auto2FA Setup Script
# Ensures all dependencies are installed and configured

echo "🚀 Auto2FA Setup Script"
echo "========================="

# Check Python version
echo "📋 Checking Python version..."
PYTHON_VERSION=$(python3 --version 2>&1)
echo "Found: $PYTHON_VERSION"

# Check if pip is available
echo "📋 Checking pip availability..."
if command -v pip3 &> /dev/null; then
    echo "✅ pip3 is available"
else
    echo "❌ pip3 not found. Please install pip3 first."
    exit 1
fi

# Install required Python packages
echo "📦 Installing required Python packages..."
pip3 install --user pexpect pyotp rich

# Check if installation was successful
echo "🔍 Verifying package installation..."
python3 -c "import pexpect; print('✅ pexpect installed')" || echo "❌ pexpect installation failed"
python3 -c "import pyotp; print('✅ pyotp installed')" || echo "❌ pyotp installation failed"
python3 -c "import rich; print('✅ rich installed')" || echo "❌ rich installation failed"

# Create log directory if it doesn't exist
echo "📁 Setting up log directory..."
LOG_DIR="/tmp"
if [ -w "$LOG_DIR" ]; then
    echo "✅ Log directory is writable: $LOG_DIR"
else
    echo "❌ Cannot write to log directory: $LOG_DIR"
fi

# Check SSH config
echo "🔧 Checking SSH configuration..."
SSH_CONFIG="$HOME/.ssh/config"
if [ -f "$SSH_CONFIG" ]; then
    echo "✅ SSH config found: $SSH_CONFIG"
else
    echo "⚠️  SSH config not found: $SSH_CONFIG"
    echo "   Please create your SSH config file"
fi

# Check password file
PASSWORD_FILE="$HOME/.ssh/passwords.json"
if [ -f "$PASSWORD_FILE" ]; then
    echo "✅ Password file found: $PASSWORD_FILE"
else
    echo "⚠️  Password file not found: $PASSWORD_FILE"
    echo "   Please create your password configuration file"
    echo "   See config_example.json for format"
fi

echo ""
echo "🎉 Setup complete!"
echo ""
echo "📖 Next steps:"
echo "   1. Configure your SSH hosts in ~/.ssh/config"
echo "   2. Set up passwords.json with your credentials"
echo "   3. Run: python3 auto2fa.py"
echo "   4. Monitor with: python3 monitor.py"
echo ""
echo "📚 See README.md for detailed instructions"