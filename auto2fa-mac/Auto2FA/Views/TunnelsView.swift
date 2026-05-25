import SwiftUI

struct TunnelsView: View {
    @EnvironmentObject var appState: AppState
    @State private var selection: Tunnel.ID?

    var body: some View {
        if appState.tunnels.isEmpty {
            VStack(spacing: 12) {
                Image(systemName: "sparkles")
                    .font(.largeTitle)
                    .foregroundStyle(.yellow)
                Text("No tunnels yet")
                    .font(.title3)
                Text("Click  +  in the toolbar (or press ⌘N) to create one.")
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                Button {
                    appState.presentNewTunnel()
                } label: {
                    Label("New tunnel", systemImage: "plus")
                }
                .controlSize(.large)
                .keyboardShortcut(.defaultAction)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .padding()
        } else {
            Table(appState.tunnels, selection: $selection) {
                TableColumn("Name") { t in
                    Text(t.name).fontDesign(.monospaced)
                }
                .width(min: 80, ideal: 110)

                TableColumn("Local → Remote") { t in
                    Text(":\(t.localPort) → :\(t.remotePort)")
                        .fontDesign(.monospaced)
                        .foregroundStyle(.secondary)
                }
                .width(min: 100, ideal: 130)

                TableColumn("Node") { t in
                    if let n = t.lastNode {
                        Text(n).fontDesign(.monospaced).lineLimit(1)
                    } else {
                        Text("(no node yet)").foregroundStyle(.tertiary).italic()
                    }
                }

                TableColumn("Via") { t in
                    Text(t.activeJump ?? "—")
                        .foregroundStyle(.secondary)
                }
                .width(min: 60, ideal: 70)

                TableColumn("Status") { t in
                    HStack(spacing: 6) {
                        if isBusy(t) {
                            ProgressView()
                                .controlSize(.small)
                                .scaleEffect(0.7)
                            Text(busyLabel(t))
                                .fontWeight(.medium)
                                .foregroundStyle(.orange)
                        } else {
                            Circle()
                                .fill(color(for: t.displayState))
                                .frame(width: 8, height: 8)
                            Text(displayName(for: t.displayState))
                                .fontWeight(.medium)
                        }
                        Text(t.lastMsg)
                            .foregroundStyle(.secondary)
                            .font(.caption)
                            .lineLimit(1)
                    }
                }
                .width(min: 200)

                TableColumn("") { t in
                    let busy = isBusy(t)
                    HStack(spacing: 4) {
                        Button {
                            Task { await appState.toggleTunnel(t) }
                        } label: {
                            if busy {
                                ProgressView().controlSize(.small).scaleEffect(0.6)
                                    .frame(width: 14, height: 14)
                            } else {
                                Image(systemName: t.displayState == .alive ? "stop.fill" : "play.fill")
                            }
                        }
                        .help(t.displayState == .alive ? "Stop" : "Start")
                        .disabled(busy)
                        Button {
                            appState.presentNodePicker(for: t)
                        } label: {
                            Image(systemName: "list.bullet.rectangle")
                        }
                        .help("Pick a node from squeue")
                        .disabled(busy)
                        Button {
                            openInBrowser(t)
                        } label: {
                            Image(systemName: "safari")
                        }
                        .help("Open localhost:\(t.localPort) in browser")
                        .disabled(busy || t.displayState != .alive)
                        Button {
                            copyURL(t.url)
                        } label: {
                            Image(systemName: "doc.on.doc")
                        }
                        .help("Copy localhost:\(t.localPort)")
                        Button {
                            appState.presentConfirmDelete(for: t)
                        } label: {
                            Image(systemName: "trash")
                        }
                        .help("Delete tunnel")
                        .disabled(busy)
                    }
                    .buttonStyle(.borderless)
                }
                .width(min: 150, ideal: 170)
            }
            .contextMenu(forSelectionType: Tunnel.ID.self) { ids in
                if let id = ids.first,
                   let t = appState.tunnels.first(where: { $0.id == id }) {
                    Button(t.displayState == .alive ? "Stop" : "Start") {
                        Task { await appState.toggleTunnel(t) }
                    }
                    Button("Pick node…") {
                        appState.presentNodePicker(for: t)
                    }
                    Button("Copy localhost:\(t.localPort)") {
                        copyURL(t.url)
                    }
                    Divider()
                    Button("Delete tunnel", role: .destructive) {
                        appState.presentConfirmDelete(for: t)
                    }
                }
            }
        }
    }

    private func copyURL(_ url: String) {
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(url, forType: .string)
    }

    /// Busy = we just clicked something (inFlightTunnels) OR the daemon is
    /// reporting starting. tunnel_toggle's RPC is slow (awaits the 10s probe)
    /// so inFlightTunnels usually overlaps with .starting, but the pick_node
    /// path can take longer than the RPC and only .starting catches that
    /// tail end.
    private func isBusy(_ t: Tunnel) -> Bool {
        if appState.inFlightTunnels.contains(t.name) { return true }
        return t.displayState == .starting
    }

    private func busyLabel(_ t: Tunnel) -> String {
        let msg = t.lastMsg.trimmingCharacters(in: .whitespacesAndNewlines)
        return msg.isEmpty ? "Working…" : msg
    }

    private func openInBrowser(_ t: Tunnel) {
        // Tunnel `url` may be just "localhost:8888"; NSWorkspace.open needs
        // a real URL with scheme. Default to http if no scheme is present.
        var raw = t.url
        if !raw.hasPrefix("http://") && !raw.hasPrefix("https://") {
            raw = "http://" + raw
        }
        guard let url = URL(string: raw) else { return }
        NSWorkspace.shared.open(url)
        appState.notchPresenter.show(
            systemImage: "safari.fill",
            title: t.name,
            description: "opening \(t.url)",
            tint: .blue
        )
    }

    private func color(for state: Tunnel.DisplayState) -> Color {
        switch state {
        case .alive: return .green
        case .starting: return .yellow
        case .stale: return .red
        case .idle: return .secondary
        case .portBusy: return .red
        case .failed: return .red
        case .unknown: return .secondary
        }
    }

    private func displayName(for state: Tunnel.DisplayState) -> String {
        switch state {
        case .alive: return "Connected"
        case .starting: return "Connecting…"
        case .stale: return "Stale"
        case .idle: return "Idle"
        case .portBusy: return "Port busy"
        case .failed: return "Failed"
        case .unknown: return "—"
        }
    }
}
