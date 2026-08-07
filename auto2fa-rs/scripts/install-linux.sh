#!/usr/bin/env bash
# Install SSH2FA on Linux for the CURRENT USER — no root, nothing system-wide.
#
#   ./scripts/install-linux.sh            # build (release) + install + enable
#   ./scripts/install-linux.sh --no-build # install already-built binaries
#   ./scripts/install-linux.sh --vault file
#
# Installs:
#   ~/.local/bin/ssh2fa-daemon, a2fa-cli, a2fa-tui
#   ~/.config/systemd/user/ssh2fa-daemon.service   (enabled + started)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="$HOME/.local/bin"
UNIT_DIR="$HOME/.config/systemd/user"
UNIT="ssh2fa-daemon.service"
BUILD=1
VAULT=""

while [ $# -gt 0 ]; do
  case "$1" in
    --no-build) BUILD=0 ;;
    --vault) VAULT="${2:-}"; shift ;;
    -h|--help) sed -n '2,10p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 1 ;;
  esac
  shift
done

command -v systemctl >/dev/null || { echo "systemd is required" >&2; exit 1; }

if [ "$BUILD" = 1 ]; then
  echo "→ building release binaries"
  ( cd "$ROOT" && cargo build --release --workspace )
fi

SRC="$ROOT/target/release"
[ -x "$SRC/ssh2fa-daemon" ] || { echo "no $SRC/ssh2fa-daemon — build first" >&2; exit 1; }

echo "→ installing to $BIN_DIR"
mkdir -p "$BIN_DIR"
# Replace by unlink-then-copy, never by writing in place: overwriting a RUNNING
# binary is ETXTBSY, and truncating one that launchd/systemd may re-exec is how
# you get a half-written daemon. Unlinking leaves the running process on its
# old inode until it is restarted deliberately, below.
for b in ssh2fa-daemon a2fa-cli a2fa-tui; do
  if [ -x "$SRC/$b" ]; then
    rm -f "$BIN_DIR/$b"
    cp "$SRC/$b" "$BIN_DIR/$b"
    echo "   $b"
  fi
done

echo "→ installing the user service"
mkdir -p "$UNIT_DIR"
install -m 0644 "$ROOT/packaging/systemd/$UNIT" "$UNIT_DIR/$UNIT"

if [ -n "$VAULT" ]; then
  mkdir -p "$UNIT_DIR/$UNIT.d"
  cat > "$UNIT_DIR/$UNIT.d/vault.conf" <<EOF
[Service]
Environment=SSH2FA_VAULT=$VAULT
EOF
  echo "   credential store: $VAULT"
fi

systemctl --user daemon-reload
systemctl --user enable "$UNIT" >/dev/null

# Restart deliberately, and SIGKILL first so live ControlMasters survive and the
# new daemon adopts them (see the unit file for why).
if systemctl --user is-active --quiet "$UNIT"; then
  echo "→ restarting (masters preserved)"
  # --kill-whom=main is required: with KillMode=process systemd refuses the
  # auxiliary-process signal and prints
  #   "Failed to send signal SIGKILL to auxiliary processes: Invalid argument"
  # even though the main process was killed — an alarming message for a step
  # that actually worked.
  systemctl --user kill -s SIGKILL --kill-whom=main "$UNIT" 2>/dev/null || true
  sleep 1
fi
systemctl --user restart "$UNIT"

sleep 2
if systemctl --user is-active --quiet "$UNIT"; then
  echo "✓ ssh2fa-daemon is running"
else
  echo "✗ ssh2fa-daemon did not start:" >&2
  systemctl --user status "$UNIT" --no-pager -l | tail -20 >&2
  exit 1
fi

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "note: $BIN_DIR is not on your PATH — add it to use 'a2fa-cli' and 'a2fa-tui'" ;;
esac

echo
echo "Next:"
echo "  a2fa-cli list                   # talk to the daemon"
echo "  a2fa-tui                        # dashboard"
echo "  journalctl --user -u $UNIT -f   # logs"
