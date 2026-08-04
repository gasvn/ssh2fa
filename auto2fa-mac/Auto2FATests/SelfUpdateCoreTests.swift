import XCTest

// SelfUpdateCore is compiled into THIS test bundle via project.yml
// (sources include Auto2FA/SelfUpdateCore.swift) — same module, no import.

/// The one-click updater replaces the running app with a bundle it downloaded
/// off the internet, so the interesting tests are the refusals: the wrong app,
/// an older build, a corrupted download, an install location we can't write.
final class SelfUpdateCoreTests: XCTestCase {
    // MARK: - Where can we self-update?

    func testNormalApplicationsInstallCanSelfUpdate() {
        let b = SelfUpdateCore.blocker(bundlePath: "/Applications/SSH2FA.app",
                                       isReadOnlyVolume: false,
                                       isWritable: { _ in true })
        XCTAssertNil(b)
    }

    func testNonBundleBuildIsRefused() {
        XCTAssertEqual(
            SelfUpdateCore.blocker(bundlePath: "/usr/local/bin/ssh2fa",
                                   isReadOnlyVolume: false, isWritable: { _ in true }),
            .notAnAppBundle)
    }

    /// Gatekeeper app translocation runs the app from a throwaway read-only
    /// mount. Swapping "the bundle" there would update a copy that vanishes.
    func testTranslocatedAppIsRefused() {
        let p = "/private/var/folders/x1/abc/T/AppTranslocation/6F2C/d/SSH2FA.app"
        XCTAssertEqual(
            SelfUpdateCore.blocker(bundlePath: p, isReadOnlyVolume: false,
                                   isWritable: { _ in true }),
            .translocated)
    }

    /// Running straight out of the DMG — the "never dragged it to Applications"
    /// case. Must be named as such, not reported as a permissions problem.
    func testReadOnlyVolumeIsRefused() {
        XCTAssertEqual(
            SelfUpdateCore.blocker(bundlePath: "/Volumes/SSH2FA/SSH2FA.app",
                                   isReadOnlyVolume: true, isWritable: { _ in true }),
            .readOnlyLocation)
    }

    func testUnwritableParentDirectoryIsRefused() {
        XCTAssertEqual(
            SelfUpdateCore.blocker(bundlePath: "/Applications/SSH2FA.app",
                                   isReadOnlyVolume: false,
                                   isWritable: { $0 != "/Applications" }),
            .noPermission("/Applications"))
    }

    /// Parent writable but the bundle itself owned by someone else (installed by
    /// another admin). The swap renames the bundle, so this must be caught too —
    /// checking only the parent would let the update fail halfway, after the
    /// download and with the app already quit.
    func testUnwritableBundleIsRefusedEvenWhenParentIsWritable() {
        XCTAssertEqual(
            SelfUpdateCore.blocker(bundlePath: "/Applications/SSH2FA.app",
                                   isReadOnlyVolume: false,
                                   isWritable: { $0 != "/Applications/SSH2FA.app" }),
            .noPermission("/Applications"))
    }

    // MARK: - Picking the download

    private func asset(_ name: String, _ url: String, size: Int = 1,
                       digest: String? = nil, state: String? = "uploaded") -> [String: Any] {
        var a: [String: Any] = ["name": name, "browser_download_url": url,
                                "size": NSNumber(value: size)]
        if let digest { a["digest"] = digest }
        if let state { a["state"] = state }
        return a
    }

    func testPicksTheNamedDMGOverOtherAssets() {
        let d = String(repeating: "a", count: 64)
        let got = SelfUpdateCore.pickDMG(assets: [
            asset("checksums.txt", "https://x/checksums.txt"),
            asset("SSH2FA-debug.dmg", "https://x/debug.dmg"),
            asset("SSH2FA.dmg", "https://x/SSH2FA.dmg", size: 7_269_516, digest: "sha256:\(d)"),
        ])
        XCTAssertEqual(got?.url.absoluteString, "https://x/SSH2FA.dmg")
        XCTAssertEqual(got?.size, 7_269_516)
        XCTAssertEqual(got?.sha256, d)
    }

