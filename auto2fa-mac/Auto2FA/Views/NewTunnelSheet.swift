import SwiftUI

/// Create-tunnel form with quick-pick templates (Jupyter / TensorBoard /
/// Code-Server / Custom) + port auto-suggest + auto-start checkbox.
/// Daemon returns DUPLICATE / PORT_IN_USE / BAD_PARAMS errors which surface
/// in the inline `error` label.
struct NewTunnelSheet: View {
    @EnvironmentObject var appState: AppState
    @State private var template: TunnelTemplate = .custom
    @State private var name = ""
    @State private var portText = ""
    /// Remote port parsed from a pasted ssh command (`-L local:host:REMOTE`).
    /// nil → daemon defaults remote = local. Was parsed, DISPLAYED, then
    /// silently discarded — pasting `-L 8888:node:6006` created 8888→8888
    /// and the service never answered.
    @State private var parsedRemotePort: Int? = nil
    @State private var autoStart = false
    @State private var error: String?
    /// Non-error confirmation of what a pasted SSH command parsed to. Kept
    /// separate from `error` so it renders calm/secondary, not alarming red.
    @State private var pasteHint: String?
    @State private var submitting = false
    @State private var suggestingPort = false
    @FocusState private var focused: Field?

    enum Field { case name, port }

    enum TunnelTemplate: String, CaseIterable, Identifiable {
        case jupyter = "Jupyter"
        case tensorboard = "TensorBoard"
        case codeServer = "Code-Server"
        case custom = "Custom"
        var id: String { rawValue }

        /// Suggested default name + port. nil port means "auto-suggest".
        var defaults: (name: String, port: Int?) {
            switch self {
            case .jupyter: return ("jupyter", 8888)
            case .tensorboard: return ("tensorboard", 6006)
            case .codeServer: return ("code-server", 8080)
            case .custom: return ("", nil)
            }
        }

