//! TOTP generation — mirrors `pyotp.TOTP(secret).now()` and
//! `extract_secret_from_url` in `backend.py`.

use totp_rs::{Algorithm, Secret, TOTP};

use crate::error::{Error, Result};

/// Default TOTP parameters (RFC 6238 / pyotp / Duo defaults).
const DEFAULT_ALGORITHM: &str = "SHA1";
const DEFAULT_DIGITS: usize = 6;
const DEFAULT_PERIOD: u64 = 30;

/// Percent-decode a query-string value: `%XX` → byte, `+` → space.
///
/// Malformed escapes (`%` not followed by two hex digits) are passed through
/// literally — the downstream base32 decode then fails with a clear error
/// instead of this helper inventing data.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = &s[i + 1..i + 3];
                if let Ok(b) = u8::from_str_radix(hex, 16) {
                    out.push(b);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Canonicalize a raw base32 secret value: percent-decode, strip whitespace,
/// strip trailing `=` padding (the base32 crate decodes pad-less RFC 4648 and
/// REJECTS padded input — pyotp accepted padded secrets, so dropping the pads
/// here is required for parity), uppercase. Errors on an empty result — an
/// empty secret would otherwise build an empty-key HMAC that happily generates
/// garbage codes (burning real Duo attempts) instead of failing loudly.
fn canonicalize_secret(raw: &str) -> Result<String> {
    let s: String = percent_decode(raw)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_uppercase();
    let s = s.trim_end_matches('=').to_string();
    if s.is_empty() {
        return Err(Error::BadParams("empty TOTP secret".into()));
    }
    Ok(s)
}

/// Parsed otpauth parameters in canonical-token form.
///
/// The canonical token is `SECRET` when all parameters are default
/// (SHA1/6/30) and `SECRET#ALGO:digits:period` otherwise. It serves two
/// roles, so it MUST stay a plain deterministic string:
/// 1. input to [`totp_now`]/[`totp_at`] (round-trips through
///    [`extract_secret`], which is idempotent on tokens), and
/// 2. the daemon's OTP **group key** — hosts sharing one Duo secret must map
///    to one group regardless of the URL's label/issuer decoration, so the
///    token deliberately excludes everything except secret + code-affecting
///    parameters.
struct OtpParams {
    secret: String,
    algorithm: Algorithm,
    digits: usize,
    period: u64,
}

impl OtpParams {
    fn to_token(&self) -> String {
        let algo = match self.algorithm {
            Algorithm::SHA1 => "SHA1",
            Algorithm::SHA256 => "SHA256",
            Algorithm::SHA512 => "SHA512",
        };
        if algo == DEFAULT_ALGORITHM
            && self.digits == DEFAULT_DIGITS
            && self.period == DEFAULT_PERIOD
        {
            self.secret.clone()
        } else {
            format!("{}#{}:{}:{}", self.secret, algo, self.digits, self.period)
        }
    }
}

fn parse_algorithm(s: &str) -> Result<Algorithm> {
    match s.to_ascii_uppercase().as_str() {
        "SHA1" => Ok(Algorithm::SHA1),
        "SHA256" => Ok(Algorithm::SHA256),
        "SHA512" => Ok(Algorithm::SHA512),
        other => Err(Error::BadParams(format!(
            "unsupported TOTP algorithm '{other}' (expected SHA1/SHA256/SHA512)"
        ))),
    }
}

/// Parse an otpauth:// URL, a bare base32 secret, or a canonical token into
/// [`OtpParams`]. Unknown algorithms / absurd digits/period are rejected
/// loudly — silently generating wrong codes would burn real 2FA attempts.
fn parse_otp_input(s: &str) -> Result<OtpParams> {
    let s = s.trim();
    if s.starts_with("otpauth://") {
        // Parse the query string manually — avoids pulling in a URL crate.
        let query = s.find('?').map(|i| &s[i + 1..]).unwrap_or("");
        let mut secret: Option<String> = None;
        let mut algorithm = parse_algorithm(DEFAULT_ALGORITHM)?;
        let mut digits = DEFAULT_DIGITS;
        let mut period = DEFAULT_PERIOD;
        for pair in query.split('&') {
            let mut parts = pair.splitn(2, '=');
            let key = parts.next().unwrap_or("").to_ascii_lowercase();
            let val = parts.next().unwrap_or("");
            match key.as_str() {
                "secret" if !val.is_empty() => secret = Some(canonicalize_secret(val)?),
                "algorithm" if !val.is_empty() => {
                    algorithm = parse_algorithm(&percent_decode(val))?;
                }
                "digits" if !val.is_empty() => {
                    digits = val.parse::<usize>().map_err(|_| {
                        Error::BadParams(format!("invalid otpauth digits '{val}'"))
                    })?;
                    if !(6..=8).contains(&digits) {
                        return Err(Error::BadParams(format!(
                            "unsupported otpauth digits {digits} (expected 6-8)"
                        )));
                    }
                }
                "period" if !val.is_empty() => {
                    period = val.parse::<u64>().map_err(|_| {
                        Error::BadParams(format!("invalid otpauth period '{val}'"))
                    })?;
                    if !(5..=300).contains(&period) {
                        return Err(Error::BadParams(format!(
                            "unsupported otpauth period {period}s (expected 5-300)"
                        )));
                    }
                }
                _ => {}
            }
        }
        match secret {
            Some(secret) => Ok(OtpParams {
                secret,
                algorithm,
                digits,
                period,
            }),
            None => Err(Error::BadParams(
                "otpauth URL has no 'secret' query parameter".into(),
            )),
        }
    } else if let Some((sec, params)) = s.split_once('#') {
        // Canonical token with non-default params: SECRET#ALGO:digits:period.
        let mut it = params.split(':');
        let algo = it.next().unwrap_or("");
        let digits_s = it.next().unwrap_or("");
        let period_s = it.next().unwrap_or("");
        let digits = digits_s
            .parse::<usize>()
            .map_err(|_| Error::BadParams(format!("invalid token digits '{digits_s}'")))?;
        let period = period_s
            .parse::<u64>()
            .map_err(|_| Error::BadParams(format!("invalid token period '{period_s}'")))?;
        Ok(OtpParams {
            secret: canonicalize_secret(sec)?,
            algorithm: parse_algorithm(algo)?,
            digits,
            period,
        })
    } else {
        // Bare base32 secret (defaults apply).
        Ok(OtpParams {
            secret: canonicalize_secret(s)?,
            algorithm: parse_algorithm(DEFAULT_ALGORITHM)?,
            digits: DEFAULT_DIGITS,
            period: DEFAULT_PERIOD,
        })
    }
}

/// Extract the canonical TOTP token from either an otpauth:// URL or a bare
/// base32 string.
///
/// - `otpauth://totp/…?secret=XXXX&…` → `XXXX` (percent-decoded, de-padded,
///   uppercased), with `#ALGO:digits:period` appended iff the URL carries
///   non-default code-affecting parameters.
/// - bare base32 string / canonical token → canonicalized and returned.
///
/// The result is what the daemon caches and uses as the OTP group key; it is
/// also a valid input to [`totp_now`]/[`totp_at`] (idempotent round-trip).
///
/// Returns `Error::BadParams` for a missing/empty secret or unsupported
/// algorithm/digits/period.
pub fn extract_secret(s: &str) -> Result<String> {
    Ok(parse_otp_input(s)?.to_token())
}

/// Build a `TOTP` instance honoring the input's algorithm/digits/period
/// (defaults: SHA1, 6 digits, 30-second step — matching pyotp).
fn make_totp(input: &str) -> Result<(TOTP, u64)> {
    let params = parse_otp_input(input)?;
    let secret_bytes = Secret::Encoded(params.secret.clone())
        .to_bytes()
        .map_err(|e| Error::BadParams(format!("invalid base32 secret: {e}")))?;
    if secret_bytes.is_empty() {
        return Err(Error::BadParams("empty TOTP secret".into()));
    }
    // Use TOTP::new_unchecked so short test vectors (like JBSWY3DPEHPK3PXP = 10 bytes)
    // don't fail the ≥128-bit length assertion that TOTP::new enforces.
    Ok((
        TOTP::new_unchecked(
            params.algorithm,
            params.digits,
            1,
            params.period,
            secret_bytes,
        ),
        params.period,
    ))
}

/// Generate a TOTP code for the current system time.
///
/// `secret` may be an otpauth URL, a bare base32 string, or a canonical
/// token — `extract_secret` semantics apply, including non-default
/// algorithm/digits/period.
pub fn totp_now(secret: &str) -> Result<String> {
    let (totp, _) = make_totp(secret)?;
    totp.generate_current()
        .map_err(|e| Error::Internal(format!("system time error: {e}")))
}

/// Generate the current TOTP code along with timing metadata for live
/// display (authenticator-style).
///
/// Returns `(code, period, seconds_remaining)` where:
/// - `period` is the TOTP step in seconds (30 unless the otpauth URL says
///   otherwise).
/// - `seconds_remaining` is `period - (unix_now % period)`, in the range
///   `1..=period` — how many seconds the current `code` stays valid.
///
/// READ-ONLY: this computes the current code for display only. It has no side
/// effects — it does not consume, submit, or replay-guard the OTP.
pub fn totp_now_detailed(secret: &str) -> Result<(String, u32, u32)> {
    let (totp, period) = make_totp(secret)?;
    let unix_now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| Error::Internal(format!("system time error: {e}")))?
        .as_secs();
    let code = totp.generate(unix_now);
    let remaining = (period - (unix_now % period)) as u32;
    Ok((code, period as u32, remaining))
}

