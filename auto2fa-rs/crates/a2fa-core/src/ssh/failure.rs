//! Failure-reason extraction from raw ssh/pexpect output.
//!
//! Mirrors the inline failure classification in `backend.py`'s
//! `_start_master_impl` (the idx-based match block) plus the terse
//! reasons logged there ("Permission denied", "Login incorrect", …).

/// Scan `output` (the accumulated pty transcript) for known SSH failure
/// patterns and return a short, human-readable reason string.
///
/// The returned string is never empty: if no known pattern is found the
/// fallback is the trimmed last non-empty line, or the generic literal
/// `"SSH login failed"`.
///
/// # Pattern priority (first match wins)
/// 1. "Permission denied"  — wrong password or OTP
/// 2. "Login incorrect"    — PAM text variant
/// 3. "Connection timed out" / "Connection refused" / "Network is unreachable"
/// 4. "Could not resolve hostname"
/// 5. "No route to host"
/// 6. "Host key verification failed"
/// 7. "Too many authentication failures"
/// 8. Fallback: last non-empty trimmed line
pub fn failure_reason(output: &str) -> String {
    // Known patterns in priority order — mirrors backend.py's idx-based log
    // messages and the OpenSSH error strings that produce them.
    let patterns: &[(&str, &str)] = &[
        ("Account locked", "Account locked"),
        ("account is locked", "Account locked"),
        ("Account expired", "Account expired"),
        ("Your account has expired", "Account expired"),
        ("Maximum number of sessions", "Server session limit reached"),
        ("open failed: administratively prohibited", "Server refused a new SSH session"),
        ("Session open refused", "Server refused a new SSH session"),
        ("shell request failed", "Server refused a new SSH session"),
        ("mux_client_request_session", "Server refused a new SSH session"),
        ("Connection closed by", "Connection closed by server"),
        ("Connection reset by", "Connection reset by server"),
        ("kex_exchange_identification", "SSH handshake rejected by server"),
        ("no matching key exchange method found", "No compatible SSH key exchange algorithm"),
        ("no matching host key type found", "No compatible SSH host-key algorithm"),
        ("no matching cipher found", "No compatible SSH cipher"),
        ("Unable to negotiate", "SSH algorithm negotiation failed"),
        ("No more authentication methods to try", "No supported authentication method"),
        ("Permission denied", "Permission denied"),
        ("Login incorrect", "Login incorrect"),
        ("Connection timed out", "Connection timed out"),
        ("Connection refused", "Connection refused"),
        ("Network is unreachable", "Network is unreachable"),
        ("Could not resolve hostname", "Could not resolve hostname"),
        ("No route to host", "No route to host"),
        ("Host key verification failed", "Host key verification failed"),
        ("Too many authentication failures", "Too many authentication failures"),
        ("Offending key", "Host key conflict"),
        ("Permission denied (publickey,password)", "Permission denied"),
    ];

    for (needle, reason) in patterns {
        if output.contains(needle) {
            return reason.to_string();
        }
    }

    // Fallback: last non-empty line (might give a useful hint)
    if let Some(last) = output.lines().rfind(|l| !l.trim().is_empty()) {
        let trimmed = last.trim().to_string();
        if !trimmed.is_empty() {
            return trimmed;
        }
    }

    "SSH login failed".to_string()
}

/// Classify both the PTY transcript and OpenSSH's `-E` diagnostic log. Some
/// connection/configuration failures are written only to `-E`, so inspecting
/// the PTY alone can misleadingly produce "(no output)".
pub fn failure_reason_from_sources(pty_output: &str, ssh_log: &str) -> String {
    if ssh_log.trim().is_empty() {
        return failure_reason(pty_output);
    }
    if pty_output.trim().is_empty() || pty_output.trim() == "(no output)" {
        return failure_reason(ssh_log);
    }
    failure_reason(&format!("{pty_output}\n{ssh_log}"))
}

