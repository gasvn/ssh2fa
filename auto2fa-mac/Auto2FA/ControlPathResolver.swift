import Foundation

/// Resolves the ControlPath the daemon's single master binds for a host, so the
/// app (Terminal button) can attach to the warm master. Mirrors a2fa-core
/// `control.rs` `resolve_control_base`: prefer `ssh -G`'s `controlpath`, else
/// fall back to `<dir>/cm-ssh2fa-<alias>`.
enum ControlPathResolver {
    /// Pure: pick the `controlpath` value out of `ssh -G <host>` stdout
    /// (ssh lowercases keys; we match case-insensitively). Returns the expanded
    /// path, or the fallback when absent / `none`.
    static func pick(fromSSHG text: String, alias: String, dir: String) -> String {
        for raw in text.split(separator: "\n") {
            let line = raw.trimmingCharacters(in: .whitespaces)
            guard line.lowercased().hasPrefix("controlpath ") else { continue }
            let value = line.dropFirst("controlpath ".count).trimmingCharacters(in: .whitespaces)
            if value.isEmpty || value.lowercased() == "none" { break }
            return (value as NSString).expandingTildeInPath
        }
        return SSHPaths.controlPathFallback(dir: dir, alias: alias)
    }

    /// Run `ssh -G <alias>` with a hard timeout and resolve. Returns the
    /// fallback if ssh can't run or wedges. NOT exercised by unit tests (spawns
    /// a process). Call OFF the main thread (see TerminalLauncher).
    static func resolve(alias: String,
                        dir: String = SSHPaths.sshDir(),
                        timeout: TimeInterval = 3.0) -> String {
        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: "/usr/bin/ssh")
        proc.arguments = ["-G", alias]
        let pipe = Pipe()
        proc.standardOutput = pipe
        proc.standardError = FileHandle.nullDevice
        do { try proc.run() } catch {
            return SSHPaths.controlPathFallback(dir: dir, alias: alias)
        }
        // Bound the wait — a wedged `ssh -G` (hung ProxyCommand/Match exec)
        // must never freeze the caller. Mirrors the daemon's bounded ssh -G.
        let sem = DispatchSemaphore(value: 0)
        DispatchQueue.global(qos: .userInitiated).async { proc.waitUntilExit(); sem.signal() }
        if sem.wait(timeout: .now() + timeout) == .timedOut {
            proc.terminate()
            return SSHPaths.controlPathFallback(dir: dir, alias: alias)
        }
        // ssh -G output is small (<4 KB) → safe to read after exit.
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        let text = String(data: data, encoding: .utf8) ?? ""
        return pick(fromSSHG: text, alias: alias, dir: dir)
    }
}
