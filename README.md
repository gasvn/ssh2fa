# Auto2FA - Robust Multi-Server SSH Manager

Auto2FA is a robust SSH connection manager that handles 2FA (TOTP) automation and maintains persistent connections using SSH ControlMaster.

## Features

- **Multi-Server Dashboard**: Manage all your SSH connections in one place (TUI).
- **Auto-2FA**: Automatically generates and inputs TOTP codes.
- **Instant Login**: Uses SSH ControlMaster to allow instant, password-less logins (`ssh host`).
- **Smart Reliability**: Actively checks connection health and automatically reconnects if the socket dies (sleep/wake protection).
- **Desktop Notifications**: Native alerts for connection status.


## Dependencies (macOS)

For the **Auto-Mount** feature to work, you need `sshfs` and the `fuse-t` library (modern replacement for macFUSE).

### Easy Install (Recommended)
We provide a helper script to automate the installation and fix common linking issues:

```bash
chmod +x install_deps.sh
./install_deps.sh
```

### Manual Install
If you prefer to install manually:
```bash
brew tap macos-fuse-t/homebrew-cask
brew install fuse-t
brew install fuse-t-sshfs
```

## Installation

```bash
git clone <repo>
cd auto2fa_dev
pip install -e .
```

### Configuration
1.  **SSH Config** (`~/.ssh/config`): Ensure your hosts are defined.
2.  **Passwords** (`~/.ssh/passwords.json`):
    ```json
    {
        "my-server": {
            "password": "mySecurePassword123",
            "otpauthUrl": "otpauth://totp/..."
        }
    }
    ```

## Usage

Run the full interactive dashboard:
```bash
auto2fa
```
*   **Space**: Toggle connection.
*   **Q**: Quit.

### Connecting
Once connected (Green Status), simply open a terminal and run:
```bash
ssh my-server
```
You will be logged in instantly.

## Troubleshooting

- **Logs**: Detailed logs are written to `/tmp/auto2fa_dashboard.log`.