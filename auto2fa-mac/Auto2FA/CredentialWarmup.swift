import Foundation

/// User-facing state for the one-time secure-storage upgrade.
///
/// Keep implementation terms such as Keychain, helper, daemon, ACL, and vault
/// out of this state. Those details are useful in logs, not in product UI.
enum CredentialUpgradeStatus: Equatable {
    case idle
    case running
    case succeeded(hostCount: Int)
    case failed(message: String)

    var isRunning: Bool {
        if case .running = self { return true }
        return false
    }
}

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
    static let readyMarkerName = "credential-storage-ready-v3"
    private static let consolidatedDefaultsKey = "auto2fa.credentials.v4-migrated"

    /// Bridge users who already completed the stable-store migration before
    /// the daemon learned to gate background credential reads. Called before a
    /// post-update daemon is installed, so the daemon never has to guess and
    /// never races the explanatory UI.
    static func ensureDaemonReadyMarkerIfNeeded() {
        let consolidated = UserDefaults.standard.bool(forKey: consolidatedDefaultsKey)
        let marker = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".ssh2fa", isDirectory: true)
            .appendingPathComponent(readyMarkerName)
        _ = writeReadyMarkerIfNeeded(consolidated: consolidated, markerURL: marker)
    }

    /// Testable filesystem core. The marker carries no secret; permissions are
    /// still owner-only so another local account cannot alter connection policy.
    @discardableResult
    static func writeReadyMarkerIfNeeded(consolidated: Bool, markerURL: URL) -> Bool {
        guard consolidated else { return false }
        do {
            try FileManager.default.createDirectory(
                at: markerURL.deletingLastPathComponent(),
                withIntermediateDirectories: true)
            try Data("ready\n".utf8).write(to: markerURL, options: .atomic)
            try FileManager.default.setAttributes(
                [.posixPermissions: 0o600],
                ofItemAtPath: markerURL.path)
            return true
        } catch {
            NSLog("[SSH2FA] could not persist secure-storage completion marker: \(error.localizedDescription)")
            return false
        }
    }

    /// Whether to offer the warm-up pass.
    ///
    /// - `hostCount`: nothing to authorize with no hosts.
    /// - `consolidated`: the saved secrets already live in the verified stable
    ///   vault. No authorization probe or banner is needed after that.
    /// - `deferredForSession`: "Not now" hides the offer only until the next app
    ///   launch. Persisting that dismissal for a whole build caused a later SSH
    ///   reconnect to surprise the user with the very prompt this UI explains.
    static func shouldOffer(hostCount: Int,
                            consolidated: Bool,
                            deferredForSession: Bool) -> Bool {
        guard hostCount > 0 else { return false }
        guard !consolidated else { return false }
        return !deferredForSession
    }

    static func successMessage(hostCount: Int) -> String {
        if hostCount == 1 {
            return String(localized: "Your saved login is ready. Future launches, reconnects, and updates should not ask for your Mac password again.")
        }
        return String(localized: "Your saved logins are ready. Future launches, reconnects, and updates should not ask for your Mac password again.")
    }

    /// Shown immediately before and while macOS may present its own protected
    /// storage authorization panel. Apple owns that system panel's title and
    /// wording, so our UI must make the reason unambiguous before it appears.
    static func migrationAuthorizationExplanation() -> String {
        String(localized: "The macOS password dialog that may appear next is authorizing SSH2FA to read and migrate logins saved by older versions. It is not an SSH login. Your Mac password goes only to macOS; SSH2FA never receives it.")
    }

    static func failureMessage() -> String {
        String(localized: "SSH2FA couldn't finish migrating your saved logins. Nothing was lost. Approve the macOS migration confirmation and try again.")
    }
}
