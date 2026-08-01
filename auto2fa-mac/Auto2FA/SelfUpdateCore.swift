import Foundation

/// Pure, dependency-free logic behind the ONE-CLICK in-app update.
///
/// The update used to be notify-only: the About pane printed two shell commands
/// and the user pasted one into Terminal. That is a dead end for anyone who
/// doesn't live in a shell, and it's the step people simply don't do — so users
/// sat on old builds carrying bugs that were already fixed.
///
/// Everything here is I/O-free so the risky decisions (which asset to download,
/// is the download intact, is the staged bundle really SSH2FA and really newer,
/// what exactly does the swap script run) are unit-tested headlessly. The
/// download / mount / swap side-effects live in `SelfUpdater`.
enum SelfUpdateCore {
    /// The only bundle identifier we will ever install over ourselves with.
    static let appBundleID = "com.ssh2fa.app"
    /// The app directory inside the release DMG.
    static let appBundleName = "SSH2FA.app"

    // MARK: - Can we even do this?

    /// Why an in-app update can't run from this install location.
    ///
    /// Each case is a genuinely different user story, so they carry their own
    /// explanation rather than collapsing into one "can't update".
    enum Blocker: Equatable {
        /// Not running from a `.app` at all (a dev build run from the CLI).
        case notAnAppBundle
        /// Running straight out of the DMG / a read-only volume — the classic
        /// "I never dragged it to Applications" case.
        case readOnlyLocation
        /// Gatekeeper app translocation: macOS is running us from a throwaway
        /// read-only mount, so the "real" bundle isn't even at this path.
        case translocated
        /// The bundle exists somewhere we may not write (e.g. installed by
        /// another user, or a locked /Applications).
        case noPermission(String)
        /// The running app is signed, but macOS does not trust its signing
        /// chain. The public self-signed build deliberately lands here: its
        /// downloaded replacement would fail the same strict verification, so
        /// offering one-click update would be a button that can never succeed.
        case untrustedSignature
    }

    /// Decide whether the running bundle can be replaced in place.
    ///
    /// `isWritable` is injected so this is testable without touching a disk;
    /// the caller passes `FileManager.default.isWritableFile(atPath:)`.
    static func blocker(bundlePath: String,
                        isReadOnlyVolume: Bool,
                        isWritable: (String) -> Bool) -> Blocker? {
        guard bundlePath.hasSuffix(".app") else { return .notAnAppBundle }
        // Translocation paths look like
        // /private/var/folders/…/AppTranslocation/<uuid>/d/SSH2FA.app
        if bundlePath.contains("/AppTranslocation/") { return .translocated }
        if isReadOnlyVolume { return .readOnlyLocation }
        let parent = (bundlePath as NSString).deletingLastPathComponent
        // BOTH matter: replacing the bundle unlinks an entry from the parent
        // directory (needs the parent writable) and moves the bundle itself.
        guard isWritable(parent), isWritable(bundlePath) else {
            return .noPermission(parent)
        }
        return nil
    }

    /// Add the signing precondition after the location checks. Location errors
    /// stay first because their fixes are concrete (move the app / permissions),
    /// while an untrusted self-signed release must use the manual update path.
    static func updateBlocker(location: Blocker?,
                              signatureTrusted: Bool) -> Blocker? {
        if let location { return location }
        return signatureTrusted ? nil : .untrustedSignature
    }

    // MARK: - Release assets

    /// The downloadable disk image of a GitHub release.
    struct ReleaseAsset: Equatable {
        var url: URL
        /// Lowercase hex SHA-256 published by GitHub, if the release has one.
        var sha256: String?
        /// Size in bytes, for the progress bar (0 when GitHub omits it).
        var size: Int64
    }

