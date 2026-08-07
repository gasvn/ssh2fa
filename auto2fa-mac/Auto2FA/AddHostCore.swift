import Foundation

/// Pure decision logic behind the Add-host wizard.
///
/// Foundation-only so it unit-tests headlessly: the sheet renders what these
/// functions decide.
enum AddHostCore {

    /// What the wizard should do with whatever is in the "2FA secret" field.
    ///
    /// Blank is a legitimate answer, not an incomplete form: plenty of SSH
    /// accounts authenticate with a password alone. Such a host is registered
    /// with an empty secret and logs in normally, because the server never
    /// prints a verification-code prompt.
    enum OTPEntry: Equatable {
        /// Left blank — register a password-only host.
        case none
        /// A usable secret, normalized to the `otpauth://` form the daemon parses.
        case secret(String)
        /// Something was typed, but it can't be a TOTP secret. Distinguishing
        /// this from `.none` is the whole point: a typo must be caught at entry
        /// rather than silently downgrading the host to password-only and
        /// failing at the next login prompt.
        case invalid
    }

    /// Classify the 2FA field. `account` only labels a bare base32 key that is
    /// wrapped into an otpauth URL; it never changes whether the input is valid.
    static func classifyOTP(input: String, account: String) -> OTPEntry {
        let raw = input.trimmingCharacters(in: .whitespacesAndNewlines)
        if raw.isEmpty { return .none }
        guard let normalized = OTPSecret.normalize(input: raw, account: account) else {
            return .invalid
        }
        return .secret(normalized)
    }

    /// The `otpauth_url` to send to the daemon: the normalized secret, or an
    /// empty string for a host with no 2FA (which the daemon accepts as
    /// "password-only" and stores as such).
    ///
    /// `.invalid` never reaches here — the wizard blocks on it first — but it
    /// maps to the empty string rather than trapping, so a future caller can't
    /// smuggle unparseable text into the Keychain.
    static func otpauthPayload(_ entry: OTPEntry) -> String {
        switch entry {
        case .secret(let url): return url
        case .none, .invalid: return ""
        }
    }

    /// Why step 1 can't be submitted (nil = fine).
    ///
    /// `alias` is the host being added — the ssh alias for a guided add, the
    /// typed hostname for an import.
    static func credentialsError(password: String, otpInput: String, alias: String) -> String? {
        if password.isEmpty { return "Password is required." }
        if case .invalid = classifyOTP(input: otpInput, account: alias) {
            return "That doesn't look like a TOTP secret. Paste the otpauth:// URL or the base32 key — or leave it empty if this account has no 2FA."
        }
        return nil
    }

    /// One-line summary of the 2FA field for the confirmation step.
    static func otpSummary(_ entry: OTPEntry) -> String {
        switch entry {
        case .secret: return String(localized: "ready")
        case .none: return String(localized: "none — this host signs in with a password only")
        case .invalid: return String(localized: "(not a valid secret)")
        }
    }
}
