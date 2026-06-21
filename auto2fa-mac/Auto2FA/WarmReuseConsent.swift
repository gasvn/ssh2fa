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

    /// Enable warm reuse: add the `Include ssh2fa.conf` line to ~/.ssh/config
    /// (with backup), so the user's OWN `ssh <alias>` reuses the warm master.
    /// ssh2fa.conf itself is owned by `AppState.syncManagedSSHConfig` (written on
    /// every reload with each host's full connection block); this path must NOT
    /// rewrite it — a `conn: nil` rewrite here would strip guided hosts'
    /// HostName/User until the next sync. Flips the persisted flag on success.
    static func apply(currentAliases: [String]) {
        let dir = SSHPaths.sshDir()
        do {
            try SSHConfigManager.enableInclude(dir: dir, timestamp: timestamp())
            UserDefaults.standard.set(true, forKey: SettingsKey.warmReuseEnabled)
        } catch {
            NSLog("[SSH2FA] warm-reuse apply failed: \(error.localizedDescription)")
        }
    }

    /// Revert: remove the Include line from ~/.ssh/config, clear the flag.
    /// Does NOT delete ssh2fa.conf — the daemon resolves hosts through it via
    /// `ssh -F`, so it is load-bearing regardless of the terminal-reuse opt-in.
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
