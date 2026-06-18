import SwiftUI

/// User-tunable behavior knobs. Stored in UserDefaults via @AppStorage. Read
/// by AppState / NotchPresenter / DaemonProcess at the relevant decision
/// points. Defaults chosen to match current behavior so flipping any switch
/// is intentional opt-out.
enum SettingsKey {
    static let notchEnabled = "auto2fa.notch.enabled"
    static let notchPersistent = "auto2fa.notch.persistent"
    static let notchDoNotDisturb = "auto2fa.notch.dnd"
    static let requireTouchID = "auto2fa.security.requireTouchID"
    static let syncPrefsViaICloud = "auto2fa.sync.icloudPrefs"
    static let autoOpenBrowser = "auto2fa.autoOpenBrowser"
    static let autoRecoverOnWake = "auto2fa.autoRecoverOnWake"
    static let spawnDaemonOnLaunch = "auto2fa.spawnDaemonOnLaunch"
    static let welcomeShown = "auto2fa.welcomeShown"
    static let compactRows = "auto2fa.compactRows"
    /// "" = ask the first time; "system" = default .command handler; else a
    /// terminal app bundle id. Used by TerminalLauncher (host "Open Terminal").
    static let terminalApp = "auto2fa.terminalApp"
    static let warmReuseEnabled = "auto2fa.warmReuseInclude"
    static let warmReuseAsked   = "auto2fa.warmReuseAsked"
    /// Set the first time a host's "Open Terminal" actually launches — drives the
    /// onboarding checklist's "open a terminal" step.
    static let usedTerminal = "auto2fa.usedTerminal"
    /// User dismissed the Get-Started checklist — hide it for good.
    static let onboardingDismissed = "auto2fa.onboardingDismissed"
    /// Which Settings tab is shown — lets the menu-bar "Troubleshoot…" deep-link
    /// straight to that tab instead of dumping the user on General.
    static let settingsTab = "auto2fa.settingsTab"
}

/// Settings tab identifiers (also the persisted `settingsTab` values).
enum SettingsTab {
    static let general = "general"
    static let troubleshoot = "troubleshoot"
    static let about = "about"
}

struct SettingsView: View {
    @EnvironmentObject private var appState: AppState
    @AppStorage(SettingsKey.notchEnabled) private var notchEnabled = true
    @AppStorage(SettingsKey.notchPersistent) private var notchPersistent = false
    @AppStorage(SettingsKey.notchDoNotDisturb) private var notchDoNotDisturb = false
    @AppStorage(SettingsKey.autoOpenBrowser) private var autoOpenBrowser = false
    @AppStorage(SettingsKey.autoRecoverOnWake) private var autoRecoverOnWake = true
    @AppStorage(SettingsKey.spawnDaemonOnLaunch) private var spawnDaemonOnLaunch = true
    @AppStorage(SettingsKey.compactRows) private var compactRows = false
    @AppStorage(SettingsKey.terminalApp) private var terminalApp = ""
    @AppStorage(SettingsKey.warmReuseEnabled) private var warmReuseEnabled = false
    @AppStorage(SettingsKey.requireTouchID) private var requireTouchID = false
    @AppStorage(SettingsKey.syncPrefsViaICloud) private var syncPrefsViaICloud = false
    @AppStorage(SettingsKey.settingsTab) private var settingsTab = SettingsTab.general
    // launch-at-login state isn't a persisted preference (it's owned by
    // macOS via SMAppService); we just mirror it in @State for the Toggle.
    @State private var launchAtLogin = LoginItem.isEnabled
    @State private var launchAtLoginError: String?

    private var iCloudDriveAvailable: Bool { PreferenceSync.iCloudDriveAvailable() }

