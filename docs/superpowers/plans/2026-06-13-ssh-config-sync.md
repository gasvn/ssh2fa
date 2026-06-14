# SSH Config ↔ SSH2FA Sync — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make hosts effortless to register by importing from `~/.ssh/config`, make the app's Terminal button reuse the daemon's warm master with zero config writes, optionally make the user's own `ssh <alias>` warm via one consented `Include` line, and surface drift — all client-side, never touching the user's own `Host` blocks.

**Architecture:** Five pure Foundation-only helpers (`SSHPaths`, `SSHConfigParser`, `SSHSyncDiff`, `ControlPathResolver`, `SSHConfigManager`) compiled into the headless test bundle and unit-tested TDD-style, plus thin SwiftUI/AppKit wiring (`TerminalLauncher`, `AppState`, `AddHostSheet`, a new `ImportHostsSheet`, `Settings`, `HostRow`) verified by build + manual QA. The app keys hosts on their ssh alias; `~/.ssh/config` stays authoritative for connectivity. SSH2FA owns `~/.ssh/ssh2fa.conf` entirely and writes at most one consented `Include` line into `~/.ssh/config`.

**Tech Stack:** Swift 5.10 / SwiftUI / AppKit, XcodeGen (`project.yml`), `xcodebuild` test+build, the daemon's `ssh -G` ControlPath convention (a2fa-core `control.rs`).

---

## Reference: build & test commands

All commands run from `auto2fa-mac/`. After **any** `project.yml` change, regenerate the Xcode project first.

```bash
# Regenerate project (needed after project.yml edits, e.g. adding a test source)
cd auto2fa-mac && xcodegen generate

# Run the headless unit-test bundle
xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FATests -destination 'platform=macOS' test 2>&1 | tail -30

# Build the app target (UI tasks — verifies compilation)
xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FA -destination 'platform=macOS' build 2>&1 | tail -25
```

A single test class can be run with `-only-testing:Auto2FATests/<ClassName>`.

## File Structure

| File | Responsibility | New? | In test bundle? |
|------|----------------|------|-----------------|
| `Auto2FA/SSHPaths.swift` | Resolve ssh dir + file paths + ControlPath fallback (honors `SSH_CONFIG_PATH`) | new | yes |
| `Auto2FA/SSHConfigParser.swift` | Parse `~/.ssh/config` → `[ConfigHost]` (concrete `Host` only) | new | yes |
| `Auto2FA/SSHSyncDiff.swift` | Pure set-diff: importable hosts, unreachable registered hosts | new | yes |
| `Auto2FA/ControlPathResolver.swift` | ControlPath the daemon master binds (`ssh -G` + fallback) | new | yes |
| `Auto2FA/SSHConfigManager.swift` | Generate `ssh2fa.conf`; ensure/backup the `Include` line | new | yes |
| `Auto2FA/TerminalLauncher.swift` | Terminal button → warm-reuse ssh invocation | modify | no |
| `Auto2FA/AppState.swift` | import/reconcile computed props, sync hook, consent trigger | modify | no |
| `Auto2FA/Views/AddHostSheet.swift` | accept a pre-filled alias | modify | no |
| `Auto2FA/Views/ImportHostsSheet.swift` | list config hosts, "Enable 2FA" | new | no |
| `Auto2FA/Views/HostsView.swift` | "Add from ~/.ssh/config" entry + onboarding surface | modify | no |
| `Auto2FA/Views/Components/HostRow.swift` | drift warning badge | modify | no |
| `Auto2FA/Settings.swift` | warm-reuse enable/status + `SettingsKey` additions | modify | no |
| `Auto2FA/ContentView.swift` | route `.addHost(prefillAlias:)` + `.importHosts` sheets | modify | no |
| `Auto2FATests/*Tests.swift` | unit tests for the five pure helpers | new | (is the bundle) |
| `project.yml` | add the five pure sources to the test bundle | modify | — |

---

### Task 1: `SSHPaths` — centralized path resolution (pure)

**Files:**
- Create: `auto2fa-mac/Auto2FA/SSHPaths.swift`
- Create: `auto2fa-mac/Auto2FATests/SSHPathsTests.swift`
- Modify: `auto2fa-mac/project.yml` (add source to test bundle)

- [ ] **Step 1: Write the failing test**

Create `auto2fa-mac/Auto2FATests/SSHPathsTests.swift`:

```swift
import XCTest

final class SSHPathsTests: XCTestCase {
    func testDefaultDirIsHomeSSH() {
        XCTAssertEqual(SSHPaths.sshDir(env: [:], home: "/Users/x"), "/Users/x/.ssh")
    }

    func testEnvOverrideStripsTrailingSlash() {
        XCTAssertEqual(SSHPaths.sshDir(env: ["SSH_CONFIG_PATH": "/Users/x/.ssh/"],
                                       home: "/Users/x"), "/Users/x/.ssh")
    }

    func testEnvTildeExpansion() {
        let dir = SSHPaths.sshDir(env: ["SSH_CONFIG_PATH": "~/alt"], home: NSHomeDirectory())
        XCTAssertEqual(dir, NSHomeDirectory() + "/alt")
    }

    func testFilePaths() {
        XCTAssertEqual(SSHPaths.configFile(dir: "/d"), "/d/config")
        XCTAssertEqual(SSHPaths.managedConfFile(dir: "/d"), "/d/ssh2fa.conf")
        XCTAssertEqual(SSHPaths.backupFile(dir: "/d", timestamp: "20260613-120000"),
                       "/d/config.ssh2fa-backup-20260613-120000")
    }

    func testControlPathFallback() {
        XCTAssertEqual(SSHPaths.controlPathFallback(dir: "/Users/x/.ssh", alias: "kempner"),
                       "/Users/x/.ssh/cm-ssh2fa-kempner")
    }
}
```

- [ ] **Step 2: Register the source in the test bundle**

In `auto2fa-mac/project.yml`, under `targets: → Auto2FATests: → sources:`, add `SSHPaths.swift` right after the existing `SlurmTime.swift` line:

```yaml
      - path: Auto2FA/SlurmTime.swift
      - path: Auto2FA/SSHPaths.swift
```

- [ ] **Step 3: Run the test to verify it fails**

```bash
cd auto2fa-mac && xcodegen generate && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FATests -destination 'platform=macOS' -only-testing:Auto2FATests/SSHPathsTests test 2>&1 | tail -20
```
Expected: FAIL — `cannot find 'SSHPaths' in scope`.

- [ ] **Step 4: Write the implementation**

Create `auto2fa-mac/Auto2FA/SSHPaths.swift`:

```swift
import Foundation

/// Centralized resolution of the user's ssh directory + the files SSH2FA
/// reads/writes, honoring SSH_CONFIG_PATH exactly like the rest of the app
/// (AddHostSheet / DaemonProcess / Settings). Pure + Foundation-only so it
/// compiles into the headless test bundle.
enum SSHPaths {
    /// The ssh config directory (no trailing slash). SSH_CONFIG_PATH wins,
    /// tilde-expanded; otherwise ~/.ssh.
    static func sshDir(env: [String: String] = ProcessInfo.processInfo.environment,
                       home: String = NSHomeDirectory()) -> String {
        let raw = env["SSH_CONFIG_PATH"].map { ($0 as NSString).expandingTildeInPath }
            ?? home + "/.ssh"
        return raw.hasSuffix("/") ? String(raw.dropLast()) : raw
    }

    static func configFile(dir: String) -> String { dir + "/config" }
    static func managedConfFile(dir: String) -> String { dir + "/ssh2fa.conf" }
    static func backupFile(dir: String, timestamp: String) -> String {
        dir + "/config.ssh2fa-backup-" + timestamp
    }

    /// The ControlPath the daemon's single master falls back to when ssh config
    /// declares no `controlpath` — `<dir>/cm-ssh2fa-<alias>`. Mirrors a2fa-core
    /// `control.rs` `resolve_control_base` fallback so the app attaches to the
    /// same socket.
    static func controlPathFallback(dir: String, alias: String) -> String {
        dir + "/cm-ssh2fa-" + alias
    }
}
```

