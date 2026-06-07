import SwiftUI

struct HostsView: View {
    @EnvironmentObject var appState: AppState

    var body: some View {
        if appState.hosts.isEmpty {
            emptyState
        } else {
            hostsList
        }
    }

    // MARK: - List

    private var hostsList: some View {
        List {
            Section {
                ForEach(appState.hosts) { host in
                    HostRow(host: host)
                        .listRowInsets(EdgeInsets(top: 0,
                                                  leading: Spacing.m,
                                                  bottom: 0,
                                                  trailing: Spacing.m))
                }
            } header: {
                Text("Hosts")
                    .sectionHeaderStyle()
            }
        }
        .listStyle(.inset)
        .environmentObject(appState)
    }

    // MARK: - Empty state

    private var emptyState: some View {
        VStack(spacing: Spacing.m) {
            Image(systemName: "server.rack")
                .font(.largeTitle)
                .foregroundStyle(.tint)
            Text("No SSH hosts yet")
                .font(.title3)
            Text("Register a host with its 2FA secret and the daemon will keep its login pool warm in the background.")
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 32)
            Button {
                appState.presentAddHost()
            } label: {
                Label("Add your first SSH host", systemImage: "plus")
            }
            .controlSize(.large)
            .buttonStyle(.borderedProminent)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding()
    }
}
