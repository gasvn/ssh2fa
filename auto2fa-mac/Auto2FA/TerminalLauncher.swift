import AppKit
import Foundation

/// Opens a Terminal window SSH'd into a host, using the user's chosen terminal
/// app.
///
/// The terminal is launched by writing a temp `.command` script and opening it
/// — NO Automation (Apple Events) permission required (unlike
/// `tell application "Terminal"`, which TCC silently denies on ad-hoc /
/// unstably-signed builds — that was the "Terminal button does nothing" bug).
///
/// The system's default `.command` handler is used with no first-run question;
/// power users can choose Terminal/iTerm in Settings.
enum TerminalLauncher {
    static let prefKey = SettingsKey.terminalApp

    static let appleTerminalBundleID = "com.apple.Terminal"
    static let iTermBundleID = "com.googlecode.iterm2"

    /// "" / "system" = default `.command` handler; otherwise a bundle id.
    static func iTermInstalled() -> Bool {
        NSWorkspace.shared.urlForApplication(withBundleIdentifier: iTermBundleID) != nil
    }

    /// Open `ssh <host>` in the chosen terminal, attaching to the daemon's warm
    /// master so there's no second 2FA prompt. The `ssh -G` ControlPath
    /// resolution runs OFF the main thread (it can be slow / wedge).
    static func openSSH(host: String) {
        let stored = UserDefaults.standard.string(forKey: prefKey) ?? ""
        let choice = stored.isEmpty ? "system" : stored
        if stored.isEmpty { UserDefaults.standard.set(choice, forKey: prefKey) }
        DispatchQueue.global(qos: .userInitiated).async {
            let dir = SSHPaths.sshDir()
            let wrapper = SSHPaths.daemonWrapperFile(dir: dir)
            let daemonConfig = FileManager.default.fileExists(atPath: wrapper) ? wrapper : nil
            let controlPath = ControlPathResolver.resolve(alias: host, dir: dir)
            DispatchQueue.main.async {
                launch(host: host, choice: choice, controlPath: controlPath,
                       daemonConfig: daemonConfig)
            }
        }
    }

    private static func launch(host: String, choice: String, controlPath: String,
                               daemonConfig: String?) {
        // Defense-in-depth: the daemon restricts host names to [A-Za-z0-9._-],
        // so both the filename and the shell literal are safe; escape anyway.
        let safeHost = host
            .replacingOccurrences(of: "\\", with: "\\\\")
            .replacingOccurrences(of: "\"", with: "\\\"")
        let safeCP = controlPath
            .replacingOccurrences(of: "\\", with: "\\\\")
            .replacingOccurrences(of: "\"", with: "\\\"")
        let configArgs: String
        if let daemonConfig {
            let safeConfig = daemonConfig
                .replacingOccurrences(of: "\\", with: "\\\\")
                .replacingOccurrences(of: "\"", with: "\\\"")
            configArgs = "-F \"\(safeConfig)\" "
        } else {
            configArgs = ""
        }
        let path = "/tmp/ssh2fa-\(host).command"
        // Use the daemon's config + exact socket, then forbid network fallback.
        // Without ProxyCommand=false, a stale/mismatched ControlPath silently
        // starts a brand-new SSH connection and asks for password/2FA, despite
        // the app saying Connected.
        let body = """
        #!/bin/bash
        if ! /usr/bin/ssh \(configArgs)-S "\(safeCP)" -O check "\(safeHost)" >/dev/null 2>&1; then
          printf '\\nSSH2FA: the verified background connection is no longer available.\\nReturn to SSH2FA, reconnect this host, then choose Open Terminal again.\\n\\n'
          read -r -p 'Press Return to close…' _
          exit 1
        fi
        exec /usr/bin/ssh \(configArgs)-o ControlMaster=no -o ControlPath="\(safeCP)" -o ProxyCommand=/usr/bin/false "\(safeHost)"
        """ + "\n"
        do {
            try body.write(toFile: path, atomically: true, encoding: .utf8)
            try FileManager.default.setAttributes([.posixPermissions: 0o755],
                                                  ofItemAtPath: path)
            let fileURL = URL(fileURLWithPath: path)
            if choice != "system",
               let appURL = NSWorkspace.shared.urlForApplication(withBundleIdentifier: choice) {
                NSWorkspace.shared.open([fileURL], withApplicationAt: appURL,
                                        configuration: NSWorkspace.OpenConfiguration())
            } else {
                NSWorkspace.shared.open(fileURL)  // system default .command handler
            }
            UserDefaults.standard.set(true, forKey: SettingsKey.usedTerminal)
            NSLog("[SSH2FA] openSSH host=\(host) via=\(choice.isEmpty ? "default" : choice) cp=\(controlPath)")
        } catch {
            NSLog("[SSH2FA] openSSH failed: \(error.localizedDescription)")
        }
    }
}
