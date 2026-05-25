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
    @State private var autoStart = false
    @State private var error: String?
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
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Text("New Tunnel")
                    .font(.title2.weight(.semibold))
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

            VStack(alignment: .leading, spacing: 6) {
                Text("Template").font(.caption).foregroundStyle(.secondary)
                Picker("Template", selection: $template) {
                    ForEach(TunnelTemplate.allCases) { t in
                        Label(t.rawValue, systemImage: t.symbol).tag(t)
                    }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .onChange(of: template) { _, new in applyTemplate(new) }
            }

            VStack(alignment: .leading, spacing: 6) {
                Text("Name").font(.caption).foregroundStyle(.secondary)
                TextField("jupyter", text: $name)
                    .textFieldStyle(.roundedBorder)
                    .focused($focused, equals: .name)
                    .onSubmit { focused = .port }
            }

            VStack(alignment: .leading, spacing: 6) {
                HStack {
                    Text("Local port").font(.caption).foregroundStyle(.secondary)
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
                TextField("8888", text: $portText)
                    .textFieldStyle(.roundedBorder)
                    .focused($focused, equals: .port)
                    .onSubmit { submit() }
            }

            Toggle("Start automatically when daemon boots", isOn: $autoStart)
                .toggleStyle(.checkbox)

            if let error {
                Text(error)
                    .foregroundStyle(.red)
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
        .padding(20)
        .frame(width: 420)
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
            return
        }
        error = nil
        if let lp = parsed.localPort { portText = String(lp) }
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
        if !bits.isEmpty {
            // Use error label as transient hint (yellow could be better but
            // the slot is single-purpose). Briefly clear.
            error = "Parsed: " + bits.joined(separator: ", ")
        }
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
                                                       autoStart: autoStart) {
                error = errMsg
                submitting = false
            }
        }
    }
}