/// Generate a TOTP code for a specific Unix timestamp (seconds).
///
/// Deterministic — same `unix_secs` always yields the same code for a given
/// secret.
pub fn totp_at(secret: &str, unix_secs: u64) -> Result<String> {
    let (totp, _) = make_totp(secret)?;
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

    /// Padded (`=`) and percent-encoded (`%3D`) secrets worked under pyotp;
    /// the pad-less base32 crate rejects them — canonicalization must strip
    /// the padding (in both URL and bare form).
    #[test]
    fn extracts_padded_and_percent_encoded_secret() {
        assert_eq!(
            extract_secret("otpauth://totp/x?secret=MFRGGZDF%3D%3D%3D%3D%3D%3D").unwrap(),
            "MFRGGZDF"
        );
        assert_eq!(extract_secret("MFRGGZDF======").unwrap(), "MFRGGZDF");
        // Padded secrets must actually generate (decode succeeds).
        assert_eq!(
            totp_at("MFRGGZDF======", 59).unwrap(),
            totp_at("MFRGGZDF", 59).unwrap()
        );
    }

    /// Group-key invariant: two URLs sharing one secret but differing in
    /// label/issuer must canonicalize to the SAME token.
    #[test]
    fn token_ignores_label_and_issuer() {
        let a = extract_secret("otpauth://totp/Duo:alice?secret=JBSWY3DPEHPK3PXP&issuer=Duo")
            .unwrap();
        let b = extract_secret("otpauth://totp/FASRC:bob?secret=JBSWY3DPEHPK3PXP&issuer=FASRC")
            .unwrap();
        assert_eq!(a, b);
    }

    /// Non-default params are preserved in the token and the token round-trips
    /// through extract_secret unchanged (idempotent — the daemon re-feeds
    /// cached tokens through totp_now → parse_otp_input).
    #[test]
    fn non_default_params_round_trip() {
        let tok = extract_secret(
            "otpauth://totp/x?secret=JBSWY3DPEHPK3PXP&algorithm=SHA256&digits=8&period=60",
        )
        .unwrap();
        assert_eq!(tok, "JBSWY3DPEHPK3PXP#SHA256:8:60");
        assert_eq!(extract_secret(&tok).unwrap(), tok);
    }

    /// RFC 6238 Appendix B test vectors — the URL's algorithm/digits/period
    /// must actually be honored (they were silently ignored before).
    #[test]
    fn rfc6238_vectors_honor_url_params() {
        // SHA1, 8 digits, secret "12345678901234567890" (base32 below), T=59.
        let sha1 = "otpauth://totp/x?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&digits=8";
        assert_eq!(totp_at(sha1, 59).unwrap(), "94287082");
        // SHA256, 8 digits, secret "12345678901234567890123456789012", T=59.
        let sha256 = "otpauth://totp/x?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZA&algorithm=SHA256&digits=8";
        assert_eq!(totp_at(sha256, 59).unwrap(), "46119246");
    }

    /// Unsupported algorithm / absurd digits must fail loudly, not generate
    /// wrong codes.
    #[test]
    fn unsupported_params_rejected() {
        assert!(extract_secret("otpauth://totp/x?secret=JBSWY3DPEHPK3PXP&algorithm=MD5").is_err());
        assert!(extract_secret("otpauth://totp/x?secret=JBSWY3DPEHPK3PXP&digits=12").is_err());
        assert!(extract_secret("otpauth://totp/x?secret=JBSWY3DPEHPK3PXP&period=1").is_err());
    }

    /// An empty secret must error — never degrade to an empty-key HMAC that
    /// "successfully" generates garbage codes (burning real Duo attempts).
    #[test]
    fn empty_secret_rejected() {
        assert!(extract_secret("").is_err());
        assert!(extract_secret("   ").is_err());
        assert!(extract_secret("======").is_err());
        assert!(totp_now("").is_err());
        assert!(totp_at("", 59).is_err());
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

    /// A non-default period must be reflected in the detailed metadata (the
    /// menu-bar countdown would otherwise lie).
    #[test]
    fn totp_now_detailed_honors_period() {
        let (_code, period, remaining) =
            totp_now_detailed("otpauth://totp/x?secret=JBSWY3DPEHPK3PXP&period=60").unwrap();
        assert_eq!(period, 60);
        assert!((1..=60).contains(&remaining), "remaining={remaining}");
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

    #[test]
    fn percent_decode_basics() {
        assert_eq!(percent_decode("MFRG%3D%3D"), "MFRG==");
        assert_eq!(percent_decode("a+b"), "a b");
        // Malformed escape passes through literally.
        assert_eq!(percent_decode("a%zz"), "a%zz");
        assert_eq!(percent_decode("a%2"), "a%2");
    }
}