    /// Pick the DMG out of a release's `assets` array.
    ///
    /// Prefers the exact `SSH2FA.dmg` name and otherwise takes the first `.dmg`,
    /// so a release that also carries a zip / checksums file still resolves.
    /// Assets GitHub hasn't finished receiving (`state != "uploaded"`) are
    /// skipped — downloading one yields a truncated file.
    static func pickDMG(assets: [[String: Any]]) -> ReleaseAsset? {
        let usable = assets.filter { a in
            guard let name = a["name"] as? String,
                  name.lowercased().hasSuffix(".dmg"),
                  a["browser_download_url"] is String else { return false }
            // Absent state = older API payloads; treat as uploaded.
            let state = (a["state"] as? String) ?? "uploaded"
            return state == "uploaded"
        }
        let chosen = usable.first { ($0["name"] as? String) == "\(appBundleName.dropLast(4)).dmg" }
            ?? usable.first
        guard let a = chosen,
              let s = a["browser_download_url"] as? String,
              let url = URL(string: s) else { return nil }
        let size = (a["size"] as? NSNumber)?.int64Value ?? 0
        return ReleaseAsset(url: url,
                            sha256: normalizedSHA256(a["digest"] as? String),
                            size: size)
    }

    /// Normalize GitHub's `"sha256:AB12…"` digest to bare lowercase hex.
    /// Anything that isn't a 64-char hex string becomes nil — an unparsable
    /// digest must not be silently compared as if it were absent-but-fine.
    static func normalizedSHA256(_ raw: String?) -> String? {
        guard let raw else { return nil }
        var s = raw.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        if let r = s.range(of: "sha256:") { s = String(s[r.upperBound...]) }
        guard s.count == 64, s.allSatisfy({ $0.isHexDigit }) else { return nil }
        return s
    }

    /// Does the downloaded file match the published digest?
    ///
    /// A missing digest is NOT a failure: releases cut before GitHub exposed
    /// per-asset digests have none, and the download is already TLS-protected
    /// end to end. When a digest IS published it is enforced strictly — that is
    /// what catches a truncated or corrupted download, which would otherwise be
    /// "installed" as a broken bundle.
    static func digestOK(expected: String?, actual: String) -> Bool {
        guard let expected else { return true }
        return expected.lowercased() == actual.lowercased()
    }

    // MARK: - Is the staged bundle safe to install over ourselves?

    /// Vet the app found inside the mounted DMG before it replaces the running
    /// one. Returns nil when acceptable, else a user-facing reason.
    ///
    /// `advertised` is the version the release *claimed*; a DMG whose bundle
    /// disagrees means the release is mislabelled and is refused.
    ///
    /// Deliberately NOT checked: that the signing authority matches the running
    /// app's. Pinning it would be stronger, but this project has already had to
    /// change signing identity once (a revoked certificate), and a pinned
    /// authority would have stranded every user with no in-app path off the
    /// broken build. Integrity is covered by the published digest + a codesign
    /// validity check on the staged bundle.
    static func rejectStagedApp(bundleID: String,
                                version: String,
                                currentVersion: String,
                                advertised: String) -> String? {
        guard bundleID == appBundleID else {
            return "The downloaded app identifies itself as “\(bundleID)”, not SSH2FA."
        }
        guard UpdateCheckCore.isNewer(version, than: currentVersion) else {
            return "The downloaded build (\(version)) isn't newer than the one you're running (\(currentVersion))."
        }
        // Tolerate the "v" prefix on either side; refuse a real mismatch.
        let a = UpdateCheckCore.normalizeTag(advertised)
        guard a.isEmpty || a == UpdateCheckCore.normalizeTag(version) else {
            return "The release says \(UpdateCheckCore.displayVersion(a)) but the download contains \(UpdateCheckCore.displayVersion(version))."
        }
        return nil
    }

    // MARK: - The swap script

    /// Single-quote a path for `/bin/sh`. Paths come from the filesystem and can
    /// contain spaces, quotes, `$`, `;` — interpolating them raw into a shell
    /// script would be a command-injection bug in our own updater.
    static func shQuote(_ s: String) -> String {
        "'" + s.replacingOccurrences(of: "'", with: "'\\''") + "'"
    }