    func testSignedReleaseRequiresBothDMGAndManifest() {
        let assets: [[String: Any]] = [
            ["name": "SSH2FA.dmg", "browser_download_url": "https://example.test/app.dmg",
             "state": "uploaded", "size": 12],
            ["name": "SSH2FA.update.json", "browser_download_url": "https://example.test/update.json",
             "state": "uploaded"]
        ]
        let package = SelfUpdateCore.pickReleasePackage(assets: assets)
        XCTAssertEqual(package?.dmg.url.absoluteString, "https://example.test/app.dmg")
        XCTAssertEqual(package?.manifestURL.absoluteString, "https://example.test/update.json")
        XCTAssertNil(SelfUpdateCore.pickReleasePackage(assets: [assets[0]]))
    }

    func testIncompleteManifestAssetIsRejected() {
        let assets: [[String: Any]] = [
            ["name": "SSH2FA.dmg", "browser_download_url": "https://example.test/app.dmg"],
            ["name": "SSH2FA.update.json", "browser_download_url": "https://example.test/update.json",
             "state": "new"]
        ]
        XCTAssertNil(SelfUpdateCore.pickReleasePackage(assets: assets))
    }

    func testReleaseAPIIsPinnedToAdvertisedTag() {
        XCTAssertEqual(
            SelfUpdateCore.releaseAPIURL(advertisedVersion: "v1.5.12")?.absoluteString,
            "https://api.github.com/repos/gasvn/ssh2fa/releases/tags/v1.5.12")
        XCTAssertEqual(
            SelfUpdateCore.releaseAPIURL(advertisedVersion: "1.5.12")?.absoluteString,
            "https://api.github.com/repos/gasvn/ssh2fa/releases/tags/v1.5.12")
        XCTAssertNil(SelfUpdateCore.releaseAPIURL(advertisedVersion: "../latest"))
        XCTAssertNil(SelfUpdateCore.releaseAPIURL(advertisedVersion: "1.5.12-beta"))
        XCTAssertNil(SelfUpdateCore.releaseAPIURL(advertisedVersion: "1.５.12"))
    }

    func testRefusesADMGWhoseNameIsNotCoveredByTheManifestProtocol() {
        XCTAssertNil(SelfUpdateCore.pickDMG(assets: [
            asset("SSH2FA-1.6.0-universal.dmg", "https://x/u.dmg"),
        ]))
    }

    /// An asset GitHub is still receiving downloads as a truncated file.
    func testSkipsAssetsThatAreNotFinishedUploading() {
        XCTAssertNil(SelfUpdateCore.pickDMG(assets: [
            asset("SSH2FA.dmg", "https://x/SSH2FA.dmg", state: "starter"),
        ]))
    }

    func testReleaseWithoutADMGYieldsNothing() {
        XCTAssertNil(SelfUpdateCore.pickDMG(assets: [
            asset("Source code.zip", "https://x/src.zip"),
        ]))
        XCTAssertNil(SelfUpdateCore.pickDMG(assets: []))
    }

    // MARK: - Integrity

    func testDigestIsNormalized() {
        let hex = String(repeating: "Ab", count: 32)
        XCTAssertEqual(SelfUpdateCore.normalizedSHA256("sha256:\(hex)"), hex.lowercased())
        XCTAssertEqual(SelfUpdateCore.normalizedSHA256("  \(hex)  "), hex.lowercased())
    }

    /// A digest we can't parse must become nil (= "none published"), never a
    /// value that gets compared and silently mismatches — or worse, one that
    /// looks compared but isn't.
    func testUnparsableDigestBecomesNil() {
        XCTAssertNil(SelfUpdateCore.normalizedSHA256(nil))
        XCTAssertNil(SelfUpdateCore.normalizedSHA256("sha256:"))
        XCTAssertNil(SelfUpdateCore.normalizedSHA256("sha256:zz\(String(repeating: "a", count: 62))"))
        XCTAssertNil(SelfUpdateCore.normalizedSHA256(String(repeating: "a", count: 63)))
    }

