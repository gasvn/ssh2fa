import SwiftUI

struct HostsView: View {
    @EnvironmentObject var appState: AppState
    @AppStorage(SettingsKey.onboardingDismissed) private var onboardingDismissed = false
    @State private var showingCredentialUpgrade = false

    private var onboardingActive: Bool {
        !onboardingDismissed && OnboardingChecklist.shouldShow(
            hostCount: appState.hosts.count,
            anyConnected: appState.hosts.contains { $0.displayState == .connected },
            usedTerminal: UserDefaults.standard.bool(forKey: SettingsKey.usedTerminal),
            warmReuse: UserDefaults.standard.bool(forKey: SettingsKey.warmReuseEnabled))
    }

    var body: some View {
        VStack(alignment: .leading, spacing: Spacing.s) {
            header
            if appState.hosts.isEmpty {
                if let activity = appState.connectionActivity {
                    VStack(spacing: Spacing.s) {
                        ProgressView().controlSize(.small)
                        Text(activity.userMessage)
                            .foregroundStyle(.secondary)
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .padding(Spacing.l)
                } else if appState.connectionError != nil {
                    // A real startup failure is explained by the actionable
                    // banner above. Keep the empty state truthful without
                    // duplicating its wording or exposing implementation detail.
                    VStack(spacing: Spacing.s) {
                        Image(systemName: "exclamationmark.triangle")
                            .foregroundStyle(.secondary)
                        Text("SSH2FA isn't ready")
                            .foregroundStyle(.secondary)
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .padding(Spacing.l)
                } else {
                    // No hosts yet → the checklist IS the empty state (carries the CTAs).
                    GetStartedChecklist(compact: false)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            } else {
                // Has hosts but onboarding not finished → slim guide above the list.
                if onboardingActive {
                    GetStartedChecklist(compact: true) { onboardingDismissed = true }
                }
                credentialWarmupBanner
                if visibleHosts.isEmpty {
                    noMatches
                } else {
                    hostsList
                }
            }
        }
        .padding(Spacing.m)
        .sheet(isPresented: $showingCredentialUpgrade) {
            CredentialUpgradeSheet()
                .environmentObject(appState)
        }
    }

    /// A calm, single-purpose entry point. The sheet explains the one-time
    /// system interaction before anything happens; routine connection rows stay
    /// free of storage implementation details.
    @ViewBuilder
    private var credentialWarmupBanner: some View {
        if appState.shouldOfferCredentialWarmup {
            HStack(spacing: Spacing.s) {
                Image(systemName: "checkmark.shield.fill").foregroundStyle(.tint)
                VStack(alignment: .leading, spacing: 2) {
                    Text("Migrate saved logins")
                        .font(.rowTitle)
                    Text("An older version saved each login separately. Review the one-time migration so future launches, reconnects, and updates stay automatic without Mac password prompts.")
                        .font(.rowMeta).foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Spacer(minLength: Spacing.s)
                Button("Review migration") { showingCredentialUpgrade = true }
                    .buttonStyle(.glass)
            }
            .padding(Spacing.m)
            .glassCard(cornerRadius: Radius.control)
        }
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
            .help("Add a host (SSH password, plus a 2FA secret if the server asks for codes)")
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


}