- [ ] **Step 5: Run the test to verify it passes**

```bash
cd auto2fa-mac && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FATests -destination 'platform=macOS' -only-testing:Auto2FATests/SSHPathsTests test 2>&1 | tail -20
```
Expected: PASS (5 tests).

- [ ] **Step 6: Commit**

```bash
git add auto2fa-mac/Auto2FA/SSHPaths.swift auto2fa-mac/Auto2FATests/SSHPathsTests.swift auto2fa-mac/project.yml
git commit -m "feat(mac): SSHPaths — centralized ssh-config path + ControlPath-fallback resolution"
```

---

### Task 2: `SSHConfigParser` — parse `~/.ssh/config` (pure)

**Files:**
- Create: `auto2fa-mac/Auto2FA/SSHConfigParser.swift`
- Create: `auto2fa-mac/Auto2FATests/SSHConfigParserTests.swift`
- Modify: `auto2fa-mac/project.yml`

- [ ] **Step 1: Write the failing test**

Create `auto2fa-mac/Auto2FATests/SSHConfigParserTests.swift`:

```swift
import XCTest

final class SSHConfigParserTests: XCTestCase {
    func testSingleHostWithHostNameAndUser() {
        let cfg = """
        Host kempner
            HostName login.rc.fas.harvard.edu
            User shgao
        """
        XCTAssertEqual(SSHConfigParser.parse(cfg),
                       [ConfigHost(alias: "kempner",
                                   hostName: "login.rc.fas.harvard.edu",
                                   user: "shgao")])
    }

    func testMultiAliasOnOneHostLineBothInheritDetails() {
        let cfg = """
        Host fasrc fas
            HostName boslogin.rc.fas.harvard.edu
            User u
        """
        XCTAssertEqual(SSHConfigParser.parse(cfg),
                       [ConfigHost(alias: "fasrc", hostName: "boslogin.rc.fas.harvard.edu", user: "u"),
                        ConfigHost(alias: "fas", hostName: "boslogin.rc.fas.harvard.edu", user: "u")])
    }

    func testWildcardHostsAreSkipped() {
        let cfg = """
        Host *
            ServerAliveInterval 60
        Host gh *.edu
            HostName example.edu
        """
        // `Host *` contributes nothing; `Host gh *.edu` keeps only `gh`.
        XCTAssertEqual(SSHConfigParser.parse(cfg),
                       [ConfigHost(alias: "gh", hostName: "example.edu", user: nil)])
    }

    func testCommentsAndIndentationAndCaseTolerated() {
        let cfg = """
        # a comment
          host  Box   # trailing comment
            hostname  1.2.3.4
        """
        XCTAssertEqual(SSHConfigParser.parse(cfg),
                       [ConfigHost(alias: "Box", hostName: "1.2.3.4", user: nil)])
    }

    func testEmptyInput() {
        XCTAssertEqual(SSHConfigParser.parse(""), [])
        XCTAssertEqual(SSHConfigParser.parse("\n\n# just a comment\n"), [])
    }

    func testHostWithNoDetails() {
        XCTAssertEqual(SSHConfigParser.parse("Host bare\n"),
                       [ConfigHost(alias: "bare", hostName: nil, user: nil)])
    }
}
```

- [ ] **Step 2: Register the source in the test bundle**

In `auto2fa-mac/project.yml`, add after the `SSHPaths.swift` line:

```yaml
      - path: Auto2FA/SSHConfigParser.swift
```

- [ ] **Step 3: Run the test to verify it fails**

```bash
cd auto2fa-mac && xcodegen generate && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FATests -destination 'platform=macOS' -only-testing:Auto2FATests/SSHConfigParserTests test 2>&1 | tail -20
```
Expected: FAIL — `cannot find 'SSHConfigParser'` / `'ConfigHost' in scope`.

- [ ] **Step 4: Write the implementation**

Create `auto2fa-mac/Auto2FA/SSHConfigParser.swift`:

```swift
import Foundation

/// One concrete `Host` entry parsed from ~/.ssh/config.
struct ConfigHost: Equatable, Hashable {
    let alias: String
    let hostName: String?
    let user: String?
}

/// Pure parser for ~/.ssh/config. v1: top-level concrete `Host` blocks only —
/// wildcard/glob/negated patterns (`Host *`, `Host *.edu`, `Host !x`) are
/// skipped (we never multiplex or import a pattern). Tolerant of comments +
/// indentation + key case. Does NOT follow Include/Match. Foundation-only →
/// unit-tested headlessly.
enum SSHConfigParser {
    static func parse(_ text: String) -> [ConfigHost] {
        var out: [ConfigHost] = []
        var current: [String] = []     // aliases on the open Host line
        var hostName: String?
        var user: String?

        func flush() {
            for a in current {
                out.append(ConfigHost(alias: a, hostName: hostName, user: user))
            }
            current = []; hostName = nil; user = nil
        }

        for rawLine in text.split(separator: "\n", omittingEmptySubsequences: false) {
            var line = String(rawLine)
            if let hash = line.firstIndex(of: "#") { line = String(line[..<hash]) }
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if trimmed.isEmpty { continue }
            let parts = trimmed.split(whereSeparator: { $0 == " " || $0 == "\t" })
            guard let keyword = parts.first else { continue }
            let values = parts.dropFirst().map(String.init)
            switch keyword.lowercased() {
            case "host":
                flush()
                current = values.filter {
                    !$0.contains("*") && !$0.contains("?") && !$0.hasPrefix("!")
                }
            case "hostname":
                if hostName == nil { hostName = values.first }
            case "user":
                if user == nil { user = values.first }
            default:
                break
            }
        }
        flush()
        return out
    }
}
```

- [ ] **Step 5: Run the test to verify it passes**

```bash
cd auto2fa-mac && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FATests -destination 'platform=macOS' -only-testing:Auto2FATests/SSHConfigParserTests test 2>&1 | tail -20
```
Expected: PASS (6 tests).

- [ ] **Step 6: Commit**

```bash
git add auto2fa-mac/Auto2FA/SSHConfigParser.swift auto2fa-mac/Auto2FATests/SSHConfigParserTests.swift auto2fa-mac/project.yml
git commit -m "feat(mac): SSHConfigParser — parse ~/.ssh/config Host blocks (concrete aliases only)"
```

---

### Task 3: `SSHSyncDiff` — importable & unreachable diffs (pure)

**Files:**
- Create: `auto2fa-mac/Auto2FA/SSHSyncDiff.swift`
- Create: `auto2fa-mac/Auto2FATests/SSHSyncDiffTests.swift`
- Modify: `auto2fa-mac/project.yml`

- [ ] **Step 1: Write the failing test**

Create `auto2fa-mac/Auto2FATests/SSHSyncDiffTests.swift`:

```swift
import XCTest

final class SSHSyncDiffTests: XCTestCase {
    private func host(_ a: String) -> ConfigHost { ConfigHost(alias: a, hostName: nil, user: nil) }

    func testImportableExcludesRegisteredPreservesOrder() {
        let cfg = [host("a"), host("b"), host("c")]
        XCTAssertEqual(SSHSyncDiff.importable(configHosts: cfg, registered: ["b"]),
                       [host("a"), host("c")])
    }

    func testImportableDedupesByAlias() {
        let cfg = [host("a"), host("a"), host("b")]
        XCTAssertEqual(SSHSyncDiff.importable(configHosts: cfg, registered: []),
                       [host("a"), host("b")])
    }

    func testImportableEmptyWhenAllRegistered() {
        XCTAssertEqual(SSHSyncDiff.importable(configHosts: [host("a")], registered: ["a"]), [])
    }

    func testUnreachableFindsRegisteredMissingFromConfig() {
        XCTAssertEqual(SSHSyncDiff.unreachable(registered: ["a", "b"], configAliases: ["a"]),
                       ["b"])
    }

    func testUnreachableEmptyWhenAllPresent() {
        XCTAssertEqual(SSHSyncDiff.unreachable(registered: ["a"], configAliases: ["a", "z"]), [])
    }
}
```

