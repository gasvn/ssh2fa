import Foundation

/// Decides when to offer the post-update "authorize saved credentials" pass.
///
/// # Why this exists
///
/// The background helper is a separate binary, so every SSH2FA update ships a
/// new one. macOS ties a Keychain item's authorization to the code identity that
/// reads it, so after an update the first read of each saved item raises an
/// "Always Allow" prompt — twice per host (password + 2FA secret). Discovered
/// one at a time, that is a confusing drip: a credential view that hangs, a
/// login that stalls, a "User canceled" error with no explanation.
///
/// Measured on a real install: the first read per item took 7-30s, and items
/// whose prompt was dismissed returned "User canceled the operation".
///
/// Doing them all at once, deliberately, with one explanation up front, turns a
/// scattered mystery into a single understood step.
///
/// Foundation-only + injectable inputs, so it unit-tests headlessly.
enum CredentialWarmup {
    /// Whether to offer the warm-up pass.
    ///
    /// - `hostCount`: nothing to authorize with no hosts.
    /// - `currentBuild` / `lastWarmedBuild`: the helper changes with the app
    ///   build, so a build the user already warmed must never re-prompt.
    ///   A nil `lastWarmedBuild` means a fresh install or a pre-feature upgrade.
    ///
    /// Deliberately keyed on the BUILD, not the marketing version: two builds of
    /// the same version are still two distinct binaries to macOS.
    static func shouldOffer(hostCount: Int,
                            currentBuild: String,
                            lastWarmedBuild: String?) -> Bool {
        guard hostCount > 0 else { return false }
        guard !currentBuild.isEmpty else { return false }
        return currentBuild != lastWarmedBuild
    }

    /// Progress label for the pass, e.g. "Authorizing k6 (2 of 6)…".
    static func progressLabel(host: String, index: Int, total: Int) -> String {
        "Authorizing \(host) (\(index + 1) of \(total))…"
    }

    /// Outcome summary. `failed` lists hosts whose prompt was denied or timed
    /// out — they are reported by name, because the fix (reopen and choose
    /// "Always Allow") is per host.
    static func summary(total: Int, failed: [String]) -> String {
        if total == 0 { return "No saved credentials to authorize." }
        if failed.isEmpty {
            return "All \(total) host\(total == 1 ? "" : "s") authorized — macOS won't ask again for this version."
        }
        let names = failed.joined(separator: ", ")
        return "\(total - failed.count) of \(total) authorized. Not authorized: \(names). Open each one's Password & Setup and choose “Always Allow”."
    }
}
