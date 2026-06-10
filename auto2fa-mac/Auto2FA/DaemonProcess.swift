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

        // 1. Installed/stable path (same as the LaunchAgent ProgramArguments).
        let installed = home + "/.auto2fa/a2fa-daemon"
        if fm.isExecutableFile(atPath: installed) {
            return installed
        }

        // 2. Daemon shipped inside this app bundle (a packaged release that
        //    hasn't run its first-run install yet).
        if let bundled = bundledDaemonURL()?.path, fm.isExecutableFile(atPath: bundled) {
            return bundled
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
    static func bundledDaemonURL() -> URL? {
        Bundle.main.url(forResource: "a2fa-daemon", withExtension: nil)
    }

    /// First-run / post-update install: copy the bundled daemon to the stable
    /// `~/.auto2fa/a2fa-daemon` path and register the per-user LaunchAgent that
    /// keeps it running (RunAtLoad + KeepAlive) and re-adopts live masters
    /// across reboots. Idempotent and NON-DESTRUCTIVE:
    ///   - no-op in a dev build (no bundled daemon → existing setup untouched);
    ///   - copies the daemon only when this app build ships a newer one than
    ///     last installed (tracked by a version marker);
    ///   - (re)writes the LaunchAgent with THIS user's home paths — never the
    ///     developer's — and reloads launchd so the new binary takes over.
    /// Best-effort: every failure is logged, never fatal (the app can still
    /// fall back to direct-spawning the daemon).
    func installBundledDaemonIfNeeded() {
        let fm = FileManager.default
        guard let bundled = DaemonProcess.bundledDaemonURL(), fm.fileExists(atPath: bundled.path) else {
            NSLog("[Auto2FA] no bundled daemon (dev build) — skipping first-run install")
            return
        }
        let home = NSHomeDirectory()
        let autoDir = home + "/.auto2fa"
        let installed = autoDir + "/a2fa-daemon"
        let marker = autoDir + "/.daemon-bundle-version"
        let appVersion = (Bundle.main.infoDictionary?["CFBundleVersion"] as? String) ?? "0"

        try? fm.createDirectory(atPath: autoDir, withIntermediateDirectories: true)

        // Copy the daemon only when this app build ships a daemon we haven't
        // installed yet (marker mismatch) or none is installed.
        let installedMarker = (try? String(contentsOfFile: marker, encoding: .utf8))?
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let needsCopy = !fm.isExecutableFile(atPath: installed) || installedMarker != appVersion
        if needsCopy {
            do {
                if fm.fileExists(atPath: installed) { try fm.removeItem(atPath: installed) }
                try fm.copyItem(atPath: bundled.path, toPath: installed)
                try fm.setAttributes([.posixPermissions: 0o755], ofItemAtPath: installed)
                try? appVersion.write(toFile: marker, atomically: true, encoding: .utf8)
                NSLog("[Auto2FA] installed bundled daemon → %@ (v%@)", installed, appVersion)
            } catch {
                NSLog("[Auto2FA] daemon install copy failed: %@", error.localizedDescription)
                return
            }
        }

        installOrRefreshLaunchAgent(daemonPath: installed, daemonWasUpdated: needsCopy)
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

        // (Re)load. `bootout` then `bootstrap` is the modern domain API; if the
        // service is already loaded and unchanged we just `kickstart -k` to pick
        // up a new binary. All best-effort across macOS versions.
        let uid = getuid()
        let domain = "gui/\(uid)"
        if plistChanged {
            DaemonProcess.runLaunchctl(["bootout", "\(domain)/\(label)"])  // ignore "not loaded"
            DaemonProcess.runLaunchctl(["bootstrap", domain, plistPath])
        } else if daemonWasUpdated {
            DaemonProcess.runLaunchctl(["kickstart", "-k", "\(domain)/\(label)"])
        }
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
