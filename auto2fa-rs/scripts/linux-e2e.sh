#!/usr/bin/env bash
# End-to-end check of the Linux daemon WITHOUT touching a real server.
#
# What it proves, on the machine it runs on:
#   * the daemon starts, binds its socket, and answers IPC;
#   * credentials round-trip through the Linux credential store;
#   * the pty expect loop drives a real login dialogue — password prompt, then
#     "Verification code:" — and submits a TOTP derived from the stored secret;
#   * a host with NO 2FA secret logs in against a server that only asks for a
#     password (the password-only feature);
#   * a host with no secret whose server DOES ask for a code fails with the
#     actionable message instead of hanging.
#
# How it avoids real infrastructure: a fake `ssh` earlier in PATH plays the
# server. It is a real pty dialogue — the daemon cannot tell the difference —
# but nothing leaves the machine and every secret here is a throwaway.
#
# Everything lives in one temp dir, and the daemon runs with an isolated
# socket + config dir + file vault, so the user's real ~/.ssh, ControlMasters
# and system keyring are untouched (server.rs::guard_test_isolation enforces it).
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DAEMON="${DAEMON:-$ROOT/target/debug/ssh2fa-daemon}"
[ -x "$DAEMON" ] || { echo "no daemon at $DAEMON — cargo build first"; exit 1; }

# A SHORT path: a unix socket path over ~104 bytes fails to bind.
WORK="$(mktemp -d /tmp/s2fe2e.XXXX)"
BIN="$WORK/bin"; mkdir -p "$BIN"
export SSH_CONFIG_PATH="$WORK/cfg"; mkdir -p "$SSH_CONFIG_PATH"
export AUTO2FA_SOCK="$WORK/d.sock"
export AUTO2FA_LOCK="$WORK/d.lock"
export SSH2FA_VAULT=file
export SSH2FA_ALLOW_DEVELOPMENT_KEYCHAIN=1   # a cargo-built binary, deliberately
export PATH="$BIN:$PATH"

PASS=0; FAIL=0
ok()   { echo "  PASS  $1"; PASS=$((PASS+1)); }
bad()  { echo "  FAIL  $1"; FAIL=$((FAIL+1)); }
cleanup() {
  [ -n "${DPID:-}" ] && kill -9 "$DPID" 2>/dev/null
  rm -rf "$WORK"
}
trap cleanup EXIT

# ── the fake server ──────────────────────────────────────────────────────────
# Reads the same TOTP secret the daemon was given and checks the code it gets,
# so a WRONG code fails the test rather than passing silently.
cat > "$BIN/fakesshd.py" <<'PY'
import base64, hmac, hashlib, struct, sys, time
def totp(secret, t=None):
    key = base64.b32decode(secret + "=" * (-len(secret) % 8), casefold=True)
    ctr = int((t if t is not None else time.time()) // 30)
    mac = hmac.new(key, struct.pack(">Q", ctr), hashlib.sha1).digest()
    off = mac[-1] & 0x0F
    return f"{(struct.unpack('>I', mac[off:off+4])[0] & 0x7FFFFFFF) % 1000000:06d}"
if __name__ == "__main__":
    print(totp(sys.argv[1]))
PY

# $1 = mode: "2fa" (password + code) or "pwonly" (password only)
make_fake_ssh() {
  cat > "$BIN/ssh" <<EOF
#!/usr/bin/env bash
# Fake ssh: plays a login dialogue on the pty it was given.
MODE="\$(cat "$WORK/mode")"
EXPECT_PW="\$(cat "$WORK/expect_pw")"
EXPECT_SECRET="\$(cat "$WORK/expect_secret" 2>/dev/null || true)"
# The daemon passes the marker command for a test login; a master login gets a
# shell. Either way we print what the expect loop is waiting for.
printf 'Password: '
read -r GOT_PW
if [ "\$GOT_PW" != "\$EXPECT_PW" ]; then printf '\nPermission denied, please try again.\n'; exit 1; fi
if [ "\$MODE" = "2fa" ]; then
  printf '\nVerification code: '
  read -r GOT_CODE
  WANT="\$(python3 "$BIN/fakesshd.py" "\$EXPECT_SECRET")"
  # Accept the neighbouring window too — a code generated a second before a
  # rollover is valid, and rejecting it would make this test flaky, not strict.
  WANT_PREV="\$(python3 -c "import sys,time; sys.path.insert(0,'$BIN'); from fakesshd import totp; print(totp('\$EXPECT_SECRET', time.time()-30))")"
  if [ "\$GOT_CODE" != "\$WANT" ] && [ "\$GOT_CODE" != "\$WANT_PREV" ]; then
    printf '\nPermission denied, please try again.\n'; exit 1
  fi
fi
printf '\n'
# Echo the marker if we were asked to run one (test-login mode), else a prompt.
for a in "\$@"; do case "\$a" in __auto2fa_login_ok__) echo "__auto2fa_login_ok__"; exit 0;; esac; done
printf 'fakehost:~\$ '
sleep 30
EOF
  chmod +x "$BIN/ssh"
  echo "$1" > "$WORK/mode"
}

rpc() { # rpc <method> <json-params>
  python3 - "$AUTO2FA_SOCK" "$1" "$2" <<'PY'
import json, socket, sys
sock, method, params = sys.argv[1], sys.argv[2], sys.argv[3]
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM); s.settimeout(90)
s.connect(sock)
s.sendall(json.dumps({"id": "e2e", "method": method, "params": json.loads(params)}).encode() + b"\n")
buf = b""
while b"\n" not in buf:
    chunk = s.recv(65536)
    if not chunk: break
    buf += chunk
print(json.dumps(json.loads(buf.split(b"\n")[0])))
PY
}

