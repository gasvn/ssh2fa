# Security model

Read this before using Auto2FA — it makes a deliberate trade-off you should
understand.

## The trade-off

Auto2FA stores your **SSH password** and your **TOTP / Duo secret** in the
macOS Keychain and **submits the second factor for you** at login. That is the
whole point — it gives you instant, persistent SSH sessions without typing a
code every time.

But it means: **on this Mac, your "second factor" is no longer a second
factor.** Anyone (or anything) that can run code as your macOS user, when your
Keychain is unlocked, can obtain a valid login to your 2FA-protected hosts.
2FA's promise — "even if my password leaks, an attacker still can't log in
without my phone/token" — does not hold against an attacker who already has
your unlocked Mac. You are trading that property for convenience.

If your threat model includes "my laptop is stolen/compromised while logged
in," or your organization's policy forbids storing the OTP seed at rest, **do
not use this.**

## What it does to protect the secrets

- **Keychain at rest.** Password and `otpauth://` secret live in the login
  Keychain (Security.framework), not in plaintext files. They are never written
  to disk by the app/daemon and never logged.
- **Pinned Keychain ACL.** The daemon is signed with a stable identifier so the
  Keychain "Always Allow" grant is scoped to *that* binary, not re-prompted on
  every rebuild and not freely readable by an arbitrary process.
- **Secrets stay off the wire and off argv.** The password and OTP code are
  written to the ssh process over a pty, never passed as command-line arguments
  (which are visible to any local process via `ps`) and never sent over the
  network beyond the ssh login itself. The TOTP code is computed locally.
- **No telemetry.** Nothing is sent anywhere except your own SSH connections
  and (only when you click "Check for Updates") an unauthenticated request to
  the GitHub Releases API.
- **OTP replay safety.** Codes are serialized across hosts that share a secret
  and never resubmitted within a window, so the tool can't get your account
  rate-limited / locked by replaying a code.

## What it does NOT protect against

- A compromised or stolen Mac with an unlocked Keychain (see above).
- A malicious admin on the **remote** host (unchanged from any SSH use).
- Shoulder-surfing of the displayed TOTP chip in the UI (it shows the current
  code, like any authenticator app).

## Recommendations

- Keep your Mac's Keychain locked when away (screen lock locks it on most
  setups) and use FileVault.
- Use a dedicated low-privilege account on the remote host where possible.
- Prefer this for convenience on machines *you physically control*, not on
  shared or unattended hardware.

## Reporting

This is a personal/research tool, not a commercially supported product. If you
find a vulnerability, open an issue at
<https://github.com/gasvn/auto2fa/issues> (omit anything sensitive).