    func testDigestMismatchIsRejectedAndMatchIsCaseInsensitive() {
        let a = String(repeating: "a", count: 64)
        let b = String(repeating: "b", count: 64)
        XCTAssertTrue(SelfUpdateCore.digestOK(expected: a, actual: a.uppercased()))
        XCTAssertFalse(SelfUpdateCore.digestOK(expected: a, actual: b))
    }

    /// Releases cut before GitHub published per-asset digests have none; the
    /// download is still TLS-protected, so absence must not block the update.
    func testMissingDigestIsAccepted() {
        XCTAssertTrue(SelfUpdateCore.digestOK(expected: nil, actual: "whatever"))
    }

    // MARK: - Vetting the staged bundle

    func testAcceptsAGenuineNewerBuild() {
        XCTAssertNil(SelfUpdateCore.rejectStagedApp(
            bundleID: "com.ssh2fa.app", version: "1.6.0", build: "170",
            currentVersion: "1.5.3", advertised: "v1.6.0",
            advertisedBuild: "170"))
    }

    func testRefusesADifferentApp() {
        let why = SelfUpdateCore.rejectStagedApp(
            bundleID: "com.evil.thing", version: "9.9.9", build: "999",
            currentVersion: "1.5.3", advertised: "9.9.9",
            advertisedBuild: "999")
        XCTAssertNotNil(why)
        XCTAssertTrue(why!.contains("com.evil.thing"))
    }

    /// A downgrade is either a mistake or an attack; either way it must not be
    /// installed silently over a newer build.
    func testRefusesADowngradeOrTheSameVersion() {
        XCTAssertNotNil(SelfUpdateCore.rejectStagedApp(
            bundleID: "com.ssh2fa.app", version: "1.4.0", build: "140",
            currentVersion: "1.5.3", advertised: "1.4.0",
            advertisedBuild: "140"))
        XCTAssertNotNil(SelfUpdateCore.rejectStagedApp(
            bundleID: "com.ssh2fa.app", version: "1.5.3", build: "153",
            currentVersion: "1.5.3", advertised: "1.5.3",
            advertisedBuild: "153"))
    }

    /// The release tag and the bundle inside the DMG must agree — a mismatch
    /// means the release was cut from the wrong build.
    func testRefusesWhenTheDMGDoesNotMatchTheAdvertisedVersion() {
        XCTAssertNotNil(SelfUpdateCore.rejectStagedApp(
            bundleID: "com.ssh2fa.app", version: "1.6.0", build: "160",
            currentVersion: "1.5.3", advertised: "1.7.0",
            advertisedBuild: "170"))
    }

    func testRefusesWhenTheDMGBuildDoesNotMatchSignedManifest() {
        let why = SelfUpdateCore.rejectStagedApp(
            bundleID: "com.ssh2fa.app", version: "1.6.0", build: "169",
            currentVersion: "1.5.3", advertised: "1.6.0",
            advertisedBuild: "170")
        XCTAssertNotNil(why)
        XCTAssertTrue(why!.contains("build 170"))
    }

    // MARK: - Shell quoting

    func testShQuoteNeutralizesHostilePaths() {
        XCTAssertEqual(SelfUpdateCore.shQuote("/Applications/SSH2FA.app"),
                       "'/Applications/SSH2FA.app'")
        XCTAssertEqual(SelfUpdateCore.shQuote("/My Apps/SSH2FA.app"),
                       "'/My Apps/SSH2FA.app'")
        // The only character that can escape single quotes.
        XCTAssertEqual(SelfUpdateCore.shQuote("/a'b"), "'/a'\\''b'")
        // Everything else stays inert inside the quotes.
        let nasty = SelfUpdateCore.shQuote("/tmp/x; rm -rf ~ #$(whoami)`id`")
        XCTAssertTrue(nasty.hasPrefix("'") && nasty.hasSuffix("'"))
        XCTAssertFalse(nasty.dropFirst().dropLast().contains("'"))
    }