        var symbol: String {
            switch self {
            case .jupyter: return "j.circle.fill"
            case .tensorboard: return "chart.line.uptrend.xyaxis"
            case .codeServer: return "chevron.left.forwardslash.chevron.right"
            case .custom: return "wrench.adjustable"
            }
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: Spacing.l) {
            // Title row
            HStack(spacing: Spacing.s) {
                Text("New Tunnel")
                    .font(.dashTitle)
                Spacer()
                Button {
                    pasteFromClipboard()
                } label: {
                    Label("Paste SSH command", systemImage: "doc.on.clipboard")
                        .labelStyle(.titleAndIcon)
                        .font(.callout)
                }
                .buttonStyle(.borderless)
                .help("If your clipboard has `ssh -L 8888:host:8888 user@node` or a localhost URL, prefill from it")
            }

            // Form fields wrapped in a glass card for a layered look
            VStack(alignment: .leading, spacing: Spacing.m) {
                fieldGroup("Template") {
                    Picker("Template", selection: $template) {
                        ForEach(TunnelTemplate.allCases) { t in
                            Label(t.rawValue, systemImage: t.symbol).tag(t)
                        }
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                    .onChange(of: template) { _, new in applyTemplate(new) }
                }

                fieldGroup("Name") {
                    TextField("jupyter", text: $name)
                        .textFieldStyle(.roundedBorder)
                        .focused($focused, equals: .name)
                        .onSubmit { focused = .port }
                }

                fieldGroup {
                    HStack {
                        Text("Local port")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        Spacer()
                        Button {
                            Task { await suggestPort() }
                        } label: {
                            if suggestingPort {
                                ProgressView().controlSize(.small).scaleEffect(0.7)
                            } else {
                                Label("Next free", systemImage: "sparkles")
                                    .labelStyle(.titleAndIcon)
                                    .font(.caption)
                            }
                        }
                        .buttonStyle(.borderless)
                        .help("Ask the daemon for the next unused local port")
                    }
                } content: {
                    TextField("8888", text: $portText)
                        .textFieldStyle(.roundedBorder)
                        .focused($focused, equals: .port)
                        .onSubmit { submit() }
                }

                Toggle("Start automatically when daemon boots", isOn: $autoStart)
                    .toggleStyle(.checkbox)
            }
            .padding(Spacing.m)
            .groupedContent(cornerRadius: Radius.control)

            if let error {
                Text(error)
                    .foregroundStyle(.red)
                    .font(.callout)
                    .fixedSize(horizontal: false, vertical: true)
            } else if let pasteHint {
                Label(pasteHint, systemImage: "checkmark.circle")
                    .foregroundStyle(.secondary)
                    .font(.callout)
                    .fixedSize(horizontal: false, vertical: true)
            }

            HStack {
                Spacer()
                Button("Cancel") { appState.dismissSheet() }
                    .keyboardShortcut(.cancelAction)
                Button {
                    submit()
                } label: {
                    if submitting { ProgressView().controlSize(.small) }
                    else { Text("Create") }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(submitting)
            }
        }
        .padding(Spacing.xl)
        .frame(width: 440)
        .task {
            // Default-fill from the current template (Custom) and ask the
            // daemon for a sensible next-free port so the user doesn't have
            // to think about it.
            applyTemplate(template)
            if portText.isEmpty {
                await suggestPort()
            }
            focused = .name
        }
    }

    // MARK: - Field helpers

    @ViewBuilder
    private func fieldGroup<C: View>(_ label: String, @ViewBuilder content: () -> C) -> some View {
        VStack(alignment: .leading, spacing: Spacing.xs) {
            Text(label)
                .font(.caption)
                .foregroundStyle(.secondary)
            content()
        }
    }

    @ViewBuilder
    private func fieldGroup<L: View, C: View>(@ViewBuilder label: () -> L,
                                               @ViewBuilder content: () -> C) -> some View {
        VStack(alignment: .leading, spacing: Spacing.xs) {
            label()
            content()
        }
    }

    // MARK: - Logic (unchanged)

    private func applyTemplate(_ t: TunnelTemplate) {
        let d = t.defaults
        if name.isEmpty || TunnelTemplate.allCases
            .map({ $0.defaults.name }).contains(name) {
            name = d.name
        }
        if let p = d.port {
            portText = String(p)
        }
    }

    private func suggestPort() async {
        suggestingPort = true
        defer { suggestingPort = false }
        let base = Int(portText.trimmingCharacters(in: .whitespacesAndNewlines)) ?? 8888
        do {
            let free = try await appState.client.suggestPort(base: base)
            portText = String(free)
        } catch {
            // Non-fatal — user can still type a port manually
        }
    }

    private func pasteFromClipboard() {
        let pb = NSPasteboard.general.string(forType: .string) ?? ""
        guard let parsed = SSHCommandParser.parse(pb) else {
            error = "Clipboard doesn't look like an SSH command or localhost URL."
            pasteHint = nil
            return
        }
        error = nil
        if let lp = parsed.localPort { portText = String(lp) }
        parsedRemotePort = parsed.remotePort
        if let suggested = parsed.suggestedName, name.isEmpty { name = suggested }
        // Surface what we extracted so the user can sanity-check before
        // hitting Create. Node/user aren't tracked on the tunnel record
        // (they're set via node picker), so we just show them in the error
        // slot as info.
        var bits: [String] = []
        if let node = parsed.node { bits.append("node=\(node)") }
        if let user = parsed.user { bits.append("user=\(user)") }
        if let lp = parsed.localPort { bits.append("local=\(lp)") }
        if let rp = parsed.remotePort { bits.append("remote=\(rp)") }
        // Surface what we extracted as a calm secondary hint (not the red error
        // label) so a successful paste never reads as a failure.
        pasteHint = bits.isEmpty ? nil : "Parsed: " + bits.joined(separator: ", ")
    }

    private func submit() {
        guard !submitting else { return }
        let trimmedName = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedName.isEmpty else {
            error = "Name cannot be empty."
            focused = .name
            return
        }
        guard let port = Int(portText.trimmingCharacters(in: .whitespacesAndNewlines)),
              port >= 1024, port <= 65535 else {
            error = "Local port must be 1024–65535."
            focused = .port
            return
        }
        submitting = true
        error = nil
        Task {
            if let errMsg = await appState.createTunnel(name: trimmedName,
                                                       localPort: port,
                                                       remotePort: parsedRemotePort,
                                                       autoStart: autoStart) {
                error = errMsg
                submitting = false
            }
        }
    }
}
