import Foundation
import AppKit

/// Manages the lifecycle of the **Rust** `a2fa-daemon` process.
///
/// On app launch we check whether a daemon is already listening on the Unix
/// socket. If yes (the normal case — a LaunchAgent `com.auto2fa.daemon` keeps
/// the Rust daemon running), we leave it alone and just connect. If not, we
/// spawn the Rust binary ourselves and keep its PID so we can shut it down when
/// the app terminates.
///
/// Daemon-binary discovery (the single self-contained Rust executable):
///   1. `~/.auto2fa/a2fa-daemon` — the installed/stable path the LaunchAgent
///      also points at.
///   2. `<project>/auto2fa-rs/target/release/a2fa-daemon` — dev checkout build.
///   3. give up; surface error to user.
///
/// The Rust daemon resolves its config dir from `SSH_CONFIG_PATH` (falling back
/// to `~/.ssh`), so we set that env explicitly when we spawn it to match the
/// LaunchAgent's environment exactly.
@MainActor
final class DaemonProcess {
    static let shared = DaemonProcess()

    private var ownedProcess: Process?
    private var logFileHandle: FileHandle?

    /// True iff the daemon process we spawned at launch is still running.
    /// Distinguishes "we own a running daemon" from "the daemon we
    /// spawned has crashed and a new socket-reconnect attempt is
    /// pointless until we respawn it."
    var ownedDaemonIsAlive: Bool {
        guard let p = ownedProcess else { return false }
        return p.isRunning
    }

    /// Re-spawn the daemon if (a) we spawned the original one AND (b) it's
    /// now dead. Returns the new SpawnResult or nil if we don't own it (it is
    /// managed externally — by the LaunchAgent — so launchd will respawn it).
    func respawnIfOwnedDaemonCrashed() async -> SpawnResult? {
        // If we never owned the daemon, the LaunchAgent manages it — don't
        // touch it (launchd's KeepAlive respawns it, re-adopting live masters).
        guard ownedProcess != nil else { return nil }
        if ownedDaemonIsAlive {
            return nil  // still alive — let the regular reconnect try
        }
        NSLog("[Auto2FA] owned daemon died — respawning")
        // Do NOT clear ownedProcess here. ensureRunning() overwrites it with
        // the new Process only on a successful spawn; if the spawn fails we
        // must keep the (dead) reference so this guard stays true and a later
        // retry will try to respawn again. Clearing it up-front meant one
        // failed respawn wedged the app into "never retry" forever.
        return await ensureRunning()
    }