/// Turn a terse SSH/credential failure into a user-facing explanation that
/// says both WHAT failed and WHAT the user can do next.
///
/// Keep this in the core rather than only in the macOS UI: CLI/TUI clients and
/// daemon snapshots must not lose the diagnosis, and the daemon is the only
/// layer that sees the complete PTY transcript.
pub fn actionable_failure(reason: &str) -> String {
    let raw = reason.trim();
    let lc = raw.to_ascii_lowercase();

    if lc.contains("could not start")
        && (lc.contains("login worker")
            || lc.contains("keychain reader")
            || lc.contains("worker thread"))
        || lc.contains("failed to spawn host-start")
    {
        return "SSH2FA could not start a login worker because this Mac refused a new thread. Quit unused apps and restart SSH2FA; restart the Mac if it persists.".into();
    }
    if lc.contains("keychain") || lc.contains("secure storage") {
        return "SSH2FA could not read the saved credentials from macOS Keychain. Unlock the login Keychain, retry, and choose “Allow” so SSH2FA can finish the one-time migration.".into();
    }
    if lc.contains("missing saved password") || lc.contains("password is empty") {
        return "No saved password was found. Open Password & setup for this host and save the current SSH password.".into();
    }
    // A host registered WITHOUT 2FA whose server turned out to ask for a code.
    // Distinct from "the secret is missing/corrupt": nothing is broken, the
    // host simply needs a secret added. Must be tested before the generic
    // missing-secret branch so the more specific wording wins.
    if lc.contains("asked for a verification code")
        && lc.contains("no 2fa secret is saved")
    {
        return "This host was added without a 2FA secret, but the server asked for a verification code. Open Password & setup and add the authenticator QR/secret.".into();
    }
    if lc.contains("missing saved 2fa") || lc.contains("missing saved otp")
        || lc.contains("2fa secret is empty")
    {
        return "No usable 2FA secret was found. Open Password & setup and scan or paste the current authenticator QR/secret.".into();
    }
    if lc.contains("invalid otpauth") || lc.contains("invalid base32")
        || lc.contains("totp") && (lc.contains("invalid") || lc.contains("parse"))
    {
        return "The saved 2FA secret is invalid. Open Password & setup and scan the authenticator QR code again.".into();
    }
    if lc.contains("verification code rejected") || lc.contains("otp rejected")
        || lc.contains("looped back to password")
    {
        return "The server rejected the 2FA code. Check that the saved 2FA secret belongs to this account and that the Mac’s date and time are set automatically, then update it in Password & setup.".into();
    }
    if lc.contains("password rejected") {
        return "The server rejected the password. Open Password & setup and replace the saved password, then use Test login.".into();
    }
    if lc.contains("permission denied") || lc.contains("login incorrect") {
        return "The server rejected the login. The password or 2FA secret may have changed; update them in Password & setup and run Test login. If they are correct, ask the server administrator whether the account is locked.".into();
    }
    if lc.contains("too many authentication failures") {
        return "The server rejected too many SSH keys before password/2FA was tried. Add IdentitiesOnly yes to this host in SSH settings, or remove unused keys from ssh-agent, then retry.".into();
    }
    if lc.contains("account locked") {
        return "The SSH account is locked. Wait for the lockout period or ask the server administrator to unlock it; do not keep retrying credentials.".into();
    }
    if lc.contains("account expired") {
        return "The SSH account has expired. Ask the server administrator to renew the account before retrying.".into();
    }
    if lc.contains("session limit") || lc.contains("refused a new ssh session")
        || lc.contains("administratively prohibited") || lc.contains("maxsessions")
        || lc.contains("mux_client_request_session") || lc.contains("shell request failed")
    {
        return "The SSH transport is alive, but the server refused a new session (often MaxSessions, a PAM limit, or a degraded login node). Close old SSH sessions and retry; if it persists, send this message to the server administrator.".into();
    }
    if lc.contains("could not resolve hostname") || lc.contains("name or service not known")
        || lc.contains("nodename nor servname")
    {
        return "The SSH host name could not be resolved. Check HostName in SSH settings and connect the required VPN/DNS, then retry.".into();
    }
    if lc.contains("no route to host") || lc.contains("network is unreachable") {
        return "There is no network route to the SSH server. Check Wi‑Fi and connect the required VPN, then retry.".into();
    }
    if lc.contains("connection refused") {
        return "The server refused the SSH connection. Check the host address and port; if they are correct, the SSH service is down and the server administrator must restart it.".into();
    }
    if lc.contains("timed out") || lc.contains("timeout") {
        return "The SSH server did not respond before the timeout. Check Wi‑Fi/VPN, HostName and Port; if those are correct, the server or login node is overloaded or offline.".into();
    }
    if lc.contains("connection closed") || lc.contains("connection reset")
        || lc.contains("broken pipe")
    {
        return "The server closed the SSH connection during login. Retry once; if it repeats, the account may be blocked or the login node may be unhealthy—send the daemon log to the server administrator.".into();
    }
    if lc.contains("handshake rejected") || lc.contains("kex_exchange_identification") {
        return "The server rejected the SSH handshake before authentication. It may be rate-limiting connections; wait a few minutes, then contact the server administrator if it continues.".into();
    }
    if lc.contains("algorithm") || lc.contains("unable to negotiate")
        || lc.contains("no matching cipher")
    {
        return "This Mac and the SSH server could not agree on a secure algorithm. Update the server, or ask its administrator for the exact HostKeyAlgorithms/KexAlgorithms setting required.".into();
    }
    if lc.contains("host key verification failed") || lc.contains("host key conflict")
        || lc.contains("remote host identification has changed")
    {
        return "The server identity key changed. Verify the new fingerprint with the server administrator before removing the old known_hosts entry.".into();
    }
    if lc.contains("no supported authentication method") {
        return "The server does not allow password/keyboard-interactive login for this account. Ask the administrator to enable it, or configure the required SSH key.".into();
    }
    if lc.contains("too many open files") || lc.contains("resource temporarily unavailable") {
        return "SSH2FA could not start SSH because this Mac is low on process/file resources. Quit unused apps and restart SSH2FA; restart the Mac if it persists.".into();
    }

    if raw.is_empty() || raw == "(no output)" || lc == "ssh login failed" {
        return "SSH exited before explaining the failure. Check HostName/Port and VPN, then open Troubleshoot → Logs and send the latest host entry to the server administrator.".into();
    }

    // Preserve a bounded, single-line technical reason for uncommon server
    // policies while still giving the user a concrete next step. Never pass an
    // entire PTY transcript through the host snapshot.
    let single_line = raw.lines().last().unwrap_or(raw).trim();
    let concise: String = single_line.chars().take(180).collect();
    format!("SSH reported: {concise}. Check this host’s SSH settings, then open Troubleshoot → Logs if you need to send the exact error to the server administrator.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_permission_denied() {
        assert_eq!(
            failure_reason("debug: ...\nPermission denied (publickey,password).\n"),
            "Permission denied"
        );
    }

    #[test]
    fn generic_when_unknown() {
        assert!(!failure_reason("some unrelated output").is_empty());
    }

    #[test]
    fn login_incorrect() {
        assert_eq!(
            failure_reason("Login incorrect\n"),
            "Login incorrect"
        );
    }

    #[test]
    fn connection_timed_out() {
        assert_eq!(
            failure_reason("ssh: connect to host k6 port 22: Connection timed out"),
            "Connection timed out"
        );
    }

    #[test]
    fn could_not_resolve() {
        assert_eq!(
            failure_reason("ssh: Could not resolve hostname bogus: nodename nor servname provided"),
            "Could not resolve hostname"
        );
    }

    #[test]
    fn fallback_to_last_line() {
        let out = failure_reason("line one\nsome weird error here");
        assert_eq!(out, "some weird error here");
    }

    #[test]
    fn diagnostic_log_recovers_error_missing_from_pty() {
        assert_eq!(
            failure_reason_from_sources(
                "(no output)",
                "ssh: connect to host example port 2222: Connection refused"
            ),
            "Connection refused"
        );
    }

    #[test]
    fn too_many_auth_failures() {
        assert_eq!(
            failure_reason("Received disconnect from ...: Too many authentication failures"),
            "Too many authentication failures"
        );
    }

    #[test]
    fn permission_denied_plain() {
        // The plain "Permission denied" variant (no parenthetical) used by
        // PAM and pexpect idx=3 in backend.py's should_send_otp branch.
        assert_eq!(
            failure_reason("debug1: ...\nPermission denied\nsome more debug"),
            "Permission denied"
        );
    }

    #[test]
    fn actionable_failures_name_a_fix() {
        let password = actionable_failure("Password rejected (server re-prompted)");
        assert!(password.contains("Password & setup"));
        let dns = actionable_failure("Could not resolve hostname");
        assert!(dns.contains("HostName") && dns.contains("VPN"));
        let sessions = actionable_failure("mux_client_request_session: session request failed");
        assert!(sessions.contains("MaxSessions") && sessions.contains("administrator"));
    }

    #[test]
    fn every_supported_failure_class_names_a_repair() {
        let cases: &[(&str, &[&str])] = &[
            ("Could not start the SSH login worker", &["restart SSH2FA"]),
            ("Keychain read timed out", &["Keychain", "one-time migration"]),
            ("Missing saved password", &["Password & setup"]),
            ("Missing saved 2FA secret", &["QR/secret"]),
            (
                "The server asked for a verification code, but no 2FA secret is saved for this host",
                &["added without a 2FA secret", "Password & setup"],
            ),
            ("Invalid otpauth/2FA secret", &["QR code again"]),
            ("Verification code rejected", &["date and time", "2FA secret"]),
            ("Password rejected", &["saved password", "Test login"]),
            ("Too many authentication failures", &["IdentitiesOnly", "ssh-agent"]),
            ("Account locked", &["administrator", "unlock"]),
            ("Account expired", &["administrator", "renew"]),
            ("Server session limit reached", &["Close old SSH sessions", "administrator"]),
            ("Could not resolve hostname", &["HostName", "VPN/DNS"]),
            ("Network is unreachable", &["Wi‑Fi", "VPN"]),
            ("Connection refused", &["port", "SSH service"]),
            ("Connection timed out", &["VPN", "Port"]),
            ("Connection reset by server", &["Retry", "daemon log"]),
            ("SSH handshake rejected by server", &["wait", "administrator"]),
            ("No compatible SSH host-key algorithm", &["Update", "HostKeyAlgorithms"]),
            ("Host key verification failed", &["fingerprint", "known_hosts"]),
            ("No supported authentication method", &["administrator", "SSH key"]),
        ];

        for (reason, required) in cases {
            let message = actionable_failure(reason);
            for term in *required {
                assert!(
                    message.contains(term),
                    "{reason:?} must name repair term {term:?}; got {message:?}"
                );
            }
        }
    }

    #[test]
    fn uncommon_failure_keeps_bounded_exact_reason_and_next_step() {
        let raw = format!("site policy denied login: {}", "x".repeat(400));
        let message = actionable_failure(&raw);
        assert!(message.contains("site policy denied login"));
        assert!(message.contains("Troubleshoot → Logs"));
        assert!(message.len() < 400, "technical transcript must stay bounded");
    }

    #[test]
    fn extracts_session_and_algorithm_failures() {
        assert_eq!(
            failure_reason("mux_client_request_session: session request failed"),
            "Server refused a new SSH session"
        );
        assert_eq!(
            failure_reason("Unable to negotiate with host: no matching host key type found"),
            "No compatible SSH host-key algorithm"
        );
    }
}
