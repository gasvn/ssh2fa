import SwiftUI

/// User-tunable behavior knobs. Stored in UserDefaults via @AppStorage. Read
/// by AppState / NotchPresenter / DaemonProcess at the relevant decision
/// points. Defaults chosen to match current behavior so flipping any switch
/// is intentional opt-out.
enum SettingsKey {
    static let notchEnabled = "auto2fa.notch.enabled"
    static let notchPersistent = "auto2fa.notch.persistent"
    static let autoOpenBrowser = "auto2fa.autoOpenBrowser"
    static let autoRecoverOnWake = "auto2fa.autoRecoverOnWake"
    static let spawnDaemonOnLaunch = "auto2fa.spawnDaemonOnLaunch"
    static let welcomeShown = "auto2fa.welcomeShown"
    static let compactRows = "auto2fa.compactRows"
}

struct SettingsView: View {
    @AppStorage(SettingsKey.notchEnabled) private var notchEnabled = true
    @AppStorage(SettingsKey.notchPersistent) private var notchPersistent = false
    @AppStorage(SettingsKey.autoOpenBrowser) private var autoOpenBrowser = false
    @AppStorage(SettingsKey.autoRecoverOnWake) private var autoRecoverOnWake = true
    @AppStorage(SettingsKey.spawnDaemonOnLaunch) private var spawnDaemonOnLaunch = true
    @AppStorage(SettingsKey.compactRows) private var compactRows = false
    // launch-at-login state isn't a persisted preference (it's owned by
    // macOS via SMAppService); we just mirror it in @State for the Toggle.
    @State private var launchAtLogin = LoginItem.isEnabled
    @State private var launchAtLoginError: String?

    var body: some View {
        TabView {
            Form {
                Section {
                    Toggle("Start Auto2FA at login", isOn: $launchAtLogin)
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
                    Text("For best reliability, drag Auto2FA.app to /Applications first — SMAppService remembers the bundle path at register time, so moving the .app later silently breaks the auto-launch.")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                } header: { Text("Launch") }

                Section {
                    Toggle("Show Dynamic Notch toasts", isOn: $notchEnabled)
                    Text("Notifications for tunnel state changes appear over the MacBook Pro notch. Disabling falls back to no UI feedback.")
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
                    Toggle("Start the auto2fa daemon when this app launches", isOn: $spawnDaemonOnLaunch)
                    Text("Off if you prefer to run the daemon yourself (LaunchAgent, or run a2fa-daemon manually).")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                } header: { Text("Daemon") }
            }
            .formStyle(.grouped)
            .tabItem { Label("General", systemImage: "gearshape") }

            AboutPane()
                .tabItem { Label("About", systemImage: "info.circle") }
        }
        .frame(width: 520, height: 440)
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
            Text("Auto2FA")
                .font(.title.weight(.semibold))
            Text(versionString)
                .font(.caption)
                .foregroundStyle(.secondary)
            Text("SSH ControlMaster pool + 2FA login + SLURM-aware port forwarding")
                .multilineTextAlignment(.center)
                .foregroundStyle(.secondary)
                .padding(.horizontal, 32)
            Link("github.com/gasvn/auto2fa",
                 destination: URL(string: "https://github.com/gasvn/auto2fa")!)
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
        URL(string: "https://api.github.com/repos/gasvn/auto2fa/releases/latest")!
    static let releasesPage =
        URL(string: "https://github.com/gasvn/auto2fa/releases")!

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
