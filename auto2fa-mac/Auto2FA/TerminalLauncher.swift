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
/// Which app to use is asked ONCE via a picker the first time, then remembered
/// in UserDefaults (`SettingsKey.terminalApp`) and changeable in Settings.
enum TerminalLauncher {
    static let prefKey = SettingsKey.terminalApp

    static let appleTerminalBundleID = "com.apple.Terminal"
    static let iTermBundleID = "com.googlecode.iterm2"

    /// "" sentinel = "ask the first time"; "system" = default `.command`
    /// handler; otherwise a bundle id.
    static func iTermInstalled() -> Bool {
        NSWorkspace.shared.urlForApplication(withBundleIdentifier: iTermBundleID) != nil
    }

    /// Open `ssh <host>` in the chosen terminal, attaching to the daemon's warm
    /// master so there's no second 2FA prompt. First call (preference empty)
    /// shows the one-time picker (on the main thread); the `ssh -G` ControlPath
    /// resolution runs OFF the main thread (it can be slow / wedge).
    static func openSSH(host: String) {
        let stored = UserDefaults.standard.string(forKey: prefKey) ?? ""
        let choice: String
        if stored.isEmpty {
            guard let picked = promptForChoice() else { return }  // dismissed
            UserDefaults.standard.set(picked, forKey: prefKey)
            choice = picked
        } else {
            choice = stored
        }
        DispatchQueue.global(qos: .userInitiated).async {
            let controlPath = ControlPathResolver.resolve(alias: host)
            DispatchQueue.main.async { launch(host: host, choice: choice, controlPath: controlPath) }
        }
    }

    /// Returns "system" or a bundle id; nil if the alert was dismissed.
    private static func promptForChoice() -> String? {
        let alert = NSAlert()
        alert.messageText = "Open SSH in which terminal?"
        alert.informativeText = "Pick the app the host “Open Terminal” action should use. It's remembered for next time — change it anytime in Settings."
        var options: [(title: String, value: String)] = []
        if iTermInstalled() { options.append(("iTerm", iTermBundleID)) }
        options.append(("Terminal", appleTerminalBundleID))
        options.append(("System Default", "system"))
        for o in options { alert.addButton(withTitle: o.title) }
        let resp = alert.runModal()
        let idx = resp.rawValue - NSApplication.ModalResponse.alertFirstButtonReturn.rawValue
        guard idx >= 0, idx < options.count else { return nil }
        return options[idx].value
    }

    private static func launch(host: String, choice: String, controlPath: String) {
        // Defense-in-depth: the daemon restricts host names to [A-Za-z0-9._-],
        // so both the filename and the shell literal are safe; escape anyway.
        let safeHost = host
            .replacingOccurrences(of: "\\", with: "\\\\")
            .replacingOccurrences(of: "\"", with: "\\\"")
        let safeCP = controlPath
            .replacingOccurrences(of: "\\", with: "\\\\")
            .replacingOccurrences(of: "\"", with: "\\\"")
        let path = "/tmp/ssh2fa-\(host).command"
        // ControlMaster=no → attach to the live master only, never try to BECOME
        // one from the terminal. If no socket exists ssh just opens a normal
        // connection. ControlPath matches what the daemon's master binds.
        let body = "#!/bin/bash\nexec ssh -o ControlMaster=no -o ControlPath=\"\(safeCP)\" \"\(safeHost)\"\n"
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
            NSLog("[SSH2FA] openSSH host=\(host) via=\(choice.isEmpty ? "default" : choice) cp=\(controlPath)")
        } catch {
            NSLog("[SSH2FA] openSSH failed: \(error.localizedDescription)")
        }
    }
}