- [ ] **Step 2: Register the source in the test bundle**

In `auto2fa-mac/project.yml`, add after the `SSHConfigParser.swift` line:

```yaml
      - path: Auto2FA/SSHSyncDiff.swift
```

- [ ] **Step 3: Run the test to verify it fails**

```bash
cd auto2fa-mac && xcodegen generate && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FATests -destination 'platform=macOS' -only-testing:Auto2FATests/SSHSyncDiffTests test 2>&1 | tail -20
```
Expected: FAIL — `cannot find 'SSHSyncDiff' in scope`.

- [ ] **Step 4: Write the implementation**

Create `auto2fa-mac/Auto2FA/SSHSyncDiff.swift`:

```swift
import Foundation

/// Pure set-diff between the user's ssh config and SSH2FA's registered hosts.
/// Drives the import list (capability 1) and the reconciliation warning
/// (capability 4). Foundation-only → unit-tested.
enum SSHSyncDiff {
    /// Config hosts not yet registered (by alias), preserving config order,
    /// deduped by alias.
    static func importable(configHosts: [ConfigHost], registered: [String]) -> [ConfigHost] {
        let have = Set(registered)
        var seen = Set<String>()
        var out: [ConfigHost] = []
        for h in configHosts where !have.contains(h.alias) && !seen.contains(h.alias) {
            seen.insert(h.alias); out.append(h)
        }
        return out
    }

    /// Registered aliases that no longer appear as a Host in config — these
    /// cannot connect.
    static func unreachable(registered: [String], configAliases: [String]) -> [String] {
        let cfg = Set(configAliases)
        return registered.filter { !cfg.contains($0) }
    }
}
```

- [ ] **Step 5: Run the test to verify it passes**

```bash
cd auto2fa-mac && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FATests -destination 'platform=macOS' -only-testing:Auto2FATests/SSHSyncDiffTests test 2>&1 | tail -20
```
Expected: PASS (5 tests).

- [ ] **Step 6: Commit**

```bash
git add auto2fa-mac/Auto2FA/SSHSyncDiff.swift auto2fa-mac/Auto2FATests/SSHSyncDiffTests.swift auto2fa-mac/project.yml
git commit -m "feat(mac): SSHSyncDiff — importable + unreachable host diffs"
```

---

### Task 4: `ControlPathResolver` — warm-master ControlPath (pure pick + bounded spawn)

**Files:**
- Create: `auto2fa-mac/Auto2FA/ControlPathResolver.swift`
- Create: `auto2fa-mac/Auto2FATests/ControlPathResolverTests.swift`
- Modify: `auto2fa-mac/project.yml`

- [ ] **Step 1: Write the failing test** (covers the *pure* `pick`; `resolve` spawns ssh and is not unit-tested)

Create `auto2fa-mac/Auto2FATests/ControlPathResolverTests.swift`:

```swift
import XCTest

final class ControlPathResolverTests: XCTestCase {
    func testPicksExplicitControlPath() {
        let g = "user shgao\ncontrolpath /Users/x/.ssh/cm-ssh2fa-kempner\nport 22\n"
        XCTAssertEqual(ControlPathResolver.pick(fromSSHG: g, alias: "kempner", dir: "/Users/x/.ssh"),
                       "/Users/x/.ssh/cm-ssh2fa-kempner")
    }

    func testExpandsTilde() {
        let g = "controlpath ~/.ssh/cm-ssh2fa-h\n"
        XCTAssertEqual(ControlPathResolver.pick(fromSSHG: g, alias: "h", dir: "/d"),
                       NSHomeDirectory() + "/.ssh/cm-ssh2fa-h")
    }

    func testNoneFallsBack() {
        let g = "controlpath none\n"
        XCTAssertEqual(ControlPathResolver.pick(fromSSHG: g, alias: "h", dir: "/d"),
                       "/d/cm-ssh2fa-h")
    }

    func testMissingControlPathFallsBack() {
        XCTAssertEqual(ControlPathResolver.pick(fromSSHG: "user u\nport 22\n", alias: "h", dir: "/d"),
                       "/d/cm-ssh2fa-h")
    }
}
```

- [ ] **Step 2: Register the source in the test bundle**

In `auto2fa-mac/project.yml`, add after the `SSHSyncDiff.swift` line:

```yaml
      - path: Auto2FA/ControlPathResolver.swift
```

- [ ] **Step 3: Run the test to verify it fails**

```bash
cd auto2fa-mac && xcodegen generate && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FATests -destination 'platform=macOS' -only-testing:Auto2FATests/ControlPathResolverTests test 2>&1 | tail -20
```
Expected: FAIL — `cannot find 'ControlPathResolver' in scope`.

- [ ] **Step 4: Write the implementation**

Create `auto2fa-mac/Auto2FA/ControlPathResolver.swift`:

```swift
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
```

- [ ] **Step 5: Run the test to verify it passes**

```bash
cd auto2fa-mac && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FATests -destination 'platform=macOS' -only-testing:Auto2FATests/ControlPathResolverTests test 2>&1 | tail -20
```
Expected: PASS (4 tests).

- [ ] **Step 6: Commit**

```bash
git add auto2fa-mac/Auto2FA/ControlPathResolver.swift auto2fa-mac/Auto2FATests/ControlPathResolverTests.swift auto2fa-mac/project.yml
git commit -m "feat(mac): ControlPathResolver — ssh -G controlpath with bounded spawn + fallback"
```

---

### Task 5: `SSHConfigManager` — pure string transforms

**Files:**
- Create: `auto2fa-mac/Auto2FA/SSHConfigManager.swift`
- Create: `auto2fa-mac/Auto2FATests/SSHConfigManagerTests.swift`
- Modify: `auto2fa-mac/project.yml`

- [ ] **Step 1: Write the failing test** (pure transforms only)

Create `auto2fa-mac/Auto2FATests/SSHConfigManagerTests.swift`:

```swift
import XCTest

final class SSHConfigManagerTests: XCTestCase {
    func testGeneratedConfIsSortedWithCorrectControlPath() {
        let out = SSHConfigManager.generateManagedConf(aliases: ["b", "a"], dir: "/d")
        XCTAssertTrue(out.hasPrefix("# Managed by SSH2FA"))
        // sorted: a before b
        let aIdx = out.range(of: "Host a")!.lowerBound
        let bIdx = out.range(of: "Host b")!.lowerBound
        XCTAssertLessThan(aIdx, bIdx)
        XCTAssertTrue(out.contains("ControlPath /d/cm-ssh2fa-a"))
        XCTAssertTrue(out.contains("ControlMaster auto"))
        XCTAssertTrue(out.contains("ControlPersist yes"))
    }

    func testHasIncludeDetectsBareLine() {
        XCTAssertTrue(SSHConfigManager.hasInclude("Host x\nInclude ssh2fa.conf\n"))
        XCTAssertFalse(SSHConfigManager.hasInclude("Host x\n  User u\n"))
    }

    func testEnsureIncludePutsRegionAtTop() {
        let out = SSHConfigManager.ensureInclude(in: "Host kempner\n    User shgao\n")
        XCTAssertTrue(out.hasPrefix(SSHConfigManager.beginMarker))
        XCTAssertTrue(out.contains("Include ssh2fa.conf"))
        XCTAssertTrue(out.contains("Host kempner"))   // user block preserved
    }

    func testEnsureIncludeIsIdempotent() {
        let once = SSHConfigManager.ensureInclude(in: "Host k\n")
        let twice = SSHConfigManager.ensureInclude(in: once)
        XCTAssertEqual(once, twice)
        // exactly one include line
        XCTAssertEqual(twice.components(separatedBy: "Include ssh2fa.conf").count - 1, 1)
    }

    func testEnsureIncludeNormalizesAPreexistingBareInclude() {
        let input = "Include ssh2fa.conf\nHost k\n"
        let out = SSHConfigManager.ensureInclude(in: input)
        XCTAssertEqual(out.components(separatedBy: "Include ssh2fa.conf").count - 1, 1)
        XCTAssertTrue(out.hasPrefix(SSHConfigManager.beginMarker))
    }

    func testEnsureIncludeOnEmptyConfig() {
        let out = SSHConfigManager.ensureInclude(in: "")
        XCTAssertEqual(out, "\(SSHConfigManager.beginMarker)\nInclude ssh2fa.conf\n\(SSHConfigManager.endMarker)\n")
    }
}
```

