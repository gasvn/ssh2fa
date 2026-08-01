import Foundation

/// Decides when to offer the post-update "authorize saved credentials" pass.
///
/// # Why this exists
///
/// Old SSH2FA builds wrote either one Keychain item per secret or a consolidated
/// item with an unstable access policy.  The current daemon copies those secrets
/// into one fresh item owned by its stable signed identity.  Reading the old
/// items may require authorization one final time; the new item does not require
/// authorization again on later releases.
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
    /// - `consolidated`: the saved secrets already live in the verified stable
    ///   vault. No authorization probe or banner is needed after that.
    static func shouldOffer(hostCount: Int,
                            currentBuild: String,
                            lastWarmedBuild: String?,
                            consolidated: Bool = false) -> Bool {
        guard hostCount > 0 else { return false }
        guard !currentBuild.isEmpty else { return false }
        guard !consolidated else { return false }
        return currentBuild != lastWarmedBuild
    }

    /// Progress label for the pass, e.g. "Authorizing k6 (2 of 6)…".
    static func progressLabel(host: String, index: Int, total: Int) -> String {
        String(localized: "Authorizing \(host) (\(index + 1) of \(total))…")
    }

    /// Outcome summary. `failed` lists old entries whose one-time read was
    /// denied or timed out, so the user knows exactly which migration to retry.
    /// `consolidated` = how many hosts were folded into the single Keychain
    /// item on this run. Reported because it completes the one-time migration.
    static func summary(total: Int, failed: [String], consolidated: Int = 0,
                        consolidationSucceeded: Bool = true) -> String {
        if total == 0 { return String(localized: "No saved credentials to authorize.") }
        if failed.isEmpty {
            // Two keys rather than one with an interpolated "s": a translator
            // needs the whole phrase, and plural rules differ per language.
            let base = total == 1
                ? String(localized: "All 1 host authorized")
                : String(localized: "All \(total) hosts authorized")
            if consolidationSucceeded {
                return String(localized: "\(base), and moved into SSH2FA's stable Keychain vault — macOS won't ask again on future updates.")
            }
            return String(localized: "\(base), but the new Keychain vault could not be verified. Try the migration again before relying on saved logins.")
        }
        let names = failed.joined(separator: ", ")
        return String(localized: "\(total - failed.count) of \(total) read. Could not read: \(names). Try again and allow SSH2FA to read each old Keychain item; this is the final migration pass.")
    }
}
