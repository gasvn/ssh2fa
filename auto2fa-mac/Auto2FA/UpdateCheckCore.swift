import Foundation

/// Pure, dependency-free logic behind the notify-only update reminder.
///
/// All the decisions (is a tag newer? should the daily background check run?
/// should we surface a notification?) live here as static functions with no
/// I/O, so they're unit-tested headlessly. The networking + UI side-effects
/// live in `UpdateChecker` / `AppState` / `MacNotifications`.
enum UpdateCheckCore {
    /// Normalize a GitHub release tag to a comparable version string: trim
    /// surrounding whitespace and drop a single leading "v"/"V" — but only when
    /// it actually prefixes a version number, so a non-version tag ("vanity")
    /// survives untouched and `isNewer` then treats it as 0 (never nags).
    static func normalizeTag(_ tag: String) -> String {
        let t = tag.trimmingCharacters(in: .whitespacesAndNewlines)
        if let first = t.first, first == "v" || first == "V",
           let second = t.dropFirst().first, second.isNumber {
            return String(t.dropFirst())
        }
        return t
    }

    /// Compare dotted numeric versions ("1.2.10" > "1.2.9"). Missing trailing
    /// components count as 0 ("1.0" == "1.0.0"); non-numeric parts also count as
    /// 0, so a tag the parser doesn't understand is "not newer" — we never nag
    /// on garbage.
    static func isNewer(_ a: String, than b: String) -> Bool {
        let pa = a.split(separator: ".").map { Int($0) ?? 0 }
        let pb = b.split(separator: ".").map { Int($0) ?? 0 }
        let n = max(pa.count, pb.count)
        for i in 0..<n {
            let x = i < pa.count ? pa[i] : 0
            let y = i < pb.count ? pb[i] : 0
            if x != y { return x > y }
        }
        return false
    }

    /// Should an automatic background check run now? Gated by the user toggle
    /// and a minimum interval since the last check, so we hit GitHub at most
    /// once per `interval` even across many launches, wakes, and timer ticks.
    static func shouldCheckNow(enabled: Bool, lastCheck: Date?, now: Date,
                               interval: TimeInterval) -> Bool {
        guard enabled else { return false }
        guard let lastCheck else { return true }
        return now.timeIntervalSince(lastCheck) >= interval
    }

    /// Should the update be SURFACED at all (menu-bar marker + About pane)?
    /// True when it's strictly newer than the running version and not a version
    /// the user explicitly chose to skip. This is the single gate both the
    /// passive surfaces and `shouldNotify` build on.
    static func shouldSurface(latest: String, current: String, skipped: String?) -> Bool {
        guard isNewer(latest, than: current) else { return false }
        return latest != skipped
    }

    /// Should we raise a NOTIFICATION for `latest`? Everything `shouldSurface`
    /// requires, plus: we haven't already notified about this exact version (so
    /// the reminder fires once per release, not once per check).
    static func shouldNotify(latest: String, current: String,
                             lastNotified: String?, skipped: String?) -> Bool {
        guard shouldSurface(latest: latest, current: current, skipped: skipped) else { return false }
        return latest != lastNotified
    }

    /// Format a version for DISPLAY with one consistent leading "v" everywhere
    /// (menu item, notification, About pane). Empty/blank stays empty.
    static func displayVersion(_ v: String) -> String {
        let t = v.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !t.isEmpty else { return "" }
        if let f = t.first, f == "v" || f == "V" { return "v" + t.dropFirst() }
        return "v" + t
    }

    /// Copy-paste-able update commands shown in the About pane, so the update
    /// path is never a dead-end. Both end with the `xattr` de-quarantine step —
    /// the un-notarized build is quarantined on download, and skipping it is the
    /// classic "it won't open after I updated" wall.
    static let brewUpdateCommand =
        "brew reinstall --cask ssh2fa && xattr -dr com.apple.quarantine /Applications/SSH2FA.app && open /Applications/SSH2FA.app"
    static let manualUpdateCommand =
        "d=\"$(mktemp -d)\" && curl -fL --retry 3 --retry-all-errors https://github.com/gasvn/ssh2fa/releases/latest/download/SSH2FA.dmg -o \"$d/SSH2FA.dmg\" && hdiutil attach \"$d/SSH2FA.dmg\" -nobrowse -quiet -mountpoint \"$d/mnt\" && ditto \"$d/mnt/SSH2FA.app\" /Applications/SSH2FA.app && xattr -dr com.apple.quarantine /Applications/SSH2FA.app && open /Applications/SSH2FA.app; hdiutil detach \"$d/mnt\" -quiet 2>/dev/null; rm -rf \"$d\""
}
