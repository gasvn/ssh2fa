import SwiftUI

/// First-run welcome / empty-state. Shown automatically when the daemon
/// reports zero hosts. Walks the user through the basics + opens the
/// Add Host wizard.
///
/// We deliberately don't auto-dismiss on configured-hosts > 0 here —
/// the user might want to read it through, and once they hit "Add
/// host" the wizard takes over and our sheet stays open until they
/// either skip or close.
struct WelcomeSheet: View {
    @EnvironmentObject var appState: AppState
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            content
            Divider()
            footer
        }
        .frame(width: 440)
    }

    private var header: some View {
        VStack(spacing: Spacing.s) {
            Image(systemName: "point.3.connected.trianglepath.dotted")
                .font(.system(size: 56))
                .foregroundStyle(.tint)
                .padding(.top, Spacing.xl)
            Text("Welcome to Auto2FA")
                .font(.dashTitle)
            Text("Log into your 2FA-protected SSH hosts once, then stay connected — no more typing a Duo/TOTP code every time you `ssh`.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 32)
            Text("Built for HPC / cluster logins. Works with any host that asks for a password + a typed 2FA code.")
                .font(.caption)
                .foregroundStyle(.tertiary)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 32)
                .padding(.bottom, Spacing.l)
        }
    }

    private var content: some View {
        VStack(alignment: .leading, spacing: Spacing.m) {
            // Feature rows wrapped in a glass card for a cohesive panel look
            VStack(alignment: .leading, spacing: Spacing.m) {
                row(icon: "server.rack",
                    title: "1. Add a host",
                    body: "Its ssh-config alias, your password, and your 2FA secret. The wizard shows you how to find the secret (incl. Duo). We test-login before saving so a wrong code can't lock you out.")
                row(icon: "bolt.horizontal.circle",
                    title: "2. Stay logged in",
                    body: "A background helper keeps two connections warm per host, so every `ssh` reuses them instantly — no code to type. It auto-recovers after sleep, network changes, and reboots.")
                row(icon: "arrow.triangle.branch",
                    title: "3. (Optional) Forward a port to a compute node",
                    body: "On a SLURM cluster? Pick a running job's node and forward a local port to it (Jupyter, etc.). If you just want no-retype SSH, skip this entirely.")
                row(icon: "menubar.dock.rectangle",
                    title: "Lives in your menu bar",
                    body: "Status at a glance, right-click for quick actions. ⌘, for Settings; daemon logs from the menu-bar menu.")
            }
            .padding(Spacing.m)
            .glassCard(cornerRadius: Radius.control)
        }
        .padding(Spacing.xl)
    }

    private func row(icon: String, title: String, body: String) -> some View {
        HStack(alignment: .top, spacing: Spacing.m) {
            Image(systemName: icon)
                .font(.title2)
                .foregroundStyle(.tint)
                .frame(width: 28)
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.rowTitle)
                Text(body).font(.callout).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private var footer: some View {
        HStack {
            Button("Skip for now") {
                UserDefaults.standard.set(true, forKey: SettingsKey.welcomeShown)
                dismiss()
            }
            Spacer()
            Button {
                UserDefaults.standard.set(true, forKey: SettingsKey.welcomeShown)
                dismiss()
                // Tiny delay so dismiss animation finishes before next sheet opens.
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.25) {
                    appState.presentAddHost()
                }
            } label: {
                Label("Add my first host", systemImage: "plus")
            }
            .keyboardShortcut(.defaultAction)
            .controlSize(.large)
            .buttonStyle(.borderedProminent)
        }
        .padding(Spacing.xl)
        .background(.bar)
    }
}
