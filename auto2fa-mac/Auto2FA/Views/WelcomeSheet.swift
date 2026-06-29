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
        // Persist "seen" on EVERY dismissal route — Esc / scrim / button — so the
        // welcome sheet never reappears just because the user closed it with the
        // keyboard (the buttons set this too; this covers the paths they don't).
        .onDisappear { UserDefaults.standard.set(true, forKey: SettingsKey.welcomeShown) }
    }

    private var header: some View {
        VStack(spacing: Spacing.s) {
            Image(systemName: "point.3.connected.trianglepath.dotted")
                .font(.system(size: 56))
                .foregroundStyle(.tint)
                .padding(.top, Spacing.xl)
            Text("Welcome to SSH2FA")
                .font(.dashTitle)
            Text("Log into your 2FA-protected SSH hosts once, then stay connected — SSH2FA types your TOTP code for you every time you ssh.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 32)
            Text("Built for HPC / cluster logins. You'll need your 2FA **secret key** (the TOTP setup key) — SSH2FA types the codes. It does **not** work with Duo Push / approve-on-phone.")
                .font(.caption)
                .foregroundStyle(.tertiary)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 32)
            Text("On first connect, macOS asks permission for SSH2FA to use its own saved credentials in your Keychain — click “Always Allow”.")
                .font(.caption2)
                .foregroundStyle(.tertiary)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 32)
                .padding(.bottom, Spacing.l)
        }
    }

    private var content: some View {
        VStack(alignment: .leading, spacing: Spacing.m) {
            if !appState.importableHosts.isEmpty {
                VStack(alignment: .leading, spacing: Spacing.s) {
                    Label("Found \(appState.importableHosts.count) host(s) in your ~/.ssh/config",
                          systemImage: "sparkles")
                        .font(.callout.weight(.semibold))
                    Text("Pick which to protect — we pre-fill the alias, you just enter the password and 2FA secret (or scan the QR). We test-login before saving so a wrong code can't lock you out.")
                        .font(.callout).foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                    Button {
                        UserDefaults.standard.set(true, forKey: SettingsKey.welcomeShown)
                        dismiss()
                        DispatchQueue.main.asyncAfter(deadline: .now() + 0.25) {
                            appState.presentImport()
                        }
                    } label: {
                        Label("Pick hosts to protect →", systemImage: "square.and.arrow.down")
                            .frame(maxWidth: .infinity)
                    }
                    .controlSize(.large)
                    .buttonStyle(.borderedProminent)
                }
                .padding(Spacing.m)
                .groupedContent(cornerRadius: Radius.control)
            } else {
                Text("SSH2FA refers to each host by its ~/.ssh/config alias. Add your first one and enter its password + 2FA secret — that's it.")
                    .font(.callout).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(Spacing.m)
                    .groupedContent(cornerRadius: Radius.control)
            }
        }
        .padding(Spacing.xl)
    }

    private var footer: some View {
        HStack {
            Button("Skip for now") {
                UserDefaults.standard.set(true, forKey: SettingsKey.welcomeShown)
                dismiss()
            }
            Spacer()
            // Manual add: prominent only when there's no easier import path.
            // (.bordered/.borderedProminent are PrimitiveButtonStyles and can't
            // be type-erased into one ?: expression, so branch the whole button.)
            if appState.importableHosts.isEmpty {
                Button(action: addManually) {
                    Label("Add a host manually", systemImage: "plus")
                }
                .buttonStyle(.borderedProminent).controlSize(.large)
            } else {
                Button(action: addManually) {
                    Label("Add a host manually", systemImage: "plus")
                }
                .buttonStyle(.bordered).controlSize(.large)
            }
        }
        .padding(Spacing.xl)
    }

    private func addManually() {
        UserDefaults.standard.set(true, forKey: SettingsKey.welcomeShown)
        dismiss()
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.25) {
            appState.presentAddHost()
        }
    }
}
