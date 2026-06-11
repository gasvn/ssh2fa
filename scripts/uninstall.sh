#!/usr/bin/env bash
# uninstall.sh — fully remove SSH2FA from this Mac.
#
# Removes (in order):
#   1. the LaunchAgent (stops the daemon — it tears down its SSH masters on the
#      way out)
#   2. every Keychain credential SSH2FA stored (service "auto2fa")
#   3. ~/.auto2fa (socket, install marker, any legacy daemon copy)
#   4. (only with --purge-config) ~/.ssh/passwords.json + tunnels.json — your
#      saved host metadata + tunnel definitions
#
# It does NOT delete /Applications/SSH2FA.app — drag that to the Trash
# yourself (a running app can't reliably delete its own bundle).
#
# Usage:
#   ./uninstall.sh                # remove app state, KEEP host/tunnel config
#   ./uninstall.sh --purge-config # also remove passwords.json + tunnels.json
#   ./uninstall.sh --yes          # skip the confirmation prompt
set -euo pipefail

PURGE_CONFIG=0
ASSUME_YES=0
for arg in "$@"; do
  case "$arg" in
    --purge-config) PURGE_CONFIG=1 ;;
    --yes|-y)       ASSUME_YES=1 ;;
    *) echo "unknown option: $arg"; exit 2 ;;
  esac
done

LABEL="com.auto2fa.daemon"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
SSH_DIR="${SSH_CONFIG_PATH:-$HOME/.ssh}"
SSH_DIR="${SSH_DIR%/}"

echo "This will remove SSH2FA's daemon, LaunchAgent, and Keychain credentials."
[ "$PURGE_CONFIG" -eq 1 ] && echo "It will ALSO delete $SSH_DIR/passwords.json and tunnels.json."
if [ "$ASSUME_YES" -ne 1 ]; then
  printf "Continue? [y/N] "
  read -r reply
  case "$reply" in [yY]*) ;; *) echo "aborted."; exit 0 ;; esac
fi

# 1. Stop + unload the LaunchAgent (daemon exits, tearing down its masters).
if launchctl print "gui/$(id -u)/$LABEL" >/dev/null 2>&1; then
  launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
  echo "• unloaded LaunchAgent"
fi
if [ -f "$PLIST" ]; then
  rm -f "$PLIST"
  echo "• removed $PLIST"
fi
# Belt-and-braces: SIGTERM any daemon still running so masters close cleanly.
pkill -TERM -x ssh2fa-daemon 2>/dev/null || true

# 2. Delete every Keychain credential under service "auto2fa".
n=0
while security delete-generic-password -s auto2fa >/dev/null 2>&1; do
  n=$((n + 1))
done
echo "• deleted $n Keychain credential(s)"

# 3. Remove ~/.auto2fa (socket, marker, legacy daemon copy).
if [ -d "$HOME/.auto2fa" ]; then
  rm -rf "$HOME/.auto2fa"
  echo "• removed ~/.auto2fa"
fi

# 4. Optionally remove the saved config.
if [ "$PURGE_CONFIG" -eq 1 ]; then
  for f in passwords.json tunnels.json; do
    if [ -f "$SSH_DIR/$f" ]; then rm -f "$SSH_DIR/$f"; echo "• removed $SSH_DIR/$f"; fi
  done
else
  echo "• KEPT $SSH_DIR/passwords.json + tunnels.json (run with --purge-config to delete)"
fi

echo ""
echo "Done. Finally, drag SSH2FA.app to the Trash:"
echo "  open /Applications  # then move SSH2FA.app to the Trash"