    // MARK: - The swap script

    private func script(daemon: String? = "/Applications/SSH2FA.app/Contents/Resources/ssh2fa-daemon")
        -> String {
        SelfUpdateCore.swapScript(
            appPID: 4242,
            target: "/Applications/SSH2FA.app",
            staged: "/Applications/SSH2FA.app.new-ab12",
            old: "/Applications/SSH2FA.app.old-ab12",
            daemonPath: daemon,
            logPath: "/tmp/ssh2fa-update.log")
    }

    func testScriptWaitsForTheAppToExitBeforeTouchingAnything() {
        let s = script()
        let wait = s.range(of: "kill -0 4242")
        let move = s.range(of: "mv '/Applications/SSH2FA.app' ")
        XCTAssertNotNil(wait)
        XCTAssertNotNil(move)
        XCTAssertTrue(wait!.lowerBound < move!.lowerBound,
                      "the bundle must not be moved while the app is still running")
    }

    /// The wait is bounded: a hung app must not leave a swap script polling
    /// forever in the background.
    func testTheWaitIsBounded() {
        XCTAssertTrue(script().contains("-gt 300"))
    }

    /// THE load-bearing ordering. Freeze the daemon before moving its executable
    /// (otherwise macOS may re-authorize credential access under the temporary
    /// path), install the new bundle, then kill the frozen daemon so launchd
    /// respawns the new one. Only then may the old bundle be deleted.
    func testDaemonIsPausedBeforeTheMoveAndKilledAfterTheSwap() {
        let s = script()
        // `.backwards` for the purge: the script also clears any STALE
        // `.old-` left by a previous failed attempt, before the swap.
        guard let pause = s.range(of: "pkill -STOP"),
              let moveOld = s.range(of: "mv '/Applications/SSH2FA.app' '/Applications/SSH2FA.app.old-ab12'"),
              let swap = s.range(of: "mv '/Applications/SSH2FA.app.new-ab12' '/Applications/SSH2FA.app'"),
              let kill = s.range(of: "pkill -9"),
              let purge = s.range(of: "rm -rf '/Applications/SSH2FA.app.old-ab12'",
                                  options: .backwards) else {
            return XCTFail("script is missing the swap / kill / cleanup steps")
        }
        XCTAssertTrue(pause.lowerBound < moveOld.lowerBound,
                      "freeze credential access before the running executable moves")
        XCTAssertTrue(swap.lowerBound < kill.lowerBound,
                      "kill the daemon only after the new bundle is in place")
        XCTAssertTrue(kill.lowerBound < purge.lowerBound,
                      "the old bundle must outlive the daemon running from it")
    }

    func testFailedSwapResumesTheFrozenOldDaemon() {
        let s = script()
        XCTAssertGreaterThanOrEqual(s.components(separatedBy: "pkill -CONT").count - 1, 2,
                                    "both bundle-move failures must resume the old daemon")
    }

