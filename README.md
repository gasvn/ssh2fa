# Auto2FA - Robust Multi-Server SSH Manager

Auto2FA is a robust SSH connection manager that handles 2FA (TOTP) automation and maintains persistent connections using SSH ControlMaster.

## Three frontends, one backend

| Frontend | Best for | How to run |
|---|---|---|
| **Textual TUI** (`auto2fa`) | SSH sessions, headless servers, no GUI | `auto2fa` |
| **Daemon mode** (`auto2fa-daemon`) | Background service consumed by the Mac app | `auto2fa-daemon`, or auto-start is configured by `python3 install.py` |
| **Native macOS app** (Swift) | Daily driver on a Mac — menu bar status + dock window + Dynamic Notch toasts | Open `auto2fa-mac/Auto2FA.xcodeproj` in Xcode, ⌘R |

All three share the same Python backend (`auto2fa/backend.py`, `auto2fa/tunnels.py`). The TUI runs it in-process; the Mac app talks to the daemon over `~/.auto2fa/auto2fa.sock` (line-delimited JSON-RPC). See [`auto2fa-mac/README.md`](auto2fa-mac/README.md) and [`docs/superpowers/specs/2026-05-24-mac-app-design.md`](docs/superpowers/specs/2026-05-24-mac-app-design.md).

## Features

- **Multi-Server Dashboard**: Manage all your SSH connections in one place.
- **Auto-2FA**: Automatically generates and inputs TOTP codes.
- **Instant Login**: Uses SSH ControlMaster to allow instant, password-less logins (`ssh host`).
- **Two-Layer Port Forwards**: Named, persistent SLURM-aware tunnels (login host → compute node) with auto-recovery.
- **Smart Reliability**: Actively checks connection health and automatically reconnects.
- **Native Notifications**: Desktop alerts on connection / disconnection; **Dynamic Notch** toasts on MacBook Pros (Mac app only).
- **Auto-start on Login**: LaunchAgent template included.

## Installation

```bash
git clone <repo>
cd auto2fa
python3 install.py        # creates .venv, installs, generates + loads the daemon
```

`install.py` is idempotent — re-run it any time (after moving the repo, switching
machines, or pulling updates) and it regenerates this machine's deployment
artifacts. No manual path editing.

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

### Step 1: (Optional) config location

By default auto2fa stores `passwords.json` + `tunnels.json` in `~/.ssh`,
resolved per-user at runtime — nothing to configure. Only set
`SSH_CONFIG_PATH` if you keep them elsewhere, either as an exported
environment variable or in a `.env` file in the project root:
```bash
SSH_CONFIG_PATH=/path/to/your/.ssh
```
Do **not** commit a `.env` with an absolute path — it would override the
per-user default on every machine that checked it out.

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

### Two-Layer Port Forwards (Tunnels)

Auto2FA can manage named SSH tunnels that hop through a connected login host
and forward a local port to a SLURM compute node — equivalent to running:

    ssh -J <jump> -L <local>:localhost:<remote> <user>@<compute-node>

but with automatic discovery of running jobs, persistence, and recovery when
nodes disappear.

**Controls (when focus is on the TUNNELS section):**

- **Tab**: Switch focus between HOSTS and TUNNELS
- **T**: Create a new tunnel (prompts for name + local port)
- **Enter**: Pick a compute node from `squeue`
- **Space**: Start/stop the selected tunnel
- **D**: Delete the selected tunnel

**Configuration:** `$SSH_CONFIG_PATH/tunnels.json`

```json
{
  "tunnels": {
    "jupyter": {
      "local_port": 8888,
      "remote_port": 8888,
      "jump_candidates": null,
      "last_node": "holygpu8a11103.rc.fas.harvard.edu",
      "last_user": "shgao",
      "auto_start": true
    }
  }
}
```

- `jump_candidates: null` ⇒ any host in `passwords.json` may be used as jump.
- `auto_start: true` ⇒ try to start on dashboard launch (after a 3s grace).
- `last_node` is updated automatically whenever you pick a node from the picker.

**Behavior:**

- If the active jump host disconnects, the tunnel silently fails over to the
  next connected candidate. Same compute-node target.
- If the compute node disappears from `squeue` (job ended), the tunnel turns
  red/stale. Press **Enter** to pick a new node.
- The local port is validated for availability before the tunnel starts; the
  modal blocks creation if a port is already in use.

Use it from any terminal once the tunnel shows `alive`:

    curl http://localhost:8888    # reaches the compute node's service

## Troubleshooting

- **Logs**: Check `/tmp/auto2fa_dashboard.log` for detailed error messages.
- **Host name mismatch**: Verify that the key in `passwords.json` exactly matches the `Host` in SSH config.