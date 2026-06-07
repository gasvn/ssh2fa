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
    if let Some(last) = output.lines().filter(|l| !l.trim().is_empty()).last() {
        let trimmed = last.trim().to_string();
        if !trimmed.is_empty() {
            return trimmed;
        }
    }

    "SSH login failed".to_string()
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
}
