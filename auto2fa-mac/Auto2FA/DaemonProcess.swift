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

        // Use the user's shell so PATH / aliases / pyenv / virtualenvs resolve
        // correctly. Bash -lc loads .zshrc/.bash_profile so `python` is the
        // same one the user uses interactively.
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/bin/zsh")
        p.arguments = ["-lc", "cd \(shellQuote(projectDir)) && exec python -m auto2fa.daemon"]
        p.currentDirectoryURL = URL(fileURLWithPath: projectDir)

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

            // Wait up to 5s for the socket to appear and respond.
            for _ in 0..<25 {
                try? await Task.sleep(nanoseconds: 200_000_000)
                if DaemonProcess.socketResponds() {
                    return .spawned(pid: p.processIdentifier)
                }
            }
            return .failed(reason: "Daemon spawned (PID \(p.processIdentifier)) but didn't open the socket within 5s. See /tmp/auto2fa-daemon-mac.log.")
        } catch {
            return .failed(reason: "Could not launch daemon: \(error.localizedDescription)")
        }
    }

    /// Kill the daemon if we spawned it. No-op if it was already running when
    /// we started.
    func terminateOwnedDaemon() {
        guard let p = ownedProcess, p.isRunning else { return }
        NSLog("[Auto2FA] terminating owned daemon PID=\(p.processIdentifier)")
        p.terminate()
        // Give it a moment to clean up SSH masters before going harder
        let start = Date()
        while p.isRunning && Date().timeIntervalSince(start) < 6.0 {
            Thread.sleep(forTimeInterval: 0.1)
        }
        if p.isRunning {
            NSLog("[Auto2FA] daemon didn't exit gracefully; sending SIGKILL")
            kill(p.processIdentifier, SIGKILL)
        }
    }

    private func shellQuote(_ s: String) -> String {
        // Single-quote the path and escape any embedded single quotes.
        return "'" + s.replacingOccurrences(of: "'", with: "'\\''") + "'"
    }
}
