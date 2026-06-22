import Foundation

/// Normalizes a 2FA secret typed into the Add-Host wizard into a canonical
/// `otpauth://` URL the daemon definitely parses.
///
/// The wizard's field promises "an otpauth:// URL **or just the secret key**",
/// and most authenticator setups show you a bare base32 key (`JBSWY3DPEHPK3PXP`)
/// rather than a full URL. This accepts either form:
/// - a full `otpauth://…` URL or any string already carrying a `secret=` param
///   passes through unchanged;
/// - a bare base32 key is wrapped into `otpauth://totp/<account>?secret=<KEY>`.
///
/// Returns `nil` if the input can't be a TOTP secret. Pure + Foundation-only so
/// it compiles into the headless test bundle.
enum OTPSecret {
    private static let base32 = Set("ABCDEFGHIJKLMNOPQRSTUVWXYZ234567=")

    static func normalize(input: String, account: String) -> String? {
        let raw = input.trimmingCharacters(in: .whitespacesAndNewlines)
        if raw.isEmpty { return nil }
        // Already a URL or carries an explicit secret= param → trust it as-is.
        if raw.lowercased().hasPrefix("otpauth://") || raw.lowercased().contains("secret=") {
            return raw
        }
        // Bare key: base32 alphabet (A-Z, 2-7, optional `=` padding), spaces
        // allowed (authenticator apps group the key), case-insensitive.
        let cleaned = raw.uppercased().replacingOccurrences(of: " ", with: "")
        guard !cleaned.isEmpty, cleaned.allSatisfy({ base32.contains($0) }) else { return nil }
        let acct = account.trimmingCharacters(in: .whitespacesAndNewlines)
        let label = acct.isEmpty ? "ssh2fa" : acct
        return "otpauth://totp/\(label)?secret=\(cleaned)"
    }
}
