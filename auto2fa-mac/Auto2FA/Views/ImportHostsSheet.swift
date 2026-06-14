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
