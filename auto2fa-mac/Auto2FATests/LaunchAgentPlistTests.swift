import XCTest

/// Invariants of the daemon's LaunchAgent job.
///
/// These are not style preferences. Each assertion below stands for a way the
/// daemon has actually failed to run, and a plist key is exactly the kind of
/// thing that gets "tidied up" later by someone who cannot see what it prevents.
final class LaunchAgentPlistTests: XCTestCase {

    private func plist() -> [String: Any] {
        LaunchAgentPlist.dictionary(
            daemonPath: "/Applications/SSH2FA.app/Contents/Resources/ssh2fa-daemon",
            home: "/Users/example"
        )
    }

    /// REGRESSION (2026-08-07, cost hours to diagnose): `ProcessType` was
    /// "Background", which puts the job in launchd's low-priority scheduling
    /// band. On a machine under heavy load (load average ~300) launchd created
    /// the xpcproxy stub and then NEVER scheduled it far enough to exec the
    /// daemon — `state = xpcproxy` for over an hour, surviving repeated
    /// bootout/bootstrap/kickstart, re-signing, and a wholesale bundle replace.
    /// An identical job under a different label, differing only in NOT setting
    /// ProcessType, exec'd the same binary in under 5 seconds.
    ///
    /// The daemon answers a UI socket, drives SSH keepalives and rebuilds
    /// masters; being deprioritised under pressure is precisely when that
    /// matters most, and a starved daemon is indistinguishable from a crashed
    /// one at the UI (every host stuck "Reconnecting").
    func testNoProcessTypeSoTheJobIsNeverPutInTheBackgroundBand() {
        let p = plist()
        XCTAssertNil(
            p["ProcessType"],
            """
            ProcessType must stay ABSENT (= Standard scheduling). "Background" \
            lets launchd starve this job indefinitely on a loaded machine, \
            leaving it stuck in xpcproxy and never exec'd.
            """
        )
    }

    /// A clean SIGTERM tears down every ControlMaster on purpose, so respawning
    /// after a SUCCESSFUL exit would silently undo a deliberate stop. Crashes
    /// must still be restarted.
    func testKeepAliveRestartsOnCrashButNotAfterACleanExit() {
        guard let keepAlive = plist()["KeepAlive"] as? [String: Bool] else {
            return XCTFail("KeepAlive must be a dictionary, not a bare bool")
        }
        XCTAssertEqual(keepAlive["SuccessfulExit"], false)
    }

    /// launchd hands agents a minimal PATH. Without the Homebrew prefixes the
    /// daemon cannot find sshfs, and mounting fails with "sshfs not installed"
    /// on a machine where it plainly is.
    func testPathIncludesTheHomebrewPrefixes() {
        guard let env = plist()["EnvironmentVariables"] as? [String: String],
              let path = env["PATH"] else {
            return XCTFail("EnvironmentVariables.PATH missing")
        }
        for prefix in ["/usr/local/bin", "/opt/homebrew/bin", "/usr/bin"] {
            XCTAssertTrue(path.contains(prefix), "PATH is missing \(prefix): \(path)")
        }
    }

    /// The daemon resolves its config dir from SSH_CONFIG_PATH; if launchd does
    /// not set it, a reboot-time start reads the wrong directory and finds no
    /// hosts. (`zsh -lc` does not source .zshrc, so the shell's value is not
    /// inherited — the job must carry it explicitly.)
    func testConfigPathIsSetExplicitlyForTheUsersHome() {
        guard let env = plist()["EnvironmentVariables"] as? [String: String] else {
            return XCTFail("EnvironmentVariables missing")
        }
        XCTAssertEqual(env["SSH_CONFIG_PATH"], "/Users/example/.ssh/")
    }

    func testRunsTheGivenBinaryAtLoad() {
        let p = plist()
        XCTAssertEqual(p["RunAtLoad"] as? Bool, true)
        XCTAssertEqual(
            p["ProgramArguments"] as? [String],
            ["/Applications/SSH2FA.app/Contents/Resources/ssh2fa-daemon"]
        )
        XCTAssertEqual(p["Label"] as? String, "com.ssh2fa.daemon")
    }

    /// A long-lived supervisor holds an IPC listener, accepted client
    /// connections, per-host ControlMaster sockets, pty fds during logins and
    /// retained tunnel pipes. launchd's default soft limit is far too low, and
    /// exhausting it makes every subsequent spawn fail with "Too many open
    /// files" — which drives an endless restart + credential-read storm.
    func testRaisesTheOpenFileLimit() {
        guard let limits = plist()["SoftResourceLimits"] as? [String: Int] else {
            return XCTFail("SoftResourceLimits missing")
        }
        XCTAssertGreaterThanOrEqual(limits["NumberOfFiles"] ?? 0, 4096)
    }

    /// Both streams go to one file that the app and support requests read.
    func testStdoutAndStderrShareTheDaemonLog() {
        let p = plist()
        XCTAssertEqual(p["StandardOutPath"] as? String, "/tmp/ssh2fa_daemon.log")
        XCTAssertEqual(p["StandardErrorPath"] as? String, "/tmp/ssh2fa_daemon.log")
    }

    /// It must actually serialize — a plist that cannot be written leaves the
    /// user with no LaunchAgent and no daemon after a reboot.
    func testSerializesToAPropertyList() {
        guard let data = LaunchAgentPlist.data(
            daemonPath: "/tmp/d", home: "/Users/example"
        ) else {
            return XCTFail("plist must serialize")
        }
        let back = try? PropertyListSerialization.propertyList(
            from: data, options: [], format: nil
        ) as? [String: Any]
        XCTAssertEqual((back ?? [:])["Label"] as? String, "com.ssh2fa.daemon")
        XCTAssertNil((back ?? [:])["ProcessType"], "the absence must survive serialization")
    }
}
