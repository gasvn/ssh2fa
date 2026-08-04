import SwiftUI

/// One deliberate explanation around the only credential migration that may
/// require macOS interaction. Normal startup and reconnect UI never mentions
/// storage internals or asks the user to interpret helper/daemon state.
struct CredentialUpgradeSheet: View {
    @EnvironmentObject private var appState: AppState
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: Spacing.l) {
            header
            content
            Divider()
            actions
        }
        .padding(Spacing.xl)
        .frame(width: 480)
        .interactiveDismissDisabled(appState.credentialUpgradeStatus.isRunning)
    }

    private var header: some View {
        HStack(spacing: Spacing.m) {
            Image(systemName: headerIcon)
                .font(.system(size: 28, weight: .semibold))
                .foregroundStyle(headerColor)
                .frame(width: 42, height: 42)
            VStack(alignment: .leading, spacing: 3) {
                Text(headerTitle).font(.dashTitle)
                Text(headerSubtitle)
                    .font(.rowMeta)
                    .foregroundStyle(.secondary)
            }
        }
    }

    @ViewBuilder
    private var content: some View {
        switch appState.credentialUpgradeStatus {
        case .idle:
            VStack(alignment: .leading, spacing: Spacing.m) {
                Text("Older SSH2FA versions saved each login separately. This migration moves them into the new protected format so future updates stay quiet.")
                    .fixedSize(horizontal: false, vertical: true)
                migrationAuthorizationCallout
                benefit("Your SSH password and 2FA secret stay protected on this Mac.", icon: "lock.fill")
                benefit("Current SSH connections keep running while the migration finishes.", icon: "bolt.horizontal.fill")
                benefit("If you have several saved logins, macOS may show more than one confirmation because the old items were protected separately. Approve each one; this is the final migration.", icon: "hand.raised.fill")
            }
        case .running:
            VStack(alignment: .leading, spacing: Spacing.m) {
                ProgressView()
                    .controlSize(.regular)
                migrationAuthorizationCallout
                Text("Approve each macOS confirmation that appears, then keep this migration window open until it completes.")
                    .fixedSize(horizontal: false, vertical: true)
                Text("You do not need to enter an SSH password or a 2FA code again.")
                    .font(.rowMeta)
                    .foregroundStyle(.secondary)
            }
        case .succeeded(let hostCount):
            Text(CredentialWarmup.successMessage(hostCount: hostCount))
                .fixedSize(horizontal: false, vertical: true)
        case .failed(let message):
            VStack(alignment: .leading, spacing: Spacing.s) {
                Text(message).fixedSize(horizontal: false, vertical: true)
                Text("You can try again now or continue using SSH2FA and finish after the next launch.")
                    .font(.rowMeta)
                    .foregroundStyle(.secondary)
            }
        }
    }

    @ViewBuilder
    private var actions: some View {
        HStack {
            Spacer()
            switch appState.credentialUpgradeStatus {
            case .idle:
                Button("Not now") {
                    appState.deferCredentialUpgrade()
                    dismiss()
                }
                .buttonStyle(.glass)
                Button("Start migration") {
                    Task { await appState.runCredentialWarmup() }
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
            case .running:
                Text("Migration in progress…")
                    .font(.rowMeta)
                    .foregroundStyle(.secondary)
            case .succeeded:
                Button("Done") { dismiss() }
                    .buttonStyle(.borderedProminent)
                    .keyboardShortcut(.defaultAction)
            case .failed:
                Button("Not now") {
                    appState.deferCredentialUpgrade()
                    dismiss()
                }
                .buttonStyle(.glass)
                Button("Try again") {
                    Task { await appState.runCredentialWarmup() }
                }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
            }
        }
    }

    private func benefit(_ text: LocalizedStringKey, icon: String) -> some View {
        HStack(alignment: .top, spacing: Spacing.s) {
            Image(systemName: icon)
                .foregroundStyle(.tint)
                .frame(width: 18)
            Text(text)
                .font(.rowMeta)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var migrationAuthorizationCallout: some View {
        HStack(alignment: .top, spacing: Spacing.s) {
            Image(systemName: "macbook.and.iphone")
                .foregroundStyle(.tint)
                .frame(width: 20)
            VStack(alignment: .leading, spacing: 4) {
                Text("Why macOS may ask for your password")
                    .font(.rowTitle)
                Text(CredentialWarmup.migrationAuthorizationExplanation())
                    .font(.rowMeta)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(Spacing.m)
        .background(Color.accentColor.opacity(0.09), in: RoundedRectangle(cornerRadius: Radius.control))
        .overlay {
            RoundedRectangle(cornerRadius: Radius.control)
                .stroke(Color.accentColor.opacity(0.24), lineWidth: 1)
        }
        .accessibilityElement(children: .combine)
    }

    private var headerIcon: String {
        switch appState.credentialUpgradeStatus {
        case .succeeded: return "checkmark.circle.fill"
        case .failed: return "exclamationmark.triangle.fill"
        default: return "checkmark.shield.fill"
        }
    }

    private var headerColor: Color {
        switch appState.credentialUpgradeStatus {
        case .succeeded: return .green
        case .failed: return .orange
        default: return .accentColor
        }
    }

    private var headerTitle: LocalizedStringKey {
        switch appState.credentialUpgradeStatus {
        case .idle: return "Migrate saved logins"
        case .running: return "Migrating saved logins"
        case .succeeded: return "Setup complete"
        case .failed: return "Setup wasn't completed"
        }
    }

    private var headerSubtitle: LocalizedStringKey {
        switch appState.credentialUpgradeStatus {
        case .idle: return "One-time migration from an older SSH2FA version"
        case .running: return "Moving old saved logins into the new protected format"
        case .succeeded: return "Automatic connections are ready"
        case .failed: return "Your saved logins are unchanged"
        }
    }
}
