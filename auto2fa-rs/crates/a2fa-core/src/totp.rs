//! TOTP generation — mirrors `pyotp.TOTP(secret).now()` and
//! `extract_secret_from_url` in `backend.py`.

use totp_rs::{Algorithm, Secret, TOTP};

use crate::error::{Error, Result};

/// Extract the raw base32 TOTP secret from either an otpauth:// URL or a bare
/// base32 string.
///
/// - `otpauth://totp/…?secret=XXXX&…` → returns `XXXX` (uppercased, trimmed)
/// - bare base32 string → returned as-is (uppercased, spaces stripped)
///
/// Returns `Error::BadParams` if the input is an otpauth URL with no `secret=`
/// query parameter.
pub fn extract_secret(s: &str) -> Result<String> {
    let s = s.trim();
    if s.starts_with("otpauth://") {
        // Parse the query string manually — avoids pulling in a URL crate.
        // Find '?' first, then scan the query parameters.
        let query = s
            .find('?')
            .map(|i| &s[i + 1..])
            .unwrap_or("");
        for pair in query.split('&') {
            let mut parts = pair.splitn(2, '=');
            let key = parts.next().unwrap_or("").to_ascii_lowercase();
            let val = parts.next().unwrap_or("");
            if key == "secret" && !val.is_empty() {
                // Percent-decode is unnecessary for base32 (A-Z, 2-7 only) but
                // strip any trailing noise (unlikely but be safe).
                let secret = val
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect::<String>()
                    .to_ascii_uppercase();
                return Ok(secret);
            }
        }
        Err(Error::BadParams(
            "otpauth URL has no 'secret' query parameter".into(),
        ))
    } else {
        // Treat as bare base32: uppercase and strip spaces.
        let secret = s
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_ascii_uppercase();
        Ok(secret)
    }
}

/// Build a `TOTP` instance from a base32-encoded secret string using the
/// standard TOTP defaults (SHA1, 6 digits, 30-second step) that match pyotp.
fn make_totp(secret: &str) -> Result<TOTP> {
    let secret_bytes = Secret::Encoded(secret.to_ascii_uppercase())
        .to_bytes()
        .map_err(|e| Error::BadParams(format!("invalid base32 secret: {e}")))?;
    // Use TOTP::new_unchecked so short test vectors (like JBSWY3DPEHPK3PXP = 10 bytes)
    // don't fail the ≥128-bit length assertion that TOTP::new enforces.
    Ok(TOTP::new_unchecked(
        Algorithm::SHA1,
        6,
        1,
        30,
        secret_bytes,
    ))
}

/// Generate a 6-digit TOTP code for the current system time.
///
/// `secret` may be an otpauth URL or a bare base32 string — `extract_secret`
/// is called first.
pub fn totp_now(secret: &str) -> Result<String> {
    let bare = extract_secret(secret)?;
    let totp = make_totp(&bare)?;
    totp.generate_current()
        .map_err(|e| Error::Internal(format!("system time error: {e}")))
}

/// Generate the current 6-digit TOTP code along with timing metadata for live
/// display (authenticator-style).
///
/// Returns `(code, period, seconds_remaining)` where:
/// - `period` is the TOTP step in seconds (always 30, matching pyotp).
/// - `seconds_remaining` is `period - (unix_now % period)`, in the range
///   `1..=30` — how many seconds the current `code` stays valid.
///
/// `secret` may be an otpauth URL or a bare base32 string — `extract_secret`
/// is called first (via `make_totp`/`totp_now` semantics).
///
/// READ-ONLY: this computes the current code for display only. It has no side
/// effects — it does not consume, submit, or replay-guard the OTP.
pub fn totp_now_detailed(secret: &str) -> Result<(String, u32, u32)> {
    const PERIOD: u32 = 30;
    let bare = extract_secret(secret)?;
    let totp = make_totp(&bare)?;
    let unix_now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| Error::Internal(format!("system time error: {e}")))?
        .as_secs();
    let code = totp.generate(unix_now);
    let remaining = PERIOD - (unix_now % PERIOD as u64) as u32;
    Ok((code, PERIOD, remaining))
}

/// Generate a 6-digit TOTP code for a specific Unix timestamp (seconds).
///
/// Deterministic — same `unix_secs` always yields the same code for a given
/// secret.
pub fn totp_at(secret: &str, unix_secs: u64) -> Result<String> {
    let bare = extract_secret(secret)?;
    let totp = make_totp(&bare)?;
    Ok(totp.generate(unix_secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_secret() {
        assert_eq!(
            extract_secret(
                "otpauth://totp/Example:alice?secret=JBSWY3DPEHPK3PXP&issuer=Example"
            )
            .unwrap(),
            "JBSWY3DPEHPK3PXP"
        );
        // bare secret (no url) passes through
        assert_eq!(
            extract_secret("JBSWY3DPEHPK3PXP").unwrap(),
            "JBSWY3DPEHPK3PXP"
        );
        assert!(extract_secret("otpauth://totp/x?issuer=y").is_err()); // no secret param
    }

    #[test]
    fn generates_6_digit_code() {
        let c = totp_now("JBSWY3DPEHPK3PXP").unwrap();
        assert_eq!(c.len(), 6);
        assert!(c.chars().all(|ch| ch.is_ascii_digit()));
    }

    #[test]
    fn totp_now_detailed_code_is_6_digits() {
        let (code, _period, _remaining) = totp_now_detailed("JBSWY3DPEHPK3PXP").unwrap();
        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|ch| ch.is_ascii_digit()));
    }

    #[test]
    fn totp_now_detailed_period_and_remaining() {
        let (_code, period, remaining) = totp_now_detailed("JBSWY3DPEHPK3PXP").unwrap();
        assert_eq!(period, 30);
        assert!((1..=30).contains(&remaining), "remaining={remaining}");
    }

    #[test]
    fn totp_now_detailed_accepts_otpauth_url() {
        let (code, period, remaining) = totp_now_detailed(
            "otpauth://totp/Example:alice?secret=JBSWY3DPEHPK3PXP&issuer=Example",
        )
        .unwrap();
        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|ch| ch.is_ascii_digit()));
        assert_eq!(period, 30);
        assert!((1..=30).contains(&remaining));
    }

    #[test]
    fn totp_now_detailed_rejects_url_without_secret() {
        assert!(totp_now_detailed("otpauth://totp/x?issuer=y").is_err());
    }

    #[test]
    fn totp_at_is_deterministic() {
        let a = totp_at("JBSWY3DPEHPK3PXP", 59).unwrap();
        let b = totp_at("JBSWY3DPEHPK3PXP", 59).unwrap();
        assert_eq!(a, b);
    }
}