- [ ] **Step 2: Register the source in the test bundle**

In `auto2fa-mac/project.yml`, add after the `ControlPathResolver.swift` line:

```yaml
      - path: Auto2FA/SSHConfigManager.swift
```

- [ ] **Step 3: Run the test to verify it fails**

```bash
cd auto2fa-mac && xcodegen generate && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FATests -destination 'platform=macOS' -only-testing:Auto2FATests/SSHConfigManagerTests test 2>&1 | tail -20
```
Expected: FAIL — `cannot find 'SSHConfigManager' in scope`.

- [ ] **Step 4: Write the implementation** (pure transforms + FS methods together; FS methods are tested in Task 6)

Create `auto2fa-mac/Auto2FA/SSHConfigManager.swift`:

```swift
import Foundation

/// Owns ~/.ssh/ssh2fa.conf (per-registered-host ControlMaster blocks) and the
/// single managed `Include ssh2fa.conf` line in ~/.ssh/config. Pure string
/// transforms (generate / detect / insert) are unit-tested; FS methods take an
/// explicit `dir` so they're temp-dir-tested.
enum SSHConfigManager {
    static let beginMarker = "# >>> SSH2FA managed (Include) >>>"
    static let endMarker   = "# <<< SSH2FA managed (Include) <<<"
    static let includeLine = "Include ssh2fa.conf"

    // MARK: - Pure transforms

    /// The full ssh2fa.conf body for a set of aliases (sorted → stable output).
    /// Per-host ControlPath = the daemon's fallback path so daemon + clients
    /// agree on one socket and enabling the Include never rebuilds a master.
    static func generateManagedConf(aliases: [String], dir: String) -> String {
        let header = "# Managed by SSH2FA — do not edit. Regenerated on host add/remove.\n"
        let blocks = aliases.sorted().map { alias -> String in
            let cp = SSHPaths.controlPathFallback(dir: dir, alias: alias)
            return """
            Host \(alias)
                ControlMaster auto
                ControlPath \(cp)
                ControlPersist yes
            """
        }
        return header + "\n" + blocks.joined(separator: "\n\n") + (blocks.isEmpty ? "" : "\n")
    }

    /// True if the config text already contains an `Include ssh2fa.conf` line
    /// (marked region OR a bare line).
    static func hasInclude(_ configText: String) -> Bool {
        for raw in configText.split(separator: "\n") {
            if raw.trimmingCharacters(in: .whitespaces).lowercased() == includeLine.lowercased() {
                return true
            }
        }
        return false
    }

    /// Idempotently ensure the marked Include region sits at the TOP of the
    /// config. Any pre-existing managed region or bare include line is removed
    /// first, so re-running yields identical output.
    static func ensureInclude(in configText: String) -> String {
        var kept: [String] = []
        var inRegion = false
        for raw in configText.split(separator: "\n", omittingEmptySubsequences: false) {
            let line = String(raw)
            let t = line.trimmingCharacters(in: .whitespaces)
            if t == beginMarker { inRegion = true; continue }
            if t == endMarker { inRegion = false; continue }
            if inRegion { continue }
            if t.lowercased() == includeLine.lowercased() { continue }
            kept.append(line)
        }
        while kept.first?.trimmingCharacters(in: .whitespaces).isEmpty == true { kept.removeFirst() }
        while kept.last?.trimmingCharacters(in: .whitespaces).isEmpty == true { kept.removeLast() }
        let region = "\(beginMarker)\n\(includeLine)\n\(endMarker)\n"
        if kept.isEmpty { return region }
        return region + "\n" + kept.joined(separator: "\n") + "\n"
    }

    // MARK: - Filesystem (dir-parameterized for testability)

    /// Resolve a symlinked path to its target (so we back up + write THROUGH the
    /// link, never replacing the symlink with a regular file).
    static func realPath(_ path: String) -> String {
        guard let dest = try? FileManager.default.destinationOfSymbolicLink(atPath: path) else {
            return path
        }
        return dest.hasPrefix("/") ? dest
            : (path as NSString).deletingLastPathComponent + "/" + dest
    }

    /// Write ssh2fa.conf for `aliases` into `dir` (perms 600). Idempotent: skips
    /// the write when content is unchanged. Returns true iff a write happened.
    @discardableResult
    static func writeManagedConf(aliases: [String], dir: String) throws -> Bool {
        let path = SSHPaths.managedConfFile(dir: dir)
        let content = generateManagedConf(aliases: aliases, dir: dir)
        if let existing = try? String(contentsOfFile: path, encoding: .utf8), existing == content {
            return false
        }
        try atomicWrite(content, to: path, perms: 0o600)
        return true
    }

    /// Add the Include to ~/.ssh/config in `dir` after backing the file up.
    /// Creates config if missing. Idempotent. `timestamp` is injected so the
    /// backup name is deterministic/testable.
    static func enableInclude(dir: String, timestamp: String) throws {
        let cfgPath = realPath(SSHPaths.configFile(dir: dir))
        let original = (try? String(contentsOfFile: cfgPath, encoding: .utf8)) ?? ""
        if !original.isEmpty {
            try original.write(toFile: SSHPaths.backupFile(dir: dir, timestamp: timestamp),
                               atomically: true, encoding: .utf8)
        }
        try atomicWrite(ensureInclude(in: original), to: cfgPath, perms: 0o600)
    }

    /// Remove the managed Include region (revert) and delete ssh2fa.conf.
    static func disableInclude(dir: String) throws {
        let cfgPath = realPath(SSHPaths.configFile(dir: dir))
        if let original = try? String(contentsOfFile: cfgPath, encoding: .utf8) {
            var kept: [String] = []
            var inRegion = false
            for raw in original.split(separator: "\n", omittingEmptySubsequences: false) {
                let t = raw.trimmingCharacters(in: .whitespaces)
                if t == beginMarker { inRegion = true; continue }
                if t == endMarker { inRegion = false; continue }
                if inRegion { continue }
                if t.lowercased() == includeLine.lowercased() { continue }
                kept.append(String(raw))
            }
            while kept.first?.trimmingCharacters(in: .whitespaces).isEmpty == true { kept.removeFirst() }
            try atomicWrite(kept.joined(separator: "\n") + (kept.isEmpty ? "" : "\n"),
                            to: cfgPath, perms: 0o600)
        }
        try? FileManager.default.removeItem(atPath: SSHPaths.managedConfFile(dir: dir))
    }

    private static func atomicWrite(_ content: String, to path: String, perms: Int) throws {
        let tmp = path + ".ssh2fa-tmp"
        try content.write(toFile: tmp, atomically: false, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: perms], ofItemAtPath: tmp)
        if FileManager.default.fileExists(atPath: path) {
            _ = try FileManager.default.replaceItemAt(URL(fileURLWithPath: path),
                                                      withItemAt: URL(fileURLWithPath: tmp))
        } else {
            try FileManager.default.moveItem(atPath: tmp, toPath: path)
        }
    }
}
```

- [ ] **Step 5: Run the test to verify it passes**

```bash
cd auto2fa-mac && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FATests -destination 'platform=macOS' -only-testing:Auto2FATests/SSHConfigManagerTests test 2>&1 | tail -20
```
Expected: PASS (6 tests).

