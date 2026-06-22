import Foundation

/// Centralized resolution of the user's ssh directory + the files SSH2FA
/// reads/writes, honoring SSH_CONFIG_PATH exactly like the rest of the app
/// (AddHostSheet / DaemonProcess / Settings). Pure + Foundation-only so it
/// compiles into the headless test bundle.
enum SSHPaths {
    /// The ssh config directory (no trailing slash). SSH_CONFIG_PATH wins,
    /// tilde-expanded; otherwise ~/.ssh.
    static func sshDir(env: [String: String] = ProcessInfo.processInfo.environment,
                       home: String = NSHomeDirectory()) -> String {
        let raw = env["SSH_CONFIG_PATH"].map { ($0 as NSString).expandingTildeInPath }
            ?? home + "/.ssh"
        return raw.hasSuffix("/") ? String(raw.dropLast()) : raw
    }

    static func configFile(dir: String) -> String { dir + "/config" }
    static func managedConfFile(dir: String) -> String { dir + "/ssh2fa.conf" }
    static func backupFile(dir: String, timestamp: String) -> String {
        dir + "/config.ssh2fa-backup-" + timestamp
    }

    /// The ControlPath the daemon's single master falls back to when ssh config
    /// declares no `controlpath` — `<dir>/cm-ssh2fa-<alias>`. Mirrors a2fa-core
    /// `control.rs` `resolve_control_base` fallback so the app attaches to the
    /// same socket.
    static func controlPathFallback(dir: String, alias: String) -> String {
        dir + "/cm-ssh2fa-" + alias
    }
}
