#!/bin/bash

# Auto2FA Daemon Complete Setup Script

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

echo "🚀 Auto2FA Daemon Complete Setup"
echo "================================="

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Function to print colored output
print_status() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

print_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $1"
}

print_warning() {
    echo -e "${YELLOW}[WARNING]${NC} $1"
}

print_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

# Check if running on macOS
if [[ "$OSTYPE" != "darwin"* ]]; then
    print_error "This script is designed for macOS only"
    exit 1
fi

print_status "Checking system requirements..."

# Check Python 3
if ! command -v python3 &> /dev/null; then
    print_error "Python 3 is required but not installed"
    print_status "Please install Python 3 from https://python.org"
    exit 1
fi

PYTHON_VERSION=$(python3 --version | cut -d' ' -f2)
print_success "Python 3 found: $PYTHON_VERSION"

# Check pip3
if ! command -v pip3 &> /dev/null; then
    print_error "pip3 is required but not installed"
    exit 1
fi

print_success "pip3 found"

# Install Python dependencies
print_status "Installing Python dependencies..."
# Try different installation methods
if command -v brew &> /dev/null; then
    print_status "Using Homebrew Python..."
    /opt/homebrew/bin/python3 -m pip install --break-system-packages pexpect pyotp || {
        print_warning "Failed to install with Homebrew Python, trying system pip..."
        pip3 install --break-system-packages pexpect pyotp || {
            print_error "Failed to install Python dependencies"
            exit 1
        }
    }
else
    pip3 install --break-system-packages pexpect pyotp || {
        print_error "Failed to install Python dependencies"
        exit 1
    }
fi
print_success "Python dependencies installed"

# Check if SSH config exists
SSH_CONFIG="$HOME/.ssh/config"
if [[ ! -f "$SSH_CONFIG" ]]; then
    print_warning "SSH config not found at $SSH_CONFIG"
    print_status "Creating SSH config from template..."
    mkdir -p "$HOME/.ssh"
    cp ssh_config_template "$SSH_CONFIG"
    print_success "SSH config created from template"
    print_warning "Please edit $SSH_CONFIG to add your server configurations"
else
    print_success "SSH config found at $SSH_CONFIG"
fi

# Check if passwords.json exists
PASSWORDS_FILE="$HOME/.ssh/passwords.json"
if [[ ! -f "$PASSWORDS_FILE" ]]; then
    print_warning "Passwords file not found at $PASSWORDS_FILE"
    print_status "Creating example passwords.json..."
    cat > "$PASSWORDS_FILE" << 'EOF'
{
  "example-server": {
    "password": "your_password_here",
    "otpauthUrl": "otpauth://totp/Example:user@example.com?secret=YOUR_SECRET_HERE&issuer=Example"
  }
}
EOF
    print_success "Example passwords.json created"
    print_warning "Please edit $PASSWORDS_FILE to add your actual credentials"
else
    print_success "Passwords file found at $PASSWORDS_FILE"
fi

# Create control directory
CONTROL_DIR="$HOME/.ssh/auto2fa_control"
mkdir -p "$CONTROL_DIR"
print_success "Control directory created: $CONTROL_DIR"

# Make scripts executable
chmod +x install_service.sh
chmod +x auto2fa_daemon.py
print_success "Scripts made executable"

# Install the daemon service
print_status "Installing daemon service..."
./install_service.sh || {
    print_error "Failed to install daemon service"
    exit 1
}

# Wait for daemon to start
print_status "Waiting for daemon to start..."
sleep 3

# Check if daemon is running
if launchctl list | grep -q "com.auto2fa.daemon"; then
    print_success "Daemon service is running"
else
    print_error "Daemon service failed to start"
    print_status "Check logs: tail -f /tmp/auto2fa_daemon.log"
    exit 1
fi

# Setup VSCode extension
print_status "Setting up VSCode extension..."
cd vscode-extension

# Check if npm is available
if ! command -v npm &> /dev/null; then
    print_warning "npm not found, skipping VSCode extension setup"
    print_status "Please install Node.js and npm to build the VSCode extension"