    /// Returns true if a daemon is already responding on the socket. Used to
    /// short-circuit spawning a duplicate.
    static func socketResponds() -> Bool {
        let path = ("~/.auto2fa/auto2fa.sock" as NSString).expandingTildeInPath
        guard FileManager.default.fileExists(atPath: path) else { return false }
        // Try a quick connect; if it fails immediately, the socket is stale.
        // A real running daemon will accept and respond.
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { return false }
        defer { close(fd) }
        // NON-BLOCKING connect + short poll: this runs on the main actor
        // (ensureRunning / respawn loops), and a blocking connect() against a
        // wedged daemon with a full accept backlog would hang the UI thread
        // indefinitely — the exact blocking-syscall-on-critical-thread class.
        let flags = fcntl(fd, F_GETFL)
        _ = fcntl(fd, F_SETFL, flags | O_NONBLOCK)
        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let sunPathSize = MemoryLayout.size(ofValue: addr.sun_path)
        path.withCString { src in
            withUnsafeMutablePointer(to: &addr.sun_path) { dst in
                _ = strncpy(UnsafeMutableRawPointer(dst).assumingMemoryBound(to: CChar.self),
                            src, sunPathSize - 1)
            }
        }
        let len = socklen_t(MemoryLayout<sockaddr_un>.size)
        let result = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.connect(fd, $0, len)
            }
        }
        if result == 0 { return true }
        guard errno == EINPROGRESS else { return false }
        // Wait up to 500 ms for the connect to complete.
        var pfd = pollfd(fd: fd, events: Int16(POLLOUT), revents: 0)
        guard poll(&pfd, 1, 500) > 0, (pfd.revents & Int16(POLLOUT)) != 0 else { return false }
        // Check the final connect status.
        var soErr: Int32 = 0
        var soLen = socklen_t(MemoryLayout<Int32>.size)
        guard getsockopt(fd, SOL_SOCKET, SO_ERROR, &soErr, &soLen) == 0 else { return false }
        return soErr == 0
    }

    /// Discover the project directory containing the Rust workspace
    /// (`auto2fa-rs/`). Order: explicit override file > dev default > nil.
    static func discoverProjectDir() -> String? {
        let fm = FileManager.default
        let home = NSHomeDirectory()

        // 1. Explicit override
        let overridePath = home + "/.auto2fa/project-dir.txt"
        if let data = try? String(contentsOfFile: overridePath, encoding: .utf8) {
            let trimmed = data.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmed.isEmpty && fm.fileExists(atPath: trimmed + "/auto2fa-rs") {
                return trimmed
            }
        }

        // 2. Default dev path
        let defaultPath = home + "/logs/auto2fa_dev"
        if fm.fileExists(atPath: defaultPath + "/auto2fa-rs") {
            return defaultPath
        }

        return nil
    }

    /// Resolve the Rust `a2fa-daemon` binary to launch.
    ///
    /// Order: installed stable path (`~/.auto2fa/a2fa-daemon`, what the
    /// LaunchAgent uses) > daemon bundled inside this .app > dev release build
    /// under the project checkout > nil.
    static func discoverDaemonBinary() -> String? {
        let fm = FileManager.default
        let home = NSHomeDirectory()

        // 1. Daemon shipped inside this app bundle — preferred for a packaged
        //    release, and the same path the LaunchAgent runs (run in place from
        //    where it was signed; avoids the copy → OS_REASON_EXEC issue).
        if let bundled = bundledDaemonURL()?.path, fm.isExecutableFile(atPath: bundled) {
            return bundled
        }

        // 2. Installed/stable path (legacy hand-deploy / dev convenience).
        let installed = home + "/.auto2fa/a2fa-daemon"
        if fm.isExecutableFile(atPath: installed) {
            return installed
        }

        // 3. Dev release build under the project checkout.
        if let projectDir = discoverProjectDir() {
            let devBuild = projectDir + "/auto2fa-rs/target/release/a2fa-daemon"
            if fm.isExecutableFile(atPath: devBuild) {
                return devBuild
            }
        }

        return nil
    }

    /// The daemon binary shipped inside this .app
    /// (`Auto2FA.app/Contents/Resources/a2fa-daemon`), or nil in a dev build
    /// where it isn't bundled (the packaging script copies it in).
    /// True iff a `com.auto2fa.daemon` LaunchAgent is installed — i.e. launchd
    /// owns the daemon's lifecycle and the app must not spawn a competing copy.
    static func launchAgentInstalled() -> Bool {
        let p = NSHomeDirectory() + "/Library/LaunchAgents/com.auto2fa.daemon.plist"
        return FileManager.default.fileExists(atPath: p)
    }

    static func bundledDaemonURL() -> URL? {
        // Construct the path explicitly from the Resources dir rather than
        // Bundle.main.url(forResource:withExtension:) — the latter is
        // unreliable for an EXTENSIONLESS executable dropped into Resources by
        // the packaging script (it returned nil, so first-run install silently
        // no-op'd). resourceURL/a2fa-daemon is deterministic.
        guard let res = Bundle.main.resourceURL else { return nil }
        let url = res.appendingPathComponent("a2fa-daemon")
        return FileManager.default.fileExists(atPath: url.path) ? url : nil
    }

    /// First-run / post-update install: register the per-user LaunchAgent that
    /// keeps the daemon running (RunAtLoad + KeepAlive) and re-adopts live
    /// masters across reboots.
    ///
    /// The LaunchAgent points DIRECTLY at the daemon inside this app bundle
    /// (`Auto2FA.app/Contents/Resources/a2fa-daemon`) — it is NOT copied to
    /// `~/.auto2fa`. Two reasons:
    ///   1. A code-signed binary that is COPIED to a new path can be refused at
    ///      exec (`OS_REASON_EXEC`) under an Apple-Development cert — i.e. the
    ///      free, un-notarized distribution build. Running it in place from the
    ///      bundle (where it was signed) sidesteps that entirely.
    ///   2. App updates then update the daemon automatically (same path, new
    ///      bytes) — a kickstart picks them up.
    /// The trade-off (LaunchAgent path breaks if the app is moved/deleted) is
    /// handled by re-pointing on every launch.
    ///
    /// Idempotent and NON-DESTRUCTIVE: a no-op in a dev build (no bundled
    /// daemon → an existing hand-installed setup is untouched); rewrites the
    /// LaunchAgent only when it's missing or differs.
    func installBundledDaemonIfNeeded() {
        let fm = FileManager.default
        guard let bundled = DaemonProcess.bundledDaemonURL(), fm.fileExists(atPath: bundled.path) else {
            NSLog("[Auto2FA] no bundled daemon (dev build) — skipping first-run install")
            return
        }
        let home = NSHomeDirectory()
        let autoDir = home + "/.auto2fa"
        let marker = autoDir + "/.daemon-bundle-version"
        // Marker = "<app build>@<bundle daemon path>". A change in either (app
        // updated, or app moved) means launchd should kickstart to pick up the
        // new binary / path.
        let appVersion = (Bundle.main.infoDictionary?["CFBundleVersion"] as? String) ?? "0"
        let stamp = "\(appVersion)@\(bundled.path)"

        try? fm.createDirectory(atPath: autoDir, withIntermediateDirectories: true)
        let prevStamp = (try? String(contentsOfFile: marker, encoding: .utf8))?
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let daemonChanged = prevStamp != stamp
        if daemonChanged {
            try? stamp.write(toFile: marker, atomically: true, encoding: .utf8)
        }

        installOrRefreshLaunchAgent(daemonPath: bundled.path, daemonWasUpdated: daemonChanged)
    }

    /// Write `~/Library/LaunchAgents/com.auto2fa.daemon.plist` with this user's
    /// paths and (re)load it. Writes only when the on-disk plist is missing or
    /// differs, so a working install isn't churned. When the daemon binary was
    /// just updated, kickstart the service so the new binary runs (launchd
    /// re-adopts the live masters — zero relogin).
    private func installOrRefreshLaunchAgent(daemonPath: String, daemonWasUpdated: Bool) {
        let fm = FileManager.default
        let home = NSHomeDirectory()
        let label = "com.auto2fa.daemon"
        let agentsDir = home + "/Library/LaunchAgents"
        let plistPath = agentsDir + "/\(label).plist"

        let plist: [String: Any] = [
            "Label": label,
            "ProgramArguments": [daemonPath],
            "EnvironmentVariables": [
                // launchd gives agents a minimal PATH; include the Homebrew
                // prefixes so the daemon can find sshfs/macFUSE tooling.
                "PATH": "/usr/bin:/bin:/usr/sbin:/sbin:/usr/local/bin:/opt/homebrew/bin",
                "SSH_CONFIG_PATH": home + "/.ssh/",
            ],
            "RunAtLoad": true,
            // Restart on crash but NOT after a clean exit (a graceful SIGTERM
            // tears down masters on purpose).
            "KeepAlive": ["SuccessfulExit": false],
            "StandardOutPath": "/tmp/auto2fa_daemon.log",
            "StandardErrorPath": "/tmp/auto2fa_daemon.log",
            "ProcessType": "Background",
            "ThrottleInterval": 10,
            "ExitTimeOut": 30,
            "WorkingDirectory": home,
            "SoftResourceLimits": ["NumberOfFiles": 8192],
        ]
        guard let data = try? PropertyListSerialization.data(
            fromPropertyList: plist, format: .xml, options: 0
        ) else {
            NSLog("[Auto2FA] could not serialize LaunchAgent plist")
            return
        }

        try? fm.createDirectory(atPath: agentsDir, withIntermediateDirectories: true)
        let existing = try? Data(contentsOf: URL(fileURLWithPath: plistPath))
        let plistChanged = existing != data
        if plistChanged {
            do {
                try data.write(to: URL(fileURLWithPath: plistPath))
                NSLog("[Auto2FA] wrote LaunchAgent %@", plistPath)
            } catch {
                NSLog("[Auto2FA] LaunchAgent write failed: %@", error.localizedDescription)
                return
            }
        }

        // (Re)load. To pick up a CHANGED plist (e.g. a new daemon path) the
        // service must be booted out and re-bootstrapped — `kickstart` keeps
        // the old definition. `bootout` is ASYNC, so a `bootstrap` fired
        // immediately after can race the teardown and FAIL, leaving the
        // service UNLOADED (observed: "Could not find service … in domain").
        // So: bootout, then bootstrap with a bounded retry until it sticks.
        let uid = getuid()
        let domain = "gui/\(uid)"
        let target = "\(domain)/\(label)"
        if plistChanged {
            DaemonProcess.runLaunchctl(["bootout", target])  // async; ignore "not loaded"
            var loaded = false
            for attempt in 0..<6 {
                if DaemonProcess.runLaunchctl(["bootstrap", domain, plistPath]) == 0 {
                    loaded = true
                    break
                }
                // bootout not finished yet (or transient) — back off briefly.
                if attempt < 5 { Thread.sleep(forTimeInterval: 0.5) }
            }
            if !loaded {
                NSLog("[Auto2FA] LaunchAgent bootstrap did not succeed after retries — the daemon may need a manual relaunch")
            }
        } else if daemonWasUpdated {
            DaemonProcess.runLaunchctl(["kickstart", "-k", target])
        }
    }

    /// Fully tear down the install: unload + remove the LaunchAgent (the daemon
    /// exits, closing its SSH masters), delete every Keychain credential under
    /// the "auto2fa" service, remove ~/.auto2fa, and — if `purgeConfig` —
    /// remove passwords.json + tunnels.json. The .app itself is left for the
    /// user to drag to the Trash (a running app can't delete its own bundle).
    func performUninstall(purgeConfig: Bool) {
        let fm = FileManager.default
        let home = NSHomeDirectory()
        let label = "com.auto2fa.daemon"
        let uid = getuid()

        // 1. Unload + remove the LaunchAgent.
        DaemonProcess.runLaunchctl(["bootout", "gui/\(uid)/\(label)"])
        let plist = home + "/Library/LaunchAgents/\(label).plist"
        try? fm.removeItem(atPath: plist)

        // SIGTERM any daemon still running so its masters close cleanly.
        DaemonProcess.runProcess("/usr/bin/pkill", ["-TERM", "-x", "a2fa-daemon"])

        // 2. Delete every Keychain credential (service "auto2fa"). Each call
        //    removes one; loop until none remain.
        var deleted = 0
        while DaemonProcess.runProcess("/usr/bin/security",
                                       ["delete-generic-password", "-s", "auto2fa"]) == 0 {
            deleted += 1
            if deleted > 1000 { break } // safety
        }
        NSLog("[Auto2FA] uninstall: removed %d Keychain credential(s)", deleted)

        // 3. ~/.auto2fa (socket, marker, legacy daemon copy).
        try? fm.removeItem(atPath: home + "/.auto2fa")

        // 4. Optional config.
        if purgeConfig {
            let sshDir = (ProcessInfo.processInfo.environment["SSH_CONFIG_PATH"]
                .map { ($0 as NSString).expandingTildeInPath } ?? home + "/.ssh")
            let dir = sshDir.hasSuffix("/") ? String(sshDir.dropLast()) : sshDir
            for f in ["passwords.json", "tunnels.json"] {
                try? fm.removeItem(atPath: dir + "/" + f)
            }
        }
        NSLog("[Auto2FA] uninstall complete (purgeConfig=%@)", purgeConfig ? "yes" : "no")
    }

    /// Run an arbitrary tool, returning its exit code (best-effort).
    @discardableResult
    private static func runProcess(_ path: String, _ args: [String]) -> Int32 {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: path)
        p.arguments = args
        p.standardOutput = Pipe()
        p.standardError = Pipe()
        do { try p.run(); p.waitUntilExit(); return p.terminationStatus }
        catch { return -1 }
    }

    /// Run `/bin/launchctl` with `args`, best-effort (errors logged, ignored).
    @discardableResult
    private static func runLaunchctl(_ args: [String]) -> Int32 {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/bin/launchctl")
        p.arguments = args
        do {
            try p.run()
            p.waitUntilExit()
            return p.terminationStatus
        } catch {
            NSLog("[Auto2FA] launchctl %@ failed: %@", args.joined(separator: " "), error.localizedDescription)
            return -1
        }
    }

    /// Spawn the daemon if it isn't already running. Returns:
    ///   - .alreadyRunning if a daemon was already listening
    ///   - .spawned if we just started one
    ///   - .failed(reason) if we couldn't find / start the daemon
    enum SpawnResult {
        case alreadyRunning
        case spawned(pid: Int32)
        case failed(reason: String)
    }

    func ensureRunning() async -> SpawnResult {
        if DaemonProcess.socketResponds() {
            NSLog("[Auto2FA] daemon already running; not spawning")
            return .alreadyRunning
        }

        // If a LaunchAgent manages the daemon, NEVER spawn a competing copy.
        // A non-responding socket here means the daemon is either down (launchd
        // KeepAlive respawns it) or merely BUSY — e.g. a relogin storm makes its
        // IPC briefly unresponsive. Spawning our own daemon in that window
        // created a SECOND instance → singleton-lock conflict → a graceful
        // shutdown that tore down every master (full 2FA relogin) → the daemon
        // got busy again → the probe failed again → an infinite
        // teardown/relogin loop. Defer to launchd; the connection watcher +
        // poll fallback reconnect on their own once it's responsive again.
        if DaemonProcess.launchAgentInstalled() {
            NSLog("[Auto2FA] socket not responding but a LaunchAgent manages the daemon — deferring to launchd, not spawning a duplicate")
            return .alreadyRunning
        }

        guard let binary = DaemonProcess.discoverDaemonBinary() else {
            let msg = "a2fa-daemon binary not found. Install it to " +
                      "~/.auto2fa/a2fa-daemon (or build auto2fa-rs in release)."
            NSLog("[Auto2FA] %@", msg)
            return .failed(reason: msg)
        }

        let p = Process()
        p.executableURL = URL(fileURLWithPath: binary)
        p.arguments = []
        // Match the LaunchAgent environment: the Rust daemon resolves its
        // config dir from SSH_CONFIG_PATH (falling back to ~/.ssh). Set it
        // explicitly so a daemon we spawn reads the SAME passwords.json /
        // tunnels.json the LaunchAgent-managed one would.
        var env = ProcessInfo.processInfo.environment
        if env["SSH_CONFIG_PATH"] == nil {
            env["SSH_CONFIG_PATH"] = NSHomeDirectory() + "/.ssh/"
        }
        p.environment = env

        // The Rust daemon writes its own log to /tmp/auto2fa_daemon.log; we
        // capture any stdout/stderr (the "listening on …" line, panics) to a
        // separate file so a spawn that dies before logging is still debuggable.
        let logURL = URL(fileURLWithPath: "/tmp/auto2fa-daemon-mac.log")
        if !FileManager.default.fileExists(atPath: logURL.path) {
            FileManager.default.createFile(atPath: logURL.path, contents: nil)
        }
        // Close any handle from a previous spawn attempt before opening a new
        // one — the backoff loop can call ensureRunning several times per
        // disconnect, and overwriting the property leaked one fd each time.
        try? self.logFileHandle?.close()
        self.logFileHandle = nil
        if let handle = try? FileHandle(forWritingTo: logURL) {
            _ = try? handle.seekToEnd()
            handle.write("\n--- a2fa-daemon spawn at \(Date()) ---\n".data(using: .utf8)!)
            p.standardOutput = handle
            p.standardError = handle
            self.logFileHandle = handle
        }

        do {
            try p.run()
            self.ownedProcess = p
            NSLog("[Auto2FA] spawned a2fa-daemon PID=\(p.processIdentifier) from \(binary)")

            // Wait up to 10s for the socket to appear and respond. The Rust
            // daemon starts in milliseconds (no interpreter warmup), but it
            // also adopts live ControlMasters / reads config on boot, so give
            // it a comfortable window.
            for _ in 0..<50 {
                try? await Task.sleep(nanoseconds: 200_000_000)
                if DaemonProcess.socketResponds() {
                    return .spawned(pid: p.processIdentifier)
                }
            }
            return .failed(reason: "Daemon spawned (PID \(p.processIdentifier)) but didn't open the socket within 10s. See /tmp/auto2fa-daemon-mac.log.")
        } catch {
            return .failed(reason: "Could not launch a2fa-daemon: \(error.localizedDescription)")
        }
    }

    /// Kill the daemon if we spawned it. No-op if it was already running when
    /// we started (LaunchAgent-managed — leave it to launchd).
    ///
    /// Called from NSApplication.willTerminateNotification on the main thread.
    func terminateOwnedDaemon() {
        guard let p = ownedProcess, p.isRunning else { return }
        NSLog("[Auto2FA] sending SIGTERM to a2fa-daemon PID=\(p.processIdentifier)")
        p.terminate()
        // Don't wait here. The Rust daemon's SIGTERM handler tears down its
        // masters + tunnels and removes the socket. If macOS SIGKILLs us
        // first, the next daemon start's cleanup_orphans reaps any strays.
    }
}