echo "workdir: $WORK"
echo "== starting the isolated daemon =="
"$DAEMON" > "$WORK/daemon.log" 2>&1 &
DPID=$!
for _ in $(seq 1 50); do [ -S "$AUTO2FA_SOCK" ] && break; sleep 0.2; done
if [ ! -S "$AUTO2FA_SOCK" ]; then
  echo "daemon did not bind its socket:"; sed -n 1,30p "$WORK/daemon.log"; exit 1
fi
ok "daemon started and bound $AUTO2FA_SOCK"

R=$(rpc ping '{}')
echo "$R" | grep -q '"ok": *true' && ok "ping" || bad "ping: $R"

# ── 1. a host WITH 2FA ───────────────────────────────────────────────────────
SECRET="JBSWY3DPEHPK3PXP"
echo "testpw-2fa" > "$WORK/expect_pw"
echo "$SECRET"    > "$WORK/expect_secret"
make_fake_ssh 2fa

R=$(rpc host_add "{\"host\":\"e2e2fa\",\"password\":\"testpw-2fa\",\"otpauth_url\":\"otpauth://totp/e2e?secret=$SECRET\",\"auto_connect\":false}")
echo "$R" | grep -q '"error"' && bad "host_add (2fa): $R" || ok "host_add with a 2FA secret"

R=$(rpc host_test_credentials '{"host":"e2e2fa"}')
echo "$R" | grep -q '"ok": *true' \
  && ok "test login WITH 2FA — password + TOTP accepted by the server" \
  || bad "test login with 2FA: $R"

R=$(rpc host_credentials '{"host":"e2e2fa"}')
echo "$R" | grep -q '"has_otp_secret": *true' && ok "credentials report a stored 2FA secret" \
  || bad "has_otp_secret: $R"

R=$(rpc host_totp '{"host":"e2e2fa"}')
echo "$R" | grep -qE '"code": *"[0-9]{6}"' && ok "live TOTP code generated from the stored secret" \
  || bad "host_totp: $R"

# ── 2. a host WITHOUT 2FA (the feature under test) ───────────────────────────
echo "testpw-only" > "$WORK/expect_pw"
: > "$WORK/expect_secret"
make_fake_ssh pwonly

R=$(rpc host_add '{"host":"e2enotfa","password":"testpw-only","otpauth_url":"","auto_connect":false}')
echo "$R" | grep -q '"error"' && bad "host_add (no 2fa): $R" || ok "host_add with an EMPTY 2FA secret"

R=$(rpc host_credentials '{"host":"e2enotfa"}')
echo "$R" | grep -q '"has_otp_secret": *false' && ok "credentials report no 2FA secret" \
  || bad "has_otp_secret should be false: $R"

R=$(rpc host_test_credentials '{"host":"e2enotfa"}')
echo "$R" | grep -q '"ok": *true' \
  && ok "test login WITHOUT 2FA — password-only login succeeds" \
  || bad "password-only test login: $R"

# 2b. the same host against a server that DOES ask for a code: it must fail
#     with the actionable message, not hang and not claim a bad password.
make_fake_ssh 2fa
echo "$SECRET" > "$WORK/expect_secret"
R=$(rpc host_test_credentials '{"host":"e2enotfa"}')
if echo "$R" | grep -q '"ok": *true'; then
  bad "a no-2FA host must NOT pass against a server demanding a code: $R"
elif echo "$R" | grep -qi "without a 2FA secret"; then
  ok "no-2FA host + code-demanding server → actionable 'add the secret' message"
else
  bad "wrong diagnosis for no-2FA vs code-demanding server: $R"
fi

# ── 3. adding a secret later, then removing it again ─────────────────────────
R=$(rpc host_set_credentials "{\"host\":\"e2enotfa\",\"otpauth_url\":\"otpauth://totp/e2e?secret=$SECRET\"}")
echo "$R" | grep -q '"error"' && bad "add a secret later: $R" || ok "2FA secret added to an existing host"

echo "testpw-only" > "$WORK/expect_pw"
R=$(rpc host_test_credentials '{"host":"e2enotfa"}')
echo "$R" | grep -q '"ok": *true' && ok "that host now passes a 2FA login" || bad "after adding a secret: $R"

R=$(rpc host_set_credentials '{"host":"e2enotfa","clear_otp_secret":true}')
echo "$R" | grep -q '"error"' && bad "clear_otp_secret: $R" || ok "2FA secret removed again"
R=$(rpc host_credentials '{"host":"e2enotfa"}')
echo "$R" | grep -q '"has_otp_secret": *false' && ok "host is password-only again" \
  || bad "after clear: $R"

# ── 4. the credential store really is the isolated file vault ────────────────
VAULT="$SSH_CONFIG_PATH/credentials.json"
if [ -f "$VAULT" ]; then
  ok "file vault created at \$SSH_CONFIG_PATH/credentials.json"
  MODE=$(stat -c '%a' "$VAULT" 2>/dev/null || stat -f '%Lp' "$VAULT")
  [ "$MODE" = "600" ] && ok "vault is mode 600" || bad "vault mode is $MODE, expected 600"
  grep -q "testpw-2fa" "$VAULT" && ok "credentials round-tripped through the vault" \
    || bad "vault does not contain the stored password"
else
  bad "no vault at $VAULT"
fi

# ── 5. nothing real was touched ──────────────────────────────────────────────
if ls "$HOME/.ssh/cm-ssh2fa-"* >/dev/null 2>&1; then
  bad "the test daemon created ControlMasters in the real ~/.ssh"
else
  ok "no ControlMasters in the real ~/.ssh"
fi

echo
echo "== $PASS passed, $FAIL failed =="
[ "$FAIL" -eq 0 ]
