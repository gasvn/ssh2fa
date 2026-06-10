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
    var body: some View {
        VStack(spacing: 16) {
            Image(systemName: "point.3.connected.trianglepath.dotted")
                .font(.system(size: 64))
                .foregroundStyle(.tint)
                .padding(.top, 24)
            Text("Auto2FA")
                .font(.title.weight(.semibold))
            Text("SSH ControlMaster pool + 2FA login + SLURM-aware port forwarding")
                .multilineTextAlignment(.center)
                .foregroundStyle(.secondary)
                .padding(.horizontal, 32)
            Link("github.com/gasvn/auto2fa",
                 destination: URL(string: "https://github.com/gasvn/auto2fa")!)
                .font(.callout)
            Spacer()
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}
