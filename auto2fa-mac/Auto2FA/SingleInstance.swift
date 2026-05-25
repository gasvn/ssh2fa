import AppKit

/// Ensure only one Auto2FA.app process is alive. If another instance is
/// already running, ask it to come forward, then exit ourselves —
/// otherwise both copies would spawn daemons and fight over passwords.json
/// / tunnels.json / SSH masters.
///
/// We use NSRunningApplication rather than a file lock so it's robust
/// across kill/SIGKILL leaving stale lock files behind.
@MainActor
enum SingleInstance {
    static func enforceOrExit() {
        let me = ProcessInfo.processInfo.processIdentifier
        let bundleID = Bundle.main.bundleIdentifier ?? ""
        let others = NSRunningApplication.runningApplications(
            withBundleIdentifier: bundleID
        ).filter { $0.processIdentifier != me }
        guard let first = others.first else { return }
        NSLog("[Auto2FA] another instance is running (PID \(first.processIdentifier)) — activating it and exiting")
        first.activate(options: [.activateAllWindows])
        NSApp.terminate(nil)
        // belt and suspenders — terminate is async
        exit(0)
    }
}
