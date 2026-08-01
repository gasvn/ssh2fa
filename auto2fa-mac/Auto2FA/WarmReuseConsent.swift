import Foundation

/// Zero-setup management of the one safe `Include` that makes the user's own
/// `ssh <alias>` reuse SSH2FA's verified background master. The original config
/// is backed up and existing Host blocks are never rewritten. Users can still
/// explicitly opt out in Settings.
enum WarmReuseConsent {
    /// Install warm reuse by default once there is a host. `migration` is true
    /// on reload so existing installs get the new zero-setup behavior once; an
    /// explicit Settings opt-out is always respected.
    static func enableByDefaultIfNeeded(currentAliases: [String], migration: Bool = false) {
        let d = UserDefaults.standard
        guard SSHConfigManager.shouldEnableWarmReuseByDefault(
            hasHosts: !currentAliases.isEmpty,
            enabled: d.bool(forKey: SettingsKey.warmReuseEnabled),
            explicitlyDisabled: d.bool(forKey: SettingsKey.warmReuseExplicitlyDisabled),
            migration: migration,
            migrationCompleted: d.bool(forKey: SettingsKey.warmReuseDefaultMigrated)
        ) else { return }
        if migration {
            // Set before the filesystem attempt: a read-only/malformed config
            // must not trigger a backup/write attempt on every five-second poll.
            d.set(true, forKey: SettingsKey.warmReuseDefaultMigrated)
        }
        apply()
    }

    /// Enable warm reuse: add the `Include ssh2fa.conf` line to ~/.ssh/config
    /// (with backup), so the user's OWN `ssh <alias>` reuses the warm master.
    /// ssh2fa.conf itself is owned by `AppState.syncManagedSSHConfig`; this path
    /// only makes the already-generated file visible to ordinary ssh. Flips the
    /// persisted flag on success.
    static func apply() {
        let dir = SSHPaths.sshDir()
        do {
            let configPath = SSHConfigManager.realPath(SSHPaths.configFile(dir: dir))
            let existing = (try? String(contentsOfFile: configPath, encoding: .utf8)) ?? ""
            if !SSHConfigManager.hasInclude(existing) {
                try SSHConfigManager.enableInclude(dir: dir, timestamp: timestamp())
            }
            let d = UserDefaults.standard
            d.set(true, forKey: SettingsKey.warmReuseEnabled)
            d.set(false, forKey: SettingsKey.warmReuseExplicitlyDisabled)
            d.set(true, forKey: SettingsKey.warmReuseDefaultMigrated)
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
            let d = UserDefaults.standard
            d.set(false, forKey: SettingsKey.warmReuseEnabled)
            d.set(true, forKey: SettingsKey.warmReuseExplicitlyDisabled)
            d.set(true, forKey: SettingsKey.warmReuseDefaultMigrated)
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