    var body: some View {
        TabView(selection: $settingsTab) {
            Form {
                Section {
                    HStack(alignment: .top, spacing: Spacing.m) {
                        Image(systemName: "bolt.shield")
                            .font(.title2).foregroundStyle(.tint)
                        VStack(alignment: .leading, spacing: 4) {
                            Text("How SSH2FA works")
                                .font(.callout.weight(.semibold))
                            Text("It answers the password + 2FA prompt for you and keeps a warm connection to each host, so ssh, scp, and your editor connect instantly with no code to type. Your password and 2FA secret are stored in the macOS Keychain.")
                                .font(.caption).foregroundStyle(.secondary)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                    }
                } header: { Text("Overview") }

                Section {
                    Toggle("Start SSH2FA at login", isOn: $launchAtLogin)
                        .disabled(!LoginItem.isSupported)
                        .onChange(of: launchAtLogin) { _, on in
                            launchAtLoginError = LoginItem.setEnabled(on)
                            if launchAtLoginError != nil {
                                // Revert toggle if the OS rejected the change.
                                DispatchQueue.main.async {
                                    launchAtLogin = LoginItem.isEnabled
                                }
                            }
                        }
                    Text(LoginItem.statusDescription)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    if let err = launchAtLoginError {
                        Text(err)
                            .font(.caption)
                            .foregroundStyle(.red)
                    }
                    Text("For best reliability, drag SSH2FA.app to /Applications first — SMAppService remembers the bundle path at register time, so moving the .app later silently breaks the auto-launch.")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                } header: { Text("Launch") }

                Section {
                    Picker("Open SSH in", selection: $terminalApp) {
                        Text("Ask the first time").tag("")
                        Text("System default").tag("system")
                        Text("Terminal").tag(TerminalLauncher.appleTerminalBundleID)
                        if TerminalLauncher.iTermInstalled() {
                            Text("iTerm").tag(TerminalLauncher.iTermBundleID)
                        }
                    }
                    Text("Which terminal app a host's “Open Terminal” action launches and SSHes in with.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                } header: { Text("Terminal") }

                Section {
                    Text(warmReuseEnabled
                         ? "On — running ssh <host> in your own Terminal reuses SSH2FA's warm connection (via one Include line added to ~/.ssh/config)."
                         : "Off — the app's \"Open Terminal\" already reuses the connection. Turning this on also makes ssh <host> in your own Terminal skip the 2FA prompt.")
                        .font(.caption).foregroundStyle(.secondary)
                    if warmReuseEnabled {
                        Button("Turn off & remove the Include") { WarmReuseConsent.revert() }
                    } else {
                        Button("Turn on (backs up config, adds one Include line)") {
                            // Pass the live host list so ssh2fa.conf is written
                            // populated, not momentarily empty until the next poll.
                            WarmReuseConsent.apply(currentAliases: appState.hosts.map { $0.host })
                        }
                    }
                } header: { Text("Warm connection reuse") }

                Section {
                    Toggle("Show Dynamic Notch toasts", isOn: $notchEnabled)
                    Text("Notifications for tunnel state changes appear over the MacBook Pro notch. Disabling falls back to no UI feedback.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Toggle("Do Not Disturb (compact notch)", isOn: $notchDoNotDisturb)
                        .disabled(!notchEnabled)
                    Text("In Do Not Disturb, toasts don't drop a panel down — they just expand compactly around the notch (icon + a few words). Less intrusive while you work.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Toggle("Always-on notch status (experimental)", isOn: $notchPersistent)
                        .disabled(!notchEnabled)
                    Text("When any tunnel is alive or transitioning, a small persistent indicator sits over the notch. Click for the full toast. Off by default — can be visually busy.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                } header: { Text("Notifications") }

                Section {
                    Toggle("Open localhost URL in browser when tunnel comes up", isOn: $autoOpenBrowser)
                    Text("Triggers once per tunnel transition idle → alive. If your tunnel hosts a notebook server (jupyter etc.), this saves you a click.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Toggle("Compact row mode", isOn: $compactRows)
                    Text("Tighter row height + smaller font for the tunnel table. Helpful when you have more than 10 tunnels.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                } header: { Text("Tunnels") }

                Section {
                    Toggle("Rebuild SSH masters + restart tunnels on wake", isOn: $autoRecoverOnWake)
                    Text("After Mac sleeps, the underlying TCP for every SSH master dies. Recommended on — without it tunnels silently break with no automatic recovery.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                } header: { Text("Sleep & Wake") }

                Section {
                    Toggle("Start the background helper when this app launches", isOn: $spawnDaemonOnLaunch)
                    Text("SSH2FA uses a small background helper to keep your connections alive. Leave this on unless you run it yourself.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                } header: { Text("Background helper") }

                Section {
                    Toggle("Require Touch ID to open the dashboard", isOn: $requireTouchID)
                    Text("Locks the dashboard and log windows behind Touch ID (falls back to your Mac login password). The menu-bar icon stays visible. Re-locks ~60s after you close the window.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    if requireTouchID && !BiometricLock.availability().ok {
                        Text("⚠︎ This Mac can't evaluate Touch ID or a login password right now — the lock may not engage.")
                            .font(.caption)
                            .foregroundStyle(.orange)
                    }
                } header: { Text("Privacy & Security") }

                Section {
                    Toggle("Sync preferences via iCloud Drive (free)", isOn: $syncPrefsViaICloud)
                    Text("Syncs only these app preferences across your Macs via a file in iCloud Drive — no paid Apple Developer account needed. Never includes your hosts, tunnels, or 2FA secret.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    if syncPrefsViaICloud && !iCloudDriveAvailable {
                        Text("⚠︎ iCloud Drive isn't available — turn it on in System Settings to sync.")
                            .font(.caption)
                            .foregroundStyle(.orange)
                    }
                } header: { Text("Sync") }
            }
            .formStyle(.grouped)
            .tabItem { Label("General", systemImage: "gearshape") }
            .tag(SettingsTab.general)

            TroubleshootPane()
                .tabItem { Label("Troubleshoot", systemImage: "stethoscope") }
                .tag(SettingsTab.troubleshoot)

            AboutPane()
                .tabItem { Label("About", systemImage: "info.circle") }
                .tag(SettingsTab.about)
        }
        .frame(width: 520, height: 460)
    }
}

// MARK: - Troubleshoot / health

/// Self-diagnostic so a user (or a bug report) can see WHY something isn't
/// working without reading Console logs. Pure read-only checks + a couple of
/// safe actions (restart daemon, open log, copy a diagnostics report).
private struct TroubleshootPane: View {
    @StateObject private var model = DiagnosticsModel()
    @State private var copied = false

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("Health checks").font(.headline)
                Spacer()
                if model.running { ProgressView().controlSize(.small) }
                Button { model.run() } label: { Image(systemName: "arrow.clockwise") }
                    .help("Re-run checks")
                    .disabled(model.running)
            }
            Divider()

            ScrollView {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(model.checks) { c in
                        HStack(alignment: .top, spacing: 8) {
                            Image(systemName: c.status.symbol)
                                .foregroundStyle(c.status.color)
                                .frame(width: 16)
                            VStack(alignment: .leading, spacing: 1) {
                                Text(c.name).font(.callout.weight(.medium))
                                Text(c.detail).font(.caption).foregroundStyle(.secondary)
                                    .fixedSize(horizontal: false, vertical: true)
                                if let fix = c.fixHint {
                                    Text(fix).font(.caption2).foregroundStyle(.tertiary)
                                        .fixedSize(horizontal: false, vertical: true)
                                }
                            }
                            Spacer()
                        }
                    }
                }
            }

            Divider()
            HStack {
                Button("Restart daemon") { model.restartDaemon() }
                Button("Open daemon log") {
                    NSWorkspace.shared.activateFileViewerSelecting(
                        [URL(fileURLWithPath: "/tmp/ssh2fa_daemon.log")])
                }
                Spacer()
                Button(copied ? "Copied ✓" : "Copy diagnostics") {
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(model.report(), forType: .string)
                    copied = true
                    DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) { copied = false }
                }
            }
        }
        .padding(20)
        .onAppear { model.run() }
    }
}

struct DiagCheck: Identifiable {
    enum Status { case ok, warn, fail, info
        var symbol: String {
            switch self {
            case .ok: return "checkmark.circle.fill"
            case .warn: return "exclamationmark.triangle.fill"
            case .fail: return "xmark.octagon.fill"
            case .info: return "info.circle"
            }
        }
        var color: Color {
            switch self {
            case .ok: return .green
            case .warn: return .orange
            case .fail: return .red
            case .info: return .secondary
            }
        }
        var tag: String {
            switch self { case .ok: return "OK"; case .warn: return "WARN"; case .fail: return "FAIL"; case .info: return "INFO" }
        }
    }
    let id = UUID()
    let name: String
    let status: Status
    let detail: String
    var fixHint: String? = nil
}

@MainActor
final class DiagnosticsModel: ObservableObject {
    @Published var checks: [DiagCheck] = []
    @Published var running = false

    func run() {
        running = true
        Task.detached(priority: .userInitiated) {
            let results = DiagnosticsModel.collect()
            await MainActor.run { self.checks = results; self.running = false }
        }
    }

    func restartDaemon() {
        let label = "com.ssh2fa.daemon"
        let domain = "gui/\(getuid())"
        _ = Self.sh("/bin/launchctl", ["kickstart", "-k", "\(domain)/\(label)"])
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.0) { self.run() }
    }

    /// A plain-text report for bug reports / the clipboard.
    func report() -> String {
        var s = "SSH2FA diagnostics\n"
        let v = Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "?"
        let b = Bundle.main.infoDictionary?["CFBundleVersion"] as? String ?? "?"
        s += "app \(v) (\(b)) · \(Bundle.main.bundlePath)\n"
        s += "macOS \(ProcessInfo.processInfo.operatingSystemVersionString)\n\n"
        for c in checks {
            s += "[\(c.status.tag)] \(c.name): \(c.detail)\n"
            if let f = c.fixHint { s += "       → \(f)\n" }
        }
        return s
    }

    // ---- the checks (run off the main thread) ----

    nonisolated static func collect() -> [DiagCheck] {
        var out: [DiagCheck] = []
        let home = NSHomeDirectory()
        let label = "com.ssh2fa.daemon"
        let domain = "gui/\(getuid())"

        // 1. Daemon running (via launchd).
        let print = sh("/bin/launchctl", ["print", "\(domain)/\(label)"]).out
        if print.contains("state = running") {
            let pid = firstMatch(print, #"pid = (\d+)"#) ?? "?"
            out.append(DiagCheck(name: "Daemon", status: .ok,
                                 detail: "Running (pid \(pid))."))
        } else if print.isEmpty || print.contains("Could not find service") {
            out.append(DiagCheck(name: "Daemon", status: .fail,
                                 detail: "Not loaded by launchd.",
                                 fixHint: "Try “Restart daemon”, or relaunch the app to reinstall the LaunchAgent."))
        } else {
            out.append(DiagCheck(name: "Daemon", status: .warn,
                                 detail: "Registered but not running.",
                                 fixHint: "Try “Restart daemon”."))
        }

        // 2. Socket responds.
        if socketResponds(home + "/.ssh2fa/ssh2fa.sock") {
            out.append(DiagCheck(name: "Daemon socket", status: .ok,
                                 detail: "Responding at ~/.ssh2fa/ssh2fa.sock."))
        } else {
            out.append(DiagCheck(name: "Daemon socket", status: .fail,
                                 detail: "No response on ~/.ssh2fa/ssh2fa.sock.",
                                 fixHint: "The daemon may be starting (signature validation can take a minute on first launch) — wait, then re-check."))
        }

        // 3. LaunchAgent plist.
        let plist = home + "/Library/LaunchAgents/\(label).plist"
        if FileManager.default.fileExists(atPath: plist) {
            let prog = firstMatch(sh("/usr/bin/plutil", ["-extract", "ProgramArguments.0", "raw", plist]).out, #"(.+)"#) ?? "?"
            out.append(DiagCheck(name: "LaunchAgent", status: .ok,
                                 detail: "Installed → \(prog)"))
        } else {
            out.append(DiagCheck(name: "LaunchAgent", status: .warn,
                                 detail: "Not installed.",
                                 fixHint: "Relaunch the app — it installs the LaunchAgent on first run (packaged builds only)."))
        }

        // 4. App location.
        let inApps = Bundle.main.bundlePath.hasPrefix("/Applications/")
        out.append(DiagCheck(name: "App location",
                             status: inApps ? .ok : .warn,
                             detail: Bundle.main.bundlePath,
                             fixHint: inApps ? nil : "Move SSH2FA.app to /Applications so the background helper has a stable path."))

        // 5. Quarantine (downloaded + un-notarized).
        let quarantined = sh("/usr/bin/xattr", ["-p", "com.apple.quarantine", Bundle.main.bundlePath]).code == 0
        if quarantined {
            out.append(DiagCheck(name: "Gatekeeper", status: .warn,
                                 detail: "App is quarantined (downloaded, not notarized).",
                                 fixHint: "If things won't start: System Settings → Privacy & Security → \"Open Anyway\", or run: xattr -dr com.apple.quarantine \(Bundle.main.bundlePath)"))
        } else {
            out.append(DiagCheck(name: "Gatekeeper", status: .ok, detail: "Not quarantined."))
        }

        // 6. SSH config.
        let sshDir = (ProcessInfo.processInfo.environment["SSH_CONFIG_PATH"].map { ($0 as NSString).expandingTildeInPath } ?? home + "/.ssh").trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        let cfg = "/" + sshDir + "/config"
        if let text = try? String(contentsOfFile: cfg, encoding: .utf8) {
            let hosts = text.split(separator: "\n").filter { $0.trimmingCharacters(in: .whitespaces).lowercased().hasPrefix("host ") }.count
            out.append(DiagCheck(name: "SSH config",
                                 status: hosts > 0 ? .ok : .warn,
                                 detail: hosts > 0 ? "\(cfg): \(hosts) Host alias(es)." : "\(cfg) has no Host entries.",
                                 fixHint: hosts > 0 ? nil : "Add a Host <alias> block for each machine you connect to."))
        } else {
            out.append(DiagCheck(name: "SSH config", status: .warn,
                                 detail: "No \(cfg).",
                                 fixHint: "Create ~/.ssh/config with a Host <alias> block per machine — SSH2FA refers to hosts by their ssh alias."))
        }

        // 7. sshfs / macFUSE (only needed for the optional mount feature).
        let sshfs = ["/usr/local/bin/sshfs", "/opt/homebrew/bin/sshfs"].first { FileManager.default.isExecutableFile(atPath: $0) }
            ?? (sh("/usr/bin/which", ["sshfs"]).code == 0 ? "sshfs (in PATH)" : nil)
        if let s = sshfs {
            out.append(DiagCheck(name: "sshfs (mount feature)", status: .ok, detail: "Found: \(s)."))
        } else {
            out.append(DiagCheck(name: "sshfs (mount feature)", status: .info,
                                 detail: "Not installed.",
                                 fixHint: "Only needed for the optional “mount host filesystem” feature. Install macFUSE + sshfs if you want it."))
        }

        return out
    }

    // ---- helpers (nonisolated; safe off-main) ----

    nonisolated static func sh(_ path: String, _ args: [String]) -> (out: String, code: Int32) {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: path)
        p.arguments = args
        let pipe = Pipe()
        p.standardOutput = pipe
        p.standardError = Pipe()
        do {
            try p.run()
            let data = pipe.fileHandleForReading.readDataToEndOfFile()
            p.waitUntilExit()
            return (String(data: data, encoding: .utf8) ?? "", p.terminationStatus)
        } catch {
            return ("", -1)
        }
    }

    nonisolated static func firstMatch(_ s: String, _ pattern: String) -> String? {
        guard let re = try? NSRegularExpression(pattern: pattern) else { return nil }
        let r = NSRange(s.startIndex..., in: s)
        guard let m = re.firstMatch(in: s, range: r), m.numberOfRanges > 1,
              let g = Range(m.range(at: 1), in: s) else { return nil }
        return String(s[g]).trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// Non-blocking-enough connect to the unix socket (success == daemon alive).
    nonisolated static func socketResponds(_ path: String) -> Bool {
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        if fd < 0 { return false }
        defer { close(fd) }
        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let bytes = Array(path.utf8)
        let cap = MemoryLayout.size(ofValue: addr.sun_path)
        if bytes.count >= cap { return false }
        // Copy the path into sun_path (rebinding the C tuple to CChar) without
        // taking the address of a single tuple element (which trips Swift's
        // exclusive-access check).
        withUnsafeMutablePointer(to: &addr.sun_path) { tuplePtr in
            tuplePtr.withMemoryRebound(to: CChar.self, capacity: cap) { dst in
                for (i, b) in bytes.enumerated() { dst[i] = CChar(bitPattern: b) }
                dst[bytes.count] = 0
            }
        }
        let len = socklen_t(MemoryLayout<sockaddr_un>.size)
        let r = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                connect(fd, $0, len)
            }
        }
        return r == 0
    }
}

private struct AboutPane: View {
    @StateObject private var updater = UpdateChecker()

    private var versionString: String {
        let v = Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "?"
        let b = Bundle.main.infoDictionary?["CFBundleVersion"] as? String ?? "?"
        return "Version \(v) (build \(b))"
    }

    var body: some View {
        VStack(spacing: 14) {
            Image(systemName: "point.3.connected.trianglepath.dotted")
                .font(.system(size: 64))
                .foregroundStyle(.tint)
                .padding(.top, 24)
            Text("SSH2FA")
                .font(.title.weight(.semibold))
            Text(versionString)
                .font(.caption)
                .foregroundStyle(.secondary)
            Text("SSH ControlMaster pool + 2FA login + SLURM-aware port forwarding")
                .multilineTextAlignment(.center)
                .foregroundStyle(.secondary)
                .padding(.horizontal, 32)
            Link("github.com/gasvn/ssh2fa",
                 destination: URL(string: "https://github.com/gasvn/ssh2fa")!)
                .font(.callout)

            // ---- Update check ----
            VStack(spacing: 6) {
                Button {
                    Task { await updater.check() }
                } label: {
                    if case .checking = updater.result {
                        HStack(spacing: 6) { ProgressView().controlSize(.small); Text("Checking…") }
                    } else {
                        Text("Check for Updates")
                    }
                }
                .disabled({ if case .checking = updater.result { return true } else { return false } }())

                switch updater.result {
                case .idle, .checking:
                    EmptyView()
                case .upToDate:
                    Label("You're on the latest version.", systemImage: "checkmark.circle")
                        .font(.caption).foregroundStyle(.green)
                case .updateAvailable(let latest, let url):
                    VStack(spacing: 4) {
                        Label("Version \(latest) is available.", systemImage: "arrow.down.circle")
                            .font(.caption).foregroundStyle(.blue)
                        Link("Open the releases page", destination: url).font(.caption)
                    }
                case .failed(let msg):
                    Label("Update check failed: \(msg)", systemImage: "exclamationmark.triangle")
                        .font(.caption2).foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                }
            }
            .padding(.top, 4)

            Spacer()
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

/// Dependency-free update check against the project's GitHub Releases.
///
/// Deliberately lightweight (no Sparkle, no embedded keys, no self-hosted
/// appcast): it queries the public Releases API, compares the latest release
/// tag to this bundle's version, and — if newer — points the user at the
/// release page. The app never downloads or self-installs; the user stays in
/// control of what runs (it holds SSH creds + TOTP secrets). Full Sparkle
/// auto-update is a documented future option (see docs/RELEASE.md).
@MainActor
final class UpdateChecker: ObservableObject {
    enum Result: Equatable {
        case idle
        case checking
        case upToDate(current: String)
        case updateAvailable(latest: String, url: URL)
        case failed(String)
    }

    @Published var result: Result = .idle

    private static let releasesAPI =
        URL(string: "https://api.github.com/repos/gasvn/ssh2fa/releases/latest")!
    static let releasesPage =
        URL(string: "https://github.com/gasvn/ssh2fa/releases")!

    static var currentVersion: String {
        (Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String) ?? "0"
    }

    func check() async {
        result = .checking
        var req = URLRequest(url: Self.releasesAPI)
        req.timeoutInterval = 10
        req.setValue("application/vnd.github+json", forHTTPHeaderField: "Accept")
        do {
            let (data, resp) = try await URLSession.shared.data(for: req)
            let code = (resp as? HTTPURLResponse)?.statusCode ?? 0
            // 404 = no published releases yet → "up to date" (nothing to offer)
            // rather than an error in the user's face.
            if code == 404 {
                result = .upToDate(current: Self.currentVersion)
                return
            }
            guard code == 200 else {
                result = .failed("GitHub returned HTTP \(code)")
                return
            }
            guard
                let obj = try JSONSerialization.jsonObject(with: data) as? [String: Any],
                let tag = obj["tag_name"] as? String
            else {
                result = .failed("Unexpected response from GitHub")
                return
            }
            let latest = tag.hasPrefix("v") ? String(tag.dropFirst()) : tag
            let pageURL = (obj["html_url"] as? String).flatMap(URL.init) ?? Self.releasesPage
            if Self.isNewer(latest, than: Self.currentVersion) {
                result = .updateAvailable(latest: latest, url: pageURL)
            } else {
                result = .upToDate(current: Self.currentVersion)
            }
        } catch {
            result = .failed(error.localizedDescription)
        }
    }

    /// Compare dotted numeric versions ("1.2.10" > "1.2.9"). Non-numeric parts
    /// compare as 0, so a tag the parser doesn't understand is "not newer"
    /// (never nag on garbage).
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
}