    /// The script that actually performs the swap, run detached so it outlives
    /// the app it is replacing.
    ///
    /// Ordering here is load-bearing:
    ///
    /// 1. Wait for the app to exit (bounded — never wait forever).
    /// 2. Swap the bundle FIRST, while the old daemon keeps running from the
    ///    renamed-away directory (a rename doesn't disturb a running process).
    /// 3. Only then SIGKILL the daemon. Never SIGTERM: the daemon treats a
    ///    graceful stop as "tear down every ControlMaster", which costs the user
    ///    a fresh 2FA login on every host. SIGKILL leaves the detached masters
    ///    running and the replacement daemon adopts them. Because the swap has
    ///    already happened, launchd's KeepAlive respawn picks up the NEW binary.
    /// 4. Delete the old bundle only after its daemon is gone.
    ///
    /// Every failure path rolls back and relaunches something, so a failed
    /// update can never leave the user with no app.
    ///
    /// `openTool` exists so the script can be executed end-to-end in a test
    /// without LaunchServices actually opening anything; production always uses
    /// the default.
    static func swapScript(appPID: Int32,
                           target: String,
                           staged: String,
                           old: String,
                           daemonPath: String?,
                           logPath: String,
                           openTool: String = "/usr/bin/open") -> String {
        let t = shQuote(target), n = shQuote(staged), o = shQuote(old)
        let killDaemon = daemonPath.map {
            "/usr/bin/pkill -9 -f \(shQuote($0)) 2>/dev/null || true"
        } ?? "true"
        return """
        #!/bin/sh
        # SSH2FA in-app update: swap the bundle once the app has quit, relaunch.
        exec >>\(shQuote(logPath)) 2>&1
        echo "--- ssh2fa update swap (app pid \(appPID)) ---"
        n=0
        while kill -0 \(appPID) 2>/dev/null; do
          n=$((n+1))
          if [ "$n" -gt 300 ]; then
            echo "app pid \(appPID) still running after 30s — aborting, nothing changed"
            exit 1
          fi
          sleep 0.1
        done
        if [ ! -d \(n) ]; then
          echo "staged bundle \(staged) is missing — aborting"
          exit 1
        fi
        rm -rf \(o)
        if [ -d \(t) ]; then
          if ! mv \(t) \(o); then
            echo "could not move the old bundle aside — aborting"
            \(openTool) \(t) 2>/dev/null || true
            exit 1
          fi
        fi
        if ! mv \(n) \(t); then
          echo "swap failed — rolling back"
          [ -d \(o) ] && mv \(o) \(t)
          \(openTool) \(t) 2>/dev/null || true
          exit 1
        fi
        # SIGKILL, never SIGTERM: a graceful daemon stop tears down every
        # ControlMaster and forces a fresh 2FA login on every host.
        \(killDaemon)
        rm -rf \(o)
        /usr/bin/xattr -dr com.apple.quarantine \(t) 2>/dev/null || true
        \(openTool) \(t)
        echo "update installed"
        """
    }

    // MARK: - Progress formatting

    /// Download progress as 0…1. An unknown total (GitHub omitted the size, or
    /// a chunked response) yields 0 so the view shows an indeterminate bar
    /// rather than a bar frozen at 100%.
    static func fraction(received: Int64, total: Int64) -> Double {
        guard total > 0, received > 0 else { return 0 }
        return min(1.0, Double(received) / Double(total))
    }

    /// "7.3 MB" — decimal megabytes, one decimal, locale-independent so the
    /// number in the progress line is stable and testable.
    static func formatBytes(_ bytes: Int64) -> String {
        if bytes < 0 { return "0 KB" }
        if bytes < 1_000_000 {
            return "\(max(0, bytes) / 1000) KB"
        }
        let mb = Double(bytes) / 1_000_000
        return String(format: "%.1f MB", mb)
    }
}
