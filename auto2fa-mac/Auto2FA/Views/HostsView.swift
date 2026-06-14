import SwiftUI

struct HostsView: View {
    @EnvironmentObject var appState: AppState

    var body: some View {
        VStack(alignment: .leading, spacing: Spacing.s) {
            header
            if appState.hosts.isEmpty {
                emptyState
            } else if visibleHosts.isEmpty {
                noMatches
            } else {
                hostsList
            }
        }
        .padding(Spacing.m)
    }

    private var visibleHosts: [SSHHost] {
        appState.hosts.filter { SearchFilter.matches(query: appState.searchQuery, in: [$0.host]) }
    }

    private var noMatches: some View {
        VStack(spacing: Spacing.s) {
            Image(systemName: "magnifyingglass").font(.title2).foregroundStyle(.secondary)
            Text("No hosts match “\(appState.searchQuery)”").foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(Spacing.l)
    }

    // MARK: - Header

    private var header: some View {
        HStack(spacing: Spacing.s) {
            Image(systemName: "server.rack")
                .foregroundStyle(.secondary)
            Text("Hosts")
                .font(.dashTitle)
            countPill(appState.hosts.count)
            Spacer()
            Button { appState.presentAddHost() } label: {
                Label("Add Host", systemImage: "plus")
            }
            .buttonStyle(.glass)
            .help("Add a host (register SSH + 2FA)")
            Button { appState.presentImport() } label: {
                Label("Add from ~/.ssh/config", systemImage: "square.and.arrow.down")
            }
            .buttonStyle(.glass)
            .help("Import hosts from your SSH config file")
        }
    }

    private func countPill(_ n: Int) -> some View {
        Text("\(n)")
            .font(.countBadge)
            .foregroundStyle(Brand.accent)
            .padding(.horizontal, Spacing.s)
            .padding(.vertical, 2)
            .glassEffect(.regular.tint(Brand.accent.opacity(0.5)), in: .capsule)
    }

    // MARK: - List

    private var hostsList: some View {
        List {
            ForEach(visibleHosts) { host in
                HostRow(host: host)
                    .listRowInsets(EdgeInsets(top: 1,
                                              leading: Spacing.m,
                                              bottom: 1,
                                              trailing: Spacing.m))
                    .listRowBackground(Color.clear)
                    .listRowSeparator(.hidden)
            }
        }
        .listStyle(.plain)
        .scrollContentBackground(.hidden)
        .environmentObject(appState)
        // Rows float directly on the window's frosted glass (no separate card).
        .groupedContent()
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
            if !appState.importableHosts.isEmpty {
                Button {
                    appState.presentImport()
                } label: {
                    Label("Found \(appState.importableHosts.count) host(s) in ~/.ssh/config — pick which to protect",
                          systemImage: "sparkles")
                }
                .buttonStyle(.glassProminent)
            }
            Button {
                appState.presentAddHost()
            } label: {
                Label("Add your first SSH host", systemImage: "plus")
            }
            .controlSize(.large)
            .buttonStyle(.borderedProminent)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(Spacing.xl)
    }
}
