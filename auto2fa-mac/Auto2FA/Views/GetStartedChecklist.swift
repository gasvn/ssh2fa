import SwiftUI

/// In-app "Get started" guide. Two modes:
/// - `compact: false` — the full empty-state panel (icon + steps + the import /
///   add-host CTAs) shown when there are no hosts yet.
/// - `compact: true` — a slim card shown ABOVE the host list while onboarding is
///   still incomplete, with a dismiss button.
struct GetStartedChecklist: View {
    @EnvironmentObject var appState: AppState
    let compact: Bool
    /// Called when the user dismisses the compact card.
    var onDismiss: () -> Void = {}

    private var done: Set<OnboardingStep> {
        OnboardingChecklist.completed(
            hostCount: appState.hosts.count,
            anyConnected: appState.hosts.contains { $0.displayState == .connected },
            usedTerminal: UserDefaults.standard.bool(forKey: SettingsKey.usedTerminal),
            warmReuse: UserDefaults.standard.bool(forKey: SettingsKey.warmReuseEnabled))
    }

    var body: some View {
        if compact { compactCard } else { fullPanel }
    }

    // MARK: - Steps

    private func stepRow(_ step: OnboardingStep, _ text: String) -> some View {
        let isDone = done.contains(step)
        return HStack(spacing: Spacing.s) {
            Image(systemName: isDone ? "checkmark.circle.fill" : "circle")
                .foregroundStyle(isDone ? Color.green : Color.secondary)
            Text(text)
                .strikethrough(isDone, color: .secondary)
                .foregroundStyle(isDone ? .secondary : .primary)
        }
        .font(.callout)
    }

    private var steps: some View {
        VStack(alignment: .leading, spacing: Spacing.xs) {
            stepRow(.addHost, "Add your first host")
            stepRow(.seeConnect, "Watch it connect (stays warm in the background)")
            stepRow(.openTerminal, "Open a Terminal from its row — no 2FA code to type")
        }
    }

    // MARK: - Full empty-state panel

    private var fullPanel: some View {
        VStack(spacing: Spacing.m) {
            Image(systemName: "checklist")
                .font(.largeTitle)
                .foregroundStyle(.tint)
            Text("Get started")
                .font(.title3)
            steps
                .padding(Spacing.m)
                .groupedContent(cornerRadius: Radius.control)
            if !appState.importableHosts.isEmpty {
                Button { appState.presentImport() } label: {
                    Label("Found \(appState.importableHosts.count) host(s) in ~/.ssh/config — pick which to protect",
                          systemImage: "sparkles")
                }
                .buttonStyle(.glassProminent)
            }
            Button { appState.presentAddHost() } label: {
                Label("Add a host manually", systemImage: "plus")
            }
            .controlSize(.large)
            .buttonStyle(.borderedProminent)
            Text("On a SLURM cluster? You can also forward a local port to a compute node — see the Tunnels tab.")
                .font(.caption).foregroundStyle(.tertiary)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 32)
        }
        .padding(Spacing.l)
    }

    // MARK: - Compact above-list card

    private var compactCard: some View {
        HStack(alignment: .top, spacing: Spacing.m) {
            VStack(alignment: .leading, spacing: Spacing.xs) {
                Text("Get started").font(.callout.weight(.semibold))
                steps
            }
            Spacer()
            Button { onDismiss() } label: {
                Image(systemName: "xmark.circle.fill").foregroundStyle(.secondary)
            }
            .buttonStyle(.borderless)
            .help("Dismiss")
        }
        .padding(Spacing.m)
        .groupedContent(cornerRadius: Radius.control)
    }
}