- [ ] **Step 6: Commit**

```bash
git add auto2fa-mac/Auto2FA/SSHConfigManager.swift auto2fa-mac/Auto2FATests/SSHConfigManagerTests.swift auto2fa-mac/project.yml
git commit -m "feat(mac): SSHConfigManager — generate ssh2fa.conf + idempotent managed Include region"
```

---

### Task 6: `SSHConfigManager` filesystem behavior (temp-dir tests)

**Files:**
- Modify: `auto2fa-mac/Auto2FATests/SSHConfigManagerTests.swift` (add FS tests)

- [ ] **Step 1: Write the failing FS tests**

Append these methods inside the `SSHConfigManagerTests` class in `auto2fa-mac/Auto2FATests/SSHConfigManagerTests.swift`:

```swift
    private func tempDir() -> String {
        let d = NSTemporaryDirectory() + "ssh2fa-test-" + UUID().uuidString
        try? FileManager.default.createDirectory(atPath: d, withIntermediateDirectories: true)
        return d
    }

    func testWriteManagedConfCreatesFileWithPerms() throws {
        let dir = tempDir()
        let wrote = try SSHConfigManager.writeManagedConf(aliases: ["k"], dir: dir)
        XCTAssertTrue(wrote)
        let path = SSHPaths.managedConfFile(dir: dir)
        XCTAssertTrue(FileManager.default.fileExists(atPath: path))
        let attrs = try FileManager.default.attributesOfItem(atPath: path)
        XCTAssertEqual((attrs[.posixPermissions] as? NSNumber)?.intValue, 0o600)
    }

    func testWriteManagedConfSkipsUnchanged() throws {
        let dir = tempDir()
        XCTAssertTrue(try SSHConfigManager.writeManagedConf(aliases: ["k"], dir: dir))
        XCTAssertFalse(try SSHConfigManager.writeManagedConf(aliases: ["k"], dir: dir))
    }

    func testEnableIncludeBacksUpAndAddsRegion() throws {
        let dir = tempDir()
        let cfg = SSHPaths.configFile(dir: dir)
        try "Host kempner\n    User shgao\n".write(toFile: cfg, atomically: true, encoding: .utf8)
        try SSHConfigManager.enableInclude(dir: dir, timestamp: "TS")
        let after = try String(contentsOfFile: cfg, encoding: .utf8)
        XCTAssertTrue(after.hasPrefix(SSHConfigManager.beginMarker))
        XCTAssertTrue(after.contains("Host kempner"))
        let backup = try String(contentsOfFile: SSHPaths.backupFile(dir: dir, timestamp: "TS"),
                                encoding: .utf8)
        XCTAssertEqual(backup, "Host kempner\n    User shgao\n")
    }

    func testEnableIncludeCreatesMissingConfig() throws {
        let dir = tempDir()
        try SSHConfigManager.enableInclude(dir: dir, timestamp: "TS")
        let after = try String(contentsOfFile: SSHPaths.configFile(dir: dir), encoding: .utf8)
        XCTAssertTrue(after.contains("Include ssh2fa.conf"))
        // No original content → no backup file.
        XCTAssertFalse(FileManager.default.fileExists(
            atPath: SSHPaths.backupFile(dir: dir, timestamp: "TS")))
    }

    func testEnableIncludeTwiceIsStable() throws {
        let dir = tempDir()
        let cfg = SSHPaths.configFile(dir: dir)
        try "Host k\n".write(toFile: cfg, atomically: true, encoding: .utf8)
        try SSHConfigManager.enableInclude(dir: dir, timestamp: "T1")
        let firstPass = try String(contentsOfFile: cfg, encoding: .utf8)
        try SSHConfigManager.enableInclude(dir: dir, timestamp: "T2")
        let secondPass = try String(contentsOfFile: cfg, encoding: .utf8)
        XCTAssertEqual(firstPass, secondPass)
        XCTAssertEqual(secondPass.components(separatedBy: "Include ssh2fa.conf").count - 1, 1)
    }

    func testDisableIncludeRevertsAndRemovesConf() throws {
        let dir = tempDir()
        let cfg = SSHPaths.configFile(dir: dir)
        try "Host k\n".write(toFile: cfg, atomically: true, encoding: .utf8)
        try SSHConfigManager.writeManagedConf(aliases: ["k"], dir: dir)
        try SSHConfigManager.enableInclude(dir: dir, timestamp: "T1")
        try SSHConfigManager.disableInclude(dir: dir)
        let after = try String(contentsOfFile: cfg, encoding: .utf8)
        XCTAssertFalse(after.contains("Include ssh2fa.conf"))
        XCTAssertTrue(after.contains("Host k"))
        XCTAssertFalse(FileManager.default.fileExists(atPath: SSHPaths.managedConfFile(dir: dir)))
    }
```

- [ ] **Step 2: Run the FS tests to verify they pass** (implementation already exists from Task 5)

```bash
cd auto2fa-mac && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FATests -destination 'platform=macOS' -only-testing:Auto2FATests/SSHConfigManagerTests test 2>&1 | tail -20
```
Expected: PASS (12 tests total — 6 pure + 6 FS). If a perms assertion fails, confirm `setAttributes` ran before the rename in `atomicWrite`.

- [ ] **Step 3: Run the FULL suite to confirm nothing regressed**

```bash
cd auto2fa-mac && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FATests -destination 'platform=macOS' test 2>&1 | tail -25
```
Expected: PASS (all suites: SyncCore, SearchFilter, RecentNodes, SlurmTime, SSHPaths, SSHConfigParser, SSHSyncDiff, ControlPathResolver, SSHConfigManager).

- [ ] **Step 4: Commit**

```bash
git add auto2fa-mac/Auto2FATests/SSHConfigManagerTests.swift
git commit -m "test(mac): SSHConfigManager filesystem behavior — write/backup/idempotency/revert"
```

---

### Task 7: Terminal button warm-reuse

**Files:**
- Modify: `auto2fa-mac/Auto2FA/TerminalLauncher.swift`

- [ ] **Step 1: Change the launch path to resolve + pass the ControlPath, off the main thread**

In `auto2fa-mac/Auto2FA/TerminalLauncher.swift`, replace the `openSSH(host:)` method and the `launch(host:choice:)` signature/body. New `openSSH`:

```swift
    /// Open `ssh <host>` in the chosen terminal, attaching to the daemon's warm
    /// master so there's no second 2FA prompt. First call (preference empty)
    /// shows the one-time picker (on the main thread); the `ssh -G` ControlPath
    /// resolution runs OFF the main thread (it can be slow / wedge).
    static func openSSH(host: String) {
        let stored = UserDefaults.standard.string(forKey: prefKey) ?? ""
        let choice: String
        if stored.isEmpty {
            guard let picked = promptForChoice() else { return }  // dismissed
            UserDefaults.standard.set(picked, forKey: prefKey)
            choice = picked
        } else {
            choice = stored
        }
        DispatchQueue.global(qos: .userInitiated).async {
            let controlPath = ControlPathResolver.resolve(alias: host)
            DispatchQueue.main.async { launch(host: host, choice: choice, controlPath: controlPath) }
        }
    }
```

- [ ] **Step 2: Update `launch` to bake the ControlPath into the command**

Replace the `launch` method body's `path`/`body` lines. The new signature + body:

```swift
    private static func launch(host: String, choice: String, controlPath: String) {
        // Defense-in-depth: the daemon restricts host names to [A-Za-z0-9._-],
        // so both the filename and the shell literal are safe; escape anyway.
        let safeHost = host
            .replacingOccurrences(of: "\\", with: "\\\\")
            .replacingOccurrences(of: "\"", with: "\\\"")
        let safeCP = controlPath
            .replacingOccurrences(of: "\\", with: "\\\\")
            .replacingOccurrences(of: "\"", with: "\\\"")
        let path = "/tmp/ssh2fa-\(host).command"
        // ControlMaster=no → attach to the live master only, never try to BECOME
        // one from the terminal. If no socket exists ssh just opens a normal
        // connection. ControlPath matches what the daemon's master binds.
        let body = "#!/bin/bash\nexec ssh -o ControlMaster=no -o ControlPath=\"\(safeCP)\" \"\(safeHost)\"\n"
        do {
            try body.write(toFile: path, atomically: true, encoding: .utf8)
            try FileManager.default.setAttributes([.posixPermissions: 0o755],
                                                  ofItemAtPath: path)
            let fileURL = URL(fileURLWithPath: path)
            if choice != "system",
               let appURL = NSWorkspace.shared.urlForApplication(withBundleIdentifier: choice) {
                NSWorkspace.shared.open([fileURL], withApplicationAt: appURL,
                                        configuration: NSWorkspace.OpenConfiguration())
            } else {
                NSWorkspace.shared.open(fileURL)  // system default .command handler
            }
            NSLog("[SSH2FA] openSSH host=\(host) via=\(choice.isEmpty ? "default" : choice) cp=\(controlPath)")
        } catch {
            NSLog("[SSH2FA] openSSH failed: \(error.localizedDescription)")
        }
    }
```

- [ ] **Step 3: Build the app target**

```bash
cd auto2fa-mac && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FA -destination 'platform=macOS' build 2>&1 | tail -25
```
Expected: **BUILD SUCCEEDED**.

- [ ] **Step 4: Commit**

```bash
git add auto2fa-mac/Auto2FA/TerminalLauncher.swift
git commit -m "feat(mac): Terminal button attaches to the daemon's warm master (no config write)"
```

---

### Task 8: Pre-fill the Add-Host wizard with an alias

**Files:**
- Modify: `auto2fa-mac/Auto2FA/AppState.swift` (ActiveSheet case + presentAddHost)
- Modify: `auto2fa-mac/Auto2FA/ContentView.swift` (route the prefill)
- Modify: `auto2fa-mac/Auto2FA/Views/AddHostSheet.swift` (accept prefill)

- [ ] **Step 1: Carry an optional prefill alias on the `.addHost` sheet case**

In `auto2fa-mac/Auto2FA/AppState.swift`, change the enum case and its `id`:

```swift
    case addHost(prefillAlias: String?)
```
and in the `id` switch:
```swift
        case .addHost(let a): return "addHost:\(a ?? "")"
```

- [ ] **Step 2: Update the presenter**

In `auto2fa-mac/Auto2FA/AppState.swift`, replace `func presentAddHost()`:

```swift
    func presentAddHost(prefillAlias: String? = nil) { activeSheet = .addHost(prefillAlias: prefillAlias) }
```

- [ ] **Step 3: Route the prefill in ContentView**

In `auto2fa-mac/Auto2FA/ContentView.swift`, change the `.addHost` arm of `sheetContent(for:)` (around line 154):

```swift
        case .addHost(let alias):
            AddHostSheet(prefillAlias: alias).environmentObject(appState)
```

(The other `case .newTunnel, .nodePicker, .customNode, .addHost:` pattern around line 203 needs no change — listing `.addHost` without binding still matches any payload.)

- [ ] **Step 4: Accept the prefill in AddHostSheet**

In `auto2fa-mac/Auto2FA/Views/AddHostSheet.swift`, add an initializer above `body` and seed `hostname` from it. Change the `@State private var hostname = ""` to be seeded via init by adding (just after the `struct AddHostSheet: View {` line and its `@EnvironmentObject` / `@State` declarations) an explicit init:

```swift
    let prefillAlias: String?

    init(prefillAlias: String? = nil) {
        self.prefillAlias = prefillAlias
        _hostname = State(initialValue: prefillAlias ?? "")
    }
```

(Leave the existing `@State private var hostname = ""` declaration as-is — the `_hostname = State(initialValue:)` in init overrides the default. Swift permits both.)

- [ ] **Step 5: Build the app target**

```bash
cd auto2fa-mac && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FA -destination 'platform=macOS' build 2>&1 | tail -25
```
Expected: **BUILD SUCCEEDED**.

- [ ] **Step 6: Commit**

```bash
git add auto2fa-mac/Auto2FA/AppState.swift auto2fa-mac/Auto2FA/ContentView.swift auto2fa-mac/Auto2FA/Views/AddHostSheet.swift
git commit -m "feat(mac): Add-Host wizard accepts a pre-filled ssh alias"
```

---

### Task 9: AppState — config-host computed properties + sync hook + SettingsKeys

**Files:**
- Modify: `auto2fa-mac/Auto2FA/Settings.swift` (SettingsKey additions)
- Modify: `auto2fa-mac/Auto2FA/AppState.swift` (computed props, sync hook)

> **Ordering note:** the `.importHosts` sheet case and `presentImport()` are intentionally added in Task 10, together with the switch arms that handle them — adding the enum case here would make ContentView's `ActiveSheet` switches non-exhaustive and break this task's build.

- [ ] **Step 1: Add the warm-reuse SettingsKeys**

In `auto2fa-mac/Auto2FA/Settings.swift`, find the `SettingsKey` definition (it already has `terminalApp = "auto2fa.terminalApp"`) and add two keys alongside it:

```swift
    static let warmReuseEnabled = "auto2fa.warmReuseInclude"
    static let warmReuseAsked   = "auto2fa.warmReuseAsked"
```

- [ ] **Step 2: Add computed properties + sync hook**

In `auto2fa-mac/Auto2FA/AppState.swift`, add these methods to the `AppState` class (e.g. right after `func presentAddHost`):

```swift
    /// Hosts parsed from ~/.ssh/config (concrete Host blocks).
    var configHosts: [ConfigHost] {
        let dir = SSHPaths.sshDir()
        let text = (try? String(contentsOfFile: SSHPaths.configFile(dir: dir), encoding: .utf8)) ?? ""
        return SSHConfigParser.parse(text)
    }

    /// Config hosts not yet registered — fuel for the import sheet.
    var importableHosts: [ConfigHost] {
        SSHSyncDiff.importable(configHosts: configHosts, registered: hosts.map { $0.host })
    }

    /// Registered hosts that vanished from ~/.ssh/config — they can't connect.
    var unreachableRegisteredHosts: [String] {
        SSHSyncDiff.unreachable(registered: hosts.map { $0.host },
                                configAliases: configHosts.map { $0.alias })
    }

    /// Regenerate ssh2fa.conf from the live host list — only when the user has
    /// opted into warm reuse. No-op otherwise. Safe to call on every reload
    /// (writeManagedConf skips unchanged content).
    func syncSSHConfigIfEnabled() {
        guard UserDefaults.standard.bool(forKey: SettingsKey.warmReuseEnabled) else { return }
        try? SSHConfigManager.writeManagedConf(aliases: hosts.map { $0.host }, dir: SSHPaths.sshDir())
    }
```

- [ ] **Step 3: Call the sync hook after the host list is refreshed**

In `auto2fa-mac/Auto2FA/AppState.swift`, in `func reloadAll()` (line ~200), after the host list is assigned and after the existing `checkDeadlines()`/`updateDockBadge` calls, add:

```swift
        syncSSHConfigIfEnabled()
```

(If `reloadAll` already ends with deadline/badge bookkeeping, append this as the last line of the method.)

- [ ] **Step 4: Build the app target**

```bash
cd auto2fa-mac && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FA -destination 'platform=macOS' build 2>&1 | tail -25
```
Expected: **BUILD SUCCEEDED**.

- [ ] **Step 5: Commit**

```bash
git add auto2fa-mac/Auto2FA/AppState.swift auto2fa-mac/Auto2FA/Settings.swift
git commit -m "feat(mac): AppState config-host diffs + ssh2fa.conf sync hook (opt-in)"
```

---

### Task 10: Import sheet + entry points (onboarding centerpiece)

