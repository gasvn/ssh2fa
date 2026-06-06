import Foundation
import AppKit

/// Manages the lifecycle of the Python daemon process.
///
/// On app launch we check whether a daemon is already listening on the Unix
/// socket. If yes, we leave it alone (user might have started it manually,
/// or it might be running under LaunchAgent). If not, we spawn one ourselves
/// with `python -m auto2fa.daemon` and keep its PID so we can shut it down
/// when the app terminates.
///
/// Project-dir discovery (where the auto2fa Python package lives):
///   1. `~/.auto2fa/project-dir.txt` — user-set explicit path
///   2. `$HOME/logs/auto2fa_dev` — default for the dev workstation
///   3. give up; surface error to user
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
    /// now dead. Returns the new SpawnResult or .alreadyRunning if the
    /// socket somehow came back without our intervention.
    func respawnIfOwnedDaemonCrashed() async -> SpawnResult? {
        // If we never owned the daemon, the user manages it externally
        // (LaunchAgent, manual launch) — don't touch it.
        guard ownedProcess != nil else { return nil }
        if ownedDaemonIsAlive {
            return nil  // still alive — let the regular reconnect try
        }
        NSLog("[Auto2FA] owned daemon died — respawning")
        ownedProcess = nil
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
        return result == 0
    }

    /// Discover the project directory containing `auto2fa/daemon.py`.
    /// Order: explicit override file > XDG-ish default > nil.
    static func discoverProjectDir() -> String? {
        let fm = FileManager.default
        let home = NSHomeDirectory()

        // 1. Explicit override
        let overridePath = home + "/.auto2fa/project-dir.txt"
        if let data = try? String(contentsOfFile: overridePath, encoding: .utf8) {
            let trimmed = data.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmed.isEmpty && fm.fileExists(atPath: trimmed + "/auto2fa/daemon.py") {
                return trimmed
            }
        }

        // 2. Default dev path
        let defaultPath = home + "/logs/auto2fa_dev"
        if fm.fileExists(atPath: defaultPath + "/auto2fa/daemon.py") {
            return defaultPath
        }

        return nil
    }

    /// Resolve a CONCRETE Python interpreter that can run `-m auto2fa.daemon`.
    ///
    /// We must never depend on a login shell's PATH at spawn time. At login
    /// launchd hands the app a pristine environment, and `zsh -lc` is a
    /// *non-interactive* login shell that does NOT source ~/.zshrc (that's
    /// interactive-only). So the user's conda/pyenv/venv `python` — and on
    /// modern macOS even a bare `python`, which no longer exists — isn't on
    /// PATH. The daemon then failed to start after every reboot with
    /// "command not found: python", which looked to the user like all their
    /// hosts and tunnels had vanished. The real files in ~/.ssh were fine.
    ///
    /// Order: explicit pin > one-time discovery through an *interactive*
    /// login shell (which DOES source .zshrc), cached for next boot >
    /// /usr/bin/python3 as a last resort.
    static func discoverInterpreter() -> String {
        let fm = FileManager.default
        let home = NSHomeDirectory()
        let pinPath = home + "/.auto2fa/python-path.txt"

        // 1. Explicit pin (this file also doubles as our discovery cache).
        if let data = try? String(contentsOfFile: pinPath, encoding: .utf8) {
            let trimmed = data.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmed.isEmpty && fm.isExecutableFile(atPath: trimmed) {
                return trimmed
            }
        }

        // 2. Discover the user's interactive interpreter ONCE. `-i` forces zsh
        //    to source ~/.zshrc, so conda/pyenv/venv activation runs and
        //    `python` resolves to the same one the user uses by hand.
        let probe = Process()
        probe.executableURL = URL(fileURLWithPath: "/bin/zsh")
        probe.arguments = ["-ilc", "command -v python || command -v python3"]
        let pipe = Pipe()
        probe.standardOutput = pipe
        probe.standardError = FileHandle.nullDevice
        if (try? probe.run()) != nil {
            probe.waitUntilExit()
            let out = String(data: pipe.fileHandleForReading.readDataToEndOfFile(),
                             encoding: .utf8) ?? ""
            // An interactive shell may print banners; take the last line that
            // actually points at an executable file.
            for line in out.split(separator: "\n").reversed() {
                let cand = line.trimmingCharacters(in: .whitespaces)
                if !cand.isEmpty && fm.isExecutableFile(atPath: cand) {
                    // Cache so we never pay the interactive-shell cost again.
                    try? cand.write(toFile: pinPath, atomically: true, encoding: .utf8)
                    return cand
                }
            }
        }

        // 3. Last resort. May lack deps, but yields an explicit ModuleNotFound
        //    in the log rather than a silent "command not found".
        return "/usr/bin/python3"
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

        guard let projectDir = DaemonProcess.discoverProjectDir() else {
            let msg = "Daemon project dir not found. Create ~/.auto2fa/project-dir.txt " +
                      "pointing at your auto2fa source checkout."
            NSLog("[Auto2FA] %@", msg)
            return .failed(reason: msg)
        }

        // Launch with a CONCRETE interpreter path — never through a login
        // shell. `zsh -lc` does not source ~/.zshrc, so a bare `python` is
        // unresolvable at login and the daemon would silently fail to start.
        // See discoverInterpreter() for the full story.
        let interpreter = DaemonProcess.discoverInterpreter()
        let p = Process()
        p.executableURL = URL(fileURLWithPath: interpreter)
        p.arguments = ["-m", "auto2fa.daemon"]
        p.currentDirectoryURL = URL(fileURLWithPath: projectDir)
        // Make the package importable regardless of how this interpreter
        // handles `-m` + cwd, while preserving any inherited environment.
        var env = ProcessInfo.processInfo.environment
        if let existing = env["PYTHONPATH"], !existing.isEmpty {
            env["PYTHONPATH"] = projectDir + ":" + existing
        } else {
            env["PYTHONPATH"] = projectDir
        }
        p.environment = env

        // Capture stdout/stderr to a log file so we can debug daemon crashes.
        let logURL = URL(fileURLWithPath: "/tmp/auto2fa-daemon-mac.log")
        if !FileManager.default.fileExists(atPath: logURL.path) {
            FileManager.default.createFile(atPath: logURL.path, contents: nil)
        }
        if let handle = try? FileHandle(forWritingTo: logURL) {
            _ = try? handle.seekToEnd()
            handle.write("\n--- daemon spawn at \(Date()) ---\n".data(using: .utf8)!)
            p.standardOutput = handle
            p.standardError = handle
            self.logFileHandle = handle
        }

        do {
            try p.run()
            self.ownedProcess = p
            NSLog("[Auto2FA] spawned daemon PID=\(p.processIdentifier) from \(projectDir)")

            // Wait up to 15s for the socket to appear and respond.
            // Cold-start Python (interpreter load + asyncio init) can
            // routinely take 5-10s; the old 5s window made first-launch
            // brittle on slower machines.
            for _ in 0..<75 {
                try? await Task.sleep(nanoseconds: 200_000_000)
                if DaemonProcess.socketResponds() {
                    return .spawned(pid: p.processIdentifier)
                }
            }
            return .failed(reason: "Daemon spawned (PID \(p.processIdentifier)) but didn't open the socket within 15s. See /tmp/auto2fa-daemon-mac.log.")
        } catch {
            return .failed(reason: "Could not launch daemon: \(error.localizedDescription)")
        }
    }

    /// Kill the daemon if we spawned it. No-op if it was already running when
    /// we started.
    ///
    /// Called from NSApplication.willTerminateNotification on the main thread.
    /// macOS gives the app ~5s after willTerminate before SIGKILL, so we
    /// SIGTERM and return immediately — the daemon's own signal handler
    /// owns its cleanup. Previously we Thread.sleep'd for up to 6s on main,
    /// which both blocked the UI and could be cut short by macOS anyway.
    func terminateOwnedDaemon() {
        guard let p = ownedProcess, p.isRunning else { return }
        NSLog("[Auto2FA] sending SIGTERM to daemon PID=\(p.processIdentifier)")
        p.terminate()
        // Don't wait here. The daemon's SIGINT/SIGTERM handler triggers
        // its asyncio shutdown which joins host threads (best-effort) and
        // removes the socket. If macOS SIGKILLs us before that finishes,
        // ssh ControlMaster cleanup will be picked up by cleanup_orphans
        // on the next daemon start.
    }
}
