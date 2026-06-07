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
    fn totp_at_is_deterministic() {
        let a = totp_at("JBSWY3DPEHPK3PXP", 59).unwrap();
        let b = totp_at("JBSWY3DPEHPK3PXP", 59).unwrap();
        assert_eq!(a, b);
    }
}