else
    print_status "Installing VSCode extension dependencies..."
    npm install || {
        print_warning "Failed to install VSCode extension dependencies"
        print_status "You can manually run: cd vscode-extension && npm install && npm run compile"
    }
    
    print_status "Compiling VSCode extension..."
    npm run compile || {
        print_warning "Failed to compile VSCode extension"
        print_status "You can manually run: cd vscode-extension && npm run compile"
    }
    
    # Install vsce and package the extension
    print_status "Installing vsce and packaging extension..."
    npm install -g vsce 2>/dev/null || {
        print_warning "Failed to install vsce globally"
        print_status "You can manually run: npm install -g vsce && vsce package"
    }
    
    if command -v vsce &> /dev/null; then
        print_status "Packaging VSCode extension..."
        vsce package || {
            print_warning "Failed to package VSCode extension"
            print_status "You can manually run: vsce package"
        }
        
        if [[ -f "auto2fa-1.0.0.vsix" ]]; then
            print_success "VSCode extension packaged: auto2fa-1.0.0.vsix"
        else
            print_warning "VSCode extension package not found"
        fi
    else
        print_warning "vsce not available, skipping packaging"
    fi
    
    print_success "VSCode extension setup completed"
fi

cd "$SCRIPT_DIR"

# Final status check
print_status "Performing final status check..."

# Check daemon status
if launchctl list | grep -q "com.auto2fa.daemon"; then
    DAEMON_STATUS="Running"
else
    DAEMON_STATUS="Not Running"
fi

# Check socket file
if [[ -S "/tmp/auto2fa_daemon.sock" ]]; then
    SOCKET_STATUS="Available"
else
    SOCKET_STATUS="Not Available"
fi

# Check log file
if [[ -f "/tmp/auto2fa_daemon.log" ]]; then
    LOG_STATUS="Available"
    LOG_SIZE=$(wc -l < "/tmp/auto2fa_daemon.log")
else
    LOG_STATUS="Not Available"
    LOG_SIZE=0
fi

# Check VSCode extension
if [[ -f "vscode-extension/auto2fa-1.0.0.vsix" ]]; then
    EXTENSION_STATUS="Packaged (auto2fa-1.0.0.vsix)"
else
    EXTENSION_STATUS="Not Packaged"
fi

echo ""
echo "📊 Setup Summary"
echo "================"
echo "Daemon Status: $DAEMON_STATUS"
echo "Socket File: $SOCKET_STATUS"
echo "Log File: $LOG_STATUS ($LOG_SIZE lines)"
echo "VSCode Extension: $EXTENSION_STATUS"
echo ""

if [[ "$DAEMON_STATUS" == "Running" && "$SOCKET_STATUS" == "Available" ]]; then
    print_success "🎉 Auto2FA setup completed successfully!"
    echo ""
    echo "Next steps:"
    echo "1. Edit ~/.ssh/config to add your server configurations"
    echo "2. Edit ~/.ssh/passwords.json to add your credentials"
    echo "3. Install the VSCode extension:"
    echo "   - Open VSCode"
    echo "   - Press Cmd+Shift+P"
    echo "   - Type 'Extensions: Install from VSIX'"
    echo "   - Select 'auto2fa-1.0.0.vsix' file"
    echo "4. Use VSCode Remote-SSH to connect to your servers"
    echo ""
    echo "Useful commands:"
    echo "  View logs: tail -f /tmp/auto2fa_daemon.log"
    echo "  Restart daemon: launchctl unload ~/Library/LaunchAgents/com.auto2fa.daemon.plist && launchctl load ~/Library/LaunchAgents/com.auto2fa.daemon.plist"
    echo "  Stop daemon: launchctl unload ~/Library/LaunchAgents/com.auto2fa.daemon.plist"
else
    print_error "Setup completed with issues"
    echo "Please check the logs and configuration files"
    echo "Log file: /tmp/auto2fa_daemon.log"
fi
