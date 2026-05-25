import SwiftUI

/// User-tunable behavior knobs. Stored in UserDefaults via @AppStorage. Read
/// by AppState / NotchPresenter / DaemonProcess at the relevant decision
/// points. Defaults chosen to match current behavior so flipping any switch
/// is intentional opt-out.
enum SettingsKey {
    static let notchEnabled = "auto2fa.notch.enabled"
    static let autoOpenBrowser = "auto2fa.autoOpenBrowser"
    static let autoRecoverOnWake = "auto2fa.autoRecoverOnWake"
    static let spawnDaemonOnLaunch = "auto2fa.spawnDaemonOnLaunch"
}

struct SettingsView: View {
    @AppStorage(SettingsKey.notchEnabled) private var notchEnabled = true
    @AppStorage(SettingsKey.autoOpenBrowser) private var autoOpenBrowser = false
    @AppStorage(SettingsKey.autoRecoverOnWake) private var autoRecoverOnWake = true
    @AppStorage(SettingsKey.spawnDaemonOnLaunch) private var spawnDaemonOnLaunch = true

    var body: some View {
        TabView {
            Form {
                Section {
                    Toggle("Show Dynamic Notch toasts", isOn: $notchEnabled)
                    Text("Notifications for tunnel state changes appear over the MacBook Pro notch. Disabling falls back to no UI feedback.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                } header: { Text("Notifications") }

                Section {
                    Toggle("Open localhost URL in browser when tunnel comes up", isOn: $autoOpenBrowser)
                    Text("Triggers once per tunnel transition idle → alive. If your tunnel hosts a notebook server (jupyter etc.), this saves you a click.")
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
                    Text("Off if you prefer to run the daemon yourself (LaunchAgent, manual `python -m auto2fa.daemon`, etc.).")
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
