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
            Text("Two-factor SSH login + auto-rotating ControlMaster pool + SLURM-aware port forwarding")
                .font(.callout)
                .foregroundStyle(.secondary)
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
                    title: "1. Add an SSH host",
                    body: "Hostname, user, password, and your TOTP secret URL. We do a one-shot test login before saving so wrong creds never trigger a rate-limit cascade.")
                row(icon: "point.3.connected.trianglepath.dotted",
                    title: "2. The daemon keeps a connection pool warm",
                    body: "Two SSH ControlMaster processes per host. New sessions reuse them instantly — no 2FA prompt every time you `ssh`.")
                row(icon: "arrow.triangle.branch",
                    title: "3. Create tunnels that ride the pool",
                    body: "Pick a SLURM compute node, the tunnel runs `ssh -L localhost:<port>:<node>:<port> via your warm jump`. Auto-recovers on Mac sleep/wake.")
                row(icon: "menubar.dock.rectangle",
                    title: "Lives in your menu bar + Dock",
                    body: "Tunnel count badge. Right-click for quick actions. ⌘, for Settings; open daemon logs from the menu-bar menu.")
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
