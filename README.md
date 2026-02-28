# Auto2FA - Robust Multi-Server SSH Manager

Auto2FA is a robust SSH connection manager that handles 2FA (TOTP) automation and maintains persistent connections using SSH ControlMaster.

## Features

- **Multi-Server Dashboard**: Manage all your SSH connections in one place (TUI).
- **Auto-2FA**: Automatically generates and inputs TOTP codes.
- **Instant Login**: Uses SSH ControlMaster to allow instant, password-less logins (`ssh host`).
- **Smart Reliability**: Actively checks connection health and automatically reconnects if the socket dies (sleep/wake protection).
- **Desktop Notifications**: Native alerts for connection status.

## Installation

```bash
git clone <repo>
cd auto2fa
pip install -e .
```

### Dependencies (macOS)

For the **Auto-Mount** feature to work, you need `sshfs` and the `fuse-t` library (modern replacement for macFUSE).

**Easy Install (Recommended):**
```bash
chmod +x install_deps.sh
./install_deps.sh
```

**Manual Install:**
```bash
brew tap macos-fuse-t/homebrew-cask
brew install fuse-t
brew install fuse-t-sshfs
```

## Configuration

### Step 1: Create `.env` file

Create a `.env` file in the project root directory:
```bash
SSH_CONFIG_PATH=/path/to/your/.ssh
```
Example: `SSH_CONFIG_PATH=/Users/yourname/.ssh`

### Step 2: Configure SSH Config

Edit `~/.ssh/config` to define your hosts:
```
Host my-server
  HostName login05.rc.fas.harvard.edu
  User yourusername
  Port 22
```
You can change the name of your login node.

### Step 3: Configure passwords.json

Create `$SSH_CONFIG_PATH/passwords.json`:

```json
{
    "my-server": {
        "password": "YourPassword123!",
        "otpauthUrl": "otpauth://totp/{USER}@login.rc.fas.harvard.edu?secret=YOURSECRETKEY"
    }
}
```

**⚠️ Important Notes:**

1. **Host name must match exactly**: The key in `passwords.json` (e.g., `"my-server"`) **must match** the `Host` name in your SSH config file. Otherwise, the connection will fail.

2. **Password format**: Use your actual login password as a plain string.

3. **otpauthUrl format**: Must be a valid TOTP URL with the following structure:
   ```
   otpauth://totp/LABEL?secret=YOURSECRETKEY
   ```
   The `secret` parameter is the Base32-encoded TOTP secret key.

### Step 4: Generate TOTP Secret Key (Harvard RC)

For Harvard Research Computing users:

1. Visit [https://two-factor.rc.fas.harvard.edu/](https://two-factor.rc.fas.harvard.edu/)
2. Login with your credentials
3. Find the secret key below the QR code
4. Construct your `otpauthUrl`:
   ```
   otpauth://totp/yourusername@login.rc.fas.harvard.edu?secret=YOURSECRETKEY
   ```
5. You do not need to revoke the previous key or rescan the QR code

**Example configuration:**
```json
{
    "rcfas_login": {
        "password": "MySecurePassword!",
        "otpauthUrl": "otpauth://totp/myuser@login.rc.fas.harvard.edu?secret=ABCD1234EFGH5678",
        "autoConnect": true
    }
}
```

## Usage

Run the interactive dashboard:
```bash
auto2fa
```

**Controls:**
- **↑/↓**: Navigate between hosts
- **Space**: Toggle connection on/off
- **M**: Mount/Unmount remote filesystem (SSHFS)
- **R**: Manually rotate connection pool
- **Q**: Quit

### Connecting

Once a host shows **Green (Connected)** status, open any terminal and run:
```bash
ssh my-server
```
You will be logged in instantly without password or 2FA prompt.

### File Mounting (SSHFS)

Auto2FA allows you to mount the remote filesystem locally for easy file editing.

1. Select a connected host.
2. Press **M**.
3. The remote files will be mounted at `~/Mounts/<host_name>`.
4. Press **M** again to unmount.

*Requires `sshfs` and `fuse-t` (installed via `install_deps.sh`).*

### Connection Pooling

Auto2FA maintains a pool of 2 active connections per host to ensure zero-downtime performance. The dashboard includes a **Pool** column displaying the active connection index and the total number of alive connections in the pool (e.g., `0/2`).

- **Auto-Rotation**: If the active connection hangs or dies, the system automatically switches to the standby connection.
- **Manual Rotation (R)**: You can force a rotation if you feel the current shell is sluggish.
- **Sleep Recovery**: The pool automatically rebuilds itself after system sleep/wake cycles.

## Troubleshooting

- **Logs**: Check `/tmp/auto2fa_dashboard.log` for detailed error messages.
- **Host name mismatch**: Verify that the key in `passwords.json` exactly matches the `Host` in SSH config.