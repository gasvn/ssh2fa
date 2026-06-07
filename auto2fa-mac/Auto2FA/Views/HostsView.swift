import SwiftUI

struct HostsView: View {
    @EnvironmentObject var appState: AppState

    var body: some View {
        VStack(alignment: .leading, spacing: Spacing.s) {
            header
            if appState.hosts.isEmpty {
                emptyState
            } else {
                hostsList
            }
        }
        .padding(Spacing.m)
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
        }
    }

    private func countPill(_ n: Int) -> some View {
        Text("\(n)")
            .font(.countBadge)
            .foregroundStyle(Brand.accent)
            .padding(.horizontal, Spacing.s)
            .padding(.vertical, 2)
            .background(Brand.accent.opacity(0.15), in: Capsule())
    }

    // MARK: - List

    private var hostsList: some View {
        List {
            ForEach(appState.hosts) { host in
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
        // Content sits at the BASE layer in a quiet OPAQUE grouped surface —
        // no glass. Rows read crisply against the solid control background.
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