    /// A graceful daemon stop tears down every ControlMaster and costs a fresh
    /// 2FA login on every host. The updater must never do that.
    func testTheDaemonIsNeverStoppedGracefully() {
        // Command lines only — the script's own comment names SIGTERM to explain
        // why it is avoided, and that must not read as a violation.
        let commands = script()
            .split(separator: "\n")
            .filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("#") }
            .joined(separator: "\n")
        XCTAssertTrue(commands.contains("pkill -STOP"))
        XCTAssertTrue(commands.contains("pkill -9"))
        for forbidden in ["pkill -TERM", "pkill -15", "kill -TERM", "kill -15",
                          "bootout", "kickstart"] {
            XCTAssertFalse(commands.contains(forbidden),
                           "\(forbidden) stops the daemon gracefully — that tears down every ControlMaster")
        }
    }

    /// A dev build with no bundled daemon still gets a valid script.
    func testScriptIsValidWithoutABundledDaemon() {
        let s = script(daemon: nil)
        XCTAssertFalse(s.contains("pkill"))
        XCTAssertTrue(s.contains("mv '/Applications/SSH2FA.app.new-ab12' '/Applications/SSH2FA.app'"))
    }

    /// Every failure path must leave the user with a working app.
    func testEveryFailurePathRollsBackAndRelaunches() {
        let s = script()
        XCTAssertTrue(s.contains("mv '/Applications/SSH2FA.app.old-ab12' '/Applications/SSH2FA.app'"),
                      "a failed swap must restore the old bundle")
        // Three exits before the happy path (still running / missing staged /
        // swap failed) and each of the recoverable ones reopens the app.
        XCTAssertGreaterThanOrEqual(s.components(separatedBy: "/usr/bin/open").count - 1, 3)
        XCTAssertTrue(s.hasSuffix("echo \"update installed\""))
    }

    func testScriptQuotesEveryPathItTouches() {
        let s = SelfUpdateCore.swapScript(
            appPID: 1, target: "/My Apps/SSH 2FA.app", staged: "/My Apps/SSH 2FA.app.new",
            old: "/My Apps/SSH 2FA.app.old", daemonPath: "/My Apps/SSH 2FA.app/d",
            logPath: "/tmp/u.log")
        // An unquoted path with a space would split into two shell words.
        XCTAssertFalse(s.contains("mv /My Apps"))
        XCTAssertTrue(s.contains("'/My Apps/SSH 2FA.app'"))
        XCTAssertTrue(s.contains("exec >>'/tmp/u.log'"))
    }

    /// The script logs somewhere durable: by the time it runs, the app that
    /// started it is gone and there is nothing else to report a failure.
    func testScriptLogsToAFile() {
        XCTAssertTrue(script().contains("exec >>'/tmp/ssh2fa-update.log' 2>&1"))
    }

    // MARK: - Running the swap script for real

    /// String assertions can't catch a shell syntax error, an `sh`-ism that only
    /// works in bash, or an ordering mistake that only shows up at runtime — and
    /// this script runs after the app is gone, where a mistake means the user is
    /// left with no app at all. So these actually execute it against throwaway
    /// directories.
    private func runSwap(makeStaged: Bool, waitFor pid: Int32,
                         in dir: URL) throws -> (status: Int32, log: String) {
        let target = dir.appendingPathComponent("SSH2FA.app")
        let staged = dir.appendingPathComponent("SSH2FA.app.new-t")
        let old = dir.appendingPathComponent("SSH2FA.app.old-t")
        let log = dir.appendingPathComponent("u.log")
        let fm = FileManager.default
        try fm.createDirectory(at: target, withIntermediateDirectories: true)
        try "old".write(to: target.appendingPathComponent("marker"), atomically: true, encoding: .utf8)
        if makeStaged {
            try fm.createDirectory(at: staged, withIntermediateDirectories: true)
            try "new".write(to: staged.appendingPathComponent("marker"), atomically: true, encoding: .utf8)
        }
        let script = SelfUpdateCore.swapScript(
            appPID: pid, target: target.path, staged: staged.path, old: old.path,
            daemonPath: nil, logPath: log.path,
            // /usr/bin/true stands in for `open` so the test never asks
            // LaunchServices to launch a directory.
            openTool: "/usr/bin/true")
        let path = dir.appendingPathComponent("swap.sh")
        try script.write(to: path, atomically: true, encoding: .utf8)
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/bin/sh")
        p.arguments = [path.path]
        try p.run()
        p.waitUntilExit()
        let text = (try? String(contentsOf: log, encoding: .utf8)) ?? ""
        return (p.terminationStatus, text)
    }

    private func tempDir() throws -> URL {
        let d = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("ssh2fa-swaptest-\(UUID().uuidString.prefix(8))")
        try FileManager.default.createDirectory(at: d, withIntermediateDirectories: true)
        addTeardownBlock { try? FileManager.default.removeItem(at: d) }
        return d
    }

    func testTheScriptActuallySwapsTheBundle() throws {
        let dir = try tempDir()
        // A pid that has already exited: the wait loop falls straight through.
        let dead = Process()
        dead.executableURL = URL(fileURLWithPath: "/usr/bin/true")
        try dead.run()
        let pid = dead.processIdentifier
        dead.waitUntilExit()

        let r = try runSwap(makeStaged: true, waitFor: pid, in: dir)
        XCTAssertEqual(r.status, 0, "swap script failed:\n\(r.log)")
        let marker = try String(contentsOf: dir.appendingPathComponent("SSH2FA.app/marker"),
                                encoding: .utf8)
        XCTAssertEqual(marker, "new", "the installed bundle is still the old one")
        XCTAssertFalse(FileManager.default.fileExists(
            atPath: dir.appendingPathComponent("SSH2FA.app.old-t").path),
            "the replaced bundle must be cleaned up")
        XCTAssertFalse(FileManager.default.fileExists(
            atPath: dir.appendingPathComponent("SSH2FA.app.new-t").path),
            "the staging copy must be consumed, not left behind")
    }

    /// If the staged bundle vanished, the script must abort with the WORKING app
    /// still installed. Deleting first and copying second would have left the
    /// user with nothing.
    func testAnAbortedSwapLeavesTheInstalledAppIntact() throws {
        let dir = try tempDir()
        let dead = Process()
        dead.executableURL = URL(fileURLWithPath: "/usr/bin/true")
        try dead.run()
        let pid = dead.processIdentifier
        dead.waitUntilExit()

        let r = try runSwap(makeStaged: false, waitFor: pid, in: dir)
        XCTAssertEqual(r.status, 1)
        let marker = try String(contentsOf: dir.appendingPathComponent("SSH2FA.app/marker"),
                                encoding: .utf8)
        XCTAssertEqual(marker, "old", "the working app was damaged by a failed update")
    }

    /// The bundle must not be touched while the app is still running.
    func testTheScriptReallyWaitsForTheAppToExit() throws {
        let dir = try tempDir()
        let sleeper = Process()
        sleeper.executableURL = URL(fileURLWithPath: "/bin/sleep")
        sleeper.arguments = ["0.6"]
        try sleeper.run()

        let started = Date()
        let r = try runSwap(makeStaged: true, waitFor: sleeper.processIdentifier, in: dir)
        let elapsed = Date().timeIntervalSince(started)
        sleeper.waitUntilExit()

        XCTAssertEqual(r.status, 0, "swap script failed:\n\(r.log)")
        XCTAssertGreaterThan(elapsed, 0.4, "the swap did not wait for the app to exit")
        let marker = try String(contentsOf: dir.appendingPathComponent("SSH2FA.app/marker"),
                                encoding: .utf8)
        XCTAssertEqual(marker, "new")
    }

    // MARK: - Progress display

    func testFractionIsClampedAndSafeWithAnUnknownTotal() {
        XCTAssertEqual(SelfUpdateCore.fraction(received: 0, total: 0), 0)
        XCTAssertEqual(SelfUpdateCore.fraction(received: 500, total: 0), 0,
                       "an unknown total must not read as complete")
        XCTAssertEqual(SelfUpdateCore.fraction(received: 50, total: 100), 0.5, accuracy: 0.001)
        XCTAssertEqual(SelfUpdateCore.fraction(received: 300, total: 100), 1.0)
    }

    func testByteFormatting() {
        XCTAssertEqual(SelfUpdateCore.formatBytes(0), "0 KB")
        XCTAssertEqual(SelfUpdateCore.formatBytes(-5), "0 KB")
        XCTAssertEqual(SelfUpdateCore.formatBytes(512_000), "512 KB")
        XCTAssertEqual(SelfUpdateCore.formatBytes(7_269_516), "7.3 MB")
    }
}