**Files:**
- Modify: `auto2fa-mac/Auto2FA/AppState.swift` (`.importHosts` sheet case + `presentImport()`)
- Create: `auto2fa-mac/Auto2FA/Views/ImportHostsSheet.swift`
- Modify: `auto2fa-mac/Auto2FA/ContentView.swift` (route `.importHosts`)
- Modify: `auto2fa-mac/Auto2FA/Views/HostsView.swift` ("Add from ~/.ssh/config" + empty-state surface)

> All four files are committed together (Step 6) so the new enum case and every switch over `ActiveSheet` stay exhaustive in the same commit.

- [ ] **Step 1: Add the `.importHosts` sheet case + presenter**

In `auto2fa-mac/Auto2FA/AppState.swift`, add to the `ActiveSheet` enum:

```swift
    case importHosts
```
and to its `id` switch:
```swift
        case .importHosts: return "importHosts"
```
and add the presenter method to the `AppState` class (next to `presentImport`'s siblings, e.g. after `presentAddHost`):
```swift
    func presentImport() { activeSheet = .importHosts }
```

- [ ] **Step 2: Create the import sheet**

Create `auto2fa-mac/Auto2FA/Views/ImportHostsSheet.swift`:

```swift
import SwiftUI

/// Lists `Host` entries from ~/.ssh/config that aren't 2FA-enabled yet. Each
/// "Enable 2FA" opens the Add-Host wizard pre-filled with that alias — the user
/// only enters the password + TOTP.
struct ImportHostsSheet: View {
    @EnvironmentObject var appState: AppState

    var body: some View {
        VStack(alignment: .leading, spacing: Spacing.l) {
            HStack(alignment: .firstTextBaseline) {
                Text("Add from ~/.ssh/config").font(.dashTitle)
                Spacer()
            }
            let hosts = appState.importableHosts
            if hosts.isEmpty {
                Text("Every host in your ~/.ssh/config is already 2FA-enabled, or your config has no Host entries.")
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .center)
                    .padding(.vertical, Spacing.xl)
            } else {
                ScrollView {
                    VStack(spacing: Spacing.xs) {
                        ForEach(hosts, id: \.alias) { h in
                            HStack {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(h.alias).fontDesign(.monospaced)
                                    if let host = h.hostName {
                                        Text(host + (h.user.map { " · \($0)" } ?? ""))
                                            .font(.caption).foregroundStyle(.secondary)
                                    }
                                }
                                Spacer()
                                Button {
                                    appState.presentAddHost(prefillAlias: h.alias)
                                } label: {
                                    Label("Enable 2FA", systemImage: "lock.shield")
                                }
                                .buttonStyle(.borderedProminent)
                                .controlSize(.small)
                            }
                            .padding(Spacing.s)
                            .groupedContent(cornerRadius: Radius.control)
                        }
                    }
                }
                .frame(minHeight: 200, maxHeight: 360)
            }
            HStack {
                Spacer()
                Button("Done") { appState.dismissSheet() }
                    .keyboardShortcut(.defaultAction)
            }
        }
        .padding(Spacing.xl)
        .frame(width: 560)
    }
}
```

(Uses existing design tokens `Spacing`, `Radius`, `.dashTitle`, `.groupedContent` — confirm they're the same names used by `NodePickerSheet.swift`. They are.)

- [ ] **Step 3: Route the sheet**

In `auto2fa-mac/Auto2FA/ContentView.swift`, add an arm to `sheetContent(for:)`:

```swift
        case .importHosts:
            ImportHostsSheet().environmentObject(appState)
```

And add `.importHosts` to the list pattern around line 203 (`case .newTunnel, .nodePicker, .customNode, .addHost, .importHosts:`).

- [ ] **Step 4: Add the entry points in HostsView**

In `auto2fa-mac/Auto2FA/Views/HostsView.swift`, next to the existing "Add Host" button, add an "Add from ~/.ssh/config" button:

```swift
                Button {
                    appState.presentImport()
                } label: {
                    Label("Add from ~/.ssh/config", systemImage: "square.and.arrow.down")
                }
                .buttonStyle(.glass)
```

And in the hosts empty-state (when `appState.hosts.isEmpty`), surface the import as the primary call to action — add above/with the existing empty text:

```swift
                if !appState.importableHosts.isEmpty {
                    Button {
                        appState.presentImport()
                    } label: {
                        Label("Found \(appState.importableHosts.count) host(s) in ~/.ssh/config — pick which to protect",
                              systemImage: "sparkles")
                    }
                    .buttonStyle(.glassProminent)
                }
```

(Match the surrounding view's existing button-style idiom — `HostsView` already uses `.buttonStyle(.glass)` / `.glassProminent` for its section buttons; mirror whatever it uses.)

- [ ] **Step 5: Build the app target**

```bash
cd auto2fa-mac && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FA -destination 'platform=macOS' build 2>&1 | tail -25
```
Expected: **BUILD SUCCEEDED**. If `.dashTitle`/`Spacing`/`Radius`/`.groupedContent` are unresolved, open `NodePickerSheet.swift` and copy the exact token names it uses.

- [ ] **Step 6: Manual QA**

Run the app. With at least one un-registered `Host` in `~/.ssh/config`:
1. Empty host list shows the "Found N host(s)…" prominent button.
2. Clicking "Add from ~/.ssh/config" lists the un-registered hosts with hostname/user.
3. "Enable 2FA" opens the wizard with the alias pre-filled; only password + TOTP remain.
4. After enabling, that host disappears from the import list.

- [ ] **Step 7: Commit**

```bash
git add auto2fa-mac/Auto2FA/AppState.swift auto2fa-mac/Auto2FA/Views/ImportHostsSheet.swift auto2fa-mac/Auto2FA/ContentView.swift auto2fa-mac/Auto2FA/Views/HostsView.swift
git commit -m "feat(mac): import hosts from ~/.ssh/config — onboarding centerpiece + wizard prefill"
```

---

### Task 11: One-time warm-reuse consent + Settings control

**Files:**
- Create: `auto2fa-mac/Auto2FA/WarmReuseConsent.swift` (AppKit alert + apply/revert)
- Modify: `auto2fa-mac/Auto2FA/AppState.swift` (trigger after first host)
- Modify: `auto2fa-mac/Auto2FA/Settings.swift` (enable/disable control + status)

- [ ] **Step 1: Create the consent helper**

Create `auto2fa-mac/Auto2FA/WarmReuseConsent.swift`:

```swift
import AppKit
import Foundation

/// The one-time "make `ssh <alias>` in your own Terminal skip 2FA?" consent and
/// the apply/revert of the managed `Include`. Keeps the AppKit alert + file
/// writes out of the SwiftUI views.
enum WarmReuseConsent {
    /// Show the consent once, right after the first host is enabled. No-op if
    /// already enabled or already asked. Returns immediately; applies on accept.
    @MainActor static func offerIfNeeded(currentAliases: [String]) {
        let d = UserDefaults.standard
        if d.bool(forKey: SettingsKey.warmReuseEnabled) { return }
        if d.bool(forKey: SettingsKey.warmReuseAsked) { return }
        d.set(true, forKey: SettingsKey.warmReuseAsked)

        let alert = NSAlert()
        alert.messageText = "Make `ssh <host>` in your own Terminal skip the 2FA prompt too?"
        alert.informativeText = "SSH2FA backs up your SSH config and adds one `Include` line — it never touches your existing hosts. The app’s own “Open Terminal” already reuses the connection without this."
        alert.addButton(withTitle: "Set it up")
        alert.addButton(withTitle: "Not now")
        let resp = alert.runModal()
        guard resp == .alertFirstButtonReturn else { return }   // "Not now" → leave off, never nag
        apply(currentAliases: currentAliases)
    }

    /// Enable warm reuse: write ssh2fa.conf for the current hosts + add the
    /// Include (with backup). Flips the persisted flag on success.
    static func apply(currentAliases: [String]) {
        let dir = SSHPaths.sshDir()
        do {
            try SSHConfigManager.writeManagedConf(aliases: currentAliases, dir: dir)
            try SSHConfigManager.enableInclude(dir: dir, timestamp: timestamp())
            UserDefaults.standard.set(true, forKey: SettingsKey.warmReuseEnabled)
        } catch {
            NSLog("[SSH2FA] warm-reuse apply failed: \(error.localizedDescription)")
        }
    }

    /// Revert: remove the Include + ssh2fa.conf, clear the flag.
    static func revert() {
        do {
            try SSHConfigManager.disableInclude(dir: SSHPaths.sshDir())
            UserDefaults.standard.set(false, forKey: SettingsKey.warmReuseEnabled)
        } catch {
            NSLog("[SSH2FA] warm-reuse revert failed: \(error.localizedDescription)")
        }
    }

    private static func timestamp() -> String {
        let f = DateFormatter()
        f.dateFormat = "yyyyMMdd-HHmmss"
        return f.string(from: Date())
    }
}
```

- [ ] **Step 2: Trigger the consent after the first successful host add**

In `auto2fa-mac/Auto2FA/AppState.swift`, in `func addHost(...)`, in the success branch (after `await reloadAll()` and before `return nil`), add:

```swift
            WarmReuseConsent.offerIfNeeded(currentAliases: hosts.map { $0.host })
```

- [ ] **Step 3: Add the Settings control**

In `auto2fa-mac/Auto2FA/Settings.swift`, add a "Warm connection reuse" section. Use an `@AppStorage` flag and Enable/Disable buttons (a section near the existing Terminal Picker section):

```swift
            Section("Warm connection reuse") {
                let enabled = UserDefaults.standard.bool(forKey: SettingsKey.warmReuseEnabled)
                Text(enabled
                     ? "On — `ssh <host>` in your own Terminal reuses SSH2FA's connection (one `Include` line in ~/.ssh/config)."
                     : "Off — the app’s “Open Terminal” still reuses the connection; this also makes your own `ssh <host>` skip 2FA.")
                    .font(.caption).foregroundStyle(.secondary)
                if enabled {
                    Button("Turn off & remove the Include") { WarmReuseConsent.revert() }
                } else {
                    Button("Turn on (backs up config, adds one Include line)") {
                        WarmReuseConsent.apply(currentAliases: [])
                        // Re-sync from the live host list happens on next reloadAll.
                    }
                }
            }
```

(If `Settings.swift` is a SwiftUI `Form`/`Section` view this drops straight in. If it reads flags via `@AppStorage`, prefer `@AppStorage(SettingsKey.warmReuseEnabled) var enabled = false` for live updates. Match the file's existing style.)

- [ ] **Step 4: Build the app target**

```bash
cd auto2fa-mac && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FA -destination 'platform=macOS' build 2>&1 | tail -25
```
Expected: **BUILD SUCCEEDED**.

- [ ] **Step 5: Manual QA**

1. Fresh defaults (`defaults delete com.ssh2fa.app auto2fa.warmReuseAsked; defaults delete com.ssh2fa.app auto2fa.warmReuseInclude`), enable the first host → the consent alert appears once.
2. "Set it up" → `~/.ssh/config` gains the marked `Include` region at the top; a `config.ssh2fa-backup-<ts>` exists; `~/.ssh/ssh2fa.conf` lists the host with `ControlPath …/cm-ssh2fa-<alias>`.
3. From a plain Terminal, `ssh <alias>` attaches to the warm master with no 2FA prompt (host must be connected in the app).
4. Add a second host → ssh2fa.conf regenerates to include it (on reload), no re-prompt of consent.
5. Settings → "Turn off & remove the Include" → the region is gone, ssh2fa.conf deleted, existing Host blocks intact.

- [ ] **Step 6: Commit**

```bash
git add auto2fa-mac/Auto2FA/WarmReuseConsent.swift auto2fa-mac/Auto2FA/AppState.swift auto2fa-mac/Auto2FA/Settings.swift
git commit -m "feat(mac): one-time warm-reuse consent + Settings enable/disable for the managed Include"
```

---

### Task 12: Drift warning on the host row

**Files:**
- Modify: `auto2fa-mac/Auto2FA/Views/Components/HostRow.swift`

- [ ] **Step 1: Show a warning when a registered host is gone from config**

In `auto2fa-mac/Auto2FA/Views/Components/HostRow.swift`, where the host's title/metadata is rendered, add a conditional warning badge driven by `appState.unreachableRegisteredHosts`. Add near the host name `HStack`:

```swift
                if appState.unreachableRegisteredHosts.contains(host.host) {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .foregroundStyle(.orange)
                        .help("“\(host.host)” is no longer a Host in ~/.ssh/config — it won’t connect. Add it back, or remove this registration.")
                }
```

(`HostRow` already has `@EnvironmentObject var appState: AppState` — confirm; if it only receives `host`, add the environment object. Match how sibling rows access `appState`.)

- [ ] **Step 2: Build the app target**

```bash
cd auto2fa-mac && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FA -destination 'platform=macOS' build 2>&1 | tail -25
```
Expected: **BUILD SUCCEEDED**.

- [ ] **Step 3: Manual QA**

1. Register a host (e.g. `kempner`).
2. Comment out / remove its `Host kempner` block from `~/.ssh/config`.
3. In the app (after a reload), the `kempner` row shows the orange warning with the tooltip.
4. Restore the block → the warning clears on next reload.

- [ ] **Step 4: Commit**

```bash
git add auto2fa-mac/Auto2FA/Views/Components/HostRow.swift
git commit -m "feat(mac): host-row drift warning when a registered alias leaves ~/.ssh/config"
```

---

### Task 13: Full-suite green + final manual smoke

**Files:** none (verification only)

- [ ] **Step 1: Run the entire unit-test suite**

```bash
cd auto2fa-mac && xcodegen generate && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FATests -destination 'platform=macOS' test 2>&1 | tail -30
```
Expected: PASS — all suites green (SyncCore, SearchFilter, RecentNodes, SlurmTime, SSHPaths, SSHConfigParser, SSHSyncDiff, ControlPathResolver, SSHConfigManager).

- [ ] **Step 2: Build the app one last time**

```bash
cd auto2fa-mac && xcodebuild -project Auto2FA.xcodeproj -scheme Auto2FA -destination 'platform=macOS' build 2>&1 | tail -25
```
Expected: **BUILD SUCCEEDED**.

- [ ] **Step 3: End-to-end smoke (manual)**

1. Import a host from config → wizard prefilled → enable → connects.
2. "Open Terminal" on a connected host → shell with no 2FA prompt.
3. Accept the warm-reuse consent → own-terminal `ssh <alias>` is also prompt-free; config backed up; Host blocks untouched.
4. Remove a host from config → drift warning shows.
5. Turn warm reuse off in Settings → Include + ssh2fa.conf removed, config otherwise intact.

- [ ] **Step 4: Finish the branch**

Announce: "I'm using the finishing-a-development-branch skill to complete this work." Then follow **superpowers:finishing-a-development-branch** (verify tests, present options, execute choice).

---

## Notes for the implementer

- **Never** edit the user's own `Host` blocks. The only write into `~/.ssh/config` is the marked `Include` region via `SSHConfigManager.enableInclude` (after a backup). Everything else lives in `~/.ssh/ssh2fa.conf`.
- The ControlPath in `ssh2fa.conf` must stay the literal `cm-ssh2fa-<alias>` fallback path (not `%h`) so the daemon, the user's `ssh`, and the Terminal button all resolve to the **same** socket and enabling the Include never triggers a master rebuild.
- All five pure helpers are Foundation-only — keep `import SwiftUI`/`AppKit` out of them so they keep compiling into the headless test bundle.
- Bundle-version / signing are untouched here; this is a code-only feature. Cutting a signed release is a separate step (see `docs/RELEASE.md`).
