import Foundation

/// The LaunchAgent definition for the daemon, as a plain dictionary.
///
/// Pure and Foundation-only so the job's invariants can be unit-tested. They
/// are not cosmetic: each one below has a failure mode behind it, and a plist
/// key is the kind of thing that gets "tidied up" years later by someone who
/// cannot see what it prevents.
enum LaunchAgentPlist {

    static let label = "com.ssh2fa.daemon"

    /// The daemon's log, used for both stdout and stderr.
    static let logPath = "/tmp/ssh2fa_daemon.log"

    static func dictionary(daemonPath: String, home: String) -> [String: Any] {
        [
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
            "StandardOutPath": logPath,
            "StandardErrorPath": logPath,
            //
            // NOTE THE ABSENCE OF `ProcessType`.
            //
            // It used to be "Background", which puts the job in launchd's
            // low-priority scheduling band — and that band can be starved
            // INDEFINITELY on a busy machine.
            //
            // Observed 2026-08-07: with a load average around 300 (32 runaway
            // processes burning ~850% CPU), launchd created this job's xpcproxy
            // stub and then never scheduled it far enough to exec the daemon.
            // It sat in `state = xpcproxy` for over an hour across repeated
            // bootout / bootstrap / kickstart cycles, and survived re-signing
            // and a wholesale bundle replacement — while an otherwise identical
            // job under a different label, differing ONLY in not setting
            // ProcessType, exec'd the same binary in under 5 seconds.
            //
            // Omitting the key means Standard, i.e. ordinary scheduling. That
            // is what this process needs: it answers a UI socket, drives SSH
            // keepalives, and rebuilds masters. Being deprioritised exactly
            // when the machine is under pressure is when its job matters most —
            // and a starved daemon is indistinguishable from a crashed one at
            // the UI, which shows every host stuck "Reconnecting".
            //
            "ThrottleInterval": 10,
            "ExitTimeOut": 30,
            "WorkingDirectory": home,
            "SoftResourceLimits": ["NumberOfFiles": 8192],
        ]
    }

    /// Serialized XML, ready to write. Nil only if the dictionary is somehow
    /// not property-list representable.
    static func data(daemonPath: String, home: String) -> Data? {
        try? PropertyListSerialization.data(
            fromPropertyList: dictionary(daemonPath: daemonPath, home: home),
            format: .xml,
            options: 0
        )
    }
}
