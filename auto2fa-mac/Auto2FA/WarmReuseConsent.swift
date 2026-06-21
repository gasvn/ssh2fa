import AppKit
import Foundation

/// The one-time "make `ssh <alias>` in your own Terminal skip 2FA?" consent and
/// the apply/revert of the managed `Include`. Keeps the AppKit alert + file
/// writes out of the SwiftUI views.
enum WarmReuseConsent {
    /// Show the consent once, right after the first host is enabled. No-op if
    /// already enabled or already asked. Returns immediately; applies on accept.
    @MainActor static func offerIfNeeded(currentAliases: [String]) {
        let d = UserDefaults.standard
        if d.bool(forKey: SettingsKey.warmReuseEnabled) { return }
        if d.bool(forKey: SettingsKey.warmReuseAsked) { return }

        let alert = NSAlert()
        alert.messageText = "Make `ssh <host>` in your own Terminal skip the 2FA prompt too?"
        alert.informativeText = "SSH2FA backs up your SSH config and adds one `Include` line — it never touches your existing hosts. The app's own \"Open Terminal\" already reuses the connection without this."
        alert.addButton(withTitle: "Set it up")
        alert.addButton(withTitle: "Not now")
        let resp = alert.runModal()
        // Mark "asked" only AFTER the user responds — if the app is killed while
        // the dialog is up, the offer survives instead of being silenced forever.
        d.set(true, forKey: SettingsKey.warmReuseAsked)
        guard resp == .alertFirstButtonReturn else { return }   // "Not now" → leave off, never nag
        apply(currentAliases: currentAliases)
    }

    /// Enable warm reuse: write ssh2fa.conf for the current hosts + add the
    /// Include (with backup). Flips the persisted flag on success.
    static func apply(currentAliases: [String]) {
        let dir = SSHPaths.sshDir()
        do {
            try SSHConfigManager.writeManagedConf(hosts: currentAliases.map { .init(alias: $0, conn: nil) }, dir: dir)
            try SSHConfigManager.enableInclude(dir: dir, timestamp: timestamp())
            UserDefaults.standard.set(true, forKey: SettingsKey.warmReuseEnabled)
        } catch {
            NSLog("[SSH2FA] warm-reuse apply failed: \(error.localizedDescription)")
        }
    }

    /// Revert: remove the Include + ssh2fa.conf, clear the flag.
    static func revert() {
        do {
            try SSHConfigManager.disableInclude(dir: SSHPaths.sshDir())
            UserDefaults.standard.set(false, forKey: SettingsKey.warmReuseEnabled)
        } catch {
            NSLog("[SSH2FA] warm-reuse revert failed: \(error.localizedDescription)")
        }
    }

    private static func timestamp() -> String {
        let f = DateFormatter()
        f.dateFormat = "yyyyMMdd-HHmmss"
        return f.string(from: Date())
    }
}
